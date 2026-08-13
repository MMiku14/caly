//! Config-driven routing rules rendered as sing-box `route.rules` (typed).
//!
//! Mihomo consumes Clash-format rule lines directly; sing-box needs structured
//! route rule objects instead. This module is the single mapping between the
//! domain `RoutingRule` model and sing-box route semantics, shared by the
//! subscription document renderer and the subscription-less base renderer.
//!
//! P3b typed-model form: route rules, headless rules and rule-set sources are
//! serde structs whose fields are declared alphabetically, mirroring the byte
//! order the pre-typed `serde_json::Value` (BTreeMap) assembly produced.

use caly_domain::{
    RoutingRule, RuleMatch, RulePolicy, RuleProvider, RuleProviderBehavior, RuleProviderFormat,
    RuleProviderSource,
};
use serde::Serialize;
use std::collections::BTreeSet;

/// Rendered routing rules ready to embed into a sing-box document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingBoxRules {
    /// Route rule objects; empty when no rule is representable (the document
    /// should omit the `rules` key).
    pub rules: Vec<RouteRule>,
    /// `route.rule_set` sources (remote geo rule-sets referenced by GEOIP
    /// rules, plus user-declared providers); empty when not needed.
    pub rule_sets: Vec<RuleSetSource>,
    /// The outbound tag for `route.final` (derived from the MATCH rule, or a
    /// sensible default when absent).
    pub final_outbound: String,
    /// Whether the document must add the built-in `block` outbound (any rule
    /// steers to REJECT).
    pub block_outbound: bool,
    /// Rules skipped because their policy has no outbound in the document.
    pub skipped: usize,
}

/// One sing-box route rule: exactly one matcher key plus the `outbound` (and
/// the `rule_set` lookup key for rule-set-referencing matchers).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RouteRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_keyword: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_suffix: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_cidr: Option<Vec<String>>,
    pub outbound: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_set: Option<String>,
    /// `ip_is_private: true` — the lossless sing-box rendering of
    /// `GEOIP,private` (the plain `geoip` key was removed in 1.12).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_is_private: Option<bool>,
}

/// One sing-box headless matcher object inside an inline rule-set
/// (`{ "<kind>": ["<value>"] }` — no `outbound`; the referencing route rule
/// supplies the action).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HeadlessRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_keyword: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_suffix: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_cidr: Option<Vec<String>>,
}

/// One `route.rule_set` source: `remote` (HTTP), `local` (file) or `inline`
/// (headless rules). The optional fields are mutually exclusive per kind; the
/// constructors below are the only producers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleSetSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<HeadlessRule>>,
    pub tag: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Maps the config rules onto sing-box route semantics.
///
/// `proxy_representable` states whether the document carries proxy outbounds
/// (a subscription renders a `PROXY` selector; the subscription-less base
/// config only carries `direct`). Unrepresentable proxy/group policies are
/// skipped and counted instead of fabricating outbounds.
///
/// `user_providers` is the list of `rule_providers:` entries from the
/// config; each is rendered as one `route.rule_set` source so
/// `RULE-SET,<name>,…` rules resolve at runtime. Auto-emitted GEOIP
/// rule-sets and per-category SagerNet geosite rule-sets are added on
/// top of the user list.
pub fn render_sing_box_rules(
    rules: &[RoutingRule],
    user_providers: &[RuleProvider],
    proxy_representable: bool,
    group_outbounds: &BTreeSet<String>,
) -> SingBoxRules {
    let mut objects: Vec<RouteRule> = Vec::new();
    let mut final_outbound: Option<String> = None;
    let mut block_outbound = false;
    let mut skipped = 0usize;
    for rule in rules {
        if let RuleMatch::Match = &rule.matcher {
            // Clash semantics: the first MATCH wins; later catch-alls are dead.
            if final_outbound.is_none() {
                match outbound_tag_for_policy(&rule.policy, proxy_representable, group_outbounds) {
                    Some(outbound) => final_outbound = Some(outbound),
                    None => skipped += 1,
                }
            }
            continue;
        }
        let Some(outbound) =
            outbound_tag_for_policy(&rule.policy, proxy_representable, group_outbounds)
        else {
            skipped += 1;
            continue;
        };
        if outbound == "block" {
            block_outbound = true;
        }
        // The declared provider tags decide whether a GEOIP/GEOSITE rule
        // can render as a `rule_set` reference (the tag the provider is
        // published under) instead of being skipped. Case-insensitive:
        // rule keys (`GEOIP,cn`) and provider names (`CN`) are the same
        // intent in different spellings.
        let provider_tags: std::collections::HashSet<String> = user_providers
            .iter()
            .map(|provider| provider.name.as_str().to_ascii_lowercase())
            .collect();
        if let Some(object) = matcher_object(&rule.matcher, &outbound, &provider_tags) {
            objects.push(object);
        } else {
            // The matcher is not representable in the target kernel
            // (e.g. Mihomo-only `PROCESS-NAME` / `SRC-IP-CIDR` in
            // sing-box). Count it as skipped so the caller can warn
            // the operator without aborting the render.
            skipped += 1;
        }
    }
    let final_outbound = final_outbound.unwrap_or_else(|| {
        if proxy_representable {
            "PROXY".to_owned()
        } else {
            "direct".to_owned()
        }
    });
    if final_outbound == "block" {
        block_outbound = true;
    }
    let (rule_sets, inline_skipped) = build_rule_sets(user_providers);
    SingBoxRules {
        rules: objects,
        rule_sets,
        final_outbound,
        block_outbound,
        skipped: skipped + inline_skipped,
    }
}

/// Builds the `route.rule_set` source list from the user-declared
/// providers. The auto-emitted SagerNet geoip/geosite sources were removed
/// (2026-08): upstream decommissioned the `.srs` rule-set directory, and
/// referencing the dead URL made sing-box FATAL at boot (实测:
/// `core switch sing-box` 404). GEOIP/GEOSITE rules now either render
/// natively (`GEOIP,private` → `ip_is_private`) or are skipped with a
/// warning; only explicitly declared providers emit remote sources.
fn build_rule_sets(user_providers: &[RuleProvider]) -> (Vec<RuleSetSource>, usize) {
    let mut sources: Vec<RuleSetSource> = Vec::new();
    let mut skipped = 0usize;
    for provider in user_providers {
        let (source, inline_skipped) = provider_to_source(provider);
        skipped += inline_skipped;
        sources.push(source);
    }
    (sources, skipped)
}

/// Renders one user-declared rule provider as a sing-box `route.rule_set`
/// source, plus the count of payload lines that had no sing-box
/// representation (folded into the caller's `skipped` accounting).
/// `inline` providers render as real `type: inline` sources whose
/// payload lines are converted to headless rules (#62); HTTP / file
/// providers render as `type: remote` (HTTP) or `type: local` (file)
/// sources, preserving the user's URL / path / interval.
fn provider_to_source(provider: &RuleProvider) -> (RuleSetSource, usize) {
    let tag = provider.name.as_str().to_owned();
    let format = sing_box_format(provider.format);
    match &provider.source {
        RuleProviderSource::Http { url, interval_ms } => (
            RuleSetSource {
                // sing-box's `behavior` discriminator mirrors our domain enum
                // by name, so the user's intent (domain vs ipcidr vs classical)
                // is preserved across the kernel boundary.
                behavior: sing_box_behavior(provider.behavior),
                // sing-box 1.12 uses `download_interval` (Go duration), not
                // raw milliseconds; `*time.Minute` is the supported shorthand.
                download_interval: Some(sing_box_interval(*interval_ms)),
                format: Some(format),
                path: None,
                rules: None,
                tag,
                kind: "remote",
                url: Some(url.as_str().to_owned()),
            },
            0,
        ),
        RuleProviderSource::File { path } => (
            RuleSetSource {
                behavior: sing_box_behavior(provider.behavior),
                download_interval: None,
                format: Some(format),
                path: Some(path.as_str().to_owned()),
                rules: None,
                tag,
                kind: "local",
                url: None,
            },
            0,
        ),
        RuleProviderSource::Inline { payload } => {
            // Audit #62: sing-box consumes `type: inline` rule-sets whose
            // `rules` are headless matcher objects — NOT the Clash-payload
            // text this provider carries. Convert each payload line to the
            // matching headless rule so `inline provider + sing-box` boots
            // with a config the kernel actually accepts. The pre-#62 shape
            // emitted the payload verbatim as an unknown `payload:` key
            // (plus a `format:` key inline sources do not take), which
            // sing-box rejected at startup.
            let (rules, skipped) = inline_payload_rules(provider, payload.as_str());
            (
                RuleSetSource {
                    behavior: None,
                    download_interval: None,
                    format: None,
                    path: None,
                    rules: Some(rules),
                    tag,
                    kind: "inline",
                    url: None,
                },
                skipped,
            )
        }
    }
}

/// Converts one inline provider's payload text into sing-box headless
/// rules, honouring the declared `behavior:`
///
/// - `domain`: one hostname per line; a leading `.` / `+.` marks a
///   suffix matcher, anything else is an exact domain;
/// - `domain-suffix`: every line is a suffix matcher;
/// - `ipcidr`: one CIDR per line;
/// - `classical`: full Clash rule lines, parsed through
///   [`RoutingRule::from_clash_line`]; only the matcher is kept (the
///   rule-set supplies matchers, the referencing route rule supplies
///   the action).
///
/// Returns the headless-rule objects and how many lines were skipped
/// (comments/blanks are ignored, not counted; unparseable or
/// kernel-unrepresentable lines are counted so the caller can warn).
fn inline_payload_rules(provider: &RuleProvider, payload: &str) -> (Vec<HeadlessRule>, usize) {
    let mut rules: Vec<HeadlessRule> = Vec::new();
    let mut skipped = 0usize;
    for raw in payload.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let headless = match provider.behavior {
            RuleProviderBehavior::Domain => {
                let suffix = line
                    .strip_prefix("+.")
                    .or_else(|| line.strip_prefix('.'))
                    .filter(|stripped| !stripped.is_empty());
                match suffix {
                    Some(suffix) => Some(headless_rule_domain_suffix(suffix)),
                    None => Some(headless_rule_domain(line)),
                }
            }
            RuleProviderBehavior::DomainSuffix => {
                let suffix = line
                    .strip_prefix("+.")
                    .or_else(|| line.strip_prefix('.'))
                    .unwrap_or(line);
                if suffix.is_empty() {
                    skipped += 1;
                    None
                } else {
                    Some(headless_rule_domain_suffix(suffix))
                }
            }
            RuleProviderBehavior::IpCidr => Some(headless_rule_ip_cidr(line)),
            RuleProviderBehavior::Classical => {
                let converted = caly_domain::RoutingRule::from_clash_line(line)
                    .ok()
                    .and_then(|rule| headless_from_matcher(&rule.matcher));
                if converted.is_none() {
                    skipped += 1;
                }
                converted
            }
        };
        if let Some(rule) = headless {
            rules.push(rule);
        }
    }
    (rules, skipped)
}

/// Headless rule constructors (single matcher key each).
fn headless_rule_domain(value: &str) -> HeadlessRule {
    HeadlessRule {
        domain: Some(vec![value.to_owned()]),
        domain_keyword: None,
        domain_suffix: None,
        ip_cidr: None,
    }
}

fn headless_rule_domain_suffix(value: &str) -> HeadlessRule {
    HeadlessRule {
        domain: None,
        domain_keyword: None,
        domain_suffix: Some(vec![value.to_owned()]),
        ip_cidr: None,
    }
}

fn headless_rule_ip_cidr(value: &str) -> HeadlessRule {
    HeadlessRule {
        domain: None,
        domain_keyword: None,
        domain_suffix: None,
        ip_cidr: Some(vec![value.to_owned()]),
    }
}

/// Projects a parsed classical matcher onto the headless-rule kinds
/// sing-box consumes. Returns `None` for matchers a rule-set cannot
/// express (geo / nested rule-set references, Mihomo-only
/// `PROCESS-NAME` / `SRC-IP-CIDR`, and the catch-all `MATCH`).
fn headless_from_matcher(matcher: &RuleMatch) -> Option<HeadlessRule> {
    match matcher {
        RuleMatch::Domain(text) => Some(headless_rule_domain(text.as_str())),
        RuleMatch::DomainSuffix(text) => Some(headless_rule_domain_suffix(text.as_str())),
        RuleMatch::DomainKeyword(text) => Some(HeadlessRule {
            domain: None,
            domain_keyword: Some(vec![text.as_str().to_owned()]),
            domain_suffix: None,
            ip_cidr: None,
        }),
        RuleMatch::IpCidr(text) => Some(headless_rule_ip_cidr(text.as_str())),
        RuleMatch::Geoip(_)
        | RuleMatch::Geosite(_)
        | RuleMatch::RuleSet(_)
        | RuleMatch::ProcessName(_)
        | RuleMatch::SourceIpCidr(_)
        | RuleMatch::Match => None,
    }
}

/// Returns the sing-box `format` discriminator for one of our provider
/// formats. SagerNet compiles `binary` (`.srs`) and `source` (plain text)
/// rule-sets; we mirror those names exactly.
fn sing_box_format(format: RuleProviderFormat) -> &'static str {
    match format {
        RuleProviderFormat::Source => "source",
        RuleProviderFormat::Binary => "binary",
    }
}

/// Maps our `behavior` enum to the sing-box `route_set` discriminator.
/// Classical rule bodies are an exception: sing-box 1.12 does not
/// consume mixed Clash rule lines; an upstream converter is required,
/// so we omit the field and let sing-box try the default path
/// (the kernel will reject classical bodies with a clear validation
/// error rather than silently dropping rules).
fn sing_box_behavior(behavior: RuleProviderBehavior) -> Option<&'static str> {
    match behavior {
        RuleProviderBehavior::Domain => Some("domain"),
        RuleProviderBehavior::DomainSuffix => Some("domain_suffix"),
        RuleProviderBehavior::IpCidr => Some("ipcidr"),
        RuleProviderBehavior::Classical => None,
    }
}

/// sing-box consumes a Go duration string (`30m`, `12h`, …) for
/// `download_interval`. The bounded form keeps the value readable
/// when the user picks 1-day or 1-hour polling. Non-multiples fall back
/// to seconds — the input is *milliseconds*, so it must be divided by
/// 1_000 (a bare `"{ms}s"` would inflate the interval a thousandfold:
/// `90_000` ms is `90s`, not `90000s`). Sub-second remainders round up
/// so a duration never degenerates to `0s`.
fn sing_box_interval(interval_ms: u64) -> String {
    const ONE_MINUTE_MS: u64 = 60_000;
    const ONE_HOUR_MS: u64 = 3_600_000;
    if interval_ms >= ONE_HOUR_MS && interval_ms.is_multiple_of(ONE_HOUR_MS) {
        format!("{}h", interval_ms / ONE_HOUR_MS)
    } else if interval_ms >= ONE_MINUTE_MS && interval_ms.is_multiple_of(ONE_MINUTE_MS) {
        format!("{}m", interval_ms / ONE_MINUTE_MS)
    } else {
        format!("{}s", interval_ms.div_ceil(1_000))
    }
}

/// Maps one rule policy onto a sing-box outbound tag, or `None` when the
/// document cannot represent it. Group names fold into the `PROXY` selector
/// (the subscription document exposes every node through it); a literal
/// `GLOBAL` group keeps its dedicated selector.
fn outbound_tag_for_policy(
    policy: &RulePolicy,
    proxy_representable: bool,
    group_outbounds: &BTreeSet<String>,
) -> Option<String> {
    match policy {
        RulePolicy::Direct => Some("direct".to_owned()),
        RulePolicy::Reject => Some("block".to_owned()),
        // 2026-08-09 规划: a policy naming a subscription-declared group
        // resolves to that group outbound VERBATIM (the group renders under
        // its own tag); only policies outside the declared set collapse to
        // the PROXY catch-all selector, keeping the legacy single-group
        // behaviour for routing assets that predate the decision.
        RulePolicy::Proxy(group) if proxy_representable => {
            if group_outbounds.contains(group.as_str()) || group.as_str() == "GLOBAL" {
                Some(group.as_str().to_owned())
            } else {
                Some("PROXY".to_owned())
            }
        }
        RulePolicy::Proxy(_) => None,
    }
}

/// Builds one sing-box route rule object for a non-catch-all matcher.
/// `provider_tags` are the declared rule-provider tags (lowercased); a
/// GEOIP/GEOSITE rule whose code matches one renders as a `rule_set`
/// reference instead of being skipped.
fn matcher_object(
    matcher: &RuleMatch,
    outbound: &str,
    provider_tags: &std::collections::HashSet<String>,
) -> Option<RouteRule> {
    let mut object = RouteRule {
        domain: None,
        domain_keyword: None,
        domain_suffix: None,
        ip_cidr: None,
        outbound: outbound.to_owned(),
        rule_set: None,
        ip_is_private: None,
    };
    match matcher {
        RuleMatch::Domain(text) => {
            object.domain = Some(vec![text.as_str().to_owned()]);
        }
        RuleMatch::DomainSuffix(text) => {
            object.domain_suffix = Some(vec![text.as_str().to_owned()]);
        }
        RuleMatch::DomainKeyword(text) => {
            object.domain_keyword = Some(vec![text.as_str().to_owned()]);
        }
        RuleMatch::IpCidr(text) => {
            object.ip_cidr = Some(vec![text.as_str().to_owned()]);
        }
        RuleMatch::Geoip(code) => {
            // The plain `geoip` rule key was removed in sing-box 1.12, and
            // the SagerNet `.srs` rule-set source this renderer used to
            // point at was decommissioned upstream (repo reorganized, the
            // rule-set directory is gone — 2026-08). The lossless `private`
            // case renders natively; a rule whose code matches a declared
            // rule-provider renders as a `rule_set` reference to it; any
            // other GEOIP rule is skipped (counted and warned).
            if code.as_str().eq_ignore_ascii_case("private") {
                object.ip_is_private = Some(true);
                return Some(object);
            }
            if provider_tags.contains(&code.as_str().to_ascii_lowercase()) {
                object.rule_set = Some(code.as_str().to_owned());
                return Some(object);
            }
            return None;
        }
        RuleMatch::RuleSet(name) => {
            // The named provider supplies the matchers; the rule only names
            // it.
            object.rule_set = Some(name.as_str().to_owned());
        }
        RuleMatch::Geosite(code) => {
            // SagerNet's sing-geosite no longer ships per-category `.srs`
            // rule-sets; auto-referencing the dead URL made sing-box FATAL
            // at boot (实测: `core switch sing-box` 404, 2026-08-11). Skip
            // with a warning; a rule whose code matches a declared
            // rule-provider still renders as a `rule_set` reference.
            if provider_tags.contains(&code.as_str().to_ascii_lowercase()) {
                object.rule_set = Some(code.as_str().to_owned());
                return Some(object);
            }
            return None;
        }
        RuleMatch::ProcessName(_) | RuleMatch::SourceIpCidr(_) => {
            // sing-box 1.12 does not consume Mihomo's `PROCESS-NAME` or
            // `SRC-IP-CIDR` matchers; the rule renderer skips them so the
            // config can still be committed, and the count surfaces to the
            // caller through the existing `skipped` accounting.
            return None;
        }
        RuleMatch::Match => return None,
    }
    Some(object)
}

#[cfg(test)]
mod tests;
