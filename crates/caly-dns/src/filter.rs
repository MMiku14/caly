//! DNS filter values: fake-ip bypass patterns and the anti-poisoning
//! fallback filter. Pure bounded value types; rendering is infrastructure.

use caly_domain::BoundedText;
use caly_domain::BoundedVec;
use caly_domain::GeoipCode;

use crate::nameserver::is_valid_range_shape;

/// Maximum entries in one DNS filter list (fake-ip-filter, fallback-filter).
pub const MAX_DNS_FILTER_ENTRIES: usize = 256;
/// Maximum bytes in one DNS filter pattern (`+.example.com`, `*.wild`, ...).
pub const DNS_PATTERN_MAX_BYTES: usize = 256;
/// Maximum bytes in the DNS listener socket-address text.
pub const DNS_LISTEN_MAX_BYTES: usize = 64;

/// One DNS filter pattern (fake-ip bypass or fallback-filter domain entry).
pub type DnsPattern = BoundedText<DNS_PATTERN_MAX_BYTES>;

/// DNS filter pattern validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsFilterError {
    Empty,
    TooLong,
    TooManyEntries,
    InvalidGeoipCode,
    /// The DNS listen value is not a socket address (B6: validated in the
    /// single home, `DnsSettingsBuilder::listen`).
    InvalidListen,
    /// A domain pattern carries bytes outside the DNS-pattern glyph set
    /// (#122: a quote/whitespace/control byte would break out of the
    /// rendered YAML single-quoted block).
    InvalidCharacter,
    /// A fallback-filter `ipcidr` entry is not CIDR-shaped (#122: the
    /// ipcidr list shares the CIDR shape discipline of `FakeIpRange`,
    /// closing the B5 sibling gap).
    InvalidIpCidr,
}

impl core::fmt::Display for DnsFilterError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("DNS filter pattern must not be empty"),
            Self::TooLong => formatter.write_str("DNS filter pattern is too long"),
            Self::TooManyEntries => formatter.write_str("DNS filter list has too many entries"),
            Self::InvalidGeoipCode => formatter.write_str("fallback-filter geoip code is invalid"),
            Self::InvalidListen => {
                formatter.write_str("DNS listen must be a socket address (host:port)")
            }
            Self::InvalidCharacter => formatter
                .write_str("DNS filter pattern contains characters outside the allowed set"),
            Self::InvalidIpCidr => {
                formatter.write_str("fallback-filter ipcidr entry must look like a CIDR range")
            }
        }
    }
}

/// Anti-poisoning fallback filter (Mihomo `fallback-filter`). Decides when a
/// `nameserver` answer is treated as poisoned and the `fallback` answer wins:
/// GeoIP mismatch, answers inside suspicious CIDRs, or forced domains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FallbackFilter {
    geoip: bool,
    geoip_code: Option<GeoipCode>,
    ipcidr: BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES>,
    domain: BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES>,
}

impl FallbackFilter {
    /// Builds a validated filter; patterns must be non-empty and bounded.
    pub fn new(
        geoip: bool,
        geoip_code: Option<String>,
        ipcidr: Vec<String>,
        domain: Vec<String>,
    ) -> Result<Self, DnsFilterError> {
        let geoip_code = match geoip_code {
            Some(value) => {
                Some(GeoipCode::new(value).map_err(|_| DnsFilterError::InvalidGeoipCode)?)
            }
            None => None,
        };
        Ok(Self {
            geoip,
            geoip_code,
            ipcidr: ipcidr_patterns_from(&ipcidr)?,
            domain: patterns_from(&domain)?,
        })
    }

    /// Whether GeoIP checks decide poisoning.
    pub const fn geoip(&self) -> bool {
        self.geoip
    }

    /// The expected GeoIP country code for non-poisoned answers.
    pub fn geoip_code(&self) -> Option<&GeoipCode> {
        self.geoip_code.as_ref()
    }

    /// CIDRs whose answers are treated as poisoned.
    pub const fn ipcidr(&self) -> &BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES> {
        &self.ipcidr
    }

    /// Domains forced through the fallback resolvers.
    pub const fn domain(&self) -> &BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES> {
        &self.domain
    }
}

/// Builds one bounded domain-pattern list, rejecting empty/oversized/
/// over-capacity entries and any pattern outside the glyph set (#122:
/// patterns render into a YAML single-quoted block — `'`/`\n`/control
/// bytes would otherwise break out of it or corrupt the document, and the
/// kernel's parse error would land far from the operator's input).
pub(crate) fn patterns_from(
    values: &[String],
) -> Result<BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES>, DnsFilterError> {
    let mut list = BoundedVec::new();
    for value in values {
        if value.is_empty() {
            return Err(DnsFilterError::Empty);
        }
        if !value.bytes().all(is_domain_pattern_byte) {
            return Err(DnsFilterError::InvalidCharacter);
        }
        let pattern = DnsPattern::new(value.clone()).map_err(|_| DnsFilterError::TooLong)?;
        list.try_push(pattern)
            .map_err(|_| DnsFilterError::TooManyEntries)?;
    }
    Ok(list)
}

/// Builds the fallback-filter `ipcidr` list: every entry must be a
/// CIDR-shaped range (#122 — B5 sibling discipline; the shape check is
/// shared verbatim with `FakeIpRange` so the two cannot drift).
fn ipcidr_patterns_from(
    values: &[String],
) -> Result<BoundedVec<DnsPattern, MAX_DNS_FILTER_ENTRIES>, DnsFilterError> {
    let mut list = BoundedVec::new();
    for value in values {
        if value.is_empty() {
            return Err(DnsFilterError::Empty);
        }
        let Some(slash) = value.rfind('/') else {
            return Err(DnsFilterError::InvalidIpCidr);
        };
        let (address, prefix) = value.split_at(slash);
        if address.is_empty()
            || prefix.len() == 1
            || !prefix[1..].bytes().all(|byte| byte.is_ascii_digit())
            || !is_valid_range_shape(address, &prefix[1..])
        {
            return Err(DnsFilterError::InvalidIpCidr);
        }
        let pattern = DnsPattern::new(value.clone()).map_err(|_| DnsFilterError::TooLong)?;
        list.try_push(pattern)
            .map_err(|_| DnsFilterError::TooManyEntries)?;
    }
    Ok(list)
}

/// Accepts host/wildcard pattern glyphs (`+.example.com`, `*.wild`,
/// `_dmarc`-style labels); quotes, whitespace and control bytes refused.
fn is_domain_pattern_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+' | b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a122_domain_pattern_charset_gates_the_yaml_block() {
        // Quote break-out: pre-fix this rendered as `- 'we'ird'` — an
        // unterminated YAML scalar; a newline corrupted the whole document.
        assert!(matches!(
            patterns_from(&["we'ird".to_owned()]),
            Err(DnsFilterError::InvalidCharacter)
        ));
        assert!(matches!(
            patterns_from(&["line\nbreak".to_owned()]),
            Err(DnsFilterError::InvalidCharacter)
        ));
        assert!(matches!(
            patterns_from(&["white space".to_owned()]),
            Err(DnsFilterError::InvalidCharacter)
        ));
        // Legal wildcard/host glyphs still pass.
        assert!(patterns_from(&["+.example.com".to_owned()]).is_ok());
        assert!(patterns_from(&["*.wild.example".to_owned()]).is_ok());
        assert!(patterns_from(&["dmarc_mail._tcp".to_owned()]).is_ok());
    }

    #[test]
    fn a122_fallback_ipcidr_requires_cidr_shape() {
        // B5 sibling: the ipcidr list now shares the CIDR discipline —
        // impossible octets/prefixes die at the model, not in the kernel.
        let bad = FallbackFilter::new(false, None, vec!["999.1.1.1/99".to_owned()], Vec::new());
        assert!(matches!(bad, Err(DnsFilterError::InvalidIpCidr)));
        let no_slash = FallbackFilter::new(false, None, vec!["10.0.0.0".to_owned()], Vec::new());
        assert!(matches!(no_slash, Err(DnsFilterError::InvalidIpCidr)));
        let ok = FallbackFilter::new(
            false,
            None,
            vec!["10.0.0.0/8".to_owned(), "[2001:db8::]/32".to_owned()],
            Vec::new(),
        );
        assert!(ok.is_ok());
        // The domain arm inherits the pattern charset gate.
        let quoted = FallbackFilter::new(false, None, Vec::new(), vec!["x'inject".to_owned()]);
        assert!(matches!(quoted, Err(DnsFilterError::InvalidCharacter)));
    }
}
