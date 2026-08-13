//! Shared host-resolution helpers for the SSRF-safe fetch pipelines.
//!
//! Both the profile fetcher (`profile_fetch`) and the subscription actor
//! backend (`caly-backends::subscription::http`) resolve untrusted
//! configured hosts before connecting. They used to carry two near-verbatim
//! copies of the retry loop and the public-address filter — which had
//! already drifted once (one copy stopped feeding ETag validators back). The
//! single implementation lives here; callers only map the small error enum
//! into their domain-specific failure type.

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use crate::AddressClassifier;

/// How many resolution attempts one call makes (empty answers and resolver
/// failures are both retried).
pub const RESOLVE_ATTEMPTS: usize = 3;
/// Backoff between two resolution attempts.
pub const RESOLVE_BACKOFF: Duration = Duration::from_millis(100);

/// Why a host could not be resolved to any usable address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveRejection {
    /// The resolver answered, but with an empty address set.
    Empty,
    /// The resolver call itself failed on every attempt.
    Lookup,
}

impl core::fmt::Display for ResolveRejection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("DNS returned no answers"),
            Self::Lookup => formatter.write_str("DNS resolution failed"),
        }
    }
}

impl std::error::Error for ResolveRejection {}

/// Resolves `host:port` with bounded retries on both empty answer sets and
/// transient resolver failures.
///
/// Blocking by design: callers run on dedicated worker threads
/// (`spawn_blocking` / actor threads), never inside an async task.
pub fn resolve_host_with_retry(host: &str, port: u16) -> Result<Vec<IpAddr>, ResolveRejection> {
    for attempt in 0..RESOLVE_ATTEMPTS {
        let resolved = (host, port).to_socket_addrs().ok().map(|iterator| {
            iterator
                .map(|address| address.ip())
                .collect::<Vec<IpAddr>>()
        });
        let retry = attempt + 1 < RESOLVE_ATTEMPTS;
        match resolved {
            Some(values) if !values.is_empty() => return Ok(values),
            Some(_) if retry => std::thread::sleep(RESOLVE_BACKOFF),
            Some(_) => return Err(ResolveRejection::Empty),
            None if retry => std::thread::sleep(RESOLVE_BACKOFF),
            None => return Err(ResolveRejection::Lookup),
        }
    }
    Err(ResolveRejection::Lookup)
}

/// Deduplicates and reorders the answer set, dropping non-public addresses
/// and preferring IPv4 over IPv6 (so a single successful connect handshake
/// is more likely).
pub fn prefer_public_addresses(
    addresses: Vec<IpAddr>,
    classifier: &impl AddressClassifier,
) -> Vec<IpAddr> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for address in addresses {
        if !classifier.is_globally_routable(address) {
            continue;
        }
        if seen.insert(address) {
            out.push(address);
        }
    }
    out.sort_by_key(|address| !matches!(address, IpAddr::V4(_)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PublicAddressClassifier;

    #[test]
    fn loopback_resolves_through_the_retry_helper() -> Result<(), ResolveRejection> {
        let addresses = resolve_host_with_retry("127.0.0.1", 80)?;
        assert!(addresses.contains(&IpAddr::from([127, 0, 0, 1])));
        Ok(())
    }

    #[test]
    fn prefer_public_drops_private_loopback_and_dedupes() {
        let classifier = PublicAddressClassifier;
        let mixed = vec![
            IpAddr::from([10, 0, 0, 1]),
            IpAddr::from([8, 8, 8, 8]),
            IpAddr::from([127, 0, 0, 1]),
            IpAddr::from([1, 1, 1, 1]),
            IpAddr::from([8, 8, 8, 8]),
        ];
        assert_eq!(
            prefer_public_addresses(mixed, &classifier),
            vec![IpAddr::from([8, 8, 8, 8]), IpAddr::from([1, 1, 1, 1])]
        );
    }

    #[test]
    fn prefer_public_prefers_v4_over_v6() {
        let classifier = PublicAddressClassifier;
        let mixed = vec![
            IpAddr::from([0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111]),
            IpAddr::from([1, 1, 1, 1]),
        ];
        let out = prefer_public_addresses(mixed, &classifier);
        assert_eq!(out.first(), Some(&IpAddr::from([1, 1, 1, 1])));
    }
}
