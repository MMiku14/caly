//! Versioned application configuration parsing and safety validation.

use caly_domain::{BoundedText, SecretText};
use serde::Deserialize;

mod dns;
mod error;
mod fragments_dns;
mod fragments_profiles;
mod fragments_proxy_groups;
mod fragments_routing;
mod kernel;
mod profile;
mod proxy_group;
mod settings;
mod validate;

pub use dns::{DnsSchemaConfig, DnsSchemaError, FallbackFilterConfig, validated_dns_settings};
pub use kernel::{KernelConfig, RestartConfig, SnifferConfig, TransparentConfig, TransparentMode};
pub use profile::{ProfileConfig, ProfileSourceConfig};
pub use proxy_group::{
    ProxyGroupConfig, ProxyGroupConfigError, ProxyGroupMemberConfig, ProxyGroupTypeConfig,
    UrlTestConfigConfig,
};
pub use validate::validate;

pub use settings::{
    ControllersConfig, CoreBinariesConfig, LogConfig, ProviderConfig, ProviderKind,
    RuleProviderBehaviorConfig, RuleProviderConfig, RuleProviderFormatConfig,
    RuleProviderSourceConfig, SubscriptionConfig, SubscriptionSource, TelemetryConfig,
    render_default_base, render_default_config, render_default_config_files,
};

/// Maximum accepted source configuration size.
pub const MAX_CONFIG_BYTES: usize = 16 * 1_024 * 1_024;

/// Complete validated bootstrap configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub schema_version: u32,
    pub core: CoreConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub controllers: ControllersConfig,
    #[serde(default)]
    pub core_binaries: CoreBinariesConfig,
    #[serde(default)]
    pub subscriptions: SubscriptionConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub tun: TunConfig,
    #[serde(default)]
    pub system_proxy: SystemProxyConfig,
    #[serde(default)]
    pub kernel: KernelConfig,
    #[serde(default)]
    pub dns: DnsSchemaConfig,
    /// Ordered Clash-format routing rules (`TYPE,VALUE,POLICY`); first match
    /// wins. Rendered into Mihomo `rules:` and sing-box `route.rules`.
    #[serde(default)]
    pub rules: Vec<String>,
    /// Traffic sniffing (domain recovery) rendered into the core config.
    #[serde(default)]
    pub sniffer: SnifferConfig,
    /// Explicit providers. Unset derives a `default` enumeration provider containing
    /// every enabled subscription source (see [`Self::resolved_providers`]).
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// User-defined rule providers. Each entry renders as one
    /// `rule-providers:` block in the Mihomo config and one `route.rule_set`
    /// entry in the sing-box config; rules can then reference the
    /// provider by tag through `RULE-SET,<name>,<policy>`.
    #[serde(default)]
    pub rule_providers: Vec<RuleProviderConfig>,
    /// User-declared profile set. Each entry is one of Remote / Local /
    /// Merge; the loader resolves Remote bodies to a cache at
    /// `<state>/profiles/<id>.yaml`, validates merge cycles, and
    /// applies the profiles in declaration order on top of the
    /// base config and the `config.d/` fragments. See
    /// `crates/caly-config/src/profile_store.rs` for the cache
    /// layout and `crates/caly-config/src/loader/layered.rs` for
    /// the merge order.
    #[serde(default)]
    pub profiles: Vec<ProfileConfig>,
    /// User-declared proxy groups. Each entry renders as one
    /// Mihomo `proxy-groups:` block (or as the sing-box
    /// `outbounds` selector / urltest / fallback / loadbalance /
    /// relay tag). Groups are referenced from `rules:` and from
    /// each other's `members:` lists; the schema validator
    /// enforces unique names, member-reference resolution, and
    /// the absence of `relay` cycles.
    #[serde(default)]
    pub proxy_groups: Vec<ProxyGroupConfig>,
}

impl AppConfig {
    /// Resolves the effective provider set: explicit `providers:` entries when
    /// present, otherwise the auto `default` enumeration provider containing
    /// every enabled subscription source. A config with only inline node
    /// content (no URL sources) resolves to an empty set.
    pub fn resolved_providers(&self) -> Vec<ProviderConfig> {
        if self.providers.is_empty() {
            if let Some(default_provider) = self.subscriptions.default_provider() {
                return vec![default_provider];
            }
            return Vec::new();
        }
        self.providers.clone()
    }
}

/// Managed proxy-core selection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CoreConfig {
    Mihomo,
    SingBox,
    Xray,
}

/// Daemon transport security settings.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub listen: String,
    pub tls_enabled: bool,
    /// PEM server certificate chain presented on the TLS listener.
    /// Required when `tls_enabled` is true (#57).
    pub tls_cert_path: Option<String>,
    /// PEM private key matching `tls_cert_path`. Required when
    /// `tls_enabled` is true.
    pub tls_key_path: Option<String>,
    pub auth_token: Option<SecretText<4_096>>,
    /// Whether the daemon engages the configured core during boot. A failed
    /// auto-start never aborts the daemon: it serves with a stopped core and
    /// `caly core start` retries on demand.
    pub auto_start_core: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:17890".to_owned(),
            tls_enabled: false,
            tls_cert_path: None,
            tls_key_path: None,
            auth_token: None,
            auto_start_core: true,
        }
    }
}

/// TUN protocol stack; `deny_unknown_fields` keeps the enum closed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TunStack {
    #[default]
    Gvisor,
    Mixed,
    System,
}

/// TUN intent; runtime engagement remains platform-owned.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TunConfig {
    pub enabled: bool,
    pub mtu: u16,
    /// User-space packet-stack implementation (gvisor/mixed/system).
    pub stack: TunStack,
    /// Whether the daemon installs routes automatically.
    pub auto_route: bool,
    /// Whether routing is strict (force all traffic through TUN).
    pub strict_route: bool,
    /// Privilege escalation for the privileged `ip` commands when the daemon
    /// lacks CAP_NET_ADMIN: `auto|pkexec|sudo|none`.
    pub escalation: String,
}

impl Default for TunConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mtu: 1_500,
            stack: TunStack::default(),
            auto_route: true,
            strict_route: true,
            escalation: "auto".to_owned(),
        }
    }
}

/// System-proxy intent; original platform state is not represented here.
///
/// `host`/`port` describe the local proxy endpoint the desktop environment
/// should use. The port defaults to `kernel.mixed_port` at resolution time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SystemProxyConfig {
    pub enabled: bool,
    /// Address the desktop proxy settings point at.
    pub host: String,
    /// Port the desktop proxy settings point at; `None` resolves to
    /// `kernel.mixed_port`.
    pub port: Option<u16>,
}

impl Default for SystemProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: None,
        }
    }
}

/// Safe configuration rejection with user remediation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    TooLarge {
        limit: usize,
        actual: usize,
    },
    Parse {
        line: usize,
        column: usize,
        message: BoundedText<512>,
    },
    ParseDetailsUnavailable,
    UnsupportedSchemaVersion(u32),
    InvalidListenAddress,
    RemoteListenRequiresTls,
    TlsMaterialMissing,
    RemoteListenRequiresAuth,
    InvalidControllerAddress {
        core: String,
        address: String,
    },
    InvalidLogLevel,
    InvalidTelemetryInterval,
    InvalidSubscriptionUrl,
    InvalidTunMtu,
    /// `kernel.mixed_port` must be a non-zero port.
    InvalidKernelPort,
    /// `kernel.log_level` is not a level both kernels understand.
    InvalidKernelLogLevel,
    /// `kernel.restart` bounds are invalid.
    InvalidKernelRestart,
    /// `kernel.start_timeout_ms`/`stop_timeout_ms` are invalid.
    InvalidKernelTimeout,
    /// Transparent proxy is enabled with a zero port.
    InvalidTransparentPort,
    /// An enabled `dns` section violates a domain invariant.
    InvalidDns(DnsSchemaError),
    /// `dns.listen` is not a parseable socket address.
    InvalidDnsListen,
    /// A `fallback_filter.ipcidr` entry is not a valid CIDR block.
    InvalidDnsCidrFilter,
    /// One subscription fetch-policy field is out of range; names the
    /// offending field and its accepted range so the operator can fix the
    /// exact value instead of guessing which of the three knobs failed.
    InvalidFetchPolicy {
        field: &'static str,
        expected: &'static str,
    },
    /// System proxy host/port are invalid.
    InvalidSystemProxy,
    /// `tun.escalation` is not one of auto/pkexec/sudo/none.
    InvalidTunEscalation,
    /// A routing rule failed to parse; carries the rule index and reason.
    InvalidRule {
        index: usize,
        reason: String,
    },
    /// A sniffer port spec is not `N` or `N-M` within 1..=65535.
    InvalidSnifferPort {
        spec: String,
    },
    /// A `RULE-SET,<name>,…` rule references a provider that is neither
    /// defined under `rule_providers:` nor auto-emitted for the rule body
    /// (only `GEOIP,<cc>,…` rules are auto-resolved to a SagerNet rule-set
    /// with no user-side declaration).
    UnknownRuleProvider {
        name: String,
        rule_index: usize,
    },
    /// Two `rule_providers:` entries share a tag. Tags are referenced from
    /// rules and must be unique within one document.
    DuplicateRuleProvider {
        name: String,
    },
    /// A `rule_providers:` tag is not a path-safe component. The
    /// application layer materialises inline payloads under
    /// `<state>/rule-providers/<name>.yaml`, so the tag obeys the same
    /// path rule as profile ids and proxy group names.
    InvalidRuleProviderName {
        name: String,
    },
    /// A `providers:` entry failed structural validation (empty or
    /// duplicated name, unparseable inline node URI, budget exceeded).
    InvalidProvider {
        name: String,
        reason: String,
    },
    /// A `rule_providers:` `type: http` URL is missing the scheme or host,
    /// or points at a private/loopback address (SSRF containment, mirrored
    /// from `subscriptions.url` validation).
    InvalidRuleProviderUrl {
        name: String,
        url: String,
    },
    /// A `rule_providers:` `type: file` path is empty.
    EmptyRuleProviderPath {
        name: String,
    },
    /// A `rule_providers:` `type: inline` payload is empty.
    EmptyRuleProviderPayload {
        name: String,
    },
    /// A `rule_providers:` `type: http` `interval_ms` is below the polling
    /// minimum (60s). Below that the kernel would hammer the upstream.
    InvalidRuleProviderInterval {
        name: String,
        interval_ms: u64,
    },
    /// A `profiles:` entry or reference failed a structural check
    /// (unknown id, duplicate id, merge cycle, escape path, …).
    /// The nested [`caly_domain::ProfileError`] carries the precise
    /// reason so the daemon's user-facing diagnostic names the
    /// profile by id and the rule that triggered it.
    Profile(caly_domain::ProfileError),
    /// A `proxy_groups:` entry or reference failed a structural
    /// check (unknown name, duplicate name, missing `url_test:`,
    /// unresolved member reference, relay cycle, …). The
    /// nested [`caly_domain::ProxyGroupError`] carries the
    /// precise reason.
    ProxyGroup(caly_domain::ProxyGroupError),
}

/// Parses YAML and validates all cross-field bootstrap invariants.
pub fn parse_and_validate_yaml(bytes: &[u8]) -> Result<AppConfig, ConfigError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge {
            limit: MAX_CONFIG_BYTES,
            actual: bytes.len(),
        });
    }
    let source = core::str::from_utf8(bytes).map_err(|_| ConfigError::ParseDetailsUnavailable)?;
    let config: AppConfig =
        serde_norway::from_str(source).map_err(|error| validate::parse_yaml_error(&error))?;
    validate(&config)?;
    Ok(config)
}

impl From<caly_domain::ProfileError> for ConfigError {
    fn from(value: caly_domain::ProfileError) -> Self {
        Self::Profile(value)
    }
}

impl From<caly_domain::ProxyGroupError> for ConfigError {
    fn from(value: caly_domain::ProxyGroupError) -> Self {
        Self::ProxyGroup(value)
    }
}

/// Parses JSON for compatibility and validates all cross-field invariants.
pub fn parse_and_validate_json(bytes: &[u8]) -> Result<AppConfig, ConfigError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge {
            limit: MAX_CONFIG_BYTES,
            actual: bytes.len(),
        });
    }
    let config: AppConfig = serde_json::from_slice(bytes).map_err(validate::parse_error)?;
    validate(&config)?;
    Ok(config)
}

/// Validates an already decoded configuration.
#[cfg(test)]
mod tests;

#[cfg(test)]
mod kernel_validation_tests;
