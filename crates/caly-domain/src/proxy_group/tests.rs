//! Tests for `proxy_group.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

fn name(text: &str) -> ProxyGroupName {
    ProxyGroupName::new(text.to_owned()).unwrap()
}

#[test]
fn type_labels_match_clash_spelling() {
    assert_eq!(ProxyGroupType::Select.clash_label(), "select");
    assert_eq!(ProxyGroupType::UrlTest.clash_label(), "url-test");
    assert_eq!(ProxyGroupType::Fallback.clash_label(), "fallback");
    assert_eq!(ProxyGroupType::LoadBalance.clash_label(), "load-balance");
    assert_eq!(ProxyGroupType::Relay.clash_label(), "relay");
}

#[test]
fn needs_url_only_for_probe_driven_groups() {
    assert!(!ProxyGroupType::Select.needs_url());
    assert!(!ProxyGroupType::Relay.needs_url());
    assert!(ProxyGroupType::UrlTest.needs_url());
    assert!(ProxyGroupType::Fallback.needs_url());
    assert!(ProxyGroupType::LoadBalance.needs_url());
}

#[test]
fn member_renders_clash_form() {
    let m_node = ProxyGroupMember::Node {
        tag: BoundedText::new("hong-kong-1".to_owned()).unwrap(),
    };
    assert_eq!(m_node.to_clash(), "hong-kong-1");
    let m_group = ProxyGroupMember::Group { name: name("Auto") };
    assert_eq!(m_group.to_clash(), "Auto");
    assert_eq!(ProxyGroupMember::Direct.to_clash(), "DIRECT");
    assert_eq!(ProxyGroupMember::Reject.to_clash(), "REJECT");
}

#[test]
fn url_test_defaults_are_stable() {
    let cfg = UrlTestConfig::default_probe().unwrap();
    assert_eq!(cfg.url.as_str(), "http://www.gstatic.com/generate_204");
    assert_eq!(cfg.interval_seconds, 300);
    assert_eq!(cfg.tolerance_ms, 50);
}

#[test]
fn proxy_group_round_trips_through_yaml() {
    // A single member parses flat: `kind: node` is the
    // variant tag, the other fields are inline.
    let member_yaml = "kind: node\ntag: hong-kong-1\n";
    let m: ProxyGroupMember = serde_norway::from_str(member_yaml)
        .unwrap_or_else(|error| panic!("member must parse: {error}"));
    assert!(matches!(m, ProxyGroupMember::Node { .. }));
    // JSON round-trip mirrors what the schema validator sees
    // (the project uses `serde_json` for the validator path
    // and `serde_norway` for the file load path; both must
    // accept the same shape).
    let group_json = r#"{
        "name": "Auto",
        "type": "url-test",
        "members": [
            {"kind": "node", "tag": "hong-kong-1"},
            {"kind": "group", "name": "Sub"},
            {"kind": "direct"},
            {"kind": "reject"}
        ],
        "url_test": {
            "url": "http://www.gstatic.com/generate_204",
            "interval_seconds": 300,
            "tolerance_ms": 50
        }
    }"#;
    let group: ProxyGroup = serde_json::from_str(group_json)
        .unwrap_or_else(|error| panic!("group must parse: {error}"));
    assert_eq!(group.name.as_str(), "Auto");
    assert_eq!(group.kind, ProxyGroupType::UrlTest);
    assert_eq!(group.members.len(), 4);
    assert!(group.url_test.is_some());
    // Round-trip: serialise then deserialise via JSON.
    let rendered = serde_json::to_string(&group).unwrap();
    let reparsed: ProxyGroup = serde_json::from_str(&rendered)
        .unwrap_or_else(|error| panic!("round-trip must parse: {error}\n{rendered}"));
    assert_eq!(group, reparsed);
}

#[test]
fn proxy_group_rejects_unknown_type() {
    // `deny_unknown_fields` is not on the outer struct (we
    // want the schema validator to do that), but the enum
    // has `rename_all = kebab-case`. A typo'd `type` is
    // caught by serde with an "unknown variant" error.
    let yaml = "name: Bad\ntype: round-robin\nmembers: []\n";
    let result: Result<ProxyGroup, _> = serde_norway::from_str(yaml);
    assert!(result.is_err(), "unknown group type must fail");
}

#[test]
fn display_includes_group_name() {
    let error = ProxyGroupError::DuplicateName {
        name: "Auto".to_owned(),
    };
    assert!(format!("{error}").contains("Auto"));
    let error = ProxyGroupError::RelayCycle {
        group: "X".to_owned(),
    };
    assert!(format!("{error}").contains('X'));
}
