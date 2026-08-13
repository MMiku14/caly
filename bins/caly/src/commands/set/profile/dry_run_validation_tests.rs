//! Tests for `commands/set/profile.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 19: every `set profile …` dry-run must
//! surface the same validation errors the apply
//! path does. Before this change, `edit` /
//! `enable` / `disable` short-circuited with a
//! fabricated `ok: would be …` envelope before
//! the writer ever saw the id, so a dry-run
//! against a non-existent id reported success.
//!
//! These tests exercise the underlying writers
//! (`client::profile::edit_profile` /
//! `set_profile_enabled`) with a hermetic
//! `AppPaths`, so the dry-run return value is
//! the same one the dispatch now sees.
//!
//! Round 22: the dispatch's `set_enabled_dispatch`
//! shim folds the writer's `set_profile_enabled`
//! path into the shared `run_writer` envelope. The
//! dry-run contract is preserved (the writer's
//! `find_declared` check fires through the shim's
//! pre-flight), so the existing regression tests
//! still cover the contract.

use crate::client::profile as cmd;
use crate::test_helpers::{hermetic_paths, temp_root};
use caly_platform::paths::AppPaths;
use std::fs;

fn seed_config(paths: &AppPaths) {
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(
        paths.config.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nprofiles: []\n",
    )
    .unwrap();
}

#[test]
fn edit_dry_run_against_unknown_id_surfaces_not_declared() {
    let dir = temp_root("edit-dry");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    let result = cmd::edit_profile(&paths, "missing-id", false);
    assert!(
        matches!(result, Err(cmd::ProfileCmdError::NotDeclared(_))),
        "expected NotDeclared, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enable_dry_run_against_unknown_id_surfaces_not_declared() {
    let dir = temp_root("enable-dry");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    // Round 30: the writer now takes an
    // explicit `apply: bool` parameter. The
    // `apply: true` form is the post-Round 30
    // equivalent of the pre-Round 30 call
    // (the writer was apply-only; the dry-run
    // path lived in the dispatch). The
    // unknown-id path is still `NotDeclared`
    // because the writer's `find_declared` scan
    // runs before the dry-run short-circuit.
    let result = cmd::set_profile_enabled(&paths, "missing-id", true, true);
    assert!(
        matches!(result, Err(cmd::ProfileCmdError::NotDeclared(_))),
        "expected NotDeclared, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disable_dry_run_against_unknown_id_surfaces_not_declared() {
    let dir = temp_root("disable-dry");
    let paths = hermetic_paths(&dir);
    seed_config(&paths);
    // Round 30: pre-Round 30, the 3-arg call
    // (no `apply`) hit the dispatch's dry-run
    // path, which duplicated the writer's
    // existence scan. Post-Round 30, the
    // 4-arg call with `apply: false` goes
    // straight through the writer's dry-run
    // branch, which runs the same
    // `find_declared` scan as the apply
    // path. The error shape is identical
    // (`NotDeclared`), so the contract is
    // uniform across both paths.
    let result = cmd::set_profile_enabled(&paths, "missing-id", false, false);
    assert!(
        matches!(result, Err(cmd::ProfileCmdError::NotDeclared(_))),
        "expected NotDeclared, got {result:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}
