//! Tests for `rule.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

fn rule(line: &str) -> Result<RoutingRule, RuleError> {
    RoutingRule::from_clash_line(line)
}

#[test]
fn parses_all_rule_types() -> Result<(), RuleError> {
    assert!(matches!(
        rule("DOMAIN,example.com,DIRECT")?.matcher,
        RuleMatch::Domain(_)
    ));
    assert!(matches!(
        rule("DOMAIN-SUFFIX,google.com,PROXY")?.matcher,
        RuleMatch::DomainSuffix(_)
    ));
    assert!(matches!(
        rule("DOMAIN-KEYWORD,ad,REJECT")?.matcher,
        RuleMatch::DomainKeyword(_)
    ));
    assert!(matches!(
        rule("IP-CIDR,192.168.0.0/16,DIRECT")?.matcher,
        RuleMatch::IpCidr(_)
    ));
    assert!(matches!(
        rule("GEOIP,CN,DIRECT")?.matcher,
        RuleMatch::Geoip(_)
    ));
    assert!(matches!(
        rule("RULE-SET,my-prov,DIRECT")?.matcher,
        RuleMatch::RuleSet(_)
    ));
    assert!(matches!(
        rule("GEOSITE,private,DIRECT")?.matcher,
        RuleMatch::Geosite(_)
    ));
    assert!(matches!(
        rule("PROCESS-NAME,curl,DIRECT")?.matcher,
        RuleMatch::ProcessName(_)
    ));
    assert!(matches!(
        rule("SRC-IP-CIDR,192.168.0.0/16,DIRECT")?.matcher,
        RuleMatch::SourceIpCidr(_)
    ));
    assert!(matches!(rule("MATCH,PROXY")?.matcher, RuleMatch::Match));
    Ok(())
}

#[test]
fn policy_parses_direct_reject_and_group() -> Result<(), RuleError> {
    assert_eq!(rule("DOMAIN,a.com,DIRECT")?.policy, RulePolicy::Direct);
    assert_eq!(rule("DOMAIN,a.com,REJECT")?.policy, RulePolicy::Reject);
    assert_eq!(rule("DOMAIN,a.com,Auto")?.policy.to_clash(), "Auto");
    Ok(())
}

#[test]
fn rejects_malformed_rules() {
    assert!(matches!(
        RoutingRule::from_clash_line(""),
        Err(RuleError::Empty)
    ));
    assert!(matches!(
        RoutingRule::from_clash_line("BOGUS,foo,DIRECT"),
        Err(RuleError::UnknownType)
    ));
    assert!(matches!(
        RoutingRule::from_clash_line("DOMAIN,foo"),
        Err(RuleError::Malformed)
    ));
    assert!(matches!(
        RoutingRule::from_clash_line("IP-CIDR,no-slash,DIRECT"),
        Err(RuleError::InvalidCidr)
    ));
}

#[test]
fn domain_matching_is_exact_and_case_insensitive() -> Result<(), RuleError> {
    let r = rule("DOMAIN,Example.COM,DIRECT")?;
    assert!(r.matches_host("example.com"));
    assert!(r.matches_host("EXAMPLE.com"));
    assert!(!r.matches_host("sub.example.com"));
    Ok(())
}

#[test]
fn suffix_matching_covers_apex_and_subdomains() -> Result<(), RuleError> {
    let r = rule("DOMAIN-SUFFIX,example.com,DIRECT")?;
    assert!(r.matches_host("example.com"));
    assert!(r.matches_host("a.b.example.com"));
    assert!(!r.matches_host("notexample.com"));
    Ok(())
}

#[test]
fn keyword_matching_finds_substrings() -> Result<(), RuleError> {
    let r = rule("DOMAIN-KEYWORD,ad,REJECT")?;
    assert!(r.matches_host("ads.example.com"));
    assert!(r.matches_host("example.adroll.net"));
    assert!(!r.matches_host("example.com"));
    Ok(())
}

#[test]
fn first_host_match_wins_and_match_is_catch_all() -> Result<(), RuleError> {
    let rules = vec![
        rule("DOMAIN-SUFFIX,example.com,DIRECT")?,
        rule("DOMAIN-KEYWORD,ad,REJECT")?,
        rule("MATCH,PROXY")?,
    ];
    assert_eq!(
        match_host(&rules, "example.com").map(|r| r.policy.clone()),
        Some(RulePolicy::Direct)
    );
    assert_eq!(
        match_host(&rules, "ads.example.com").map(|r| r.policy.clone()),
        Some(RulePolicy::Direct)
    );
    let catch_all = rule("MATCH,PROXY")?;
    assert_eq!(
        match_host(&rules, "other.org").map(|r| r.policy.clone()),
        Some(catch_all.policy.clone())
    );
    Ok(())
}

#[test]
fn geoip_does_not_match_hosts() -> Result<(), RuleError> {
    assert!(!rule("GEOIP,CN,DIRECT")?.matches_host("example.cn"));
    Ok(())
}

#[test]
fn clash_line_round_trips() -> Result<(), RuleError> {
    for line in [
        "DOMAIN,example.com,DIRECT",
        "DOMAIN-SUFFIX,google.com,PROXY",
        "IP-CIDR,10.0.0.0/8,DIRECT",
        "GEOIP,CN,DIRECT",
        "RULE-SET,my-prov,DIRECT",
        "GEOSITE,private,DIRECT",
        "PROCESS-NAME,curl,DIRECT",
        "SRC-IP-CIDR,192.168.0.0/16,DIRECT",
        "MATCH,Auto",
    ] {
        assert_eq!(rule(line)?.to_clash_line(), line);
    }
    Ok(())
}

#[test]
fn rule_set_and_geosite_never_match_hosts_offline() -> Result<(), RuleError> {
    // The offline engine has no rule-provider database, so references to
    // a rule-set or a geosite category cannot be evaluated locally. They
    // are render-only matchers; the live core consumes them.
    assert!(!rule("RULE-SET,my-prov,DIRECT")?.matches_host("example.com"));
    assert!(!rule("GEOSITE,private,DIRECT")?.matches_host("example.com"));
    assert!(!rule("PROCESS-NAME,curl,DIRECT")?.matches_host("example.com"));
    Ok(())
}

#[test]
fn parses_trailing_flags_after_the_policy() -> Result<(), RuleError> {
    let flagged = rule("IP-CIDR,1.2.3.0/24,DIRECT,no-resolve")?;
    assert_eq!(flagged.policy, RulePolicy::Direct);
    assert!(flagged.flags.no_resolve);
    assert!(!flagged.flags.src);
    assert_eq!(
        flagged.to_clash_line(),
        "IP-CIDR,1.2.3.0/24,DIRECT,no-resolve"
    );

    let both = rule("IP-CIDR6,::1/128,REJECT,src,no-resolve")?;
    assert_eq!(both.policy, RulePolicy::Reject);
    assert!(both.flags.no_resolve && both.flags.src);
    // Canonical render order is no-resolve,src regardless of input order.
    assert_eq!(
        both.to_clash_line(),
        "IP-CIDR6,::1/128,REJECT,no-resolve,src"
    );

    assert!(matches!(
        RoutingRule::from_clash_line("IP-CIDR,1.2.3.0/24,DIRECT,bogus-flag"),
        Err(RuleError::UnknownFlag)
    ));
    assert!(matches!(
        RoutingRule::from_clash_line("MATCH,PROXY,no-resolve"),
        Err(RuleError::Malformed)
    ));
    Ok(())
}

#[test]
fn adjacent_automated_tests_keep_flagless_default() -> Result<(), RuleError> {
    let plain = rule("DOMAIN,example.com,PROXY")?;
    assert_eq!(plain.flags, RuleFlags::none());
    assert_eq!(plain.to_clash_line(), "DOMAIN,example.com,PROXY");
    Ok(())
}

#[test]
fn rule_provider_behavior_and_format_have_clash_labels() {
    assert_eq!(RuleProviderBehavior::Domain.clash_label(), "domain");
    assert_eq!(
        RuleProviderBehavior::DomainSuffix.clash_label(),
        "domain_suffix"
    );
    assert_eq!(RuleProviderBehavior::IpCidr.clash_label(), "ipcidr");
    assert_eq!(RuleProviderBehavior::Classical.clash_label(), "classical");
    assert_eq!(RuleProviderFormat::Source.clash_label(), "source");
    assert_eq!(RuleProviderFormat::Binary.clash_label(), "binary");
}
