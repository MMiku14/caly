//! DNS feature schema: the `dns:` section mapped onto the bounded domain
//! model, including fake-ip bypass patterns and the anti-poisoning
//! fallback-filter. Kept out of `kernel.rs` for the file-size budget.

use caly_dns::{
    DnsError, DnsMode, DnsSettings, DnsSettingsBuilder, FallbackFilter, MAX_DNS_SERVERS,
};
use serde::Deserialize;

/// Configuration-driven DNS feature rendered into the kernel config.
///
/// Mirrors the `CALY_DNS_*` environment knobs; the environment still wins
/// when `CALY_DNS_ENABLE` is set, matching the controller/log precedence.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DnsSchemaConfig {
    /// Master switch; when false the kernel config carries no DNS block.
    pub enabled: bool,
    /// One of `standard`, `fake-ip`, `redir-host`.
    pub mode: String,
    /// Primary upstream resolvers (at least one when enabled; max 16).
    pub nameservers: Vec<String>,
    /// Anti-poisoning fallback resolvers.
    pub fallback: Vec<String>,
    /// Resolvers for direct (non-proxied) lookups.
    pub direct: Vec<String>,
    /// Default resolvers applied to unmatched queries.
    pub default: Vec<String>,
    /// CIDR for synthesized answers; required in `fake-ip` mode.
    pub fake_ip_range: Option<String>,
    /// Domains that must receive real IPs even in fake-ip mode (STUN/NTP/
    /// NCSI/mDNS etc.; see the generated 40-dns fragment defaults).
    pub fake_ip_filter: Vec<String>,
    /// Resolve AAAA records in the kernel DNS engine.
    pub ipv6: bool,
    /// DNS listener socket address (`host:port`); absent keeps the renderer
    /// default. Ports below 1024 require CAP_NET_BIND_SERVICE.
    pub listen: Option<String>,
    /// Anti-poisoning filter applied to fallback answers.
    pub fallback_filter: FallbackFilterConfig,
}

/// Anti-poisoning `fallback-filter`: decides when a `nameserver` answer is
/// treated as poisoned and the `fallback` answer wins.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FallbackFilterConfig {
    /// Compare answer GeoIP against `geoip_code`.
    pub geoip: bool,
    /// Expected country code for non-poisoned answers (e.g. `CN`).
    pub geoip_code: Option<String>,
    /// CIDRs whose answers are treated as poisoned (e.g. `240.0.0.0/4`).
    pub ipcidr: Vec<String>,
    /// Domains forced through the fallback resolvers.
    pub domain: Vec<String>,
}

impl DnsSchemaConfig {
    /// Maps the schema section onto the bounded domain model.
    ///
    /// Returns `None` when the feature is disabled. An enabled section that
    /// violates a domain invariant (bad mode, empty nameservers, missing
    /// fake-ip range, oversized list) maps to `None` as well; callers that
    /// must distinguish use `validated_dns_settings`.
    pub fn to_settings(&self) -> Option<DnsSettings> {
        validated_dns_settings(self).ok().flatten()
    }
}

/// Strict mapping used by schema validation: reports *why* an enabled DNS
/// section is unusable instead of silently dropping it.
pub fn validated_dns_settings(
    config: &DnsSchemaConfig,
) -> Result<Option<DnsSettings>, DnsSchemaError> {
    if !config.enabled {
        return Ok(None);
    }
    let mode = match config.mode.as_str() {
        "standard" => DnsMode::Standard,
        "fake-ip" => DnsMode::FakeIp,
        "redir-host" => DnsMode::RedirHost,
        _ => return Err(DnsSchemaError::InvalidMode),
    };
    let mut builder = DnsSettingsBuilder::new().enabled(true).mode(mode);
    for value in &config.nameservers {
        builder = builder.push_nameserver(value).map_err(schema_dns_error)?;
    }
    for value in &config.fallback {
        builder = builder.push_fallback(value).map_err(schema_dns_error)?;
    }
    for value in &config.direct {
        builder = builder.push_direct(value).map_err(schema_dns_error)?;
    }
    for value in &config.default {
        builder = builder.push_default(value).map_err(schema_dns_error)?;
    }
    if let Some(range) = &config.fake_ip_range {
        builder = builder
            .fake_ip_range(range)
            .map_err(|_| DnsSchemaError::InvalidFakeIpRange)?;
    }
    if !config.fake_ip_filter.is_empty() {
        builder = builder
            .fake_ip_filter(&config.fake_ip_filter)
            .map_err(|_| DnsSchemaError::InvalidFilter)?;
    }
    builder = builder.ipv6(config.ipv6);
    if let Some(listen) = &config.listen {
        builder = builder
            .listen(listen)
            .map_err(|_| DnsSchemaError::InvalidListen)?;
    }
    let filter = &config.fallback_filter;
    if filter.geoip
        || filter.geoip_code.is_some()
        || !filter.ipcidr.is_empty()
        || !filter.domain.is_empty()
    {
        let built = FallbackFilter::new(
            filter.geoip,
            filter.geoip_code.clone(),
            filter.ipcidr.clone(),
            filter.domain.clone(),
        )
        // #122: the CIDR shape check moved into the domain model (single
        // home, closing the B6-class bypass); keep the dedicated variant so
        // the diagnostic still names `fallback_filter.ipcidr`.
        .map_err(|error| match error {
            caly_dns::DnsFilterError::InvalidIpCidr => DnsSchemaError::InvalidIpCidr,
            _ => DnsSchemaError::InvalidFilter,
        })?;
        builder = builder.fallback_filter(built);
    }
    builder.build().map_err(|_| DnsSchemaError::DomainInvariant)
}

/// Maps a domain build failure onto the schema surface, keeping a capacity
/// overflow distinct from a malformed single entry (the pre-fix umbrella
/// mapping blamed the *value* when the real problem was the *count*).
fn schema_dns_error(error: DnsError) -> DnsSchemaError {
    match error {
        DnsError::TooManyNameservers => DnsSchemaError::TooManyServers,
        _ => DnsSchemaError::InvalidNameserver,
    }
}

/// Why an enabled `dns` section failed to map onto the domain model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsSchemaError {
    /// `dns.mode` is not one of standard/fake-ip/redir-host.
    InvalidMode,
    /// A nameserver entry is not a valid resolver reference.
    InvalidNameserver,
    /// A nameserver group exceeds the bounded capacity.
    TooManyServers,
    /// `fake_ip_range` is not a valid CIDR block.
    InvalidFakeIpRange,
    /// The domain builder rejected the combination (e.g. enabled without
    /// nameservers, fake-ip without a range, too many servers).
    DomainInvariant,
    /// A filter entry (fake-ip-filter / fallback-filter) is invalid.
    InvalidFilter,
    /// `dns.listen` is not a valid `host:port` socket address.
    InvalidListen,
    /// A `fallback_filter.ipcidr` entry is not CIDR-shaped (#122: rejected
    /// by the domain model's single home; surfaced with its own variant so
    /// `ConfigError::InvalidDnsCidrFilter` keeps naming the field).
    InvalidIpCidr,
}

impl core::fmt::Display for DnsSchemaError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidMode => {
                formatter.write_str("dns.mode must be standard, fake-ip, or redir-host")
            }
            Self::InvalidNameserver => {
                formatter.write_str("a dns nameserver entry is invalid (bad resolver reference)")
            }
            Self::TooManyServers => {
                write!(
                    formatter,
                    "a dns nameserver group exceeds {MAX_DNS_SERVERS} entries"
                )
            }
            Self::InvalidFakeIpRange => {
                formatter.write_str("dns.fake_ip_range must be a CIDR like 198.18.0.1/16")
            }
            Self::DomainInvariant => formatter.write_str(
                "dns is enabled but incomplete: need at least one nameserver, and a \
                 fake_ip_range in fake-ip mode",
            ),
            Self::InvalidFilter => formatter.write_str(
                "dns filter entry is invalid: patterns must be non-empty and bounded, \
                 fallback-filter CIDRs must be valid blocks",
            ),
            Self::InvalidListen => formatter
                .write_str("dns.listen must be a valid host:port address (e.g. 127.0.0.1:1053)"),
            Self::InvalidIpCidr => {
                formatter.write_str("fallback_filter.ipcidr entries must be valid CIDR blocks")
            }
        }
    }
}

impl std::error::Error for DnsSchemaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_dns_maps_to_none() {
        let config = DnsSchemaConfig::default();
        assert_eq!(validated_dns_settings(&config), Ok(None));
    }
    #[test]
    fn enabled_dns_requires_nameservers() {
        let config = DnsSchemaConfig {
            enabled: true,
            mode: "standard".to_owned(),
            ..DnsSchemaConfig::default()
        };
        assert_eq!(
            validated_dns_settings(&config),
            Err(DnsSchemaError::DomainInvariant)
        );
    }
    #[test]
    fn fake_ip_mode_requires_range() {
        let config = DnsSchemaConfig {
            enabled: true,
            mode: "fake-ip".to_owned(),
            nameservers: vec!["8.8.8.8".to_owned()],
            ..DnsSchemaConfig::default()
        };
        assert_eq!(
            validated_dns_settings(&config),
            Err(DnsSchemaError::DomainInvariant)
        );
        let with_range = DnsSchemaConfig {
            fake_ip_range: Some("198.18.0.1/16".to_owned()),
            ..config
        };
        let settings = validated_dns_settings(&with_range);
        assert!(matches!(settings, Ok(Some(_))), "got: {settings:?}");
    }
    #[test]
    fn invalid_mode_and_servers_are_reported() {
        let bad_mode = DnsSchemaConfig {
            enabled: true,
            mode: "turbo".to_owned(),
            nameservers: vec!["8.8.8.8".to_owned()],
            ..DnsSchemaConfig::default()
        };
        assert_eq!(
            validated_dns_settings(&bad_mode),
            Err(DnsSchemaError::InvalidMode)
        );
        let bad_server = DnsSchemaConfig {
            enabled: true,
            mode: "standard".to_owned(),
            nameservers: vec![String::new()],
            ..DnsSchemaConfig::default()
        };
        assert_eq!(
            validated_dns_settings(&bad_server),
            Err(DnsSchemaError::InvalidNameserver)
        );
    }

    #[test]
    fn overflowing_a_group_reports_too_many_servers() {
        let config = DnsSchemaConfig {
            enabled: true,
            mode: "standard".to_owned(),
            nameservers: (0..=caly_dns::MAX_DNS_SERVERS)
                .map(|index| format!("10.0.{index}.1"))
                .collect(),
            ..DnsSchemaConfig::default()
        };
        assert_eq!(
            validated_dns_settings(&config),
            Err(DnsSchemaError::TooManyServers),
            "17 declared nameservers must error, not silently truncate"
        );
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn enabled_config() -> DnsSchemaConfig {
        DnsSchemaConfig {
            enabled: true,
            mode: "fake-ip".to_owned(),
            nameservers: vec!["8.8.8.8".to_owned()],
            fake_ip_range: Some("198.18.0.1/16".to_owned()),
            ..DnsSchemaConfig::default()
        }
    }

    /// Pulls the success branch out of the `Result<Option<_>, _>` returned by
    /// `validated_dns_settings`. The previous test versions used
    /// `unwrap_or_else(|| std::process::abort())` which made a happy-path
    /// test failure abort the test binary instead of reporting it as a
    /// regular assertion failure. The helper returns the inner
    /// `DnsSettings` on success, the error string on schema rejection, or a
    /// "disabled" marker on `Ok(None)` so the test author can match the
    /// expected outcome. Tests that consume the helper use `?` and a custom
    /// error type to keep the workspace's `clippy::panic` deny satisfied
    /// without `unwrap_or_else` or `panic!` in the assertion path.
    fn settings_or_error(config: &DnsSchemaConfig) -> Result<DnsSettings, String> {
        match validated_dns_settings(config) {
            Ok(Some(settings)) => Ok(settings),
            Ok(None) => Err("dns section disabled".to_owned()),
            Err(error) => Err(format!("{error}")),
        }
    }

    #[test]
    fn fake_ip_filter_maps_into_settings() -> Result<(), String> {
        let config = DnsSchemaConfig {
            fake_ip_filter: vec!["+.local".to_owned(), "+.stun.*".to_owned()],
            ..enabled_config()
        };
        let settings = settings_or_error(&config)?;
        assert_eq!(settings.fake_ip_filter().len(), 2);
        Ok(())
    }

    #[test]
    fn empty_filter_pattern_is_rejected() {
        let config = DnsSchemaConfig {
            fake_ip_filter: vec![String::new()],
            ..enabled_config()
        };
        assert_eq!(
            validated_dns_settings(&config),
            Err(DnsSchemaError::InvalidFilter)
        );
    }

    #[test]
    fn fallback_filter_maps_geoip_cidr_and_domain() -> Result<(), String> {
        let config = DnsSchemaConfig {
            fallback: vec!["1.1.1.1".to_owned()],
            fallback_filter: FallbackFilterConfig {
                geoip: true,
                geoip_code: Some("CN".to_owned()),
                ipcidr: vec!["240.0.0.0/4".to_owned()],
                domain: vec!["+.google.com".to_owned()],
            },
            ..enabled_config()
        };
        let settings = settings_or_error(&config)?;
        let filter = settings.fallback_filter().ok_or_else(|| {
            "fallback_filter must be set when the filter config is non-empty".to_owned()
        })?;
        assert!(filter.geoip());
        assert_eq!(filter.ipcidr().len(), 1);
        assert_eq!(filter.domain().len(), 1);
        Ok(())
    }

    #[test]
    fn listen_and_ipv6_map_into_settings() -> Result<(), String> {
        let config = DnsSchemaConfig {
            ipv6: true,
            listen: Some("127.0.0.1:1053".to_owned()),
            ..enabled_config()
        };
        let settings = settings_or_error(&config)?;
        assert!(settings.ipv6());
        let listen = settings
            .listen()
            .ok_or_else(|| "listen must be set when the config carries one".to_owned())?;
        assert_eq!(listen.as_str(), "127.0.0.1:1053");
        Ok(())
    }

    /// B6: both public entry points (`validated_dns_settings`, used by
    /// `validate`, and `to_settings`, used by renderers) funnel through the
    /// caly-dns builder, so a non-socket listen is refused identically.
    /// Pre-fix only the validator carried the shape check; `to_settings`
    /// accepted the value and rendered it downstream.
    #[test]
    fn b6_listen_rejected_consistently_on_both_paths() {
        let config = DnsSchemaConfig {
            listen: Some("not-an-address".to_owned()),
            ..enabled_config()
        };
        assert_eq!(
            validated_dns_settings(&config),
            Err(DnsSchemaError::InvalidListen)
        );
        assert_eq!(config.to_settings(), None);
        let good = DnsSchemaConfig {
            listen: Some("127.0.0.1:1053".to_owned()),
            ..enabled_config()
        };
        assert!(good.to_settings().is_some());
    }

    /// Regression: a malformed `dns.listen` (empty / not `host:port`) used
    /// to surface as `DnsSchemaError::InvalidFilter`, which was misleading
    /// and broke callers that key off the variant name. It now reports
    /// `InvalidListen` so error messages and tests can distinguish the
    /// filter path from the listen path.
    #[test]
    fn invalid_listen_reports_invalid_listen_not_filter() {
        let config = DnsSchemaConfig {
            listen: Some(String::new()),
            ..enabled_config()
        };
        assert_eq!(
            validated_dns_settings(&config),
            Err(DnsSchemaError::InvalidListen)
        );
        // The Display message names "listen", not "filter", so a human
        // reading the daemon diagnostic can act on it.
        let message = format!("{}", DnsSchemaError::InvalidListen);
        assert!(
            message.contains("listen"),
            "InvalidListen message must mention `listen`, got: {message}"
        );
    }
}
