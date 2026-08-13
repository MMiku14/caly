//! Mihomo config backend: owner of one mihomo destination file.
//!
//! Split out of `backends/config/mod.rs` (audit #70 file-length
//! budget): the backend struct keeps a small in-memory generation
//! history for rollback; the [`ConfigActorPort`] transaction
//! implementation lives in the sibling `mihomo_port` module.

use caly_coreconf::mihomo::proxy_sections::{
    render_proxy_sections_with_rules_and_providers, render_proxy_sections_with_subscription_groups,
};
use caly_coreconf::mihomo::{MihomoConfigRenderer, MihomoConfigSettings};
use caly_corectl::{
    contract::SpawnSpecFactory,
    mihomo::MihomoSpawnSpecFactory,
    validation::{CoreValidator, LinuxCommandValidator, ValidationReport},
};
use caly_platform::{
    command::LinuxCommandRunner,
    fs::{atomic_write, AtomicFileContents, AtomicWritePlan, LinuxAtomicFileBackend},
};
use caly_ports::ActorFailure;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::Duration,
};

use super::failure;

/// ConfigActor backend publishing validated Mihomo generations atomically.
pub struct MihomoConfigBackend {
    pub(crate) renderer: MihomoConfigRenderer,
    pub(crate) filesystem: LinuxAtomicFileBackend,
    pub(crate) destination: PathBuf,
    pub(crate) generation: u64,
    pub(crate) prepared: HashMap<[u8; 16], (u64, AtomicFileContents)>,
    pub(crate) history: BTreeMap<u64, AtomicFileContents>,
    pub(crate) registry: Option<crate::core::CoreNodeRegistry>,
    /// Subscription-author routing (groups + rules), indexed on refresh.
    /// When any subscription declares groups they own the rendered topology
    /// (2026-08-09 规划) and the implicit `url-test` group is omitted.
    pub(crate) routing: Option<crate::core::CoreRoutingRegistry>,
    /// The config's own declared `proxy_groups:` (2026-08-12 组源统一):
    /// rendered ahead of the subscription groups — the operator's explicit
    /// topology wins over subscription authors.
    pub(crate) declared_groups: Vec<caly_domain::ProxyGroup>,
    pub(crate) tun: Option<caly_domain::TunConfig>,
    pub(crate) mixed_port: u16,
    pub(crate) allow_lan: bool,
    pub(crate) bind_address: String,
    pub(crate) log_level: String,
    pub(crate) external_controller_port: u16,
    pub(crate) secret: Option<String>,
    pub(crate) dns: Option<caly_dns::DnsSettings>,
    pub(crate) rules: Vec<caly_domain::RoutingRule>,
    pub(crate) rule_providers: Vec<caly_domain::RuleProvider>,
    pub(crate) transparent: caly_profile::schema::TransparentConfig,
    pub(crate) sniffer: caly_profile::schema::SnifferConfig,
    pub(crate) binary: Option<PathBuf>,
    pub(crate) workdir: PathBuf,
    pub(crate) validation_timeout: Duration,
}

impl MihomoConfigBackend {
    /// Creates an owner for one destination.
    pub fn new(destination: PathBuf) -> Self {
        Self {
            renderer: MihomoConfigRenderer,
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
            external_controller_port: 9090,
            secret: None,
            dns: None,
            rules: Vec::new(),
            rule_providers: Vec::new(),
            transparent: caly_profile::schema::TransparentConfig::default(),
            sniffer: caly_profile::schema::SnifferConfig::default(),
            binary: None,
            workdir: PathBuf::from("."),
            validation_timeout: Duration::from_secs(10),
        }
    }

    /// Supplies config-driven routing rules for `rules:`.
    #[must_use]
    pub fn with_rules(mut self, rules: Vec<caly_domain::RoutingRule>) -> Self {
        self.rules = rules;
        self
    }

    /// Supplies the config's declared proxy groups (2026-08-12 组源统一).
    #[must_use]
    pub fn with_declared_groups(mut self, groups: Vec<caly_domain::ProxyGroup>) -> Self {
        self.declared_groups = groups;
        self
    }

    /// Supplies config-driven rule providers (rendered into
    /// `rule-providers:`).
    #[must_use]
    pub fn with_rule_providers(mut self, providers: Vec<caly_domain::RuleProvider>) -> Self {
        self.rule_providers = providers;
        self
    }

    /// Supplies transparent-proxy inbound settings (redirect/tproxy).
    #[must_use]
    pub fn with_transparent(
        mut self,
        transparent: caly_profile::schema::TransparentConfig,
    ) -> Self {
        self.transparent = transparent;
        self
    }

    /// Supplies sniffer tuning (domain recovery for bare-IP traffic).
    #[must_use]
    pub fn with_sniffer(mut self, sniffer: caly_profile::schema::SnifferConfig) -> Self {
        self.sniffer = sniffer;
        self
    }

    /// Enables a TUN block in rendered generations.
    #[must_use]
    pub fn with_tun(mut self, tun: Option<caly_domain::TunConfig>) -> Self {
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
        dns: Option<caly_dns::DnsSettings>,
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

    /// Shares the subscription-author routing registry: when subscriptions
    /// declare `proxy-groups` they own the rendered topology verbatim.
    #[must_use]
    pub fn with_routing(mut self, routing: crate::core::CoreRoutingRegistry) -> Self {
        self.routing = Some(routing);
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

    /// Builds settings from the subscription-indexed registry.
    pub(crate) fn build_settings(&self) -> Result<MihomoConfigSettings, ActorFailure> {
        let mut settings = MihomoConfigSettings {
            tun: self.tun.clone(),
            mixed_port: self.mixed_port,
            allow_lan: self.allow_lan,
            bind_address: self.bind_address.clone(),
            log_level: self.log_level.clone(),
            external_controller_port: self.external_controller_port,
            secret: self.secret.clone(),
            dns: self.dns.clone(),
            transparent_port: u16::from(self.transparent.enabled) * self.transparent.port,
            transparent_tproxy: matches!(
                self.transparent.mode,
                caly_profile::schema::TransparentMode::Tproxy
            ),
            sniffer: if self.sniffer.enabled {
                Some(caly_coreconf::mihomo::MihomoSniffer {
                    override_destination: self.sniffer.override_destination,
                    parse_pure_ip: self.sniffer.parse_pure_ip,
                    force_dns_mapping: self.sniffer.force_dns_mapping,
                    http_ports: self.sniffer.http_ports.clone(),
                    tls_ports: self.sniffer.tls_ports.clone(),
                    quic_ports: self.sniffer.quic_ports.clone(),
                })
            } else {
                None
            },
            ..MihomoConfigSettings::default()
        };
        let Some(registry) = &self.registry else {
            return Ok(settings);
        };
        let mapping = registry.lock().map_err(|_| {
            failure(
                "core node registry is poisoned",
                "restart the application runtime",
            )
        })?;
        if mapping.is_empty() {
            return Ok(settings);
        }
        // BTreeMap iteration ascends by NodeId: deterministic render order.
        let mut pairs: Vec<(&str, &str)> = Vec::with_capacity(mapping.len());
        let mut group = String::from("AUTO");
        for (index, registered) in mapping.values().enumerate() {
            pairs.push((registered.name.as_str(), registered.yaml.as_str()));
            if index == 0 {
                group.clone_from(&registered.landing_group);
            }
        }
        let section = match self.routing.as_ref().and_then(|routing| {
            crate::core::merged_routing_with_declared(routing, &self.declared_groups)
        }) {
            // 2026-08-09 规划: a subscription that declares proxy-groups owns
            // the routing — its groups render verbatim, the schema's own
            // `rules:` keep precedence (user rules first, the usual Clash
            // convention), and the implicit url-test group is not emitted.
            Some((groups, subscription_rules)) => {
                let mut rules = self.rules.clone();
                rules.extend(subscription_rules);
                let fallback = groups
                    .iter()
                    .find(|group| matches!(group.kind, caly_domain::ProxyGroupType::Select))
                    .or_else(|| groups.first())
                    .map_or("AUTO", |group| group.name.as_str());
                render_proxy_sections_with_subscription_groups(
                    &pairs,
                    &rules,
                    &self.rule_providers,
                    &groups,
                    fallback,
                )
            }
            None => render_proxy_sections_with_rules_and_providers(
                &group,
                &pairs,
                &self.rules,
                &self.rule_providers,
            ),
        };
        settings.proxies = Some(caly_domain::BoundedText::new(section).map_err(|_| {
            failure(
                "Mihomo proxy section is too large",
                "reduce subscription size",
            )
        })?);
        Ok(settings)
    }

    /// Runs bounded `-t` validation on a rendered config.
    pub(crate) fn validate_config(
        &mut self,
        binary: PathBuf,
        workdir: PathBuf,
        config_path: PathBuf,
        generation: u64,
    ) -> Result<ValidationReport, ActorFailure> {
        validate_rendered_config(
            "Mihomo",
            MihomoSpawnSpecFactory::new(binary, workdir),
            self.validation_timeout,
            config_path,
            generation,
        )
    }
}

/// Publishes `contents` into `destination` atomically (creating the
/// owner-only parent first), records it in the bounded rollback history and
/// reports kernel-specific publish failures. Shared by the Mihomo and
/// sing-box config backends; the caller owns the no-op comparison and the
/// committed-generation bookkeeping.
pub(super) fn publish_and_record(
    filesystem: &mut impl caly_platform::fs::AtomicFileBackend,
    destination: &std::path::Path,
    contents: AtomicFileContents,
    generation: u64,
    history: &mut BTreeMap<u64, AtomicFileContents>,
    label: &str,
) -> Result<(), ActorFailure> {
    // Ensure the owner-only destination directory exists before atomic publish.
    if let Some(parent) = destination.parent() {
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
    let temporary = PathBuf::from(format!("{}.tmp.{generation}", destination.display()));
    atomic_write(
        filesystem,
        AtomicWritePlan {
            destination: destination.to_path_buf(),
            temporary,
            contents: contents.clone(),
        },
    )
    .map_err(|error| {
        failure(
            &format!("{label} config publish failed: {error}"),
            "inspect config filesystem ownership",
        )
    })?;
    history.insert(generation, contents);
    while history.len() > super::MAX_CONFIG_HISTORY {
        if let Some(oldest) = history.keys().next().copied() {
            history.remove(&oldest);
        }
    }
    Ok(())
}

/// Runs bounded kernel validation on a staged config through the kernel's
/// spawn-spec factory, with kernel-specific failure wording. Shared by the
/// Mihomo (`-t`) and sing-box (`check`) config backends; the factory
/// construction stays at the call site so each kernel keeps its own spawn
/// spec error surface.
pub(super) fn validate_rendered_config<F: SpawnSpecFactory>(
    label: &str,
    factory: Result<F, caly_corectl::contract::KernelFailure>,
    timeout: Duration,
    config_path: PathBuf,
    generation: u64,
) -> Result<ValidationReport, ActorFailure> {
    let factory = factory.map_err(|error| {
        failure(
            &format!("{label} factory failed: {error}"),
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
                &format!("{label} validation spec failed: {error}"),
                "inspect config",
            )
        })?;
    let mut validator = LinuxCommandValidator::new(LinuxCommandRunner);
    validator.validate(spec, timeout).map_err(|error| {
        failure(
            &format!("{label} validator failed: {error}"),
            "inspect binary",
        )
    })
}

/// Stages a rendered config into `config.validate.<generation>.<ext>` for
/// real-binary validation, cleaning stale staging files first, with
/// kernel-specific staging failure wording. Shared by the Mihomo and
/// sing-box config backends; the caller runs the bounded binary check,
/// cleans the staging files and maps the report via [`ensure_accepted`].
pub(super) fn stage_validation(
    label: &str,
    extension: &str,
    filesystem: &mut impl caly_platform::fs::AtomicFileBackend,
    workdir: &std::path::Path,
    contents: &AtomicFileContents,
    generation: u64,
) -> Result<PathBuf, ActorFailure> {
    clean_stale_validation_files(workdir);
    let validation_path = workdir.join(format!("config.validate.{generation}.{extension}"));
    let write = AtomicWritePlan {
        destination: validation_path.clone(),
        temporary: workdir.join(format!("config.validate.{generation}.tmp")),
        contents: contents.clone(),
    };
    atomic_write(filesystem, write).map_err(|error| {
        failure(
            &format!("{label} validation staging failed: {error}"),
            "inspect working-directory ownership",
        )
    })?;
    Ok(validation_path)
}

/// Maps a kernel validation report into the backend error, surfacing the
/// kernel's own rejection reason instead of a generic "rejected" summary; the
/// diagnostic is the stderr from the validation executable and directly
/// points at the bad field.
pub(super) fn ensure_accepted(label: &str, report: ValidationReport) -> Result<(), ActorFailure> {
    if report.accepted {
        return Ok(());
    }
    let detail = report
        .diagnostic
        .as_ref()
        .map(|d| d.as_str().trim())
        .filter(|d| !d.is_empty())
        .map(|d| format!(": {d}"))
        .unwrap_or_default();
    Err(failure(
        &format!("generated config was rejected by the {label} binary{detail}"),
        "inspect the validation diagnostic above or the base settings",
    ))
}

/// Removes stale `config.validate.*` staging files left by a previous apply
/// that crashed between staging and cleanup; the files are disposable copies
/// that must never be mistaken for the committed generation.
pub(super) fn clean_stale_validation_files(workdir: &std::path::Path) {
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
