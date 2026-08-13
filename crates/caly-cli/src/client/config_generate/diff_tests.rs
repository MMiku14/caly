//! Tests for `client/config_generate.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::{DiffKind, naive_line_diff, render_structural_diff};

#[test]
fn identical_inputs_have_no_insert_or_delete_hunks() {
    let left = "schema_version: 1\ncore: mihomo\n";
    let right = "schema_version: 1\ncore: mihomo\n";
    let hunks = naive_line_diff(left, right);
    let non_equal = hunks.iter().filter(|h| h.kind != DiffKind::Equal).count();
    assert_eq!(non_equal, 0, "identical inputs must produce no diff hunks");
}

#[test]
fn insertion_produces_an_insert_hunk() {
    let left = "schema_version: 1\n";
    let right = "schema_version: 1\ncore: mihomo\n";
    let hunks = naive_line_diff(left, right);
    let inserts: Vec<_> = hunks
        .iter()
        .filter(|h| h.kind == DiffKind::Insert)
        .collect();
    assert_eq!(inserts.len(), 1);
    assert_eq!(inserts[0].right_text, "core: mihomo");
    assert_eq!(inserts[0].right_line, Some(2));
}

#[test]
fn deletion_produces_a_delete_hunk() {
    let left = "schema_version: 1\ncore: mihomo\n";
    let right = "schema_version: 1\n";
    let hunks = naive_line_diff(left, right);
    let deletes: Vec<_> = hunks
        .iter()
        .filter(|h| h.kind == DiffKind::Delete)
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0].left_text, "core: mihomo");
    assert_eq!(deletes[0].left_line, Some(2));
}

#[test]
fn replacement_emits_a_delete_then_an_insert() {
    let left = "core: mihomo\n";
    let right = "core: sing-box\n";
    let hunks = naive_line_diff(left, right);
    let inserts: Vec<_> = hunks
        .iter()
        .filter(|h| h.kind == DiffKind::Insert)
        .collect();
    let deletes: Vec<_> = hunks
        .iter()
        .filter(|h| h.kind == DiffKind::Delete)
        .collect();
    assert_eq!(inserts.len(), 1);
    assert_eq!(deletes.len(), 1);
    assert_eq!(inserts[0].right_text, "core: sing-box");
    assert_eq!(deletes[0].left_text, "core: mihomo");
}

// ── W3a structural diff (§5.5) ───────────────────────────────────

#[test]
fn identical_documents_report_no_changes() {
    let doc = "schema_version: 1\ncore: mihomo\n";
    let out = render_structural_diff(doc, doc);
    assert!(out.contains("(no changes)"), "out: {out}");
    assert!(out.contains("Summary: 0 modified, 0 added"), "out: {out}");
}

#[test]
fn scalar_change_renders_inline() {
    let left = "core: mihomo\n";
    let right = "core: sing-box\n";
    let out = render_structural_diff(left, right);
    assert!(out.contains("~ core:"), "out: {out}");
    assert!(out.contains("→"), "out: {out}");
    assert!(out.contains("Summary: 1 modified, 0 added"), "out: {out}");
}

#[test]
fn top_level_add_and_remove_keys() {
    let left = "schema_version: 1\ncore: mihomo\n";
    let right = "schema_version: 1\ncore: mihomo\nlog:\n  level: info\n";
    let out = render_structural_diff(left, right);
    assert!(out.contains("+ log:"), "out: {out}");
    assert!(out.contains("Summary: 0 modified, 1 added"), "out: {out}");

    let out = render_structural_diff(right, left);
    assert!(out.contains("- log:"), "out: {out}");
    assert!(
        out.contains("Summary: 0 modified, 0 added, 1 removed"),
        "out: {out}"
    );
}

#[test]
fn group_entries_align_by_name_and_diff_fields() {
    // The §5.5 shape: `~ 自动选择:` with per-field lines.
    let left = r"
proxy_groups:
  - name: 节点选择
    type: select
    members: [hk-01]
  - name: 自动选择
    type: url-test
    url: http://old-url.com
    members: [hk-01]
";
    let right = r"
proxy_groups:
  - name: 节点选择
    type: select
    members: [hk-01]
  - name: 自动选择
    type: url-test
    url: http://new-url.com
    members: [hk-02, sg-01]
";
    let out = render_structural_diff(left, right);
    assert!(out.contains("~ 自动选择:"), "out: {out}");
    assert!(out.contains("~ url:"), "out: {out}");
    assert!(out.contains("old-url.com"), "out: {out}");
    assert!(out.contains("new-url.com"), "out: {out}");
    assert!(out.contains("- members: [ hk-01 ]"), "out: {out}");
    assert!(out.contains("+ members: [ hk-02, sg-01 ]"), "out: {out}");
    assert!(out.contains("Summary: 1 modified, 0 added"), "out: {out}");
}

#[test]
fn added_group_entry_counts_as_added() {
    let left = "proxy_groups:\n  - {name: A, type: select}\n";
    let right = "proxy_groups:\n  - {name: A, type: select}\n  - {name: B, type: select}\n";
    let out = render_structural_diff(left, right);
    assert!(out.contains("+ B:"), "out: {out}");
    assert!(out.contains("Summary: 0 modified, 1 added"), "out: {out}");
}

#[test]
fn malformed_yaml_degrades_to_empty_diff_without_panic() {
    let out = render_structural_diff("not: [valid", "core: mihomo\n");
    assert!(out.contains("Summary:"), "out: {out}");
}
