//! Cross-field validation of a parsed `AppConfig`.
//!
//! Kept in a sibling file so `schema/mod.rs` stays within the project's
//! file-size limit. Every check here turns a parseable-but-unusable
//! configuration into an explicit boot failure.

use std::net::SocketAddr;

use super::dns::{DnsSchemaError, validated_dns_settings};
use caly_domain::BoundedText;

use super::settings::RuleProviderSourceConfig;
use super::{AppConfig, ConfigError};

mod profiles;
mod providers;
mod proxy_groups;

/// Two-colour DFS bookkeeping shared across the profile-merge and relay-cycle
/// walks: `in_stack` holds ids on the current recursion path (a revisit here is
/// a genuine cycle), `done` holds fully explored ids (revisiting one through a
/// diamond-shaped graph is not a cycle). The two graphs are independent, but
/// the state shape is identical.
#[derive(Default)]
pub(super) struct MergeWalkState {
    pub(super) in_stack: std::collections::HashSet<String>,
    pub(super) done: std::collections::HashSet<String>,
}

/// Parses one routing-rule line through the domain parser, mapping failures
/// to the same `ConfigError::InvalidRule` shape every rule loop reports.
fn parse_rule(index: usize, line: &str) -> Result<caly_domain::RoutingRule, ConfigError> {
    caly_domain::RoutingRule::from_clash_line(line).map_err(|error| ConfigError::InvalidRule {
        index,
        reason: error.to_string(),
    })
}

/// Validates all cross-field bootstrap invariants of a parsed configuration.
pub fn validate(config: &AppConfig) -> Result<(), ConfigError> {
    if config.schema_version != 1 {
        return Err(ConfigError::UnsupportedSchemaVersion(config.schema_version));
    }
    let listen: SocketAddr = config
        .daemon
        .listen
        .parse()
        .map_err(|_| ConfigError::InvalidListenAddress)?;
    if config.daemon.tls_enabled
        && (config.daemon.tls_cert_path.is_none() || config.daemon.tls_key_path.is_none())
    {
        return Err(ConfigError::TlsMaterialMissing);
    }
    if !listen.ip().is_loopback() && !config.daemon.tls_enabled {
        return Err(ConfigError::RemoteListenRequiresTls);
    }
    if !listen.ip().is_loopback() && config.daemon.auth_token.is_none() {
        return Err(ConfigError::RemoteListenRequiresAuth);
    }
    if !(576..=9_000).contains(&config.tun.mtu) {
        return Err(ConfigError::InvalidTunMtu);
    }
    for (core, address) in [
        ("mihomo", &config.controllers.mihomo),
        ("sing-box", &config.controllers.sing_box),
    ] {
        if address.parse::<SocketAddr>().is_err() {
            return Err(ConfigError::InvalidControllerAddress {
                core: core.to_owned(),
                address: address.clone(),
            });
        }
    }
    if !matches!(
        config.log.level.as_str(),
        "error" | "warn" | "info" | "debug" | "trace"
    ) {
        return Err(ConfigError::InvalidLogLevel);
    }
    if config.telemetry.interval_ms < 100 {
        return Err(ConfigError::InvalidTelemetryInterval);
    }
    if let Some(url) = &config.subscriptions.url {
        let parsed = url::Url::parse(url).map_err(|_| ConfigError::InvalidSubscriptionUrl)?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(ConfigError::InvalidSubscriptionUrl);
        }
    }
    for source in &config.subscriptions.sources {
        let parsed =
            url::Url::parse(&source.url).map_err(|_| ConfigError::InvalidSubscriptionUrl)?;
        // W2-β2a (CLI v3 Q5): `sub add` accepts local files; they are
        // stored as canonical `file://` URLs. The legacy singular
        // `subscriptions.url` above keeps its HTTP(S)-only rule.
        if parsed.scheme() == "file" {
            if parsed.path().is_empty() {
                return Err(ConfigError::InvalidSubscriptionUrl);
            }
            continue;
        }
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(ConfigError::InvalidSubscriptionUrl);
        }
    }
    validate_kernel(config)?;
    validate_dns(config)?;
    validate_dns_filters(config)?;
    validate_fetch_policy(config)?;
    validate_system_proxy(config)?;
    validate_rules(config)?;
    validate_rule_providers(config)?;
    validate_rule_provider_references(config)?;
    profiles::validate_profiles(config)?;
    providers::validate_providers(config)?;
    proxy_groups::validate_proxy_groups(config)?;
    validate_sniffer(config)?;
    if !is_valid_tun_escalation(&config.tun.escalation) {
        return Err(ConfigError::InvalidTunEscalation);
    }
    Ok(())
}

/// Validates every routing rule through the domain parser so a bad rule fails
/// at config load instead of at render time. IP-CIDR values get full address
/// validation from the infrastructure matcher (the domain only checks the
/// `address/prefix` shape).
fn validate_rules(config: &AppConfig) -> Result<(), ConfigError> {
    if config.rules.len() > caly_domain::MAX_RULES {
        return Err(ConfigError::InvalidRule {
            index: config.rules.len(),
            reason: format!("more than {} rules", caly_domain::MAX_RULES),
        });
    }
    for (index, line) in config.rules.iter().enumerate() {
        let rule = parse_rule(index, line)?;
        if let caly_domain::RuleMatch::IpCidr(cidr) = &rule.matcher
            && !crate::rule_match::is_valid_cidr(cidr.as_str())
        {
            return Err(ConfigError::InvalidRule {
                index,
                reason: format!("`{}` is not a valid CIDR block", cidr.as_str()),
            });
        }
    }
    Ok(())
}

/// Validates every `rule_providers:` entry before render. Names must be
/// unique; HTTP URLs must be public and have a sane polling interval; file
/// paths and inline payloads must be non-empty.
fn validate_rule_providers(config: &AppConfig) -> Result<(), ConfigError> {
    if config.rule_providers.len() > caly_domain::MAX_RULE_PROVIDERS {
        return Err(ConfigError::InvalidRule {
            index: config.rule_providers.len(),
            reason: format!(
                "more than {} rule providers",
                caly_domain::MAX_RULE_PROVIDERS
            ),
        });
    }
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for provider in &config.rule_providers {
        if provider.name.trim().is_empty() {
            return Err(ConfigError::InvalidRule {
                index: 0,
                reason: "rule provider name must not be empty".to_owned(),
            });
        }
        // The provider name becomes a file name when the application layer
        // materialises inline payloads (`<state>/rule-providers/<name>.yaml`)
        // — the same path-safety rule as profile ids and proxy group names.
        if !caly_domain::is_path_safe_component(&provider.name) {
            return Err(ConfigError::InvalidRuleProviderName {
                name: provider.name.clone(),
            });
        }
        if !seen_names.insert(provider.name.clone()) {
            return Err(ConfigError::DuplicateRuleProvider {
                name: provider.name.clone(),
            });
        }
        match &provider.kind {
            RuleProviderSourceConfig::Http { url, interval_ms } => {
                let parsed =
                    url::Url::parse(url).map_err(|_| ConfigError::InvalidRuleProviderUrl {
                        name: provider.name.clone(),
                        url: url.clone(),
                    })?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(ConfigError::InvalidRuleProviderUrl {
                        name: provider.name.clone(),
                        url: url.clone(),
                    });
                }
                if *interval_ms < 60_000 {
                    return Err(ConfigError::InvalidRuleProviderInterval {
                        name: provider.name.clone(),
                        interval_ms: *interval_ms,
                    });
                }
            }
            RuleProviderSourceConfig::File { path } => {
                if path.trim().is_empty() {
                    return Err(ConfigError::EmptyRuleProviderPath {
                        name: provider.name.clone(),
                    });
                }
            }
            RuleProviderSourceConfig::Inline { payload } => {
                if payload.trim().is_empty() {
                    return Err(ConfigError::EmptyRuleProviderPayload {
                        name: provider.name.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Validates that every `RULE-SET,<name>,…` rule references a known
/// provider. Auto-emitted GEOIP rule-sets (`geoip-<cc>`) are exempt —
/// the rule renderer inserts them on demand.
fn validate_rule_provider_references(config: &AppConfig) -> Result<(), ConfigError> {
    let declared: std::collections::HashSet<&str> = config
        .rule_providers
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    for (index, line) in config.rules.iter().enumerate() {
        let rule = parse_rule(index, line)?;
        if let caly_domain::RuleMatch::RuleSet(name) = &rule.matcher
            && !declared.contains(name.as_str())
        {
            return Err(ConfigError::UnknownRuleProvider {
                name: name.as_str().to_owned(),
                rule_index: index,
            });
        }
    }
    Ok(())
}

/// Validates every sniffer port spec so a bad spec fails at config load
/// instead of at render time. Specs are `N` or `N-M` within 1..=65535.
fn validate_sniffer(config: &AppConfig) -> Result<(), ConfigError> {
    for spec in config.sniffer.all_port_specs() {
        if !super::kernel::SnifferConfig::is_valid_port_spec(spec) {
            return Err(ConfigError::InvalidSnifferPort {
                spec: spec.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_kernel(config: &AppConfig) -> Result<(), ConfigError> {
    if config.kernel.mixed_port == 0 {
        return Err(ConfigError::InvalidKernelPort);
    }
    if !super::kernel::KernelConfig::is_valid_log_level(&config.kernel.log_level) {
        return Err(ConfigError::InvalidKernelLogLevel);
    }
    let restart = &config.kernel.restart;
    if restart.initial_backoff_ms < 100 || restart.max_backoff_ms < restart.initial_backoff_ms {
        return Err(ConfigError::InvalidKernelRestart);
    }
    if config.kernel.transparent.enabled && config.kernel.transparent.port == 0 {
        return Err(ConfigError::InvalidTransparentPort);
    }
    if config.kernel.start_timeout_ms < 100 || config.kernel.stop_timeout_ms < 100 {
        return Err(ConfigError::InvalidKernelTimeout);
    }
    Ok(())
}

fn validate_dns(config: &AppConfig) -> Result<(), ConfigError> {
    validated_dns_settings(&config.dns)
        .map(|_| ())
        .map_err(|error| match error {
            // B6: the builder is the single home of the socket-address
            // check; keep the dedicated variant so the diagnostic still
            // names `dns.listen` (not the umbrella "DNS" bucket).
            DnsSchemaError::InvalidListen => ConfigError::InvalidDnsListen,
            // #122: the shape check lives in the domain model now; keep the
            // dedicated surface so the diagnostic names `fallback_filter.ipcidr`.
            DnsSchemaError::InvalidIpCidr => ConfigError::InvalidDnsCidrFilter,
            other => ConfigError::InvalidDns(other),
        })
}

/// Structural checks the domain builder cannot express: every
/// `fallback_filter.ipcidr` entry must be a valid CIDR (full address
/// arithmetic lives in the infrastructure matcher). `dns.listen` shape is
/// a builder invariant since B6 and needs no second check here.
fn validate_dns_filters(config: &AppConfig) -> Result<(), ConfigError> {
    let dns = &config.dns;
    if dns
        .fallback_filter
        .ipcidr
        .iter()
        .any(|cidr| !crate::rule_match::is_valid_cidr(cidr))
    {
        return Err(ConfigError::InvalidDnsCidrFilter);
    }
    Ok(())
}

fn validate_fetch_policy(config: &AppConfig) -> Result<(), ConfigError> {
    let subscriptions = &config.subscriptions;
    if subscriptions.connect_timeout_ms < 100 {
        return Err(ConfigError::InvalidFetchPolicy {
            field: "connect_timeout_ms",
            expected: ">= 100",
        });
    }
    if subscriptions.request_timeout_ms < 1_000 {
        return Err(ConfigError::InvalidFetchPolicy {
            field: "request_timeout_ms",
            expected: ">= 1000",
        });
    }
    if !(1..=256).contains(&subscriptions.max_body_mb) {
        return Err(ConfigError::InvalidFetchPolicy {
            field: "max_body_mb",
            expected: "1..=256",
        });
    }
    Ok(())
}

/// Accepts exactly the escalation strategies the platform backend implements.
pub fn is_valid_tun_escalation(value: &str) -> bool {
    matches!(value, "auto" | "pkexec" | "sudo" | "none")
}

fn validate_system_proxy(config: &AppConfig) -> Result<(), ConfigError> {
    let system_proxy = &config.system_proxy;
    if system_proxy.host.trim().is_empty() {
        return Err(ConfigError::InvalidSystemProxy);
    }
    if system_proxy.port == Some(0) {
        return Err(ConfigError::InvalidSystemProxy);
    }
    Ok(())
}

/// Converts a YAML parser failure into a `ConfigError::Parse`, preserving
/// the line/column the YAML reader reported (the old `parse_message` helper
/// flattened every failure to `0:0`, making user configs impossible to
/// locate).
pub(super) fn parse_yaml_error(error: &serde_norway::Error) -> ConfigError {
    let (line, column) = error
        .location()
        .map_or((0, 0), |location| (location.line(), location.column()));
    match BoundedText::new(error.to_string()) {
        Ok(message) => ConfigError::Parse {
            line,
            column,
            message,
        },
        Err(_) => ConfigError::ParseDetailsUnavailable,
    }
}

pub(super) fn parse_error(error: serde_json::Error) -> ConfigError {
    match BoundedText::new(error.to_string()) {
        Ok(message) => ConfigError::Parse {
            line: error.line(),
            column: error.column(),
            message,
        },
        Err(_) => ConfigError::ParseDetailsUnavailable,
    }
}
