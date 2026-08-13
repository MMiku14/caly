//! `set sub …` dispatch.
//!
//! Round 21: the 4 source-CRUD leaves
//! (`add` / `remove` / `enable` / `disable`) now go
//! through the shared
//! `commands::set::common::run_writer` envelope. The
//! `import` and `refresh` leaves keep their bespoke
//! paths:
//!
//! - `refresh` drives the daemon RPC; the daemon
//!   returns `ExitCode`, not `Result`, and the typed
//!   `run_client_collect` is the future hook for a
//!   real `SubscriptionRefresh` trait integration.
//! - `import` inspects a file/clipboard and prints a
//!   preview; a future `--save` flag can promote the
//!   preview to a write by lifting the
//!   `import_preview` projection into a typed
//!   writer (the contract is fixed: `nodes` must be
//!   non-empty, otherwise `SubCmdError::InvalidUrl`
//!   surfaces). Round 33: the writer itself is
//!   absent today (the `import_inline` helper that
//!   held the path-safety + non-empty guard was
//!   retired in Round 32 with its 3 unit tests), so
//!   a follow-up round that wires the leaf would
//!   re-introduce the writer + 3 unit tests at the
//!   same site.
//!
//! Every writer returns `Result<SubWriteOutcome,
//! SubCmdError>`; the dispatch maps each variant to a
//! stable `code:` in the JSON envelope through the
//! shared `run_writer`.
//!
//! Round 23: the previous `summaries_for(&'static str)`
//! helper (a defensive 5-arm `match` over a free-form
//! `&'static str` verb) collapsed into a single
//! `Summaries::standard(NOUN, ResourceVerb::*)` call
//! per leaf. The four CRUD verbs are now expressed
//! through the typed [`ResourceVerb`] enum shared
//! with `set rule-provider` / `set proxy-group` /
//! `set proxy`; a future verb is a compile error
//! instead of a runtime fall-through to the `other`
//! arm.

use std::process::ExitCode;

use crate::cli::SetSubCmd;
use crate::client::subscription as cmd;
use crate::commands::set::common::{run_standard_writer, ResourceVerb};
use crate::output::CliOutput;

/// The singular resource name used by
/// [`run_standard_writer`] for every `set sub`
/// CRUD leaf. Mirrors the `proxy_group::NOUN`
/// and `rule_provider::NOUN` constants — all
/// three are the Round 23 single-points-of-truth
/// for the "X added" / "X would be added
/// (dry-run)" pair. Round 29: `NOUN` is now
/// passed to `run_standard_writer` as the
/// `noun` argument (the helper builds the
/// `Summaries::standard` triple itself).
const NOUN: &str = "subscription source";

/// The leaf-prefix token (the second token in
/// the `set sub <verb> <url>` leaf string).
/// Kept distinct from [`NOUN`] because the leaf
/// wants the CLI form (`"set sub add
/// https://…"`) while the human summary wants
/// the long form (`"subscription source
/// added"`).
const CLI_PREFIX: &str = "sub";

/// Projects a `cmd::SubCmdError` to the stable CLI
/// `code:` used in the JSON error envelope.
fn code_for(error: &cmd::SubCmdError) -> &'static str {
    use cmd::SubCmdError as E;
    match error {
        E::InvalidUrl(_) => "sub.invalid_url",
        E::InvalidName(_) => "sub.invalid_name",
        E::AlreadyDeclared(_) => "sub.already_declared",
        // W2-β2a (§8-4/§8-7): name collisions and missing files get
        // their own stable codes; `--every` on a file source is a
        // usage error (exit 2 via the `usage.` prefix rule).
        E::NameTaken(_) => "sub.name_taken",
        E::SourceFileNotFound { .. } => "sub.file_not_found",
        E::EveryOnFile => "usage.sub.every_on_file",
        // W2-β2b: a name matching >1 source is not addressable —
        // usage class (exit 2 via the prefix rule).
        E::AmbiguousName(_) => "usage.sub.ambiguous_name",
        E::NotDeclared(_) => "sub.not_declared",
        E::Shared(shared) => match shared {
            crate::client::resource_writer::ResourceError::Invalid(_) => "sub.invalid",
            crate::client::resource_writer::ResourceError::Write(_) => "sub.write_failed",
            crate::client::resource_writer::ResourceError::Parse(_) => "sub.parse_failed",
            crate::client::resource_writer::ResourceError::Validate(_) => "sub.validate_failed",
            crate::client::resource_writer::ResourceError::AlreadyDeclared(_)
            | crate::client::resource_writer::ResourceError::NotDeclared(_) => "sub.invalid",
        },
    }
}

pub fn dispatch(c: SetSubCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    let paths = cmd::resolve_paths();
    dispatch_with_paths(c, &paths, options, output)
}

/// Round 25: the path-injected dispatch. The
/// `dispatch` shim resolves the operator's `AppPaths`
/// from process env (the production path); the
/// `dispatch_with_paths` form takes the paths
/// explicitly so a test can drive the dispatch
/// with a hermetic `AppPaths::from_env_vars`
/// fixture without mutating the operator's XDG
/// state. The two arms are 1-for-1 — the path
/// resolution is the only thing that differs.
///
/// The `Refresh` and `Import` leaves do not depend
/// on `paths` (they drive the daemon RPC and the
/// clipboard respectively) and keep their
/// `crate::client::...` calls inline.
///
/// Round 29: the 4 CRUD leaves (`add` / `remove` /
/// `enable` / `disable`) all collapse to a single
/// [`run_standard_writer`] call. The leaf string,
/// the `WriteEnvelope` construction, the
/// `Summaries::standard(NOUN, verb)` call, the
/// `classify_resource_outcome` projection, and the
/// `no_extra_payload` pass-through are all folded
/// into the helper. The dispatch's only per-leaf
/// data is the [`ResourceVerb`] variant and the
/// `cmd::*_source` writer call.
#[allow(clippy::too_many_lines)] // 4 CRUD leaves + set leaf + refresh
pub fn dispatch_with_paths(
    c: SetSubCmd,
    paths: &caly_platform::paths::AppPaths,
    options: crate::cli::CliOptions,
    output: CliOutput,
) -> ExitCode {
    match c {
        SetSubCmd::Refresh {
            target,
            force,
            asynchronous,
        } => refresh_leaf_dispatch(output, paths, target, force, asynchronous, options),
        // The 4 CRUD leaves all share the
        // `set sub <verb> <url> --apply` shape:
        // they're the standard 4-verb `ResourceVerb`
        // table routed through
        // `run_standard_writer`. Round 30
        // collapsed the 4 near-identical arms
        // (each was an 8-line `run_standard_writer`
        // call) into a single inner `match` on
        // the verb: the verb drives the
        // `ResourceVerb` variant AND the writer
        // function pointer. The dry-run / apply
        // flag, the leaf string, the
        // `WriteEnvelope`, the `Summaries::standard`
        // call, the `classify_resource_outcome`
        // projection, and the `no_extra_payload`
        // pass-through are all folded into the
        // shared `run_standard_writer` helper —
        // the dispatch's only per-leaf data is
        // the verb + the writer call. The
        // `pre-Round 30` shape was 32 lines of
        // repetitive match arms; the Round 30
        // shape is 1 match per verb (4 total,
        // 1 per arm).
        SetSubCmd::Add {
            url,
            name,
            refresh_every_minutes,
            apply,
            dry_run: _,
        } => {
            // C-G interactive naming: on a full TTY without an
            // explicit `--name`, ask once (Enter = skip). Piped
            // shells fall through silently with no name.
            let name = match name {
                Some(_) => name,
                None if crate::client::interact::interactive_capable() => {
                    match crate::client::interact::prompt_text(
                        "Subscription name (optional, Enter to skip)",
                    ) {
                        crate::client::interact::TextOutcome::Entered(entered) => entered,
                        crate::client::interact::TextOutcome::Interrupted => {
                            return ExitCode::from(130);
                        }
                    }
                }
                None => None,
            };
            crud_dispatch(
                output,
                paths,
                &url,
                ResourceVerb::Add,
                apply,
                |paths, url, apply| {
                    cmd::add_source(paths, url, name.as_deref(), refresh_every_minutes, apply)
                },
            )
        }
        SetSubCmd::Remove {
            url,
            purge,
            apply,
            dry_run: _,
        } => crud_and_converge(
            options,
            output,
            paths,
            &url,
            ResourceVerb::Remove,
            apply,
            |paths, url, apply| cmd::remove_source(paths, url, purge, apply),
        ),
        // W2-β2b (`caly sub set`): the in-place edit doesn't fit the
        // 4-verb `ResourceVerb` table, so it drives `run_writer`
        // directly with a custom summary triple — same envelope,
        // same `code_for`, same classify projection.
        SetSubCmd::Set {
            target,
            url,
            name,
            refresh_every_minutes,
            apply,
            dry_run: _,
        } => set_leaf_dispatch(
            output,
            paths,
            target,
            url,
            name,
            refresh_every_minutes,
            apply,
        ),
        SetSubCmd::Enable {
            url,
            apply,
            dry_run: _,
        } => crud_and_converge(
            options,
            output,
            paths,
            &url,
            ResourceVerb::Enable,
            apply,
            cmd::enable_source,
        ),
        SetSubCmd::Disable {
            url,
            apply,
            dry_run: _,
        } => crud_and_converge(
            options,
            output,
            paths,
            &url,
            ResourceVerb::Disable,
            apply,
            cmd::disable_source,
        ),
        SetSubCmd::Import {
            path,
            clipboard,
            apply: _,
            dry_run: _,
        } => crate::client::subscription::import_preview(path.as_deref(), clipboard, options.json),
    }
}

/// Round 30: the inner dispatch helper for the 4
/// standard CRUD leaves. Pre-Round 30 each of the
/// `Add` / `Remove` / `Enable` / `Disable` arms
/// in [`dispatch_with_paths`] was an 8-line
/// `run_standard_writer(...)` call that differed
/// only in the `ResourceVerb` variant + the
/// `cmd::*_source` writer function. The 4 arms
/// are now collapsed into this single helper:
/// the verb + the writer closure are the only
/// per-call data, the leaf string / envelope /
/// summary / classify / payload are all folded
/// into `run_standard_writer`.
///
/// `W` is the writer closure type; the
/// `Fn(&AppPaths, &str, bool) -> Result<SubWriteOutcome, SubCmdError>`
/// signature matches the 4 `cmd::*_source`
/// functions exactly.
fn crud_dispatch<W>(
    output: CliOutput,
    paths: &caly_platform::paths::AppPaths,
    url: &str,
    verb: ResourceVerb,
    apply: bool,
    writer: W,
) -> ExitCode
where
    W: FnOnce(
        &caly_platform::paths::AppPaths,
        &str,
        bool,
    ) -> Result<cmd::SubWriteOutcome, cmd::SubCmdError>,
{
    run_standard_writer(
        output,
        CLI_PREFIX,
        NOUN,
        verb,
        url,
        || writer(paths, url, apply),
        code_for,
    )
}

/// Runs a CRUD verb, then — when it applied successfully (not dry-run)
/// and the verb changes the enabled node set (remove/disable/enable) —
/// triggers a full daemon refresh so the node registry and the kernel
/// config converge: otherwise a disabled/removed subscription keeps its
/// stale nodes (2026-08-13 user report: nodes persisted after
/// `sub disable`; the verb only rewrote the config flag and nothing
/// re-converged the registry or re-rendered the kernel config).
fn crud_and_converge(
    options: crate::cli::CliOptions,
    output: CliOutput,
    paths: &caly_platform::paths::AppPaths,
    url: &str,
    verb: ResourceVerb,
    apply: bool,
    writer: impl Fn(
        &caly_platform::paths::AppPaths,
        &str,
        bool,
    ) -> Result<cmd::SubWriteOutcome, cmd::SubCmdError>,
) -> ExitCode {
    let code = crud_dispatch(output, paths, url, verb, apply, writer);
    if apply
        && code == ExitCode::SUCCESS
        && matches!(
            verb,
            ResourceVerb::Remove | ResourceVerb::Disable | ResourceVerb::Enable
        )
    {
        // Best-effort full refresh: releases disabled/removed sources'
        // cache entries, node registrations and re-renders the kernel
        // config without their nodes.
        refresh_leaf_dispatch(output, paths, None, false, false, options);
    }
    code
}

/// W2-β2b (`caly sub set`): the in-place edit doesn't fit the
/// 4-verb `ResourceVerb` table, so it drives `run_writer`
/// directly with a custom summary triple — same envelope,
/// same `code_for`, same classify projection. Extracted from
/// [`dispatch_with_paths`] to keep the dispatch under the
/// 100-line clippy budget.
#[allow(clippy::too_many_arguments)]
fn set_leaf_dispatch(
    output: CliOutput,
    paths: &caly_platform::paths::AppPaths,
    target: String,
    url: Option<String>,
    name: Option<String>,
    refresh_every_minutes: Option<u64>,
    apply: bool,
) -> ExitCode {
    let leaf = format!("set sub set {target}");
    let envelope = crate::commands::set::common::WriteEnvelope {
        name: &target,
        leaf: &leaf,
    };
    crate::commands::set::common::run_writer(
        output,
        crate::commands::set::common::Summaries::custom(
            "subscription source updated",
            "subscription source would be updated (dry-run)",
            "subscription source already up to date",
        ),
        &envelope,
        || {
            cmd::set_source(
                paths,
                &target,
                url.as_deref(),
                name.as_deref(),
                refresh_every_minutes,
                apply,
            )
        },
        crate::commands::set::common::classify_resource_outcome,
        code_for,
        crate::commands::set::common::no_extra_payload,
    )
}

/// W2-β2b (§4.3): address one source by name or URL, then run the
/// refresh RPC. The resolution is offline (config read only) so a
/// mistyped name fails before the daemon round-trip; the daemon
/// re-checks the id against its own resolve pass anyway. Extracted
/// from [`dispatch_with_paths`] to keep the dispatch under the
/// 100-line clippy budget.
fn refresh_leaf_dispatch(
    output: CliOutput,
    paths: &caly_platform::paths::AppPaths,
    target: Option<String>,
    force: bool,
    asynchronous: bool,
    options: crate::cli::CliOptions,
) -> ExitCode {
    let subscription_id = match target.as_deref() {
        None => None,
        Some(token) => match cmd::resolve_source_ref(paths, token) {
            Ok(url) => {
                Some(caly_backends::subscription::subscription_id_for_url(&url).into_bytes())
            }
            Err(error) => {
                let leaf = format!("set sub refresh {token}");
                let mut cli =
                    crate::output::CliError::new(code_for(&error), format!("{error}"), &leaf);
                if let Some(hint) = crate::output::ErrorHint::hint(&error) {
                    cli = cli.with_hint(hint);
                }
                return crate::output::report_error_returning(output, cli);
            }
        },
    };
    crate::client::run_client(
        crate::ClientCommand::RefreshSubscription {
            subscription_id,
            force,
            asynchronous,
        },
        options,
    )
}

#[cfg(test)]
mod dispatch_tests;
