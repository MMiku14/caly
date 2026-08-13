//! Tests for `commands/set/common.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 23: the `Summaries::standard(noun, verb)`
//! constructor is the single point of truth for the
//! 16 standard-verb summary pairs (4 nouns × 4
//! verbs). These tests lock the strings so a future
//! typo in `ResourceVerb::past_tense` or in the
//! `format!` template surfaces here, not in a
//! dispatch regression test.
//!
//! Round 25: the constructor now returns a *triple*
//! (applied / dry-run / no-change). The tests
//! extend the lock to the third column so a
//! future rename of the no-change shape surfaces
//! here, not in a dispatch regression.
//!
//! Round 30: `Summaries` is an enum (`Standard` /
//! `Custom`); the 3 lines are built lazily by
//! [`Summaries::summary_for`]. The tests now
//! exercise the 3 [`OutcomeKind`] arms so a
//! future typo in the `summary_for` match arms
//! surfaces here, not in a dispatch regression.
use super::{OutcomeKind, ResourceVerb, Summaries};

#[test]
fn standard_summary_matches_legacy_inlined_strings() {
    // The 4 standard nouns + 4 verbs × 3 paths =
    // 12 expected strings. Cross-checked against
    // the pre-Round-23 inline `Summaries { ... }`
    // literals in `commands/set/{sub,rule_provider,
    // proxy_group,proxy}.rs` for the applied /
    // dry-run pair, and against the Round 25
    // `no_change` template for the third column.
    for (noun, verb, applied, dry_run, no_change) in [
        (
            "subscription source",
            ResourceVerb::Add,
            "subscription source added",
            "subscription source would be added (dry-run)",
            "subscription source already added",
        ),
        (
            "subscription source",
            ResourceVerb::Remove,
            "subscription source removed",
            "subscription source would be removed (dry-run)",
            "subscription source already removed",
        ),
        (
            "rule provider",
            ResourceVerb::Enable,
            "rule provider enabled",
            "rule provider would be enabled (dry-run)",
            "rule provider already enabled",
        ),
        (
            "proxy group",
            ResourceVerb::Disable,
            "proxy group disabled",
            "proxy group would be disabled (dry-run)",
            "proxy group already disabled",
        ),
    ] {
        let summaries = Summaries::standard(noun, verb);
        assert_eq!(summaries.summary_for(OutcomeKind::Applied), applied);
        assert_eq!(summaries.summary_for(OutcomeKind::DryRun), dry_run);
        assert_eq!(summaries.summary_for(OutcomeKind::NoChange), no_change);
    }
}

#[test]
fn past_tense_table_is_locked() {
    // A typo in `past_tense` (e.g. "add" instead of
    // "added") would silently produce a wrong
    // summary line. Lock the 4 entries here.
    assert_eq!(ResourceVerb::Add.past_tense(), "added");
    assert_eq!(ResourceVerb::Remove.past_tense(), "removed");
    assert_eq!(ResourceVerb::Enable.past_tense(), "enabled");
    assert_eq!(ResourceVerb::Disable.past_tense(), "disabled");
}

#[test]
fn cli_name_table_is_locked() {
    // The CLI name table feeds a future unified
    // dispatch router (it would build the `set
    // RESOURCE VERB ID` leaf prefix). Lock
    // it here so a rename lands in the tests
    // before it lands in the dispatch.
    assert_eq!(ResourceVerb::Add.cli_name(), "add");
    assert_eq!(ResourceVerb::Remove.cli_name(), "remove");
    assert_eq!(ResourceVerb::Enable.cli_name(), "enable");
    assert_eq!(ResourceVerb::Disable.cli_name(), "disable");
}

#[test]
fn custom_summary_for_each_kind() {
    // Round 30: the `Custom` arm of
    // `summary_for` projects the 3
    // `&'static str` lines to the right
    // `OutcomeKind`. The bespoke strings
    // (e.g. `set profile edit` /
    // `set profile export` / `set proxy
    // edit` / `set proxy import`) don't fit
    // the standard `X <past-tense>` template,
    // so they live in `Custom` and are
    // returned verbatim per kind. Locking
    // the 3 arm outputs surfaces a future
    // mix-up (e.g. swapping `applied` and
    // `no_change` in the match) here, not
    // in a dispatch regression.
    let summaries = Summaries::custom(
        "profile edited",
        "profile would be edited (dry-run)",
        "profile already edited",
    );
    assert_eq!(
        summaries.summary_for(OutcomeKind::Applied),
        "profile edited"
    );
    assert_eq!(
        summaries.summary_for(OutcomeKind::DryRun),
        "profile would be edited (dry-run)"
    );
    assert_eq!(
        summaries.summary_for(OutcomeKind::NoChange),
        "profile already edited"
    );
}
