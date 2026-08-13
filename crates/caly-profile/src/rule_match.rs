//! Rule matching engine: evaluates which routing rule a host or IP hits.
//!
//! Host matching delegates to the pure domain matcher; IP/CIDR matching does
//! address arithmetic and therefore lives here (infrastructure) rather than in
//! the domain, which must stay free of `std::net`. Powers the
//! `caly core rule-match` diagnostic and config rule validation.

use caly_domain::RoutingRule;

/// Re-exports the domain host matcher for callers wanting one entry point.
pub use caly_domain::match_host;

/// Returns the first rule whose matcher hits the IP address (order-sensitive).
/// Only IP-CIDR and the catch-all evaluate; GEOIP needs a GeoIP database the
/// offline engine does not carry.
pub fn match_ip<'a>(rules: &'a [RoutingRule], ip: &str) -> Option<&'a RoutingRule> {
    rules.iter().find(|rule| rule_matches_ip(rule, ip))
}

/// Whether a single rule matches an IP address.
pub fn rule_matches_ip(rule: &RoutingRule, ip: &str) -> bool {
    match rule_match_kind(rule) {
        RuleIpView::IpCidr(cidr) => cidr_contains(cidr, ip),
        RuleIpView::Match => true,
        RuleIpView::None => false,
    }
}

enum RuleIpView<'a> {
    IpCidr(&'a str),
    Match,
    None,
}

fn rule_match_kind(rule: &RoutingRule) -> RuleIpView<'_> {
    match &rule.matcher {
        caly_domain::RuleMatch::IpCidr(cidr) => RuleIpView::IpCidr(cidr.as_str()),
        caly_domain::RuleMatch::Match => RuleIpView::Match,
        _ => RuleIpView::None,
    }
}

/// Validates CIDR text (`addr/prefix`) using the standard library's pure value
/// parsing (no I/O). Accepts IPv4 and IPv6.
pub fn is_valid_cidr(value: &str) -> bool {
    cidr_parts(value).is_some()
}

/// Whether `ip` falls inside the CIDR block. Pure arithmetic on value types.
fn cidr_contains(cidr: &str, ip: &str) -> bool {
    let Some((network, prefix)) = cidr_parts(cidr) else {
        return false;
    };
    let Ok(target) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    match (network, target) {
        (std::net::IpAddr::V4(net), std::net::IpAddr::V4(addr)) => {
            if prefix > 32 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(net) & mask) == (u32::from(addr) & mask)
        }
        (std::net::IpAddr::V6(net), std::net::IpAddr::V6(addr)) => {
            if prefix > 128 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(net) & mask) == (u128::from(addr) & mask)
        }
        _ => false,
    }
}

fn cidr_parts(value: &str) -> Option<(std::net::IpAddr, u32)> {
    let (address, prefix) = value.split_once('/')?;
    let network = address.parse::<std::net::IpAddr>().ok()?;
    let prefix: u32 = prefix.parse().ok()?;
    let max = match network {
        std::net::IpAddr::V4(_) => 32,
        std::net::IpAddr::V6(_) => 128,
    };
    if prefix > max {
        return None;
    }
    Some((network, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(line: &str) -> RoutingRule {
        RoutingRule::from_clash_line(line)
            .unwrap_or_else(|e| panic!("rule parse failed for {line}: {e}"))
    }

    #[test]
    fn cidr_matching_is_correct() {
        let r = rule("IP-CIDR,192.168.0.0/16,DIRECT");
        assert!(rule_matches_ip(&r, "192.168.1.10"));
        assert!(!rule_matches_ip(&r, "10.0.0.1"));
        assert!(!rule_matches_ip(&r, "192.169.0.1"));
        assert!(rule_matches_ip(
            &rule("IP-CIDR,0.0.0.0/0,DIRECT"),
            "8.8.8.8"
        ));
        assert!(rule_matches_ip(
            &rule("IP-CIDR6,::/0,DIRECT"),
            "2001:db8::1"
        ));
    }

    #[test]
    fn first_ip_match_wins() {
        let rules = vec![rule("IP-CIDR,192.168.0.0/16,DIRECT"), rule("MATCH,PROXY")];
        let hit = match_ip(&rules, "192.168.5.1").unwrap();
        assert_eq!(hit.policy, caly_domain::RulePolicy::Direct);
        let hit = match_ip(&rules, "8.8.8.8").unwrap();
        assert_eq!(
            hit.policy,
            caly_domain::RulePolicy::Proxy(caly_domain::RuleText::new("PROXY").unwrap())
        );
    }

    #[test]
    fn invalid_cidr_never_matches() {
        assert!(!is_valid_cidr("not-a-cidr"));
        assert!(!is_valid_cidr("1.2.3.4/64"));
        assert!(is_valid_cidr("1.2.3.4/24"));
        assert!(is_valid_cidr("2001:db8::/32"));
    }

    #[test]
    fn geoip_does_not_match_ips_offline() {
        assert!(!rule_matches_ip(&rule("GEOIP,CN,DIRECT"), "1.2.3.4"));
    }
}
