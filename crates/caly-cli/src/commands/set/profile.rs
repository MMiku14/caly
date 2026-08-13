//! `set profile …` dispatch.
//!
//! Round 22: every CRUD leaf now goes through the shared
//! `commands::set::common::run_writer` envelope, mirroring
//! `set sub` / `set rule-provider` / `set proxy-group`.
//! The pre-Round-22 hand-rolled envelope builders (8
//! near-identical `Ok(_)` / `Err(_)` matches) collapsed
//! into a one-line shim per verb.
//!
//! `refresh` is the only leaf that doesn't fit the
//! `run_writer` envelope (it routes through
//! `Refreshable::refresh` and emits the `refreshed: N`
//! JSON shape) and keeps its bespoke dispatch.

use std::process::ExitCode;

use crate::cli::SetProfileCmd;
use crate::client::profile as cmd;
use crate::commands::refresh::{ProfileRefresh, RefreshTarget};
use crate::commands::set::common::{
    self, OutcomeKind, ResourceVerb, Summaries, WriteEnvelope, no_extra_payload,
};
use crate::output::CliOutput;

/// Projects a `cmd::ProfileCmdError` to the stable CLI
/// `code:` used in the JSON error envelope. Mirrors the
/// `set sub` / `set rule-provider` / `set proxy-group`
/// dispatch shape.
fn code_for(error: &cmd::ProfileCmdError) -> &'static str {
    use cmd::ProfileCmdError as E;
    match error {
        E::InvalidId(_) => "profile.invalid_id",
        E::InvalidSource(_) => "profile.invalid_source",
        E::NotDeclared(_) => "profile.not_declared",
        E::AlreadyDeclared(_) => "profile.already_declared",
        E::ReadConfig(_) => "profile.read_failed",
        E::ParseConfig(_) => "profile.parse_failed",
        E::Store(_) => "profile.store_failed",
        E::Fetch(_) => "profile.fetch_failed",
        E::FetchChain { .. } => "profile.fetch_chain",
        E::Backup { .. } => "profile.backup_failed",
    }
}

/// The `classify` closure used by every CRUD verb. The
/// profile writer returned two separate enums
/// (`AddOutcome` / `RemoveOutcome`) before Round 22; the
/// collapsed [`cmd::ProfileWriteOutcome`] alias matches
/// the same shape [`common::run_writer`] expects from
/// the other resources.
///
/// Round 25: upgraded from the `Fn(&O) -> bool` shape
/// to `Fn(&O) -> OutcomeKind` so the dispatch can
/// distinguish an idempotent `NoChange` from a real
/// `Applied`. The profile writer does not currently
/// surface `NoChange` (its `add_profile` /
/// `remove_profile` short-circuit on duplicate /
/// unknown ids instead), so the match is exhaustive
/// at the writer level — a future writer extension
/// that adds `NoChange` would surface as a compile
/// error here.
///
/// Round 26: the closure now delegates to
/// [`common::classify_resource_outcome`]. The
/// per-resource `classify` closures were 3-arm
/// `match` copies of the same projection; the
/// central helper collapses them into one
/// definition.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn classify(outcome: &cmd::ProfileWriteOutcome) -> OutcomeKind {
    common::classify_resource_outcome(outcome)
}

/// The singular resource name used by
/// [`Summaries::standard`] for the four standard
/// CRUD verbs. Mirrors the `proxy_group::NOUN` /
/// `rule_provider::NOUN` / `sub::NOUN` constants —
/// all four are the Round 23 single-points-of-truth
/// for the "X added" / "X would be added (dry-run)"
/// pair, and the Round 29 `run_standard_writer` /
/// `run_writer` envelopes accept it as a single
/// `&'static str` argument. Round 30: extracted
/// from the 5-arm `summaries_for` match (the 4
/// standard arms repeated the literal verbatim) so
/// a future renamer of the resource name lands
/// here, not in 5 places.
const NOUN: &str = "profile";

/// Per-verb human summary strings. Each verb is a
/// two-line pair: the applied-path message and the
/// dry-run path message. The dispatch threads the right
/// pair to [`common::run_writer`].
///
/// Round 23: the four standard CRUD verbs (Add /
/// Remove / Enable / Disable) route through the
/// shared [`Summaries::standard`] constructor, which
/// builds the `applied` / `dry_run` strings from a
/// noun + verb pair. The profile-specific Edit and
/// Export verbs don't fit the standard shape (Export
/// is `"profile export would write the body"` rather
/// than `"profile would be exported"`) and use
/// [`Summaries::custom`] (Round 26) for the
/// three-line summary.
///
/// Round 30: the four standard arms now route through
/// [`ProfileVerb::to_resource_verb`] so the standard
/// `ResourceVerb::past_tense` is the single source of
/// truth for "added" / "removed" / "enabled" /
/// "disabled" (it was already used in
/// `commands::set::common::run_standard_writer`'s
/// `Summaries::standard(noun, verb)`). Pre-Round 30
/// the four standard arms called `Summaries::standard`
/// with the `ResourceVerb` constant directly, which
/// worked but tied the two `enum`s together — moving
/// `ResourceVerb` to a 5-verb table would require
/// touching both arms. The `to_resource_verb`
/// projection keeps the two enums in lock-step.
fn summaries_for(verb: ProfileVerb) -> Summaries {
    let standard = |rv| Summaries::standard(NOUN, rv);
    match verb {
        ProfileVerb::Add => standard(ProfileVerb::Add.to_resource_verb()),
        ProfileVerb::Remove => standard(ProfileVerb::Remove.to_resource_verb()),
        ProfileVerb::Enable => standard(ProfileVerb::Enable.to_resource_verb()),
        ProfileVerb::Disable => standard(ProfileVerb::Disable.to_resource_verb()),
        ProfileVerb::Edit => Summaries::custom(
            "profile edited",
            "profile would be edited (dry-run)",
            "profile already edited",
        ),
        ProfileVerb::Export => Summaries::custom(
            "profile exported",
            "profile export would write the body (dry-run)",
            // `export` writes a fresh body to a new
            // file; the writer cannot tell from the
            // output file's mtime whether the body
            // is byte-identical to the cached body,
            // so the `NoChange` outcome is reserved
            // for a future round that adds the
            // comparison. The summary string is
            // locked so a future renamer lands in
            // the test first.
            "profile already exported",
        ),
    }
}

/// The six CRUD verbs the dispatch knows about. `Refresh`
/// is a separate path through the `Refreshable` trait.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ProfileVerb {
    Add,
    Remove,
    Enable,
    Disable,
    Edit,
    Export,
}

impl ProfileVerb {
    /// The `set profile …` leaf string used in the JSON
    /// envelope's `leaf:` field and the `CliError`
    /// command path. Verb-specific extras (`<id>`,
    /// `<out>`) are appended by the dispatch.
    ///
    /// Round 30: the four standard CRUD verbs (Add /
    /// Remove / Enable / Disable) delegate to
    /// [`ResourceVerb::cli_name`] — the canonical
    /// `set RESOURCE VERB ID` table that
    /// `commands::set::common::run_standard_writer`
    /// also reads. The two profile-only verbs
    /// (`Edit` / `Export`) stay bespoke because
    /// they don't fit the `ResourceVerb` enum (the
    /// `set profile` namespace is the only one
    /// that ships them; the standard 4-verb table
    /// is shared across all 4 standard resources).
    fn leaf(self) -> &'static str {
        match self {
            Self::Add => ResourceVerb::Add.cli_name(),
            Self::Remove => ResourceVerb::Remove.cli_name(),
            Self::Enable => ResourceVerb::Enable.cli_name(),
            Self::Disable => ResourceVerb::Disable.cli_name(),
            Self::Edit => "edit",
            Self::Export => "export",
        }
    }

    /// Round 30: projects the four standard CRUD
    /// verbs onto the [`ResourceVerb`] enum (the
    /// shared `set RESOURCE VERB ID` table where
    /// `RESOURCE` is the cli-prefix, `VERB` is the
    /// verb's CLI name, and `ID` is the
    /// operator-supplied id).
    /// `Edit` / `Export` are profile-only and don't
    /// fit the standard table, so the function
    /// returns `None` for them. The dispatch uses
    /// this to thread the canonical verb name
    /// through [`Summaries::standard`] (the past
    /// tense is read from
    /// [`ResourceVerb::past_tense`]) and through
    /// the leaf-prefix string in
    /// [`ProfileVerb::leaf`].
    fn to_resource_verb(self) -> ResourceVerb {
        match self {
            Self::Add => ResourceVerb::Add,
            Self::Remove => ResourceVerb::Remove,
            Self::Enable => ResourceVerb::Enable,
            Self::Disable => ResourceVerb::Disable,
            // The caller is responsible for not
            // calling this on `Edit` / `Export` —
            // both `summaries_for` and `leaf` match
            // the 4 standard arms before falling
            // through to the profile-only ones, so
            // a `Self::Edit` here is a programming
            // error. The `unreachable!` is a debug
            // assertion rather than a runtime
            // branch (the match above is exhaustive
            // and the 2 unhandled arms are a real
            // contract violation).
            Self::Edit | Self::Export => {
                unreachable!("Edit / Export are profile-only verbs with no ResourceVerb mapping")
            }
        }
    }
}

/// Dispatches a `set profile` leaf to the matching
/// implementation. Every CRUD leaf routes through the
/// shared [`common::run_writer`] envelope; `refresh`
/// keeps its bespoke `Refreshable` dispatch.
pub fn dispatch(c: SetProfileCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    let paths = cmd::resolve_paths();
    match c {
        SetProfileCmd::Add {
            id,
            source,
            apply,
            dry_run: _,
        } => {
            let parsed = match cmd::parse_source_spec(&source) {
                Ok(v) => v,
                Err(error) => {
                    let cli = crate::output::CliError::new(
                        code_for(&error),
                        error.to_string(),
                        "set profile add",
                    );
                    return crate::output::report_error_returning(output, cli);
                }
            };
            // Round 19: the writer's `dry_run` flag IS
            // `!apply` by definition. Pass `apply` as the
            // writer's "write" flag so the dispatch and
            // writer share the same boolean. The envelope
            // drops the redundant `apply` field.
            let write = apply;
            run_writer(output, ProfileVerb::Add, &id, || {
                cmd::add_profile(&paths, &id, parsed, !write)
            })
        }
        SetProfileCmd::Remove {
            id,
            apply,
            dry_run: _,
        } => {
            let write = apply;
            run_writer(output, ProfileVerb::Remove, &id, || {
                cmd::remove_profile(&paths, &id, !write)
            })
        }
        SetProfileCmd::Edit { id, apply, dry_run } => {
            // `edit` is always "write" in the apply path —
            // there is no inline "validate but don't spawn
            // the editor" dry-run. The writer still returns
            // `ProfileWriteOutcome::DryRun` for the
            // `!apply` branch so the dispatch's run_writer
            // envelope shape is identical to the other
            // verbs.
            run_writer(output, ProfileVerb::Edit, &id, || {
                cmd::edit_profile(&paths, &id, apply && !dry_run)
            })
        }
        SetProfileCmd::Export { id, out } => {
            // `export` writes the cached body to `<out>`.
            // There's no separate dry-run path; the
            // dispatch always passes `apply: true`. The
            // shared envelope still emits a planned-ok
            // shape if a future round adds a
            // `--dry-run` flag.
            run_writer(output, ProfileVerb::Export, &id, || {
                cmd::export_profile(&paths, &id, &out)
            })
        }
        SetProfileCmd::Enable { id, apply, dry_run } => set_enabled_dispatch(
            output,
            ProfileVerb::Enable,
            &id,
            true,
            apply && !dry_run,
            &paths,
        ),
        SetProfileCmd::Disable { id, apply, dry_run } => set_enabled_dispatch(
            output,
            ProfileVerb::Disable,
            &id,
            false,
            apply && !dry_run,
            &paths,
        ),
        // Refresh always writes the body (the body IS the
        // cache). A `NotModified` HTTP 304 is a real outcome,
        // not a dry-run.
        SetProfileCmd::Refresh { id } => refresh(id.as_deref(), options, output),
        // W1: `caly profile use` — immediate local context write
        // (cli-v3-design.md §10; no dry-run, like refresh).
        SetProfileCmd::Use { id } => crate::client::context::use_profile(&id, options.json),
    }
}

/// `set profile enable|disable` dispatch.
///
/// Round 30: the dry-run path folded into the writer
/// via the new `apply: bool` parameter on
/// [`cmd::set_profile_enabled`]. Pre-Round 30 the
/// dry-run branch was hand-rolled here: a duplicated
/// path-safety check, a `load_declared_profiles` call,
/// an `iter().any()` existence scan, and a 3-line
/// `CliError::new(…)` per error arm, totalling ~60
/// lines of bespoke validation. The dry-run path is
/// now a single `run_writer` call: the writer's
/// `find_declared` check surfaces `NotDeclared` and
/// the path-safety check surfaces `InvalidId` for
/// both `apply: true` and `apply: false`, so the
/// contract is uniform across the two paths. The
/// `Err(_)` from the writer is the only error shape
/// the dispatch has to project into the envelope,
/// and `common::run_writer` already handles the
/// projection through the shared `code_for` closure.
fn set_enabled_dispatch(
    output: CliOutput,
    verb: ProfileVerb,
    id: &str,
    enabled: bool,
    apply: bool,
    paths: &caly_platform::paths::AppPaths,
) -> ExitCode {
    run_writer(output, verb, id, || {
        cmd::set_profile_enabled(paths, id, enabled, apply)
    })
}

/// The single mutation primitive for the `set profile`
/// CRUD leaves. The five-arg `Envelope { name, leaf }`
/// + closure + `code_for` mirrors the pattern in
/// - The same shape is used by
///   `sub.rs` / `rule_provider.rs` / `proxy_group.rs`.
///
/// Round 25: the `is_dry_run: bool` parameter was
/// upgraded to the typed [`classify`] closure so
/// the dispatch can surface an idempotent `NoChange`
/// outcome (a future `set profile add` against an
/// already-declared id, for example).
fn run_writer<F>(output: CliOutput, verb: ProfileVerb, id: &str, write: F) -> ExitCode
where
    F: FnOnce() -> Result<cmd::ProfileWriteOutcome, cmd::ProfileCmdError>,
{
    let leaf = format!("{} {id}", verb.leaf());
    common::run_writer(
        output,
        summaries_for(verb),
        &WriteEnvelope {
            name: id,
            leaf: &leaf,
        },
        write,
        classify,
        code_for,
        no_extra_payload,
    )
}

fn refresh(id: Option<&str>, _options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    // Round 13: route through `Refreshable::refresh`. The
    // trait impl owns the success / error envelope shape.
    // Round 33: the `Ok` / `Err` envelope split folded
    // into `commands::refresh::run_refresh`; the
    // dispatch now owns only the `target` projection.
    let target = id.map_or_else(
        || RefreshTarget::ProfileId("(all)".to_owned()),
        |s| RefreshTarget::ProfileId(s.to_owned()),
    );
    crate::commands::refresh::run_refresh::<ProfileRefresh>(output, target, "set profile refresh")
}

#[cfg(test)]
mod dry_run_validation_tests;
