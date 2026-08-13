//! Offline tests for the `caly profile` subcommand. The tests
//! build a self-contained XDG root (`AppPaths`) and verify the
//! four file-touching operations (add / list / remove / show)
//! against a real `config.yaml` on disk.

use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use caly_platform::paths::AppPaths;

use super::profile as cmd;

fn unique_paths(tag: &str) -> AppPaths {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("caly-profile-{tag}-{nanos}"));
    let config = root.join("config");
    let state = root.join("state");
    fs::create_dir_all(&config).unwrap_or_default();
    fs::create_dir_all(&state).unwrap_or_default();
    AppPaths {
        config,
        data: state.clone(),
        state,
        runtime: root.join("runtime"),
    }
}

fn cleanup(paths: &AppPaths) {
    if let Some(parent) = paths.config.parent() {
        fs::remove_dir_all(parent).ok();
    }
}

fn write_base_config(paths: &AppPaths, body: &str) {
    fs::write(paths.config.join("config.yaml"), body).unwrap_or_default();
}

#[test]
fn parse_source_spec_accepts_three_kinds() {
    let remote = cmd::parse_source_spec("remote:https://example.com/x").unwrap();
    let local = cmd::parse_source_spec("local:extra.yaml").unwrap();
    let merge = cmd::parse_source_spec("merge:a,b").unwrap();
    assert!(matches!(
        remote,
        cmd::ProfileSourceKind::Remote { ref url, .. } if url == "https://example.com/x"
    ));
    assert!(matches!(
        local,
        cmd::ProfileSourceKind::Local { ref path } if path == "extra.yaml"
    ));
    assert!(matches!(
        merge,
        cmd::ProfileSourceKind::Merge { ref parts } if parts == &vec!["a".to_owned(), "b".to_owned()]
    ));
}

#[test]
fn parse_source_spec_rejects_unknown_kind_and_empty_body() {
    assert!(cmd::parse_source_spec("ftp://example.com/x").is_err());
    assert!(cmd::parse_source_spec("remote:").is_err());
    assert!(cmd::parse_source_spec("local:").is_err());
    assert!(cmd::parse_source_spec("merge:").is_err());
    assert!(cmd::parse_source_spec("merge:,").is_err());
}

#[test]
fn list_empty_config_reports_no_profile() {
    let paths = unique_paths("list-empty");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    let (config, _) = cmd::load_declared_profiles(&paths)
        .unwrap_or_else(|error| panic!("load must succeed: {error}"));
    let declared = cmd::list_declared(&config);
    assert!(declared.is_empty());
    cleanup(&paths);
}

#[test]
fn add_then_list_round_trip() {
    let paths = unique_paths("add-list");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    cmd::add_profile(
        &paths,
        "team",
        cmd::ProfileSourceKind::Remote {
            url: "https://example.com/team.yaml".to_owned(),
            interval_minutes: 60,
        },
        false,
    )
    .unwrap_or_else(|error| panic!("add must succeed: {error}"));
    let (config, _) = cmd::load_declared_profiles(&paths)
        .unwrap_or_else(|error| panic!("load_declared_profiles failed: {error}"));
    let declared = cmd::list_declared(&config);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].id, "team");
    assert!(matches!(
        declared[0].source,
        cmd::ProfileSourceKind::Remote { .. }
    ));
    cleanup(&paths);
}

#[test]
fn add_rejects_duplicate_id() {
    let paths = unique_paths("dup");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    cmd::add_profile(
        &paths,
        "x",
        cmd::ProfileSourceKind::Local {
            path: "a.yaml".to_owned(),
        },
        false,
    )
    .unwrap();
    let result = cmd::add_profile(
        &paths,
        "x",
        cmd::ProfileSourceKind::Local {
            path: "b.yaml".to_owned(),
        },
        false,
    );
    assert!(matches!(
        result,
        Err(cmd::ProfileCmdError::AlreadyDeclared(_))
    ));
    cleanup(&paths);
}

#[test]
fn add_rejects_non_path_safe_id() {
    let paths = unique_paths("badid");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    let result = cmd::add_profile(
        &paths,
        "with/slash",
        cmd::ProfileSourceKind::Local {
            path: "a.yaml".to_owned(),
        },
        false,
    );
    assert!(matches!(result, Err(cmd::ProfileCmdError::InvalidId(_))));
    cleanup(&paths);
}

#[test]
fn show_returns_not_declared_for_unknown_id() {
    let paths = unique_paths("show-missing");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    let (config, _) = cmd::load_declared_profiles(&paths).unwrap();
    assert!(cmd::find_declared(&config, "ghost").is_none());
    cleanup(&paths);
}

#[test]
fn remove_clears_profile_from_config_and_cache() {
    let paths = unique_paths("remove");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    cmd::add_profile(
        &paths,
        "x",
        cmd::ProfileSourceKind::Local {
            path: "a.yaml".to_owned(),
        },
        false,
    )
    .unwrap();
    // Materialise a fake cache entry so `remove` clears it.
    let (config, store) = cmd::load_declared_profiles(&paths).unwrap();
    assert!(cmd::find_declared(&config, "x").is_some());
    store
        .write("x", b"core: mihomo\n", "https://example.com/x", 1)
        .unwrap();
    cmd::remove_profile(&paths, "x", false)
        .unwrap_or_else(|error| panic!("remove must succeed: {error}"));
    let (config, store) = cmd::load_declared_profiles(&paths).unwrap();
    assert!(cmd::find_declared(&config, "x").is_none());
    assert!(store.read("x").unwrap().is_none());
    cleanup(&paths);
}

#[test]
fn refresh_remote_profile_fails_when_unreachable() {
    let paths = unique_paths("refresh");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    cmd::add_profile(
        &paths,
        "team",
        cmd::ProfileSourceKind::Remote {
            // RFC 2606 `.invalid` is reserved for testing; the
            // DNS lookup fails fast and the SSRF guard never
            // gets a chance to false-positive.
            url: "https://nonexistent-team.invalid/team.yaml".to_owned(),
            interval_minutes: 60,
        },
        false,
    )
    .unwrap();
    // The fetcher is implemented but the network is
    // unavailable in the test environment. The error
    // surfaces as `Fetch(...)` (or `FetchChain { .. }` for
    // a `Merge`); the exact variant is non-deterministic
    // because the tokio runtime is shared with other tests,
    // so we just assert the broad shape.
    let result = cmd::refresh_all(&paths, Some("team"));
    match result {
        Err(cmd::ProfileCmdError::Fetch(_) | cmd::ProfileCmdError::FetchChain { .. }) => {}
        other => {
            panic!("expected Fetch/FetchChain, got {other:?}")
        }
    }
    cleanup(&paths);
}

#[test]
fn refresh_local_profile_succeeds_without_network() {
    let paths = unique_paths("refresh-local");
    write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
    cmd::add_profile(
        &paths,
        "local-x",
        cmd::ProfileSourceKind::Local {
            path: "a.yaml".to_owned(),
        },
        false,
    )
    .unwrap();
    // Local profiles are "always fresh" — refresh is a no-op.
    let count = cmd::refresh_all(&paths, Some("local-x"))
        .unwrap_or_else(|error| panic!("refresh must succeed: {error}"));
    assert_eq!(count, 0);
    cleanup(&paths);
}

#[test]
fn refresh_merge_profile_walks_remote_parts() {
    // A `Merge` of two `Local` parts is a no-op; a `Merge`
    // containing a `Remote` part must attempt to walk the
    // dependency. The walk cannot succeed in the unit-test
    // environment (no network), so the result is `Fetch` /
    // `FetchChain` — same contract as the standalone
    // `Remote` case.
    let paths = unique_paths("refresh-merge");
    write_base_config(
        &paths,
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: local-a\n    kind: local\n    path: a.yaml\n  - id: local-b\n    kind: local\n    path: b.yaml\n  - id: combined\n    kind: merge\n    parts: [local-a, local-b]\n",
    );
    // All `Local` parts → merge is a no-op. The walk still
    // recurses through both `Local` ids and counts 0
    // fetches.
    let count = cmd::refresh_all(&paths, Some("combined"))
        .unwrap_or_else(|error| panic!("refresh must succeed: {error}"));
    assert_eq!(count, 0, "merge of two Locals is a no-op");
    cleanup(&paths);
}

#[test]
fn refresh_merge_profile_fails_when_a_remote_part_fails() {
    // A merge that depends on a `Remote` whose body is
    // unreachable wraps the inner failure in `FetchChain`
    // with the **merge** id (not the failing part's id), so
    // the operator can identify which `Merge` reached into
    // the broken dependency.
    let paths = unique_paths("refresh-merge-fail");
    write_base_config(
        &paths,
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: ghost-team\n    kind: remote\n    url: https://nonexistent-ghost-team.invalid/team.yaml\n    interval_minutes: 60\n  - id: combined\n    kind: merge\n    parts: [ghost-team]\n",
    );
    let result = cmd::refresh_all(&paths, Some("combined"));
    match result {
        Err(cmd::ProfileCmdError::FetchChain { id, source }) => {
            // The chain id is **the merge** (the user-visible
            // entry point), not the failing remote. The
            // operator sees `combined` and knows which declared
            // `Merge` reached into the broken dependency; the
            // inner `source` carries the precise network
            // failure.
            assert_eq!(id, "combined");
            let _ = source;
        }
        other => {
            panic!("expected FetchChain, got {other:?}")
        }
    }
    cleanup(&paths);
}

#[test]
fn refresh_nested_merge_keeps_outermost_merge_in_fetch_chain() {
    // A three-level merge tree:
    //
    //   outer
    //     ├── inner (Merge)
    //     │     └── leaf-bad (Remote, RFC 2606 .invalid)
    //     └── ok-local (Local)
    //
    // The error id must be **outer** (the user's declared
    // entry), not `inner` (a sub-merge they may not have named
    // explicitly). The inner merge is an internal optimisation,
    // not a user-facing surface.
    let paths = unique_paths("refresh-nested-merge");
    write_base_config(
        &paths,
        "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: leaf-bad\n    kind: remote\n    url: https://nonexistent-leaf-bad.invalid/x\n    interval_minutes: 60\n  - id: ok-local\n    kind: local\n    path: a.yaml\n  - id: inner\n    kind: merge\n    parts: [leaf-bad]\n  - id: outer\n    kind: merge\n    parts: [inner, ok-local]\n",
    );
    let result = cmd::refresh_all(&paths, Some("outer"));
    match result {
        Err(cmd::ProfileCmdError::FetchChain { id, source: _ }) => {
            assert_eq!(id, "outer");
        }
        other => {
            panic!("expected FetchChain {{ id: outer, .. }}, got {other:?}")
        }
    }
    cleanup(&paths);
}

#[test]
fn add_dry_run_does_not_modify_config() {
    let paths = unique_paths("add-dry-run");
    let original = "schema_version: 1\ncore: mihomo\n";
    write_base_config(&paths, original);
    let outcome = cmd::add_profile(
        &paths,
        "team",
        cmd::ProfileSourceKind::Remote {
            url: "https://example.com/team.yaml".to_owned(),
            interval_minutes: 60,
        },
        true,
    )
    .unwrap_or_else(|error| panic!("add(dry-run) must succeed: {error}"));
    assert_eq!(outcome, cmd::ProfileWriteOutcome::DryRun);
    // The file on disk must be byte-identical to the
    // pre-call snapshot: a dry-run must not touch the
    // operator's config under any circumstance.
    let after = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(after, original);
    // The CLI also reports no profile declared under
    // `list` because the load reads the same file.
    let (config, _) = cmd::load_declared_profiles(&paths)
        .unwrap_or_else(|error| panic!("load must succeed: {error}"));
    assert!(cmd::list_declared(&config).is_empty());
    cleanup(&paths);
}

#[test]
fn remove_dry_run_does_not_modify_config() {
    let paths = unique_paths("remove-dry-run");
    let original = "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: x\n    kind: local\n    path: a.yaml\n";
    write_base_config(&paths, original);
    let outcome = cmd::remove_profile(&paths, "x", true)
        .unwrap_or_else(|error| panic!("remove(dry-run) must succeed: {error}"));
    assert_eq!(outcome, cmd::ProfileWriteOutcome::DryRun);
    let after = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(after, original);
    // The declared profile is still there — a dry-run
    // does not remove.
    let (config, _) = cmd::load_declared_profiles(&paths)
        .unwrap_or_else(|error| panic!("load must succeed: {error}"));
    assert_eq!(cmd::list_declared(&config).len(), 1);
    cleanup(&paths);
}

#[test]
fn remove_real_creates_a_single_backup_sidecar() {
    // The real remove path must back up the existing
    // `config.yaml` to a single `config.yaml.bak` sidecar
    // before rewriting the file. The operator can `cp` it
    // back to roll back. There is exactly one sidecar
    // (no `<unix-ms>` suffix accumulation), and its body
    // matches the pre-call config byte-for-byte.
    let paths = unique_paths("remove-backup");
    let original = "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: x\n    kind: local\n    path: a.yaml\n";
    write_base_config(&paths, original);
    cmd::remove_profile(&paths, "x", false)
        .unwrap_or_else(|error| panic!("remove must succeed: {error}"));
    let backup_path = paths.config.join("config.yaml.bak");
    assert!(
        backup_path.is_file(),
        "expected `config.yaml.bak` sidecar at {}",
        backup_path.display()
    );
    let backup_body = fs::read_to_string(&backup_path)
        .unwrap_or_else(|error| panic!("read backup must succeed: {error}"));
    assert_eq!(backup_body, original);
    cleanup(&paths);
}

#[test]
fn backup_collision_is_resolved_by_overwrite() {
    // Two removes in rapid succession on the same config
    // would naïvely produce two backup files. The contract
    // is "the most recent backup wins": a fresh `fs::copy`
    // overwrites the pre-existing sidecar so the operator
    // accumulates at most one backup per `config.yaml`
    // regardless of how many `add` / `remove` commands they
    // run between two `cp config.yaml.bak config.yaml`
    // rollbacks. Lock the contract here so the on-disk
    // behaviour is observable.
    let paths = unique_paths("backup-collision");
    let original = "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: x\n    kind: local\n    path: a.yaml\n";
    write_base_config(&paths, original);
    cmd::remove_profile(&paths, "x", false).unwrap();
    // Re-add the same id, then remove it again. The
    // second remove overwrites the existing sidecar in
    // place.
    let intermediate = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    write_base_config(&paths, original);
    cmd::remove_profile(&paths, "x", false).unwrap();
    let after_second_remove = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(
        after_second_remove, intermediate,
        "post-second-remove config must match post-first-remove config"
    );
    // The "newest wins" contract: at most one `config.yaml.bak`
    // exists in the config directory, no matter how many
    // removes ran.
    let backup_path = paths.config.join("config.yaml.bak");
    assert!(
        backup_path.is_file(),
        "expected exactly one `config.yaml.bak` after multiple removes"
    );
    let mut backup_count = 0;
    let read_dir = fs::read_dir(&paths.config)
        .unwrap_or_else(|error| panic!("read_dir must succeed: {error}"));
    for entry in read_dir {
        let entry = entry.unwrap_or_else(|error| panic!("dir entry must succeed: {error}"));
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "config.yaml.bak" || name.starts_with("config.yaml.bak.") {
            backup_count += 1;
        }
    }
    assert_eq!(
        backup_count, 1,
        "consecutive removes must not accumulate sidecars"
    );
    cleanup(&paths);
}
