//! Offline routing-rule commands: list configured rules and diagnose which
//! rule a host or IP hits. Both run without a daemon, reading the configured
//! rules and applying the domain matching engine.

use std::process::ExitCode;

use caly_domain::RoutingRule;
use caly_profile::rule_match::{match_host, match_ip};

/// Executes an offline rule command (`core rules` / `core rule-match`).
pub(super) fn run_rules(cmd: &super::legacy::CoreCmd, json: bool) -> ExitCode {
    use super::legacy::CoreCmd;
    let rules = crate::config::routing_rules();
    match cmd {
        CoreCmd::Rules => list_rules(&rules, json),
        CoreCmd::RuleMatch(target) => rule_match(&rules, target, json),
        _ => ExitCode::FAILURE,
    }
}

fn list_rules(rules: &[RoutingRule], json: bool) -> ExitCode {
    if json {
        let entries: Vec<serde_json::Value> = rules
            .iter()
            .map(|rule| serde_json::json!({ "rule": rule.to_clash_line() }))
            .collect();
        println!("{}", serde_json::json!({ "rules": entries }));
    } else if rules.is_empty() {
        println!("no routing rules configured (add a `rules:` list to the config)");
    } else {
        println!("routing rules ({} total, first match wins):", rules.len());
        for (index, rule) in rules.iter().enumerate() {
            println!("  {index}: {}", rule.to_clash_line());
        }
    }
    ExitCode::SUCCESS
}

fn rule_match(rules: &[RoutingRule], target: &str, json: bool) -> ExitCode {
    // IP targets evaluate IP-CIDR/MATCH; everything else evaluates the host
    // matchers. A literal that parses as an IP is treated as an IP.
    let hit = if target.parse::<std::net::IpAddr>().is_ok() {
        match_ip(rules, target)
    } else {
        match_host(rules, target)
    };
    if let Some(rule) = hit {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "target": target,
                    "matched": true,
                    "rule": rule.to_clash_line(),
                    "policy": rule.policy.to_clash(),
                })
            );
        } else {
            println!(
                "{target} -> {} ({})",
                rule.to_clash_line(),
                rule.policy.to_clash()
            );
        }
        return ExitCode::SUCCESS;
    }
    if json {
        println!(
            "{}",
            serde_json::json!({ "target": target, "matched": false })
        );
    } else {
        println!("{target} -> no rule matched");
    }
    // Not matching is a valid diagnostic outcome, not an error.
    ExitCode::SUCCESS
}
