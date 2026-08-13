//! `set core …` dispatch.
//!
//! Round 15: each `SetCoreCmd` variant maps to the typed
//! `client::ClientCommand::Core(...)` path directly (the
//! `bridge` shim was retired). Round 16: `url-test` is
//! the single-node `core delay` path with explicit
//! `--url`. Round 24 (debug): the `--timeout` flag on
//! `url-test` was silently dropped by the dispatch (the
//! offline `Query::UrlTest` resolver has a hard-coded
//! `QUERY_TIMEOUT` constant, no per-call override path).
//! The flag is removed from the grammar so the operator
//! sees a clap error instead of a no-op success.
//!
//! W4 (cli-v3-design.md §12): the `node pick` leaf and the
//! group face of `node test` land here. Both are *group
//! control* writes: the offline declared tree is the
//! authority for group kind and member spelling, dry-run is
//! the default (写面三轨制: 组控制 dry-run), and `--apply`
//! commits through the daemon / controller.

use std::process::ExitCode;

use crate::cli::SetCoreCmd;
use crate::client::legacy::CoreCmd;
use crate::entry_tree::{TreeGroup, TreeMember};
use crate::output::CliOutput;

pub fn dispatch(c: SetCoreCmd, options: crate::cli::CliOptions, _output: CliOutput) -> ExitCode {
    let mapped = match c {
        SetCoreCmd::Start => CoreCmd::Start,
        SetCoreCmd::Stop => CoreCmd::Stop,
        SetCoreCmd::Restart => CoreCmd::Restart,
        SetCoreCmd::Switch(core) => CoreCmd::Switch(core),
        SetCoreCmd::Select { node, delay, poll } => CoreCmd::Select { node, delay, poll },
        SetCoreCmd::Mode(m) => CoreCmd::Mode(m),
        SetCoreCmd::CloseConnections => CoreCmd::CloseConnections,
        SetCoreCmd::Delay {
            name,
            all,
            url,
            samples,
        } => CoreCmd::Delay {
            all,
            name,
            url,
            samples,
        },
        // W4: `node test <name>` gains the group face. A declared
        // urltest/fallback group re-tests all members (dry-run by
        // default; `--apply` commits the probe, whose side-effect is
        // the kernel re-selecting the fastest member); a selector
        // group is a type misuse (exit 2). Anything else keeps the
        // Round 15 single-entry path — the `--apply` flag is ignored
        // there.
        SetCoreCmd::UrlTest {
            name,
            url,
            samples,
            apply,
        } => return dispatch_url_test(&name, url, samples, apply, options),
        // W4 (`node pick`, cli-v3-design.md §4.2 C-B): pick a member
        // inside a selector group. Type checks and member validation
        // run against the offline declared tree; the daemon resolves
        // kernel-side tags.
        SetCoreCmd::Pick {
            group,
            member,
            apply,
            dry_run,
        } => return dispatch_pick(&group, member.as_deref(), apply, dry_run, options),
    };
    crate::client::run_client(crate::ClientCommand::Core(mapped), options)
}

/// Loads the declared `proxy_groups:` through the shared builder used by
/// the tree surface, so `node pick` / group `node test` type checks and
/// member lists never drift from `node list --offline --format=tree`.
/// A corrupt or missing config degrades to `Ok(vec![])` for the test
/// face (the single-entry path must stay usable without a config) and
/// to a terminal error for pick (a group control write without a
/// declared tree cannot validate anything).
fn declared_groups(options: &crate::cli::CliOptions) -> Result<Vec<TreeGroup>, ExitCode> {
    let paths = caly_platform::paths::AppPaths::from_env();
    match crate::client::config_generate::load_config_with_bootstrap(&paths, options.json) {
        Ok(config) => Ok(crate::entry_tree::declared_groups(&paths, &config, false).0),
        Err(error) => {
            let _ = crate::client::output::report_failure(
                &format!("cannot read the layered config: {error}"),
                options.json,
            );
            Err(ExitCode::from(1))
        }
    }
}

/// The W4 group faces share one look-up: find the declared group, then
/// judge its kind against the leaf's legal kinds. `Ok(None)` means the
/// name is not a declared group (the caller falls back to the
/// single-entry face); `Err` carries the leaf's usage/failure exit.
fn declared_group<'a>(groups: &'a [TreeGroup], name: &str) -> Option<&'a TreeGroup> {
    // Exact spelling first, then a case-insensitive fallback — group
    // names are path-safe ASCII, so a case-only mismatch is a typing
    // slip, not an ambiguity (2026-08-12 CLI audit: members matched
    // case-insensitively while groups did not).
    groups.iter().find(|group| group.name == name).or_else(|| {
        groups
            .iter()
            .find(|group| group.name.eq_ignore_ascii_case(name))
    })
}

fn member_name(member: &TreeMember) -> &str {
    match member {
        TreeMember::Node { name, .. }
        | TreeMember::Group { name, .. }
        | TreeMember::Builtin { name, .. }
        | TreeMember::Unknown { name } => name.as_str(),
    }
}

/// W4 group face of `node test`: a declared urltest/fallback group
/// re-tests every member through the kernel's `GET /proxies/{name}/delay`
/// (which updates the selection as its side-effect). Dry-run is the
/// default; the single-entry face is untouched.
fn dispatch_url_test(
    name: &str,
    url: Option<String>,
    samples: Option<u32>,
    apply: bool,
    options: crate::cli::CliOptions,
) -> ExitCode {
    let json = options.json;
    if let Ok(groups) = declared_groups(&options)
        && let Some(group) = declared_group(&groups, name)
    {
        match group.kind.as_str() {
            // cli-v3-design.md §4.2: urltest/fallback = re-test + reselect.
            "urltest" | "fallback" => {
                if !apply {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "ok": true,
                                "command": "node test",
                                "group": name,
                                "members": group.members.len(),
                                "dry_run": true,
                            })
                        );
                    } else {
                        println!(
                            "would test group `{name}` ({} members) — dry-run, `--apply` to commit",
                            group.members.len()
                        );
                    }
                    return ExitCode::SUCCESS;
                }
                // Commit: the single-entry probe path already dials
                // `GET /proxies/{name}/delay`; for a group the kernel
                // re-tests every member and re-selects the fastest.
                return crate::client::run_client(
                    crate::ClientCommand::Core(CoreCmd::Delay {
                        all: false,
                        name: Some(name.to_owned()),
                        url,
                        samples,
                    }),
                    options,
                );
            }
            // §4.2: a selector has no latency logic.
            "selector" => {
                return crate::client::output::report_usage_error(
                    &format!(
                        "selector groups have no latency logic; use `caly node ping {name}` to probe their members"
                    ),
                    json,
                );
            }
            other => {
                return crate::client::output::report_usage_error(
                    &format!(
                        "`node test` applies to urltest/fallback groups; `{name}` is a {other} group"
                    ),
                    json,
                );
            }
        }
    }
    // Single-entry face (Round 15 path). A missing config degrades here
    // silently — the single-entry probe never needed the config.
    crate::client::run_client(
        crate::ClientCommand::Core(CoreCmd::Delay {
            all: false,
            name: Some(name.to_owned()),
            url,
            samples,
        }),
        options,
    )
}

/// W4 `node pick`: pick a member inside a selector group.
///
/// Contract (cli-v3-design.md §4.2 / §7 / G3):
/// - group must be a declared selector (exit 2 on auto-managed groups
///   and non-selector kinds — "类型误触 exit 2");
/// - member must belong to the group (exit 1 — "成员校验 exit 1");
/// - omitted member on a live terminal opens the interactive picker
///   (Esc → exit 1 `cancelled, nothing changed`, Ctrl-C → 130); a
///   piped shell gets the usage error with the member list (exit 2);
/// - dry-run by default (组控制写面三轨制), `--apply` commits through
///   `WireCommand::SelectProxyGroup`.
fn dispatch_pick(
    group: &str,
    member: Option<&str>,
    apply: bool,
    dry_run: bool,
    options: crate::cli::CliOptions,
) -> ExitCode {
    let json = options.json;
    let groups = match declared_groups(&options) {
        Ok(groups) => groups,
        Err(code) => return code,
    };
    let Some(target) = declared_group(&groups, group) else {
        // R-G2: a group the daemon does not know about is a live-config
        // problem, not a typing mistake.
        return crate::client::output::report_failure(
            &format!(
                "group `{group}` is not in the active configuration; enable it with `caly node enable {group}` or refresh the subscription first"
            ),
            json,
        );
    };
    match target.kind.as_str() {
        // The only legal target.
        "selector" => {}
        // §4.2: auto-managed groups answer with the re-test leaf.
        "urltest" | "fallback" => {
            return crate::client::output::report_usage_error(
                &format!(
                    "group `{group}` is auto-managed; use `caly node test {group}` to re-test it"
                ),
                json,
            );
        }
        other => {
            return crate::client::output::report_usage_error(
                &format!(
                    "`node pick` applies to selector groups only; `{group}` is a {other} group"
                ),
                json,
            );
        }
    }
    if target.members.is_empty() {
        return crate::client::output::report_failure(
            &format!("group `{group}` declares no members; nothing to pick"),
            json,
        );
    }
    let member = match member {
        Some(member) => member.to_owned(),
        None if crate::client::interact::interactive_capable() => {
            match pick_member_interactively(group, target) {
                // The picker payload is already the declared spelling;
                // re-resolving it case-insensitively could pick a
                // *different* member when the group declares both
                // `HK-01` and `hk-01` (2026-08-12 agent audit).
                Ok(member) => return finish_pick(group, &member, apply, dry_run, json, options),
                Err(code) => return code,
            }
        }
        None => {
            let available: Vec<&str> = target.members.iter().map(member_name).collect();
            return crate::client::output::report_usage_error(
                &format!(
                    "node pick requires <member> in non-interactive mode. Available members of `{group}`: {}",
                    available.join(", ")
                ),
                json,
            );
        }
    };
    // Non-interactive spellings are canonicalized: an exact declared
    // match wins, then case-insensitive ones (builtins render as
    // `DIRECT`/`REJECT` while operators type `direct`). A group that
    // declares both `HK-01` and `hk-01` must not silently pick the
    // first case-insensitive hit — every ambiguous match is reported
    // (2026-08-12 agent audit).
    let mut candidates = target
        .members
        .iter()
        .map(member_name)
        .filter(|candidate| *candidate == member);
    let canonical = match (candidates.next(), candidates.next()) {
        (Some(exact), None) => exact,
        (Some(_), Some(_)) => {
            // Duplicate exact spellings are schema-rejected; unreachable.
            unreachable!("duplicate exact member names are schema-rejected")
        }
        _ => {
            let fuzzy: Vec<&str> = target
                .members
                .iter()
                .map(member_name)
                .filter(|candidate| candidate.eq_ignore_ascii_case(&member))
                .collect();
            match fuzzy.as_slice() {
                [one] => *one,
                [] => {
                    return crate::client::output::report_failure(
                        &format!(
                            "member `{member}` is not in group `{group}`; run `caly node list --offline --format=tree` for the declared members"
                        ),
                        json,
                    );
                }
                ambiguous => {
                    return crate::client::output::report_usage_error(
                        &format!(
                            "member `{member}` is ambiguous in group `{group}`: {} — pass the exact spelling",
                            ambiguous.join(", ")
                        ),
                        json,
                    );
                }
            }
        }
    };
    finish_pick(group, canonical, apply, dry_run, json, options)
}

/// Shared tail of [`dispatch_pick`]: dry-run preview by default, wire
/// commit under `--apply`.
fn finish_pick(
    group: &str,
    member: &str,
    apply: bool,
    dry_run: bool,
    json: bool,
    options: crate::cli::CliOptions,
) -> ExitCode {
    if !apply || dry_run {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "command": "node pick",
                    "group": group,
                    "member": member,
                    "dry_run": true,
                })
            );
        } else {
            println!("would select `{member}` in group `{group}` — dry-run, `--apply` to commit");
        }
        return ExitCode::SUCCESS;
    }
    crate::client::run_client(
        crate::ClientCommand::PickProxyGroup {
            group: group.to_owned(),
            member: member.to_owned(),
        },
        options,
    )
}

/// Interactive member picker (W4 §7): a live terminal without an explicit
/// member gets the inquire menu. Esc → `Err(exit 1)` after printing
/// `cancelled, nothing changed`; Ctrl-C → `Err(130)` via the picker
/// contract. Long member pools (>10) auto-switch to the fuzzy filter
/// (type-to-search) inside [`crate::client::interact::pick`].
fn pick_member_interactively(group: &str, target: &TreeGroup) -> Result<String, ExitCode> {
    let items: Vec<crate::client::interact::PickItem> =
        target.members.iter().map(member_pick_item).collect();
    match crate::client::interact::pick(&format!("Select member for `{group}`"), &items) {
        crate::client::interact::PickOutcome::Selected(index) => Ok(items[index].payload.clone()),
        crate::client::interact::PickOutcome::Escaped => {
            eprintln!("{}", crate::output::CANCELLED_NOTHING_CHANGED);
            Err(ExitCode::FAILURE)
        }
        crate::client::interact::PickOutcome::Interrupted => Err(ExitCode::from(130)),
    }
}

/// Builds one picker row for a declared member: payload is the member
/// name (the wire spelling), label is the §7 layout — `[kind]` badge in
/// the shared 10-column lane, name, then a latency placeholder column
/// (`—` offline; the online select fills the real delay).
fn member_pick_item(member: &TreeMember) -> crate::client::interact::PickItem {
    let (name, kind) = match member {
        TreeMember::Node { name, kind } | TreeMember::Builtin { name, kind } => {
            (name.as_str(), kind.as_str())
        }
        TreeMember::Group { name, .. } => (name.as_str(), "group"),
        TreeMember::Unknown { name } => (name.as_str(), "unknown"),
    };
    crate::client::interact::PickItem {
        payload: name.to_owned(),
        // The label is hostile-input territory (subscription names);
        // sanitize the same way the tree does so a `%1B[` / bidi /
        // newline payload cannot forge ANSI or fake menu rows.
        label: format!(
            "{}{}  —",
            crate::entry_tree::badge_lane(kind),
            crate::entry_tree::strip_controls(name)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(kind: &str, name: &str) -> TreeMember {
        match kind {
            "node" => TreeMember::Node {
                name: name.to_owned(),
                kind: "vmess".to_owned(),
            },
            "direct" => TreeMember::Builtin {
                name: name.to_owned(),
                kind: "direct".to_owned(),
            },
            "group" => TreeMember::Group {
                name: name.to_owned(),
                kind: "unknown".to_owned(),
            },
            _ => TreeMember::Unknown {
                name: name.to_owned(),
            },
        }
    }

    /// §7 sample layout: the picker row carries the `[kind]` badge in the
    /// 12-column lane, the member name, and the latency placeholder — so
    /// `[vmess]        hk-01` and `[direct]       DIRECT` line up.
    #[test]
    fn member_pick_item_uses_the_badge_lane_layout() {
        let node = member_pick_item(&member("node", "hk-01"));
        assert_eq!(node.payload, "hk-01");
        assert_eq!(node.label, "[vmess]        hk-01  —");
        let direct = member_pick_item(&member("direct", "DIRECT"));
        assert_eq!(direct.label, "[direct]       DIRECT  —");
        // CJK names keep their visible width in the label (inquire
        // measures with unicode-width; the lane is spaces so alignment
        // is byte-agnostic).
        let cjk = member_pick_item(&member("node", "日本-香港一"));
        assert_eq!(cjk.label, "[vmess]        日本-香港一  —");
    }

    /// The payload is the wire spelling — what `--apply` sends — not the
    /// decorated label.
    #[test]
    fn member_pick_item_payload_is_the_plain_name() {
        assert_eq!(
            member_pick_item(&member("direct", "DIRECT")).payload,
            "DIRECT"
        );
        assert_eq!(member_pick_item(&member("group", "auto")).payload, "auto");
    }
}
