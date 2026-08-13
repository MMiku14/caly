//! Config-driven runtime tuning bundle handed to composition at boot.

/// Config-driven runtime tuning resolved once at boot and shared by the
/// renderers, the subscription fetch path and platform effects.
#[derive(Clone, Debug)]
pub struct RuntimeTuning {
    /// Rendered DNS feature (None = no DNS block in the core config).
    pub dns: Option<caly_dns::DnsSettings>,
    /// Local inbound port (Mihomo `mixed-port`, sing-box mixed inbound).
    pub mixed_port: u16,
    /// Bind the inbound to LAN instead of loopback only.
    pub allow_lan: bool,
    /// LAN bind address when `allow_lan` is true (`"*"` = all interfaces).
    pub bind_address: String,
    /// Kernel log level rendered into the core config.
    pub log_level: String,
    /// Subscription HTTP fetch policy bounds.
    pub fetch_policy: caly_subscription::FetchPolicy,
    /// Desktop system-proxy endpoint host.
    pub system_proxy_host: String,
    /// Desktop system-proxy endpoint port.
    pub system_proxy_port: u16,
    /// Privilege escalation for the TUN `ip` commands.
    pub tun_escalation: caly_platform::tun::TunEscalation,
    /// First crash-restart backoff in milliseconds.
    pub restart_initial_backoff_ms: u64,
    /// Crash-restart backoff ceiling in milliseconds.
    pub restart_max_backoff_ms: u64,
    /// Config-driven routing rules rendered into the core config.
    pub rules: Vec<caly_domain::RoutingRule>,
    /// The config's declared `proxy_groups:` in the domain model
    /// (2026-08-12 组源统一): rendered into the kernel ahead of the
    /// subscription-author groups, so `node pick` on a config group no
    /// longer passes the offline check and then 404s in the kernel.
    pub declared_groups: Vec<caly_domain::ProxyGroup>,
    /// User-declared rule providers (rendered into `rule-providers:` /
    /// `route.rule_set`). Auto-emitted GEOIP / GEOSITE rule-sets are
    /// derived from `rules` and do not appear here.
    pub rule_providers: Vec<caly_domain::RuleProvider>,
    /// Traffic sniffing (domain recovery) rendered into the core config.
    pub sniffer: caly_profile::schema::SnifferConfig,
    /// Transparent-proxy inbound settings (redirect/tproxy).
    pub transparent: caly_profile::schema::TransparentConfig,
    /// Core readiness budget for start/restart, in milliseconds.
    pub start_timeout_ms: u64,
    /// Graceful stop budget, in milliseconds.
    pub stop_timeout_ms: u64,
    /// Engage the desktop system proxy during boot.
    pub system_proxy_enabled: bool,
}

impl RuntimeTuning {
    /// Crash-restart backoff bounds as a single handoff value.
    pub const fn restart_backoffs(&self) -> (u64, u64) {
        (self.restart_initial_backoff_ms, self.restart_max_backoff_ms)
    }
}

impl Default for RuntimeTuning {
    fn default() -> Self {
        Self {
            dns: None,
            mixed_port: 7890,
            allow_lan: false,
            bind_address: "*".to_owned(),
            log_level: "error".to_owned(),
            fetch_policy: caly_subscription::FetchPolicy::direct_default(),
            system_proxy_host: "127.0.0.1".to_owned(),
            system_proxy_port: 7890,
            tun_escalation: caly_platform::tun::TunEscalation::default(),
            restart_initial_backoff_ms: 1_000,
            restart_max_backoff_ms: 30_000,
            rules: Vec::new(),
            declared_groups: Vec::new(),
            rule_providers: Vec::new(),
            sniffer: caly_profile::schema::SnifferConfig::default(),
            transparent: caly_profile::schema::TransparentConfig::default(),
            start_timeout_ms: 10_000,
            stop_timeout_ms: 5_000,
            system_proxy_enabled: false,
        }
    }
}
