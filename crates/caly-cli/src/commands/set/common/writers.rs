//! The shared writer-runner for every `set RESOURCE VERB` leaf.
//!
//! Split out of `commands/set/common.rs` (audit #70 file-length
//! budget): [`run_writer`] is the single envelope point that turns
//! a writer's outcome into the human summary + JSON envelope;
//! [`run_standard_writer`] is the small shim for the resources
//! whose outcome is the shared
//! [`crate::client::resource_writer::ResourceWriteOutcome`].

use std::process::ExitCode;

use super::{
    classify_resource_outcome, no_extra_payload, OutcomeKind, ResourceVerb, Summaries,
    WriteEnvelope,
};
use crate::output::{self, CliOutput};

/// Runs a resource writer through the shared success /
/// failure envelope. The single point every `set
/// RESOURCE VERB` leaf reaches.
///
/// # Arguments
///
/// - `output` — the JSON / human output sink.
/// - `summaries` — pre-computed human summaries for the
///   applied / dry-run / no-change paths.
/// - `envelope` — the `name` / `leaf` fields the JSON
///   envelope carries (the `dry_run` field is filled from
///   the outcome kind).
/// - `write` — the per-resource writer call. Returns the
///   outcome (which decides the summary + the
///   `dry_run: bool` envelope field).
/// - `classify` — projects the writer's outcome type
///   into an [`OutcomeKind`] so the dispatch can pick
///   the right summary. The per-resource module passes a
///   closure that knows its concrete `Outcome` type
///   (e.g. `|o| match o { RpWriteOutcome::DryRun =>
///   DryRun, RpWriteOutcome::NoChange => NoChange,
///   _ => Applied }`). Writers that never emit `NoChange`
///   pass a 2-arm match.
/// - `code_for` — a closure that maps the resource's
///   domain error type to a stable `code: &str`.
/// - `extra_payload` — a closure that projects any
///   outcome-specific fields into the success envelope
///   (e.g. the on-disk `id` for `add proxy`, the
///   imported `count` for `import proxy`). Returns
///   `serde_json::Value::Object(...)` whose entries are
///   merged into the envelope; pass `|_| json!({})` when
///   the writer has no extra data.
///
/// Round 23: this is the seventh `run_writer` argument
/// (the original six), added so the `set proxy` dispatch
/// can route the `add` and `import` leaves through the
/// shared envelope — `add_proxy` returns the on-disk
/// id (a hash of the URI) and `import_proxy` returns
/// the number of new entries, both of which the JSON
/// envelope needs. Every existing call site was
/// updated to pass `|_| json!({})` (no extra fields).
///
/// Round 25: the `is_dry_run: Fn(&O) -> bool` sixth
/// argument was upgraded to `classify: Fn(&O) ->
/// OutcomeKind` so the dispatch can distinguish a real
/// `Applied` from an idempotent `NoChange` (the
/// subscription writer's `set_source_enabled` short-
/// circuit used to fabricate `Applied` with no disk
/// I/O, which the operator saw as a successful write).
pub fn run_writer<O, E, F, K, C, X>(
    output: CliOutput,
    summaries: Summaries,
    envelope: &WriteEnvelope<'_>,
    write: F,
    classify: K,
    code_for: C,
    extra_payload: X,
) -> ExitCode
where
    F: FnOnce() -> Result<O, E>,
    K: Fn(&O) -> OutcomeKind,
    C: Fn(&E) -> &'static str,
    X: Fn(&O) -> serde_json::Value,
    // W2-β2a: domain errors join the §8 three-question contract
    // (what/why/next) by carrying their own remediation hint; the
    // envelope picks it up here instead of growing a per-dispatch
    // `hint_for` parameter across every resource.
    E: crate::output::ErrorHint + core::fmt::Display,
{
    match write() {
        Ok(outcome) => {
            let kind = classify(&outcome);
            // The `dry_run` envelope flag is only
            // `true` when the user opted in to a
            // preview (the dispatch's `apply` flag was
            // `false`). An idempotent `NoChange` is
            // NOT a dry-run: the user did not ask for
            // a preview, the writer observed the
            // on-disk state was already the target.
            let dry_run = matches!(kind, OutcomeKind::DryRun);
            // Round 30: the previous 3-arm match
            // selected the eager pre-built `String`
            // field. `summary_for` builds only the
            // needed line, which lets the
            // `Summaries::standard` /
            // `Summaries::custom` constructors stay
            // allocation-free (they store `(noun,
            // verb)` or 3 `&'static str` instead of
            // 3 owned `String`s).
            let mut summary = summaries.summary_for(kind);
            // A dry-run preview must say how to commit it — the first-use
            // 动线 (`sub add` then `sub list`) died on the missing hint
            // (2026-08-12 user-flow audit). The JSON envelope already
            // carries `dry_run: true`; only the human line grows.
            if dry_run && !output.is_json() {
                summary.push_str("\nhint: run with `--apply` to commit");
            }
            let mut payload = serde_json::json!({
                "name": envelope.name,
                "leaf": envelope.leaf,
                "dry_run": dry_run,
            });
            // Round 25: surface the no-change flag in
            // the JSON envelope so a downstream tool
            // can branch on it without parsing the
            // human message. `no_change: true` is
            // mutually exclusive with the implicit
            // `applied` (which is `true` when the
            // writer changed the on-disk state).
            if kind == OutcomeKind::NoChange {
                payload["no_change"] = serde_json::Value::Bool(true);
            }
            // Merge the writer's extra fields into the
            // base envelope. `Object` is the only shape
            // that can extend the base map; `Array` /
            // `Number` / `String` / `Bool` / `Null` from
            // the writer would silently drop. A writer
            // that needs an array field (e.g. `ids: [...]`)
            // wraps the array in a single-key object.
            if let (serde_json::Value::Object(map), serde_json::Value::Object(extra)) =
                (&mut payload, extra_payload(&outcome))
            {
                map.extend(extra);
            }
            output.success(&summary, payload);
            ExitCode::SUCCESS
        }
        Err(error) => {
            let code = code_for(&error);
            let mut cli = crate::output::CliError::new(code, format!("{error}"), envelope.leaf);
            if let Some(hint) = error.hint() {
                cli = cli.with_hint(hint);
            }
            output::report_error_returning(output, cli)
        }
    }
}

/// Round 29: the standard-CRU envelope for resources
/// whose writer returns
/// [`crate::client::resource_writer::ResourceWriteOutcome`]
/// and whose summary fits the `X <past-tense>` /
/// `X would be <past-tense> (dry-run)` / `X already
/// <past-tense>` template (every resource except
/// `profile`, which extends the verb set with `Edit`
/// and `Export`).
///
/// The 4 standard resources (`set sub` /
/// `set rule-provider` / `set proxy-group` / `set
/// proxy add|remove`) all followed the same
/// 9-line shape before this round:
///
/// ```ignore
/// let leaf = format!("set {prefix} {verb} {name}");
/// common::run_writer(
///     output,
///     Summaries::standard(NOUN, verb),
///     &WriteEnvelope { name, leaf: &leaf },
///     || cmd::writer(paths, name, apply),
///     classify_resource_outcome,
///     code_for,
///     no_extra_payload,
/// )
/// ```
///
/// Round 29 collapses the 4 repeated 9-line blocks
/// per resource (4 resources × 4 verbs × 9 lines ≈
/// 144 lines of structurally identical dispatch
/// boilerplate) into a single function call per
/// verb. The call site becomes a one-liner that
/// reads as the *intent* — "run a `set {prefix}
/// {verb}` against this writer with this name" —
/// rather than the *mechanics* — "build a leaf
/// string, call the envelope with these 7
/// arguments, all of which are the same per
/// resource".
///
/// # Arguments
///
/// - `output` — the JSON / human output sink.
/// - `cli_prefix` — the second token in the leaf
///   string (`"sub"` / `"rule-provider"` /
///   `"proxy-group"` / `"proxy"`). The leaf is
///   built as `format!("set {cli_prefix} {verb.cli_name()} {name}")`.
/// - `noun` — the singular resource name used by
///   [`Summaries::standard`] (e.g. `"subscription
///   source"` / `"rule provider"` / `"proxy
///   group"`). Distinct from `cli_prefix` because
///   the human summary wants the long form
///   (`"subscription source added"`) while the leaf
///   wants the CLI form (`"set sub add https://…"`).
/// - `verb` — the standard [`ResourceVerb`] variant
///   (drives both the leaf token and the
///   `Summaries::standard` past-tense).
/// - `name` — the operator-supplied id / url.
/// - `write` — the per-resource writer call.
/// - `code_for` — maps the resource's domain error
///   to a stable `code: &str`.
///
/// Round 29 also folds the
/// `Summaries::standard(NOUN, verb)` +
/// `WriteEnvelope { name, leaf: &leaf }` +
/// `classify_resource_outcome` +
/// `no_extra_payload` quartet into the helper. The
/// `classify` and `extra_payload` slots are
/// *not* caller parameters because the 4 standard
/// resources all use the same canonical projection
/// (the writer's `ResourceWriteOutcome` enum) and
/// the same no-op extra payload (every standard
/// writer carries the per-call id / url in
/// `envelope.name`, not in a separate
/// `extra_payload` field). Bespoke writers like
/// `InlineProxyOutcome` / `ImportOutcome` keep
/// using [`run_writer`] directly because their
/// payloads project into the envelope.
pub fn run_standard_writer<E, F, C>(
    output: CliOutput,
    cli_prefix: &'static str,
    noun: &'static str,
    verb: ResourceVerb,
    name: &str,
    write: F,
    code_for: C,
) -> ExitCode
where
    F: FnOnce() -> Result<crate::client::resource_writer::ResourceWriteOutcome, E>,
    C: Fn(&E) -> &'static str,
    E: crate::output::ErrorHint + core::fmt::Display,
{
    let leaf = format!("set {cli_prefix} {} {name}", verb.cli_name());
    let envelope = WriteEnvelope { name, leaf: &leaf };
    run_writer(
        output,
        Summaries::standard(noun, verb),
        &envelope,
        write,
        classify_resource_outcome,
        code_for,
        no_extra_payload,
    )
}

/// Shared CRUD leaf dispatch for `set RESOURCE VERB`.
///
/// Collapsed from three per-resource copies
/// (`commands::proxy_group`, `commands::rule_provider`,
/// `commands::set::sub`): the copies differed only in the
/// `ResourceVerb` variant, the writer closure and the
/// error-mapping fn; the leaf string / envelope /
/// summary / classify / payload are all folded into
/// [`run_standard_writer`]. The `add` leaves stay bespoke
/// where the writer needs extra arguments (e.g.
/// `add_provider`'s `&source` / behavior projection).
///
/// `--apply` and `--dry-run` are mutually exclusive
/// (clap enforces it). `apply: true` writes,
/// `dry_run: true` runs every check + reports planned,
/// both `false` (default) is the safe dry-run path.
///
/// `W` is the writer closure type; the
/// `Fn(bool) -> Result<ResourceWriteOutcome, E>` shape
/// matches the per-resource `cmd::*` functions once
/// `paths` / `name` are bound at the call site. `C`
/// maps the resource's error type to the human hint.
#[allow(clippy::too_many_arguments)]
pub fn crud_dispatch<W, E, C>(
    output: CliOutput,
    cli_prefix: &'static str,
    noun: &'static str,
    name: &str,
    apply: bool,
    dry_run: bool,
    verb: ResourceVerb,
    writer: W,
    code_for: C,
) -> ExitCode
where
    W: FnOnce(bool) -> Result<crate::client::resource_writer::ResourceWriteOutcome, E>,
    C: Fn(&E) -> &'static str,
    E: crate::output::ErrorHint + core::fmt::Display,
{
    // `--apply` and `--dry-run` are mutually
    // exclusive (clap enforces it). `apply: true`
    // writes, `dry_run: true` runs every check +
    // reports planned, both `false` (default) is the
    // safe dry-run path.
    let effective_apply = apply && !dry_run;
    run_standard_writer(
        output,
        cli_prefix,
        noun,
        verb,
        name,
        || writer(effective_apply),
        code_for,
    )
}
