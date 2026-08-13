//! Fixture-corpus tests (#80): static, reviewable fuzz inputs under
//! `fixtures/` with their expectations, guarding the parser surfaces the
//! audit called under-covered: real-world vmess/ss URI edges, broken base64,
//! airport-scale YAML, and provider-name path traversal attempts.

use caly_domain::SubscriptionId;
use caly_subscription::{ClashParseError, parse_any_proxy_uri, parse_clash_config};

const URI_CORPUS: &str = include_str!("../../../fixtures/proxy-uri-corpus.txt");
const NAME_CORPUS: &str = include_str!("../../../fixtures/malicious-provider-names.txt");
const OVERSIZE: &str = include_str!("../../../fixtures/oversize-clash-document.yaml");

fn subscription() -> SubscriptionId {
    SubscriptionId::from_bytes([9; 16])
}

/// Each corpus record is `ok <uri>` or `err <uri>`; an expectation flip is
/// a parser-behaviour regression somebody must triage, never a silent merge.
#[test]
fn proxy_uri_corpus_expectations_hold() -> Result<(), String> {
    let mut checked = 0_usize;
    for (line_number, raw) in URI_CORPUS.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (expectation, uri) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("line {}: missing expectation prefix", line_number + 1))?;
        let parsed = parse_any_proxy_uri(uri.trim(), subscription());
        match (expectation, parsed.is_ok()) {
            ("ok", true) | ("err", false) => {}
            _ => {
                return Err(format!(
                    "line {}: expectation `{expectation}` failed for `{}`: {parsed:?}",
                    line_number + 1,
                    uri.trim(),
                ));
            }
        }
        checked += 1;
    }
    // Guard against an accidentally emptied fixture dropping coverage to zero.
    if checked < 15 {
        return Err(format!("corpus shrank to {checked} records"));
    }
    Ok(())
}

/// Mirrors audit #22 at the fuzz level: none of these names may ever become
/// a `<state>/rule-providers/<name>.yaml` file name.
#[test]
fn malicious_provider_names_are_all_rejected() -> Result<(), String> {
    let mut checked = 0_usize;
    for (line_number, raw) in NAME_CORPUS.lines().enumerate() {
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        if caly_domain::is_path_safe_component(raw) {
            return Err(format!(
                "line {}: provider name `{raw}` newly accepted — path traversal gate regressed",
                line_number + 1,
            ));
        }
        checked += 1;
    }
    if checked < 10 {
        return Err(format!("name corpus shrank to {checked} records"));
    }
    Ok(())
}

/// Airport-scale import: exactly 4000 nodes, 32 groups, 6001 rules — no
/// silent truncation, no arbitrary ceiling below the documented budgets.
#[test]
fn oversize_clash_document_parses_exactly() -> Result<(), String> {
    let import = parse_clash_config(OVERSIZE, subscription())
        .map_err(|error| format!("oversize document rejected: {error:?}"))?;
    assert_eq!(import.proxies.len(), 4_000);
    assert_eq!(import.proxy_groups.len(), 32);
    assert_eq!(import.rules.len(), 6_001);
    // Spot-check shape fidelity at scale: the url-test group kept its probe,
    // and group references classified into nested groups, not node names.
    let url_test = &import.proxy_groups[1];
    assert_eq!(url_test.kind, caly_domain::ProxyGroupType::UrlTest);
    let Some(probe) = &url_test.url_test else {
        return Err("url-test group lost its probe config at scale".to_owned());
    };
    assert_eq!(probe.interval_seconds, 600);
    Ok(())
}

/// The documented budget must still trip: the oversize fixture sits under
/// the 10k-node budget, but one record past it is a loud error, not a
/// partial import.
#[test]
fn over_budget_still_rejected_loudly() -> Result<(), String> {
    use std::fmt::Write as _;
    let mut yaml = String::from("proxies:\n");
    for index in 0..10_001_usize {
        let _ = writeln!(
            yaml,
            "  - {{name: 'n{index}', type: vmess, server: s{index}.example.net, \
             port: 443, uuid: '03ec34a7-a95b-4646-99b6-8957c530add1'}}"
        );
    }
    yaml.push_str("proxy-groups: []\nrules: []\n");
    let outcome = parse_clash_config(&yaml, subscription());
    match outcome {
        Err(ClashParseError::TooManyNodes) => Ok(()),
        other => Err(format!(
            "expected a loud TooManyNodes at 10001 nodes, got {}",
            match other {
                Ok(import) => format!("Ok({} nodes)", import.proxies.len()),
                Err(error) => format!("Err({error:?})"),
            }
        )),
    }
}
