//! Full-document Clash YAML import tests driven by a real-world airport
//! subscription (`fixtures/clash-airport-full.yaml`): 30 vmess/ws nodes,
//! 16 proxy groups (incl. a `url-test` with `interval`/`tolerance`) and the
//! complete Clash rule-table shape. Guards the field coverage an operator
//! expects when importing such a file — previously `proxy-groups:` and
//! `rules:` were silently dropped.

use caly_backends::subscription::render_compose::uri_body_to_sing_box_outbound_map;
use caly_coreconf::{
    mihomo::proxy_sections::render_proxy_sections_with_subscription_groups,
    sing_box::proxy_group_to_sing_box_outbound,
};
use caly_domain::{
    Protocol, ProxyGroupMember, ProxyGroupType, RuleMatch, RulePolicy, SubscriptionId, Transport,
    VmessCipher,
};
use caly_subscription::{ClashParseError, parse_clash_config, parse_clash_yaml};

const FIXTURE: &str = include_str!("../../../fixtures/clash-airport-full.yaml");

fn subscription() -> SubscriptionId {
    SubscriptionId::from_bytes([7; 16])
}

#[test]
fn parses_all_thirty_vmess_ws_nodes() -> Result<(), String> {
    let import = parse_clash_config(FIXTURE, subscription()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(import.proxies.len(), 30, "every proxies entry must parse");

    let first = &import.proxies[0];
    // The airport's marquee entry names carry a full-width colon and spaces;
    // the display name must survive untouched.
    let shown = first
        .display(true, None)
        .map_err(|e| format!("display: {e:?}"))?;
    assert_eq!(shown.name().as_str(), "剩余流量：-0.28 GB");

    for (index, node) in import.proxies.iter().enumerate() {
        match node.protocol() {
            Protocol::Vmess {
                user_id,
                alter_id,
                security,
            } => {
                user_id.with_exposed(|uuid| {
                    assert_eq!(
                        uuid, "03ec34a7-a95b-4646-99b6-8957c530add1",
                        "node {index} uuid"
                    );
                });
                assert_eq!(*alter_id, 0, "node {index} alterId");
                assert_eq!(*security, VmessCipher::Auto, "node {index} cipher");
            }
            other => return Err(format!("node {index}: expected vmess, got {other:?}")),
        }
        match node.transport() {
            Some(Transport::WebSocket {
                path,
                host,
                early_data,
            }) => {
                assert_eq!(path.as_str(), "/", "node {index} ws path");
                assert_eq!(
                    host.as_ref().map(caly_domain::BoundedText::as_str),
                    Some("4e4c424e04ff1def17f1a9e45108cc00.mobgslb.tbcache.com"),
                    "node {index} ws Host header"
                );
                assert!(early_data.is_none(), "Clash ws has no early data");
            }
            other => {
                return Err(format!(
                    "node {index}: expected ws transport, got {other:?}"
                ));
            }
        }
    }
    // Port spread is the only structural difference between the tiers.
    assert_eq!(import.proxies[3].endpoint().port().get(), 16617);
    assert_eq!(import.proxies[19].endpoint().port().get(), 16648);
    assert_eq!(import.proxies[28].endpoint().port().get(), 16645);
    Ok(())
}

#[test]
fn parses_all_sixteen_proxy_groups() -> Result<(), String> {
    let import = parse_clash_config(FIXTURE, subscription()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(import.proxy_groups.len(), 16, "every proxy-groups entry");

    let first = &import.proxy_groups[0];
    assert_eq!(first.name.as_str(), "节点选择");
    assert_eq!(first.kind, ProxyGroupType::Select);
    assert_eq!(first.members.len(), 31);
    // Forward reference: 自动选择 is declared AFTER 节点选择 in the file and
    // must still classify as a nested group, not a node tag.
    assert!(
        matches!(&first.members[0], ProxyGroupMember::Group { name } if name.as_str() == "自动选择"),
        "forward group reference must classify as Group: {:?}",
        first.members[0]
    );
    assert!(
        matches!(&first.members[1], ProxyGroupMember::Node { tag } if tag.as_str() == "剩余流量：-0.28 GB"),
        "node member must classify as Node: {:?}",
        first.members[1]
    );
    assert!(first.url_test.is_none(), "select groups carry no probe");

    let autoselect = &import.proxy_groups[1];
    assert_eq!(autoselect.name.as_str(), "自动选择");
    assert_eq!(autoselect.kind, ProxyGroupType::UrlTest);
    assert_eq!(autoselect.members.len(), 15);
    let probe = autoselect
        .url_test
        .as_ref()
        .ok_or_else(|| "url-test group must keep its probe".to_owned())?;
    assert_eq!(probe.url.as_str(), "http://www.YouTube.com");
    assert_eq!(probe.interval_seconds, 600);
    assert_eq!(probe.tolerance_ms, 200);

    // `磁力下载` is DIRECT-only — the built-in token classifies as Direct.
    let magnet = &import.proxy_groups[4];
    assert_eq!(magnet.name.as_str(), "磁力下载");
    assert_eq!(magnet.members, vec![ProxyGroupMember::Direct]);

    // Every member of every group must be one of the three legal shapes and
    // every Node/Group name must be non-empty (bounded text guarantees).
    for group in &import.proxy_groups {
        assert!(
            !group.members.is_empty(),
            "{} must not render an empty selector",
            group.name.as_str()
        );
    }
    Ok(())
}

#[test]
fn parses_the_complete_rule_table() -> Result<(), String> {
    let import = parse_clash_config(FIXTURE, subscription()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(import.rules.len(), 85, "every rules entry must parse");

    // First rule: DOMAIN with DIRECT policy.
    assert!(matches!(
        &import.rules[0].matcher,
        RuleMatch::Domain(value) if value.as_str() == "mojie.me"
    ));
    assert_eq!(import.rules[0].policy, RulePolicy::Direct);

    // REJECT policy survives (adblock / loopback sinks in the table).
    let reject_count = import
        .rules
        .iter()
        .filter(|rule| rule.policy == RulePolicy::Reject)
        .count();
    assert_eq!(reject_count, 2, "REJECT,e.crashlytics.com + loopback sink");

    // GEOIP extension codes are preserved verbatim (lan / telegram / CN).
    let geoip_codes: Vec<&str> = import
        .rules
        .iter()
        .filter_map(|rule| match &rule.matcher {
            RuleMatch::Geoip(code) => Some(code.as_str()),
            _ => None,
        })
        .collect();
    for expected in ["lan", "telegram", "CN"] {
        assert!(
            geoip_codes.contains(&expected),
            "GEOIP,{expected} must parse, got {geoip_codes:?}"
        );
    }

    // Flags: no-resolve set exactly where written; GEOIP,CN,DIRECT (flagless)
    // is a distinct rule from GEOIP,CN,DIRECT,no-resolve.
    let flagless_cn = import.rules.iter().any(|rule| {
        matches!(&rule.matcher, RuleMatch::Geoip(code) if code.as_str() == "CN")
            && !rule.flags.no_resolve
    });
    let flagged_cn = import.rules.iter().any(|rule| {
        matches!(&rule.matcher, RuleMatch::Geoip(code) if code.as_str() == "CN")
            && rule.flags.no_resolve
    });
    assert!(flagless_cn && flagged_cn, "both GEOIP,CN flag shapes parse");

    // PROCESS-NAME entries keep the process pattern.
    let processes: Vec<&str> = import
        .rules
        .iter()
        .filter_map(|rule| match &rule.matcher {
            RuleMatch::ProcessName(value) => Some(value.as_str()),
            _ => None,
        })
        .collect();
    for expected in ["Thunder", "aria2c.exe", "onedriveupdater"] {
        assert!(processes.contains(&expected), "missing {expected}");
    }

    // IP-CIDR6 parses into the same matcher family as v4.
    assert!(import.rules.iter().any(|rule| {
        matches!(&rule.matcher, RuleMatch::IpCidr(value) if value.as_str() == "fe80::/10")
    }));

    // MATCH anchors the table and points at the declared group.
    let last = import.rules.last().ok_or("rules is empty")?;
    assert!(matches!(last.matcher, RuleMatch::Match));
    assert!(
        matches!(&last.policy, RulePolicy::Proxy(target) if target.as_str() == "节点选择"),
        "MATCH must reference the declared group"
    );

    parse_clash_yaml(FIXTURE, subscription()).map_err(|e| format!("{e:?}"))?;
    Ok(())
}

#[test]
fn every_grouped_policy_target_resolves() -> Result<(), String> {
    let import = parse_clash_config(FIXTURE, subscription()).map_err(|e| format!("{e:?}"))?;
    let group_names: Vec<&str> = import
        .proxy_groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    for (index, rule) in import.rules.iter().enumerate() {
        if let RulePolicy::Proxy(target) = &rule.policy {
            assert!(
                group_names.contains(&target.as_str()),
                "rule {index} policy `{}` must be a declared group",
                target.as_str()
            );
        }
    }
    Ok(())
}

#[test]
fn scale_far_beyond_the_original_table_is_fine() -> Result<(), String> {
    // The source document ships thousands of near-identical DOMAIN-SUFFIX
    // rows; the fixture keeps the distinct shapes and this test proves the
    // repetitive volume itself is no problem.
    use std::fmt::Write as _;
    let mut document = String::from("proxies: []\nproxy-groups: []\nrules:\n");
    for index in 0..3_000 {
        let _ = writeln!(
            document,
            "  - 'DOMAIN-SUFFIX,host{index}.example.com,DIRECT'"
        );
    }
    document.push_str("  - 'MATCH,DIRECT'\n");
    let import = parse_clash_config(&document, subscription()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(import.rules.len(), 3_001);
    Ok(())
}

#[test]
fn unknown_rule_policy_is_rejected_loudly() {
    let yaml = "proxies: []\nproxy-groups: []\nrules:\n  - 'MATCH,不存在的组'\n";
    assert_eq!(
        parse_clash_config(yaml, subscription()).err(),
        Some(ClashParseError::UnknownRulePolicy { index: 0 })
    );
}

#[test]
fn rule_policy_may_target_a_node_name() -> Result<(), String> {
    let yaml = "proxies:\n  - { name: 落地A, type: vmess, server: a.example.com, port: 443, uuid: 03ec34a7-a95b-4646-99b6-8957c530add1, alterId: 0, cipher: auto, network: ws }\nproxy-groups: []\nrules:\n  - 'MATCH,落地A'\n";
    let import = parse_clash_config(yaml, subscription()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(import.rules.len(), 1);
    Ok(())
}

#[test]
fn duplicate_group_names_are_rejected() {
    let yaml = "proxies: []\nproxy-groups:\n  - { name: A, type: select, proxies: [DIRECT] }\n  - { name: A, type: select, proxies: [DIRECT] }\n";
    assert_eq!(
        parse_clash_config(yaml, subscription()).err(),
        Some(ClashParseError::DuplicateGroupName { index: 1 })
    );
}

#[test]
fn non_http_probe_url_is_rejected() {
    let yaml = "proxies: []\nproxy-groups:\n  - { name: A, type: url-test, proxies: [DIRECT], url: 'ftp://example.com/x' }\n";
    assert_eq!(
        parse_clash_config(yaml, subscription()).err(),
        Some(ClashParseError::InvalidProbeUrl { index: 0 })
    );
}

#[test]
fn probe_defaults_mirror_mihomo() -> Result<(), String> {
    // A url-test group without url/interval/tolerance gets the kernel's own
    // defaults (gstatic 204, 300 s, 50 ms).
    let yaml = "proxies: []\nproxy-groups:\n  - { name: A, type: url-test, proxies: [DIRECT] }\n";
    let import = parse_clash_config(yaml, subscription()).map_err(|e| format!("{e:?}"))?;
    let probe = import.proxy_groups[0]
        .url_test
        .as_ref()
        .ok_or_else(|| "probe must default in".to_owned())?;
    assert_eq!(probe.url.as_str(), "http://www.gstatic.com/generate_204");
    assert_eq!(probe.interval_seconds, 300);
    assert_eq!(probe.tolerance_ms, 50);
    Ok(())
}

#[test]
fn unsupported_group_type_is_rejected() {
    let yaml = "proxies: []\nproxy-groups:\n  - { name: A, type: smart, proxies: [DIRECT] }\n";
    assert_eq!(
        parse_clash_config(yaml, subscription()).err(),
        Some(ClashParseError::UnsupportedGroupType { index: 0 })
    );
}

/// 2026-08-09 规划 smoke test: the airport document's groups render
/// verbatim into both kernels — mihomo keeps declaration order with probe
/// parameters intact, sing-box resolves every node member of every group to
/// a canonical outbound tag.
#[test]
fn airport_groups_render_verbatim_into_both_kernels() {
    let body = include_str!("../../../fixtures/clash-airport-full.yaml");
    let id = subscription();
    let import = parse_clash_config(body, id)
        .map_err(|e| format!("{e:?}"))
        .ok()
        .filter(|import| !import.proxy_groups.is_empty())
        .unwrap_or_else(|| unreachable!("airport fixture parses with groups"));
    let fallback = import.proxy_groups[0].name.as_str();
    assert_eq!(fallback, "节点选择");

    // mihomo: every declared group renders, probe parameters survive, and
    // the author MATCH means no fallback is appended.
    let section = render_proxy_sections_with_subscription_groups(
        &[],
        &import.rules,
        &[],
        &import.proxy_groups,
        fallback,
    );
    for declared in &import.proxy_groups {
        let name = declared.name.as_str();
        assert!(section.contains(name), "mihomo section lost group {name}");
    }
    assert!(section.contains("interval: 600"), "probe interval lost");
    assert!(section.contains("tolerance: 200"), "probe tolerance lost");
    assert!(section.contains("MATCH,节点选择"), "author MATCH lost");
    assert!(!section.contains("generate_204"), "implicit probe leaked");

    // sing-box: every node member of every group resolves through the
    // canonical outbound map built from the same document.
    let (outbounds, _skipped) = uri_body_to_sing_box_outbound_map(body.as_bytes(), id)
        .map_err(|e| format!("{e:?}"))
        .ok()
        .unwrap_or_else(|| unreachable!("airport fixture renders outbounds"));
    let name_to_tag: std::collections::BTreeMap<String, String> = import
        .proxies
        .iter()
        .filter_map(|node| {
            let name = node.display(true, None).ok()?.name().as_str().to_owned();
            let outbound = outbounds.get(&node.id())?;
            let value: serde_json::Value = serde_json::from_str(outbound).ok()?;
            Some((name, value["tag"].as_str()?.to_owned()))
        })
        .collect();
    let resolve = |name: &str| name_to_tag.get(name).cloned();
    let mut rendered_groups = 0_usize;
    for declared in &import.proxy_groups {
        let Some(outbound) = proxy_group_to_sing_box_outbound(declared, &resolve) else {
            continue;
        };
        rendered_groups += 1;
        let members = outbound["outbounds"].as_array().map_or(0, Vec::len);
        assert!(members > 0, "{} rendered with zero members", declared.name);
    }
    // 磁力下载 is DIRECT-only; every other group keeps ≥1 member, so all 16
    // render (磁力下载's DIRECT member counts too).
    assert_eq!(rendered_groups, 16, "groups lost in sing-box render");
}
