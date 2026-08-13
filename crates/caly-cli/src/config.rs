//! Layered `config.yaml` read/parse surface, shared by the CLI (offline
//! commands) and the daemon host boot path (P8a: moved up from
//! `bins/caly/src/daemon_config.rs`).
//!
//! Loads the XDG `AppConfig` (base `config.yaml`, optional active profile,
//! then `config.d/*.yaml` fragments) using the bounded layered loader, and
//! exposes per-field `*_from_config` parsers so the daemon boot resolves
//! everything from ONE load (single-snapshot invariant: the host's
//! `resolve_daemon` calls [`load_from`] once and derives every value).
//!
//! A present-but-invalid config fails boot so a broken bootstrap never
//! silently runs on stale env defaults; an absent `config.yaml` means "no
//! configuration" and the caller falls back to env.

use std::{fmt, path::PathBuf};

use caly_platform::paths::CoreBinaryPaths;
use caly_domain::{Controllers, CoreKind};
use caly_platform::paths::{AppPaths, SafeName};
use caly_platform::tun::TunEscalation;
use caly_profile::{
    loader::{LayeredConfigPaths, LoaderLimits, load_layered_yaml_strict},
    schema::{AppConfig, CoreConfig, KernelConfig, SnifferConfig},
};
use caly_subscription::FetchPolicy;

/// Layered configuration load/resolution failure.
#[derive(Debug)]
pub enum DaemonConfigError {
    /// The `CALY_PROFILE` value is not a safe profile name.
    InvalidProfile,
    /// The configured core is recognized but not yet supported by the daemon.
    UnsupportedCore,
    /// The layered loader rejected the configuration.
    Layered(caly_profile::loader::LayeredConfigError),
    /// The validated `daemon.listen` did not parse as a socket address.
    InvalidListenAddress,
    /// `daemon.listen` is non-loopback while the required transport
    /// protections are incomplete: enforced TLS (#57) and auth-token
    /// admission (#58) must both be configured for a remote endpoint;
    /// the daemon fails closed instead of serving one unauthenticated.
    RemoteListenUnavailable,
}

impl fmt::Display for DaemonConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProfile => formatter.write_str("CALY_PROFILE must be a safe profile name"),
            Self::UnsupportedCore => formatter.write_str("configured core is not supported"),
            Self::InvalidListenAddress => {
                formatter.write_str("daemon.listen is not a valid socket address")
            }
            Self::RemoteListenUnavailable => formatter.write_str(
                "daemon.listen must be a loopback address: remote listening requires enforced \
                 TLS (cert + key) and a configured auth-token admission",
            ),
            Self::Layered(error) => write!(formatter, "layered configuration error: {error:?}"),
        }
    }
}

impl std::error::Error for DaemonConfigError {}

/// Loads the layered config from an explicit config root, returning `Ok(None)`
/// when no `config.yaml` exists (env-driven fallback).
pub fn load_from(root: PathBuf) -> Result<Option<AppConfig>, DaemonConfigError> {
    let base = root.join("config.yaml");
    if !base.is_file() {
        return Ok(None);
    }
    // W2/D10 (cli-v3-design.md §10): CALY_PROFILE stays an
    // explicit override; absent it, the `caly profile use` context
    // becomes the fallback selector, so the context takes effect
    // on the next daemon start ("daemon 重启后有效").
    let active_profile = match std::env::var("CALY_PROFILE") {
        Ok(value) => Some(SafeName::new(value).map_err(|_| DaemonConfigError::InvalidProfile)?),
        Err(_) => crate::client::context::active_profile(),
    };
    let paths = LayeredConfigPaths::new(root, active_profile);
    // Strict production resolution: declared `profiles:` bodies come from the
    // on-disk ProfileStore cache, so a config the CLI previewed boots
    // identically here.
    load_layered_yaml_strict(
        &paths,
        LoaderLimits::secure_default(),
        &AppPaths::from_env().state,
    )
    .map(Some)
    .map_err(DaemonConfigError::Layered)
}

/// Resolves the managed core from an explicit config root. Returns `None` when
/// no config exists (env drives selection). Precedence between configuration
/// and the `CALY_CORE` environment variable is applied by the application
/// composition, which keeps a single source of truth.
#[cfg(test)]
pub fn core_override_from(root: PathBuf) -> Result<Option<CoreKind>, DaemonConfigError> {
    core_override_from_config(load_from(root)?.as_ref())
}

pub fn core_override_from_config(
    config: Option<&AppConfig>,
) -> Result<Option<CoreKind>, DaemonConfigError> {
    let Some(config) = config else {
        return Ok(None);
    };
    match config.core {
        CoreConfig::Mihomo => Ok(Some(CoreKind::Mihomo)),
        CoreConfig::SingBox => Ok(Some(CoreKind::SingBox)),
        CoreConfig::Xray => Err(DaemonConfigError::UnsupportedCore),
    }
}

/// Parses the configured routing rules. Invalid rules were already rejected by
/// schema validation, so any parse failure here degrades to skipping the line.
pub fn routing_rules() -> Vec<caly_domain::RoutingRule> {
    routing_rules_from_config(
        load_from(AppPaths::from_env().config)
            .ok()
            .flatten()
            .as_ref(),
    )
}

pub fn routing_rules_from_config(config: Option<&AppConfig>) -> Vec<caly_domain::RoutingRule> {
    let Some(config) = config else {
        return Vec::new();
    };
    config
        .rules
        .iter()
        .filter_map(|line| caly_domain::RoutingRule::from_clash_line(line).ok())
        .collect()
}

/// Resolves the user-declared rule providers from a layered config. Each
/// entry was already validated by `validate_rule_providers` in the
/// schema layer; the loader only translates the deserialized
/// `RuleProviderConfig` into the domain [`caly_domain::RuleProvider`]
/// for the runtime tuning bundle.
pub fn rule_providers_from_config(
    config: Option<&AppConfig>,
) -> Vec<caly_domain::RuleProvider> {
    let Some(config) = config else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(config.rule_providers.len());
    for provider in &config.rule_providers {
        match provider.to_rule_provider() {
            Ok(domain) => out.push(domain),
            // Validation runs at parse time; reaching this branch means
            // the layered config is inconsistent with the runtime
            // domain bounds, which is a daemon-internal contract failure.
            // Skip the entry rather than abort boot — the broken provider
            // stays absent from the rendered kernel config and the
            // operator sees the skipped count through existing
            // diagnostics.
            Err(error) => {
                tracing::warn!(
                    provider = provider.name.as_str(),
                    error = %error,
                    "rule provider dropped at boot; check the config"
                );
            }
        }
    }
    out
}

/// Client-side mirror of [`auth_token_from_config`]: reads the
/// layered config and returns the token a handshake must
/// present. A config that fails to load degrades to `None`
/// (the resulting handshake will fail admission against a
/// token-enforcing daemon instead of crashing the client).
pub fn auth_token() -> Option<String> {
    let root = caly_platform::paths::AppPaths::from_env().config;
    auth_token_from_config(load_from(root).ok().flatten().as_ref())
}

/// Resolves `daemon.auth_token` (the handshake admission token)
/// from the layered config, redacting nothing structurally:
/// `SecretText` is revealed exactly once at boot and handed to
/// the service adapter; it is never logged.
pub fn auth_token_from_config(config: Option<&AppConfig>) -> Option<String> {
    config
        .and_then(|config| {
            // `with_exposed` scopes the plaintext to this trim/clone;
            // the value is never logged and the config's `SecretText`
            // wrapper stays redacted in every Debug/Display path.
            config
                .daemon
                .auth_token
                .as_ref()
                .map(|token| token.with_exposed(|exposed| exposed.trim().to_owned()))
        })
        .filter(|token| !token.is_empty())
}

/// Resolves core executable paths from an explicit config root.
pub fn core_binaries_from(root: PathBuf) -> Result<CoreBinaryPaths, DaemonConfigError> {
    Ok(core_binaries_from_config(load_from(root)?.as_ref()))
}

pub fn core_binaries_from_config(config: Option<&AppConfig>) -> CoreBinaryPaths {
    config.map_or_else(CoreBinaryPaths::default, |value| CoreBinaryPaths {
        mihomo: value.core_binaries.mihomo.clone(),
        sing_box: value.core_binaries.sing_box.clone(),
    })
}

/// Resolves the controller endpoints from the config file (falling back to the
/// environment and then built-in defaults).
pub fn controllers() -> Result<caly_domain::Controllers, DaemonConfigError> {
    controllers_from(AppPaths::from_env().config)
}

/// Resolves the controller endpoints from an explicit config root.
pub fn controllers_from(root: PathBuf) -> Result<Controllers, DaemonConfigError> {
    Ok(controllers_from_config(load_from(root)?.as_ref()))
}

pub fn controllers_from_config(config: Option<&AppConfig>) -> Controllers {
    let defaults = Controllers::defaults();
    let mihomo = std::env::var("CALY_MIHOMO_CONTROLLER").unwrap_or_else(|_| {
        config.map_or(defaults.mihomo.clone(), |c| c.controllers.mihomo.clone())
    });
    let sing_box = std::env::var("CALY_SINGBOX_CONTROLLER").unwrap_or_else(|_| {
        config.map_or(defaults.sing_box.clone(), |c| {
            c.controllers.sing_box.clone()
        })
    });
    Controllers { mihomo, sing_box }
}

/// Resolves kernel tuning from an explicit config root.
#[cfg(test)]
pub fn kernel_from(root: PathBuf) -> caly_profile::schema::KernelConfig {
    kernel_from_config(load_from(root).ok().flatten().as_ref())
}

pub fn kernel_from_config(config: Option<&AppConfig>) -> KernelConfig {
    config
        .as_ref()
        .map_or_else(KernelConfig::default, |config| config.kernel.clone())
}

pub fn sniffer_from_config(config: Option<&AppConfig>) -> SnifferConfig {
    config
        .as_ref()
        .map_or_else(SnifferConfig::default, |config| config.sniffer.clone())
}

/// Resolves the configured nameserver references for diagnostics. Unlike
/// `dns_settings`, this does not require `dns.enabled` and never consults the
/// environment: it exists so `caly dns` can probe user-listed resolvers.
pub fn dns_nameservers() -> Vec<String> {
    load_from(AppPaths::from_env().config)
        .ok()
        .flatten()
        .map(|config| config.dns.nameservers)
        .unwrap_or_default()
}

/// Resolves the fetch policy from an explicit config root.
#[cfg(test)]
pub fn fetch_policy_from(root: PathBuf) -> caly_subscription::FetchPolicy {
    fetch_policy_from_config(load_from(root).ok().flatten().as_ref())
}

pub fn fetch_policy_from_config(config: Option<&AppConfig>) -> FetchPolicy {
    let Some(config) = config else {
        return FetchPolicy::direct_default();
    };
    let subscriptions = &config.subscriptions;
    FetchPolicy {
        connect_timeout: std::time::Duration::from_millis(subscriptions.connect_timeout_ms),
        request_timeout: std::time::Duration::from_millis(subscriptions.request_timeout_ms),
        // Mebibytes → bytes with a saturating fallback: the schema
        // caps `max_body_mb` at 256, but a 32-bit `usize` build
        // would have silently wrapped the multiply and the old
        // `try_from(..).unwrap_or(32)` conflated "huge" with
        // "negative" into the same fallback (#56).
        max_body_bytes: usize::try_from(subscriptions.max_body_mb)
            .ok()
            .and_then(|megabytes: usize| megabytes.checked_mul(1024 * 1024))
            .unwrap_or(32 * 1024 * 1024),
        redirects_allowed: subscriptions.follow_redirects,
        use_environment_proxy: subscriptions.use_environment_proxy,
    }
}

/// Resolves the system-proxy endpoint from an explicit config root.
#[cfg(test)]
pub fn system_proxy_endpoint_from(root: PathBuf) -> (String, u16) {
    system_proxy_endpoint_from_config(load_from(root).ok().flatten().as_ref())
}

/// Resolves the effective system-proxy endpoint, honouring the same
/// precedence the daemon boot uses: `CALY_SYSTEM_PROXY_HOST`/
/// `CALY_SYSTEM_PROXY_PORT` env overrides, then `config.yaml`, then
/// `127.0.0.1` / `kernel.mixed_port` (default 7890). Shared by the
/// daemon boot path and `sysproxy status` so both agree on the endpoint.
pub fn system_proxy_endpoint_from_config(config: Option<&AppConfig>) -> (String, u16) {
    let default_port = config
        .as_ref()
        .map_or(7890, |value| value.kernel.mixed_port);
    let host = std::env::var("CALY_SYSTEM_PROXY_HOST").unwrap_or_else(|_| {
        config.as_ref().map_or_else(
            || "127.0.0.1".to_owned(),
            |value| value.system_proxy.host.clone(),
        )
    });
    let port = std::env::var("CALY_SYSTEM_PROXY_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| config.as_ref().and_then(|value| value.system_proxy.port))
        .unwrap_or(default_port);
    (host, port)
}

/// Resolves the TUN escalation policy from an explicit config root.
#[cfg(test)]
pub fn tun_escalation_from(root: PathBuf) -> caly_platform::tun::TunEscalation {
    tun_escalation_from_config(load_from(root).ok().flatten().as_ref())
}

pub fn tun_escalation_from_config(config: Option<&AppConfig>) -> TunEscalation {
    let Some(config) = config else {
        return TunEscalation::default();
    };
    match config.tun.escalation.as_str() {
        "pkexec" => TunEscalation::Pkexec,
        "sudo" => TunEscalation::Sudo,
        "none" => TunEscalation::None,
        _ => TunEscalation::Auto,
    }
}

/// Resolves the configured log level (defaults to `info`).
pub fn log_level() -> String {
    let default = "info".to_owned();
    let Some(config) = load_from(AppPaths::from_env().config).ok().flatten() else {
        return default;
    };
    config.log.level
}

/// Resolves the configured telemetry sampling interval in milliseconds
/// (defaults to 2000; 刀 6 CPU audit — the previous 1s default made the
/// telemetry actor do two HTTP round-trips per second and kept it busy
/// ~2s per sample under sing-box).
pub fn telemetry_interval_ms() -> u64 {
    let Some(config) = load_from(AppPaths::from_env().config).ok().flatten() else {
        return 2_000;
    };
    config.telemetry.interval_ms
}

#[cfg(test)]
mod config_tests;
