//! Tests for the entry-tree renderer (cli-v3-design.md G1 / W3a).
//!
//! Fixture coverage per the W3a exit criterion (三 fixture 人工排版
//! diff + §6.1 契约快照): the full airport fixture, a small
//! handcrafted Clash document, and the degenerate URI-lines shape.

use super::*;
use caly_subscription::{parse_clash_config, parse_uri_body_to_display_lossy};

/// Subscription id shared by every offline parse in these tests.
fn sub_id() -> caly_domain::SubscriptionId {
    caly_domain::SubscriptionId::from_bytes([0; 16])
}

/// Parses a Clash YAML body into an [`EntryTree`] via the same offline
/// pipeline `sub parse` uses.
fn tree_from_yaml(body: &str) -> EntryTree {
    let import = parse_clash_config(body, sub_id()).expect("fixture must parse");
    from_clash_import(&import, "clash-yaml")
}

/// Small handcrafted document: one selector with a nested urltest
/// group, one direct member, two protocol nodes, and two rules.
const SMALL_YAML: &str = r#"
proxies:
  - {name: hk-01, type: vmess, server: a.example.com, port: 443, uuid: "11111111-1111-1111-1111-111111111111", alterId: 0, cipher: auto}
  - {name: us-02, type: ss, server: b.example.com, port: 8388, cipher: aes-256-gcm, password: pw}
proxy-groups:
  - {name: 节点选择, type: select, proxies: [自动选择, hk-01, DIRECT]}
  - {name: 自动选择, type: url-test, proxies: [hk-01, us-02], url: "http://cp.example.com/generate_204", interval: 600, tolerance: 200}
rules:
  - DOMAIN-SUFFIX,example.com,节点选择
  - MATCH,自动选择
"#;

#[test]
fn small_document_builds_full_three_zone_tree() {
    let tree = tree_from_yaml(SMALL_YAML);
    assert_eq!(tree.groups.len(), 2);
    assert_eq!(tree.protocol_count(), 2);
    assert_eq!(tree.builtins.len(), 1);
    assert_eq!(tree.rules.len(), 2);
    // Both nodes are grouped — the 未入组 zone is empty.
    assert!(tree.ungrouped.is_empty());

    let human = render_human(&tree);
    assert!(human.contains("[selector]     节点选择"), "human: {human}");
    assert!(human.contains("[urltest]      自动选择"), "human: {human}");
    // Badge mapping: select→selector, url-test→urltest, ss→ss.
    assert!(human.contains("[ss]           us-02"), "human: {human}");
    assert!(human.contains("[vmess]        hk-01"), "human: {human}");
    assert!(human.contains("[direct]       DIRECT"), "human: {human}");
    // Nested group is shown once, without recursion.
    assert!(human.contains("→ 嵌套组(见 自动选择)"), "human: {human}");
    // Probe annotation on the urltest group header.
    assert!(
        human.contains("url=http://cp.example.com/generate_204"),
        "human: {human}"
    );
    assert!(human.contains("interval=600s"), "human: {human}");
    assert!(human.contains("tolerance=200ms"), "human: {human}");
    // Rule zone with policy back-links, targets sharing the badge lane.
    assert!(
        human.contains("DOMAIN-SUFFIX,example.com,节点选择 → [selector]     节点选择"),
        "human: {human}"
    );
    assert!(
        human.contains("MATCH,自动选择 → [urltest]      自动选择"),
        "human: {human}"
    );
}

#[test]
fn small_document_json_matches_six_one_contract() {
    let tree = tree_from_yaml(SMALL_YAML);
    let json = render_json(&tree);
    assert_eq!(json["format"], "clash-yaml");
    assert_eq!(json["ok"], true);
    assert_eq!(
        json["counts"],
        json!({
            "entries": 5, "protocols": 2, "groups": 2, "builtins": 1, "rules": 2
        })
    );

    // entries[]: groups first (declaration order), then protocols, then
    // distinct builtins.
    let entries = json["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0]["name"], "节点选择");
    assert_eq!(entries[0]["type"], "group");
    assert_eq!(entries[0]["kind"], "selector");
    assert_eq!(entries[1]["name"], "自动选择");
    assert_eq!(entries[1]["kind"], "urltest");
    assert_eq!(entries[1]["url"], "http://cp.example.com/generate_204");
    assert_eq!(entries[1]["interval_seconds"], 600);
    assert_eq!(entries[1]["tolerance_ms"], 200);
    // Member list: nested group is a ref, nodes carry name+kind, DIRECT
    // carries a name.
    let members = entries[0]["members"].as_array().expect("members array");
    assert_eq!(members.len(), 3);
    assert_eq!(
        members[0],
        json!({"name": "自动选择", "kind": "urltest", "ref": true})
    );
    assert_eq!(members[1], json!({"name": "hk-01", "kind": "vmess"}));
    assert_eq!(members[2], json!({"name": "DIRECT", "kind": "direct"}));
    // Protocol entries with groups_in back-links.
    assert_eq!(
        entries[2],
        json!({
            "name": "hk-01", "kind": "vmess", "type": "protocol",
            "groups_in": ["节点选择", "自动选择"],
        })
    );
    assert_eq!(
        entries[3],
        json!({
            "name": "us-02", "kind": "ss", "type": "protocol",
            "groups_in": ["自动选择"],
        })
    );
    // Builtin entry, once.
    assert_eq!(
        entries[4],
        json!({"name": "DIRECT", "kind": "direct", "type": "builtin"})
    );
    // Rules with target_kind resolution.
    assert_eq!(
        json["rules"],
        json!([
            {"text": "DOMAIN-SUFFIX,example.com,节点选择", "target": "节点选择", "target_kind": "selector"},
            {"text": "MATCH,自动选择", "target": "自动选择", "target_kind": "urltest"},
        ])
    );
}

#[test]
fn ungrouped_nodes_land_in_the_ungrouped_zone() {
    // us-02 is declared but referenced by no group.
    let yaml = r#"
proxies:
  - {name: hk-01, type: vmess, server: a.example.com, port: 443, uuid: "11111111-1111-1111-1111-111111111111", alterId: 0, cipher: auto}
  - {name: us-02, type: ss, server: b.example.com, port: 8388, cipher: aes-256-gcm, password: pw}
proxy-groups:
  - {name: 节点选择, type: select, proxies: [hk-01]}
"#;
    let tree = tree_from_yaml(yaml);
    assert_eq!(tree.ungrouped.len(), 1);
    assert_eq!(tree.ungrouped[0].name, "us-02");
    let human = render_human(&tree);
    assert!(human.contains("ungrouped nodes"), "human: {human}");
    assert!(human.contains("└─ [ss]           us-02"), "human: {human}");
    // The grouped node carries groups_in in JSON; the ungrouped one omits it.
    let json = render_json(&tree);
    let entries = json["entries"].as_array().expect("entries array");
    let hk = entries
        .iter()
        .find(|e| e["name"] == "hk-01")
        .expect("hk entry");
    assert_eq!(hk["groups_in"], json!(["节点选择"]));
    let us = entries
        .iter()
        .find(|e| e["name"] == "us-02")
        .expect("us entry");
    assert!(
        us.get("groups_in").is_none(),
        "ungrouped must omit groups_in"
    );
}

#[test]
fn degenerate_uri_document_renders_protocol_listing() {
    let tree = from_uri_nodes(
        "uri-lines",
        vec![
            ("hk-01".to_owned(), "vmess".to_owned()),
            ("us-02".to_owned(), "ss".to_owned()),
        ],
    );
    let human = render_human(&tree);
    assert_eq!(human, "[vmess]        hk-01\n[ss]           us-02\n\n");
    let json = render_json(&tree);
    assert_eq!(json["format"], "uri-lines");
    assert_eq!(
        json["counts"],
        json!({
            "entries": 2, "protocols": 2, "groups": 0, "builtins": 0, "rules": 0
        })
    );
    assert_eq!(json["entries"].as_array().expect("entries").len(), 2);
    assert!(json["entries"][0].get("groups_in").is_none());
}

#[test]
fn empty_document_renders_no_entries_placeholder() {
    let tree = from_uri_nodes("uri-lines", Vec::new());
    assert_eq!(render_human(&tree), "(no entries)\n");
}

#[test]
fn dangling_group_reference_is_not_a_cycle() {
    // A references a group G that does not exist (schema-rejected upstream;
    // defensive here). Regression: the cycle walk must not short-circuit
    // `from == target` against a missing group and mislabel it `⟲ cycle`.
    let groups = vec![TreeGroup {
        name: "A".to_owned(),
        kind: "selector".to_owned(),
        url: None,
        interval_seconds: None,
        tolerance_ms: None,
        members: vec![TreeMember::Group {
            name: "G".to_owned(),
            // resolve_group_refs degrades a dangling reference to [unknown].
            kind: "unknown".to_owned(),
        }],
    }];
    let tree = EntryTree {
        format: "config".to_owned(),
        groups,
        ungrouped: Vec::new(),
        rules: Vec::new(),
        builtins: Vec::new(),
    };
    let human = render_human(&tree);
    assert!(
        !human.contains("⟲ cycle"),
        "dangling ref must not read as a cycle: {human}"
    );
    assert!(
        human.contains("[unknown]      G → 嵌套组(见 G)"),
        "human: {human}"
    );
    let json = render_json(&tree);
    let members = json["entries"][0]["members"].as_array().expect("members");
    assert_eq!(
        members[0],
        json!({"name": "G", "kind": "unknown", "ref": true})
    );
}

#[test]
fn self_reference_marks_cycle() {
    // A → A: the tightest cycle; must be marked and never recurse forever.
    let groups = vec![TreeGroup {
        name: "A".to_owned(),
        kind: "selector".to_owned(),
        url: None,
        interval_seconds: None,
        tolerance_ms: None,
        members: vec![TreeMember::Group {
            name: "A".to_owned(),
            kind: "selector".to_owned(),
        }],
    }];
    let tree = EntryTree {
        format: "config".to_owned(),
        groups,
        ungrouped: Vec::new(),
        rules: Vec::new(),
        builtins: Vec::new(),
    };
    let human = render_human(&tree);
    assert!(human.contains("→ ⟲ cycle"), "human: {human}");
}

#[test]
fn reference_cycle_marks_cycle_and_never_panics() {
    // A → B → A: schema-rejected, but the walk must terminate and mark it.
    let groups = vec![
        TreeGroup {
            name: "A".to_owned(),
            kind: "selector".to_owned(),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            members: vec![TreeMember::Group {
                name: "B".to_owned(),
                kind: "selector".to_owned(),
            }],
        },
        TreeGroup {
            name: "B".to_owned(),
            kind: "selector".to_owned(),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            members: vec![TreeMember::Group {
                name: "A".to_owned(),
                kind: "selector".to_owned(),
            }],
        },
    ];
    let tree = EntryTree {
        format: "config".to_owned(),
        groups,
        ungrouped: Vec::new(),
        rules: Vec::new(),
        builtins: Vec::new(),
    };
    let human = render_human(&tree);
    assert!(human.contains("→ ⟲ cycle"), "human: {human}");
}

#[test]
fn airport_fixture_full_tree_and_json_contract() {
    // The real-world airport fixture (30 nodes / 17 groups / full rule
    // table) exercises the renderer end to end; assertions pin the
    // structural invariants, the exact counts are the W3a 人工排版 diff.
    let body = include_bytes!("../../../../fixtures/clash-airport-full.yaml");
    let tree = tree_from_yaml(std::str::from_utf8(body).expect("fixture is utf8"));

    assert_eq!(tree.groups.len(), 16, "airport fixture group count");
    assert_eq!(tree.protocol_count(), 30, "airport fixture node count");
    assert!(!tree.builtins.is_empty(), "DIRECT appears in groups");
    assert!(tree.builtins.iter().any(|b| b.name == "DIRECT"));
    assert!(!tree.rules.is_empty(), "rule table is parsed");

    let human = render_human(&tree);
    // First group header is the declaration-order opener.
    assert!(
        human.starts_with("[selector]     节点选择"),
        "human starts: {human}"
    );
    assert!(human.contains("[urltest]      自动选择"), "human: {human}");
    // The 自动选择 group carries the probe annotation from the fixture.
    assert!(human.contains("interval=600s"), "human: {human}");
    assert!(human.contains("tolerance=200ms"), "human: {human}");
    // A nested group reference is marked, not expanded.
    assert!(human.contains("→ 嵌套组(见 自动选择)"), "human: {human}");
    // Rule zone exists with back-links.
    assert!(human.contains("rules zone"), "human: {human}");
    assert!(human.contains("→ [selector]"), "human: {human}");

    let json = render_json(&tree);
    assert_eq!(json["counts"]["groups"], 16);
    assert_eq!(json["counts"]["protocols"], 30);
    assert_eq!(
        json["counts"]["entries"].as_u64().unwrap_or(0),
        (tree.groups.len() + 30 + tree.builtins.len()) as u64
    );
    // Every protocol entry has a well-formed shape.
    for entry in json["entries"].as_array().expect("entries") {
        assert!(entry["name"].is_string(), "entry: {entry}");
        assert!(entry["kind"].is_string(), "entry: {entry}");
        let kind = entry["type"].as_str().expect("type");
        assert!(
            matches!(kind, "group" | "protocol" | "builtin"),
            "entry: {entry}"
        );
    }
    // members[] are two-level only: no nested members arrays.
    for group in json["entries"].as_array().expect("entries") {
        if group["type"] == "group" {
            for member in group["members"].as_array().expect("members") {
                assert!(
                    member.get("members").is_none(),
                    "must stay two-level: {member}"
                );
            }
        }
    }
}

#[test]
fn uri_body_document_degenerates_to_listing() {
    // The real-world base64 aggregate (one long line, no groups) must
    // degrade to the protocol listing without a group zone.
    let body = include_bytes!("../../../../fixtures/subscription-20260803.txt");
    let projection =
        parse_uri_body_to_display_lossy(body.to_vec(), sub_id()).expect("aggregate must parse");
    let nodes: Vec<(String, String)> = projection
        .nodes
        .iter()
        .map(|node| {
            (
                node.name().as_str().to_owned(),
                badge_kind(node.protocol().as_str()),
            )
        })
        .collect();
    assert!(!nodes.is_empty(), "aggregate carries nodes");
    let tree = from_uri_nodes("base64-uri-lines", nodes);
    assert!(!tree.has_groups());
    assert!(tree.rules.is_empty());
    let human = render_human(&tree);
    assert!(
        !human.contains("ungrouped nodes"),
        "degenerate listing has no zone title: {human}"
    );
    assert!(human.contains("[vless]"), "human: {human}");
    let json = render_json(&tree);
    assert_eq!(json["format"], "base64-uri-lines");
    assert_eq!(json["counts"]["groups"], 0);
    assert_eq!(json["counts"]["rules"], 0);
}

#[test]
fn from_declared_builds_ungrouped_builtins_and_refs() {
    // `node list --offline --format=tree` path: declared groups + inline
    // nodes + rules. Node kinds come pre-resolved by the caller.
    let groups = vec![
        TreeGroup {
            name: "自动选择".to_owned(),
            kind: "urltest".to_owned(),
            url: Some("http://cp.example.com/generate_204".to_owned()),
            interval_seconds: Some(300),
            tolerance_ms: Some(50),
            members: vec![
                TreeMember::Node {
                    name: "hk-01".to_owned(),
                    kind: "vmess".to_owned(),
                },
                TreeMember::Group {
                    name: "备用".to_owned(),
                    kind: "selector".to_owned(),
                },
                TreeMember::Builtin {
                    name: "DIRECT".to_owned(),
                    kind: "direct".to_owned(),
                },
            ],
        },
        TreeGroup {
            name: "备用".to_owned(),
            kind: "selector".to_owned(),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            members: vec![TreeMember::Node {
                name: "us-02".to_owned(),
                kind: "ss".to_owned(),
            }],
        },
    ];
    let nodes = vec![
        ("hk-01".to_owned(), "vmess".to_owned()),
        ("us-02".to_owned(), "ss".to_owned()),
        ("de-03".to_owned(), "tuic".to_owned()),
    ];
    let rules = vec![TreeRule {
        text: "MATCH,自动选择".to_owned(),
        target: "自动选择".to_owned(),
        target_kind: None, // caller-resolved; renderer leaves it as-is
    }];
    let tree = from_declared("config", groups, nodes, rules);
    // de-03 is referenced by no group → the 未入组 zone.
    assert_eq!(tree.ungrouped.len(), 1);
    assert_eq!(tree.ungrouped[0].name, "de-03");
    assert_eq!(tree.ungrouped[0].kind, "tuic");
    assert_eq!(tree.protocol_count(), 3);
    assert_eq!(tree.builtins.len(), 1);
    assert_eq!(tree.builtins[0].name, "DIRECT");
    let human = render_human(&tree);
    assert!(human.contains("ungrouped nodes"), "human: {human}");
    assert!(human.contains("└─ [tuic]         de-03"), "human: {human}");
    assert!(human.contains("→ 嵌套组(见 备用)"), "human: {human}");
    assert!(
        human.contains("url=http://cp.example.com/generate_204"),
        "human: {human}"
    );
    let json = render_json(&tree);
    assert_eq!(
        json["counts"],
        json!({
            "entries": 6, "protocols": 3, "groups": 2, "builtins": 1, "rules": 1
        })
    );
}

#[test]
fn hostile_control_sequences_are_stripped_from_the_human_tree() {
    // W3a deep-review R-1: subscription-derived names and rule text must
    // never reach the terminal with control sequences intact (ANSI
    // injection). The JSON face keeps raw bytes.
    let evil_name = "HK-\u{1b}[2J\u{1b}[31mRED";
    let evil_rule = "DOMAIN-SUFFIX,evil.example,\u{1b}[31m节点选择";
    let groups = vec![TreeGroup {
        name: "节点选择".to_owned(),
        kind: "selector".to_owned(),
        url: None,
        interval_seconds: None,
        tolerance_ms: None,
        members: vec![TreeMember::Node {
            name: evil_name.to_owned(),
            kind: "vmess".to_owned(),
        }],
    }];
    let tree = EntryTree {
        format: "config".to_owned(),
        groups,
        ungrouped: Vec::new(),
        rules: vec![TreeRule {
            text: evil_rule.to_owned(),
            target: "节点选择".to_owned(),
            target_kind: Some("selector".to_owned()),
        }],
        builtins: Vec::new(),
    };
    let human = render_human(&tree);
    assert!(
        !human.contains('\u{1b}'),
        "ESC leaked into human tree: {human:?}"
    );
    assert!(
        human.contains("HK-[2J[31mRED"),
        "controls stripped, content kept: {human:?}"
    );
    assert!(human.contains("evil.example,"), "rule text kept: {human:?}");
    // JSON keeps the raw bytes (machine consumers make their own policy).
    let json = render_json(&tree);
    let text = json.to_string();
    assert!(
        text.contains("\\u001b"),
        "JSON should keep the escaped control char"
    );
}

#[test]
fn hostile_control_sequences_are_stripped_from_human_faces() {
    // CLI deep review R-1: rule text, group names and member tags come
    // from the subscription document un-sanitized — ESC/CSI payloads
    // must never reach the terminal. The JSON face keeps the raw text.
    let hostile_group = "Auto\u{1b}[2J\u{1b}[31mRed";
    let hostile_rule = "DOMAIN-SUFFIX,evil.com,\u{1b}[31mAuto\u{1b}[0m";
    let tree = EntryTree {
        format: "config".to_owned(),
        groups: vec![TreeGroup {
            name: hostile_group.to_owned(),
            kind: "selector".to_owned(),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            members: vec![TreeMember::Node {
                name: "HK\u{1b}[2J".to_owned(),
                kind: "vmess".to_owned(),
            }],
        }],
        ungrouped: Vec::new(),
        rules: vec![TreeRule {
            text: hostile_rule.to_owned(),
            target: "Auto".to_owned(),
            target_kind: Some("selector".to_owned()),
        }],
        builtins: Vec::new(),
    };
    let human = render_human(&tree);
    assert!(
        !human.contains('\u{1b}'),
        "ESC must not reach the terminal: {human:?}"
    );
    assert!(!human.contains('\u{7}'), "BEL must not reach the terminal");
    assert!(
        human.contains("Auto"),
        "group name content preserved: {human}"
    );
    assert!(
        human.contains("HK"),
        "member name content preserved: {human}"
    );
    assert!(
        human.contains("evil.com"),
        "rule text content preserved: {human}"
    );
    // The JSON face is machine-consumed: raw text stays intact.
    let json = render_json(&tree);
    let text = json.to_string();
    assert!(
        text.contains("\\u001b") || text.contains('\u{1b}'),
        "JSON keeps raw payload"
    );
}

/// 组源统一 (2026-08-12): the offline tree surfaces subscription-author
/// groups from the daemon's cached bodies, so `node pick <订阅组>` passes
/// the offline check exactly when the kernel renders the group. Config
/// groups win on name collisions.
#[test]
fn declared_groups_include_cached_subscription_groups() {
    use caly_profile::loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits};
    let dir = crate::test_helpers::temp_root("entry-tree-subgroups");
    let paths = crate::test_helpers::hermetic_paths(&dir);
    std::fs::create_dir_all(&paths.config).unwrap();
    let source_url = "https://sub.example.com/clash.yaml";
    std::fs::write(
        paths.config.join("config.yaml"),
        format!(
            "schema_version: 1\ncore: mihomo\nsubscriptions:\n  sources:\n    - url: {source_url}\n      name: airport\n"
        ),
    )
    .unwrap();
    let body = "proxies:\n  - name: hk-01\n    type: vmess\n    server: a.example.com\n    port: 443\n    uuid: aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\n    alterId: 0\n    cipher: auto\nproxy-groups:\n  - name: \u{81ea}\u{52a8}\u{9009}\u{62e9}\n    type: url-test\n    proxies: [hk-01]\nrules:\n  - MATCH,\u{81ea}\u{52a8}\u{9009}\u{62e9}\n";
    let id = caly_backends::subscription::subscription_id_for_url(source_url);
    let cache_dir = paths.state.join("subscriptions");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        cache_dir.join(crate::client::hex(id.into_bytes())),
        body.as_bytes(),
    )
    .unwrap();
    // 加载 config → declared_groups
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    let config = caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver).unwrap();
    let (groups, _nodes) = crate::entry_tree::declared_groups(&paths, &config, false);
    assert!(
        groups
            .iter()
            .any(|group| group.name == "\u{81ea}\u{52a8}\u{9009}\u{62e9}"),
        "subscription group must surface in the offline tree: {:?}",
        groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
