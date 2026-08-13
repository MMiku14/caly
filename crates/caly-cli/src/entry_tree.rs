//! Entry-tree renderer (cli-v3-design.md G1 / W3a).
//!
//! The shared 三区制 layout (策略组区 / 未入组节点区 / 规则区, C-F)
//! behind both offline entry surfaces:
//!
//! - `sub parse` — full tree from a Clash document
//!   ([`from_clash_import`]), degenerate protocol-only listing for
//!   URI-line documents ([`from_uri_nodes`]).
//! - `node list --offline --format=tree` — declared-config tree
//!   ([`from_declared`]).
//!
//! Pure display module: input is a typed entry model, output is a human
//! tree string or the §6.1 JSON contract. Never touches the daemon and
//! never reads anything beyond the caller-supplied model (show-family
//! discipline). Layout laws follow G-§3.2 / v3 §5.3:
//!
//! - declaration order is display order for groups, members and rules;
//! - nested groups show `→ 嵌套组(见 <name>)` without recursion; a
//!   reference cycle (schema-rejected, defensive here) shows `⟲ cycle`
//!   and never panics;
//! - a document with no groups omits the group zone entirely — the
//!   degenerate protocol-only listing is a legal form, not an error;
//! - residual unknown members / policies render as `[unknown]` and the
//!   walk continues.

use std::collections::HashSet;

use caly_domain::{ProxyGroupMember, RoutingRule};
use caly_subscription::ClashImport;
use serde_json::json;

/// How one entry's badge reads in the human tree and in the JSON `kind`
/// field. The CLI vocabulary maps domain spellings to the user-facing
/// ones (G-§3.1): `select`→`selector`, `url-test`→`urltest`,
/// `load-balance`→`loadbalance`, `shadowsocks`→`ss`. The mapping lives
/// only in this module — files keep their own spellings.
pub(crate) fn badge_kind(kind: &str) -> String {
    match kind {
        "select" => "selector".to_owned(),
        "url-test" => "urltest".to_owned(),
        "load-balance" => "loadbalance".to_owned(),
        "shadowsocks" => "ss".to_owned(),
        other => other.to_owned(),
    }
}

/// One entry in the tree: a protocol node, a group, or a builtin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeEntry {
    /// Display name (group tag / node tag / `DIRECT` / `REJECT`).
    pub(crate) name: String,
    /// Badge kind after [`badge_kind`] mapping.
    pub(crate) kind: String,
    /// `protocol` / `group` / `builtin`.
    pub(crate) entry_type: &'static str,
}

/// One member slot inside a group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TreeMember {
    /// A real node reference.
    Node { name: String, kind: String },
    /// A nested-group reference (`ref: true` in JSON).
    Group { name: String, kind: String },
    /// `DIRECT` / `REJECT`.
    Builtin { name: String, kind: String },
    /// Residual unknown reference (schema-rejected; defensive `[unknown]`).
    Unknown { name: String },
}

/// A declared (or imported) proxy group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeGroup {
    pub(crate) name: String,
    /// Badge kind (`selector` / `urltest` / `fallback` / `loadbalance` /
    /// `relay`).
    pub(crate) kind: String,
    pub(crate) url: Option<String>,
    pub(crate) interval_seconds: Option<u32>,
    pub(crate) tolerance_ms: Option<u32>,
    pub(crate) members: Vec<TreeMember>,
}

/// A protocol entry (grouped or ungrouped).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeNode {
    pub(crate) name: String,
    pub(crate) kind: String,
}

/// One routing rule with its policy back-link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeRule {
    pub(crate) text: String,
    pub(crate) target: String,
    pub(crate) target_kind: Option<String>,
}

/// The whole entry tree: three zones plus provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EntryTree {
    /// `clash-yaml` / `uri-lines` / `base64-uri-lines` / `config`.
    pub(crate) format: String,
    /// 策略组区 — declaration order.
    pub(crate) groups: Vec<TreeGroup>,
    /// 未入组节点区 — nodes not referenced by any group member list,
    /// in declaration order.
    pub(crate) ungrouped: Vec<TreeNode>,
    /// 规则区 — only the offline faces carry rules.
    pub(crate) rules: Vec<TreeRule>,
    /// Distinct builtins referenced (`DIRECT` / `REJECT`), declaration
    /// order of first use.
    pub(crate) builtins: Vec<TreeEntry>,
}

impl EntryTree {
    /// Total protocol entry count (grouped + ungrouped, deduplicated).
    pub(crate) fn protocol_count(&self) -> usize {
        let mut count = self.ungrouped.len();
        let mut seen: HashSet<&str> = self.ungrouped.iter().map(|n| n.name.as_str()).collect();
        for group in &self.groups {
            for member in &group.members {
                if let TreeMember::Node { name, .. } = member
                    && seen.insert(name.as_str())
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// Total entry count for §6.1 `counts.entries`.
    pub(crate) fn entry_count(&self) -> usize {
        self.protocol_count() + self.groups.len() + self.builtins.len()
    }

    /// Whether any group zone exists (drives the human layout: no groups
    /// means the degenerate protocol-only listing).
    pub(crate) fn has_groups(&self) -> bool {
        !self.groups.is_empty()
    }
}

// ── builders ───────────────────────────────────────────────────

/// Builds the tree from a parsed Clash import (`sub parse` path).
/// `format` is the document format label (`clash-yaml`).
pub(crate) fn from_clash_import(import: &ClashImport, format: &str) -> EntryTree {
    // Node kind lookup table: tag → badge kind.
    let mut node_kinds: Vec<(String, String)> = Vec::with_capacity(import.proxies.len());
    for node in &import.proxies {
        match node.display(true, None) {
            Ok(display) => node_kinds.push((
                display.name().as_str().to_owned(),
                badge_kind(display.protocol().as_str()),
            )),
            Err(_) => {
                // Display projection failure is unreachable for
                // schema-validated nodes; degrade to an [unknown] slot
                // rather than panic (defensive walk law).
                node_kinds.push(("unknown".to_owned(), "unknown".to_owned()));
            }
        }
    }

    let mut groups: Vec<TreeGroup> = import
        .proxy_groups
        .iter()
        .map(|group| build_group(group, &node_kinds))
        .collect();
    resolve_group_refs(&mut groups);

    let ungrouped = collect_ungrouped(&groups, &node_kinds);
    let builtins = collect_builtins(&groups);

    EntryTree {
        format: format.to_owned(),
        groups,
        ungrouped,
        rules: import
            .rules
            .iter()
            .map(|rule| build_rule(rule, import))
            .collect(),
        builtins,
    }
}

/// Builds the degenerate tree for URI-line documents: no group zone, no
/// rule zone — the legal protocol-only listing (G-§3.2). `nodes` are
/// `(tag, badge kind)` pairs in declaration order.
pub(crate) fn from_uri_nodes(format: &str, nodes: Vec<(String, String)>) -> EntryTree {
    EntryTree {
        format: format.to_owned(),
        groups: Vec::new(),
        ungrouped: nodes
            .into_iter()
            .map(|(name, kind)| TreeNode { name, kind })
            .collect(),
        rules: Vec::new(),
        builtins: Vec::new(),
    }
}

/// Builds the declared-config group list plus the inline-provider node
/// kind table (`(tag, badge kind)`), shared by the tree renderer
/// (`node list --offline --format=tree`) and the W4 online leaves
/// (`node pick` / group `node test`) so type checks and member lists
/// never drift between surfaces. `enabled_only` mirrors the
/// `node list --offline --enabled` table filter.
#[allow(clippy::too_many_lines)] // 声明组 + 订阅组统一(2026-08-12 组源统一)
pub(crate) fn declared_groups(
    paths: &caly_platform::paths::AppPaths,
    config: &caly_profile::schema::AppConfig,
    enabled_only: bool,
) -> (Vec<TreeGroup>, Vec<(String, String)>) {
    // Inline provider nodes: (tag, badge kind). The tag is the parsed
    // display name (the URI's `#fragment`); a URI that fails to parse
    // degrades to an [unknown] slot keyed by the raw text — the walk
    // never aborts.
    let mut nodes: Vec<(String, String)> = Vec::new();
    let subscription = caly_domain::SubscriptionId::from_bytes([0; 16]);
    for provider in &config.providers {
        if let caly_profile::schema::ProviderKind::InlineNodes(uris) = &provider.kind {
            for uri in uris {
                match caly_subscription::parse_any_proxy_uri(uri.as_str(), subscription)
                    .ok()
                    .and_then(|node| node.display(true, None).ok())
                {
                    Some(display) => nodes.push((
                        display.name().as_str().to_owned(),
                        badge_kind(display.protocol().as_str()),
                    )),
                    None => nodes.push((uri.clone(), "unknown".to_owned())),
                }
            }
        }
    }

    let mut groups: Vec<TreeGroup> = config
        .proxy_groups
        .iter()
        .filter(|group| !enabled_only || group.enabled)
        .map(|group| {
            let members: Vec<TreeMember> = group
                .members
                .iter()
                .map(|member| match member {
                    caly_profile::schema::ProxyGroupMemberConfig::Node { tag } => {
                        let name = tag.clone();
                        let kind = nodes
                            .iter()
                            .find(|(candidate, _)| *candidate == name)
                            .map_or_else(|| "unknown".to_owned(), |(_, kind)| kind.clone());
                        TreeMember::Node { name, kind }
                    }
                    caly_profile::schema::ProxyGroupMemberConfig::Group { name } => {
                        TreeMember::Group {
                            name: name.clone(),
                            // Resolved by the renderer's group-ref pass.
                            kind: "unknown".to_owned(),
                        }
                    }
                    caly_profile::schema::ProxyGroupMemberConfig::Direct => TreeMember::Builtin {
                        name: "DIRECT".to_owned(),
                        kind: "direct".to_owned(),
                    },
                    caly_profile::schema::ProxyGroupMemberConfig::Reject => TreeMember::Builtin {
                        name: "REJECT".to_owned(),
                        kind: "reject".to_owned(),
                    },
                })
                .collect();
            let (url, interval_seconds, tolerance_ms) = match &group.url_test {
                Some(probe) => (
                    Some(probe.url.clone()),
                    Some(probe.interval_seconds),
                    Some(probe.tolerance_ms),
                ),
                None => (None, None, None),
            };
            TreeGroup {
                name: group.name.clone(),
                kind: badge_kind(group.group_type.clash_label()),
                url,
                interval_seconds,
                tolerance_ms,
                members,
            }
        })
        .collect();
    // 组源统一 (2026-08-12): the daemon renders subscription-author groups
    // from the cached bodies, so the offline tree must show them too —
    // otherwise `node pick <订阅组>` fails the offline check while the
    // kernel has the group. Config groups win on name collisions; only
    // Clash-YAML bodies can carry groups (mirrors the daemon's routing
    // registry source).
    let mut seen: std::collections::HashSet<String> =
        groups.iter().map(|group| group.name.clone()).collect();
    let mut subscription_urls: Vec<&str> = config
        .subscriptions
        .sources
        .iter()
        .map(|source| source.url.as_str())
        .collect();
    if let Some(legacy) = config.subscriptions.url.as_deref() {
        subscription_urls.push(legacy);
    }
    for url in subscription_urls {
        let id = caly_subscription::subscription_id_for_url(url);
        let body_path = paths
            .state
            .join("subscriptions")
            .join(crate::client::hex(id.into_bytes()));
        let Ok(body) = std::fs::read(&body_path) else {
            continue;
        };
        let Some((subscription_groups, _)) = caly_subscription::clash_routing_from_body(&body, id)
        else {
            continue;
        };
        // 2026-08-13: the subscription's own nodes must join the node list
        // too — `clash_routing_from_body` only surfaces groups, so without
        // this the group members resolved to the `[unknown]` slot.
        if let Ok(import) = caly_subscription::parse_clash_config(
            std::str::from_utf8(&body).unwrap_or_default(),
            id,
        ) {
            for node in &import.proxies {
                if let Ok(display) = node.display(true, None) {
                    nodes.push((
                        display.name().to_string(),
                        badge_kind(display.protocol().as_str()),
                    ));
                }
            }
        }
        for group in subscription_groups {
            if seen.insert(group.name.as_str().to_owned()) {
                groups.push(build_group(&group, &nodes));
            }
        }
    }
    (groups, nodes)
}

/// Builds the declared-config tree (`node list --offline --format=tree`
/// path): `groups` are the schema-declared `proxy_groups:` (already
/// `enabled`-filtered by the caller), `nodes` are `(tag, badge kind)`
/// pairs resolved from the inline provider URIs, and `rules` are the
/// config `rules:` lines (parse failures degrade to `[unknown]`, never
/// abort the walk).
pub(crate) fn from_declared(
    format: &str,
    mut groups: Vec<TreeGroup>,
    nodes: Vec<(String, String)>,
    rules: Vec<TreeRule>,
) -> EntryTree {
    resolve_group_refs(&mut groups);
    let ungrouped = collect_ungrouped(&groups, &nodes);
    let builtins = collect_builtins(&groups);
    EntryTree {
        format: format.to_owned(),
        groups,
        ungrouped,
        rules,
        builtins,
    }
}

fn build_group(group: &caly_domain::ProxyGroup, node_kinds: &[(String, String)]) -> TreeGroup {
    let members: Vec<TreeMember> = group
        .members
        .iter()
        .map(|member| match member {
            ProxyGroupMember::Node { tag } => {
                let name = tag.as_str().to_owned();
                match node_kinds.iter().find(|(candidate, _)| *candidate == name) {
                    Some((_, kind)) => TreeMember::Node {
                        name,
                        kind: kind.clone(),
                    },
                    None => {
                        // Dangling node reference (schema-rejected;
                        // defensive `[unknown]` slot keeps the walk alive).
                        TreeMember::Unknown { name }
                    }
                }
            }
            ProxyGroupMember::Group { name } => TreeMember::Group {
                name: name.as_str().to_owned(),
                // Resolved by `resolve_group_refs` once the group set
                // is complete (forward references are legal in Clash).
                kind: "unknown".to_owned(),
            },
            ProxyGroupMember::Direct => TreeMember::Builtin {
                name: "DIRECT".to_owned(),
                kind: "direct".to_owned(),
            },
            ProxyGroupMember::Reject => TreeMember::Builtin {
                name: "REJECT".to_owned(),
                kind: "reject".to_owned(),
            },
        })
        .collect();
    let kind = badge_kind(group.kind.clash_label());
    let (url, interval_seconds, tolerance_ms) = match &group.url_test {
        Some(probe) => (
            Some(probe.url.as_str().to_owned()),
            Some(probe.interval_seconds),
            Some(probe.tolerance_ms),
        ),
        None => (None, None, None),
    };
    TreeGroup {
        name: group.name.as_str().to_owned(),
        kind,
        url,
        interval_seconds,
        tolerance_ms,
        members,
    }
}

/// Resolves nested-group member kinds once the full group set exists
/// (Clash allows forward references). A schema-rejected dangling
/// reference degrades to `[unknown]` — the walk never aborts.
fn resolve_group_refs(groups: &mut [TreeGroup]) {
    // Snapshot the name→kind map first (the mutable pass cannot hold a
    // borrow of `groups` while reading it back).
    let kinds: Vec<(String, String)> = groups
        .iter()
        .map(|group| (group.name.clone(), group.kind.clone()))
        .collect();
    for group in groups.iter_mut() {
        for member in &mut group.members {
            if let TreeMember::Group { name, kind } = member {
                *kind = kinds
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .map_or_else(|| "unknown".to_owned(), |(_, kind)| kind.clone());
            }
        }
    }
}

/// 未入组 = declared nodes not referenced by any group member list.
fn collect_ungrouped(groups: &[TreeGroup], node_kinds: &[(String, String)]) -> Vec<TreeNode> {
    let mut referenced: HashSet<&str> = HashSet::new();
    for group in groups {
        for member in &group.members {
            if let TreeMember::Node { name, .. } = member {
                referenced.insert(name.as_str());
            }
        }
    }
    node_kinds
        .iter()
        .filter(|(name, _)| !referenced.contains(name.as_str()))
        .map(|(name, kind)| TreeNode {
            name: name.clone(),
            kind: kind.clone(),
        })
        .collect()
}

fn collect_builtins(groups: &[TreeGroup]) -> Vec<TreeEntry> {
    let mut builtins: Vec<TreeEntry> = Vec::new();
    let mut seen = HashSet::new();
    for group in groups {
        for member in &group.members {
            if let TreeMember::Builtin { name, kind } = member
                && seen.insert(name.clone())
            {
                builtins.push(TreeEntry {
                    name: name.clone(),
                    kind: kind.clone(),
                    entry_type: "builtin",
                });
            }
        }
    }
    builtins
}

fn build_rule(rule: &RoutingRule, import: &ClashImport) -> TreeRule {
    let text = rule.to_clash_line();
    let (target, target_kind) = match &rule.policy {
        caly_domain::RulePolicy::Direct => ("DIRECT".to_owned(), Some("direct".to_owned())),
        caly_domain::RulePolicy::Reject => ("REJECT".to_owned(), Some("reject".to_owned())),
        caly_domain::RulePolicy::Proxy(name) => {
            let name = name.as_str().to_owned();
            let kind = import
                .proxy_groups
                .iter()
                .find(|group| group.name.as_str() == name)
                .map(|group| badge_kind(group.kind.clash_label()))
                .or_else(|| {
                    import
                        .proxies
                        .iter()
                        .find(|node| node_display_name(node).as_deref() == Some(name.as_str()))
                        .map(|node| badge_kind(node.protocol().label()))
                });
            (name, kind)
        }
    };
    TreeRule {
        text,
        target,
        target_kind,
    }
}

/// Credential-free display name of a node; `None` on a pathological
/// display projection (defensive — see the walk laws).
fn node_display_name(node: &caly_domain::DialableNode) -> Option<String> {
    node.display(true, None)
        .ok()
        .map(|display| display.name().as_str().to_owned())
}

/// Marks a group member slot `⟲ cycle` when the referenced group
/// transitively references back. Defensive only — the schema validator
/// rejects cycles; the walk must still terminate and never panic.
fn member_cycles(groups: &[TreeGroup]) -> Vec<Vec<bool>> {
    let mut cycles: Vec<Vec<bool>> = Vec::with_capacity(groups.len());
    for owner in groups {
        let mut flags = vec![false; owner.members.len()];
        for (index, member) in owner.members.iter().enumerate() {
            if let TreeMember::Group { name, .. } = member
                && reaches_back(
                    groups,
                    name.as_str(),
                    owner.name.as_str(),
                    &mut HashSet::new(),
                )
            {
                flags[index] = true;
            }
        }
        cycles.push(flags);
    }
    cycles
}

/// Whether `from` (a group tag) transitively references `target`
/// through group member slots. `seen` bounds the walk; a schema-valid
/// document terminates immediately at depth 1.
fn reaches_back(
    groups: &[TreeGroup],
    from: &str,
    target: &str,
    seen: &mut HashSet<String>,
) -> bool {
    if from == target {
        return true;
    }
    if !seen.insert(from.to_owned()) {
        return false;
    }
    let Some(group) = groups.iter().find(|candidate| candidate.name == from) else {
        return false;
    };
    for member in &group.members {
        if let TreeMember::Group { name, .. } = member
            && reaches_back(groups, name.as_str(), target, seen)
        {
            return true;
        }
    }
    false
}

// ── human rendering ─────────────────────────────────────────────

/// Renders the human tree. Layout: group zone (one block per group,
/// members indented with `├─`/`└─`), then 未入组节点 zone, then 规则区
/// (offline faces only). A document without groups degenerates to a
/// protocol-only listing.
/// Left-pads the `[kind]` badge to the 13-column badge lane followed
/// by two spaces, so names align across badge widths (cli-v3-design.md
/// §5.3). The lane is 13 because the widest badge — `[loadbalance]` at
/// 13 columns — must not push its row's name column right of the others
/// (2026-08-12 audit: lane 10 then 12 both misaligned it). Shared with
/// the W4/§7 picker rows so menu columns line up with the tree.
pub(crate) fn badge_lane(kind: &str) -> String {
    const BADGE_LANE_WIDTH: usize = 13;
    let badge = format!("[{kind}]");
    let mut lane = badge.clone();
    for _ in badge.chars().count()..BADGE_LANE_WIDTH {
        lane.push(' ');
    }
    lane.push_str("  ");
    lane
}

/// Strips control and format characters from a display string
/// (W3a deep-review R-1, plus 2026-08-12 agent audit): the human tree
/// and picker rows render subscription-derived names and rule text, and
/// a hostile document could otherwise inject ANSI sequences
/// (`\x1b[2J` clear-screen, colour forgery, `\r` overwrites), bidi
/// overrides (`U+202A-202E` — RLO can reorder a display name into an
/// impersonation), line/paragraph separators (`U+2028/2029` — fake
/// rows), zero-width marks and word joiners (`U+200B-200F`, `U+2060-206F`,
/// `U+FEFF`) into the operator's terminal. The JSON face keeps the raw
/// bytes — machine consumers make their own policy. Mirrors the
/// node-name sanitisation in `caly-domain` (`sanitized_display_name`).
/// Shared with the W4/§7 picker labels so menu rows sanitize the same
/// way the tree does.
pub(crate) fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !ch.is_control()
                && !matches!(
                    *ch as u32,
                    // Bidi overrides / isolates (incl. U+061C ALM — the
                    // fifth Bidi_Control code point), line & paragraph
                    // separators, ZWSP/ZWNJ/ZWJ/LTR-RTL marks, word
                    // joiners, variation selectors (incl. the
                    // supplementary plane E0100–E01EF), BOM.
                    0x061C
                        | 0x202A..=0x202E
                        | 0x2028
                        | 0x2029
                        | 0x200B..=0x200F
                        | 0x2060..=0x206F
                        | 0xE0100..=0xE01EF
                        | 0xFE00..=0xFE0F
                        | 0xFEFF
                )
        })
        .collect()
}

pub(crate) fn render_human(tree: &EntryTree) -> String {
    let mut out = String::new();
    let cycles = member_cycles(&tree.groups);
    if tree.has_groups() {
        for (index, group) in tree.groups.iter().enumerate() {
            render_group_header(&mut out, group);
            render_members(&mut out, group, &cycles[index]);
        }
        out.push('\n');
    }
    if !tree.ungrouped.is_empty() {
        if tree.has_groups() {
            // Document with groups: the 未入组节点 zone (G-§3.2).
            out.push_str("ungrouped nodes\n");
            for (index, node) in tree.ungrouped.iter().enumerate() {
                let branch = if index + 1 == tree.ungrouped.len() {
                    "└─"
                } else {
                    "├─"
                };
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!(
                        "  {branch} {}{}\n",
                        badge_lane(&node.kind),
                        strip_controls(&node.name)
                    ),
                );
            }
            out.push('\n');
        } else {
            // Group-less document (URI lines / base64): the degenerate
            // protocol-only listing (G-§3.2) — no zone title, no tree
            // glyphs.
            for node in &tree.ungrouped {
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!("{}{}\n", badge_lane(&node.kind), strip_controls(&node.name)),
                );
            }
            out.push('\n');
        }
    }
    if !tree.rules.is_empty() {
        out.push_str("rules zone\n");
        for rule in &tree.rules {
            let kind = rule.target_kind.as_deref().unwrap_or("unknown");
            // The target column shares the 10-column badge lane with the
            // group/member zones, so rule targets align across badge
            // widths (`→ [direct]  DIRECT` / `→ [selector] Telegram`).
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "  {} → {}{}\n",
                    strip_controls(&rule.text),
                    badge_lane(kind),
                    strip_controls(&rule.target)
                ),
            );
        }
    }
    if out.is_empty() {
        out.push_str("(no entries)\n");
    }
    out
}

/// Renders one group's header line: `[badge] name` with the probe
/// annotation when the group probes (`url=… · interval=…s · tolerance=…ms`).
fn render_group_header(out: &mut String, group: &TreeGroup) {
    let _ = std::fmt::Write::write_fmt(
        out,
        format_args!(
            "{}{}{}\n",
            badge_lane(&group.kind),
            strip_controls(&group.name),
            probe_note(group)
        ),
    );
}

fn probe_note(group: &TreeGroup) -> String {
    let Some(url) = &group.url else {
        return String::new();
    };
    let mut note = format!("  url={}", strip_controls(url));
    if let Some(interval) = group.interval_seconds {
        let _ = std::fmt::Write::write_fmt(&mut note, format_args!(" · interval={interval}s"));
    }
    if let Some(tolerance) = group.tolerance_ms {
        let _ = std::fmt::Write::write_fmt(&mut note, format_args!(" · tolerance={tolerance}ms"));
    }
    note
}

fn render_members(out: &mut String, group: &TreeGroup, cycle_flags: &[bool]) {
    for (index, member) in group.members.iter().enumerate() {
        let branch = if index + 1 == group.members.len() {
            "└─"
        } else {
            "├─"
        };
        match member {
            TreeMember::Node { name, kind } | TreeMember::Builtin { name, kind } => {
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!("  {branch} {}{}\n", badge_lane(kind), strip_controls(name)),
                );
            }
            TreeMember::Group { name, kind } => {
                if cycle_flags.get(index).copied().unwrap_or(false) {
                    let _ = std::fmt::Write::write_fmt(
                        out,
                        format_args!(
                            "  {branch} {}{} → ⟲ cycle\n",
                            badge_lane(kind),
                            strip_controls(name)
                        ),
                    );
                } else {
                    let display = strip_controls(name);
                    let _ = std::fmt::Write::write_fmt(
                        out,
                        format_args!(
                            "  {branch} {}{} → 嵌套组(见 {display})\n",
                            badge_lane(kind),
                            display
                        ),
                    );
                }
            }
            TreeMember::Unknown { name } => {
                let _ = std::fmt::Write::write_fmt(
                    out,
                    format_args!(
                        "  {branch} {}{}\n",
                        badge_lane("unknown"),
                        strip_controls(name)
                    ),
                );
            }
        }
    }
}

// ── JSON rendering (§6.1 contract, frozen from the W3a exit) ────

/// Renders the §6.1 C-C JSON contract: flat `entries[]` + `members[]`
/// two-level shape. `entries[]` order: groups (declaration order), then
/// protocols (ungrouped first, then grouped, both in declaration order),
/// then the distinct builtins. Optional fields (`delay_ms`, `selected`,
/// `groups_in`) are omitted when absent (serde skip_none semantics);
/// W3b's online enrichment adds them without renaming or removing.
pub(crate) fn render_json(tree: &EntryTree) -> serde_json::Value {
    let mut entries: Vec<serde_json::Value> = Vec::new();

    for group in &tree.groups {
        let mut object = json!({
            "name": group.name,
            "kind": group.kind,
            "type": "group",
        });
        if let Some(url) = &group.url {
            object["url"] = json!(url);
        }
        if let Some(interval) = group.interval_seconds {
            object["interval_seconds"] = json!(interval);
        }
        if let Some(tolerance) = group.tolerance_ms {
            object["tolerance_ms"] = json!(tolerance);
        }
        let members: Vec<serde_json::Value> = group
            .members
            .iter()
            .map(|member| match member {
                TreeMember::Node { name, kind } | TreeMember::Builtin { name, kind } => {
                    json!({ "name": name, "kind": kind })
                }
                TreeMember::Group { name, kind } => {
                    json!({ "name": name, "kind": kind, "ref": true })
                }
                TreeMember::Unknown { name } => json!({ "name": name, "kind": "unknown" }),
            })
            .collect();
        object["members"] = serde_json::Value::Array(members);
        entries.push(object);
    }

    for node in &tree.ungrouped {
        entries.push(json!({
            "name": node.name,
            "kind": node.kind,
            "type": "protocol",
        }));
    }
    let mut seen: HashSet<&str> = tree.ungrouped.iter().map(|n| n.name.as_str()).collect();
    for group in &tree.groups {
        for member in &group.members {
            if let TreeMember::Node { name, kind } = member
                && seen.insert(name.as_str())
            {
                entries.push(json!({
                    "name": name,
                    "kind": kind,
                    "type": "protocol",
                    "groups_in": groups_in_for(name, &tree.groups),
                }));
            }
        }
    }

    for builtin in &tree.builtins {
        entries.push(json!({
            "name": builtin.name,
            "kind": builtin.kind,
            "type": "builtin",
        }));
    }

    let rules: Vec<serde_json::Value> = tree
        .rules
        .iter()
        .map(|rule| {
            let mut object = json!({ "text": rule.text, "target": rule.target });
            if let Some(kind) = &rule.target_kind {
                object["target_kind"] = json!(kind);
            }
            object
        })
        .collect();

    json!({
        "format": tree.format,
        "ok": true,
        "counts": {
            "entries": tree.entry_count(),
            "protocols": tree.protocol_count(),
            "groups": tree.groups.len(),
            "builtins": tree.builtins.len(),
            "rules": tree.rules.len(),
        },
        "entries": entries,
        "rules": rules,
    })
}

/// Member groups of one protocol tag, in group declaration order.
fn groups_in_for(name: &str, groups: &[TreeGroup]) -> Vec<String> {
    let mut names = Vec::new();
    for group in groups {
        for member in &group.members {
            if let TreeMember::Node { name: tag, .. } = member
                && tag == name
            {
                names.push(group.name.clone());
                break;
            }
        }
    }
    names
}

#[cfg(test)]
mod tests;
