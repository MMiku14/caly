//! Tests for `client/execute/delay.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 31: the bulk-sweep median must use the
//! same lower-middle algorithm as the per-node
//! `core delay` leaf. Pre-Round 31 the
//! `DelayOutcome::median_ms` helper indexed into
//! the sorted samples with `len / 2` (the upper
//! middle on even counts — e.g. `[50, 200]` →
//! 200 instead of the canonical lower middle
//! 50). The two callers now share the central
//! `query::median` helper, so this test pins
//! the bulk-sweep shape to the same contract
//! `query::median`'s `even_sample_count_picks_the_lower_middle`
//! test pins for the per-node leaf.
use super::{DelayOutcome, SweepRow};

/// Two reachable samples `[50, 200]` must
/// report median 50 (lower middle), matching
/// `query::DelayProbe`'s 2-sample median. The
/// pre-Round 31 bulk-sweep shape reported 200
/// (upper middle) — an inconsistency that
/// made the `core select --delay` winner
/// disagree with the per-node leaf for any
/// 2-sample node.
#[test]
fn sweep_even_sample_count_picks_the_lower_middle() {
    let outcome = DelayOutcome::Reachable {
        samples: vec![50, 200],
        url: "https://probe.example/".to_owned(),
    };
    assert_eq!(outcome.median_ms(), Some(50));
}

/// Three reachable samples `[50, 200, 800]` →
/// median 200 (the middle). The pre-Round 31
/// shape also returned 200 for odd counts
/// (the algorithm was the same as the new
/// shape modulo the even-counts corner), so
/// this case is unchanged from before. The
/// test exists as a forward-compatibility
/// guard: a future algorithm drift must keep
/// the canonical 3-sample median.
#[test]
fn sweep_three_sample_median_is_the_middle() {
    let outcome = DelayOutcome::Reachable {
        samples: vec![50, 200, 800],
        url: "https://probe.example/".to_owned(),
    };
    assert_eq!(outcome.median_ms(), Some(200));
}

/// Four reachable samples `[50, 100, 200, 800]`
/// → median 100 (the lower middle, the lower of
/// the two middle indices 1 and 2). The
/// pre-Round 31 shape reported 200 (the upper
/// middle). Sorting is assumed upstream
/// (`DelayProbe::from_raw` sorts on
/// construction), so the helper indexes
/// without re-sorting.
#[test]
fn sweep_four_sample_count_picks_the_lower_middle() {
    let outcome = DelayOutcome::Reachable {
        samples: vec![50, 100, 200, 800],
        url: "https://probe.example/".to_owned(),
    };
    assert_eq!(outcome.median_ms(), Some(100));
}

/// Empty `Reachable` (the kernel dropped every
/// sample — e.g. an immediate disconnect on
/// the very first probe) returns `None`, same
/// as every other non-`Reachable` outcome. The
/// pre-Round 31 shape returned `Some(0)` for
/// the empty case (it indexed into a `len = 0`
/// `Vec` and returned the 0th element, which is
/// 0 — a value that would have been conflated
/// with a real 0ms answer). The new shape
/// treats empty samples as unreachable.
#[test]
fn sweep_empty_reachable_returns_none() {
    let outcome = DelayOutcome::Reachable {
        samples: Vec::new(),
        url: "https://probe.example/".to_owned(),
    };
    assert_eq!(outcome.median_ms(), None);
}

/// Round 31: the sweep JSON envelope now
/// surfaces `min_ms` / `max_ms` / `stdev_ms`
/// alongside the existing `median_ms` /
/// `jitter_ms` / `samples` triple. The test
/// pins the envelope shape by serialising a
/// known `[50, 200, 800]` reachable outcome
/// and asserting every scalar matches the
/// canonical derivation: `min = 50`, `max =
/// 800`, `median = 200`, `jitter = 750`,
/// `stdev = 324` (rounded from
/// `sqrt(105000) ≈ 324.04`).
///
/// The shape is the same one the per-node
/// `print_delay` JSON envelope uses, so a
/// script consumer running both `core
/// delay <node>` and `delay --all` can
/// compare the two paths field-by-field
/// without re-deriving the math.
#[test]
fn sweep_json_envelope_surfaces_min_max_stdev() {
    let row = SweepRow {
        node_id: [0xab; 16],
        name: "node-a".to_owned(),
        controller_name: "proxy-ab".to_owned(),
        outcome: DelayOutcome::Reachable {
            samples: vec![50, 200, 800],
            url: "https://probe.example/".to_owned(),
        },
    };
    let value = super::sweep_row_to_json(&row);
    let obj = value.as_object().unwrap();
    assert_eq!(obj["status"], "ok");
    assert_eq!(obj["median_ms"], 200);
    assert_eq!(obj["min_ms"], 50);
    assert_eq!(obj["max_ms"], 800);
    assert_eq!(obj["jitter_ms"], 750);
    assert_eq!(obj["stdev_ms"], 324);
    // `delay_ms` is the historical alias for
    // `median_ms`; the sweep envelope keeps
    // it for `jq .delay_ms` consumers that
    // pre-date the `median_ms` rename.
    assert_eq!(obj["delay_ms"], 200);
    assert_eq!(obj["samples"], serde_json::json!([50, 200, 800]));
    assert_eq!(obj["controller_name"], "proxy-ab");
}

/// Round 31: a single-sample reachable
/// outcome reports `min = max = median =
/// sample` and `stdev = jitter = None`
/// (no spread to measure). The pre-Round 31
/// envelope only reported `median` and
/// `samples`, so a single-sample node's
/// spread was invisible to script
/// consumers; the new shape surfaces the
/// `min` / `max` pair (which collapse to the
/// single sample) so a `jq` consumer can
/// see "this node answered exactly one
/// probe" without inspecting the `samples`
/// length.
#[test]
fn sweep_single_sample_reports_min_max_collapsed() {
    let row = SweepRow {
        node_id: [0xcd; 16],
        name: "node-b".to_owned(),
        controller_name: "proxy-cd".to_owned(),
        outcome: DelayOutcome::Reachable {
            samples: vec![42],
            url: "https://probe.example/".to_owned(),
        },
    };
    let value = super::sweep_row_to_json(&row);
    let obj = value.as_object().unwrap();
    assert_eq!(obj["min_ms"], 42);
    assert_eq!(obj["max_ms"], 42);
    assert_eq!(obj["median_ms"], 42);
    assert_eq!(obj["stdev_ms"], serde_json::Value::Null);
    assert_eq!(obj["jitter_ms"], serde_json::Value::Null);
}
