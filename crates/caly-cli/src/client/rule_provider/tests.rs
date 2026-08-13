//! Tests for `client/rule_provider.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::test_helpers::temp_root;
use std::fs;

fn seed_at_roots(dir: &std::path::Path) -> AppPaths {
    let paths = crate::test_helpers::hermetic_paths(dir);
    let target = paths.config.join("config.yaml");
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(
        &target,
        "schema_version: 1\ncore: mihomo\nrule_providers: []\n",
    )
    .unwrap();
    paths
}

#[test]
fn add_http_dry_run_does_not_touch_disk() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let outcome = add_provider(
        &paths,
        "geoip-cn",
        &RpSourceSpec::Http {
            url: "https://example.com/geoip-cn".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        false,
    )
    .unwrap();
    assert_eq!(outcome, RpWriteOutcome::DryRun);
    let expected = "schema_version: 1\ncore: mihomo\nrule_providers: []\n";
    let after = std::fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(after, expected);
}

#[test]
fn add_http_apply_persists_a_new_provider() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let outcome = add_provider(
        &paths,
        "geoip-cn",
        &RpSourceSpec::Http {
            url: "https://example.com/geoip-cn".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::DomainSuffix,
        true,
    )
    .unwrap();
    assert_eq!(outcome, RpWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let providers = value
        .get("rule_providers")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("rule_providers must exist after a successful write"));
    assert_eq!(providers.len(), 1);
    assert_eq!(
        providers[0].get("name").and_then(|v| v.as_str()),
        Some("geoip-cn")
    );
    assert_eq!(
        providers[0].get("type").and_then(|v| v.as_str()),
        Some("http")
    );
    assert_eq!(
        providers[0].get("behavior").and_then(|v| v.as_str()),
        Some("domain_suffix")
    );
    // The default `enabled: true` is preserved.
    assert_eq!(
        providers[0]
            .get("enabled")
            .and_then(serde_norway::Value::as_bool),
        Some(true)
    );
    // A backup sidecar must exist.
    let backup = paths.config.join("config.yaml.bak");
    assert!(backup.exists(), "backup sidecar must be written");
}

#[test]
fn add_inline_apply_writes_an_inline_provider() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let outcome = add_provider(
        &paths,
        "local-rules",
        &RpSourceSpec::Inline {
            payload: "DOMAIN,example.com,auto\n".to_owned(),
        },
        RpBehavior::Classical,
        true,
    )
    .unwrap();
    assert_eq!(outcome, RpWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let providers = value
        .get("rule_providers")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("rule_providers must exist after a successful write"));
    assert_eq!(providers.len(), 1);
    assert_eq!(
        providers[0].get("type").and_then(|v| v.as_str()),
        Some("inline")
    );
}

#[test]
fn add_provider_rejects_duplicate_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_provider(
        &paths,
        "geoip-cn",
        &RpSourceSpec::Http {
            url: "https://example.com/geoip-cn".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    )
    .unwrap();
    let result = add_provider(
        &paths,
        "geoip-cn",
        &RpSourceSpec::Http {
            url: "https://example.com/other".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    );
    // Round 21: the duplicate check now surfaces
    // `AlreadyDeclared` (not the generic `Write`).
    assert!(matches!(result, Err(RpWriteError::AlreadyDeclared(_))));
}

#[test]
fn add_provider_rejects_unsafe_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = add_provider(
        &paths,
        "../escape",
        &RpSourceSpec::Http {
            url: "https://example.com/x".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    );
    assert!(matches!(result, Err(RpWriteError::InvalidName(_))));
}

#[test]
fn add_provider_rejects_non_http_url() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = add_provider(
        &paths,
        "x",
        &RpSourceSpec::Http {
            url: "file:///etc/passwd".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    );
    assert!(matches!(result, Err(RpWriteError::InvalidUrl(_))));
}

#[test]
fn remove_provider_filters_a_matching_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_provider(
        &paths,
        "a",
        &RpSourceSpec::Http {
            url: "https://example.com/a".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    )
    .unwrap();
    add_provider(
        &paths,
        "b",
        &RpSourceSpec::Http {
            url: "https://example.com/b".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    )
    .unwrap();
    let outcome = remove_provider(&paths, "a", true).unwrap();
    assert_eq!(outcome, RpWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let providers = value
        .get("rule_providers")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("rule_providers must exist after a successful write"));
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].get("name").and_then(|v| v.as_str()), Some("b"));
}

#[test]
fn remove_provider_rejects_unknown_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = remove_provider(&paths, "nope", true);
    // Round 21: the unknown-name check now surfaces
    // `NotDeclared` (not the generic `Write`).
    assert!(matches!(result, Err(RpWriteError::NotDeclared(_))));
}

#[test]
fn set_enabled_flips_the_flag() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_provider(
        &paths,
        "a",
        &RpSourceSpec::Http {
            url: "https://example.com/a".to_owned(),
            interval_ms: 86_400_000,
        },
        RpBehavior::Domain,
        true,
    )
    .unwrap();
    let outcome = set_enabled(&paths, "a", false, true).unwrap();
    assert_eq!(outcome, RpWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let providers = value
        .get("rule_providers")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("rule_providers must exist after a successful write"));
    assert_eq!(
        providers[0]
            .get("enabled")
            .and_then(serde_norway::Value::as_bool),
        Some(false)
    );
}

#[test]
fn set_enabled_dry_run_against_unknown_name_surfaces_not_declared() {
    // Round 27 (debug): the pre-Round 27 shape
    // returned `DryRun` (success) for an unknown
    // name on the dry-run path, but `NotDeclared`
    // (error) on the apply path. The dry-run now
    // previews the apply outcome: an unknown
    // name surfaces `NotDeclared` regardless of
    // `apply`, matching the `add_provider` /
    // `remove_provider` writers' contract. The
    // `set_enabled_flips_the_flag` positive test
    // above locks the `apply` path; this test
    // locks the `dry-run` path's existence
    // check.
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = set_enabled(&paths, "nope", false, false);
    assert!(
        matches!(result, Err(RpWriteError::NotDeclared(_))),
        "dry-run enable on an unknown name must fail with NotDeclared, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}
