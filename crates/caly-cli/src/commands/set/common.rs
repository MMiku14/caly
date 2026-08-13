//! Shared helpers for `set` resource dispatch.
//!
//! Round 13: extracted from `commands::set::profile` so the
//! other resources can reuse the same shape if they grow
//! past pure planned-ok stubs.
//!
//! Round 21: the per-resource `run_writer` closures in
//! `commands::set::{sub, rule_provider, proxy_group}` were
//! three near-identical copies of the same `Ok(_)` / `Err(_)`
//! match — each projected a per-resource `XxxWriteError`
//! to a `code: &str` and a human summary. This module
//! owns the *common* part (the JSON envelope shape, the
//! success / dry-run differentiation, the error reporting
//! pipeline); the per-resource dispatch is a one-line
//! shim that supplies a `code_for` closure and a `summary`
//! lookup.

/// The shape every `set RESOURCE VERB` dispatch
/// surfaces in the JSON envelope: a `name:` (the
/// resource's stable id) and a `leaf:` (the literal
/// verb the user typed, for log-grep). The `dry_run`
/// flag is added from the writer's outcome.
#[derive(Debug, Clone)]
pub struct WriteEnvelope<'a> {
    /// The resource's stable id (`group` / `provider` /
    /// `rule_provider` / etc.).
    pub name: &'a str,
    /// The verb the user typed (`set sub add
    /// https://…` → `set sub add https://…`).
    pub leaf: &'a str,
}

/// The three human summaries a `set RESOURCE VERB` leaf
/// emits. The applied-path line is the verb in past
/// tense (`X added`); the dry-run line is the same
/// with `would be` and `(dry-run)` (`X would be added
/// (dry-run)`); the no-change line is the noun in its
/// target state (`X already enabled`, `X already disabled`).
///
/// Round 25: the `no_change` variant was added so idempotent
/// operations (e.g. `enable` on an already-enabled source)
/// report a distinct summary instead of a fabricated
/// `Applied`. The previous writer shape short-circuited
/// such calls with a literal `Applied` outcome but no
/// disk I/O, so the operator saw `ok: subscription source
/// enabled` and assumed the state was just changed. The
/// new variant round-trips through [`Summaries::standard`]
/// (which builds the no-change string from a noun + verb
/// pair the same way it builds the applied / dry-run
/// pairs); a bespoke leaf (e.g. `proxy import`) inlines
/// all three strings explicitly.
///
/// Round 30: the previous `Summaries { applied: String,
/// dry_run: String, no_change: String }` struct eagerly
/// built all 3 strings at construction time even though
/// every dispatch only consumed one of them. The new
/// enum stores either the (noun, verb) pair used by
/// [`Summaries::standard`] (which derives all 3 lines
/// from a single past-tense lookup at read time) or
/// the 3 literal `&'static str` lines used by
/// [`Summaries::custom`]; [`Summaries::summary_for`]
/// builds the single needed `String` per call, cutting
/// 2 of 3 heap allocations on the hot path. The
/// `Copy` derive means the dispatch can pass it around
/// without a `Clone` shim.
#[derive(Debug, Clone, Copy)]
pub enum Summaries {
    /// A `X <past-tense>` / `X would be <past-tense>
    /// (dry-run)` / `X already <past-tense>` triple
    /// built lazily from a noun + [`ResourceVerb`] pair
    /// (the 4 standard resources' shape).
    Standard {
        /// The singular resource name (`"subscription
        /// source"` / `"rule provider"` / `"proxy
        /// group"` / `"profile"` / etc.). Distinct from
        /// the CLI prefix used in the leaf string.
        noun: &'static str,
        /// The verb that drives the past-tense lookup.
        verb: ResourceVerb,
    },
    /// A bespoke 3-line summary for verbs that don't
    /// fit the standard `X <past-tense>` template
    /// (`set profile edit` / `set profile export` /
    /// `set proxy edit` / `set proxy import`). The
    /// 3 `&'static str` are the applied / dry-run /
    /// no-change lines verbatim.
    Custom {
        /// The applied-path summary (e.g. `"profile
        /// edited"`).
        applied: &'static str,
        /// The dry-run-path summary (e.g. `"profile
        /// would be edited (dry-run)"`).
        dry_run: &'static str,
        /// The no-change-path summary (e.g. `"profile
        /// already edited"`). For writers that don't
        /// currently surface a `NoChange` outcome this
        /// string is reserved for a future round that
        /// adds the comparison; the dispatch still
        /// pattern-matches the [`OutcomeKind`] arm.
        no_change: &'static str,
    },
}

/// The four standard CRUD verbs every `set RESOURCE`
/// dispatch exposes. `Edit` and `Export` are
/// resource-specific (only `set profile` uses them)
/// and stay outside this enum; `Import` is a 1-of-1
/// oddity (only `set proxy import` exists, with its
/// own bespoke summary shape).
///
/// Round 23: the enum is the dispatch-time source of
/// truth for the verb's English past tense and CLI
/// name. The 4 standard resources use
/// [`Summaries::standard`] with one of these variants;
/// the profile dispatch uses its own typed `ProfileVerb`
/// enum to extend the set with `Edit` / `Export`.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum ResourceVerb {
    Add,
    Remove,
    Enable,
    Disable,
}

impl ResourceVerb {
    /// The verb's English past-tense form, used in
    /// the applied-path summary ("added" / "removed" / ...).
    pub const fn past_tense(self) -> &'static str {
        match self {
            Self::Add => "added",
            Self::Remove => "removed",
            Self::Enable => "enabled",
            Self::Disable => "disabled",
        }
    }

    /// The verb's CLI name (the second token in the
    /// `set RESOURCE VERB ID` leaf). The
    /// `set_profile_verb::leaf()` method uses the
    /// same convention; the two are kept consistent
    /// so a future caller can use one table for both.
    /// Round 29: the `#[allow(dead_code)]` was
    /// removed — `run_standard_writer` is now the
    /// primary consumer of the table, and the table
    /// would surface a typo here as a compile error
    /// in every standard-resource dispatch.
    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

impl Summaries {
    /// The standard 4-verb `X <past-tense>` /
    /// `X would be <past-tense> (dry-run)` /
    /// `X already <past-tense>` summary triple. The 4
    /// standard resources (`set sub` /
    /// `set rule-provider` / `set proxy-group` /
    /// `set proxy`) call this once per verb; the
    /// profile dispatch uses its own `summaries_for`
    /// helper (private to the profile module)
    /// because it has 2 extra verbs (edit / export)
    /// that don't fit the standard shape.
    ///
    /// Round 25: the `no_change` line is `X already
    /// <past-tense>` (e.g. `subscription source
    /// already enabled`). The verb's past-tense form
    /// is reused: `Add` → `added` → `already added`,
    /// which is correct for `remove` (an `add` after
    /// a `remove` re-`add`s the entry) and for
    /// `enable` / `disable` (an `enable` against an
    /// already-enabled source is the canonical
    /// idempotent case).
    ///
    /// Round 30: stores only the `(noun, verb)` pair;
    /// the 3 `String` lines are built lazily in
    /// [`Summaries::summary_for`] when the dispatch
    /// reaches the `Ok(outcome)` arm. This avoids 2
    /// of 3 heap allocations on every dispatch (the
    /// 2 summary lines the writer's outcome doesn't
    /// pick are never built).
    pub fn standard(noun: &'static str, verb: ResourceVerb) -> Self {
        Self::Standard { noun, verb }
    }

    /// The bespoke-summary constructor for verbs that
    /// do not fit the `X <past-tense>` template (e.g.
    /// `set profile edit` / `set profile export` /
    /// `set proxy import`). The three strings are the
    /// applied / dry-run / no-change lines verbatim;
    /// the dispatch's `run_writer` envelope projects
    /// them into the JSON envelope and the human
    /// summary unchanged. Round 26 introduced this
    /// helper so the 4 inline `Summaries { ... }`
    /// literals (profile Edit / Export, proxy Edit /
    /// Import) collapse into a one-line call site.
    ///
    /// Round 30: takes `&'static str` (the previous
    /// `impl Into<String>` was unused — every call
    /// site passed a string literal). The
    /// `&'static str` shape lets `Custom` participate
    /// in the same `Copy` enum as `Standard`, so the
    /// dispatch can pass `Summaries` by value without
    /// a `Clone` shim.
    pub fn custom(applied: &'static str, dry_run: &'static str, no_change: &'static str) -> Self {
        Self::Custom {
            applied,
            dry_run,
            no_change,
        }
    }

    /// Build the single human-summary line the
    /// dispatch needs for the writer's outcome kind.
    /// Round 30: replaces the previous 3-arm `match
    /// kind { Applied => summaries.applied, ... }`
    /// inside [`run_writer`]. The `Standard` arm
    /// does the `format!` per call (1 alloc per
    /// outcome, not 3), the `Custom` arm does a
    /// `String::from` on the relevant `&'static
    /// str` (1 alloc per outcome, not 3). The
    /// pre-Round-30 implementation eagerly built
    /// all 3 lines in [`Summaries::standard`] /
    /// [`Summaries::custom`] even though the
    /// dispatch consumed only one; the
    /// lazy-evaluated version saves 2 of 3
    /// allocations on every call.
    pub fn summary_for(&self, kind: OutcomeKind) -> String {
        match (self, kind) {
            (Self::Standard { noun, verb }, OutcomeKind::Applied) => {
                format!("{noun} {}", verb.past_tense())
            }
            (Self::Standard { noun, verb }, OutcomeKind::DryRun) => {
                format!("{noun} would be {} (dry-run)", verb.past_tense())
            }
            (Self::Standard { noun, verb }, OutcomeKind::NoChange) => {
                format!("{noun} already {}", verb.past_tense())
            }
            (Self::Custom { applied, .. }, OutcomeKind::Applied) => (*applied).to_owned(),
            (Self::Custom { dry_run, .. }, OutcomeKind::DryRun) => (*dry_run).to_owned(),
            (Self::Custom { no_change, .. }, OutcomeKind::NoChange) => (*no_change).to_owned(),
        }
    }
}

/// Round 25: the per-call outcome classification the
/// dispatch's [`run_writer`] uses to pick the right
/// human summary. The previous `is_dry_run: Fn(&O) ->
/// bool` could not distinguish `Applied` from
/// `NoChange` (both rendered as "applied" with
/// `dry_run: false`), which is how the Round 25 bug
/// (`set sub enable` against an already-enabled URL
/// reporting `Applied` with no disk I/O) stayed
/// hidden. The new closure returns one of three
/// explicit kinds; the dispatch maps each kind to a
/// distinct summary line.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum OutcomeKind {
    /// The writer actually mutated the on-disk state.
    Applied,
    /// The writer validated the change but did not
    /// write (the user did not pass `--apply`).
    DryRun,
    /// The writer observed that the on-disk state was
    /// already the target state (idempotent call), so
    /// neither the apply path nor the dry-run path did
    /// any I/O. The dispatch emits a distinct "already
    /// in the target state" summary.
    NoChange,
}

impl OutcomeKind {
    /// True when the writer performed a no-op
    /// (idempotent success). The dispatch surfaces
    /// this as `dry_run: false` in the JSON envelope
    /// — the user did not opt in to a preview — but
    /// uses a distinct human summary so the operator
    /// can tell apart "I changed the state" from "I
    /// confirmed the state was already what I asked
    /// for".
    #[allow(dead_code)] // test-only consumer in the bin
    // unit (the locked-table test in
    // `tests` matches every kind).
    // Reserved for a future refresh
    // leaf that needs the
    // discriminator without
    // hand-rolling the match.
    pub const fn is_no_change(self) -> bool {
        matches!(self, Self::NoChange)
    }
}

/// Round 23: the no-op `extra_payload` closure. The
/// `set RESOURCE VERB` writers that don't carry any
/// outcome-specific data beyond the envelope's `name` /
/// `leaf` / `dry_run` (every existing writer except
/// `add proxy` and `import proxy`) pass this so the
/// type-inferred `X: Fn(&O) -> serde_json::Value` slot
/// in [`run_writer`] is satisfied. The closure ignores
/// the outcome and returns an empty JSON object, which
/// the envelope merge treats as a no-op.
pub fn no_extra_payload<O>(_outcome: &O) -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Round 26: the canonical `classify` closure for
/// every writer that surfaces the standard
/// [`crate::client::resource_writer::ResourceWriteOutcome`]
/// (5 type aliases: `SubWriteOutcome` /
/// `ProfileWriteOutcome` / `RpWriteOutcome` /
/// `PgWriteOutcome` / `InlineProxyOutcome`-aliased).
///
/// Before this round each dispatch module had its own
/// 5-line `match` closure that did the same
/// `Applied` → `Applied` / `DryRun` → `DryRun` /
/// `NoChange` → `NoChange` projection (see
/// `commands::set::sub::classify` for the most
/// literal copy of the 3-arm pattern). The closure
/// here is the single source of truth; the 4
/// `set RESOURCE` dispatch modules call it
/// directly, and the `set proxy` dispatch keeps its
/// bespoke `classify_inline` / `classify_import`
/// because the inline-proxy enums carry per-call
/// payloads (the `id` for `add` / the `count` for
/// `import`).
///
/// The `&ResourceWriteOutcome` argument is the
/// `Fn(&O) -> OutcomeKind` trait bound on
/// [`run_writer`]; the type is `Copy` so the `&`
/// triggers a pedantic
/// `trivially_copy_pass_by_ref` lint. The `#[allow]`
/// lives on the `pub fn` so the lint is silenced
/// here (the function definition is the one source
/// of truth for the parameter shape; silencing at
/// the call sites would require 4 parallel
/// `#[allow]` attributes — one per dispatch module).
#[allow(clippy::trivially_copy_pass_by_ref)]
pub fn classify_resource_outcome(
    outcome: &crate::client::resource_writer::ResourceWriteOutcome,
) -> OutcomeKind {
    use crate::client::resource_writer::ResourceWriteOutcome as O;
    match outcome {
        O::Applied => OutcomeKind::Applied,
        O::DryRun => OutcomeKind::DryRun,
        O::NoChange => OutcomeKind::NoChange,
    }
}

#[cfg(test)]
mod summaries_tests;

#[cfg(test)]
mod outcome_kind_tests;

mod writers;

pub use writers::{run_standard_writer, run_writer};
