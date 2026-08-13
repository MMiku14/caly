//! Minimal Mihomo YAML generation (pure; publication is the caller's job).

pub mod proxy_sections;
pub mod render;

use std::collections::BTreeSet;

use caly_dns::{DnsMode, DnsSettings};
use caly_domain::{BoundedText, BoundedVec, TunConfig};

/// Maximum rendered bytes for the subscription proxy section (proxies, groups,
/// rules). Bounded to keep a single generation within the atomic-file limit.
pub const MIHOMO_CONFIG_SECTION_MAX_BYTES: usize = 4_000_000;
/// Maximum rendered bytes for a whole Mihomo config generation. Byte-equal to
/// `caly_platform::fs::MAX_ATOMIC_FILE_BYTES` at split time (P3a); this crate
/// must not import platform, so the budget is restated here.
pub const MIHOMO_CONFIG_MAX_BYTES: usize = 16 * 1_024 * 1_024;
/// Rendered Mihomo config bytes (bounded like the platform atomic-write budget).
pub type MihomoConfigBytes = BoundedVec<u8, MIHOMO_CONFIG_MAX_BYTES>;

/// Minimal bounded Mihomo configuration input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MihomoConfigSettings {
    pub mixed_port: u16,
    pub external_controller_port: u16,
    pub allow_lan: bool,
    /// LAN bind address rendered when `allow_lan` is true (`"*"` = all).
    pub bind_address: String,
    /// Mihomo `log-level`: `debug|info|warning|error|silent`.
    pub log_level: String,
    pub dns: Option<DnsSettings>,
    /// Shared controller auth secret; embedded in config and sent by clients.
    /// It must never be projected to clients or logs.
    pub secret: Option<String>,
    /// Optional pre-rendered `proxies:`/`proxy-groups:`/`rules:` YAML section
    /// produced by the subscription renderer. Absent yields a base-only config.
    pub proxies: Option<BoundedText<MIHOMO_CONFIG_SECTION_MAX_BYTES>>,
    /// Optional TUN device configuration (stack/auto-route/strict-route).
    pub tun: Option<TunConfig>,
    /// Transparent inbound port; 0 disables. Rendered as `redir-port` unless
    /// `transparent_tproxy` selects `tproxy-port`.
    pub transparent_port: u16,
    /// Use `tproxy-port` instead of `redir-port` when transparent is enabled.
    pub transparent_tproxy: bool,
    /// Optional traffic sniffer block (domain recovery for bare-IP traffic).
    pub sniffer: Option<MihomoSniffer>,
}

/// Validated sniffer tuning rendered as the Mihomo `sniffer:` section. Port
/// specs are pre-validated by the config schema (`N` or `N-M`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MihomoSniffer {
    pub override_destination: bool,
    pub parse_pure_ip: bool,
    pub force_dns_mapping: bool,
    pub http_ports: Vec<String>,
    pub tls_ports: Vec<String>,
    pub quic_ports: Vec<String>,
}

impl Default for MihomoConfigSettings {
    fn default() -> Self {
        Self {
            mixed_port: 78_90,
            external_controller_port: 90_90,
            allow_lan: false,
            bind_address: "*".to_owned(),
            log_level: "info".to_owned(),
            dns: None,
            secret: None,
            proxies: None,
            tun: None,
            transparent_port: 0,
            transparent_tproxy: false,
            sniffer: None,
        }
    }
}

/// Maps the generic kernel log level onto Mihomo's vocabulary. Mihomo knows
/// `debug|info|warning|error|silent`; `trace` lands on debug, `warn` on
/// warning, and anything unexpected falls back to info.
fn mihomo_log_level(level: &str) -> &'static str {
    match level {
        "trace" | "debug" => "debug",
        "info" => "info",
        "warn" => "warning",
        "error" | "silent" => "error",
        _ => "info",
    }
}

/// Generates and durably publishes a minimal Mihomo YAML config.
#[derive(Default)]
pub struct MihomoConfigRenderer;

/// Configuration generation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MihomoConfigError {
    InvalidPort,
    InvalidSecret,
    OutputTooLarge,
    InvalidGeneratedConfig,
}

impl core::fmt::Display for MihomoConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidPort => {
                formatter.write_str("mixed-port and controller port must be non-zero")
            }
            Self::InvalidSecret => formatter.write_str("controller secret must not be empty"),
            Self::OutputTooLarge => formatter.write_str("rendered config is too large"),
            Self::InvalidGeneratedConfig => {
                formatter.write_str("generated config failed validation")
            }
        }
    }
}

impl MihomoConfigRenderer {
    /// Renders a bounded YAML document and validates required fields.
    pub fn render(
        &self,
        settings: &MihomoConfigSettings,
    ) -> Result<MihomoConfigBytes, MihomoConfigError> {
        if settings.mixed_port == 0 || settings.external_controller_port == 0 {
            return Err(MihomoConfigError::InvalidPort);
        }
        let mut yaml = format!(
            "mixed-port: {}\nallow-lan: {}\nmode: rule\nlog-level: {}\nexternal-controller: 127.0.0.1:{}\n",
            settings.mixed_port,
            settings.allow_lan,
            mihomo_log_level(&settings.log_level),
            settings.external_controller_port
        );
        if settings.allow_lan {
            yaml.push_str("bind-address: ");
            yaml.push_str(&settings.bind_address);
            yaml.push('\n');
        }
        if settings.transparent_port != 0 {
            let key = if settings.transparent_tproxy {
                "tproxy-port"
            } else {
                "redir-port"
            };
            yaml.push_str(key);
            yaml.push_str(": ");
            yaml.push_str(&settings.transparent_port.to_string());
            yaml.push('\n');
        }
        if let Some(secret) = &settings.secret {
            if secret.is_empty() {
                return Err(MihomoConfigError::InvalidSecret);
            }
            yaml.push_str("secret: ");
            yaml.push_str(secret);
            yaml.push('\n');
        }
        if let Some(dns) = &settings.dns {
            yaml.push_str(&render_dns(dns, &server_domains(settings.proxies.as_ref())));
        } else if settings.tun.is_some() {
            // W3a 兜底: a TUN block with `dns-hijack: [any:53]` but no
            // `dns:` section would send every hijacked query nowhere —
            // inject the built-in default (fake-ip + public upstreams).
            let fallback = caly_dns::default_tun_dns()
                .map_err(|_error| MihomoConfigError::InvalidGeneratedConfig)?;
            if let Some(dns) = fallback {
                yaml.push_str(&render_dns(
                    &dns,
                    &server_domains(settings.proxies.as_ref()),
                ));
            }
        }
        if let Some(tun) = &settings.tun {
            yaml.push_str(&render_tun(tun));
        }
        if let Some(sniffer) = &settings.sniffer {
            yaml.push_str(&render_sniffer(sniffer));
        }
        if let Some(proxies) = &settings.proxies {
            yaml.push_str(proxies.as_str());
            if !proxies.as_str().ends_with('\n') {
                yaml.push('\n');
            }
        }
        let bytes = BoundedVec::try_from_vec(yaml.into_bytes())
            .map_err(|_| MihomoConfigError::OutputTooLarge)?;
        self.validate(bytes.as_slice())?;
        Ok(bytes)
    }

    /// Validates the minimal generated document before publication.
    pub fn validate(&self, bytes: &[u8]) -> Result<(), MihomoConfigError> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
        for required in ["mixed-port:", "allow-lan:", "mode:", "external-controller:"] {
            if !text.lines().any(|line| line.starts_with(required)) {
                return Err(MihomoConfigError::InvalidGeneratedConfig);
            }
        }
        if text.lines().any(|line| line.starts_with("dns:")) && !text.contains("enhanced-mode:") {
            return Err(MihomoConfigError::InvalidGeneratedConfig);
        }
        Ok(())
    }
}

/// Collects hostnames from `server:` lines of rendered proxy YAML so they
/// can be pinned into fake-ip-filter.
///
/// Pure IPs are skipped (they never consult DNS). The result is sorted and
/// deduplicated; the renderer strips entries that duplicate the user's own
/// filter list before emitting them.
fn server_domains(
    proxy_yaml: Option<&BoundedText<MIHOMO_CONFIG_SECTION_MAX_BYTES>>,
) -> Vec<String> {
    let Some(yaml) = proxy_yaml else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    for line in yaml.as_str().lines() {
        let Some(rest) = line.trim_start().strip_prefix("server:") else {
            continue;
        };
        let host = rest.trim().trim_matches(['"', '\'']);
        if host.is_empty() || host.contains(' ') || host.parse::<std::net::IpAddr>().is_ok() {
            continue;
        }
        seen.insert(host.to_owned());
    }
    seen.into_iter().collect()
}

/// Renders a Mihomo `tun:` block from a core-agnostic TUN configuration.
///
/// Follows the Mihomo docs: `auto-detect-interface` excludes TUN-bound traffic
/// from re-entering TUN, and `dns-hijack: [any:53]` redirects DNS into the
/// internal resolver so routing sees real domain names. `device` is pinned to
/// the platform TUN name (`caly0`, see `caly_application::...::lifecycle`)
/// instead of Mihomo's default `Meta`: the platform layer manages that exact
/// device (probe/delete/adopt), and a mismatched name leaves a second
/// unmanaged device behind while the rules point at the core-owned one.
fn render_tun(tun: &TunConfig) -> String {
    format!(
        "tun:\n  enable: true\n  device: caly0\n  stack: {}\n  auto-route: {}\n  strict-route: {}\n  auto-detect-interface: true\n  dns-hijack:\n    - any:53\n  mtu: {}\n",
        tun.stack().label(),
        tun.auto_route(),
        tun.strict_route(),
        tun.mtu()
    )
}

/// Renders a Mihomo `sniffer:` block. Port specs were validated by the config
/// schema (`N` or `N-M`), so they are embedded verbatim. Empty protocol lists
/// are omitted; with no ports at all the whole block is skipped rather than
/// emitting a `sniff:` key with no children.
fn render_sniffer(sniffer: &MihomoSniffer) -> String {
    let protocols: [(&str, &Vec<String>); 3] = [
        ("HTTP", &sniffer.http_ports),
        ("TLS", &sniffer.tls_ports),
        ("QUIC", &sniffer.quic_ports),
    ];
    if protocols.iter().all(|(_, ports)| ports.is_empty()) {
        return String::new();
    }
    let mut yaml = format!(
        "sniffer:\n  enable: true\n  override-destination: {}\n  parse-pure-ip: {}\n  force-dns-mapping: {}\n  sniff:\n",
        sniffer.override_destination, sniffer.parse_pure_ip, sniffer.force_dns_mapping
    );
    for (protocol, ports) in protocols {
        if ports.is_empty() {
            continue;
        }
        yaml.push_str("    ");
        yaml.push_str(protocol);
        yaml.push_str(":\n      ports: [");
        yaml.push_str(&ports.join(", "));
        yaml.push_str("]\n");
    }
    yaml
}

/// Pushes a `key:` server-list block; empty groups are omitted (only
/// `nameserver` is mandatory, enforced at `DnsSettings` build time).
fn push_server_block(lines: &mut Vec<String>, key: &str, servers: &[caly_dns::Nameserver]) {
    if servers.is_empty() {
        return;
    }
    lines.push(format!("  {key}:"));
    for server in servers {
        lines.push(format!("    - {}", server.as_str()));
    }
}

fn render_dns(dns: &DnsSettings, node_domains: &[String]) -> String {
    let mode = match dns.mode() {
        DnsMode::Standard => "none",
        DnsMode::FakeIp => "fake-ip",
        DnsMode::RedirHost => "redir-host",
    };
    let listen = dns.listen().map_or_else(
        || "0.0.0.0:53".to_owned(),
        |value| value.as_str().to_owned(),
    );
    let mut lines = vec![
        "dns:".to_owned(),
        "  enable: true".to_owned(),
        format!("  enhanced-mode: {mode}"),
        format!("  listen: {listen}"),
    ];
    if dns.ipv6() {
        lines.push("  ipv6: true".to_owned());
    }
    push_server_block(&mut lines, "nameserver", dns.nameservers());
    push_server_block(&mut lines, "fallback", dns.fallback());
    // B4: the direct group was collected by the model but never rendered —
    // a dead configuration. mihomo's `direct-nameserver` feeds DIRECT-rule
    // domain lookups; emit it between fallback and default-nameserver.
    push_server_block(&mut lines, "direct-nameserver", dns.direct());
    push_server_block(&mut lines, "default-nameserver", dns.default());
    if let Some(range) = dns.fake_ip_range() {
        lines.push(format!("  fake-ip-range: {}", range.as_str()));
    }
    if !dns.fake_ip_filter().is_empty() || !node_domains.is_empty() {
        lines.push("  fake-ip-filter:".to_owned());
        for pattern in dns.fake_ip_filter() {
            lines.push(format!("    - '{}'", pattern.as_str()));
        }
        // Node hosts must resolve to REAL IPs: a fake-ip answer for a proxy
        // server means the core dials 198.18.x.x and every node dies. The
        // user filter wins on duplicates (strip `+.`/`*.` prefixes while
        // comparing, and skip when any user entry contains the host).
        let user = dns
            .fake_ip_filter()
            .iter()
            .map(|p| {
                let s = p.as_str();
                s.strip_prefix("+.")
                    .or_else(|| s.strip_prefix("*."))
                    .unwrap_or(s)
            })
            .collect::<BTreeSet<_>>();
        for domain in node_domains {
            if user.contains(domain.as_str()) {
                continue;
            }
            lines.push(format!("    - '+.{domain}'"));
        }
    }
    if let Some(filter) = dns.fallback_filter() {
        lines.push("  fallback-filter:".to_owned());
        if filter.geoip() {
            lines.push("    geoip: true".to_owned());
            if let Some(code) = filter.geoip_code() {
                lines.push(format!("    geoip-code: {}", code.as_str()));
            }
        }
        if !filter.ipcidr().is_empty() {
            lines.push("    ipcidr:".to_owned());
            for cidr in filter.ipcidr() {
                lines.push(format!("      - {}", cidr.as_str()));
            }
        }
        if !filter.domain().is_empty() {
            lines.push("    domain:".to_owned());
            for domain in filter.domain() {
                lines.push(format!("      - '{}'", domain.as_str()));
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod config_tests;
