//! Golden byte snapshots for every sing-box renderer in `caly-coreconf`.
//!
//! Each case renders through the public API and compares the exact output
//! bytes against `tests/golden/<case>.snap`. Regenerate after an intentional
//! renderer change with `CALY_GOLDEN_BLESS=1 cargo test -p caly-coreconf
//! --test render_golden`, then review the fixture diff — every changed case
//! must carry a numbered semantic-equivalence argument (docs/crate-replan.md
//! P3b/P4 exit gates).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // #53: golden harness asserts on render success

use std::path::PathBuf;

use caly_coreconf::rules::{SingBoxRules, render_sing_box_rules};
use caly_coreconf::sing_box::{
    SingBoxBaseTuning, SingBoxConfigRenderer, SingBoxRenderTuning, SniffOptions,
    node_to_json_string, nodes_to_outbounds, proxy_group_to_sing_box_outbound, sing_box_document,
};
use caly_dns::DnsSettingsBuilder;
use caly_domain::{RoutingRule, SubscriptionId, TunConfig, TunStack};
use caly_subscription::parse_any_proxy_uri;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Renders every case (name, exact bytes). Error variants are folded into
/// text so unsupported-input cases still snapshot deterministically.
fn cases() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    node_cases(&mut out);
    group_cases(&mut out);
    rules_cases(&mut out);
    dns_cases(&mut out);
    document_cases(&mut out);
    tuned_cases(&mut out);
    out
}

fn push(out: &mut Vec<(String, Vec<u8>)>, name: &str, bytes: impl Into<Vec<u8>>) {
    out.push((name.to_owned(), bytes.into()));
}

// ---------------------------------------------------------------- nodes

fn node(out: &mut Vec<(String, Vec<u8>)>, name: &str, uri: &str) {
    let parsed = parse_any_proxy_uri(uri, SubscriptionId::from_bytes([9; 16]));
    let bytes = match parsed {
        Ok(node) => match node_to_json_string(&node) {
            Ok(json) => json.into_bytes(),
            Err(error) => format!("ERR {error}").into_bytes(),
        },
        Err(error) => format!("PARSE-ERR {error:?}").into_bytes(),
    };
    push(out, name, bytes);
}

fn node_cases(out: &mut Vec<(String, Vec<u8>)>) {
    node(
        out,
        "node-vless-reality-ws",
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&flow=xtls-rprx-vision&sni=example.com&fp=chrome&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk&sid=afeed89ae23b36ed&type=ws&path=%2Fstream&host=example.com#reality-ws",
    );
    node(
        out,
        "node-vless-tls-plain",
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls#vless-tls",
    );
    node(
        out,
        "node-vmess-ws",
        "vmess://eyJhZGQiOiJleGFtcGxlLmNvbSIsInBvcnQiOjQ0MywiaWQiOiI2YTg5YTIxNS0yMmJmLTRiZDctOTY0Mi1iOTViMmUwOTU4M2EiLCJhaWQiOjAsIm5ldCI6IndzIiwicGF0aCI6Ii9zIiwiaG9zdCI6ImV4YW1wbGUuY29tIiwidGxzIjoidGxzIiwicHMiOiJ2bWVzLW5vZGUifQ==",
    );
    node(
        out,
        "node-trojan-ws",
        "trojan://secret-pass@example.com:443?security=tls&sni=example.com&type=ws&path=%2Fstream#trojan-ws",
    );
    node(
        out,
        "node-ss-chacha20",
        "ss://Y2hhY2hhMjAtaWV0Zi1wb2x5MTMwNTpzZWNyZXRwYXNz@example.com:8443#ss-node",
    );
    node(
        out,
        "node-ss-aes128gcm",
        "ss://YWVzLTEyOC1nY206cGFzc3dvcmQ=@example.com:8388#ss-aes",
    );
    node(
        out,
        "node-hysteria2-obfs",
        "hysteria2://secret@example.com:443?security=tls&sni=example.com&obfs=salamander&obfs-password=obfspass&upmbps=50&downmbps=100#hy2",
    );
    node(
        out,
        "node-tuic-bbr",
        "tuic://2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365:hello@example.com:443?congestion_control=bbr#tuic",
    );
    node(
        out,
        "node-vless-grpc",
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&sni=example.com&type=grpc&serviceName=grpc-svc#grpc",
    );
    node(
        out,
        "node-vless-insecure",
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&sni=example.com&allowInsecure=1#insecure",
    );
    node(
        out,
        "node-ss-unsupported-cipher",
        "ss://YWVzLTEyOC1jZmI6cGFzc3dvcmQ=@example.com:8388#ss-cfb",
    );
}

// ---------------------------------------------------------------- groups

fn group(
    out: &mut Vec<(String, Vec<u8>)>,
    name: &str,
    kind: caly_domain::ProxyGroupType,
    members: Vec<caly_domain::ProxyGroupMember>,
    probe: Option<caly_domain::UrlTestConfig>,
) {
    let group = caly_domain::ProxyGroup {
        name: caly_domain::ProxyGroupName::new(name.to_owned()).unwrap(),
        kind,
        members,
        url_test: probe,
    };
    let resolve = |tag: &str| (tag == "node-a").then(|| "proxy-1".to_owned());
    let bytes = match proxy_group_to_sing_box_outbound(&group, &resolve) {
        Some(value) => serde_json::to_string(&value).unwrap().into_bytes(),
        None => b"OMITTED".to_vec(),
    };
    push(out, &format!("group-{name}"), bytes);
}

fn member_node(tag: &str) -> caly_domain::ProxyGroupMember {
    caly_domain::ProxyGroupMember::Node {
        tag: caly_domain::ProxyGroupNodeTag::new(tag.to_owned()).unwrap(),
    }
}

fn group_cases(out: &mut Vec<(String, Vec<u8>)>) {
    group(
        out,
        "select",
        caly_domain::ProxyGroupType::Select,
        vec![
            member_node("node-a"),
            caly_domain::ProxyGroupMember::Direct,
            caly_domain::ProxyGroupMember::Reject,
        ],
        None,
    );
    let probe = || {
        Some(caly_domain::UrlTestConfig {
            url: caly_domain::ProxyGroupUrl::new("https://cp.cloudflare.com/".to_owned()).unwrap(),
            interval_seconds: 600,
            tolerance_ms: 200,
        })
    };
    group(
        out,
        "urltest",
        caly_domain::ProxyGroupType::UrlTest,
        vec![member_node("node-a")],
        probe(),
    );
    group(
        out,
        "fallback",
        caly_domain::ProxyGroupType::Fallback,
        vec![member_node("node-a"), caly_domain::ProxyGroupMember::Direct],
        None,
    );
    group(
        out,
        "relay",
        caly_domain::ProxyGroupType::Relay,
        vec![member_node("node-a")],
        None,
    );
    group(
        out,
        "empty-dropped",
        caly_domain::ProxyGroupType::Select,
        vec![member_node("no-such-node")],
        None,
    );
}

// ---------------------------------------------------------------- rules

fn rules_case(
    out: &mut Vec<(String, Vec<u8>)>,
    name: &str,
    rules: &[RoutingRule],
    providers: &[caly_domain::RuleProvider],
    proxy_representable: bool,
    groups: &[&str],
) {
    let group_tags: std::collections::BTreeSet<String> =
        groups.iter().map(|tag| (*tag).to_owned()).collect();
    let rendered: SingBoxRules =
        render_sing_box_rules(rules, providers, proxy_representable, &group_tags);
    // P3b: the renderer hands out typed rule/rule-set lists; the snapshot
    // serializes them (alphabetical field order = historical byte order).
    let rules_text = if rendered.rules.is_empty() {
        "-".to_owned()
    } else {
        serde_json::to_string(&rendered.rules).unwrap()
    };
    let rule_sets_text = if rendered.rule_sets.is_empty() {
        "-".to_owned()
    } else {
        serde_json::to_string(&rendered.rule_sets).unwrap()
    };
    let text = format!(
        "RULES={rules_text}\nRULE_SETS={rule_sets_text}\nFINAL={}\nBLOCK={}\nSKIPPED={}",
        rendered.final_outbound, rendered.block_outbound, rendered.skipped
    );
    push(out, &format!("rules-{name}"), text.into_bytes());
}

fn clash(line: &str) -> RoutingRule {
    RoutingRule::from_clash_line(line).unwrap()
}

fn provider(
    name: &str,
    behavior: caly_domain::RuleProviderBehavior,
    source: caly_domain::RuleProviderSource,
) -> caly_domain::RuleProvider {
    caly_domain::RuleProvider {
        name: caly_domain::RuleProviderName::new(name.to_owned()).unwrap(),
        source,
        behavior,
        format: caly_domain::RuleProviderFormat::Source,
    }
}

fn rules_cases(out: &mut Vec<(String, Vec<u8>)>) {
    rules_case(
        out,
        "matchers",
        &[
            clash("DOMAIN,a.example,PROXY"),
            clash("DOMAIN-SUFFIX,b.example,DIRECT"),
            clash("DOMAIN-KEYWORD,ads,REJECT"),
            clash("IP-CIDR,192.168.0.0/16,DIRECT"),
            clash("GEOIP,CN,DIRECT"),
            clash("GEOSITE,cn,PROXY"),
            clash("PROCESS-NAME,chrome,DIRECT"),
        ],
        &[],
        true,
        &[],
    );
    rules_case(out, "empty", &[], &[], true, &[]);
    rules_case(
        out,
        "match-catch-all",
        &[
            clash("DOMAIN-SUFFIX,google.com,PROXY"),
            clash("MATCH,GLOBAL"),
        ],
        &[],
        true,
        &[],
    );
    rules_case(
        out,
        "no-proxy-outbounds",
        &[
            clash("DOMAIN-SUFFIX,google.com,PROXY"),
            clash("IP-CIDR,10.0.0.0/8,DIRECT"),
        ],
        &[],
        false,
        &[],
    );
    rules_case(
        out,
        "group-policies",
        &[
            clash("DOMAIN-SUFFIX,a.example,MyGroup"),
            clash("DOMAIN-SUFFIX,b.example,GLOBAL"),
            clash("DOMAIN-SUFFIX,c.example,Undeclared"),
        ],
        &[],
        true,
        &["MyGroup"],
    );
    rules_case(
        out,
        "providers",
        &[clash("RULE-SET,my-google,PROXY")],
        &[
            provider(
                "my-google",
                caly_domain::RuleProviderBehavior::Domain,
                caly_domain::RuleProviderSource::Http {
                    url: caly_domain::RuleText::new("https://example.com/g.yaml".to_owned())
                        .unwrap(),
                    interval_ms: 86_400_000,
                },
            ),
            provider(
                "local",
                caly_domain::RuleProviderBehavior::Classical,
                caly_domain::RuleProviderSource::File {
                    path: caly_domain::RuleText::new("/etc/caly/local.yaml".to_owned()).unwrap(),
                },
            ),
            provider(
                "inline",
                caly_domain::RuleProviderBehavior::Domain,
                caly_domain::RuleProviderSource::Inline {
                    payload: caly_domain::BoundedText::new(
                        "# comment\n\nexact.example.com\n.suffix.example.org\n".to_owned(),
                    )
                    .unwrap(),
                },
            ),
        ],
        true,
        &[],
    );
    rules_case(
        out,
        "provider-override-autotag",
        &[clash("GEOIP,CN,DIRECT")],
        &[provider(
            "geoip-cn",
            caly_domain::RuleProviderBehavior::IpCidr,
            caly_domain::RuleProviderSource::Inline {
                payload: caly_domain::BoundedText::new("1.2.3.0/24\n".to_owned()).unwrap(),
            },
        )],
        true,
        &[],
    );
}

// ---------------------------------------------------------------- dns

// P3b: the raw `render_dns_fragment` entry point was retired with the typed
// model. DNS cases now snapshot the subscription-less tuned document the
// settings are embedded into (the resolver wiring shows up as
// `route.default_domain_resolver`), which covers the same rendering path
// consumers actually use.
fn dns(out: &mut Vec<(String, Vec<u8>)>, name: &str, dns: &caly_dns::DnsSettings) {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer
        .render_tuned(&SingBoxBaseTuning {
            dns: Some(dns.clone()),
            ..SingBoxBaseTuning::default()
        })
        .unwrap();
    renderer.validate_bytes(&bytes).unwrap();
    push(out, &format!("dns-{name}"), bytes);
}

fn dns_cases(out: &mut Vec<(String, Vec<u8>)>) {
    let fakeip = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(caly_dns::DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")
        .unwrap()
        .push_nameserver("1.1.1.1")
        .unwrap()
        .push_fallback("tls://dns.google")
        .unwrap()
        .push_direct("223.5.5.5")
        .unwrap()
        .fake_ip_range("198.18.0.1/16")
        .unwrap()
        .build()
        .unwrap()
        .unwrap();
    dns(out, "fakeip", &fakeip);

    let plain = DnsSettingsBuilder::new()
        .enabled(true)
        .push_nameserver("8.8.8.8")
        .unwrap()
        .build()
        .unwrap()
        .unwrap();
    dns(out, "plain-udp", &plain);

    let domain_hosted = DnsSettingsBuilder::new()
        .enabled(true)
        .push_nameserver("https://dns.google/dns-query")
        .unwrap()
        .push_default("223.5.5.5")
        .unwrap()
        .build()
        .unwrap()
        .unwrap();
    dns(out, "domain-hosted", &domain_hosted);

    let local_only = DnsSettingsBuilder::new()
        .enabled(true)
        .push_nameserver("local")
        .unwrap()
        .build()
        .unwrap()
        .unwrap();
    dns(out, "local-only", &local_only);
}

// ---------------------------------------------------------------- document

fn sing_box_doc(
    out: &mut Vec<(String, Vec<u8>)>,
    name: &str,
    tuning: &SingBoxRenderTuning,
    uris: &[&str],
) {
    let nodes: Vec<caly_domain::DialableNode> = uris
        .iter()
        .map(|uri| parse_any_proxy_uri(uri, SubscriptionId::from_bytes([5; 16])).unwrap())
        .collect();
    let outbounds = nodes_to_outbounds(nodes).unwrap();
    let bytes = match sing_box_document(tuning, outbounds) {
        Ok(json) => json,
        Err(error) => format!("ERR {error}").into_bytes(),
    };
    push(out, &format!("doc-{name}"), bytes);
}

fn document_cases(out: &mut Vec<(String, Vec<u8>)>) {
    sing_box_doc(out, "standard-empty", &SingBoxRenderTuning::standard(), &[]);
    sing_box_doc(
        out,
        "two-nodes",
        &SingBoxRenderTuning::standard(),
        &[
            "trojan://secret-pass@example.com:443?security=tls&sni=example.com&type=ws&path=%2Fstream#trojan-ws",
            "ss://YWVzLTEyOC1nY206cGFzc3dvcmQ=@example.com:8388#ss-aes",
        ],
    );
    let fakeip = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(caly_dns::DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")
        .unwrap()
        .push_fallback("tls://dns.google")
        .unwrap()
        .fake_ip_range("198.18.0.1/16")
        .unwrap()
        .build()
        .unwrap()
        .unwrap();
    let rules = render_sing_box_rules(
        &[
            clash("DOMAIN-SUFFIX,google.com,PROXY"),
            clash("DOMAIN-KEYWORD,ad,REJECT"),
            clash("IP-CIDR,192.168.0.0/16,DIRECT"),
            clash("GEOIP,CN,DIRECT"),
            clash("MATCH,PROXY"),
        ],
        &[],
        true,
        &std::collections::BTreeSet::new(),
    );
    let mut full = SingBoxRenderTuning {
        controller: "127.0.0.1:9291".to_owned(),
        secret: "topsecret".to_owned(),
        log_level: "warn".to_owned(),
        mixed_port: 8899,
        allow_lan: false,
        bind_address: "*".to_owned(),
        tun: Some(TunConfig::new(TunStack::Gvisor, true, true, 1400).unwrap()),
        tun_interface: "caly0".to_owned(),
        dns: Some(fakeip),
        sniff: true,
        sniff_override_destination: true,
        route_rules: rules.rules,
        rule_sets: rules.rule_sets,
        route_final: rules.final_outbound,
        block_outbound: rules.block_outbound,
    };
    sing_box_doc(
        out,
        "full-tuning",
        &full,
        &["vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls#vless-tls"],
    );
    "block".clone_into(&mut full.route_final);
    full.block_outbound = true;
    sing_box_doc(out, "block-final", &full, &[]);
}

// ---------------------------------------------------------------- base (render_tuned)

fn tuned(out: &mut Vec<(String, Vec<u8>)>, name: &str, tuning: &SingBoxBaseTuning) {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer.render_tuned(tuning).unwrap();
    renderer.validate_bytes(&bytes).unwrap();
    push(out, &format!("tuned-{name}"), bytes);
}

fn tuned_cases(out: &mut Vec<(String, Vec<u8>)>) {
    tuned(out, "default", &SingBoxBaseTuning::default());
    let fakeip = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(caly_dns::DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")
        .unwrap()
        .push_fallback("tls://dns.google")
        .unwrap()
        .fake_ip_range("198.18.0.1/16")
        .unwrap()
        .build()
        .unwrap()
        .unwrap();
    let rules = render_sing_box_rules(
        &[
            clash("DOMAIN-SUFFIX,example.com,DIRECT"),
            clash("GEOIP,CN,DIRECT"),
        ],
        &[],
        false,
        &std::collections::BTreeSet::new(),
    );
    tuned(
        out,
        "full",
        &SingBoxBaseTuning {
            controller: "127.0.0.1:9091".to_owned(),
            dns: Some(fakeip),
            secret: Some("abc123".to_owned()),
            log_level: "warn".to_owned(),
            mixed_port: 7890,
            allow_lan: true,
            bind_address: "192.168.1.100".to_owned(),
            tun: Some(TunConfig::new(TunStack::System, true, true, 1_400).unwrap()),
            tun_interface: "caly0".to_owned(),
            transparent_port: 7892,
            transparent_tproxy: true,
            sniff: SniffOptions {
                enabled: true,
                override_destination: true,
            },
            route_rules: rules.rules,
            rule_sets: rules.rule_sets,
            route_final: rules.final_outbound,
            block_outbound: rules.block_outbound,
        },
    );
}

// ---------------------------------------------------------------- harness

#[test]
fn rendered_bytes_match_the_golden_snapshots() {
    let bless = std::env::var_os("CALY_GOLDEN_BLESS").is_some();
    let dir = golden_dir();
    if bless {
        std::fs::create_dir_all(&dir).unwrap();
    }
    let mut failures: Vec<String> = Vec::new();
    for (name, actual) in cases() {
        let path = dir.join(format!("{name}.snap"));
        if bless {
            std::fs::write(&path, &actual).unwrap();
            continue;
        }
        match std::fs::read(&path) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => failures.push(format!(
                "{name}: golden {} bytes != rendered {} bytes",
                expected.len(),
                actual.len()
            )),
            Err(_) => failures.push(format!(
                "{name}: no snapshot (run with CALY_GOLDEN_BLESS=1)"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatch ({} case(s)):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
