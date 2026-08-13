//! Tests for `profile_store.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

fn unique_dir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("caly-profile-store-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

#[test]
fn cache_path_rejects_path_unsafe_id() {
    let store = ProfileStore::new(unique_dir("cfg"), unique_dir("state"));
    assert!(matches!(
        store.cache_path("../etc"),
        Err(ProfileStoreError::InvalidId)
    ));
    assert!(matches!(
        store.cache_path("a/b"),
        Err(ProfileStoreError::InvalidId)
    ));
}

#[test]
fn read_returns_none_when_cache_missing() {
    let store = ProfileStore::new(unique_dir("cfg-m"), unique_dir("state-m"));
    let result = store.read("team").unwrap();
    assert!(result.is_none());
}

#[test]
fn write_then_read_round_trip() {
    let store = ProfileStore::new(unique_dir("cfg-w"), unique_dir("state-w"));
    store
        .write(
            "team",
            b"core: mihomo\ndaemon:\n  listen: 127.0.0.1:17890\n",
            "https://example.com/team.yaml",
            1_700_000_000_000,
        )
        .unwrap();
    let body = store.read("team").unwrap();
    let body = body.unwrap();
    assert!(body.starts_with(b"core: mihomo"));
    let meta = store.read_metadata("team").unwrap().unwrap();
    assert_eq!(meta.url, "https://example.com/team.yaml");
    assert_eq!(meta.fetched_at_ms, 1_700_000_000_000);
    assert_eq!(meta.body_bytes, body.len());
}

#[test]
fn write_rejects_oversize_body() {
    let store = ProfileStore::new(unique_dir("cfg-big"), unique_dir("state-big"));
    let big = vec![b'x'; caly_domain::PROFILE_BODY_MAX_BYTES + 1];
    let error = store
        .write("big", &big, "https://example.com/big", 1)
        .err()
        .unwrap();
    assert!(matches!(error, ProfileStoreError::BodyTooLarge { .. }));
}

#[test]
fn remove_clears_body_and_metadata() {
    let store = ProfileStore::new(unique_dir("cfg-r"), unique_dir("state-r"));
    store
        .write("x", b"core: mihomo\n", "https://example.com/x", 1)
        .unwrap();
    store.remove("x").unwrap();
    assert!(store.read("x").unwrap().is_none());
    assert!(store.read_metadata("x").unwrap().is_none());
}

#[test]
fn list_cached_returns_only_yaml_files() {
    let store = ProfileStore::new(unique_dir("cfg-l"), unique_dir("state-l"));
    store.write("a", b"a: 1\n", "u", 1).unwrap();
    store.write("b", b"b: 2\n", "u", 1).unwrap();
    let mut ids = store.list_cached().unwrap();
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0].as_str(), "a");
    assert_eq!(ids[1].as_str(), "b");
}

#[test]
fn resolver_falls_back_to_local_file() {
    let config_root = unique_dir("cfg-resolver");
    let state_root = unique_dir("state-resolver");
    let profiles = config_root.join("profiles");
    fs::create_dir_all(&profiles).unwrap_or_default();
    fs::write(profiles.join("manual.yaml"), b"core: mihomo\n").unwrap_or_default();
    let store = ProfileStore::new(config_root, state_root);
    let body = ProfileBodyResolver::resolve(&store, "manual").unwrap();
    assert!(body.starts_with(b"core: mihomo"));
}
