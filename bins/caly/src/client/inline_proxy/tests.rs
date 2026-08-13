//! Tests for `client/inline_proxy.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::test_helpers::{hermetic_paths, temp_root};
use std::fs;

#[test]
fn validate_uri_rejects_unknown_schemes() {
    assert!(matches!(
        validate_uri(""),
        Err(InlineProxyError::InvalidUri(_))
    ));
    assert!(matches!(
        validate_uri("file:///etc/passwd"),
        Err(InlineProxyError::InvalidUri(_))
    ));
    assert!(validate_uri("vmess://abc").is_ok());
    assert!(validate_uri("ss://YWVz").is_ok());
}

#[test]
fn add_proxy_dry_run_against_existing_uri_reports_already_declared() {
    // Round 24 (debug): the `add_proxy` path checks
    // `path.exists()` *before* the `apply` flag, so
    // a dry-run against an already-declared URI
    // surfaces `AlreadyDeclared` (the same shape
    // the apply path would surface) instead of a
    // fabricated `DryRun { id }`. This is the
    // right contract: the writer's path-safety +
    // existence checks must fire on every entry,
    // regardless of the dispatch's intent, so the
    // operator's `set proxy add` against an
    // existing URI always errors (not silently
    // reports "would be added").
    let dir = temp_root("drydup");
    let paths = hermetic_paths(dir.as_path());
    add_proxy(&paths, "ss://x", None, true).unwrap();
    let result = add_proxy(&paths, "ss://x", None, false);
    assert!(matches!(result, Err(InlineProxyError::AlreadyDeclared(_))));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn import_proxy_fails_fast_on_first_invalid_line() {
    // Round 24 (debug): `import_proxy` calls
    // `validate_uri(line)?` on every non-comment
    // line, so the first invalid URI short-circuits
    // the whole import with `InvalidUri`. That's
    // the right contract (a hard fail on a
    // non-proxy line is better than a partial
    // write — silent partial writes would be
    // noticed only after `config apply`, by which
    // time the operator doesn't remember which
    // lines of the import file were which). Lock
    // the current fail-fast contract: the import
    // stops at the first invalid line and returns
    // the `InvalidUri` error verbatim.
    let dir = temp_root("invalid");
    let paths = hermetic_paths(dir.as_path());
    let list_path = dir.join("list.txt");
    std::fs::write(&list_path, "ss://a\nbogus://x\nvmess://b\n").unwrap();
    let result = import_proxy(&paths, &list_path, true);
    assert!(
        matches!(result, Err(InlineProxyError::InvalidUri(_))),
        "expected InvalidUri on the second non-comment line, got {result:?}"
    );
    // The import is now transactional-by-construction
    // (#46): validation runs in a pure first pass, so
    // a failing line aborts the run BEFORE any entry
    // reaches disk — the hermetic state root stays
    // empty, and the caller can fix the file and
    // re-run without cleaning up partial writes.
    let list = list_proxies(&paths);
    assert!(
        list.is_empty(),
        "a failed import must leave no partial entries on disk"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn add_proxy_dry_run_does_not_write() {
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    let outcome = add_proxy(&paths, "ss://YWVz", None, false).unwrap();
    assert!(matches!(outcome, InlineProxyOutcome::DryRun { .. }));
    let list = list_proxies(&paths);
    assert!(list.is_empty());
}

#[test]
#[allow(clippy::panic, clippy::match_wildcard_for_single_variants)] // test assertion: a non-`Applied` outcome
// is a contract violation we want to surface
// loud, not silently absorb. The wildcard arm
// and the `panic!` are deliberate.
fn add_proxy_apply_persists_an_entry() {
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    let outcome = add_proxy(&paths, "ss://YWVz", Some("auto"), true).unwrap();
    let id = match &outcome {
        InlineProxyOutcome::Applied { id } => id.clone(),
        other => panic!("expected Applied, got {other:?}"),
    };
    let list = list_proxies(&paths);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, id);
    assert_eq!(list[0].1, "ss://YWVz");
    // Re-adding the same URI is a no-op error.
    let result = add_proxy(&paths, "ss://YWVz", None, true);
    assert!(matches!(result, Err(InlineProxyError::AlreadyDeclared(_))));
}

#[test]
#[allow(clippy::panic, clippy::match_wildcard_for_single_variants)] // test assertion: a non-`Applied` outcome
// is a contract violation we want to surface
// loud, not silently absorb.
fn remove_proxy_filters_a_matching_id() {
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    let id = match add_proxy(&paths, "ss://a", None, true).unwrap() {
        InlineProxyOutcome::Applied { id } => id,
        other => panic!("expected Applied, got {other:?}"),
    };
    add_proxy(&paths, "ss://b", None, true).unwrap();
    let outcome = remove_proxy(&paths, &id, true).unwrap();
    assert!(matches!(outcome, InlineProxyOutcome::Applied { .. }));
    let list = list_proxies(&paths);
    assert_eq!(list.len(), 1);
}

#[test]
fn remove_proxy_rejects_unknown_id() {
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    let result = remove_proxy(&paths, "deadbeef", true);
    assert!(matches!(result, Err(InlineProxyError::NotDeclared(_))));
}

#[test]
fn remove_proxy_accepts_a_raw_uri() {
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    add_proxy(&paths, "vmess://abc", None, true).unwrap();
    let outcome = remove_proxy(&paths, "vmess://abc", true).unwrap();
    assert!(matches!(outcome, InlineProxyOutcome::Applied { .. }));
}

#[test]
fn add_proxy_trims_uri_before_hashing() {
    // Round 27 (debug): the pre-Round 27 shape
    // computed the id from the *raw* user
    // input but stored the *trimmed* URI in
    // the file body. A whitespace-padded URI
    // (`" vmess://abc"`) and the trimmed
    // version (`"vmess://abc"`) produced two
    // different on-disk files with identical
    // body content — operator confusion
    // (the file list showed two entries,
    // both with the same URI) and wasted
    // storage. The post-Round 27 contract:
    // the id is derived from the trimmed
    // URI, so the two strings map to the
    // same id and the same on-disk file.
    // The duplicate check (`AlreadyDeclared`)
    // is the operator-visible contract: a
    // second `add` with a whitespace-padded
    // URI is rejected as a duplicate of the
    // already-stored trimmed entry.
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    // First add: the trimmed URI.
    add_proxy(&paths, "vmess://abc", None, true).unwrap();
    // Second add: a whitespace-padded variant
    // of the same URI. Pre-Round 27 this
    // would have created a second on-disk
    // file with a different id; post-Round 27
    // the trim normalizes the input and the
    // duplicate check fires.
    let result = add_proxy(&paths, "  vmess://abc  ", None, true);
    assert!(
        matches!(result, Err(InlineProxyError::AlreadyDeclared(_))),
        "expected AlreadyDeclared for whitespace-padded duplicate, got {result:?}"
    );
    // The on-disk file count is exactly one
    // (the trim-normalized entry), not two.
    let list = list_proxies(&paths);
    assert_eq!(list.len(), 1, "pre-Round 27 would have created 2 entries");
    assert_eq!(list[0].1, "vmess://abc");
}

#[test]
fn import_proxy_with_invalid_line_writes_nothing() {
    // Regression (#46): the pre-fix import wrote
    // entries as it validated them, so a file with
    // a valid line BEFORE an invalid one left a
    // partial import on disk. The two-pass shape
    // validates the whole file first, so the error
    // path touches zero bytes and reports the
    // offending line number.
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    let list_path = dir.join("list.txt");
    std::fs::write(&list_path, "ss://a\ngarbage-not-a-uri\nvmess://b\n").unwrap();
    let result = import_proxy(&paths, &list_path, true);
    match result {
        Err(InlineProxyError::InvalidUri(reason)) => {
            assert!(
                reason.contains("line 2"),
                "error must name the failing line, got: {reason}"
            );
        }
        other => panic!("expected InvalidUri with line number, got {other:?}"),
    }
    assert!(
        list_proxies(&paths).is_empty(),
        "no entry may be written when validation fails (#46)"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn edit_buffer_group_line_is_functional() {
    // Regression (#47): the `# group (optional):`
    // line in the edit buffer was an inert comment
    // — edits never applied. The parser must pick
    // the edited value up (and an empty value must
    // clear the group).
    let edited = "# Edit the URI line below; one URI per file.\n\
                  # group (optional): hk\n\
                  # id: 0123456789abcdef\n\
                  vmess://abc\n";
    let mut group = Some("old".to_owned());
    for line in edited.lines() {
        if let Some(rest) = line.strip_prefix(GROUP_COMMENT_PREFIX) {
            let value = rest.trim();
            group = (!value.is_empty()).then(|| value.to_owned());
        }
    }
    assert_eq!(group.as_deref(), Some("hk"));

    let cleared = "# group (optional): \nvmess://abc\n";
    let mut group = Some("old".to_owned());
    for line in cleared.lines() {
        if let Some(rest) = line.strip_prefix(GROUP_COMMENT_PREFIX) {
            let value = rest.trim();
            group = (!value.is_empty()).then(|| value.to_owned());
        }
    }
    assert_eq!(group, None, "an empty group line clears the group");
}

#[test]
fn import_proxy_bulk_reads_a_file() {
    let dir = temp_root("__FUNC__");
    let paths = hermetic_paths(dir.as_path());
    let list_path = dir.join("list.txt");
    std::fs::write(&list_path, "# comment\n\nss://a\nvmess://b\nss://a\n").unwrap();
    let outcome = import_proxy(&paths, &list_path, true).unwrap();
    assert_eq!(outcome, ImportOutcome::Applied { count: 2 });
    let list = list_proxies(&paths);
    assert_eq!(list.len(), 2, "duplicate `ss://a` must be skipped");
}
