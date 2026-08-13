//! Offline declared entry tree (`node list --format=tree`, W3a).
//!
//! The W3a tree surface renders the *declared* configuration — inline
//! provider nodes, schema `proxy_groups:` and `rules:` — through the
//! shared `entry_tree` renderer. It never touches the daemon: the
//! online wire carries no group membership until the W3b enrichment,
//! so `--format=tree` and `--offline --format=tree` are the same
//! offline projection today (cli-v3-design.md W3a 施工补裁 T-W3a).

use std::process::ExitCode;

use caly_platform::paths::AppPaths;
use caly_profile::loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits};

use crate::entry_tree::{TreeGroup, TreeRule, from_declared};

/// Renders the declared entry tree to stdout. `--enabled` filters
/// disabled groups (mirrors the `node list --offline --enabled`
/// table filter). Exit codes: 0 on success (including a legitimately
/// empty tree), 1 when the layered config cannot be read — a corrupt
/// config must never silently read as an empty tree.
pub fn render_declared_tree(paths: &AppPaths, enabled_only: bool, json: bool) -> ExitCode {
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    let config = match caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver) {
        Ok(config) => config,
        Err(error) => {
            return crate::client::output::report_failure(
                &format!("cannot read the layered config: {error}"),
                json,
            );
        }
    };

    // Inline provider nodes + declared groups come from the shared
    // builder so `node pick` / group `node test` type checks and member
    // lists never drift from the tree surface.
    let (groups, nodes) = crate::entry_tree::declared_groups(paths, &config, enabled_only);

    // Rules: schema lines are Clash-format text; a malformed line is
    // skipped with a warning (the tree walk never aborts), a well-formed
    // one gets its policy back-link resolved against the tree.
    let mut rules: Vec<TreeRule> = Vec::new();
    for line in &config.rules {
        match caly_domain::RoutingRule::from_clash_line(line) {
            Ok(rule) => rules.push(rule_from_declared(&rule, &groups, &nodes)),
            Err(_) => {
                eprintln!("node tree: skipping malformed rule line: {line}");
            }
        }
    }

    let tree = from_declared("config", groups, nodes, rules);
    if json {
        println!("{}", crate::entry_tree::render_json(&tree));
    } else {
        print!("{}", crate::entry_tree::render_human(&tree));
    }
    ExitCode::SUCCESS
}

/// Resolves a parsed routing rule's policy back-link against the
/// declared tree (groups first, then inline nodes; DIRECT/REJECT are
/// builtins).
fn rule_from_declared(
    rule: &caly_domain::RoutingRule,
    groups: &[TreeGroup],
    nodes: &[(String, String)],
) -> TreeRule {
    let text = rule.to_clash_line();
    let (target, target_kind) = match &rule.policy {
        caly_domain::RulePolicy::Direct => ("DIRECT".to_owned(), Some("direct".to_owned())),
        caly_domain::RulePolicy::Reject => ("REJECT".to_owned(), Some("reject".to_owned())),
        caly_domain::RulePolicy::Proxy(name) => {
            let name = name.as_str().to_owned();
            let kind = groups
                .iter()
                .find(|group| group.name == name)
                .map(|group| group.kind.clone())
                .or_else(|| {
                    nodes
                        .iter()
                        .find(|(tag, _)| *tag == name)
                        .map(|(_, kind)| kind.clone())
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
