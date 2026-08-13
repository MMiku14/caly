use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
fn temp_dir(tag: &str) -> PathBuf {
    caly_platform::paths::test_helpers::unique_path_under("caly-config-backend", tag)
}
fn registry_with_one() -> crate::core::CoreNodeRegistry {
    let registry: crate::core::CoreNodeRegistry =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    if let Ok(mut map) = registry.lock() {
        map.insert(
                caly_domain::NodeId::from_bytes([1; 16]),
                crate::core::RegisteredProxy {
                    landing_group: "AUTO".to_owned(),
                    name: "node-1".to_owned(),
                    yaml: "    - name: \"node-1\"\n      type: ss\n      server: 1.2.3.4\n      port: 443\n"
                        .to_owned(),
                    singbox: None,
                    subscription: caly_domain::SubscriptionId::from_bytes([1; 16]),
                },
            );
    }
    registry
}
#[test]
fn renders_subscription_driven_proxies_into_committed_config() -> Result<(), String> {
    let dir = temp_dir("render");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let destination = dir.join("mihomo.yaml");
    let mut backend =
        MihomoConfigBackend::new(destination.clone()).with_registry(registry_with_one());
    let prepared = backend
        .parse_and_render(ConfigCandidate { id: [7; 16] })
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(prepared.generation, 1);
    let committed = backend
        .commit_candidate(prepared)
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(committed.generation, 1);
    let text = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    assert!(text.contains("proxies:"), "missing proxies section");
    assert!(text.contains("type: ss"), "missing rendered proxy");
    assert!(
        text.contains("# caly-generation: 1"),
        "missing generation marker"
    );
    let mode = std::fs::metadata(&destination)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "committed config must be owner-only");
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
#[test]
fn base_only_config_has_no_proxies_section() -> Result<(), String> {
    let dir = temp_dir("base");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let destination = dir.join("mihomo.yaml");
    let mut backend = MihomoConfigBackend::new(destination.clone());
    let prepared = backend
        .parse_and_render(ConfigCandidate { id: [8; 16] })
        .map_err(|e| format!("{e:?}"))?;
    let committed = backend
        .commit_candidate(prepared)
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(committed.generation, 1);
    let text = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    assert!(
        !text.contains("proxies:"),
        "base config must not add proxies"
    );
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
#[test]
fn rollback_restores_previous_generation() -> Result<(), String> {
    let dir = temp_dir("rollback");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let destination = dir.join("mihomo.yaml");
    let registry = registry_with_one();
    let mut backend = MihomoConfigBackend::new(destination.clone()).with_registry(registry.clone());
    let first_prepared = backend
        .parse_and_render(ConfigCandidate { id: [1; 16] })
        .map_err(|e| format!("{e:?}"))?;
    let first = backend
        .commit_candidate(first_prepared)
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(first.generation, 1);
    // Clear the registry so generation two is base-only (no proxies).
    if let Ok(mut map) = registry.lock() {
        map.clear();
    }
    let second_prepared = backend
        .parse_and_render(ConfigCandidate { id: [2; 16] })
        .map_err(|e| format!("{e:?}"))?;
    let second = backend
        .commit_candidate(second_prepared)
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(second.generation, 2);
    backend
        .rollback_commit(second)
        .map_err(|e| format!("{e:?}"))?;
    let text = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    assert!(
        text.contains("# caly-generation: 1"),
        "rollback must restore gen 1"
    );
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

fn registry_shuffled(entries: &[(u8, &str, &str)]) -> crate::core::CoreNodeRegistry {
    let registry: crate::core::CoreNodeRegistry =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    if let Ok(mut map) = registry.lock() {
        for (seed, name, yaml) in entries {
            map.insert(
                caly_domain::NodeId::from_bytes([*seed; 16]),
                crate::core::RegisteredProxy {
                    landing_group: "AUTO".to_owned(),
                    name: (*name).to_owned(),
                    yaml: (*yaml).to_owned(),
                    singbox: None,
                    subscription: caly_domain::SubscriptionId::from_bytes([1; 16]),
                },
            );
        }
    }
    registry
}

#[test]
fn rendered_proxy_order_is_deterministic_and_sorted_by_node_id() -> Result<(), String> {
    // Insertion order is deliberately shuffled relative to NodeId order.
    let entries: [(u8, &str, &str); 3] = [
        (3, "node-c", "- name: node-c\n"),
        (1, "node-a", "- name: node-a\n"),
        (2, "node-b", "- name: node-b\n"),
    ];
    let dir = temp_dir("determinism");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut rendered: Vec<String> = Vec::new();
    for round in 0..2 {
        let backend = MihomoConfigBackend::new(dir.join(format!("mihomo-{round}.yaml")))
            .with_registry(registry_shuffled(&entries));
        let settings = backend.build_settings().map_err(|e| format!("{e:?}"))?;
        let section = settings
            .proxies
            .as_ref()
            .map(|text| text.as_str().to_owned())
            .ok_or_else(|| "no proxy section".to_owned())?;
        let a = section.find("node-a").ok_or("missing node-a")?;
        let b = section.find("node-b").ok_or("missing node-b")?;
        let c = section.find("node-c").ok_or("missing node-c")?;
        assert!(a < b && b < c, "proxy order must ascend by NodeId");
        rendered.push(section);
    }
    assert_eq!(rendered[0], rendered[1], "rendering must be reproducible");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Regression test for the 2026-08-09 "subscriptions own the routing"
/// decision: when the routing registry carries author-declared groups, the
/// committed mihomo document renders them verbatim, keeps schema rules in
/// front, and the implicit `url-test` fallback group is gone.
#[test]
fn subscription_groups_own_the_rendered_topology() -> Result<(), String> {
    let document = "proxies:\n  - name: node-a\n    type: vmess\n    server: a.example.net\n    port: 443\n    uuid: 6d5a3f10-5a2e-4a1b-9b0e-2a76b4f25a01\nproxy-groups:\n  - {name: 节点选择, type: select, proxies: [node-a]}\n  - {name: 自动选择, type: url-test, url: 'https://cp.cloudflare.com/', interval: 600, tolerance: 200, proxies: [node-a]}\nrules:\n  - 'DOMAIN-SUFFIX,example.com,节点选择'\n  - 'MATCH,节点选择'\n";
    let routing = crate::core::shared_routing_registry();
    let id = caly_domain::SubscriptionId::from_bytes([9; 16]);
    let (groups, rules) = caly_subscription::clash_routing_from_body(document.as_bytes(), id)
        .ok_or("clash routing should parse")?;
    if let Ok(mut store) = routing.lock() {
        store.insert(id, crate::core::SubscriptionRouting { groups, rules });
    }
    let registry = registry_with_one();
    let backend = MihomoConfigBackend::new(temp_dir("routing").join("mihomo.yaml"))
        .with_registry(registry)
        .with_routing(routing)
        .with_rules(vec![
            caly_domain::RoutingRule::from_clash_line("DOMAIN,internal.lan,DIRECT")
                .map_err(|e| format!("schema rule should parse: {e:?}"))?,
        ]);
    let settings = backend.build_settings().map_err(|e| format!("{e:?}"))?;
    let section = settings
        .proxies
        .as_ref()
        .map(|text| text.as_str().to_owned())
        .ok_or_else(|| "no proxy section".to_owned())?;
    // Author groups render verbatim, including probe parameters.
    assert!(
        section.contains("节点选择"),
        "select group missing:\n{section}"
    );
    assert!(
        section.contains("interval: 600"),
        "probe interval lost:\n{section}"
    );
    assert!(
        section.contains("tolerance: 200"),
        "probe tolerance lost:\n{section}"
    );
    // The implicit AUTO url-test group is NOT emitted for subscription-owned routing.
    assert!(
        !section.contains("\"AUTO\""),
        "implicit group leaked:\n{section}"
    );
    assert!(
        !section.contains("generate_204"),
        "implicit probe leaked:\n{section}"
    );
    // Schema rules keep precedence over the subscription rule table.
    let schema_rule = section
        .find("DOMAIN,internal.lan,DIRECT")
        .ok_or("schema rule missing")?;
    let subscription_rule = section
        .find("DOMAIN-SUFFIX,example.com")
        .ok_or("subscription rule missing")?;
    assert!(
        schema_rule < subscription_rule,
        "schema rules must stay in front:\n{section}"
    );
    assert!(
        section.contains("MATCH,节点选择"),
        "author MATCH lost:\n{section}"
    );
    Ok(())
}
