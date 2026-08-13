//! Stable source-URL identity (P8b: moved up from `caly-backends` so the
//! presentation layer can derive subscription ids without reaching into
//! adapters — this is intake-domain function, not a port implementation).

use caly_domain::SubscriptionId;

/// Stable 16-byte `SubscriptionId` from a source URL digest.
///
/// Audit #120: hash the *normalised* URL — `url::Url` parsing lowercases
/// scheme/host, strips a redundant default port and the empty-path
/// distinction, so equivalent spellings of one source share the id/cache
/// entry instead of fragmenting validators and cached bodies across
/// duplicates.
pub fn subscription_id_for_url(url: &str) -> SubscriptionId {
    use sha2::{Digest, Sha256};
    let normalized =
        url::Url::parse(url).map_or_else(|_| url.to_owned(), |parsed| parsed.to_string());
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    SubscriptionId::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_across_equivalent_spellings() {
        let a = subscription_id_for_url("https://example.com/sub");
        // url::Url normalisation lowercases the host and strips the
        // redundant default port; both spellings must share one id
        // (audit #120: no cache fragmentation across duplicates).
        let b = subscription_id_for_url("HTTPS://EXAMPLE.COM:443/sub");
        assert_eq!(a, b);
        // Distinct sources stay distinct.
        let c = subscription_id_for_url("https://example.com/other");
        assert_ne!(a, c);
        // Unparseable input degrades to hashing the raw string (stable).
        let d = subscription_id_for_url("not a url");
        let e = subscription_id_for_url("not a url");
        assert_eq!(d, e);
    }
}
