//! Mihomo proxy-section vocabulary and assembly (`proxies`/`proxy-groups`/
//! `rules` YAML).
//!
//! Each dialable node is rendered into a bounded YAML block (see
//! `super::render`); unsupported protocols are skipped so a mixed real
//! subscription degrades to its representable subset rather than failing the
//! whole render. The final document is assembled by
//! [`super::MihomoConfigRenderer`] from this proxy section. Decoding a
//! subscription body into nodes is intake (`caly-profile`); fusing the two is
//! `caly-backends`' job.

use std::fmt::Write as FmtWrite;

use caly_domain::{BoundedText, BoundedVec, NodeId, RuleProviderSource, TextError};

use super::render::yaml_quote;

/// Maximum number of representable Mihomo proxies in one section.
pub const MAX_MIHOMO_PROXIES: usize = 10_000;
/// Maximum rendered YAML bytes for a single proxy block.
pub const MIHOMO_PROXY_YAML_MAX_BYTES: usize = 4_096;
/// Maximum proxy-group name length in bytes.
pub const MIHOMO_GROUP_MAX_BYTES: usize = 128;
/// Maximum proxy display-name (tag) length in bytes.
pub const MIHOMO_TAG_MAX_BYTES: usize = 256;

/// Bounded rendered Mihomo proxy block.
pub type MihomoProxyYaml = BoundedText<MIHOMO_PROXY_YAML_MAX_BYTES>;
/// Bounded proxy-group name.
pub type MihomoGroupName = BoundedText<MIHOMO_GROUP_MAX_BYTES>;
/// Bounded proxy display-name used as the Clash tag.
pub type MihomoProxyTag = BoundedText<MIHOMO_TAG_MAX_BYTES>;

/// One representable subscription node plus its rendered YAML block.
#[derive(Clone, Debug)]
pub struct MihomoProxyEntry {
    /// Stable selection identity; equals the dialable node identity.
    pub id: NodeId,
    /// Clash tag, also used by core selection and group membership.
    pub tag: MihomoProxyTag,
    /// Rendered `- name: ...` YAML block (already indented two spaces).
    pub yaml: MihomoProxyYaml,
}

/// Bounded ordered set of Mihomo proxies from one subscription body.
#[derive(Clone, Debug)]
pub struct MihomoProxySet {
    entries: BoundedVec<MihomoProxyEntry, MAX_MIHOMO_PROXIES>,
    group: MihomoGroupName,
}

impl MihomoProxySet {
    /// Builds a set from validated parts. Constructing from a raw
    /// subscription body (decode → dedupe → per-node render) is the
    /// intake/render fusion in `caly-backends`; the invariants guarded here
    /// are the bounded entry vec and the bounded group name.
    pub fn from_parts(
        entries: BoundedVec<MihomoProxyEntry, MAX_MIHOMO_PROXIES>,
        group: MihomoGroupName,
    ) -> Self {
        Self { entries, group }
    }

    /// Returns the proxy-group name this set feeds.
    pub fn group(&self) -> &str {
        self.group.as_str()
    }
    /// Returns the ordered entries as a slice.
    pub fn entries(&self) -> &[MihomoProxyEntry] {
        self.entries.as_slice()
    }
    /// Returns the number of representable proxies.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Returns whether no representable proxy exists.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Subscription-to-Mihomo rendering failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MihomoProxyError {
    /// The body could not be decoded as a supported subscription format.
    InvalidFormat,
    /// The decoded document is not a URI-line subscription.
    UnsupportedDocument,
    /// More representable nodes than the bounded capacity.
    TooManyProxies,
    /// No parseable node was representable as a Mihomo proxy.
    NoUsableNodes,
    /// A bounded field failed validation.
    Text(TextError),
}

impl From<TextError> for MihomoProxyError {
    fn from(value: TextError) -> Self {
        Self::Text(value)
    }
}

/// Emits `proxies:`, `proxy-groups:` and `rules:` YAML from a proxy set.
pub fn render_proxy_sections(set: &MihomoProxySet) -> String {
    let proxies = set
        .entries()
        .iter()
        .map(|entry| (entry.tag.as_str(), entry.yaml.as_str()))
        .collect::<Vec<_>>();
    render_proxy_sections_from(set.group(), &proxies)
}

/// Assembles `proxies:`, `proxy-groups:` and `rules:` YAML from `(tag, yaml)`
/// pairs. Shared by the subscription renderer and the config backend so the
/// proxy-group/rule template stays in one place.
pub fn render_proxy_sections_from<'a>(group: &str, proxies: &[(&'a str, &'a str)]) -> String {
    render_proxy_sections_with_rules(group, proxies, &[])
}

/// Renders proxies, proxy-groups and the routing `rules:` section. Configured
/// rules are emitted in order (first match wins); a `MATCH` catch-all for the
/// active group is appended only when the rule set does not already end one.
pub fn render_proxy_sections_with_rules<'a>(
    group: &str,
    proxies: &[(&'a str, &'a str)],
    rules: &[caly_domain::RoutingRule],
) -> String {
    render_proxy_sections_with_rules_and_providers(group, proxies, rules, &[])
}

/// Same as [`render_proxy_sections_with_rules`] but additionally renders a
/// `rule-providers:` block from the user-declared rule providers. Inline
/// providers become `type: inline`; HTTP providers become `type: http`
/// with the polling interval in seconds; file providers become
/// `type: file`. The `behavior` and `format` are forwarded verbatim so
/// the kernel honours the user's intent (domain vs ipcidr, source vs
/// binary).
pub fn render_proxy_sections_with_rules_and_providers<'a>(
    group: &str,
    proxies: &[(&'a str, &'a str)],
    rules: &[caly_domain::RoutingRule],
    rule_providers: &[caly_domain::RuleProvider],
) -> String {
    render_proxy_sections_with_declared_groups(group, proxies, rules, rule_providers, &[])
}

/// Same as [`render_proxy_sections_with_rules_and_providers`] but also
/// renders user-declared proxy groups.
///
/// User-declared groups are emitted **before** the implicit
/// subscription-derived `url-test` group so a `MATCH,<user group>`
/// rule can win over the catch-all. The implicit `url-test`
/// group is the safety net for configurations that have
/// no `MATCH` rule: the kernel still routes every
/// unmatched packet through a selector.
pub fn render_proxy_sections_with_declared_groups<'a>(
    group: &str,
    proxies: &[(&'a str, &'a str)],
    rules: &[caly_domain::RoutingRule],
    rule_providers: &[caly_domain::RuleProvider],
    proxy_groups: &[caly_domain::ProxyGroup],
) -> String {
    render_sections(
        proxies,
        rules,
        rule_providers,
        proxy_groups,
        Some(group),
        group,
    )
}

/// Renders the section for a subscription that **declares its own
/// proxy-groups** (2026-08-09 规划: the subscription document is the single
/// routing source when it carries groups). The declared groups are emitted
/// verbatim — including reference cycles the kernel will diagnose — and the
/// implicit `url-test` group is **not** rendered: the author's topology is
/// complete by construction. `match_fallback` (the first `select` group at
/// the call site) is used only when the merged rule table has no `MATCH`.
pub fn render_proxy_sections_with_subscription_groups<'a>(
    proxies: &[(&'a str, &'a str)],
    rules: &[caly_domain::RoutingRule],
    rule_providers: &[caly_domain::RuleProvider],
    subscription_groups: &[caly_domain::ProxyGroup],
    match_fallback: &str,
) -> String {
    render_sections(
        proxies,
        rules,
        rule_providers,
        subscription_groups,
        None,
        match_fallback,
    )
}

/// Shared renderer: `implicit` names the safety-net `url-test` group to emit
/// after the declared groups (`None` for subscription-owned routing), and
/// `match_fallback` renders the closing `MATCH,<...>` when the rule table
/// lacks one.
fn render_sections(
    proxies: &[(&str, &str)],
    rules: &[caly_domain::RoutingRule],
    rule_providers: &[caly_domain::RuleProvider],
    groups: &[caly_domain::ProxyGroup],
    implicit: Option<&str>,
    match_fallback: &str,
) -> String {
    let mut out = String::from("proxies:\n");
    for (_, yaml) in proxies {
        let _ = writeln!(out, "{yaml}");
    }
    let _ = writeln!(out, "proxy-groups:");
    for declared in groups {
        render_mihomo_proxy_group(&mut out, declared);
    }
    if let Some(group) = implicit {
        let _ = write!(out, "  - name: {}\n    type: url-test\n", yaml_quote(group));
        out.push_str("    url: \"http://www.gstatic.com/generate_204\"\n");
        out.push_str("    interval: 300\n");
        out.push_str("    tolerance: 50\n");
        out.push_str("    proxies:\n");
        for (tag, _) in proxies {
            let _ = writeln!(out, "      - {}", yaml_quote(tag));
        }
        // Deliberately NO `DIRECT` member here. A url-test group measures the
        // probe URL through every member; when the local network can reach that
        // URL directly (gstatic is reachable from CN), DIRECT always wins the
        // race and the whole proxy group silently becomes a no-op passthrough -
        // every "matched" connection then egresses from the local IP instead of
        // a node. All-dead nodes must surface as failures, not masquerade as
        // direct connectivity.
    }
    if !rule_providers.is_empty() {
        out.push_str("rule-providers:\n");
        for provider in rule_providers {
            render_mihomo_provider(&mut out, provider);
        }
    }
    let _ = writeln!(out, "rules:");
    let mut has_match = false;
    for rule in rules {
        let _ = writeln!(out, "  - {}", rule.to_clash_line());
        if matches!(rule.matcher, caly_domain::RuleMatch::Match) {
            has_match = true;
        }
    }
    if !has_match {
        let _ = writeln!(out, "  - MATCH,{match_fallback}");
    }
    out
}

/// Appends one user-declared rule provider to the rendered mihomo
/// `rule-providers:` block. The shape mirrors `rule-providers:`
/// (https://wiki.metacubex.one/en/config/rule-providers/): each entry
/// carries a discriminator (`type:`), a `behavior:` field, and a
/// `format:` field. The inline form is caly-specific — the upstream
/// spec does not define it, so we render the rule body under a
/// `payload:` key and let the kernel read the body verbatim.
fn render_mihomo_provider(out: &mut String, provider: &caly_domain::RuleProvider) {
    let name = yaml_quote(provider.name.as_str());
    let behavior = provider.behavior.clash_label();
    let format = provider.format.clash_label();
    match &provider.source {
        RuleProviderSource::Http { url, interval_ms } => {
            let interval_seconds = *interval_ms / 1_000;
            let url = yaml_quote(url.as_str());
            let _ = writeln!(
                out,
                "  {name}:\n    type: http\n    behavior: {behavior}\n    format: {format}\n    url: {url}\n    interval: {interval_seconds}\n"
            );
        }
        RuleProviderSource::File { path } => {
            let path = yaml_quote(path.as_str());
            let _ = writeln!(
                out,
                "  {name}:\n    type: file\n    behavior: {behavior}\n    format: {format}\n    path: {path}\n"
            );
        }
        RuleProviderSource::Inline { payload } => {
            // Inline providers carry the rule body under `payload:`.
            // caly's Mihomo backend (or any other kernel configured to
            // read the field) can materialise the body into a real
            // file before the kernel starts.
            let payload = yaml_quote(payload.as_str());
            let _ = writeln!(
                out,
                "  {name}:\n    type: inline\n    behavior: {behavior}\n    format: {format}\n    payload: {payload}\n"
            );
        }
    }
}

/// Appends one user-declared proxy group to the rendered mihomo
/// `proxy-groups:` block. The shape mirrors the Mihomo spec:
///
/// ```yaml
/// proxy-groups:
///   - name: <tag>
///     type: <select|url-test|fallback|load-balance|relay>
///     url: <probe>            # url-test / fallback / load-balance only
///     interval: <seconds>     # url-test / fallback / load-balance only
///     tolerance: <ms>         # url-test only
///     proxies:                # `members:` in the caly schema
///       - <tag>
///       - <tag>
///       - DIRECT
///       - REJECT
/// ```
///
/// The caly schema names the field `members:` for symmetry
/// with the singular `proxies:` field Mihomo uses; the
/// rendered block uses Mihomo's spelling. The schema
/// validator rejects probe-driven groups without a
/// `url_test:` block, so the renderer's `unwrap_or` of a
/// default probe here is only the cold-start fallback
/// for a future caller that bypasses validation.
fn render_mihomo_proxy_group(out: &mut String, group: &caly_domain::ProxyGroup) {
    let name = yaml_quote(group.name.as_str());
    let kind = group.kind.clash_label();
    let _ = writeln!(out, "  - name: {name}\n    type: {kind}");
    if let Some(url_test) = &group.url_test {
        let url = yaml_quote(url_test.url.as_str());
        let _ = writeln!(out, "    url: {url}");
        let _ = writeln!(out, "    interval: {}", url_test.interval_seconds);
        // `tolerance` is a Mihomo-specific knob for url-test
        // groups; `fallback` and `load-balance` ignore it
        // (the spec keeps the field for forward-compat).
        let _ = writeln!(out, "    tolerance: {}", url_test.tolerance_ms);
    }
    out.push_str("    proxies:\n");
    for member in &group.members {
        let rendered = match member {
            caly_domain::ProxyGroupMember::Node { tag } => yaml_quote(tag.as_str()),
            caly_domain::ProxyGroupMember::Group { name } => yaml_quote(name.as_str()),
            caly_domain::ProxyGroupMember::Direct => "DIRECT".to_owned(),
            caly_domain::ProxyGroupMember::Reject => "REJECT".to_owned(),
        };
        let _ = writeln!(out, "      - {rendered}");
    }
}

#[cfg(test)]
mod render_tests;

#[cfg(test)]
mod url_test_group_tests {
    use super::*;

    #[test]
    fn url_test_group_never_lists_direct_as_member() {
        let section = render_proxy_sections_with_rules("AUTO", &[("n1", "    - name: n1\n")], &[]);
        // group members live between `    proxies:` and `rules:`.
        let tail = section.split("    proxies:\n").nth(1);
        assert!(tail.is_some(), "group block");
        let members = tail
            .unwrap_or_default()
            .split("rules:\n")
            .next()
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("- "))
            .collect::<Vec<_>>();
        assert_eq!(members, vec!["- \"n1\""], "group must contain only proxies");
        // the rules tail still exists with the MATCH fallback
        assert!(section.contains("  - MATCH,AUTO"));
    }
}
