//! Bounded, validated DNS configuration model.
//!
//! This module is core-agnostic: it describes what a proxy's DNS feature
//! needs (upstream nameservers, fallback, direct/default resolvers, fake-ip)
//! without knowing whether the backend is Mihomo, sing-box or Xray. Renderers
//! in the infrastructure layer translate these values into core-specific
//! configuration. The underlying `Nameserver`/`FakeIpRange` values live in
//! the sibling `nameserver` module.

use caly_domain::BoundedText;
use caly_domain::BoundedVec;

use crate::filter::{
    DNS_LISTEN_MAX_BYTES, DnsFilterError, DnsPattern, FallbackFilter, MAX_DNS_FILTER_ENTRIES,
    patterns_from,
};
use crate::nameserver::{FakeIpRange, FakeIpRangeError, Nameserver, NameserverError};

/// Maximum upstream nameservers in one DNS configuration.
pub const MAX_DNS_SERVERS: usize = 16;

/// How the proxy answers hostnames for proxied traffic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DnsMode {
    /// Do not hijack; forward every query to the configured resolvers.
    #[default]
    Standard,
    /// Answer with a synthetic IP from a reserved range and map it back.
    FakeIp,
    /// Rewrite to the real resolved address and keep the host header.
    RedirHost,
}

impl DnsMode {
    /// Returns the stable renderer-agnostic label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::FakeIp => "fake-ip",
            Self::RedirHost => "redir-host",
        }
    }
}

/// Validation failure for a complete DNS configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsError {
    Nameserver(NameserverError),
    FakeIpRange(FakeIpRangeError),
    Filter(DnsFilterError),
    EnabledWithoutNameserver,
    FakeIpModeWithoutRange,
    /// A group already holds `MAX_DNS_SERVERS` entries. The pre-fix builder
    /// silently dropped the overflow, so a configuration could *validate*
    /// while losing resolvers the operator declared.
    TooManyNameservers,
}

impl core::fmt::Display for DnsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Nameserver(error) => write!(formatter, "{error}"),
            Self::FakeIpRange(error) => write!(formatter, "{error}"),
            Self::Filter(error) => write!(formatter, "{error}"),
            Self::EnabledWithoutNameserver => {
                formatter.write_str("enabled DNS requires at least one nameserver")
            }
            Self::FakeIpModeWithoutRange => {
                formatter.write_str("fake-ip mode requires a fake-ip range")
            }
            Self::TooManyNameservers => {
                write!(formatter, "at most {MAX_DNS_SERVERS} nameservers per group")
            }
        }
    }
}

impl std::error::Error for DnsError {}

impl From<NameserverError> for DnsError {
    fn from(error: NameserverError) -> Self {
        Self::Nameserver(error)
    }
}

impl From<FakeIpRangeError> for DnsError {
    fn from(error: FakeIpRangeError) -> Self {
        Self::FakeIpRange(error)
    }
}

impl From<DnsFilterError> for DnsError {
    fn from(error: DnsFilterError) -> Self {
        Self::Filter(error)
    }
}

/// Complete DNS configuration validated before any renderer sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsSettings {
    enabled: bool,
    mode: DnsMode,
    nameservers: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    fallback: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    direct: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    default: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    fake_ip_range: Option<FakeIpRange>,
    fake_ip_filter: BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES>,
    fallback_filter: Option<FallbackFilter>,
    ipv6: bool,
    listen: Option<BoundedText<DNS_LISTEN_MAX_BYTES>>,
}

/// Builder that enforces invariants incrementally and atomically.
#[derive(Debug, Default)]
pub struct DnsSettingsBuilder {
    enabled: bool,
    mode: DnsMode,
    ipv6: bool,
    nameservers: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    fallback: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    direct: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    default: BoundedVec<Nameserver, MAX_DNS_SERVERS>,
    fake_ip_range: Option<FakeIpRange>,
    fake_ip_filter: BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES>,
    fallback_filter: Option<FallbackFilter>,
    listen: Option<BoundedText<DNS_LISTEN_MAX_BYTES>>,
}

impl DnsSettingsBuilder {
    /// Constructs a disabled, empty configuration (validates as `None`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables or disables the DNS feature.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the enhanced-mode strategy.
    #[must_use]
    pub fn mode(mut self, mode: DnsMode) -> Self {
        self.mode = mode;
        self
    }

    /// Adds one upstream nameserver (used for proxied queries).
    ///
    /// Returns `DnsError::TooManyNameservers` when the group already holds
    /// `MAX_DNS_SERVERS` entries — the pre-fix code dropped the overflow
    /// silently, so a configuration could validate while losing resolvers.
    pub fn push_nameserver(mut self, value: &str) -> Result<Self, DnsError> {
        let server = Nameserver::new(value.to_owned())?;
        self.nameservers
            .try_push(server)
            .map_err(|_| DnsError::TooManyNameservers)?;
        Ok(self)
    }

    /// Adds one fallback nameserver (capacity-bounded like [`Self::push_nameserver`]).
    pub fn push_fallback(mut self, value: &str) -> Result<Self, DnsError> {
        let server = Nameserver::new(value.to_owned())?;
        self.fallback
            .try_push(server)
            .map_err(|_| DnsError::TooManyNameservers)?;
        Ok(self)
    }

    /// Adds one direct nameserver (non-proxied resolution); capacity-bounded.
    pub fn push_direct(mut self, value: &str) -> Result<Self, DnsError> {
        let server = Nameserver::new(value.to_owned())?;
        self.direct
            .try_push(server)
            .map_err(|_| DnsError::TooManyNameservers)?;
        Ok(self)
    }

    /// Adds one default nameserver (plain bootstrap resolver); capacity-bounded.
    pub fn push_default(mut self, value: &str) -> Result<Self, DnsError> {
        let server = Nameserver::new(value.to_owned())?;
        self.default
            .try_push(server)
            .map_err(|_| DnsError::TooManyNameservers)?;
        Ok(self)
    }

    /// Sets the fake-ip CIDR range.
    pub fn fake_ip_range(mut self, value: &str) -> Result<Self, FakeIpRangeError> {
        self.fake_ip_range = Some(FakeIpRange::new(value.to_owned())?);
        Ok(self)
    }

    /// Sets the fake-ip bypass patterns (domains that must receive real IPs).
    pub fn fake_ip_filter(mut self, values: &[String]) -> Result<Self, DnsFilterError> {
        self.fake_ip_filter = patterns_from(values)?;
        Ok(self)
    }

    /// Sets the anti-poisoning fallback filter.
    #[must_use]
    pub fn fallback_filter(mut self, filter: FallbackFilter) -> Self {
        self.fallback_filter = Some(filter);
        self
    }

    /// Enables AAAA resolution in the kernel DNS engine.
    #[must_use]
    pub const fn ipv6(mut self, enabled: bool) -> Self {
        self.ipv6 = enabled;
        self
    }

    /// Sets the DNS listener socket address (`host:port`).
    ///
    /// B6: the single home of listen validation. The socket-address shape
    /// check used to live only in the profile validator, so the
    /// `to_settings()` path (builder only) quietly accepted
    /// `listen: not-an-address` and rendered it into the core config.
    pub fn listen(mut self, value: &str) -> Result<Self, DnsFilterError> {
        if value.is_empty() {
            return Err(DnsFilterError::Empty);
        }
        if value.parse::<std::net::SocketAddr>().is_err() {
            return Err(DnsFilterError::InvalidListen);
        }
        self.listen =
            Some(BoundedText::new(value.to_owned()).map_err(|_| DnsFilterError::TooLong)?);
        Ok(self)
    }

    /// Validates invariants and produces a bounded configuration.
    pub fn build(self) -> Result<Option<DnsSettings>, DnsError> {
        if !self.enabled {
            return Ok(None);
        }
        if self.nameservers.is_empty() {
            return Err(DnsError::EnabledWithoutNameserver);
        }
        if self.mode == DnsMode::FakeIp && self.fake_ip_range.is_none() {
            return Err(DnsError::FakeIpModeWithoutRange);
        }
        Ok(Some(DnsSettings {
            enabled: self.enabled,
            mode: self.mode,
            nameservers: self.nameservers,
            fallback: self.fallback,
            direct: self.direct,
            default: self.default,
            fake_ip_range: self.fake_ip_range,
            fake_ip_filter: self.fake_ip_filter,
            fallback_filter: self.fallback_filter,
            ipv6: self.ipv6,
            listen: self.listen,
        }))
    }
}

/// The built-in DNS configuration injected when a TUN inbound renders
/// without an operator DNS section (W3a 兜底): a TUN inbound answers
/// DNS itself, so a kernel config with no DNS block would blackhole
/// every name resolution. fake-ip mode with two public upstreams and
/// the reserved 28.0.0.0/8 synthesis range; IPv6 off (the injected
/// upstream set is v4-only and the TUN interface is v4-first).
pub fn default_tun_dns() -> Result<Option<DnsSettings>, DnsError> {
    DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("223.5.5.5")?
        .push_nameserver("8.8.8.8")?
        .fake_ip_range("28.0.0.1/8")?
        .ipv6(false)
        .build()
}

impl DnsSettings {
    /// Returns whether DNS hijacking is engaged.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the enhanced-mode strategy.
    pub fn mode(&self) -> DnsMode {
        self.mode
    }

    /// Iterates over upstream nameservers.
    pub fn nameservers(&self) -> &BoundedVec<Nameserver, MAX_DNS_SERVERS> {
        &self.nameservers
    }

    /// Iterates over fallback nameservers.
    pub fn fallback(&self) -> &BoundedVec<Nameserver, MAX_DNS_SERVERS> {
        &self.fallback
    }

    /// Iterates over direct nameservers.
    pub fn direct(&self) -> &BoundedVec<Nameserver, MAX_DNS_SERVERS> {
        &self.direct
    }

    /// Iterates over default bootstrap nameservers.
    pub fn default(&self) -> &BoundedVec<Nameserver, MAX_DNS_SERVERS> {
        &self.default
    }

    /// Returns the fake-ip CIDR range when configured.
    pub fn fake_ip_range(&self) -> Option<&FakeIpRange> {
        self.fake_ip_range.as_ref()
    }

    /// Returns the fake-ip bypass patterns (real-IP domains).
    pub const fn fake_ip_filter(&self) -> &BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES> {
        &self.fake_ip_filter
    }

    /// Returns the anti-poisoning fallback filter when configured.
    pub fn fallback_filter(&self) -> Option<&FallbackFilter> {
        self.fallback_filter.as_ref()
    }

    /// Whether AAAA resolution is enabled.
    pub const fn ipv6(&self) -> bool {
        self.ipv6
    }

    /// Returns the DNS listener socket address when configured.
    pub fn listen(&self) -> Option<&BoundedText<DNS_LISTEN_MAX_BYTES>> {
        self.listen.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_validates_as_none() -> Result<(), DnsError> {
        assert_eq!(DnsSettingsBuilder::new().build()?, None);
        Ok(())
    }

    #[test]
    fn enabled_requires_a_nameserver() {
        assert_eq!(
            DnsSettingsBuilder::new().enabled(true).build(),
            Err(DnsError::EnabledWithoutNameserver)
        );
    }

    #[test]
    fn fake_ip_requires_a_range() -> Result<(), DnsError> {
        let result = DnsSettingsBuilder::new()
            .enabled(true)
            .mode(DnsMode::FakeIp)
            .push_nameserver("8.8.8.8")?
            .build();
        assert_eq!(result, Err(DnsError::FakeIpModeWithoutRange));
        Ok(())
    }

    #[test]
    fn builds_a_bounded_dns_config() -> Result<(), DnsError> {
        let dns = DnsSettingsBuilder::new()
            .enabled(true)
            .mode(DnsMode::FakeIp)
            .push_nameserver("8.8.8.8")?
            .push_nameserver("1.1.1.1")?
            .push_fallback("tls://dns.google")?
            .push_default("223.5.5.5")?
            .fake_ip_range("198.18.0.1/16")?
            .build()?
            .ok_or(DnsError::EnabledWithoutNameserver)?;
        assert!(dns.enabled());
        assert_eq!(dns.mode(), DnsMode::FakeIp);
        assert_eq!(dns.nameservers().len(), 2);
        assert_eq!(
            dns.fake_ip_range().map(FakeIpRange::as_str),
            Some("198.18.0.1/16")
        );
        Ok(())
    }

    #[test]
    fn b6_listen_shape_is_validated_in_the_builder() -> Result<(), DnsError> {
        // B6: single home. Both public paths (`validate` and `to_settings`)
        // funnel through this builder, so `not-an-address` is refused here.
        let result = DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("8.8.8.8")?
            .listen("not-an-address");
        assert!(matches!(result, Err(DnsFilterError::InvalidListen)));
        assert!(matches!(
            DnsSettingsBuilder::new().listen(""),
            Err(DnsFilterError::Empty)
        ));
        let ok = DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("8.8.8.8")?
            .listen("127.0.0.1:1053");
        assert!(ok.is_ok());
        Ok(())
    }

    #[test]
    fn dns_mode_labels_are_stable() {
        assert_eq!(DnsMode::Standard.label(), "standard");
        assert_eq!(DnsMode::FakeIp.label(), "fake-ip");
        assert_eq!(DnsMode::RedirHost.label(), "redir-host");
    }

    #[test]
    fn pushing_beyond_capacity_reports_too_many_instead_of_dropping() -> Result<(), DnsError> {
        let mut builder = DnsSettingsBuilder::new().enabled(true);
        for index in 0..MAX_DNS_SERVERS {
            builder = builder.push_nameserver(&format!("10.0.0.{index}"))?;
        }
        let overflow = builder.push_nameserver("10.0.1.1");
        assert!(
            matches!(overflow, Err(DnsError::TooManyNameservers)),
            "the 17th nameserver must error, not vanish: {overflow:?}"
        );
        Ok(())
    }
}
