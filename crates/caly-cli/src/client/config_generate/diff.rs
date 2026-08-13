//! Diff faces for `caly config diff`.
//!
//! Split out of `client/config_generate.rs` (audit #70 file-length
//! budget). W3a (cli-v3-design.md §5.5): the human face is the
//! terraform-plan **structural** YAML diff
//! ([`render_structural_diff`]); `--json` keeps the hunk-list
//! contract from [`naive_line_diff`] (a dependency-free line-by-line
//! walk — no LCS, no `@@` headers).

use std::path::Path;
use std::process::ExitCode;

use super::{AppPaths, report_error};

/// `set config diff [<other>]` — line-by-line diff between
/// the active `config.yaml` and either a user-supplied file
/// (with `--file <PATH>`) or the documented default
/// (rendered via `caly_profile::schema::render_default_config`).
///
/// W3a (`cli-v3-design.md` §5.5): the human face is the terraform-plan
/// structural view ([`render_structural_diff`]); `--json` keeps the
/// hunk-list contract ([`naive_line_diff`], a dependency-free
/// line-by-line walk — no LCS, no `@@` headers despite what older
/// comments claimed).
pub fn diff_config(other: Option<&Path>, json: bool) -> ExitCode {
    let active_path = AppPaths::from_env().config.join("config.yaml");
    let active_body = match std::fs::read_to_string(&active_path) {
        Ok(s) => s,
        Err(error) => {
            return report_error(
                format!(
                    "cannot read active config ({}): {error}",
                    active_path.display()
                ),
                json,
            );
        }
    };
    let (other_label, other_body) = if let Some(path) = other {
        let label = path.display().to_string();
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(error) => {
                return report_error(
                    format!("cannot read comparison file ({label}): {error}"),
                    json,
                );
            }
        };
        (label, body)
    } else {
        let body = caly_profile::schema::render_default_config();
        ("<default>".to_owned(), body)
    };
    let hunks = naive_line_diff(&active_body, &other_body);
    if json {
        let non_equal: usize = hunks.iter().filter(|h| h.kind != DiffKind::Equal).count();
        let items: Vec<serde_json::Value> = hunks
            .iter()
            .filter(|h| h.kind != DiffKind::Equal)
            .map(|h| {
                serde_json::json!({
                    "kind": h.kind.label(),
                    "left_line": h.left_line,
                    "right_line": h.right_line,
                    "left_text": h.left_text,
                    "right_text": h.right_text,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "left": active_path.display().to_string(),
            "right": other_label,
            "hunks": items,
            "changed": non_equal > 0,
        });
        println!("{payload}");
    } else {
        print!("{}", render_structural_diff(&active_body, &other_body));
    }
    ExitCode::SUCCESS
}

/// One hunk of a line-by-line diff. The hunk carries both
/// the left-side line number (for `-` ops) and the
/// right-side line number (for `+` ops) so a JSON
/// consumer can sort / group without re-counting.
#[derive(Clone, Debug)]
pub(crate) struct DiffHunk {
    pub(crate) kind: DiffKind,
    pub(crate) left_line: Option<usize>,
    pub(crate) right_line: Option<usize>,
    pub(crate) left_text: String,
    pub(crate) right_text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffKind {
    Equal,
    Insert,
    Delete,
}

impl DiffKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Insert => "insert",
            Self::Delete => "delete",
        }
    }
}

/// Naive line-by-line diff: walk both inputs in lockstep;
/// emit a `Delete` + `Insert` hunk for each line that
/// differs. `Equal` hunks are kept for context (a future
/// round can collapse them with a Myers algorithm).
pub(crate) fn naive_line_diff(left: &str, right: &str) -> Vec<DiffHunk> {
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();
    let max = left_lines.len().max(right_lines.len());
    let mut hunks = Vec::new();
    for index in 0..max {
        let l = left_lines.get(index).copied();
        let r = right_lines.get(index).copied();
        match (l, r) {
            (Some(l), Some(r)) if l == r => hunks.push(DiffHunk {
                kind: DiffKind::Equal,
                left_line: Some(index + 1),
                right_line: Some(index + 1),
                left_text: l.to_owned(),
                right_text: r.to_owned(),
            }),
            (Some(l), Some(r)) => {
                hunks.push(DiffHunk {
                    kind: DiffKind::Delete,
                    left_line: Some(index + 1),
                    right_line: None,
                    left_text: l.to_owned(),
                    right_text: String::new(),
                });
                hunks.push(DiffHunk {
                    kind: DiffKind::Insert,
                    left_line: None,
                    right_line: Some(index + 1),
                    left_text: String::new(),
                    right_text: r.to_owned(),
                });
            }
            (Some(l), None) => hunks.push(DiffHunk {
                kind: DiffKind::Delete,
                left_line: Some(index + 1),
                right_line: None,
                left_text: l.to_owned(),
                right_text: String::new(),
            }),
            (None, Some(r)) => hunks.push(DiffHunk {
                kind: DiffKind::Insert,
                left_line: None,
                right_line: Some(index + 1),
                left_text: String::new(),
                right_text: r.to_owned(),
            }),
            (None, None) => break,
        }
    }
    hunks
}

// ── W3a structural diff (cli-v3-design.md §5.5, terraform-plan view) ──

/// Outcome counters of one structural diff pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StructuralCounts {
    modified: usize,
    added: usize,
    removed: usize,
}

/// Renders the §5.5 terraform-plan view: a semantic walk of the two
/// YAML documents (active config vs default/`--file`), not a line
/// diff. Layout laws:
///
/// - top-level mapping keys: `~ key` (changed) / `+ key` (added) /
///   `- key` (removed);
/// - scalar changes render `~ key: "old" → "new"`;
/// - name-keyed sequence entries (e.g. `proxy_groups:`) align by
///   `name` and diff per field — the `~ 自动选择:` block shape;
/// - plain sequences render as compact `- key: [ … ]` / `+ key: [ … ]`
///   whole-list replacements;
/// - the walk always terminates on any YAML shape (nested mappings
///   recurse with indentation; no cycle is representable in a tree).
pub(crate) fn render_structural_diff(left: &str, right: &str) -> String {
    let left: serde_norway::Value =
        serde_norway::from_str(left).unwrap_or(serde_norway::Value::Null);
    let right: serde_norway::Value =
        serde_norway::from_str(right).unwrap_or(serde_norway::Value::Null);
    let mut out = String::new();
    let mut counts = StructuralCounts::default();
    diff_mapping(&mut out, &mut counts, &left, &right, 0, true);
    if out.is_empty() {
        out.push_str("(no changes)\n");
    }
    let mut summary = format!(
        "Summary: {} modified, {} added",
        counts.modified, counts.added
    );
    if counts.removed > 0 {
        let _ =
            std::fmt::Write::write_fmt(&mut summary, format_args!(", {} removed", counts.removed));
    }
    summary.push_str(". Run with --apply to sync.\n");
    out.push_str(&summary);
    out
}

/// Diffs two mappings key by key (declaration order of the left, then
/// right-only keys).
fn diff_mapping(
    out: &mut String,
    counts: &mut StructuralCounts,
    left: &serde_norway::Value,
    right: &serde_norway::Value,
    indent: usize,
    count: bool,
) {
    let serde_norway::Value::Mapping(left_map) = left else {
        // Not a mapping on the left: handled by the value-level diff.
        return;
    };
    let serde_norway::Value::Mapping(right_map) = right else {
        return;
    };
    let pad = "  ".repeat(indent);
    let mut right_seen: Vec<serde_norway::Value> = Vec::new();
    for (left_key, left_value) in left_map {
        match right_map.get(left_key) {
            Some(right_value) if right_value == left_value => {}
            Some(right_value) => {
                diff_value(
                    out,
                    counts,
                    &pad,
                    key_text(left_key),
                    left_value,
                    right_value,
                    indent,
                    count,
                );
            }
            None => {
                if count {
                    counts.removed = counts.removed.saturating_add(1);
                }
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!("{pad}- {}: {}\n", key_text(left_key), compact(left_value)),
                );
            }
        }
        right_seen.push(left_key.clone());
    }
    for (right_key, right_value) in right_map {
        if right_seen.contains(right_key) {
            continue;
        }
        if count {
            counts.added = counts.added.saturating_add(1);
        }
        let _ = std::fmt::Write::write_fmt(
            out,
            format_args!("{pad}+ {}: {}\n", key_text(right_key), compact(right_value)),
        );
    }
}

/// Diffs two values under one key: scalars render inline, mappings
/// recurse (modified counts one), name-keyed sequences align by name.
fn diff_value(
    out: &mut String,
    counts: &mut StructuralCounts,
    pad: &str,
    key: String,
    left: &serde_norway::Value,
    right: &serde_norway::Value,
    indent: usize,
    count: bool,
) {
    match (left, right) {
        (serde_norway::Value::Mapping(_), serde_norway::Value::Mapping(_)) => {
            if count {
                counts.modified = counts.modified.saturating_add(1);
            }
            let _ = std::fmt::Write::write_fmt(out, format_args!("{pad}~ {key}:\n"));
            diff_mapping(out, counts, left, right, indent + 1, false);
        }
        (serde_norway::Value::Sequence(_), serde_norway::Value::Sequence(_)) => {
            if is_name_keyed(left) && is_name_keyed(right) {
                diff_named_sequence(out, counts, pad, &key, left, right, indent);
            } else {
                if count {
                    counts.modified = counts.modified.saturating_add(1);
                }
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!("{pad}- {key}: {}\n", compact(left)),
                );
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!("{pad}+ {key}: {}\n", compact(right)),
                );
            }
        }
        _ => {
            if count {
                counts.modified = counts.modified.saturating_add(1);
            }
            let _ = std::fmt::Write::write_fmt(
                out,
                format_args!("{pad}~ {key}: {} → {}\n", compact(left), compact(right)),
            );
        }
    }
}

/// Diffs two sequences of name-keyed mappings (e.g. `proxy_groups:`)
/// by aligning entries on their `name` field — the §5.5 `~ 自动选择:`
/// block shape. Entries present in both recurse as mappings; only-left
/// entries emit `- name: …`; only-right entries `+ name: …`.
fn diff_named_sequence(
    out: &mut String,
    counts: &mut StructuralCounts,
    pad: &str,
    _key: &str,
    left: &serde_norway::Value,
    right: &serde_norway::Value,
    indent: usize,
) {
    let serde_norway::Value::Sequence(left_items) = left else {
        return;
    };
    let serde_norway::Value::Sequence(right_items) = right else {
        return;
    };
    let mut right_seen: Vec<String> = Vec::new();
    let mut had_change = false;
    for left_item in left_items {
        let left_name = item_name(left_item);
        let matched = right_items
            .iter()
            .find(|right_item| item_name(right_item) == left_name);
        match matched {
            Some(right_item) if right_item == left_item => {}
            Some(right_item) => {
                had_change = true;
                counts.modified = counts.modified.saturating_add(1);
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!("{pad}~ {}:\n", item_label(left_item, left_name.as_deref())),
                );
                diff_mapping(out, counts, left_item, right_item, indent + 1, false);
            }
            None => {
                had_change = true;
                counts.removed = counts.removed.saturating_add(1);
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!(
                        "{pad}- {}: {}\n",
                        item_label(left_item, left_name.as_deref()),
                        compact(left_item)
                    ),
                );
            }
        }
        right_seen.push(left_name.clone().unwrap_or_default());
    }
    for right_item in right_items {
        let name = item_name(right_item).unwrap_or_default();
        if right_seen.contains(&name) {
            continue;
        }
        had_change = true;
        counts.added = counts.added.saturating_add(1);
        let _ = std::fmt::Write::write_fmt(
            out,
            format_args!(
                "{pad}+ {}: {}\n",
                item_label(right_item, Some(name.as_str())),
                compact(right_item)
            ),
        );
    }
    if !had_change {
        // Equal sequences must not double-count a parent modified.
        counts.modified = counts.modified.saturating_sub(1);
    }
}

/// The `name` field of a mapping item, if any.
fn item_name(item: &serde_norway::Value) -> Option<String> {
    let serde_norway::Value::Mapping(map) = item else {
        return None;
    };
    map.get(serde_norway::Value::String("name".to_owned()))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// Human label for a sequence item: `name` when present (the §5.5
/// `[vmess] new-node` shape keeps the caller's kind prefix in the
/// value, so the name alone is the anchor).
fn item_label(item: &serde_norway::Value, name: Option<&str>) -> String {
    name.map_or_else(|| compact(item), str::to_owned)
}

/// Whether a sequence contains only name-keyed mappings.
fn is_name_keyed(value: &serde_norway::Value) -> bool {
    let serde_norway::Value::Sequence(items) = value else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| {
            matches!(item, serde_norway::Value::Mapping(_)) && item_name(item).is_some()
        })
}

/// Renders a YAML key as text.
fn key_text(key: &serde_norway::Value) -> String {
    key.as_str().map_or_else(|| compact(key), str::to_owned)
}

/// One-line compact rendering of any value: scalars via the YAML
/// serializer (strings quoted), sequences as `[ a, b ]`.
fn compact(value: &serde_norway::Value) -> String {
    match value {
        serde_norway::Value::Sequence(items) => {
            let inner: Vec<String> = items.iter().map(compact).collect();
            format!("[ {} ]", inner.join(", "))
        }
        serde_norway::Value::Mapping(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", key_text(k), compact(v)))
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
        other => serde_norway::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }
}
