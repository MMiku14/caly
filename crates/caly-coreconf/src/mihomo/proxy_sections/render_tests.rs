//! Pure-renderer tests for `proxy_sections.rs` (rules / providers / declared
//! groups assembly), split from the fusion tests that need subscription
//! intake (those live in `caly-backends` next to `render_compose`).

use super::*;

#[test]
fn renders_config_rules_before_match_catch_all() {
    let rules = vec![
        caly_domain::RoutingRule::from_clash_line("DOMAIN-SUFFIX,google.com,PROXY").unwrap(),
        caly_domain::RoutingRule::from_clash_line("GEOIP,CN,DIRECT").unwrap(),
    ];
    let section = render_proxy_sections_with_rules(
        "AUTO",
        &[("node-a", "  - name: node-a\n    type: ss")],
        &rules,
    );
    assert!(section.contains("rules:"));
    assert!(section.contains("  - DOMAIN-SUFFIX,google.com,PROXY"));
    assert!(section.contains("  - GEOIP,CN,DIRECT"));
    // A MATCH catch-all is appended when the rules lack one.
    assert!(section.contains("  - MATCH,AUTO"));
}

#[test]
fn existing_match_rule_suppresses_extra_catch_all() {
    let rules = vec![caly_domain::RoutingRule::from_clash_line("MATCH,PROXY").unwrap()];
    let section = render_proxy_sections_with_rules("AUTO", &[], &rules);
    assert!(section.contains("  - MATCH,PROXY"));
    assert!(!section.contains("MATCH,AUTO"));
}

#[test]
fn rule_providers_http_renders_into_rule_providers_block() {
    use caly_domain::{
        RuleProvider, RuleProviderBehavior, RuleProviderFormat, RuleProviderName,
        RuleProviderSource, RuleText,
    };
    let provider = RuleProvider {
        name: RuleProviderName::new("my-google".to_owned()).unwrap(),
        source: RuleProviderSource::Http {
            url: RuleText::new("https://example.com/g.yaml".to_owned()).unwrap(),
            interval_ms: 86_400_000,
        },
        behavior: RuleProviderBehavior::Domain,
        format: RuleProviderFormat::Source,
    };
    let section = render_proxy_sections_with_rules_and_providers(
        "AUTO",
        &[],
        &[],
        std::slice::from_ref(&provider),
    );
    // The provider block carries the right shape: tag, type, behavior,
    // format, url, and an interval in *seconds* (Mihomo convention).
    assert!(section.contains("rule-providers:"), "section: {section}");
    assert!(section.contains("  \"my-google\":"));
    assert!(section.contains("    type: http"));
    assert!(section.contains("    behavior: domain"));
    assert!(section.contains("    format: source"));
    assert!(section.contains("    url: \"https://example.com/g.yaml\""));
    assert!(section.contains("    interval: 86400"));
}

#[test]
fn rule_providers_file_and_inline_render_with_their_respective_keys() {
    use caly_domain::{
        BoundedText, RuleProvider, RuleProviderBehavior, RuleProviderFormat, RuleProviderName,
        RuleProviderSource, RuleText,
    };
    let file_provider = RuleProvider {
        name: RuleProviderName::new("local".to_owned()).unwrap(),
        source: RuleProviderSource::File {
            path: RuleText::new("/etc/caly/local.yaml".to_owned()).unwrap(),
        },
        behavior: RuleProviderBehavior::Classical,
        format: RuleProviderFormat::Source,
    };
    let inline_provider = RuleProvider {
        name: RuleProviderName::new("inline".to_owned()).unwrap(),
        source: RuleProviderSource::Inline {
            payload: BoundedText::new("ads.example.com\n".to_owned()).unwrap(),
        },
        behavior: RuleProviderBehavior::DomainSuffix,
        format: RuleProviderFormat::Source,
    };
    let section = render_proxy_sections_with_rules_and_providers(
        "AUTO",
        &[],
        &[],
        &[file_provider, inline_provider],
    );
    assert!(section.contains("  \"local\":"));
    assert!(section.contains("    type: file"));
    assert!(section.contains("    path: \"/etc/caly/local.yaml\""));
    assert!(section.contains("  \"inline\":"));
    assert!(section.contains("    type: inline"));
    assert!(section.contains("    payload: \"ads.example.com\\n\""));
}

#[test]
fn rule_providers_section_is_omitted_when_no_providers_declared() {
    // Backwards-compat: the previous 3-arg entry point must not emit an
    // empty `rule-providers:` block.
    let section = render_proxy_sections_with_rules("AUTO", &[], &[]);
    assert!(!section.contains("rule-providers:"));
}

#[test]
fn proxy_groups_render_before_the_implicit_url_test_group() {
    use caly_domain::ProxyGroupNodeTag;
    use caly_domain::{ProxyGroup, ProxyGroupMember, ProxyGroupName, ProxyGroupType};
    let group = ProxyGroup {
        name: ProxyGroupName::new("Proxy".to_owned()).unwrap(),
        kind: ProxyGroupType::Select,
        members: vec![
            ProxyGroupMember::Node {
                tag: ProxyGroupNodeTag::new("hk-1".to_owned()).unwrap(),
            },
            ProxyGroupMember::Direct,
        ],
        url_test: None,
    };
    let section = render_proxy_sections_with_declared_groups("AUTO", &[], &[], &[], &[group]);
    // User-declared `Proxy` group appears before the implicit
    // `AUTO` url-test group. The matchers return `Option`;
    // `unwrap_or(usize::MAX)` is a no-op fallback that keeps
    // the assert meaningful while staying lint-clean
    // (the project denies `clippy::expect_used`).
    let proxy_idx = section.find("name: \"Proxy\"").unwrap_or(usize::MAX);
    let auto_idx = section.find("name: \"AUTO\"").unwrap_or(usize::MAX);
    assert!(proxy_idx < usize::MAX, "user group must render");
    assert!(auto_idx < usize::MAX, "implicit group must render");
    assert!(
        proxy_idx < auto_idx,
        "user group must precede the implicit one"
    );
    assert!(section.contains("    type: select"));
    assert!(section.contains("      - \"hk-1\""));
    assert!(section.contains("      - DIRECT"));
}

#[test]
fn proxy_groups_url_test_renders_probe_block() {
    use caly_domain::{
        ProxyGroup, ProxyGroupMember, ProxyGroupName, ProxyGroupType, ProxyGroupUrl, UrlTestConfig,
    };
    let group = ProxyGroup {
        name: ProxyGroupName::new("Auto".to_owned()).unwrap(),
        kind: ProxyGroupType::UrlTest,
        members: vec![ProxyGroupMember::Direct],
        url_test: Some(UrlTestConfig {
            url: ProxyGroupUrl::new("http://www.gstatic.com/generate_204".to_owned()).unwrap(),
            interval_seconds: 300,
            tolerance_ms: 50,
        }),
    };
    let section = render_proxy_sections_with_declared_groups("AUTO", &[], &[], &[], &[group]);
    assert!(section.contains("    type: url-test"));
    assert!(section.contains("    url: \"http://www.gstatic.com/generate_204\""));
    assert!(section.contains("    interval: 300"));
    assert!(section.contains("    tolerance: 50"));
    assert!(section.contains("      - DIRECT"));
}

#[test]
fn proxy_groups_section_omitted_when_no_groups_declared() {
    let section = render_proxy_sections_with_declared_groups("AUTO", &[], &[], &[], &[]);
    // The block header is always present; with no user
    // groups, only the implicit `AUTO` entry should appear.
    let count = section.matches("- name:").count();
    assert_eq!(count, 1, "only the implicit AUTO group should be present");
    assert!(section.contains("name: \"AUTO\""));
}
