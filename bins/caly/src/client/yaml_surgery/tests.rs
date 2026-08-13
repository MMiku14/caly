//! Tests for `client/yaml_surgery.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct Row {
    url: String,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

fn row(url: &str, enabled: bool) -> Row {
    Row {
        url: url.to_owned(),
        enabled,
        name: None,
    }
}

fn map_error(reason: String) -> SurgeryError {
    SurgeryError::Parse(reason)
}

fn changed(outcome: EditOutcome) -> String {
    match outcome {
        EditOutcome::Changed(text) => text,
        EditOutcome::Unchanged => panic!("expected a Changed outcome"),
    }
}

#[test]
fn derive_edit_classifies_the_minimal_shapes() {
    let a = row("https://a", true);
    let b = row("https://b", false);
    assert_eq!(
        derive_list_edit(std::slice::from_ref(&a), std::slice::from_ref(&a)),
        ListEdit::NoChange
    );
    assert_eq!(
        derive_list_edit(std::slice::from_ref(&a), &[a.clone(), b.clone()]),
        ListEdit::Append(vec![b.clone()])
    );
    assert_eq!(
        derive_list_edit(&[a.clone(), b.clone()], std::slice::from_ref(&a)),
        ListEdit::Remove(vec![1])
    );
    assert_eq!(
        derive_list_edit(
            std::slice::from_ref(&a),
            std::slice::from_ref(&row("https://a", false))
        ),
        ListEdit::Replace(vec![(0, row("https://a", false))])
    );
    // Same-length swaps are Replace pairs (in-place re-render
    // keeps comments); a length-changing mixed edit that is
    // neither append-tail nor subsequence is a Rewrite.
    assert!(matches!(
        derive_list_edit(std::slice::from_ref(&a), &[b.clone(), a.clone()]),
        ListEdit::Rewrite(_)
    ));
}

#[test]
fn split_comment_respects_quotes() {
    assert_eq!(
        split_trailing_comment("url: \"https://x#y\" # note"),
        ("url: \"https://x#y\" ", "# note")
    );
    assert_eq!(
        split_trailing_comment("enabled: true"),
        ("enabled: true", "")
    );
    assert_eq!(split_trailing_comment("# only"), ("", "# only"));
}

const ANNOTATED: &str = "\
# operator notes — keep me
schema_version: 1

subscriptions:
  # my sources below
  sources:
    - url: https://a.example/feed  # home uplink
      enabled: true
    - url: https://b.example/feed
      enabled: false
  connect_timeout_ms: 9000

core: mihomo
";

#[test]
fn append_preserves_comments_and_annotations() {
    let text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            ANNOTATED,
            &["subscriptions", "sources"],
            map_error,
            |current| {
                let mut next = current.to_vec();
                next.push(row("https://c.example/feed", true));
                Ok(next)
            },
        )
        .unwrap(),
    );
    assert!(text.contains("# operator notes — keep me"), "{text}");
    assert!(text.contains("  # my sources below"), "{text}");
    assert!(text.contains("connect_timeout_ms: 9000"), "{text}");
    assert!(
        text.contains("- url: https://a.example/feed  # home uplink"),
        "{text}"
    );
    assert!(text.contains("- url: https://c.example/feed\n"), "{text}");
    let value: serde_norway::Value = serde_norway::from_str(&text).unwrap();
    let parsed = navigate_typed::<Row>(&value, &["subscriptions", "sources"]).unwrap();
    assert_eq!(parsed.len(), 3);
}

#[test]
fn remove_deletes_only_the_entry_lines() {
    let text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            ANNOTATED,
            &["subscriptions", "sources"],
            map_error,
            |current| {
                Ok(current
                    .iter()
                    .filter(|row| !row.url.contains('b'))
                    .cloned()
                    .collect())
            },
        )
        .unwrap(),
    );
    assert!(!text.contains("https://b.example/feed"), "{text}");
    assert!(text.contains("# home uplink"), "{text}");
    assert!(text.contains("connect_timeout_ms: 9000"), "{text}");
}

#[test]
fn toggle_enabled_preserves_inline_comment() {
    let text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            ANNOTATED,
            &["subscriptions", "sources"],
            map_error,
            |current| {
                Ok(current
                    .iter()
                    .map(|row| Row {
                        enabled: !row.enabled,
                        ..row.clone()
                    })
                    .collect())
            },
        )
        .unwrap(),
    );
    // The inline comment stays on the line the user annotated
    // (the `url:` line), while the `enabled:` value toggles.
    assert!(
        text.contains("- url: https://a.example/feed  # home uplink"),
        "{text}"
    );
    assert!(text.contains("enabled: false"), "{text}");
    assert!(text.contains("enabled: true"), "{text}");
}

#[test]
fn remove_all_collapses_to_inline_empty_sequence() {
    let text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            ANNOTATED,
            &["subscriptions", "sources"],
            map_error,
            |_| Ok(Vec::new()),
        )
        .unwrap(),
    );
    assert!(text.contains("sources: []"), "{text}");
    let value: serde_norway::Value = serde_norway::from_str(&text).unwrap();
    let parsed = navigate_typed::<Row>(&value, &["subscriptions", "sources"]).unwrap();
    assert!(parsed.is_empty());
}

#[test]
fn no_change_returns_unchanged_without_touching_text() {
    let outcome = edit_yaml_list::<Row, _, SurgeryError>(
        ANNOTATED,
        &["subscriptions", "sources"],
        map_error,
        |current| Ok(current.to_vec()),
    )
    .unwrap();
    assert_eq!(outcome, EditOutcome::Unchanged);
}

#[test]
fn append_creates_missing_list_and_parent_block() {
    let text = "schema_version: 1\ncore: mihomo\n";
    let new_text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            text,
            &["subscriptions", "sources"],
            map_error,
            |_| Ok(vec![row("https://a", true)]),
        )
        .unwrap(),
    );
    assert!(
        new_text.contains("subscriptions:\n  sources:\n    - url: https://a\n"),
        "{new_text}"
    );
    assert!(
        new_text.starts_with("schema_version: 1\ncore: mihomo\n"),
        "{new_text}"
    );
}

#[test]
fn append_into_existing_parent_without_sources_inserts_child() {
    let text = "subscriptions:\n  connect_timeout_ms: 9000\ncore: mihomo\n";
    let new_text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            text,
            &["subscriptions", "sources"],
            map_error,
            |_| Ok(vec![row("https://a", true)]),
        )
        .unwrap(),
    );
    assert!(
        new_text.contains("  connect_timeout_ms: 9000\n  sources:\n    - url: https://a\n"),
        "{new_text}"
    );
}

#[test]
fn flow_empty_seq_append_rewrites_to_block_locally() {
    let text = "subscriptions:\n  sources: []\ncore: mihomo\n";
    let new_text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            text,
            &["subscriptions", "sources"],
            map_error,
            |_| Ok(vec![row("https://a", true)]),
        )
        .unwrap(),
    );
    assert!(
        new_text.contains("  sources:\n    - url: https://a\n"),
        "{new_text}"
    );
    assert!(new_text.contains("core: mihomo"), "{new_text}");
}

#[test]
fn empty_file_surfaces_a_parse_error_like_before() {
    let outcome = edit_yaml_list::<Row, _, SurgeryError>("", &["proxy_groups"], map_error, |_| {
        Ok(vec![row("https://a", true)])
    });
    // An empty document is YAML `null`, not a mapping — the same
    // error the previous writers surfaced.
    assert!(outcome.is_err());
}

#[test]
fn crlf_files_keep_crlf() {
    let text = "proxy_groups:\r\n  - url: https://a\r\n    enabled: true\r\n";
    let new_text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(text, &["proxy_groups"], map_error, |current| {
            let mut next = current.to_vec();
            next.push(row("https://b", false));
            Ok(next)
        })
        .unwrap(),
    );
    assert!(new_text.contains("\r\n"), "{new_text}");
    assert!(!new_text.replace("\r\n", "").contains('\n'), "{new_text}");
}

#[test]
fn rewrite_reorders_entries_without_touching_other_keys() {
    let text = changed(
        edit_yaml_list::<Row, _, SurgeryError>(
            ANNOTATED,
            &["subscriptions", "sources"],
            map_error,
            |current| Ok(current.iter().rev().cloned().collect()),
        )
        .unwrap(),
    );
    // The reordered entries both remain, other scalar keys intact.
    assert!(text.contains("connect_timeout_ms: 9000"), "{text}");
    let b_pos = text.find("https://b.example/feed").unwrap();
    let a_pos = text.find("https://a.example/feed").unwrap();
    assert!(b_pos < a_pos, "{text}");
}
