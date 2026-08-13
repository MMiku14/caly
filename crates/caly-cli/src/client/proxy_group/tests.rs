//! Tests for `client/proxy_group.rs`, extracted to the sibling
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
        "schema_version: 1\ncore: mihomo\nproxy_groups: []\n",
    )
    .unwrap();
    paths
}

#[test]
fn add_select_dry_run_does_not_touch_disk() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let outcome = add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(outcome, PgWriteOutcome::DryRun);
    let expected = "schema_version: 1\ncore: mihomo\nproxy_groups: []\n";
    let after = std::fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(after, expected);
}

#[test]
fn add_url_test_without_url_is_rejected_early() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = add_group(
        &paths,
        "Auto",
        PgTypeSpec::UrlTest,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    );
    assert!(matches!(result, Err(PgWriteError::MissingUrl(_))));
}

#[test]
fn add_select_with_url_is_rejected_early() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        Some("http://example.com/probe"),
        None,
        None,
        true,
    );
    assert!(matches!(result, Err(PgWriteError::UnexpectedUrl(_))));
}

#[test]
fn add_url_test_apply_persists_a_group() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let outcome = add_group(
        &paths,
        "Auto",
        PgTypeSpec::UrlTest,
        &[PgMemberSpec::Direct],
        Some("http://www.gstatic.com/generate_204"),
        Some(300),
        Some(50),
        true,
    )
    .unwrap_or_else(|error| panic!("must succeed: {error}"));
    assert_eq!(outcome, PgWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let groups = value
        .get("proxy_groups")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("proxy_groups must exist after a successful write"));
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].get("name").and_then(|v| v.as_str()), Some("Auto"));
    assert_eq!(
        groups[0].get("type").and_then(|v| v.as_str()),
        Some("url-test")
    );
    let url_test = groups[0]
        .get("url_test")
        .and_then(|v| v.as_mapping())
        .unwrap();
    assert_eq!(
        url_test.get("url").and_then(|v| v.as_str()),
        Some("http://www.gstatic.com/generate_204")
    );
    assert_eq!(
        url_test
            .get("interval_seconds")
            .and_then(serde_norway::Value::as_u64),
        Some(300)
    );
    assert_eq!(
        url_test
            .get("tolerance_ms")
            .and_then(serde_norway::Value::as_u64),
        Some(50)
    );
    // The backup sidecar must exist.
    let backup = paths.config.join("config.yaml.bak");
    assert!(backup.exists(), "backup sidecar must be written");
}

#[test]
fn add_group_rejects_unsafe_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = add_group(
        &paths,
        "../escape",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    );
    assert!(matches!(result, Err(PgWriteError::InvalidName(_))));
}

#[test]
fn add_group_rejects_duplicate_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    )
    .unwrap();
    let result = add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    );
    assert!(matches!(result, Err(PgWriteError::AlreadyDeclared(_))));
}

#[test]
fn remove_group_filters_a_matching_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_group(
        &paths,
        "a",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    )
    .unwrap();
    add_group(
        &paths,
        "b",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    )
    .unwrap();
    let outcome = remove_group(&paths, "a", true).unwrap();
    assert_eq!(outcome, PgWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let groups = value
        .get("proxy_groups")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("proxy_groups must exist after a successful write"));
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].get("name").and_then(|v| v.as_str()), Some("b"));
}

#[test]
fn remove_group_rejects_unknown_name() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = remove_group(&paths, "nope", true);
    assert!(matches!(result, Err(PgWriteError::NotDeclared(_))));
}

#[test]
fn set_enabled_flips_the_flag() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    )
    .unwrap();
    let outcome = set_enabled(&paths, "Proxy", false, true).unwrap();
    assert_eq!(outcome, PgWriteOutcome::Applied);
    let bytes = std::fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let groups = value
        .get("proxy_groups")
        .and_then(|v| v.as_sequence())
        .unwrap_or_else(|| panic!("proxy_groups must exist after a successful write"));
    assert_eq!(
        groups[0]
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
    // (error) on the apply path. The dry-run
    // now previews the apply outcome: an
    // unknown name surfaces `NotDeclared`
    // regardless of `apply`, matching the
    // `add_group` / `remove_group` writers'
    // contract. The `set_enabled_flips_the_flag`
    // positive test above locks the `apply`
    // path; this test locks the `dry-run`
    // path's existence check.
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    let result = set_enabled(&paths, "nope", false, false);
    assert!(
        matches!(result, Err(PgWriteError::NotDeclared(_))),
        "dry-run enable on an unknown name must fail with NotDeclared, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Round 31: an idempotent `set proxy-group
/// enable <already-enabled-group>` is a
/// `NoChange` (not `Applied`). The pre-Round 31
/// shape returned `Applied` even when the
/// on-disk `enabled` flag was already the
/// target value, so the operator saw
/// `proxy group enabled` after a redundant
/// call. The Round 25 sub writer already
/// surfaces `NoChange` for the analogous
/// `set sub enable <already-enabled-url>`;
/// the proxy-group writer now matches that
/// contract. The dispatch's `run_writer`
/// envelope maps the `NoChange` kind to the
/// `Summaries::no_change` line, so the
/// operator-visible summary is
/// `proxy group already enabled`.
#[test]
fn set_enabled_idempotent_call_surfaces_no_change_in_dispatch() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    // Apply an `add` to populate the group
    // (default `enabled: true` per the schema).
    add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    )
    .unwrap();
    // Capture the on-disk bytes so the
    // idempotent `enable` does not rewrite
    // them (the pre-Round 31 shape silently
    // rewrote the file even when the state
    // was already the target).
    let config_path = paths.config.join("config.yaml");
    let before = fs::read(&config_path).unwrap();
    // The idempotent call: `enable` on an
    // already-enabled group with
    // `apply: true`. Round 31 expectation:
    // `NoChange` (not `Applied`), the
    // on-disk file is NOT rewritten, and the
    // dispatch's envelope surfaces
    // `no_change: true` for JSON consumers.
    let result = set_enabled(&paths, "Proxy", true, true);
    assert!(
        matches!(result, Ok(PgWriteOutcome::NoChange)),
        "idempotent enable must surface NoChange, got {result:?}"
    );
    let after = fs::read(&config_path).unwrap();
    assert_eq!(
        before, after,
        "idempotent enable must not rewrite config.yaml"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Round 31: the symmetric `set proxy-group
/// disable <already-disabled-group>` also
/// surfaces `NoChange`. The pre-Round 31
/// shape returned `Applied` for the
/// idempotent disable as well; the new
/// shape is symmetric across the two
/// verbs.
#[test]
fn set_disabled_idempotent_call_surfaces_no_change_in_dispatch() {
    let dir = temp_root("__FUNC__");
    let paths = seed_at_roots(dir.as_path());
    add_group(
        &paths,
        "Proxy",
        PgTypeSpec::Select,
        &[PgMemberSpec::Direct],
        None,
        None,
        None,
        true,
    )
    .unwrap();
    // First `disable` flips the flag to
    // `enabled: false`.
    set_enabled(&paths, "Proxy", false, true).unwrap();
    // Second `disable` is the idempotent
    // call: the group is already disabled.
    let result = set_enabled(&paths, "Proxy", false, true);
    assert!(
        matches!(result, Ok(PgWriteOutcome::NoChange)),
        "idempotent disable must surface NoChange, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}
