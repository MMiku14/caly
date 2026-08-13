//! Tests for `commands/set/proxy.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::client::inline_proxy as cmd;
use crate::test_helpers::{hermetic_paths, temp_root};

/// Each `add` / `edit` / `remove` / `import` dry-run
/// must return the same JSON shape the operator's
/// `jq` pipeline expects (`ok: true` + the relevant
/// identifiers + `dry_run: true`). Round 18 replaced
/// the `planned_ok(": planned.")` envelope with this
/// contract; the regression catches any leaf that
/// drifts back to the old stub. Round 19: the test
/// also exercises `import` in the
/// `apply-with-no-new-entries` (`NoOp`) shape so
/// the envelope reports `dry_run:false` and
/// `count:0` instead of misleading the operator
/// with `dry_run:true` after an explicit `--apply`.
/// It also asserts dry-run is truly read-only at
/// the filesystem level (no `create_dir_all`
/// side-effect on the inline-proxies dir).
#[test]
#[allow(clippy::panic, clippy::match_wildcard_for_single_variants)] // test assertion: non-`Applied` is a
// contract violation we want to surface loud.
fn dry_run_envelopes_match_applied_envelopes() {
    // Round 26: the inline `temp_root` /
    // `hermetic_paths` pair folded into the
    // cross-module [`crate::test_helpers`]
    // helpers; the test setup is now 2 lines
    // instead of 14. The `tag = "proxy-dryrun"`
    // argument keeps the on-disk
    // `caly-proxy-dryrun-...` path shape so a CI
    // log-grep that scans for the old path still
    // works.
    let dir = temp_root("proxy-dryrun");
    let paths = hermetic_paths(&dir);
    let uri = "vmess://dryrun-test";

    // `add` (dry-run): must not write the proxy file
    // AND must not create the inline-proxies dir.
    // Round 19: pre-Round 19 the path lookup
    // unconditionally called `create_dir_all` on
    // the parent, so a fresh state root with no
    // `<state>/caly/inline-proxies/` would silently
    // grow one during a dry-run. The fix: use the
    // read-only path helper for the `path.exists()`
    // check.
    let inline_dir = paths.state.join("inline-proxies");
    assert!(
        !inline_dir.exists(),
        "inline-proxies must not pre-exist (test setup must be hermetic)"
    );
    let outcome = cmd::add_proxy(&paths, uri, Some("g"), false).unwrap();
    assert!(matches!(outcome, InlineProxyOutcome::DryRun { .. }));
    assert!(
        !inline_dir.exists(),
        "dry-run add must not create the inline-proxies directory"
    );
    assert!(
        cmd::list_proxies(&paths).is_empty(),
        "dry-run add must not persist any entry"
    );

    // `add` (apply): must write the proxy file and
    // create the inline-proxies dir as a side effect.
    let id = match cmd::add_proxy(&paths, uri, Some("g"), true).unwrap() {
        InlineProxyOutcome::Applied { id } => id,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert!(inline_dir.exists());
    assert_eq!(cmd::list_proxies(&paths).len(), 1);

    // `edit` (dry-run): the writer does the same
    // `NotDeclared` / `Store` validation as apply,
    // so a dry-run against a real id is `DryRun`.
    let outcome = cmd::edit_proxy(&paths, &id, false).unwrap();
    assert!(matches!(outcome, InlineProxyOutcome::DryRun { .. }));

    // `remove` (dry-run): the file is still present.
    let outcome = cmd::remove_proxy(&paths, &id, false).unwrap();
    assert!(matches!(outcome, InlineProxyOutcome::DryRun { .. }));
    assert_eq!(cmd::list_proxies(&paths).len(), 1);

    // `import` (dry-run) on a fresh path: zero writes.
    let list_path = dir.join("list.txt");
    std::fs::write(&list_path, "ss://a\nvmess://b\n").unwrap();
    let baseline = cmd::list_proxies(&paths).len();
    let outcome = cmd::import_proxy(&paths, &list_path, false).unwrap();
    assert_eq!(outcome, ImportOutcome::DryRun { count: 2 });
    assert_eq!(
        cmd::list_proxies(&paths).len(),
        baseline,
        "dry-run import must not write any proxy file"
    );

    // `import` (apply) on the same file: writes 2
    // new entries. `Applied(2)`, not `NoOp` — the
    // previous count-based short-circuit collapsed
    // this into `DryRun`, which the operator cannot
    // tell apart from a true dry-run. Round 19
    // makes the apply path honest about having
    // written.
    let outcome = cmd::import_proxy(&paths, &list_path, true).unwrap();
    assert_eq!(outcome, ImportOutcome::Applied { count: 2 });

    // `import` (apply) again on the same file: every
    // entry is already declared, so the apply path
    // is a no-op. The envelope must report `NoOp`
    // (not `Applied(0)` and not `DryRun`) so the
    // dispatch can map the operator's `--apply` to
    // `dry_run:false` in the JSON envelope. The
    // earlier `count == 0 → DryRun` short-circuit
    // would have lied here.
    let outcome = cmd::import_proxy(&paths, &list_path, true).unwrap();
    assert_eq!(outcome, ImportOutcome::NoOp);

    let _ = std::fs::remove_dir_all(&dir);
}

// Round 23: the dispatch's `extra_payload` closure
// projects the on-disk `id` and the imported `count`
// into the JSON envelope. These tests lock the
// shape so the envelope's `id` / `count` fields
// stay stable for the `jq` consumers.
#[test]
fn inline_proxy_payload_extracts_id() {
    let outcome = InlineProxyOutcome::Applied {
        id: "deadbeef".to_owned(),
    };
    let value = inline_proxy_payload(&outcome);
    assert_eq!(value, serde_json::json!({ "id": "deadbeef" }));
}

#[test]
fn inline_proxy_payload_works_for_dry_run() {
    let outcome = InlineProxyOutcome::DryRun {
        id: "cafebabe".to_owned(),
    };
    let value = inline_proxy_payload(&outcome);
    assert_eq!(value, serde_json::json!({ "id": "cafebabe" }));
}

#[test]
fn import_payload_extracts_count() {
    let outcome = ImportOutcome::Applied { count: 3 };
    let value = import_payload(&outcome);
    assert_eq!(value, serde_json::json!({ "count": 3 }));
}

#[test]
fn import_payload_collapses_noop_to_zero() {
    // `NoOp` is the idempotent success path: an
    // apply-mode call that found every entry
    // already declared. The envelope must report
    // `count: 0` so `jq` consumers see a real
    // number (not an absent key) and can branch on
    // `count == 0 && dry_run == false` for "import
    // was a no-op".
    let outcome = ImportOutcome::NoOp;
    let value = import_payload(&outcome);
    assert_eq!(value, serde_json::json!({ "count": 0 }));
}
