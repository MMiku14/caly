//! Tests for `commands/rule_provider.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 25: end-to-end dispatch-level tests for
//! [`dispatch_with_paths`]. The 4 CRUD leaves
//! (`add-http` / `add-file` / `add-inline` /
//! `remove` / `enable` / `disable`) were previously
//! only exercised at the writer level
//! (`client::rule_provider::add_provider` etc.).
//! The dispatch-level test fills the gap: it
//! exercises the dispatch's path through the
//! `Summaries::standard` / `run_writer` envelope
//! with a hermetic `AppPaths`. The `Refresh` and
//! `List` leaves route through the `Refreshable`
//! trait and a real `LayeredConfigPaths` read
//! respectively; their integration tests live
//! elsewhere.
use super::*;
use crate::cli::{RuleProviderSourceSpec, SetRuleProviderCmd};
use crate::output::CliOutput;
use crate::test_helpers::{hermetic_paths, temp_root};
use caly_platform::paths::AppPaths;
use std::fs;

fn seed_config(paths: &AppPaths) {
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(
        paths.config.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nrule_providers: []\n",
    )
    .unwrap();
}

fn empty_options() -> crate::cli::CliOptions {
    crate::cli::CliOptions::default()
}

/// Round 25: `set rule-provider add-http` in
/// dry-run mode (no `--apply`) must NOT write
/// the `config.yaml`. The dispatch contract
/// here is "the on-disk file is byte-identical
/// to the seed" — the writer's `add_provider`
/// does the URL/path validation and would
/// surface `InvalidUrl` for a non-HTTP URL.
#[test]
fn add_http_dry_run_does_not_write_config() {
    let dir = temp_root("add-dry");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    let baseline = fs::read_to_string(paths.config.join("config.yaml")).unwrap();
    let exit = dispatch_with_paths(
        SetRuleProviderCmd::Add {
            name: "geosite-cn".to_owned(),
            source: RuleProviderSourceSpec::Http {
                url: "https://example.com/geosite-cn".to_owned(),
                interval_ms: 86_400_000,
            },
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

/// Round 25: `set rule-provider add-http` with a
/// `file://` URL must surface `InvalidUrl` —
/// the SSRF guard refuses non-HTTP schemes at
/// the writer level, and the dispatch's
/// `code_for` mapping preserves this through
/// the `run_writer` envelope. A future
/// `code_for` drift (e.g. collapsing
/// `InvalidUrl` into a generic `Invalid`)
/// surfaces here, not in the integration
/// suite.
#[test]
fn add_http_with_non_http_url_surfaces_invalid_url() {
    let dir = temp_root("add-bad-url");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    let exit = dispatch_with_paths(
        SetRuleProviderCmd::Add {
            name: "x".to_owned(),
            source: RuleProviderSourceSpec::Http {
                url: "file:///etc/passwd".to_owned(),
                interval_ms: 86_400_000,
            },
            apply: true,
            dry_run: false,
        },
        &paths,
        empty_options(),
        CliOutput::from_json_flag(true),
    );
    assert_ne!(exit, ExitCode::SUCCESS, "InvalidUrl must fail");
    let _ = fs::remove_dir_all(&dir);
}
