//! Tests for `loader/layered.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

use caly_platform::paths::test_helpers::unique_path;

fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_default();
    }
    fs::write(&path, body).unwrap_or_default();
}

fn unique_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = unique_path(&format!("caly-loader-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

#[test]
fn declared_profiles_are_merged_in_declaration_order() {
    let root = unique_root("merge");
    write(
        &root,
        "config.yaml",
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: team\n    kind: remote\n    url: https://example.com/team.yaml\n    interval_minutes: 60\n  - id: local\n    kind: local\n    path: extra.yaml\n",
    );
    // Body for `team` (Remote) and `local` (Local) are served by
    // the in-memory resolver; production wiring uses the on-disk
    // store. The deep-merge order is: base → fragments →
    // declared-profiles, so the resolver runs **after** the
    // base config has been read.
    let mut resolver = InMemoryProfileResolver::new();
    resolver.insert("team", b"rules:\n  - DOMAIN-SUFFIX,example.com,DIRECT\n");
    resolver.insert("local", b"tun:\n  enabled: true\n  mtu: 1400\n");

    let paths = LayeredConfigPaths::new(root.clone(), None);
    let config = load_layered_yaml_with(&paths, LoaderLimits::secure_default(), &resolver)
        .unwrap_or_else(|error| panic!("loader must succeed: {error:?}"));
    // The declared profile set is preserved in the merged config;
    // each body's deep-merged keys reach the AppConfig.
    assert_eq!(config.profiles.len(), 2);
    assert!(config.tun.enabled);
    assert_eq!(config.tun.mtu, 1400);
    assert!(config
        .rules
        .iter()
        .any(|line| line.contains("DOMAIN-SUFFIX,example.com")));

    fs::remove_dir_all(&root).ok();
}

#[test]
fn declared_profile_with_unresolved_body_fails_clean() {
    let root = unique_root("missing");
    write(
        &root,
        "config.yaml",
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: ghost\n    kind: remote\n    url: https://example.com/ghost.yaml\n    interval_minutes: 60\n",
    );
    let resolver = InMemoryProfileResolver::new();
    let paths = LayeredConfigPaths::new(root.clone(), None);
    let error = load_layered_yaml_with(&paths, LoaderLimits::secure_default(), &resolver)
        .err()
        .unwrap();
    match error {
        LayeredConfigError::ProfileBody { id, reason } => {
            assert_eq!(id, "ghost");
            assert!(reason.as_str().contains("ghost"));
        }
        other => {
            panic!("expected ProfileBody, got {other:?}")
        }
    }
    fs::remove_dir_all(&root).ok();
}

#[test]
fn declared_profile_with_invalid_yaml_body_reports_per_profile() {
    let root = unique_root("bad");
    write(
        &root,
        "config.yaml",
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: bad\n    kind: local\n    path: bad.yaml\n",
    );
    let mut resolver = InMemoryProfileResolver::new();
    resolver.insert("bad", b"this: is: not: valid: yaml: [[[");
    let paths = LayeredConfigPaths::new(root.clone(), None);
    let error = load_layered_yaml_with(&paths, LoaderLimits::secure_default(), &resolver)
        .err()
        .unwrap();
    match error {
        LayeredConfigError::ProfileYaml { id, .. } => {
            assert_eq!(id, "bad");
        }
        other => {
            panic!("expected ProfileYaml, got {other:?}")
        }
    }
    fs::remove_dir_all(&root).ok();
}

#[test]
fn empty_profiles_list_keeps_base_config_intact() {
    let root = unique_root("empty");
    write(&root, "config.yaml", "schema_version: 1\ncore: mihomo\n");
    let resolver = InMemoryProfileResolver::new();
    let paths = LayeredConfigPaths::new(root.clone(), None);
    let config = load_layered_yaml_with(&paths, LoaderLimits::secure_default(), &resolver)
        .unwrap_or_else(|error| panic!("loader must succeed: {error:?}"));
    assert!(config.profiles.is_empty());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn lenient_resolver_skips_cache_miss_profiles() {
    // The offline CLI uses a lenient resolver so a `Remote`
    // profile whose cache is missing does not abort the load.
    // The deep-merge treats the empty body as a no-op, so the
    // merged `AppConfig` still reflects every other layer.
    let root = unique_root("lenient");
    write(
        &root,
        "config.yaml",
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: team\n    kind: remote\n    url: https://example.com/team.yaml\n    interval_minutes: 60\n  - id: local\n    kind: local\n    path: extra.yaml\n",
    );
    // Only the `local` profile has a body; `team` is empty.
    let mut resolver = InMemoryProfileResolver::lenient();
    resolver.insert("local", b"tun:\n  enabled: true\n");
    let paths = LayeredConfigPaths::new(root.clone(), None);
    let config = load_layered_yaml_with(&paths, LoaderLimits::secure_default(), &resolver)
        .unwrap_or_else(|error| panic!("loader must succeed: {error:?}"));
    assert_eq!(config.profiles.len(), 2);
    assert!(config.tun.enabled);
    fs::remove_dir_all(&root).ok();
}
