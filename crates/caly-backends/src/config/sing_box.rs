//! sing-box ConfigActor backend: renders a subscription-driven sing-box JSON
//! config, validates it against the real kernel binary, commits atomically;
//! failures leave the previous generation intact.
//!
//! Mirrors `MihomoConfigBackend` (same ownership/transaction shape), rendering
//! through the shared `sing_box_document` assembly with outbounds rebuilt from
//! the registry's per-node sing-box JSON (no subscription-body re-parsing).

use caly_coreconf::{
    rules::render_sing_box_rules,
    sing_box::{SingBoxRenderTuning, sing_box_document},
};
use caly_corectl::{
    contract::SpawnSpecFactory,
    sing_box::SingBoxSpawnSpecFactory,
    validation::{CoreValidator, LinuxCommandValidator, ValidationReport},
};
use caly_dns::DnsSettings;
use caly_domain::{RoutingRule, RuleProvider, TunConfig};
use caly_platform::{
    command::LinuxCommandRunner,
    fs::{AtomicFileContents, AtomicWritePlan, LinuxAtomicFileBackend, atomic_write},
};
use caly_ports::{ActorFailure, CommittedConfig, ConfigActorPort, ConfigCandidate, PreparedConfig};
use serde_json::Value;

use super::resolve_tun_from_config;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::Duration,
};

use super::failure;

/// Maximum committed generations retained in memory for rollback; older
/// generations are dropped (the on-disk current generation stays authoritative).
pub const MAX_CONFIG_HISTORY: usize = 2;

/// The TUN interface name rendered into sing-box inbounds.
pub const SING_BOX_TUN_INTERFACE: &str = "caly0";

/// ConfigActor backend publishing validated sing-box generations atomically.
pub struct SingBoxConfigBackend {
    filesystem: LinuxAtomicFileBackend,
    destination: PathBuf,
    generation: u64,
    prepared: HashMap<[u8; 16], (u64, AtomicFileContents)>,
    history: BTreeMap<u64, AtomicFileContents>,
    registry: Option<crate::core::CoreNodeRegistry>,
    /// Subscription-author routing (groups + rules); declared groups render
    /// as selector/urltest outbounds and own the rule table (2026-08-09 规划).
    routing: Option<crate::core::CoreRoutingRegistry>,
    /// Config-declared `proxy_groups:` rendered ahead of the
    /// subscription groups (2026-08-12 组源统一).
    declared_groups: Vec<caly_domain::ProxyGroup>,
    tun: Option<TunConfig>,
    mixed_port: u16,
    allow_lan: bool,
    bind_address: String,
    log_level: String,
    external_controller_port: u16,
    secret: Option<String>,
    dns: Option<DnsSettings>,
    rules: Vec<RoutingRule>,
    rule_providers: Vec<RuleProvider>,
    sniffer: caly_profile::schema::SnifferConfig,
    binary: Option<PathBuf>,
    workdir: PathBuf,
    validation_timeout: Duration,
}

impl SingBoxConfigBackend {
    /// Creates an owner for one destination.
    pub fn new(destination: PathBuf) -> Self {
        Self {
            filesystem: LinuxAtomicFileBackend,
            destination,
            generation: 0,
            prepared: HashMap::new(),
            history: BTreeMap::new(),
            registry: None,
            routing: None,
            declared_groups: Vec::new(),
            tun: None,
            mixed_port: 7890,
            allow_lan: false,
            bind_address: "*".to_owned(),
            log_level: "info".to_owned(),
            external_controller_port: 9091,
            secret: None,
            dns: None,
            rules: Vec::new(),
            rule_providers: Vec::new(),
            sniffer: caly_profile::schema::SnifferConfig::default(),
            binary: None,
            workdir: PathBuf::from("."),
            validation_timeout: Duration::from_secs(10),
        }
    }

    /// Supplies config-driven routing rules for `route.rules`.
    #[must_use]
    pub fn with_rules(mut self, rules: Vec<RoutingRule>) -> Self {
        self.rules = rules;
        self
    }

    /// Supplies config-driven rule providers (rendered into
    /// `route.rule_set`).
    #[must_use]
    pub fn with_rule_providers(mut self, providers: Vec<RuleProvider>) -> Self {
        self.rule_providers = providers;
        self
    }

    /// Supplies sniffer tuning (domain recovery for bare-IP traffic).
    #[must_use]
    pub fn with_sniffer(mut self, sniffer: caly_profile::schema::SnifferConfig) -> Self {
        self.sniffer = sniffer;
        self
    }

    /// Enables a TUN inbound in rendered generations.
    #[must_use]
    pub fn with_tun(mut self, tun: Option<TunConfig>) -> Self {
        self.tun = tun;
        self
    }

    /// Applies kernel tuning: inbound port/LAN, log level, controller, DNS.
    #[must_use]
    pub fn with_kernel(
        mut self,
        mixed_port: u16,
        allow_lan: bool,
        bind_address: String,
        log_level: String,
        external_controller_port: u16,
        dns: Option<DnsSettings>,
    ) -> Self {
        self.mixed_port = mixed_port;
        self.allow_lan = allow_lan;
        self.bind_address = bind_address;
        self.log_level = log_level;
        self.external_controller_port = external_controller_port;
        self.dns = dns;
        self
    }

    /// Shares the subscription-indexed proxy registry used for rendering.
    #[must_use]
    pub fn with_registry(mut self, registry: crate::core::CoreNodeRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Shares the subscription-author routing registry (groups + rules).
    #[must_use]
    pub fn with_routing(mut self, routing: crate::core::CoreRoutingRegistry) -> Self {
        self.routing = Some(routing);
        self
    }

    /// Supplies the config's declared proxy groups (2026-08-12 组源统一).
    #[must_use]
    pub fn with_declared_groups(mut self, groups: Vec<caly_domain::ProxyGroup>) -> Self {
        self.declared_groups = groups;
        self
    }

    /// Enables real-kernel validation before commit.
    #[must_use]
    pub fn with_validation(mut self, binary: PathBuf, workdir: PathBuf, timeout: Duration) -> Self {
        self.binary = Some(binary);
        self.workdir = workdir;
        self.validation_timeout = timeout;
        self
    }

    /// Supplies the daemon's shared controller auth secret (fail-closed).
    #[must_use]
    pub fn with_secret(mut self, secret: Option<String>) -> Self {
        self.secret = secret;
        self
    }

    /// Builds the full sing-box document from the subscription-indexed
    /// registry plus the config-driven header tuning.
    fn build_document(&self) -> Result<Vec<u8>, ActorFailure> {
        let routing = self.routing.as_ref().and_then(|routing| {
            crate::core::merged_routing_with_declared(routing, &self.declared_groups)
        });
        let mut outbounds: Vec<Value> = Vec::new();
        // Node-name → canonical sing-box tag map, used to resolve the
        // subscription groups' node members.
        let mut name_to_tag = std::collections::BTreeMap::new();
        if let Some(registry) = &self.registry {
            let mapping = registry.lock().map_err(|_| {
                failure(
                    "core node registry is poisoned",
                    "restart the application runtime",
                )
            })?;
            // BTreeMap iteration ascends by NodeId: deterministic render order.
            for registered in mapping.values() {
                let Some(singbox) = &registered.singbox else {
                    continue;
                };
                let Ok(outbound) = serde_json::from_str::<Value>(singbox) else {
                    continue;
                };
                if let Some(tag) = outbound.get("tag").and_then(Value::as_str) {
                    name_to_tag.insert(registered.name.clone(), tag.to_owned());
                }
                outbounds.push(outbound);
            }
        }
        // 2026-08-09 规划: author-declared groups render after the nodes, in
        // declaration order; their tags form the allow-list the rule renderer
        // steers `Proxy(<group>)` policies to. A member that has no node
        // outbound (unsupported protocol, dropped upstream) is logged and
        // skipped — silently shrinking a group would be a routing change
        // the operator cannot see.
        let mut group_tags = std::collections::BTreeSet::new();
        if let Some((groups, _)) = &routing {
            let resolve = |name: &str| {
                let tag = name_to_tag.get(name).cloned();
                if tag.is_none() {
                    tracing::warn!(
                        node = %name,
                        "subscription group member has no sing-box outbound; dropped"
                    );
                }
                tag
            };
            for declared in groups {
                if let Some(outbound) =
                    caly_coreconf::sing_box::proxy_group_to_sing_box_outbound(declared, &resolve)
                {
                    group_tags.insert(declared.name.as_str().to_owned());
                    outbounds.push(outbound);
                }
            }
        }
        // The PROXY/GLOBAL catch-all selectors are appended downstream by
        // `sing_box_document` over every outbound tag rendered here (group
        // tags included — nested selectors are legal sing-box).
        let mut merged_rules;
        let rules = match &routing {
            // Schema rules keep precedence; the subscription table follows.
            Some((_, subscription_rules)) => {
                merged_rules = self.rules.clone();
                merged_rules.extend(subscription_rules.iter().cloned());
                &merged_rules
            }
            None => &self.rules,
        };
        let rendered_rules = render_sing_box_rules(rules, &self.rule_providers, true, &group_tags);
        if rendered_rules.skipped > 0 {
            tracing::warn!(
                skipped = rendered_rules.skipped,
                "sing-box routing rule(s) skipped as unrepresentable"
            );
        }
        let tuning = SingBoxRenderTuning {
            controller: format!("127.0.0.1:{}", self.external_controller_port),
            secret: self.secret.clone().unwrap_or_default(),
            log_level: self.log_level.clone(),
            mixed_port: self.mixed_port,
            allow_lan: self.allow_lan,
            bind_address: self.bind_address.clone(),
            tun: self.tun.clone(),
            tun_interface: SING_BOX_TUN_INTERFACE.to_owned(),
            dns: self.dns.clone(),
            sniff: self.sniffer.enabled,
            sniff_override_destination: self.sniffer.override_destination,
            route_rules: rendered_rules.rules,
            rule_sets: rendered_rules.rule_sets,
            route_final: rendered_rules.final_outbound,
            block_outbound: rendered_rules.block_outbound,
        };
        sing_box_document(&tuning, outbounds).map_err(|error| {
            failure(
                &format!("sing-box config render failed: {error}"),
                "inspect the subscription render or base settings",
            )
        })
    }

    /// Runs bounded `sing-box check` validation on a rendered config.
    fn validate_config(
        &mut self,
        binary: PathBuf,
        workdir: PathBuf,
        config_path: PathBuf,
        generation: u64,
    ) -> Result<ValidationReport, ActorFailure> {
        let factory = SingBoxSpawnSpecFactory::new(binary, workdir).map_err(|error| {
            failure(
                &format!("sing-box factory failed: {error}"),
                "inspect binary path",
            )
        })?;
        let spec = factory
            .build_validation_spec(&caly_corectl::contract::RenderedConfigRef {
                generation,
                path: config_path,
            })
            .map_err(|error| {
                failure(
                    &format!("sing-box validation spec failed: {error}"),
                    "inspect config",
                )
            })?;
        let mut validator = LinuxCommandValidator::new(LinuxCommandRunner);
        validator
            .validate(spec, self.validation_timeout)
            .map_err(|error| {
                failure(
                    &format!("sing-box validator failed: {error}"),
                    "inspect binary",
                )
            })
    }
}

/// Removes stale `config.validate.*` staging files left by a previous apply
/// that crashed between staging and cleanup; the files are disposable copies
/// that must never be mistaken for the committed generation.
fn clean_stale_validation_files(workdir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(workdir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("config.validate.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl ConfigActorPort for SingBoxConfigBackend {
    fn parse_and_render(
        &mut self,
        candidate: ConfigCandidate,
    ) -> Result<PreparedConfig, ActorFailure> {
        // Re-read the TUN tuning per apply (like the subscription URL re-reads
        // on refresh): a `tun.enabled` edit takes effect without a restart.
        self.tun = resolve_tun_from_config();
        let rendered = self.build_document()?;
        let contents = AtomicFileContents::try_from_vec(rendered).map_err(|_| {
            failure(
                "sing-box generation is too large",
                "reduce subscription size",
            )
        })?;
        let generation = self.generation.saturating_add(1);
        // Validate against the real binary before the candidate is considered
        // prepared, so a rejected config can never reach commit.
        if let Some(binary) = self.binary.clone() {
            clean_stale_validation_files(&self.workdir);
            let validation_path = self
                .workdir
                .join(format!("config.validate.{generation}.json"));
            let write = AtomicWritePlan {
                destination: validation_path.clone(),
                temporary: self
                    .workdir
                    .join(format!("config.validate.{generation}.tmp")),
                contents: contents.clone(),
            };
            atomic_write(&mut self.filesystem, write).map_err(|error| {
                failure(
                    &format!("sing-box validation staging failed: {error}"),
                    "inspect working-directory ownership",
                )
            })?;
            let report = self.validate_config(
                binary,
                self.workdir.clone(),
                validation_path.clone(),
                generation,
            )?;
            let _ = std::fs::remove_file(&validation_path);
            let _ = std::fs::remove_file(
                self.workdir
                    .join(format!("config.validate.{generation}.tmp")),
            );
            if !report.accepted {
                let detail = report
                    .diagnostic
                    .as_ref()
                    .map(|d| d.as_str().trim())
                    .filter(|d| !d.is_empty())
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(failure(
                    &format!("generated config was rejected by the sing-box binary{detail}"),
                    "inspect the validation diagnostic above or the base settings",
                ));
            }
        }
        self.prepared.insert(candidate.id, (generation, contents));
        Ok(PreparedConfig {
            candidate_id: candidate.id,
            generation,
        })
    }

    fn discard_prepared(&mut self, prepared: PreparedConfig) -> Result<(), ActorFailure> {
        self.prepared.remove(&prepared.candidate_id);
        Ok(())
    }

    fn commit_candidate(
        &mut self,
        prepared: PreparedConfig,
    ) -> Result<CommittedConfig, ActorFailure> {
        let (_, contents) = self
            .prepared
            .remove(&prepared.candidate_id)
            .ok_or_else(|| failure("prepared config is missing", "re-render the candidate"))?;
        // 刀 4 (2026-08-12 pipeline design): a render that is byte-identical
        // to the already-published config skips the write and the caller
        // skips the kernel restart — a no-op apply must not drop
        // connections. JSON has no comment syntax, so the comparison is
        // direct.
        let unchanged = std::fs::read_to_string(&self.destination)
            .ok()
            .is_some_and(|existing| existing.as_bytes() == contents.as_slice());
        if unchanged {
            return Ok(CommittedConfig {
                candidate_id: prepared.candidate_id,
                generation: self.generation,
                unchanged: true,
            });
        }
        // Ensure the owner-only destination directory exists before atomic publish.
        if let Some(parent) = self.destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                failure(
                    &format!("cannot create config directory: {error}"),
                    "inspect config filesystem ownership",
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        let bytes = contents.into_vec();
        // JSON has no comment syntax, so the generation is NOT appended to the
        // file (unlike the Mihomo YAML backend); the history map below is the
        // only generation record, which is sufficient for rollback.
        let contents = AtomicFileContents::try_from_vec(bytes).map_err(|_| {
            failure(
                "sing-box generation is too large",
                "reduce generated configuration",
            )
        })?;
        let temporary = PathBuf::from(format!(
            "{}.tmp.{}",
            self.destination.display(),
            prepared.generation
        ));
        atomic_write(
            &mut self.filesystem,
            AtomicWritePlan {
                destination: self.destination.clone(),
                temporary,
                contents: contents.clone(),
            },
        )
        .map_err(|error| {
            failure(
                &format!("sing-box config publish failed: {error}"),
                "inspect config filesystem ownership",
            )
        })?;
        self.history.insert(prepared.generation, contents);
        while self.history.len() > MAX_CONFIG_HISTORY {
            if let Some(oldest) = self.history.keys().next().copied() {
                self.history.remove(&oldest);
            }
        }
        self.generation = prepared.generation;
        Ok(CommittedConfig {
            candidate_id: prepared.candidate_id,
            generation: prepared.generation,
            unchanged: false,
        })
    }

    fn current_contents(&self) -> Option<Vec<u8>> {
        self.history
            .iter()
            .next_back()
            .map(|(_, contents)| contents.clone().into_vec())
    }

    fn rollback_commit(&mut self, committed: CommittedConfig) -> Result<(), ActorFailure> {
        // A no-op apply never wrote anything, so there is nothing to roll
        // back — mirror the mihomo port's guard (2026-08-12 audit).
        if committed.unchanged {
            return Ok(());
        }
        if self.generation != committed.generation {
            return Ok(());
        }
        let previous = self
            .history
            .range(..committed.generation)
            .next_back()
            .map(|(generation, contents)| (*generation, contents.clone()));
        if let Some((generation, contents)) = previous {
            let temporary = PathBuf::from(format!(
                "{}.rollback.{}",
                self.destination.display(),
                generation
            ));
            atomic_write(
                &mut self.filesystem,
                AtomicWritePlan {
                    destination: self.destination.clone(),
                    temporary,
                    contents,
                },
            )
            .map_err(|error| {
                failure(
                    &format!("config rollback failed: {error}"),
                    "inspect generation filesystem ownership",
                )
            })?;
            self.generation = generation;
            // 刀 6 (boundary audit): drop the rolled-back generation so
            // current_contents() never serves it for a hot reload.
            self.history.remove(&committed.generation);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
