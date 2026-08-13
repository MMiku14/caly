//! Daemon boot settings, resolved from the layered config.
//!
//! P8a: the read/parse surface (load_from + per-field parsers + error
//! type) moved up to `caly-cli::config` so offline CLI commands share one
//! loader with boot. This module keeps only the daemon-side model and the
//! boot-only resolution that touches `caly-server`/`caly-backends`
//! material (TLS paths, TUN mapping, dns_env, declared proxy groups).

use std::{net::SocketAddr, path::PathBuf};

use caly_composition::RuntimeTuning;
use caly_platform::paths::CoreBinaryPaths;
use caly_dns::DnsSettings;
use caly_domain::{Controllers, CoreKind, TunConfig};
use caly_profile::schema::AppConfig;
use caly_server::tcp::TlsMaterialPaths;

/// Every daemon boot setting, resolved from a single layered config load.
///
/// The per-field `*_from(root)` getters in [`caly_cli::config`] remain
/// available for one-off CLI commands, but the daemon boot path must not
/// re-read and re-parse the same layered config once per field:
/// `resolve_daemon` loads once and derives every value (including the
/// environment overrides applied by each field getter) from that single
/// snapshot.
pub(crate) struct DaemonSettings {
    pub(crate) core_override: Option<CoreKind>,
    pub(crate) listen: Option<SocketAddr>,
    /// TLS material for the TCP listener (`None` = plaintext,
    /// loopback-only deployments, #57).
    pub(crate) tls_material: Option<TlsMaterialPaths>,
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
pub(crate) fn resolve_daemon(root: PathBuf) -> Result<DaemonSettings, caly_cli::config::DaemonConfigError> {
    let config = caly_cli::config::load_from(root)?;
    let kernel = caly_cli::config::kernel_from_config(config.as_ref());
    let (system_proxy_host, system_proxy_port) =
        caly_cli::config::system_proxy_endpoint_from_config(config.as_ref());
    let tuning = RuntimeTuning {
        dns: dns_settings_from_config(config.as_ref()),
        mixed_port: kernel.mixed_port,
        allow_lan: kernel.allow_lan,
        bind_address: kernel.bind_address.clone(),
        log_level: kernel.log_level,
        fetch_policy: caly_cli::config::fetch_policy_from_config(config.as_ref()),
        system_proxy_host,
        system_proxy_port,
        tun_escalation: caly_cli::config::tun_escalation_from_config(config.as_ref()),
        restart_initial_backoff_ms: kernel.restart.initial_backoff_ms,
        restart_max_backoff_ms: kernel.restart.max_backoff_ms,
        rules: caly_cli::config::routing_rules_from_config(config.as_ref()),
        declared_groups: config.as_ref().map_or_else(Vec::new, |config| {
            caly_backends::core::declared_groups_to_domain(&config.proxy_groups)
        }),
        rule_providers: caly_cli::config::rule_providers_from_config(config.as_ref()),
        sniffer: caly_cli::config::sniffer_from_config(config.as_ref()),
        transparent: kernel.transparent,
        start_timeout_ms: kernel.start_timeout_ms,
        stop_timeout_ms: kernel.stop_timeout_ms,
        system_proxy_enabled: system_proxy_enabled_from_config(config.as_ref()),
    };
    Ok(DaemonSettings {
        core_override: caly_cli::config::core_override_from_config(config.as_ref())?,
        listen: listen_from_config(config.as_ref())?,
        auth_token: caly_cli::config::auth_token_from_config(config.as_ref()),
        tls_material: tls_material_from_config(config.as_ref()),
        subscription_refresh: subscription_refresh_from_config(config.as_ref()),
        tun: tun_config_from_config(config.as_ref()),
        controllers: caly_cli::config::controllers_from_config(config.as_ref()),
        binaries: caly_cli::config::core_binaries_from_config(config.as_ref()),
        subscription_urls: subscription_urls_from_config(config.as_ref()),
        tuning,
        auto_start_core: auto_start_core_from_config(config.as_ref()),
    })
}

/// Resolves the optional TCP listen address from an explicit config root.
///
/// Fails closed on non-loopback addresses unless the TCP transport's
/// protections are fully configured (TLS material plus an auth token);
/// see `listen_from_config` for the reconciled gate (#17/#57/#58).
#[cfg(test)]
pub fn listen_from(root: PathBuf) -> Result<Option<SocketAddr>, caly_cli::config::DaemonConfigError> {
    listen_from_config(caly_cli::config::load_from(root)?.as_ref())
}

/// Fails closed on non-loopback addresses unless BOTH transport
/// protections are configured: TLS (`daemon.tls_enabled`, #57)
/// and a handshake admission token (`daemon.auth_token`, #58).
/// Serving a remote control plane without both would expose an
/// unauthenticated endpoint per the schema's own contract; a
/// loopback listener needs neither (local-only trust model).
pub(crate) fn listen_from_config(
    config: Option<&AppConfig>,
) -> Result<Option<SocketAddr>, caly_cli::config::DaemonConfigError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let address = config
        .daemon
        .listen
        .parse::<SocketAddr>()
        .map_err(|_| caly_cli::config::DaemonConfigError::InvalidListenAddress)?;
    if address.ip().is_loopback() {
        return Ok(Some(address));
    }
    // Remote listen: honoured exactly when the schema's own
    // contract is fully configured — TLS on, cert + key present,
    // and an admission token set (#17 reconciled with #57/#58).
    let tls_ready = config.daemon.tls_enabled
        && config.daemon.tls_cert_path.is_some()
        && config.daemon.tls_key_path.is_some();
    if tls_ready && caly_cli::config::auth_token_from_config(Some(config)).is_some() {
        return Ok(Some(address));
    }
    Err(caly_cli::config::DaemonConfigError::RemoteListenUnavailable)
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
pub(crate) fn tls_material_from_config(config: Option<&AppConfig>) -> Option<TlsMaterialPaths> {
    if !tls_enabled_from_config(config) {
        return None;
    }
    let daemon = &config?.daemon;
    Some(TlsMaterialPaths {
        certificate_chain: std::path::PathBuf::from(daemon.tls_cert_path.as_ref()?),
        private_key: std::path::PathBuf::from(daemon.tls_key_path.as_ref()?),
    })
}

pub(crate) fn subscription_urls_from_config(config: Option<&AppConfig>) -> Vec<String> {
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

pub(crate) fn auto_start_core_from_config(config: Option<&AppConfig>) -> bool {
    config
        .as_ref()
        .is_none_or(|config| config.daemon.auto_start_core)
}

/// Resolves DNS settings from an explicit config root.
#[cfg(test)]
pub fn dns_settings_from(root: PathBuf) -> Option<DnsSettings> {
    if std::env::var_os("CALY_DNS_ENABLE").is_some() {
        return caly_backends::dns_env::dns_from_env();
    }
    dns_settings_from_config(caly_cli::config::load_from(root).ok().flatten().as_ref())
}

pub(crate) fn dns_settings_from_config(config: Option<&AppConfig>) -> Option<DnsSettings> {
    if std::env::var_os("CALY_DNS_ENABLE").is_some() {
        return caly_backends::dns_env::dns_from_env();
    }
    let config = config.as_ref()?;
    config.dns.to_settings()
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
