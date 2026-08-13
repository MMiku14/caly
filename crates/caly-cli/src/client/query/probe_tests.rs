//! Tests for `client/query.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use caly_corectl::contract::{KernelControl, KernelFailure};
use std::sync::PoisonError;
use std::time::Duration;

/// A test controller whose delay is a
/// caller-supplied sequence. Lets the unit
/// suite assert the median / jitter math
/// without spinning up a real kernel.
struct ScriptedController {
    /// Each entry is one `test_delay_url` call's
    /// outcome. The probe loop drains this FIFO
    /// in order; once exhausted it returns
    /// `Ok(None)` (the URL dropped mid-sweep).
    queue: std::sync::Mutex<std::collections::VecDeque<u32>>,
}

impl ScriptedController {
    fn with_samples(samples: &[u32]) -> Self {
        Self {
            queue: std::sync::Mutex::new(samples.iter().copied().collect()),
        }
    }
}

impl KernelControl for ScriptedController {
    fn capabilities(&self) -> caly_domain::CapabilitySet {
        // The test stub does not exercise the
        // capabilities surface; an empty
        // capability set is the correct
        // "no capabilities declared" answer
        // for a unit test.
        use caly_domain::BoundedVec;
        caly_domain::CapabilitySet::from_bounded_dedup(BoundedVec::default())
    }
    fn wait_ready(&mut self, _timeout: Duration) -> Result<(), KernelFailure> {
        Ok(())
    }
    fn select_proxy(
        &mut self,
        _node: caly_domain::NodeId,
        _timeout: Duration,
    ) -> Result<(), KernelFailure> {
        Ok(())
    }
    fn health_check(&mut self, _timeout: Duration) -> Result<(), KernelFailure> {
        Ok(())
    }
    fn test_delay_url(
        &mut self,
        _name: &str,
        _url: &str,
        _timeout: Duration,
    ) -> Result<Option<u32>, KernelFailure> {
        let mut guard = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(guard.pop_front())
    }

    fn proxy_names(&mut self, _timeout: Duration) -> Result<Vec<String>, KernelFailure> {
        Ok(Vec::new())
    }
    fn proxy_groups(&mut self, _timeout: Duration) -> Result<Vec<ProxyGroup>, KernelFailure> {
        Ok(Vec::new())
    }
    fn connections(&mut self, _timeout: Duration) -> Result<ConnectionSummary, KernelFailure> {
        Ok(ConnectionSummary {
            active: 0,
            download_bytes: 0,
            upload_bytes: 0,
        })
    }
    fn traffic(&mut self, _timeout: Duration) -> Result<(u64, u64), KernelFailure> {
        Ok((0, 0))
    }
}

#[test]
fn median_of_three_samples_is_the_middle() {
    // [50, 200, 800] → median 200. The historical
    // `min` policy would have answered 50; the
    // operator never saw the true hot/warm/hiccup
    // distribution.
    let mut controller = ScriptedController::with_samples(&[50, 200, 800]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        3,
    )
    .unwrap();
    assert_eq!(probe.median_ms(), Some(200));
    assert_eq!(probe.jitter_ms(), Some(800 - 50));
    assert_eq!(probe.samples(), &[50, 200, 800]);
}

#[test]
fn jitter_is_none_for_a_single_sample() {
    let mut controller = ScriptedController::with_samples(&[42]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        1,
    )
    .unwrap();
    assert_eq!(probe.samples().len(), 1);
    assert_eq!(probe.jitter_ms(), None);
    assert_eq!(probe.median_ms(), Some(42));
}

#[test]
fn samples_clamps_to_a_safe_range() {
    // `samples = 0` is clamped to 1 (one
    // measurement still answers the question);
    // `samples = 99` is clamped to 5 (more
    // wastes wall-clock time without changing
    // the median). The Queue has only 5 entries
    // so the clamp prevents a panic on a too-
    // large requested sample count.
    let mut controller = ScriptedController::with_samples(&[10, 20, 30, 40, 50]);
    let mut probe_zero_controller = ScriptedController::with_samples(&[99]);
    let probe_zero = probe_delay(
        &mut probe_zero_controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        0,
    )
    .unwrap();
    assert_eq!(probe_zero.samples().len(), 1);
    let probe_huge = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        99,
    )
    .unwrap();
    assert_eq!(probe_huge.samples().len(), 5);
}

#[test]
fn empty_samples_after_url_drain_returns_unreachable() {
    // No samples answered. The probe is
    // reachable=false; the median is None; the
    // envelope `url` is None so the dispatch can
    // tell the operator "we tried every URL, all
    // were silent".
    let mut controller = ScriptedController::with_samples(&[]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        3,
    )
    .unwrap();
    assert!(!probe.is_reachable());
    assert_eq!(probe.median_ms(), None);
    assert_eq!(probe.url(), None);
}

#[test]
fn partial_samples_preserved_when_url_keeps_failing() {
    // First two samples answered (50, 200 ms);
    // the third call returns `Ok(None)`
    // (kernel dropped the connection). The
    // historical implementation would have
    // returned `Ok(None)` and lost the two
    // good samples. The new implementation
    // returns the partial result so the operator
    // sees *some* number for the node instead of
    // a misleading "unreachable" verdict that
    // ignored the two good measurements.
    let mut controller = ScriptedController::with_samples(&[50, 200]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        3,
    )
    .unwrap();
    assert_eq!(probe.samples(), &[50, 200]);
    // Median of sorted [50, 200] is the lower
    // middle (50). The historical `min` policy
    // also returned 50 here, so this case is
    // unchanged from Round 17. The behavioural
    // change is `[50, 200, 800]` → 200 (the
    // middle), not 50 (the min).
    assert_eq!(probe.median_ms(), Some(50));
}

#[test]
fn even_sample_count_picks_the_lower_middle() {
    // For 2 samples the median is the lower
    // value: [50, 200] → 50. The historical
    // `min` policy had the same answer; the
    // behaviour is now consistent across 1, 2,
    // 3, 4, 5 samples and documented in the
    // `median` doc-comment.
    let mut controller = ScriptedController::with_samples(&[50, 200]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        2,
    )
    .unwrap();
    assert_eq!(probe.median_ms(), Some(50));
}

/// The free `jitter_ms` / `stdev_ms` helpers are the single source of
/// truth for the spread numbers: `DelayProbe` delegates to them and the
/// bulk-sweep renderer (`execute::delay::render`) calls them on
/// `DelayOutcome::Reachable`'s sample vector. Pin the free-fn contract
/// directly so a drift between the two callers surfaces here.
#[test]
fn free_spread_helpers_match_the_method_contract() {
    assert_eq!(jitter_ms(&[50, 200]), Some(150));
    assert_eq!(jitter_ms(&[42]), None);
    assert_eq!(stdev_ms(&[50, 200, 800]), Some(324));
    assert_eq!(stdev_ms(&[100, 100, 100]), Some(0));
    assert_eq!(stdev_ms(&[42]), None);
}

/// Round 31: `min_ms` / `max_ms` are the
/// endpoints of the sorted sample vector.
/// The pre-Round 31 envelope only surfaced
/// the median / jitter / samples triple;
/// the new shape surfaces the min and max
/// explicitly so a script consumer can see
/// the bias without re-deriving it from the
/// `samples` array. A single sample has
/// `min = max = sample` (no spread).
#[test]
fn min_and_max_are_sample_endpoints() {
    let mut controller = ScriptedController::with_samples(&[50, 200, 800]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        3,
    )
    .unwrap();
    assert_eq!(probe.min_ms(), Some(50));
    assert_eq!(probe.max_ms(), Some(800));
    assert_eq!(probe.min_ms(), probe.samples().first().copied());
    assert_eq!(probe.max_ms(), probe.samples().last().copied());
}

/// Round 31: a single sample has
/// `min = max = median = sample` and
/// `stdev = jitter = None` (no spread to
/// measure). The pre-Round 31 envelope had
/// the same answer for the median but
/// silently did not surface min / max /
/// stdev at all. The new shape surfaces
/// every scalar the operator can derive
/// from the samples.
#[test]
fn single_sample_has_min_max_equal_to_sample() {
    let mut controller = ScriptedController::with_samples(&[42]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        1,
    )
    .unwrap();
    assert_eq!(probe.min_ms(), Some(42));
    assert_eq!(probe.max_ms(), Some(42));
    assert_eq!(probe.median_ms(), Some(42));
    assert_eq!(probe.stdev_ms(), None);
    assert_eq!(probe.jitter_ms(), None);
}

/// Round 31: the population standard
/// deviation for the canonical
/// `[50, 200, 800]` shape. The mean is
/// `(50 + 200 + 800) / 3 = 350`. The
/// squared deviations are
/// `(50 - 350)^2 = 90000`,
/// `(200 - 350)^2 = 22500`,
/// `(800 - 350)^2 = 202500`. Sum
/// = `315000`. Population variance
/// = `315000 / 3 = 105000`. Population
/// stdev = `sqrt(105000) ≈ 324.04`,
/// rounded to `324`. The integer rounding
/// means a future change to `f64`
/// precision in the JSON envelope stays
/// stable.
#[test]
fn stdev_for_canonical_three_samples_is_documented_constant() {
    let mut controller = ScriptedController::with_samples(&[50, 200, 800]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        3,
    )
    .unwrap();
    assert_eq!(probe.stdev_ms(), Some(324));
}

/// Round 31: an even sample count of
/// `[50, 100, 200, 800]`. The mean is
/// `(50 + 100 + 200 + 800) / 4 = 287.5`.
/// Squared deviations:
/// `(50 - 287.5)^2 = 56406.25`,
/// `(100 - 287.5)^2 = 35156.25`,
/// `(200 - 287.5)^2 = 7656.25`,
/// `(800 - 287.5)^2 = 262656.25`. Sum
/// = `361875`. Population variance
/// = `361875 / 4 = 90468.75`.
/// Population stdev
/// = `sqrt(90468.75) ≈ 300.78`,
/// rounded to `301`.
#[test]
fn stdev_for_even_sample_count_uses_population_formula() {
    let mut controller = ScriptedController::with_samples(&[50, 100, 200, 800]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        4,
    )
    .unwrap();
    assert_eq!(probe.stdev_ms(), Some(301));
}

/// Round 31: a uniform sample vector
/// (e.g. `[100, 100, 100]`) has zero
/// spread. The pre-Round 31 jitter helper
/// returned 0 for the difference but
/// `None` for a single sample; the new
/// `stdev_ms` returns 0 for any
/// `n >= 2` uniform distribution. The
/// test pins the contract so a future
/// "always return 0 for `n >= 2`" change
/// surfaces here.
#[test]
fn stdev_is_zero_for_a_uniform_sample_vector() {
    let mut controller = ScriptedController::with_samples(&[100, 100, 100]);
    let probe = probe_delay(
        &mut controller,
        "test",
        Some("https://probe.example/"),
        Duration::from_millis(100),
        3,
    )
    .unwrap();
    assert_eq!(probe.stdev_ms(), Some(0));
    assert_eq!(probe.jitter_ms(), Some(0));
    assert_eq!(probe.min_ms(), Some(100));
    assert_eq!(probe.max_ms(), Some(100));
}
