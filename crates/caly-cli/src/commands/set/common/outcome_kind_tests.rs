//! Tests for `commands/set/common.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 25: the `OutcomeKind` enum is the dispatch's
//! source of truth for "what kind of write just
//! happened". A typo or accidental addition to the
//! enum would silently break the per-resource
//! `classify` closures (a closure that doesn't
//! match a future kind would compile-error today,
//! but a typo in the kind's `Debug` / equality
//! behaviour would still slip through). These
//! tests lock the discriminator table.
use super::OutcomeKind;

#[test]
fn outcome_kind_is_no_change_table_is_locked() {
    assert!(OutcomeKind::NoChange.is_no_change());
    assert!(!OutcomeKind::Applied.is_no_change());
    assert!(!OutcomeKind::DryRun.is_no_change());
}

#[test]
fn outcome_kind_distinctness_is_locked() {
    // The three kinds must compare as distinct so
    // the dispatch's `match kind` arm selection
    // never silently falls through to the wrong
    // arm. The compiler enforces `Eq` + `Copy`
    // already, but locking the comparison
    // behaviour at the test boundary surfaces
    // future "aliasing" mistakes (e.g. a
    // `#[derive(Eq)]` that two arms accidentally
    // compare equal to).
    assert_ne!(OutcomeKind::Applied, OutcomeKind::DryRun);
    assert_ne!(OutcomeKind::Applied, OutcomeKind::NoChange);
    assert_ne!(OutcomeKind::DryRun, OutcomeKind::NoChange);
}
