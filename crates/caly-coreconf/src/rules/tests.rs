//! Tests for `rules.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use caly_domain::{BoundedText, RoutingRule, RuleProviderName, RuleText};

fn parse(line: &str) -> RoutingRule {
    RoutingRule::from_clash_line(line).unwrap()
}

#[test]
fn maps_matchers_onto_sing_box_route_rule_keys() {
    let rules = vec![
        parse("DOMAIN,a.example,PROXY"),
        parse("DOMAIN-SUFFIX,b.example,DIRECT"),
        parse("DOMAIN-KEYWORD,ads,REJECT"),
        parse("IP-CIDR,192.168.0.0/16,DIRECT"),
        parse("GEOIP,CN,DIRECT"),
    ];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(json.contains("\"domain\":[\"a.example\"]"));
    assert!(json.contains("\"domain_suffix\":[\"b.example\"]"));
    assert!(json.contains("\"domain_keyword\":[\"ads\"]"));
    assert!(json.contains("\"ip_cidr\":[\"192.168.0.0/16\"]"));
    // W3a 降级: GEOIP,cn no longer emits a rule_set reference (the
    // SagerNet .srs source is decommissioned); GEOIP,private renders
    // natively as ip_is_private.
    assert!(!json.contains("\"rule_set\":\"geoip-cn\""));
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(
        sources.is_empty() || sources == "[]",
        "GEOIP,cn must not auto-emit a dead SagerNet source: {sources}"
    );
    assert!(rendered.block_outbound);
    assert_eq!(
        rendered.skipped, 1,
        "GEOIP,cn is skipped without a provider"
    );
    assert_eq!(rendered.final_outbound, "PROXY");
}

/// W4-adjacent regression (2026-08-12): a GEOIP/GEOSITE rule whose code
/// matches a *declared* rule-provider tag renders as a `rule_set`
/// reference to that provider instead of being skipped — the comment
/// contract that predated the SagerNet decommissioning, now enforced.
#[test]
fn geoip_geosite_with_matching_provider_renders_as_rule_set_reference() {
    let rules = vec![
        parse("GEOIP,CN,PROXY"),
        parse("GEOSITE,netflix,PROXY"),
        // `GEOIP,private` keeps its native lossless rendering even when a
        // provider happens to share the spelling.
        parse("GEOIP,private,DIRECT"),
    ];
    let providers = vec![
        provider(
            "CN",
            RuleProviderSource::File {
                path: RuleText::new("/tmp/geoip.srs".to_owned()).unwrap(),
            },
            RuleProviderBehavior::IpCidr,
            RuleProviderFormat::Binary,
        ),
        provider(
            "netflix",
            RuleProviderSource::File {
                path: RuleText::new("/tmp/netflix.srs".to_owned()).unwrap(),
            },
            RuleProviderBehavior::Domain,
            RuleProviderFormat::Binary,
        ),
    ];
    let rendered =
        render_sing_box_rules(&rules, &providers, true, &std::collections::BTreeSet::new());
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(
        json.contains("\"rule_set\":\"CN\""),
        "GEOIP,cn with a declared `CN` provider must reference it: {json}"
    );
    assert!(
        json.contains("\"rule_set\":\"netflix\""),
        "GEOSITE,netflix with a declared `netflix` provider must reference it: {json}"
    );
    assert!(
        json.contains("\"ip_is_private\":true"),
        "GEOIP,private must stay native: {json}"
    );
    assert_eq!(
        rendered.skipped, 0,
        "provider-backed GEOIP/GEOSITE rules are not skipped"
    );
}

#[test]
fn match_rule_sets_route_final_and_first_wins() {
    let rules = vec![parse("MATCH,DIRECT"), parse("MATCH,PROXY")];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.final_outbound, "direct");
    assert!(rendered.rules.is_empty());
}

#[test]
fn default_final_depends_on_proxy_presence() {
    assert_eq!(
        render_sing_box_rules(&[], &[], true, &std::collections::BTreeSet::new()).final_outbound,
        "PROXY"
    );
    assert_eq!(
        render_sing_box_rules(&[], &[], false, &std::collections::BTreeSet::new()).final_outbound,
        "direct"
    );
}

#[test]
fn proxy_policies_skip_without_proxy_outbounds() {
    let rules = vec![
        parse("DOMAIN-SUFFIX,example.com,PROXY"),
        parse("MATCH,DIRECT"),
    ];
    let rendered = render_sing_box_rules(&rules, &[], false, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 1);
    assert!(rendered.rules.is_empty());
    assert_eq!(rendered.final_outbound, "direct");
}

#[test]
fn group_names_fold_into_proxy_and_global_keeps_its_selector() {
    let rules = vec![
        parse("DOMAIN,a.example,MyGroup"),
        parse("DOMAIN,b.example,GLOBAL"),
    ];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(json.contains("\"outbound\":\"PROXY\""));
    assert!(json.contains("\"outbound\":\"GLOBAL\""));
}

#[test]
fn reject_final_requires_the_block_outbound() {
    let rules = vec![parse("MATCH,REJECT")];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.final_outbound, "block");
    assert!(rendered.block_outbound);
}

fn provider(
    name: &str,
    source: RuleProviderSource,
    behavior: RuleProviderBehavior,
    format: RuleProviderFormat,
) -> RuleProvider {
    RuleProvider {
        name: RuleProviderName::new(name.to_owned()).unwrap(),
        source,
        behavior,
        format,
    }
}

#[test]
fn user_rule_provider_renders_as_remote_source_with_preserved_metadata() {
    let url = "https://example.com/my-rules.yaml".to_owned();
    let user = vec![provider(
        "my-google",
        RuleProviderSource::Http {
            url: RuleText::new(url.clone()).unwrap(),
            interval_ms: 86_400_000,
        },
        RuleProviderBehavior::Domain,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&[], &user, true, &std::collections::BTreeSet::new());
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(sources.contains("\"tag\":\"my-google\""));
    assert!(sources.contains("\"type\":\"remote\""));
    assert!(sources.contains("\"url\":\"https://example.com/my-rules.yaml\""));
    assert!(sources.contains("\"format\":\"source\""));
    assert!(sources.contains("\"behavior\":\"domain\""));
    assert!(sources.contains("\"download_interval\":\"24h\""));
}

#[test]
fn file_rule_provider_renders_as_local_source() {
    let user = vec![provider(
        "local-rules",
        RuleProviderSource::File {
            path: RuleText::new("/etc/caly/local.yaml".to_owned()).unwrap(),
        },
        RuleProviderBehavior::Classical,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&[], &user, true, &std::collections::BTreeSet::new());
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(sources.contains("\"type\":\"local\""));
    assert!(sources.contains("\"path\":\"/etc/caly/local.yaml\""));
    assert!(!sources.contains("\"behavior\":\"classical\""));
}

#[test]
fn inline_rule_provider_renders_as_headless_rules() {
    let user = vec![provider(
        "inline-ads",
        RuleProviderSource::Inline {
            payload: BoundedText::new("ads.example.com\ntrack.example.org\n".to_owned()).unwrap(),
        },
        RuleProviderBehavior::DomainSuffix,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&[], &user, true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 0);
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    // sing-box consumes a real inline rule-set: converted headless
    // rules, not the raw payload text.
    assert!(sources.contains("\"type\":\"inline\""));
    assert!(sources.contains("\"domain_suffix\":[\"ads.example.com\"]"));
    assert!(sources.contains("\"domain_suffix\":[\"track.example.org\"]"));
    assert!(!sources.contains("\"payload\""));
    assert!(!sources.contains("\"format\""));
}

#[test]
fn inline_domain_behavior_marks_dot_prefixed_lines_as_suffixes() {
    let user = vec![provider(
        "inline-domains",
        RuleProviderSource::Inline {
            payload: BoundedText::new(
                "exact.example.com\n.suffix.example.org\n+.plus.example.net\n".to_owned(),
            )
            .unwrap(),
        },
        RuleProviderBehavior::Domain,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&[], &user, true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 0);
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(sources.contains("\"domain\":[\"exact.example.com\"]"));
    assert!(sources.contains("\"domain_suffix\":[\"suffix.example.org\"]"));
    assert!(sources.contains("\"domain_suffix\":[\"plus.example.net\"]"));
}

#[test]
fn inline_classical_behavior_converts_matcher_lines_and_counts_skips() {
    let user = vec![provider(
        "inline-classical",
        RuleProviderSource::Inline {
            payload: BoundedText::new(
                "# comment\nDOMAIN,ok.example.com,DIRECT\nIP-CIDR,10.0.0.0/8,REJECT\n\
                 GEOIP,CN,PROXY\nnot-a-rule\n"
                    .to_owned(),
            )
            .unwrap(),
        },
        RuleProviderBehavior::Classical,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&[], &user, true, &std::collections::BTreeSet::new());
    // GEOIP (nested rule-set reference) + the malformed line are
    // counted as skipped; comments are not.
    assert_eq!(rendered.skipped, 2);
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(sources.contains("\"domain\":[\"ok.example.com\"]"));
    assert!(sources.contains("\"ip_cidr\":[\"10.0.0.0/8\"]"));
    assert!(!sources.contains("GEOIP"));
}

#[test]
fn inline_ipcidr_behavior_emits_ip_cidr_rules() {
    let user = vec![provider(
        "inline-nets",
        RuleProviderSource::Inline {
            payload: BoundedText::new("192.168.0.0/16\n2001:db8::/32\n".to_owned()).unwrap(),
        },
        RuleProviderBehavior::IpCidr,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&[], &user, true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 0);
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(sources.contains("\"ip_cidr\":[\"192.168.0.0/16\"]"));
    assert!(sources.contains("\"ip_cidr\":[\"2001:db8::/32\"]"));
}

#[test]
fn rule_set_rule_emits_lookup_for_declared_provider() {
    let rules = vec![parse("RULE-SET,my-google,DIRECT")];
    let user = vec![provider(
        "my-google",
        RuleProviderSource::Http {
            url: RuleText::new("https://example.com/g.yaml".to_owned()).unwrap(),
            interval_ms: 86_400_000,
        },
        RuleProviderBehavior::Domain,
        RuleProviderFormat::Source,
    )];
    let rendered = render_sing_box_rules(&rules, &user, true, &std::collections::BTreeSet::new());
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(json.contains("\"rule_set\":\"my-google\""));
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(sources.contains("\"tag\":\"my-google\""));
    assert!(!sources.contains("\"tag\":\"geoip-my-google\""));
}

#[test]
fn geosite_rule_is_skipped_without_a_user_provider() {
    // W3a 降级 (2026-08): the SagerNet sing-geosite repo no longer ships
    // per-category .srs rule-sets; auto-referencing the dead URL made
    // sing-box FATAL at boot. GEOSITE rules are skipped (counted) unless
    // the operator declares an explicit rule-provider for the tag.
    let rules = vec![parse("GEOSITE,private,DIRECT")];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(!json.contains("geosite-private"), "must skip: {json}");
    assert_eq!(rendered.skipped, 1, "skipped must be counted");
    assert!(rendered.rule_sets.is_empty(), "no auto rule-set sources");
}

#[test]
fn uppercase_geosite_category_is_skipped_not_404() {
    // Regression (2026-08-11): `GEOSITE,CN,DIRECT` produced a `CN.srs`
    // URL that 404s at sing-box boot, so `core switch sing-box` never
    // reached a ready controller. The SagerNet rule-set source is
    // decommissioned entirely; the rule must be skipped (not rendered as
    // a dead remote URL) unless the operator supplies a provider.
    let rules = vec![parse("GEOSITE,CN,DIRECT")];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 1);
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert!(
        sources.is_empty() || sources == "[]",
        "no auto sources: {sources}"
    );
}

#[test]
fn geoip_private_renders_natively_as_ip_is_private() {
    // The lossless sing-box rendering of `GEOIP,private` (the plain
    // `geoip` key was removed in sing-box 1.12; no rule-set needed).
    let rules = vec![parse("GEOIP,private,DIRECT")];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(json.contains("\"ip_is_private\":true"), "json: {json}");
    assert_eq!(rendered.skipped, 0);
    assert!(rendered.rule_sets.is_empty(), "private needs no rule-set");
}

#[test]
fn mihomo_only_matchers_are_skipped_and_counted() {
    let rules = vec![
        parse("PROCESS-NAME,curl,DIRECT"),
        parse("SRC-IP-CIDR,10.0.0.0/8,REJECT"),
        parse("DOMAIN,example.com,DIRECT"),
    ];
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 2);
    let json = serde_json::to_string(&rendered.rules).unwrap_or_default();
    assert!(json.contains("\"domain\":[\"example.com\"]"));
    assert!(!json.contains("PROCESS-NAME"));
    assert!(!json.contains("SRC-IP-CIDR"));
    assert!(rendered.block_outbound);
}

#[test]
fn user_provider_with_same_tag_as_geoip_overrides_the_sagernet_url() {
    let rules = vec![parse("GEOIP,CN,DIRECT")];
    let user = vec![provider(
        "geoip-cn",
        RuleProviderSource::Http {
            url: RuleText::new("https://mirror.example.com/cn.srs".to_owned()).unwrap(),
            interval_ms: 3_600_000,
        },
        RuleProviderBehavior::IpCidr,
        RuleProviderFormat::Binary,
    )];
    let rendered = render_sing_box_rules(&rules, &user, true, &std::collections::BTreeSet::new());
    let sources = serde_json::to_string(&rendered.rule_sets).unwrap_or_default();
    assert_eq!(
        sources
            .matches("\"url\":\"https://mirror.example.com/cn.srs\"")
            .count(),
        1
    );
    assert_eq!(
        sources
            .matches("geoip-cn.srs")
            .filter(|needle| needle.contains("sing-geoip"))
            .count(),
        0
    );
}

#[test]
fn sing_box_interval_formats_hours_minutes_and_seconds() {
    assert_eq!(sing_box_interval(3_600_000), "1h");
    assert_eq!(sing_box_interval(7_200_000), "2h");
    assert_eq!(sing_box_interval(60_000), "1m");
    // Milliseconds must be converted to seconds, not suffixed.
    assert_eq!(sing_box_interval(30_000), "30s");
    assert_eq!(sing_box_interval(45_000), "45s");
    assert_eq!(sing_box_interval(90_000), "90s");
    // Sub-second remainders round up; never render `0s`.
    assert_eq!(sing_box_interval(1_500), "2s");
    assert_eq!(sing_box_interval(500), "1s");
}
