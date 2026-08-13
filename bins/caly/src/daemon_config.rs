//! Layered daemon configuration wired into boot.
//!
//! Loads and validates the XDG `AppConfig` (base `config.yaml`, optional active
//! profile, then `config.d/*.yaml` fragments) using the bounded layered loader.
//! Only the fields that have a real runtime effect today are surfaced: the
//! managed proxy-core selection. A present-but-invalid config fails boot so a
//! broken bootstrap never silently runs on stale env defaults; an absent
//! `config.yaml` means "no configuration" and the caller falls back to env.

use std::{fmt, net::SocketAddr, path::PathBuf};

use caly_composition::{CoreBinaryPaths, RuntimeTuning};
use caly_dns::DnsSettings;
use caly_domain::{Controllers, CoreKind, TunConfig};
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

/// Every daemon boot setting, resolved from a single layered config load.
///
/// The per-field `*_from(root)` getters below remain available for one-off
/// CLI commands, but the daemon boot path must not re-read and re-parse the
/// same layered config once per field: `resolve_daemon` loads once and derives
/// every value (including the environment overrides applied by each field
/// getter) from that single snapshot.
pub(crate) struct DaemonSettings {
    pub(crate) core_override: Option<CoreKind>,
    pub(crate) listen: Option<SocketAddr>,
    /// TLS material for the TCP listener (`None` = plaintext,
    /// loopback-only deployments, #57).
    pub(crate) tls_material: Option<caly_server::tcp::TlsMaterialPaths>,
    /// Configured `daemon.auth_token`; when `Some` every
    /// transport demands it in the handshake (#58).
    pub(crate) auth_token: Option<String>,
    /// Daemon-side periodic subscription refresh cadence
    /// (`subscriptions.refresh_interval_minutes`, #59); `None`
    /// disables the background timer.
    pub(crate) subscription_refresh: Option<std::time::Duration>,
    pub(crate) tun: Option<TunConfig>,
    pub(crate) controllers: Controllers,
    pub(crate) binaries: CoreBinaryPaths,
    /// Declared subscription URLs: the legacy scalar first, then the
    /// `subscriptions.sources` entries in declaration order. Every entry
    /// is registered (and its cache revived) at daemon start — before
    /// this, `sources` were never registered and their caches were
    /// treated as ghosts (2026-08-12 user-flow audit).
    pub(crate) subscription_urls: Vec<String>,
    pub(crate) tuning: RuntimeTuning,
    pub(crate) auto_start_core: bool,
}

/// Resolves all boot settings from one layered config load; an invalid config
/// fails the whole boot.
pub(crate) fn resolve_daemon(root: PathBuf) -> Result<DaemonSettings, DaemonConfigError> {
    let config = load_from(root)?;
    let kernel = kernel_from_config(config.as_ref());
    let (system_proxy_host, system_proxy_port) = system_proxy_endpoint_from_config(config.as_ref());
    let tuning = RuntimeTuning {
        dns: dns_settings_from_config(config.as_ref()),
        mixed_port: kernel.mixed_port,
        allow_lan: kernel.allow_lan,
        bind_address: kernel.bind_address.clone(),
        log_level: kernel.log_level,
        fetch_policy: fetch_policy_from_config(config.as_ref()),
        system_proxy_host,
        system_proxy_port,
        tun_escalation: tun_escalation_from_config(config.as_ref()),
        restart_initial_backoff_ms: kernel.restart.initial_backoff_ms,
        restart_max_backoff_ms: kernel.restart.max_backoff_ms,
        rules: routing_rules_from_config(config.as_ref()),
        declared_groups: config.as_ref().map_or_else(Vec::new, |config| {
            caly_backends::core::declared_groups_to_domain(&config.proxy_groups)
        }),
        rule_providers: rule_providers_from_config(config.as_ref()),
        sniffer: sniffer_from_config(config.as_ref()),
        transparent: kernel.transparent,
        start_timeout_ms: kernel.start_timeout_ms,
        stop_timeout_ms: kernel.stop_timeout_ms,
        system_proxy_enabled: system_proxy_enabled_from_config(config.as_ref()),
    };
    Ok(DaemonSettings {
        core_override: core_override_from_config(config.as_ref())?,
        listen: listen_from_config(config.as_ref())?,
        auth_token: auth_token_from_config(config.as_ref()),
        tls_material: tls_material_from_config(config.as_ref()),
        subscription_refresh: subscription_refresh_from_config(config.as_ref()),
        tun: tun_config_from_config(config.as_ref()),
        controllers: controllers_from_config(config.as_ref()),
        binaries: core_binaries_from_config(config.as_ref()),
        subscription_urls: subscription_urls_from_config(config.as_ref()),
        tuning,
        auto_start_core: auto_start_core_from_config(config.as_ref()),
    })
}

/// Resolves the managed core from an explicit config root. Returns `None` when
/// no config exists (env drives selection). Precedence between configuration
/// and the `CALY_CORE` environment variable is applied by the application
/// composition, which keeps a single source of truth.
#[cfg(test)]
pub fn core_override_from(root: PathBuf) -> Result<Option<CoreKind>, DaemonConfigError> {
    core_override_from_config(load_from(root)?.as_ref())
}

fn core_override_from_config(
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

fn auto_start_core_from_config(config: Option<&AppConfig>) -> bool {
    config
        .as_ref()
        .is_none_or(|config| config.daemon.auto_start_core)
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

fn routing_rules_from_config(config: Option<&AppConfig>) -> Vec<caly_domain::RoutingRule> {
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
fn rule_providers_from_config(config: Option<&AppConfig>) -> Vec<caly_domain::RuleProvider> {
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

/// Resolves the optional TCP listen address from an explicit config root.
///
/// Fails closed on non-loopback addresses unless the TCP transport's
/// protections are fully configured (TLS material plus an auth token);
/// see `listen_from_config` for the reconciled gate (#17/#57/#58).
#[cfg(test)]
pub fn listen_from(root: PathBuf) -> Result<Option<SocketAddr>, DaemonConfigError> {
    listen_from_config(load_from(root)?.as_ref())
}

/// Resolves `daemon.auth_token` (the handshake admission token)
/// from the layered config, redacting nothing structurally:
/// `SecretText` is revealed exactly once at boot and handed to
/// the service adapter; it is never logged.
pub(crate) fn auth_token_from_config(config: Option<&AppConfig>) -> Option<String> {
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

/// Fails closed on non-loopback addresses unless BOTH transport
/// protections are configured: TLS (`daemon.tls_enabled`, #57)
/// and a handshake admission token (`daemon.auth_token`, #58).
/// Serving a remote control plane without both would expose an
/// unauthenticated endpoint per the schema's own contract; a
/// loopback listener needs neither (local-only trust model).
fn listen_from_config(config: Option<&AppConfig>) -> Result<Option<SocketAddr>, DaemonConfigError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let address = config
        .daemon
        .listen
        .parse::<SocketAddr>()
        .map_err(|_| DaemonConfigError::InvalidListenAddress)?;
    if address.ip().is_loopback() {
        return Ok(Some(address));
    }
    // Remote listen: honoured exactly when the schema's own
    // contract is fully configured — TLS on, cert + key present,
    // and an admission token set (#17 reconciled with #57/#58).
    let tls_ready = config.daemon.tls_enabled
        && config.daemon.tls_cert_path.is_some()
        && config.daemon.tls_key_path.is_some();
    if tls_ready && auth_token_from_config(Some(config)).is_some() {
        return Ok(Some(address));
    }
    Err(DaemonConfigError::RemoteListenUnavailable)
}

/// Daemon-side periodic subscription refresh cadence (#59):
/// `Some(duration)` when `subscriptions.refresh_interval_minutes`
/// is a positive number of minutes, `None` when the operator
/// left the timer disabled (the default).
pub(crate) fn subscription_refresh_from_config(
    config: Option<&AppConfig>,
) -> Option<std::time::Duration> {
    let minutes = config?.subscriptions.refresh_interval_minutes;
    if minutes == 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(minutes.saturating_mul(60)))
}

/// Whether the daemon serves the TCP listener through TLS
/// (#57). Mirrors the schema gate: needs `tls_enabled` plus both
/// material paths.
pub(crate) fn tls_enabled_from_config(config: Option<&AppConfig>) -> bool {
    config.is_some_and(|config| {
        config.daemon.tls_enabled
            && config.daemon.tls_cert_path.is_some()
            && config.daemon.tls_key_path.is_some()
    })
}

/// TLS material for the TCP listener (`None` when TLS is off or
/// incompletely configured).
pub(crate) fn tls_material_from_config(
    config: Option<&AppConfig>,
) -> Option<caly_server::tcp::TlsMaterialPaths> {
    if !tls_enabled_from_config(config) {
        return None;
    }
    let daemon = &config?.daemon;
    Some(caly_server::tcp::TlsMaterialPaths {
        certificate_chain: std::path::PathBuf::from(daemon.tls_cert_path.as_ref()?),
        private_key: std::path::PathBuf::from(daemon.tls_key_path.as_ref()?),
    })
}

fn subscription_urls_from_config(config: Option<&AppConfig>) -> Vec<String> {
    let Some(config) = config else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    if let Some(legacy) = config.subscriptions.url.clone() {
        urls.push(legacy);
    }
    urls.extend(
        config
            .subscriptions
            .sources
            .iter()
            .map(|source| source.url.clone()),
    );
    urls
}

/// Resolves core executable paths from an explicit config root.
pub fn core_binaries_from(root: PathBuf) -> Result<CoreBinaryPaths, DaemonConfigError> {
    Ok(core_binaries_from_config(load_from(root)?.as_ref()))
}

fn core_binaries_from_config(config: Option<&AppConfig>) -> CoreBinaryPaths {
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

fn controllers_from_config(config: Option<&AppConfig>) -> Controllers {
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

fn kernel_from_config(config: Option<&AppConfig>) -> KernelConfig {
    config
        .as_ref()
        .map_or_else(KernelConfig::default, |config| config.kernel.clone())
}

fn sniffer_from_config(config: Option<&AppConfig>) -> SnifferConfig {
    config
        .as_ref()
        .map_or_else(SnifferConfig::default, |config| config.sniffer.clone())
}

/// Resolves DNS settings from an explicit config root.
#[cfg(test)]
pub fn dns_settings_from(root: PathBuf) -> Option<DnsSettings> {
    if std::env::var_os("CALY_DNS_ENABLE").is_some() {
        return caly_backends::dns_env::dns_from_env();
    }
    dns_settings_from_config(load_from(root).ok().flatten().as_ref())
}

fn dns_settings_from_config(config: Option<&AppConfig>) -> Option<DnsSettings> {
    if std::env::var_os("CALY_DNS_ENABLE").is_some() {
        return caly_backends::dns_env::dns_from_env();
    }
    let config = config.as_ref()?;
    config.dns.to_settings()
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

fn fetch_policy_from_config(config: Option<&AppConfig>) -> FetchPolicy {
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
pub(crate) fn system_proxy_endpoint_from_config(config: Option<&AppConfig>) -> (String, u16) {
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

fn tun_escalation_from_config(config: Option<&AppConfig>) -> TunEscalation {
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

fn tun_config_from_config(config: Option<&AppConfig>) -> Option<caly_domain::TunConfig> {
    let config = config?;
    // Shared with the per-apply re-read in the config backends, so the boot
    // resolution and every later `config apply` agree on the same mapping.
    caly_backends::config::tun_config_from_schema(config)
}

/// Whether the desktop system proxy should be engaged during boot.
fn system_proxy_enabled_from_config(config: Option<&AppConfig>) -> bool {
    config
        .as_ref()
        .is_some_and(|config| config.system_proxy.enabled)
}

#[cfg(test)]
mod daemon_config_tests;
