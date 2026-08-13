//! Latency probe engine for the `query` module.
//!
//! Split out of `client/query.rs` (audit #70 file-length
//! budget): the multi-sample [`DelayProbe`] value type, the
//! probe-URL failover chain and the sample-collection loop.
//! The dispatch in `client::query::run_query` reaches in via
//! [`probe_delay`]; the `execute::delay` sweep reuses
//! [`median`] for its own statistics.

use caly_corectl::contract::KernelControl;

pub(crate) const DEFAULT_SAMPLES: u32 = 3;

/// Resolves a `--samples` argument into a clamped
/// sample count. CLI callers pass `Option<u32>` so
/// the operator can ask for 1 (fast probe) or 5
/// (jitter-heavy) without re-binding the dispatch.
/// The clamp [1, 5] is the same one the writer
/// applies, so the dispatch and the writer agree
/// on the contract.
pub(crate) fn resolve_samples(samples: Option<u32>) -> u32 {
    samples.unwrap_or(DEFAULT_SAMPLES).clamp(1, 5)
}

/// Probe targets for latency estimation, ordered by reachability of a clean
/// 204 across egresses. The kernel dials each node through the proxy, so a
/// node that cannot reach one target may reach another (different egress /
/// anycast); the first URL that answers wins, double-sampled for stability.
///
/// Cloudflare's anycast CDN leads: it is reachable from most egresses even
/// where the Google endpoints are blocked or constrained (e.g. a CN exit),
/// so the common case answers on the first URL rather than stalling the whole
/// failover across the blocked Google targets.
pub(crate) const PROBE_URLS: [&str; 3] = [
    "https://cp.cloudflare.com/generate_204",
    "https://www.google.com/generate_204",
    "https://www.gstatic.com/generate_204",
];

/// Multi-sample latency probe outcome for one node /
/// URL pair. Carries the raw samples so callers can
/// render the median *and* the spread (`jitter_ms`)
/// — a node at "50ms median ± 200ms jitter" is
/// qualitatively different from "50ms steady" even
/// though the median is the same.
///
/// `samples` is sorted ascending. `None` is the
/// "the kernel could not reach the URL at all"
/// case (HTTP 5xx, transport reset, etc.); the
/// caller distinguishes it from a kernel-side
/// `DeadlineExceeded` (which is mapped to
/// `Ok(None)` by the per-probe call and surfaces
/// upstream as the next URL in the failover chain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DelayProbe {
    /// Per-sample latencies in milliseconds, sorted
    /// ascending. `len() == samples` requested;
    /// `is_empty()` means the node could not reach
    /// the URL at all (kernel returned `Ok(None)`
    /// for every attempt).
    samples: Vec<u32>,
    /// Which URL the kernel answered. `None` when
    /// the node could not reach any URL.
    url: Option<String>,
}

impl DelayProbe {
    /// `true` when at least one sample answered. The
    /// human output uses this to render "reachable"
    /// vs "unreachable".
    pub fn is_reachable(&self) -> bool {
        !self.samples.is_empty()
    }

    /// Builds a `DelayProbe` from a raw sample
    /// vector. The samples are sorted ascending
    /// so `jitter_ms` and the JSON `samples`
    /// payload are stable across runs (kernel
    /// scheduling jitter can otherwise reorder
    /// the `VecDeque.pop_front` results).
    pub(crate) fn from_raw(samples: Vec<u32>, url: Option<String>) -> Self {
        let mut sorted = samples;
        sorted.sort_unstable();
        Self {
            samples: sorted,
            url,
        }
    }

    /// The single-number summary: median of the
    /// samples (3-sample median is more spike-
    /// resistant than 2-sample min and avoids the
    /// "first sample won" bias of the historical
    /// `min` policy). Empty samples return `None`.
    pub(crate) fn median_ms(&self) -> Option<u32> {
        median(&self.samples)
    }

    /// `max - min` of the samples, or `None` when
    /// fewer than 2 samples answered. The operator
    /// uses this to spot flaky links: a node with a
    /// 200ms median and 600ms jitter is unreliable
    /// even if the median is in the "fast" tier.
    pub fn jitter_ms(&self) -> Option<u32> {
        jitter_ms(&self.samples)
    }

    /// Round 31: minimum of the sorted
    /// samples. `None` for fewer than 1
    /// sample (the empty / all-drops
    /// case). For 1 sample the value is
    /// trivially that single sample. The
    /// operator uses this to spot a
    /// "lucky first probe" pattern: a node
    /// at `min = 50ms, max = 800ms, median
    /// = 200ms` answered 50ms once but is
    /// otherwise spiky — the `min` makes
    /// the bias visible alongside the
    /// already-reported `median_ms` /
    /// `jitter_ms` / `samples` triple.
    pub fn min_ms(&self) -> Option<u32> {
        self.samples.first().copied()
    }

    /// Round 31: maximum of the sorted
    /// samples. The mirror of `min_ms`;
    /// see that method's doc for the
    /// operator-facing rationale.
    pub fn max_ms(&self) -> Option<u32> {
        self.samples.last().copied()
    }

    /// Round 31: population standard deviation
    /// of the samples, rounded to the nearest
    /// `u32` millisecond. `None` for fewer than
    /// 2 samples (a single observation has no
    /// spread). The operator reads `stdev` as
    /// the "shape" of the latency distribution:
    /// `median = 100ms, stdev = 5ms` is
    /// rock-solid; `median = 100ms, stdev =
    /// 50ms` is "around 100ms" but the actual
    /// dial could land anywhere from 50 to
    /// 150ms. The helper uses the
    /// population formula (divide by `n`, not
    /// `n - 1`) because every sample is the
    /// full set (no sampling-from-a-larger-
    /// distribution inference). The integer
    /// `u32` representation is bounded at
    /// ~4.29e9 ms (~50 days) which is well
    /// above the operator-relevant range.
    ///
    /// The arithmetic lives in the free [`stdev_ms`]
    /// helper (shared with the bulk sweep renderer,
    /// which holds the samples in
    /// `DelayOutcome::Reachable` rather than in a
    /// `DelayProbe`); the `cast_*_lossy` allows
    /// ride along on the helper.
    pub fn stdev_ms(&self) -> Option<u32> {
        stdev_ms(&self.samples)
    }

    /// Raw samples (sorted ascending). Useful for
    /// script consumers who want the full
    /// distribution, not just the summary.
    pub fn samples(&self) -> &[u32] {
        &self.samples
    }

    /// The URL that answered. Stable for any single
    /// `DelayProbe` instance.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}

/// `max - min` of the samples, or `None` when fewer than 2 samples
/// answered. Shared by [`DelayProbe::jitter_ms`] and the bulk-sweep
/// renderer (`execute::delay::render`), which holds the samples inside
/// `DelayOutcome::Reachable` instead of a `DelayProbe`.
pub(crate) fn jitter_ms(samples: &[u32]) -> Option<u32> {
    if samples.len() < 2 {
        return None;
    }
    let min = *samples.first()?;
    let max = *samples.last()?;
    Some(max - min)
}

/// Population standard deviation of the samples, rounded to the nearest
/// `u32` millisecond; `None` for fewer than 2 samples. Shared by
/// [`DelayProbe::stdev_ms`] and the bulk-sweep renderer (see
/// [`jitter_ms`] for why the free form exists). The population formula
/// (divide by `n`, not `n - 1`) and the `f64` accumulator are explained
/// in the method's doc; the `cast_*_lossy` allows document the
/// intentional precision trade.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
pub(crate) fn stdev_ms(samples: &[u32]) -> Option<u32> {
    if samples.len() < 2 {
        return None;
    }
    let n = samples.len() as u64;
    let sum: u64 = samples.iter().map(|s| u64::from(*s)).sum();
    let mean = sum as f64 / n as f64;
    let variance = samples
        .iter()
        .map(|s| {
            let diff = f64::from(*s) - mean;
            diff * diff
        })
        .sum::<f64>()
        / n as f64;
    Some(variance.sqrt().round() as u32)
}

/// Returns the median of `samples`. For even counts
/// the lower of the two middle values is returned —
/// this matches the operator's mental model "a
/// latency of N is the smallest range that contains
/// the middle of the distribution" and avoids the
/// averaging that would inflate a single 50ms spike
/// into a 60ms answer.
pub(crate) fn median(samples: &[u32]) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted: Vec<u32> = samples.to_vec();
    sorted.sort_unstable();
    // `len / 2` rounds *down* for both odd and
    // even lengths: `[a]` → `[0]` (the single
    // element), `[a, b]` → `[0]` (the lower of
    // the two), `[a, b, c]` → `[1]` (the middle).
    Some(sorted[(sorted.len() - 1) / 2])
}

/// Probes one node through the active core controller with URL failover
/// and a 3-sample median summary.
///
/// * URLs are tried in order; the first URL the node can reach wins.
/// * On the winning URL the writer collects `samples` measurements
///   (default 3) and reports the median. Earlier the writer took
///   `min` of 2 samples, which under-estimated a single-shot spike
///   on the *second* sample: the kernel's first response includes
///   the operator's TCP / TLS handshake (cold cache), the second
///   response is warm, so `min` always picked the second. Median
///   of 3 is unbiased across cold/warm/hiccup.
/// * **Fast failover (W3a 优化)**: each URL in the chain is decided by
///   a single reachability sample — a down node otherwise burns its
///   per-probe timeout on *every* URL (3 URLs × 3 samples × 5 s =
///   45 s per dead node; the fast path cuts that to 15 s, and the
///   `delay --all` sweep inherits the same saving on every dead node).
///   The winning URL then tops up to the requested sample count; the
///   total probe count and the inter-sample gap are unchanged, so the
///   median semantics are identical for a reachable node.
/// * `samples = 1` keeps the historical single-shot
///   behaviour for callers that want the absolute
///   fastest probe (a script piping `delay` into
///   `core select --delay` round-trips).
/// * All URLs unreachable maps to `Ok(DelayProbe{ samples: vec![], url: None })`;
///   a controller transport error (connect failure, kernel
///   gone) stays `Err` so callers can distinguish an
///   unreachable node from a probe infrastructure
///   fault.
pub(crate) fn probe_delay(
    control: &mut dyn KernelControl,
    name: &str,
    url: Option<&str>,
    timeout: std::time::Duration,
    samples: u32,
) -> Result<DelayProbe, caly_corectl::contract::KernelFailure> {
    let urls: Vec<&str> = match url {
        Some(value) => vec![value],
        None => PROBE_URLS.to_vec(),
    };
    // Clamp the sample count: 1 keeps the legacy
    // "fast probe" path; >5 wastes operator time on
    // a `delay --all` sweep without materially
    // changing the median. The CLI validates the
    // upper bound at parse time, so reaching this
    // branch with a too-large value means a
    // programmatic caller (test / future wire
    // command) tried to override the contract.
    let samples = samples.clamp(1, 5);
    for probe_url in urls {
        // Fast reachability decision: one sample is enough to know
        // whether this URL is worth the full sample budget.
        let collected = collect_samples(control, name, probe_url, timeout, 1, SAMPLE_GAP);
        match collected {
            Ok(Some(mut collected)) => {
                if samples > 1 {
                    // The gap between the fast sample and the first
                    // top-up keeps the samples independent (the fast
                    // sample already exercised the kernel's per-name
                    // cache). A later `Ok(None)` (the kernel dropped
                    // the connection mid-sweep) keeps the partial
                    // result, matching the pre-optimisation contract.
                    std::thread::sleep(SAMPLE_GAP);
                    if let Some(mut tail) =
                        collect_samples(control, name, probe_url, timeout, samples - 1, SAMPLE_GAP)?
                    {
                        collected.append(&mut tail);
                    }
                }
                return Ok(DelayProbe::from_raw(collected, Some(probe_url.to_owned())));
            }
            Ok(None) => {
                // This URL is unreachable from this
                // node; try the next one in the
                // failover chain.
            }
            Err(error) => return Err(error),
        }
    }
    Ok(DelayProbe {
        samples: Vec::new(),
        url: None,
    })
}

/// Inter-sample gap between probes. The kernel's
/// per-name cache (TFO / TLS session resumption)
/// keeps a back-to-back probe from exercising a
/// fresh dial path, so a small gap is required.
/// 100ms is short enough to keep the operator's
/// wall-clock low for `delay --all`, long enough
/// to defeat the cache on a hot egress.
const SAMPLE_GAP: std::time::Duration = std::time::Duration::from_millis(100);

/// Probes one URL with up to `samples` measurements
/// and returns the successful ones. A 100ms gap
/// between samples keeps a hot egress from queueing
/// the second and third requests behind the first
/// (Mihomo's controller is single-threaded per
/// connection group; back-to-back probes otherwise
/// all serialise on the same TFO cache key).
///
/// `gap` is the inter-sample sleep; the test suite
/// passes a sub-millisecond gap to keep the suite
/// fast, production callers pass [`SAMPLE_GAP`].
pub(crate) fn collect_samples(
    control: &mut dyn KernelControl,
    name: &str,
    url: &str,
    timeout: std::time::Duration,
    samples: u32,
    gap: std::time::Duration,
) -> Result<Option<Vec<u32>>, caly_corectl::contract::KernelFailure> {
    let mut collected: Vec<u32> = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        match control.test_delay_url(name, url, timeout) {
            Ok(Some(ms)) => {
                collected.push(ms);
            }
            Ok(None) => {
                // A late sample lost the race; the
                // earlier samples are still
                // representative. Return them.
                if collected.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(collected));
            }
            Err(error)
                if error.kind == caly_corectl::contract::KernelFailureKind::DeadlineExceeded =>
            {
                // The probe stalled on this URL.
                // The node might still answer a
                // different URL; surface the
                // partial result if we have any,
                // otherwise let the caller try
                // the next URL.
                if collected.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(collected));
            }
            Err(error) => return Err(error),
        }
        // The unit tests pass a sub-millisecond
        // gap so the suite runs in milliseconds,
        // not seconds. Production callers use
        // [`SAMPLE_GAP`] to defeat the kernel's
        // per-name cache.
        if samples > 1 {
            std::thread::sleep(gap);
        }
    }
    Ok(Some(collected))
}
