//! Default address classifier used by every SSRF-safe fetch in
//! caly. Lives next to the fetch policy so the same rejection
//! rules apply whether the call site is a subscription refresh or
//! a profile body download.

use std::net::IpAddr;

use super::AddressClassifier;

/// Drops private / loopback / link-local / unspecified /
/// broadcast addresses. Both IPv4 and IPv6 are covered, and
/// IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are peeled
/// before the IPv4 rules apply — a `::ffff:10.0.0.1` looks
/// like a public IPv6 to a naive classifier but is really a
/// private IPv4 in disguise. Used by every fetch call site
/// that does not need a custom policy (subscription refresh,
/// profile body download, GEOSITE auto-emit, …).
pub struct PublicAddressClassifier;

impl AddressClassifier for PublicAddressClassifier {
    fn is_globally_routable(&self, address: IpAddr) -> bool {
        // IPv4-mapped IPv6 (`::ffff:a.b.c.d`) is functionally
        // an IPv4 address. Peeling it before the match keeps
        // the IPv4 branch the single source of truth for the
        // full private/loopback/link-local/unspecified/broadcast
        // matrix; the same DNS response can carry either
        // representation depending on resolver behaviour.
        let normalized = match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 @ IpAddr::V4(_) => v4,
        };
        match normalized {
            IpAddr::V4(value) => {
                !value.is_private()
                    && !value.is_loopback()
                    && !value.is_link_local()
                    && !value.is_unspecified()
                    && !value.is_broadcast()
            }
            IpAddr::V6(value) => {
                !value.is_loopback()
                    && !value.is_unspecified()
                    && !value.is_unique_local()
                    && !value.is_unicast_link_local()
                    // Round 31: the `2001:db8::/32`
                    // documentation prefix (RFC 3849)
                    // is conventionally unrouted on
                    // the public internet. The
                    // pre-Round 31 shape treated it
                    // as public (it didn't fall into
                    // any of the `is_*` private
                    // buckets), so a subscription or
                    // profile pointing at a docs URL
                    // (e.g.
                    // `https://[2001:db8::1]/sub`)
                    // would have passed the SSRF
                    // guard and reached a private
                    // network unreachable from the
                    // operator's box. The new shape
                    // explicitly rejects the
                    // documentation prefix so a docs
                    // URL fails the guard with the
                    // same precision as a private
                    // address. The prefix check is
                    // `2001:db8::/32`: the first 32
                    // bits must be `0x2001_0db8`.
                    && !is_documentation_v6(value)
            }
        }
    }
}

/// Round 31: whether the address falls inside
/// `2001:db8::/32` (RFC 3849 IPv6 documentation
/// prefix). The prefix is `2001:0db8:0000:0000:…`
/// — the first two 16-bit groups must be `0x2001`
/// and `0x0db8`. A future RFC could widen the
/// documentation space; the helper lives in one
/// place so widening is a 1-line change. The
/// helper is `const fn`-free because `Ipv6Addr`'s
/// segment accessors are not `const fn` on
/// `rust-version = 1.88.0`; the public call site
/// stays a `match` arm.
fn is_documentation_v6(value: std::net::Ipv6Addr) -> bool {
    let segments = value.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn ipv4_mapped_ipv6_is_classified_as_the_underlying_ipv4() {
        // `::ffff:10.0.0.1` is private IPv4 in disguise. A
        // naive IPv6 classifier would let it through; peeling
        // it routes the address through the IPv4 branch where
        // `is_private()` rejects it.
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert!(!PublicAddressClassifier.is_globally_routable(mapped));
        // Sanity: a public IPv4-mapped stays public.
        let public_mapped: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(PublicAddressClassifier.is_globally_routable(public_mapped));
    }

    #[test]
    fn ipv4_rules_locked() {
        assert!(!PublicAddressClassifier.is_globally_routable(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(
            !PublicAddressClassifier.is_globally_routable(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)))
        );
        assert!(
            !PublicAddressClassifier
                .is_globally_routable(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)))
        );
        assert!(
            !PublicAddressClassifier
                .is_globally_routable(IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1)))
        );
        assert!(!PublicAddressClassifier.is_globally_routable(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(!PublicAddressClassifier.is_globally_routable(IpAddr::V4(Ipv4Addr::BROADCAST)));
        assert!(
            PublicAddressClassifier.is_globally_routable(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)))
        );
    }

    #[test]
    fn ipv6_rules_locked() {
        assert!(!PublicAddressClassifier.is_globally_routable(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!PublicAddressClassifier.is_globally_routable(IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
        // fc00::/7 — unique local
        assert!(
            !PublicAddressClassifier
                .is_globally_routable(IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)))
        );
        // fe80::/10 — link-local
        assert!(
            !PublicAddressClassifier
                .is_globally_routable(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)))
        );
        // 2001:db8::/32 — RFC 3849 documentation prefix. The
        // pre-Round 31 shape treated it as public (it did
        // not fall into any of the `is_*` private buckets),
        // so a subscription or profile URL pointing at a
        // docs IP would have passed the SSRF guard and
        // reached a network the operator cannot actually
        // reach. Round 31 rejects the prefix so a docs
        // URL fails the guard with the same precision as
        // a private address. `Ipv6Addr::new` takes 8
        // `u16` segments in network order, so the
        // prefix is `new(0x2001, 0x0db8, 0, 0, 0, 0, 0, host)`
        // (the pre-Round 31 test passed two-byte
        // halves as if they were segments — that
        // typo silently turned `2001:db8::/32` into
        // the documentation space `00:20:00:01:…`,
        // which is not in the `2001:db8::/32` block
        // and the old shape accepted it as public).
        assert!(
            !PublicAddressClassifier
                .is_globally_routable(IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1)))
        );
        // A real public IPv6 (Google's 2001:4860:4860::8888)
        // still passes — the reject is scoped to the
        // `2001:0db8::/32` documentation space, not the
        // entire `2001::/16`.
        assert!(
            PublicAddressClassifier.is_globally_routable(IpAddr::V6(Ipv6Addr::new(
                0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888
            )))
        );
    }

    /// Round 31: every IP inside `2001:db8::/32` is
    /// rejected, including the prefix itself, the
    /// all-zeroes short form `2001:db8::`, and a
    /// host near the top of the prefix. A future
    /// widening of RFC 3849 to a larger block
    /// would land in the `is_documentation_v6`
    /// helper as a 1-line change.
    #[test]
    fn documentation_prefix_is_fully_rejected() {
        for last_segment in [0_u16, 1, 0xabcd, 0xffff] {
            assert!(
                !PublicAddressClassifier.is_globally_routable(IpAddr::V6(Ipv6Addr::new(
                    0x2001,
                    0x0db8,
                    0,
                    0,
                    0,
                    0,
                    0,
                    last_segment
                ))),
                "2001:db8::{last_segment:x} must be rejected (RFC 3849)"
            );
        }
    }
}
