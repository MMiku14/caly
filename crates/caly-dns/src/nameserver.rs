//! Bounded, validated DNS server and fake-ip range values.
//!
//! Structured form (P4): a `Nameserver` keeps the validated source text
//! *and* its parsed `NameserverShape` (kind / path-stripped address /
//! optional path). Parsing is total and mirrors the renderer's historical
//! cut rules byte for byte, so switching renderers to the structured view
//! changes no output; the extra structure (DoH path, scheme kind) is the
//! ground the P5 bug fixes (B1/B2/B3) build on.

use caly_domain::BoundedText;

/// Maximum bytes in one nameserver textual representation.
pub const DNS_TEXT_MAX_BYTES: usize = 256;
/// Maximum bytes in a fake-ip CIDR range.
pub const FAKE_IP_RANGE_MAX_BYTES: usize = 64;

/// The transport a nameserver speaks, derived from its URI scheme.
/// Scheme-less values are plain UDP (the historical default).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameserverKind {
    /// The `local` pseudo-server (system resolver).
    Local,
    Udp,
    Tcp,
    Tls,
    Https,
    Quic,
}

impl NameserverKind {
    /// The stable lower-case label renderers emit as the server `type`.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Udp => "udp",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
            Self::Https => "https",
            Self::Quic => "quic",
        }
    }
}

/// The parsed view of a nameserver: byte ranges into the source text.
/// `address` is the path-stripped upstream (`host` or `host:port`); `path`
/// is present for URI-style servers such as `https://dns.google/dns-query`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NameserverShape {
    kind: NameserverKind,
    address: (usize, usize),
    path: Option<(usize, usize)>,
}

/// Parses the structured shape. Total by construction: every value that
/// passes [`Nameserver::new`] validation has exactly one shape, and the cut
/// rules replicate the historical renderer preprocessing (scheme prefix
/// table, first-`/` path split, scheme-less defaults to UDP, `local`
/// stands alone).
fn parse_shape(text: &str) -> NameserverShape {
    if text == "local" {
        return NameserverShape {
            kind: NameserverKind::Local,
            address: (text.len(), text.len()),
            path: None,
        };
    }
    for (prefix, kind) in [
        ("tls://", NameserverKind::Tls),
        ("https://", NameserverKind::Https),
        ("tcp://", NameserverKind::Tcp),
        ("quic://", NameserverKind::Quic),
        ("udp://", NameserverKind::Udp),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let start = prefix.len();
            let end = rest.find('/').map_or(text.len(), |slash| start + slash);
            let path = (end < text.len()).then_some((end, text.len()));
            return NameserverShape {
                kind,
                address: (start, end),
                path,
            };
        }
    }
    NameserverShape {
        kind: NameserverKind::Udp,
        address: (0, text.len()),
        path: None,
    }
}

/// A validated upstream nameserver (IP literal or hostname, optional
/// scheme/port/path) plus its parsed shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nameserver {
    text: BoundedText<DNS_TEXT_MAX_BYTES>,
    shape: NameserverShape,
}

/// Validation failure for a single nameserver value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameserverError {
    Empty,
    TooLong,
    InvalidCharacter,
}

impl core::fmt::Display for NameserverError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("nameserver must not be empty"),
            Self::TooLong => write!(formatter, "nameserver exceeds {DNS_TEXT_MAX_BYTES} bytes"),
            Self::InvalidCharacter => formatter
                .write_str("nameserver contains characters outside the allowed host/scheme set"),
        }
    }
}

impl std::error::Error for NameserverError {}

impl Nameserver {
    /// Validates ASCII host/IP/port syntax and parses the structured shape.
    pub fn new(value: impl Into<String>) -> Result<Self, NameserverError> {
        let value = value.into();
        if value.is_empty() {
            return Err(NameserverError::Empty);
        }
        if value.len() > DNS_TEXT_MAX_BYTES {
            return Err(NameserverError::TooLong);
        }
        if !value.bytes().all(is_nameserver_byte) {
            return Err(NameserverError::InvalidCharacter);
        }
        let shape = parse_shape(&value);
        Ok(Self {
            text: BoundedText::new(value).map_err(|_| NameserverError::TooLong)?,
            shape,
        })
    }

    /// Borrows the validated textual representation.
    pub fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Returns the parsed structured shape.
    pub const fn shape(&self) -> NameserverShape {
        self.shape
    }

    /// The nameserver transport kind (scheme-derived).
    pub const fn kind(&self) -> NameserverKind {
        self.shape.kind
    }

    /// The path-stripped upstream address (`host` or `host:port`). For
    /// scheme-less values this is the entire source text; for `local` it is
    /// empty.
    pub fn address(&self) -> &str {
        &self.text.as_str()[self.shape.address.0..self.shape.address.1]
    }

    /// The URI path when the value carries one (`/dns-query` on a DoH
    /// server). Kept in the model now; renderers start emitting it with
    /// the P5 B1 fix.
    pub fn path(&self) -> Option<&str> {
        self.shape
            .path
            .map(|(start, end)| &self.text.as_str()[start..end])
    }

    /// B2: whether the upstream address is an IP literal, tolerating a
    /// trailing `:port`. The pre-fix renderer judged the raw address text,
    /// so `8.8.8.8:53` failed the `IpAddr` parse as a whole, was treated as
    /// a *domain*, and could never be elected `default_domain_resolver` —
    /// which in turn left domain-hosted resolvers without a bootstrap.
    pub fn is_ip_literal(&self) -> bool {
        let address = self.address();
        // #123: strip brackets on the *whole* address first — pre-fix the
        // bracket pair was only lifted in the `host:port` arm, so a bare
        // `[::1]` fell to the domain class and could never bootstrap.
        if unbracket_host(address).parse::<std::net::IpAddr>().is_ok() {
            return true;
        }
        // Only a `host:port` split is attempted: multi-colon text without
        // brackets is a bare IPv6 literal (handled above), never host:port.
        let Some((host, port)) = address.rsplit_once(':') else {
            return false;
        };
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        unbracket_host(host).parse::<std::net::IpAddr>().is_ok()
    }
}

/// Lifts a surrounding `[` `]` pair off an address or host, when present.
fn unbracket_host(value: &str) -> &str {
    value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(value)
}

/// Accepts digits, letters, dot, colon, brackets, hyphen and URL scheme glyphs.
fn is_nameserver_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'.' | b':' | b'[' | b']' | b'-' | b'/' | b'?' | b'=' | b'_'
        )
}

/// A validated fake-ip CIDR range, e.g. `198.18.0.1/16`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeIpRange(BoundedText<FAKE_IP_RANGE_MAX_BYTES>);

/// Validation failure for a fake-ip range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FakeIpRangeError {
    Empty,
    TooLong,
    InvalidSyntax,
}

impl core::fmt::Display for FakeIpRangeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("fake-ip range must not be empty"),
            Self::TooLong => write!(
                formatter,
                "fake-ip range exceeds {FAKE_IP_RANGE_MAX_BYTES} bytes"
            ),
            Self::InvalidSyntax => formatter.write_str("fake-ip range must look like a CIDR range"),
        }
    }
}

impl std::error::Error for FakeIpRangeError {}

impl FakeIpRange {
    /// Accepts values matching an IPv4/IPv6 CIDR (`a.b.c.d/len` or `[v6]/len`)
    /// whose address parses and whose prefix length fits the address family.
    pub fn new(value: impl Into<String>) -> Result<Self, FakeIpRangeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(FakeIpRangeError::Empty);
        }
        if value.len() > FAKE_IP_RANGE_MAX_BYTES {
            return Err(FakeIpRangeError::TooLong);
        }
        let slash = value.rfind('/').ok_or(FakeIpRangeError::InvalidSyntax)?;
        let (address, prefix) = value.split_at(slash);
        if address.is_empty()
            || prefix.len() == 1
            || !prefix[1..].bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(FakeIpRangeError::InvalidSyntax);
        }
        if !address.bytes().all(is_range_byte) {
            return Err(FakeIpRangeError::InvalidSyntax);
        }
        if !is_valid_range_shape(address, &prefix[1..]) {
            return Err(FakeIpRangeError::InvalidSyntax);
        }
        Ok(Self(
            BoundedText::new(value).map_err(|_| FakeIpRangeError::TooLong)?,
        ))
    }

    /// Borrows the validated CIDR range.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Accepts IP/address glyphs and brackets used in a CIDR address component.
fn is_range_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'[' | b']' | b'-')
}

/// B5: the character-set check alone accepted `999.1.1.1/99` — a range that
/// no kernel can route, which then surfaced as a core-level config rejection
/// far from the operator's input. The address must parse as an IP (square
/// brackets tolerated for v6) and the prefix must fit its family.
pub(crate) fn is_valid_range_shape(address: &str, prefix: &str) -> bool {
    let unbracketed = address
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(address);
    let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() else {
        return false;
    };
    let Ok(length) = prefix.parse::<u32>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(_) => length <= 32,
        std::net::IpAddr::V6(_) => length <= 128,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nameserver_rejects_whitespace() {
        assert_eq!(
            Nameserver::new("8.8.8.8 53"),
            Err(NameserverError::InvalidCharacter)
        );
        assert_eq!(Nameserver::new(""), Err(NameserverError::Empty));
    }

    #[test]
    fn nameserver_accepts_schemes() -> Result<(), NameserverError> {
        let server = Nameserver::new("tls://dns.google")?;
        assert_eq!(server.as_str(), "tls://dns.google");
        Ok(())
    }

    #[test]
    fn fake_ip_range_rejects_bad_syntax() {
        assert_eq!(
            FakeIpRange::new("198.18.0.1"),
            Err(FakeIpRangeError::InvalidSyntax)
        );
        assert_eq!(
            FakeIpRange::new("198.18.0.1/x"),
            Err(FakeIpRangeError::InvalidSyntax)
        );
    }

    #[test]
    fn b5_fake_ip_range_rejects_impossible_address_and_prefix() {
        // Character-set legal, semantically impossible: octet 999 and a
        // v4 prefix beyond /32 must both be refused (pre-fix accepted).
        assert_eq!(
            FakeIpRange::new("999.1.1.1/99"),
            Err(FakeIpRangeError::InvalidSyntax)
        );
        assert_eq!(
            FakeIpRange::new("198.18.0.1/33"),
            Err(FakeIpRangeError::InvalidSyntax)
        );
        assert_eq!(
            FakeIpRange::new("not-an-ip/16"),
            Err(FakeIpRangeError::InvalidSyntax)
        );
        assert!(FakeIpRange::new("198.18.0.1/16").is_ok());
        assert!(FakeIpRange::new("198.18.0.1/32").is_ok());
        assert!(FakeIpRange::new("[fd00::]/8").is_ok());
        assert_eq!(
            FakeIpRange::new("[fd00::]/129"),
            Err(FakeIpRangeError::InvalidSyntax)
        );
    }

    #[test]
    fn plain_ip_parses_as_udp_with_the_whole_text_as_address() -> Result<(), NameserverError> {
        let server = Nameserver::new("8.8.8.8")?;
        assert_eq!(server.kind(), NameserverKind::Udp);
        assert_eq!(server.address(), "8.8.8.8");
        assert_eq!(server.path(), None);
        Ok(())
    }

    #[test]
    fn host_port_keeps_the_port_in_the_address() -> Result<(), NameserverError> {
        // 8.8.8.8:53 stays one indivisible address in P4; splitting host/port
        // (and teaching `host_is_ip` about ports) is the P5 B2 fix.
        let server = Nameserver::new("8.8.8.8:53")?;
        assert_eq!(server.kind(), NameserverKind::Udp);
        assert_eq!(server.address(), "8.8.8.8:53");
        Ok(())
    }

    #[test]
    fn scheme_server_strips_and_exposes_the_path() -> Result<(), NameserverError> {
        let tls = Nameserver::new("tls://dns.google")?;
        assert_eq!(tls.kind(), NameserverKind::Tls);
        assert_eq!(tls.address(), "dns.google");
        assert_eq!(tls.path(), None);
        let doh = Nameserver::new("https://dns.google/dns-query")?;
        assert_eq!(doh.kind(), NameserverKind::Https);
        assert_eq!(doh.address(), "dns.google");
        assert_eq!(doh.path(), Some("/dns-query"));
        Ok(())
    }

    #[test]
    fn local_stands_alone() -> Result<(), NameserverError> {
        let server = Nameserver::new("local")?;
        assert_eq!(server.kind(), NameserverKind::Local);
        assert_eq!(server.address(), "");
        Ok(())
    }

    #[test]
    fn b2_host_port_ip_literal_is_recognized() -> Result<(), NameserverError> {
        // Pre-fix: `is_ip_literal` on the whole address failed for
        // `8.8.8.8:53`, so the server was classified as domain-hosted.
        assert!(Nameserver::new("8.8.8.8")?.is_ip_literal());
        assert!(Nameserver::new("8.8.8.8:53")?.is_ip_literal());
        assert!(Nameserver::new("[::1]:853")?.is_ip_literal());
        assert!(Nameserver::new("::1")?.is_ip_literal());
        assert!(!Nameserver::new("dns.google")?.is_ip_literal());
        assert!(!Nameserver::new("dns.google:53")?.is_ip_literal());
        assert!(!Nameserver::new("tls://dns.google")?.is_ip_literal());
        Ok(())
    }

    #[test]
    fn a123_bare_bracketed_ipv6_is_an_ip_literal() -> Result<(), NameserverError> {
        // #123: pre-fix the bracket pair was only stripped in the host:port
        // arm, so `[::1]` (no port) was misjudged as domain-hosted and
        // could never be elected bootstrap resolver.
        assert!(Nameserver::new("[::1]")?.is_ip_literal());
        assert!(Nameserver::new("[2001:4860:4860::8888]")?.is_ip_literal());
        assert!(!Nameserver::new("[not-an-ip]")?.is_ip_literal());
        Ok(())
    }
}
