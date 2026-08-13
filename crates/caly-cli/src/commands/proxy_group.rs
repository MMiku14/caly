//! Round 20: `set proxy-group` CLI leaf dispatcher.
//!
//! Each `SetProxyGroupCmd` variant is one leaf in the
//! `set proxy-group` family. The 5 CRUD leaves
//! (`add` / `remove` / `enable` / `disable` / `list`)
//! round-trip `proxy_groups: Vec<ProxyGroupConfig>` in
//! `config.yaml` through `client::proxy_group`. The
//! shared `commands::set::common::run_standard_writer`
//! envelope (Round 29) does the success / dry-run /
//! error projection so the dispatch stays a
//! one-page file with **one-line** CRUD bodies.
//!
//! The `add` leaf is **one** variant (not one per
//! `type:` kind) because every `proxy_groups:` entry
//! has the same shape; the `type:` field is a
//! discriminator the writer forwards verbatim to the
//! schema layer. Probe-driven groups (`url-test` /
//! `fallback` / `load-balance`) require `--url`; the
//! schema validator surfaces a missing probe as a
//! `MissingUrl` error so the operator sees a
//! precise reason.

use std::process::ExitCode;

use crate::cli::{ProxyGroupMemberSpec, SetProxyGroupCmd};
use crate::client::proxy_group as cmd;
use crate::commands::set::common::{crud_dispatch, run_standard_writer, ResourceVerb};
use crate::output::{self, CliOutput};

/// The singular resource name used by
/// [`run_standard_writer`] for every `set
/// proxy-group` CRUD leaf. Kept as a `const` so the
/// `Summaries::standard(NOUN, verb)` call inside
/// the helper doesn't pay a string-allocation cost
/// on every dispatch.
const NOUN: &str = "proxy group";

/// The leaf-prefix token (the second token in the
/// `set proxy-group <verb> <id>` leaf string). Kept
/// distinct from [`NOUN`] because the leaf wants
/// the CLI form (`"set proxy-group add Proxy"`)
/// while the human summary wants the long form
/// (`"proxy group added"`).
const CLI_PREFIX: &str = "proxy-group";

/// Projects a `cmd::PgWriteError` to the stable CLI
/// `code:` used in the JSON error envelope. The
/// `Shared` arm covers the `ResourceError::Write` /
/// `Parse` / `Validate` variants the writer passes
/// through.
fn code_for(error: &cmd::PgWriteError) -> &'static str {
    use cmd::PgWriteError as E;
    match error {
        E::InvalidName(_) => "proxy_group.invalid_name",
        E::AlreadyDeclared(_) => "proxy_group.already_declared",
        E::NotDeclared(_) => "proxy_group.not_declared",
        E::MissingUrl(_) => "proxy_group.missing_url",
        E::UnexpectedUrl(_) => "proxy_group.unexpected_url",
        E::Shared(shared) => match shared {
            crate::client::resource_writer::ResourceError::Invalid(_) => "proxy_group.invalid",
            crate::client::resource_writer::ResourceError::Write(_) => "proxy_group.write_failed",
            crate::client::resource_writer::ResourceError::Parse(_) => "proxy_group.parse_failed",
            crate::client::resource_writer::ResourceError::Validate(_) => {
                "proxy_group.validate_failed"
            }
            // The `From<ResourceError> for PgWriteError`
            // impl already lifted these to the
            // per-resource variants; the residual
            // arms are defensive.
            crate::client::resource_writer::ResourceError::AlreadyDeclared(_)
            | crate::client::resource_writer::ResourceError::NotDeclared(_) => {
                "proxy_group.invalid"
            }
        },
    }
}

/// Dispatches a `set proxy-group` leaf to the matching
/// implementation. Every CRUD leaf is a real writer; `list`
/// reads the layered `proxy_groups:` list.
pub fn dispatch(
    cmd_arg: SetProxyGroupCmd,
    options: crate::cli::CliOptions,
    output: CliOutput,
) -> ExitCode {
    let paths = caly_platform::paths::AppPaths::from_env();
    dispatch_with_paths(cmd_arg, &paths, options, output)
}

/// Round 25: the path-injected dispatch. The
/// `dispatch` shim resolves the operator's `AppPaths`
/// from process env (the production path); the
/// `dispatch_with_paths` form takes the paths
/// explicitly so a test can drive the dispatch with
/// a hermetic `AppPaths::from_env_vars` fixture
/// without mutating the operator's XDG state. The
/// two arms are 1-for-1 — the path resolution is
/// the only thing that differs.
///
/// Round 30: the 4 CRUD bodies (`add` / `remove` /
/// `enable` / `disable`) inlined into the match
/// arms. Pre-Round 30 each was a 5-arg private
/// helper (`add(paths, name, group_type, members,
/// url, interval_seconds, tolerance_ms, apply,
/// output)` etc.) that did nothing but build a
/// `run_standard_writer` call. The 4 helpers were
/// 60 lines of pure indirection — the dispatch's
/// `match` had 5 lines of `match X => helper_x(…)`
/// with no logic in between. The Round 30 shape
/// inlines each helper into its `match` arm so
/// the dispatch is one expression per verb: the
/// 4 helper functions and the 5 `match arm =>
/// helper_x(…)` call sites collapse into the
/// 4-arm `match` below.
pub fn dispatch_with_paths(
    cmd_arg: SetProxyGroupCmd,
    paths: &caly_platform::paths::AppPaths,
    options: crate::cli::CliOptions,
    output: CliOutput,
) -> ExitCode {
    match cmd_arg {
        SetProxyGroupCmd::Add {
            name,
            group_type,
            members,
            url,
            interval_seconds,
            tolerance_ms,
            apply,
            dry_run,
        } => {
            // Round 30: the previous 5-arm
            // `to_writer_type` helper collapsed into
            // the canonical `From<cli::ProxyGroupTypeSpec>
            // for cmd::PgTypeSpec` impl. The `.into()`
            // call site reads as "the writer expects
            // its own `PgTypeSpec`" instead of the
            // mechanical 5-arm match.
            let writer_type: cmd::PgTypeSpec = group_type.into();
            // The CLI grammar carries a possible member
            // re-parse failure as data (#55); surface it
            // as a command error instead of writing an
            // empty-member group.
            let members = match members {
                Ok(members) => members,
                Err(error) => {
                    return output::report_error_returning(
                        output,
                        crate::error::CliError::new(
                            "proxy_group.member_parse_failed",
                            error,
                            "set proxy-group add",
                        ),
                    );
                }
            };
            let writer_members: Vec<cmd::PgMemberSpec> =
                members.iter().map(to_writer_member).collect();
            let effective_apply = apply && !dry_run;
            run_standard_writer(
                output,
                CLI_PREFIX,
                NOUN,
                ResourceVerb::Add,
                &name,
                || {
                    cmd::add_group(
                        paths,
                        &name,
                        writer_type,
                        &writer_members,
                        url.as_deref(),
                        interval_seconds,
                        tolerance_ms,
                        effective_apply,
                    )
                },
                code_for,
            )
        }
        SetProxyGroupCmd::Remove {
            name,
            apply,
            dry_run,
        } => crud_dispatch(
            output,
            CLI_PREFIX,
            NOUN,
            &name,
            apply,
            dry_run,
            ResourceVerb::Remove,
            |apply| cmd::remove_group(paths, &name, apply),
            code_for,
        ),
        SetProxyGroupCmd::Enable {
            name,
            apply,
            dry_run,
        } => crud_dispatch(
            output,
            CLI_PREFIX,
            NOUN,
            &name,
            apply,
            dry_run,
            ResourceVerb::Enable,
            |apply| cmd::set_enabled(paths, &name, true, apply),
            code_for,
        ),
        SetProxyGroupCmd::Disable {
            name,
            apply,
            dry_run,
        } => crud_dispatch(
            output,
            CLI_PREFIX,
            NOUN,
            &name,
            apply,
            dry_run,
            ResourceVerb::Disable,
            |apply| cmd::set_enabled(paths, &name, false, apply),
            code_for,
        ),
        SetProxyGroupCmd::List { enabled_only } => {
            list(paths, enabled_only, output, options.format)
        }
    }
}

/// Maps the CLI [`ProxyGroupMemberSpec`] to the writer's
/// internal [`cmd::PgMemberSpec`]. The conversion is
/// mechanical; the writer does not import the CLI grammar
/// (separation of concerns). Round 30: the
/// `ProxyGroupTypeSpec` → `cmd::PgTypeSpec` conversion
/// folded into `From<cli::ProxyGroupTypeSpec> for
/// cmd::PgTypeSpec` (the equivalent `to_writer_type`
/// helper was a verbatim 5-arm remap that the
/// canonical `From` impl subsumes); the `Member`
/// conversion stays hand-rolled because the
/// `tag.clone()` / `name.clone()` calls make a
/// `From<&ProxyGroupMemberSpec>` impl borrow-shaped
/// and a `From<ProxyGroupMemberSpec>` impl would
/// consume the CLI enum (the dispatch's `members:
/// Vec<ProxyGroupMemberSpec>` is iterated by
/// reference).
fn to_writer_member(member: &ProxyGroupMemberSpec) -> cmd::PgMemberSpec {
    match member {
        ProxyGroupMemberSpec::Node { tag } => cmd::PgMemberSpec::Node { tag: tag.clone() },
        ProxyGroupMemberSpec::Group { name } => cmd::PgMemberSpec::Group { name: name.clone() },
        ProxyGroupMemberSpec::Direct => cmd::PgMemberSpec::Direct,
        ProxyGroupMemberSpec::Reject => cmd::PgMemberSpec::Reject,
    }
}

fn list(
    paths: &caly_platform::paths::AppPaths,
    enabled_only: bool,
    output: CliOutput,
    format: Option<crate::cli::OutputFormat>,
) -> ExitCode {
    use std::io::IsTerminal;

    // W3a: `node list --offline --format=tree` — the unified declared
    // entry tree (groups + ungrouped nodes + rules) replaces the
    // groups-only projection for the tree face.
    if format == Some(crate::cli::OutputFormat::Tree) {
        return super::node_tree::render_declared_tree(paths, enabled_only, output.is_json());
    }
    // A corrupt/unreadable layered config must not silently read as an
    // empty list (the tree and pick faces both report and exit 1) —
    // 2026-08-12 agent audit: the table face returned `Vec::new()` + 0.
    if let Err(error) = cmd::probe_layered_config(paths) {
        return crate::client::output::report_failure(
            &format!("cannot read the layered config: {error}"),
            output.is_json(),
        );
    }
    let groups = cmd::read_declared(paths);
    // Round 31: `--enabled-only` filter. The
    // pre-Round 31 shape listed every declared
    // group, including the disabled ones the
    // operator had explicitly turned off. For an
    // operator with 30+ groups (a real-world
    // Loyalsoldier-style config), the disabled
    // entries drowned out the active ones in
    // the human form (`proxy-group: <name> ...
    // enabled: false` repeated 20 times). The
    // filter lives in the dispatcher (not the
    // writer) because the writer's `read_declared`
    // contract is "the full layered list"; the
    // dispatcher's `enabled_only: bool` is the
    // one consumer-facing knob that decides what
    // shape of the list to show. The JSON form
    // carries the filter in the `enabled_only`
    // field so a script consumer can branch on
    // "the caller asked for enabled-only" (e.g.
    // a CI check that asserts "every enabled
    // group has a non-empty members list").
    let filtered: Vec<_> = if enabled_only {
        groups
            .iter()
            .filter(|(_, enabled, _, _, _)| *enabled)
            .cloned()
            .collect()
    } else {
        groups
    };
    if output.is_json() {
        let items: Vec<serde_json::Value> = filtered
            .iter()
            .map(|(name, enabled, group_type, member_count, has_url_test)| {
                serde_json::json!({
                    "name": name,
                    "enabled": enabled,
                    "type": group_type,
                    "member_count": member_count,
                    "has_url_test": has_url_test,
                })
            })
            .collect();
        output.success(
            &format!("listed {} proxy group(s)", items.len()),
            serde_json::json!({
                "groups": items,
                "count": items.len(),
                "enabled_only": enabled_only,
            }),
        );
    } else {
        // W2 (cli-v3-design.md §5.1, Q6): adaptive table/TSV
        // replaces the v1 `proxy-group: <name> (...)` lines and
        // the trailing count line (scripts read the count from
        // the TSV row count or the JSON `count` field).
        let mode = crate::output::table_mode(format);
        let rows: Vec<Vec<String>> = filtered
            .iter()
            .map(|(name, enabled, group_type, member_count, _has_url_test)| {
                // The type column carries the Clash label (`url-test`,
                // `fallback`, `load-balance`), which already encodes the
                // probe requirement — a `(url-test)` suffix was redundant
                // (`url-test (url-test)`) and is dropped. The JSON face
                // keeps `has_url_test` for script consumers.
                match mode {
                    crate::output::TableMode::Tsv => vec![
                        name.clone(),
                        group_type.clone(),
                        member_count.to_string(),
                        enabled.to_string(),
                    ],
                    crate::output::TableMode::Table => vec![
                        name.clone(),
                        group_type.clone(),
                        member_count.to_string(),
                        // ANSI only on a real terminal: an explicit
                        // `--format=table` into a pipe must stay clean
                        // bytes for scripts (same guard as the online
                        // node table).
                        if *enabled {
                            if std::io::stdout().is_terminal() {
                                crate::output::paint("32", "enabled")
                            } else {
                                "enabled".to_owned()
                            }
                        } else if std::io::stdout().is_terminal() {
                            crate::output::paint("90", "disabled")
                        } else {
                            "disabled".to_owned()
                        },
                    ],
                }
            })
            .collect();
        let headers: &[&str] = match mode {
            crate::output::TableMode::Tsv => &["name", "type", "members", "enabled"],
            crate::output::TableMode::Table => &["NAME", "TYPE", "MEMBERS", "ENABLED"],
        };
        crate::output::print_table(mode, headers, &rows);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod dispatch_tests;
