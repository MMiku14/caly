//! Tests for `client/subscription.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::test_helpers::{hermetic_paths, temp_root};
use caly_profile::schema::AppConfig;
use std::fs;

// W2-β2a: the three-segment intake helpers live in the sibling
// `sources` module; the parent only re-exports the four writers.
use super::sources::{SourceKind, normalize_source_token};

fn seed_config_at_roots(dir: &std::path::Path) -> AppPaths {
    let paths = hermetic_paths(dir);
    let target = paths.config.join("config.yaml");
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(
        &target,
        "schema_version: 1\ncore: mihomo\nsubscriptions: {}\n",
    )
    .unwrap();
    paths
}

/// Local tag used by every `temp_root` call in this
/// module. Round 26 collapsed the bespoke
/// `temp_root` helper into the cross-module
/// [`crate::test_helpers::temp_root`]; the tag
/// is the only thing that distinguishes this
/// module's fixtures under `/tmp` from the
/// fixtures of any other module. Pre-Round 26
/// the local `temp_root` used a `caly-sub-{name}-…`
/// prefix, so the tag is `"sub"` to keep the
/// resulting `/tmp/caly-sub-…-…-…` path shape
/// identical to the pre-refactor fixture paths
/// (a CI log-grep that scans for `caly-sub-…`
/// still works).
const FIXTURE_TAG: &str = "sub";

#[test]
fn validate_url_rejects_non_http_schemes() {
    assert!(is_http_url("file:///etc/passwd").is_err());
    assert!(is_http_url("data:text/plain,hello").is_err());
    assert!(is_http_url("").is_err());
    assert!(is_http_url("https://example.com/sub").is_ok());
}

#[test]
fn add_source_dry_run_does_not_touch_disk() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let outcome = add_source(&paths, "https://example.com/sub", None, None, false).unwrap();
    assert_eq!(outcome, SubWriteOutcome::DryRun);
    let expected = "schema_version: 1\ncore: mihomo\nsubscriptions: {}\n";
    let after = std::fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(after, expected);
}

#[test]
fn add_source_apply_persists_a_new_source() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let outcome = add_source(&paths, "https://example.com/sub", None, None, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    let bytes = fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    let Some(sources) = value
        .get("subscriptions")
        .and_then(|v| v.get("sources"))
        .and_then(|v| v.as_sequence())
    else {
        panic!("subscriptions.sources must exist after a successful write");
    };
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].get("url").and_then(|v| v.as_str()),
        Some("https://example.com/sub")
    );
    assert_eq!(
        sources[0]
            .get("enabled")
            .and_then(serde_norway::Value::as_bool),
        Some(true)
    );
    let backup = paths.config.join("config.yaml.bak");
    assert!(backup.exists(), "backup sidecar must be written");
}

#[test]
fn add_source_rejects_duplicate_url() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://example.com/sub", None, None, true).unwrap();
    let result = add_source(&paths, "https://example.com/sub", None, None, true);
    assert!(matches!(result, Err(SubCmdError::AlreadyDeclared(_))));
}

#[test]
fn add_source_trims_url_before_storing() {
    // Round 27 (debug): the pre-Round 27 shape
    // called `is_http_url(url).map_err(...)`
    // and *discarded* the trimmed URL with
    // `let _ = ...?;`, then stored the raw
    // `url.to_owned()` on disk. A
    // whitespace-padded URL was therefore
    // stored with its leading / trailing
    // whitespace intact, breaking the
    // round-trip with `remove_source` /
    // `enable_source` (which compared against
    // the raw input but the on-disk form
    // was the trimmed one — they matched
    // only by accident, not by design). The
    // post-Round 27 contract: the trimmed
    // URL is the single source of truth, used
    // for the duplicate check *and* the
    // on-disk entry.
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    // First add: a URL with stray whitespace.
    add_source(&paths, "  https://example.com/sub  ", None, None, true).unwrap();
    // Re-read the layered config: the on-disk
    // `url` field must be the trimmed form,
    // not the raw input.
    let config: AppConfig = caly_profile::loader::load_layered_yaml_with(
        &caly_profile::loader::LayeredConfigPaths::new(paths.config.clone(), None),
        caly_profile::loader::LoaderLimits::secure_default(),
        &caly_profile::loader::InMemoryProfileResolver::lenient(),
    )
    .unwrap();
    assert_eq!(config.subscriptions.sources.len(), 1);
    assert_eq!(
        config.subscriptions.sources[0].url,
        "https://example.com/sub"
    );
    // Second add: the trimmed form. Pre-Round
    // 27 this would have created a second
    // entry (the on-disk form was the trimmed
    // one, but the duplicate check compared
    // against the raw input which didn't
    // match the on-disk trimmed form, so the
    // check missed it). Post-Round 27 the
    // duplicate check fires against the
    // trimmed form and the second add is
    // rejected.
    let result = add_source(&paths, "https://example.com/sub", None, None, true);
    assert!(
        matches!(result, Err(SubCmdError::AlreadyDeclared(_))),
        "expected AlreadyDeclared for trimmed duplicate, got {result:?}"
    );
    // And the on-disk source list is still
    // exactly one entry.
    let config: AppConfig = caly_profile::loader::load_layered_yaml_with(
        &caly_profile::loader::LayeredConfigPaths::new(paths.config.clone(), None),
        caly_profile::loader::LoaderLimits::secure_default(),
        &caly_profile::loader::InMemoryProfileResolver::lenient(),
    )
    .unwrap();
    assert_eq!(config.subscriptions.sources.len(), 1);
}

#[test]
fn remove_source_filters_a_matching_url() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://a.example.com/sub", None, None, true).unwrap();
    add_source(&paths, "https://b.example.com/sub", None, None, true).unwrap();
    let outcome = remove_source(&paths, "https://a.example.com/sub", false, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    let config: AppConfig = caly_profile::loader::load_layered_yaml_with(
        &caly_profile::loader::LayeredConfigPaths::new(paths.config.clone(), None),
        caly_profile::loader::LoaderLimits::secure_default(),
        &caly_profile::loader::InMemoryProfileResolver::lenient(),
    )
    .unwrap();
    assert_eq!(config.subscriptions.sources.len(), 1);
    assert_eq!(
        config.subscriptions.sources[0].url,
        "https://b.example.com/sub"
    );
}

#[test]
fn remove_source_rejects_unknown_url() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let result = remove_source(&paths, "https://unknown.example.com/sub", false, true);
    assert!(matches!(result, Err(SubCmdError::NotDeclared(_))));
}

#[test]
fn disable_source_flips_the_enabled_flag() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://a.example.com/sub", None, None, true).unwrap();
    let outcome = disable_source(&paths, "https://a.example.com/sub", true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    let config: AppConfig = caly_profile::loader::load_layered_yaml_with(
        &caly_profile::loader::LayeredConfigPaths::new(paths.config.clone(), None),
        caly_profile::loader::LoaderLimits::secure_default(),
        &caly_profile::loader::InMemoryProfileResolver::lenient(),
    )
    .unwrap();
    assert!(!config.subscriptions.sources[0].enabled);
}

#[test]
fn enable_dry_run_against_unknown_url_surfaces_not_declared() {
    // Round 27 (debug): the pre-Round 27 shape
    // returned `DryRun` (success) for an unknown
    // URL on the dry-run path, but `NotDeclared`
    // (error) for the same URL on the apply
    // path. The dry-run now previews the apply
    // outcome: an unknown URL surfaces
    // `NotDeclared` regardless of `apply`, so
    // the operator's `--dry-run` safety check
    // reports the same failure mode that
    // `--apply` would. Compare to
    // `add_source_rejects_duplicate_url` and
    // `remove_source_rejects_unknown_url` —
    // the `add` and `remove` writers had this
    // contract all along; the `enable` /
    // `disable` writer was the inconsistent
    // outlier.
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let result = enable_source(&paths, "https://unknown.example.com/sub", false);
    assert!(
        matches!(result, Err(SubCmdError::NotDeclared(_))),
        "dry-run enable on an unknown URL must fail with NotDeclared, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enable_dry_run_against_known_url_succeeds() {
    // The corresponding positive case: an
    // existing URL's `enable --dry-run` returns
    // `DryRun` (success), so the operator's
    // dry-run-as-safety-check shape is
    // consistent with the apply path's
    // `NoChange` / `Applied` distinction.
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://known.example.com/sub", None, None, true).unwrap();
    let outcome = enable_source(&paths, "https://known.example.com/sub", false).unwrap();
    assert_eq!(outcome, SubWriteOutcome::DryRun);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enable_source_reports_no_change_when_already_enabled() {
    // Round 25 (debug): the previous shape
    // returned `Applied` (apply) or `DryRun`
    // (dry-run) when the on-disk `enabled` flag
    // was already the target state, which made
    // `set sub enable <already-enabled-url>
    // --apply` report a successful write with no
    // disk I/O. The new contract: the writer
    // surfaces an explicit `NoChange` outcome so
    // the dispatch can emit a distinct "already
    // enabled" summary. Lock both the apply
    // path (the one that previously lied) and
    // the dry-run path (which still returns
    // `DryRun` because the user did not pass
    // `--apply`).
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://a.example.com/sub", None, None, true).unwrap();
    // Apply path: returns `NoChange`.
    let outcome = enable_source(&paths, "https://a.example.com/sub", true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::NoChange);
    // Dry-run path: still `DryRun` (the user
    // asked for a preview, the writer validated
    // without writing).
    let outcome = enable_source(&paths, "https://a.example.com/sub", false).unwrap();
    assert_eq!(outcome, SubWriteOutcome::DryRun);
    // Disable on an enabled source: `Applied`
    // (the on-disk state changes, so the writer
    // reaches the apply path).
    let outcome = disable_source(&paths, "https://a.example.com/sub", true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    // Disable on the now-disabled source:
    // `NoChange` (idempotent).
    let outcome = disable_source(&paths, "https://a.example.com/sub", true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::NoChange);
}

// ---------------------------------------------------------------- W2-β2a --
// Three-segment `sub add` (Q5 / C-G): token normalization, per-source
// cadence ruling, and the §8-4/§8-7 error contract.

fn read_sources(paths: &AppPaths) -> Vec<serde_norway::Value> {
    let bytes = fs::read(paths.config.join("config.yaml")).unwrap();
    let value: serde_norway::Value = serde_norway::from_slice(&bytes).unwrap();
    value
        .get("subscriptions")
        .and_then(|v| v.get("sources"))
        .and_then(|v| v.as_sequence())
        .cloned()
        .unwrap()
}

#[test]
fn normalize_source_token_accepts_http_and_trims() {
    let source = normalize_source_token("  https://example.com/sub  ").unwrap();
    assert_eq!(source.kind, SourceKind::Http);
    assert_eq!(source.url, "https://example.com/sub");
}

#[test]
fn normalize_source_token_rejects_unsupported_scheme() {
    let error = normalize_source_token("ftp://example.com/sub").unwrap_err();
    assert!(matches!(error, SubCmdError::InvalidUrl(_)));
}

#[test]
fn normalize_source_token_rejects_the_empty_token() {
    let error = normalize_source_token("   ").unwrap_err();
    assert!(matches!(error, SubCmdError::InvalidUrl(_)));
}

#[test]
fn normalize_source_token_missing_file_reports_three_lines() {
    let dir = temp_root(FIXTURE_TAG);
    let given = format!("{}/nosuch.yaml", dir.as_path().display());
    let error = normalize_source_token(&given).unwrap_err();
    let SubCmdError::SourceFileNotFound { .. } = error else {
        panic!("expected SourceFileNotFound, got {error:?}");
    };
    // §8-7 contract: what / resolved absolute / URL disambiguation.
    let rendered = format!("{error}");
    assert!(rendered.starts_with("file not found: "));
    assert!(rendered.contains("\nResolved absolute: "));
    assert!(rendered.contains("\nIf you meant a URL, include the scheme: https://..."));
}

#[test]
fn normalize_source_token_existing_file_becomes_file_url() {
    let dir = temp_root(FIXTURE_TAG);
    let file = dir.as_path().join("feed.yaml");
    fs::create_dir_all(dir.as_path()).unwrap();
    fs::write(&file, b"proxies: []\n").unwrap();
    let source = normalize_source_token(file.to_str().unwrap()).unwrap();
    assert_eq!(source.kind, SourceKind::File);
    assert!(source.url.starts_with("file://"));
    assert!(source.url.ends_with("/feed.yaml"));
    // An explicit file:// token parses to the same shape.
    let via_scheme = normalize_source_token(&source.url).unwrap();
    assert_eq!(via_scheme.kind, SourceKind::File);
}

#[test]
fn add_source_file_is_pinned_static() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let file = dir.as_path().join("feed.yaml");
    fs::write(&file, b"proxies: []\n").unwrap();
    let outcome = add_source(&paths, file.to_str().unwrap(), None, None, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    let sources = read_sources(&paths);
    assert_eq!(sources.len(), 1);
    assert!(
        sources[0]
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap()
            .starts_with("file://")
    );
    // Q5: file sources are always static → refresh_every_minutes: 0.
    assert_eq!(
        sources[0]
            .get("refresh_every_minutes")
            .and_then(serde_norway::Value::as_u64),
        Some(0)
    );
}

#[test]
fn add_source_file_with_every_is_a_usage_error() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let file = dir.as_path().join("feed.yaml");
    fs::write(&file, b"proxies: []\n").unwrap();
    let result = add_source(&paths, file.to_str().unwrap(), None, Some(120), true);
    assert!(matches!(result, Err(SubCmdError::EveryOnFile)));
}

#[test]
fn add_source_url_default_cadence_is_24h() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://example.com/sub", None, None, true).unwrap();
    let sources = read_sources(&paths);
    assert_eq!(
        sources[0]
            .get("refresh_every_minutes")
            .and_then(serde_norway::Value::as_u64),
        Some(1_440)
    );
}

#[test]
fn add_source_url_every_zero_pins_static() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(&paths, "https://example.com/sub", None, Some(0), true).unwrap();
    let sources = read_sources(&paths);
    assert_eq!(
        sources[0]
            .get("refresh_every_minutes")
            .and_then(serde_norway::Value::as_u64),
        Some(0)
    );
}

#[test]
fn add_source_rejects_a_duplicate_display_name() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(
        &paths,
        "https://a.example.com/sub",
        Some("airport"),
        None,
        true,
    )
    .unwrap();
    let result = add_source(
        &paths,
        "https://b.example.com/sub",
        Some("airport"),
        None,
        true,
    );
    let Err(SubCmdError::NameTaken(name)) = result else {
        panic!("expected NameTaken, got {result:?}");
    };
    assert_eq!(name, "airport");
    // §8-4 wording + hint contract.
    assert_eq!(
        format!("{}", SubCmdError::NameTaken("airport".to_owned())),
        "subscription \"airport\" already exists."
    );
    let hint = crate::output::ErrorHint::hint(&SubCmdError::NameTaken("airport".to_owned()))
        .expect("NameTaken carries a remediation hint");
    assert!(hint.contains("caly sub set airport --url"));
    assert!(hint.contains("caly sub add <url> --name <other>"));
}

// ---------------------------------------------------------------- W2-β2b --
// Name-or-URL addressing, `sub set`, `remove --purge`.

use super::sources::{resolve_source_ref, set_source};

/// Seeds two sources (one named) and returns the hermetic paths.
fn seed_two_sources(tag: &str) -> (std::path::PathBuf, AppPaths) {
    let dir = temp_root(tag);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(
        &paths,
        "https://a.example.com/sub",
        Some("alpha"),
        None,
        true,
    )
    .unwrap();
    add_source(&paths, "https://b.example.com/sub", None, Some(720), true).unwrap();
    (dir, paths)
}

#[test]
fn resolve_source_ref_addresses_by_url_and_by_name() {
    let (_dir, paths) = seed_two_sources("sub-ref");
    assert_eq!(
        resolve_source_ref(&paths, "https://a.example.com/sub").unwrap(),
        "https://a.example.com/sub"
    );
    assert_eq!(
        resolve_source_ref(&paths, "alpha").unwrap(),
        "https://a.example.com/sub"
    );
}

#[test]
fn resolve_source_ref_unknown_tokens_are_not_declared() {
    let (_dir, paths) = seed_two_sources("sub-ref-miss");
    assert!(matches!(
        resolve_source_ref(&paths, "https://unknown.example.com/sub"),
        Err(SubCmdError::NotDeclared(_))
    ));
    assert!(matches!(
        resolve_source_ref(&paths, "ghost"),
        Err(SubCmdError::NotDeclared(_))
    ));
}

/// 2026-08-12 (订阅管理收敛): a bare integer addresses the 1-based
/// add-order id `sub list` displays — `1` = the first added source,
/// `2` = the second. Names still win over ids, and ids past the list
/// length are a ranged `NotDeclared`.
#[test]
fn resolve_source_ref_addresses_by_add_order_id() {
    let (_dir, paths) = seed_two_sources("sub-ref-id");
    // First added (named `alpha`) is id 1.
    assert_eq!(
        resolve_source_ref(&paths, "1").unwrap(),
        "https://a.example.com/sub"
    );
    // Second added (unnamed, 720-minute cadence) is id 2.
    assert_eq!(
        resolve_source_ref(&paths, "2").unwrap(),
        "https://b.example.com/sub"
    );
    // Out of range: actionable message, not a bare miss.
    match resolve_source_ref(&paths, "3") {
        Err(SubCmdError::NotDeclared(message)) => {
            assert!(message.contains("out of range"), "{message}");
            assert!(message.contains("2 source(s)"), "{message}");
        }
        other => panic!("expected ranged NotDeclared, got {other:?}"),
    }
    // Id 0 is rejected as invalid, not resolved.
    assert!(matches!(
        resolve_source_ref(&paths, "0"),
        Err(SubCmdError::InvalidUrl(_))
    ));
}

/// The id is stable across renames: `sub set 1 --name new` keeps the
/// source at position 1, so its id does not change.
#[test]
fn resolve_source_ref_id_is_stable_across_rename() {
    let (_dir, paths) = seed_two_sources("sub-ref-rename");
    set_source(&paths, "1", None, Some("renamed"), None, true).unwrap();
    // Still id 1, now also addressable by the new name.
    assert_eq!(
        resolve_source_ref(&paths, "1").unwrap(),
        "https://a.example.com/sub"
    );
    assert_eq!(
        resolve_source_ref(&paths, "renamed").unwrap(),
        "https://a.example.com/sub"
    );
}

#[test]
fn resolve_source_ref_duplicated_name_is_a_usage_error() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = hermetic_paths(dir.as_path());
    fs::create_dir_all(&paths.config).unwrap();
    // Hand-edited config (the add path refuses this): duplicate names.
    fs::write(
        paths.config.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nsubscriptions:\n  sources:\n  - url: https://x.example/sub\n    name: dup\n  - url: https://y.example/sub\n    name: dup\n",
    )
    .unwrap();
    assert!(matches!(
        resolve_source_ref(&paths, "dup"),
        Err(SubCmdError::AmbiguousName(_))
    ));
}

#[test]
fn set_source_renames_and_scales_every() {
    let (_dir, paths) = seed_two_sources("sub-set-rename");
    let outcome = set_source(&paths, "alpha", None, Some("renamed"), Some(180), true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    let sources = read_sources(&paths);
    assert_eq!(
        sources[0].get("name").and_then(|v| v.as_str()),
        Some("renamed")
    );
    assert_eq!(
        sources[0]
            .get("refresh_every_minutes")
            .and_then(serde_norway::Value::as_u64),
        Some(180)
    );
    // The second source is untouched.
    assert_eq!(
        sources[1].get("url").and_then(|v| v.as_str()),
        Some("https://b.example.com/sub")
    );
}

#[test]
fn set_source_rename_collision_excludes_self() {
    let (_dir, paths) = seed_two_sources("sub-set-taken");
    // Renaming a source to a name another source owns is rejected…
    assert!(matches!(
        set_source(
            &paths,
            "https://b.example.com/sub",
            None,
            Some("alpha"),
            None,
            true
        ),
        Err(SubCmdError::NameTaken(_))
    ));
    // …but re-asserting the source's own name is a no-change apply.
    let outcome = set_source(&paths, "alpha", None, Some("alpha"), None, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::NoChange);
}

#[test]
fn set_source_url_change_rejects_duplicates() {
    let (_dir, paths) = seed_two_sources("sub-set-dupurl");
    assert!(matches!(
        set_source(
            &paths,
            "alpha",
            Some("https://b.example.com/sub"),
            None,
            None,
            true
        ),
        Err(SubCmdError::AlreadyDeclared(_))
    ));
}

#[test]
fn set_source_file_every_rules_hold() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    let file = dir.as_path().join("feed.yaml");
    fs::write(&file, b"proxies: []\n").unwrap();
    add_source(&paths, file.to_str().unwrap(), None, None, true).unwrap();
    let file_url = format!("file://{}", std::fs::canonicalize(&file).unwrap().display());
    // Non-zero --every on the file source: usage error even in set.
    assert!(matches!(
        set_source(&paths, &file_url, None, None, Some(60), true),
        Err(SubCmdError::EveryOnFile)
    ));
    // Converting a URL source onto a (different) file re-pins it static.
    let other = dir.as_path().join("other.yaml");
    fs::write(&other, b"proxies: []\n").unwrap();
    let other_url = format!(
        "file://{}",
        std::fs::canonicalize(&other).unwrap().display()
    );
    add_source(&paths, "https://c.example.com/sub", None, Some(720), true).unwrap();
    let outcome = set_source(
        &paths,
        "https://c.example.com/sub",
        Some(other.to_str().unwrap()),
        None,
        None,
        true,
    )
    .unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    let sources = read_sources(&paths);
    let converted = sources
        .iter()
        .find(|s| s.get("url").and_then(|v| v.as_str()) == Some(other_url.as_str()))
        .unwrap()
        .clone();
    assert_eq!(
        converted
            .get("refresh_every_minutes")
            .and_then(serde_norway::Value::as_u64),
        Some(0)
    );
}

#[test]
fn set_source_dry_run_writes_nothing() {
    let (_dir, paths) = seed_two_sources("sub-set-dry");
    let before = fs::read(paths.config.join("config.yaml")).unwrap();
    let outcome = set_source(&paths, "alpha", None, Some("newname"), None, false).unwrap();
    assert_eq!(outcome, SubWriteOutcome::DryRun);
    let after = fs::read(paths.config.join("config.yaml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn remove_source_purge_deletes_the_cached_body() {
    let dir = temp_root(FIXTURE_TAG);
    let paths = seed_config_at_roots(dir.as_path());
    add_source(
        &paths,
        "https://a.example.com/sub",
        Some("alpha"),
        None,
        true,
    )
    .unwrap();
    // Forge the daemon-side cache entry the way the backend names it.
    let id = caly_subscription::subscription_id_for_url("https://a.example.com/sub");
    let hex = crate::client::hex(id.into_bytes());
    let cache_dir = paths.state.join("subscriptions");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join(&hex), b"cached body").unwrap();
    fs::write(cache_dir.join(format!("{hex}.tmp")), b"partial").unwrap();
    // Without --purge the cache survives the removal.
    let outcome = remove_source(&paths, "alpha", false, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    assert!(cache_dir.join(&hex).exists(), "default keeps the cache");
    // Re-add (name included) and remove with --purge: the cache files go.
    add_source(
        &paths,
        "https://a.example.com/sub",
        Some("alpha"),
        None,
        true,
    )
    .unwrap();
    let outcome = remove_source(&paths, "alpha", true, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
    assert!(!cache_dir.join(&hex).exists(), "--purge deletes the body");
    assert!(!cache_dir.join(format!("{hex}.tmp")).exists());
    // Purging a source with no cache at all is still a success.
    add_source(&paths, "https://nocache.example.com/sub", None, None, true).unwrap();
    let outcome = remove_source(&paths, "https://nocache.example.com/sub", true, true).unwrap();
    assert_eq!(outcome, SubWriteOutcome::Applied);
}
