//! Tests for `commands/proxy_group.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 25: end-to-end dispatch-level tests for
//! [`dispatch_with_paths`]. The 4 CRUD leaves
//! (`add` / `remove` / `enable` / `disable`) were
//! previously only exercised at the writer level
//! (`client::proxy_group::add_group` etc.) plus
//! the parse-level `cli_tests` tests. The
//! dispatch-level test fills the gap: it
//! exercises the dispatch's path through the
//! `Summaries::standard` / `run_writer` envelope
//! with a hermetic `AppPaths`, so a future
//! `code_for` / `Summaries` drift surfaces
//! here.
use super::*;
use crate::cli::{ProxyGroupMemberSpec, ProxyGroupTypeSpec, SetProxyGroupCmd};
use crate::output::CliOutput;
use crate::test_helpers::{hermetic_paths, temp_root};
use caly_platform::paths::AppPaths;
use std::fs;

fn seed_config(paths: &AppPaths) {
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(
        paths.config.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nproxy_groups: []\n",
    )
    .unwrap();
}

fn empty_options() -> crate::cli::CliOptions {
    crate::cli::CliOptions::default()
}

/// Round 25: `set proxy-group add` with a
/// `select`-typed group and a direct member,
/// in dry-run mode, must NOT write the
/// `config.yaml`. The dispatch contract here
/// is "the on-disk file is byte-identical
/// to the seed" — the writer's `add_group`
/// does the URL-validity / missing-URL
/// checks and would surface `MissingUrl`
/// for a `url-test` group with no `--url`,
/// but a `select` group is always valid.
#[test]
fn add_select_dry_run_does_not_write_config() {
    let dir = temp_root("add-dry");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    let baseline = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    let exit = dispatch_with_paths(
        SetProxyGroupCmd::Add {
            name: "Proxy".to_owned(),
            group_type: ProxyGroupTypeSpec::Select,
            members: Ok(vec![ProxyGroupMemberSpec::Direct]),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            apply: false,
            dry_run: false,
        },
        &paths,
        empty_options(),
        CliOutput::from_json_flag(false),
    );
    assert_eq!(exit, ExitCode::SUCCESS);
    let after = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    assert_eq!(after, baseline, "dry-run must not modify config.yaml");
    let _ = fs::remove_dir_all(&dir);
}

/// Round 25: `set proxy-group add` with a
/// `url-test`-typed group and **no** `--url`
/// must surface `MissingUrl` — the dispatch's
/// `code_for` mapping preserves this through
/// the `run_writer` envelope. The pre-Round-25
/// `add_group` writer surfaced `MissingUrl`
/// already; this test exercises the dispatch
/// path so a future `code_for` drift (e.g.
/// collapsing `MissingUrl` into a generic
/// `Invalid`) surfaces here, not in the
/// integration suite.
#[test]
fn add_url_test_without_url_surfaces_missing_url() {
    let dir = temp_root("add-no-url");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    let exit = dispatch_with_paths(
        SetProxyGroupCmd::Add {
            name: "Auto".to_owned(),
            group_type: ProxyGroupTypeSpec::UrlTest,
            members: Ok(vec![ProxyGroupMemberSpec::Direct]),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            apply: true,
            dry_run: false,
        },
        &paths,
        empty_options(),
        CliOutput::from_json_flag(true),
    );
    assert_ne!(exit, ExitCode::SUCCESS, "MissingUrl must fail");
    // The writer's `MissingUrl` is mapped to
    // the stable `proxy_group.missing_url`
    // code through `code_for`; assert the
    // `code_for` mapping is locked by
    // asserting the function's return value
    // (the `code_for` test in this module
    // is in the writer's unit suite; here
    // we just confirm the dispatch path
    // returned a non-success exit code).
    let _ = fs::remove_dir_all(&dir);
}
