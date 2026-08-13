//! Tests for `client/profile.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::refresh::cache_entry_fresh;
use caly_profile::profile_store::ProfileCacheEntry;

fn entry(fetched_at_ms: u64) -> ProfileCacheEntry {
    ProfileCacheEntry {
        url: "https://example.com/p.yaml".to_owned(),
        fetched_at_ms,
        body_bytes: 10,
        sha256_hex: None,
        etag: None,
        last_modified: None,
    }
}

#[test]
fn cache_entry_fresh_honours_the_declared_interval() {
    let now = 1_800_000_000_000u64;
    // fetched 30 minutes ago, interval 60 → fresh
    assert!(cache_entry_fresh(Some(&entry(now - 30 * 60_000)), 60, now));
    // fetched 90 minutes ago, interval 60 → stale
    assert!(!cache_entry_fresh(Some(&entry(now - 90 * 60_000)), 60, now));
}

#[test]
fn cache_entry_fresh_edge_cases_are_conservative() {
    let now = 1_800_000_000_000u64;
    assert!(!cache_entry_fresh(None, 60, now), "nothing cached");
    assert!(
        !cache_entry_fresh(Some(&entry(now)), 0, now),
        "zero interval must never freeze a profile"
    );
    assert!(
        !cache_entry_fresh(Some(&entry(now + 60_000_000)), 60, now),
        "a clock moved backwards counts as stale (saturating)"
    );
    assert!(
        cache_entry_fresh(Some(&entry(now - 1)), u32::MAX, now),
        "a huge interval saturates the ms conversion without wrapping"
    );
}
