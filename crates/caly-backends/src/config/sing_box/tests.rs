//! Tests for `config/sing_box.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::core::{CoreNodeRegistry, RegisteredProxy};
use caly_domain::NodeId;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn registry_with_singbox() -> CoreNodeRegistry {
    let registry: CoreNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    if let Ok(mut map) = registry.lock() {
        map.insert(
            NodeId::from_bytes([1; 16]),
            RegisteredProxy {
                landing_group: "AUTO".to_owned(),
                name: "node-1".to_owned(),
                yaml: "- name: node-1\n".to_owned(),
                singbox: Some(
                    "{\"tag\":\"proxy-01010101010101010101010101010101\",\
                     \"type\":\"shadowsocks\",\"server\":\"example.com\",\
                     \"server_port\":8388,\"method\":\"aes-128-gcm\",\
                     \"password\":\"p\"}"
                        .to_owned(),
                ),
                subscription: caly_domain::SubscriptionId::from_bytes([1; 16]),
            },
        );
    }
    registry
}

#[test]
fn document_includes_registry_outbounds_and_selectors() -> Result<(), String> {
    let backend = SingBoxConfigBackend::new(PathBuf::from("/tmp/sing-box.json"))
        .with_registry(registry_with_singbox());
    let bytes = backend.build_document().map_err(|e| format!("{e:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let outbounds = value["outbounds"].as_array().ok_or("no outbounds")?;
    let tags: Vec<&str> = outbounds.iter().filter_map(|o| o["tag"].as_str()).collect();
    assert!(
        tags.contains(&"proxy-01010101010101010101010101010101"),
        "registry outbound must render, got {tags:?}"
    );
    assert!(tags.contains(&"direct"), "direct outbound must render");
    assert!(tags.contains(&"PROXY"), "PROXY selector must render");
    assert!(tags.contains(&"GLOBAL"), "GLOBAL selector must render");
    assert_eq!(
        value["experimental"]["clash_api"]["external_controller"],
        "127.0.0.1:9091"
    );
    Ok(())
}

#[test]
fn empty_registry_renders_direct_only_document() -> Result<(), String> {
    let registry: CoreNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let backend =
        SingBoxConfigBackend::new(PathBuf::from("/tmp/sing-box.json")).with_registry(registry);
    let bytes = backend.build_document().map_err(|e| format!("{e:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let outbounds = value["outbounds"].as_array().ok_or("no outbounds")?;
    assert_eq!(
        outbounds.len(),
        3,
        "direct + PROXY + GLOBAL only: {outbounds:?}"
    );
    Ok(())
}

/// 2026-08-09 规划: author-declared groups render as real sing-box
/// outbounds that rules can steer to by name, with the subscription rule
/// table following the schema's own rules and the group's node members
/// resolved to canonical sing-box tags.
#[test]
fn subscription_groups_render_as_selector_outbounds() -> Result<(), String> {
    let document = "proxies:\n  - name: node-1\n    type: ss\n    server: example.com\n    port: 8388\n    cipher: aes-128-gcm\n    password: p\nproxy-groups:\n  - {name: 节点选择, type: select, proxies: [node-1]}\nrules:\n  - 'DOMAIN-SUFFIX,example.com,节点选择'\n  - 'MATCH,节点选择'\n";
    let routing = crate::core::shared_routing_registry();
    let id = caly_domain::SubscriptionId::from_bytes([1; 16]);
    let (groups, rules) = caly_subscription::clash_routing_from_body(document.as_bytes(), id)
        .ok_or("clash routing should parse")?;
    if let Ok(mut store) = routing.lock() {
        store.insert(id, crate::core::SubscriptionRouting { groups, rules });
    }
    let backend = SingBoxConfigBackend::new(PathBuf::from("/tmp/sing-box.json"))
        .with_registry(registry_with_singbox())
        .with_routing(routing)
        .with_rules(vec![caly_domain::RoutingRule::from_clash_line(
            "DOMAIN,internal.lan,DIRECT",
        )
        .map_err(|e| format!("schema rule should parse: {e:?}"))?]);
    let bytes = backend.build_document().map_err(|e| format!("{e:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let outbounds = value["outbounds"].as_array().ok_or("no outbounds")?;
    let group = outbounds
        .iter()
        .find(|outbound| outbound["tag"].as_str() == Some("节点选择"))
        .ok_or("select group outbound missing")?;
    assert_eq!(group["type"].as_str(), Some("selector"));
    // The node member resolved to the canonical sing-box tag.
    let members: Vec<&str> = group["outbounds"]
        .as_array()
        .map(|members| members.iter().filter_map(|m| m.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(members, vec!["proxy-01010101010101010101010101010101"]);
    // The author's MATCH became route.final — never a dangling tag.
    assert_eq!(value["route"]["final"].as_str(), Some("节点选择"));
    // Group-targeted rules steer to the group tag verbatim; the schema rule
    // stays in front of the subscription rule.
    let rules_json = value["route"]["rules"].as_array().ok_or("no route rules")?;
    let rendered = format!("{rules_json:?}");
    let schema_rule = rendered.find("internal.lan").ok_or("schema rule missing")?;
    let subscription_rule = rendered
        .find("example.com")
        .ok_or("subscription rule missing")?;
    assert!(schema_rule < subscription_rule, "order: {rendered}");
    let group_rule = rules_json
        .iter()
        .find(|rule| rule["outbound"].as_str() == Some("节点选择"))
        .ok_or("group-targeted rule missing")?;
    assert_eq!(
        group_rule["domain_suffix"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(|v| v.as_str()),
        Some("example.com")
    );
    Ok(())
}
