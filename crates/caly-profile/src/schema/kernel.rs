//! Kernel runtime tuning and the DNS feature section of the app config.
//!
//! These sections drive what the renderers emit into the managed proxy-core
//! configuration. `dns` maps onto the core-agnostic `caly_domain::DnsSettings`
//! model; validation reuses the domain builder so the schema can never express
//! a DNS configuration the renderers would reject.

use serde::Deserialize;

/// Traffic sniffing (domain recovery) for connections that arrive as bare IPs.
///
/// Rendered as the Mihomo `sniffer:` section and as sing-box inbound `sniff`
/// flags. Port lists contain validated specs: a single port (`"443"`) or an
/// inclusive range (`"8080-8880"`). The bools map 1:1 onto kernel config keys
/// (serde fields), so packing them into a bitfield would only obscure the
/// schema.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct SnifferConfig {
    /// Master switch; when false no sniffer block is rendered.
    pub enabled: bool,
    /// Use the sniffed domain as the real destination (Mihomo
    /// `override-destination`, sing-box `sniff_override_destination`).
    pub override_destination: bool,
    /// Sniff connections that carry only an IP (Mihomo `parse-pure-ip`).
    pub parse_pure_ip: bool,
    /// Force sniffing for DNS-mapped traffic (Mihomo `force-dns-mapping`).
    pub force_dns_mapping: bool,
    /// Ports sniffed as HTTP.
    pub http_ports: Vec<String>,
    /// Ports sniffed as TLS.
    pub tls_ports: Vec<String>,
    /// Ports sniffed as QUIC.
    pub quic_ports: Vec<String>,
}

impl Default for SnifferConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            override_destination: true,
            parse_pure_ip: true,
            force_dns_mapping: true,
            http_ports: vec!["80".to_owned(), "8080-8880".to_owned()],
            tls_ports: vec!["443".to_owned(), "8443".to_owned()],
            quic_ports: vec!["443".to_owned()],
        }
    }
}

impl SnifferConfig {
    /// Validates one port spec: `N` or `N-M` with 1 <= N <= M <= 65535.
    pub fn is_valid_port_spec(value: &str) -> bool {
        let (start, end) = match value.split_once('-') {
            Some((left, right)) => (left, right),
            None => (value, value),
        };
        match (start.parse::<u16>(), end.parse::<u16>()) {
            (Ok(start), Ok(end)) => start >= 1 && start <= end,
            _ => false,
        }
    }

    /// All port specs across the three protocol lists.
    pub fn all_port_specs(&self) -> impl Iterator<Item = &str> {
        self.http_ports
            .iter()
            .chain(self.tls_ports.iter())
            .chain(self.quic_ports.iter())
            .map(String::as_str)
    }
}

/// Runtime tuning for the managed proxy kernel.
///
/// The values are rendered into the kernel configuration: Mihomo receives
/// `mixed-port`/`allow-lan`/`log-level`, sing-box receives a mixed inbound and
/// the log level. The system-proxy effect targets `mixed_port` by default.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KernelConfig {
    /// Local inbound port (Mihomo `mixed-port`, sing-box mixed inbound).
    pub mixed_port: u16,
    /// Bind the inbound to LAN addresses instead of loopback only.
    pub allow_lan: bool,
    /// LAN bind address when `allow_lan` is true (`"*"` = all interfaces,
    /// or one interface address); loopback while `allow_lan` is false.
    pub bind_address: String,
    /// The kernel's own log level: `trace|debug|info|warn|error`.
    pub log_level: String,
    /// Crash-loop self-healing bounds for automatic core restarts.
    pub restart: RestartConfig,
    /// Transparent-proxy inbound (redirect/tproxy) for gateway-style takeover.
    pub transparent: TransparentConfig,
    /// Core readiness budget for start/restart, in milliseconds (>= 100).
    pub start_timeout_ms: u64,
    /// Graceful stop budget, in milliseconds (>= 100).
    pub stop_timeout_ms: u64,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            mixed_port: 7890,
            allow_lan: false,
            bind_address: "*".to_owned(),
            log_level: "error".to_owned(),
            restart: RestartConfig::default(),
            transparent: TransparentConfig::default(),
            start_timeout_ms: 10_000,
            stop_timeout_ms: 5_000,
        }
    }
}

/// Crash-loop self-healing bounds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RestartConfig {
    /// Backoff before the first auto-restart, in milliseconds (>= 100).
    pub initial_backoff_ms: u64,
    /// Exponential backoff ceiling, in milliseconds (>= initial).
    pub max_backoff_ms: u64,
}

impl Default for RestartConfig {
    fn default() -> Self {
        Self {
            initial_backoff_ms: 1_000,
            max_backoff_ms: 30_000,
        }
    }
}

/// Transparent-proxy inbound mode for gateway-style traffic takeover.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TransparentMode {
    /// TCP `redirect` (NAT) capture; broad compatibility.
    #[default]
    Redirect,
    /// Linux `tproxy`; supports UDP and preserves original addresses.
    Tproxy,
}

impl TransparentMode {
    /// Stable renderer-agnostic label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Redirect => "redirect",
            Self::Tproxy => "tproxy",
        }
    }
}

/// Transparent-proxy inbound configuration (disabled by default).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TransparentConfig {
    /// Master switch; when false no transparent inbound is rendered.
    pub enabled: bool,
    /// Capture mode: redirect or tproxy.
    pub mode: TransparentMode,
    /// Listen port for the transparent inbound.
    pub port: u16,
}

impl Default for TransparentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: TransparentMode::Redirect,
            port: 7_892,
        }
    }
}

impl KernelConfig {
    /// Accepts exactly the kernel log levels both renderers can emit.
    pub fn is_valid_log_level(value: &str) -> bool {
        matches!(value, "trace" | "debug" | "info" | "warn" | "error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_log_level_accepts_only_known_values() {
        assert!(KernelConfig::is_valid_log_level("error"));
        assert!(KernelConfig::is_valid_log_level("trace"));
        assert!(!KernelConfig::is_valid_log_level("verbose"));
    }
}
