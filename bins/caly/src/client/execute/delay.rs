//! Latency probe / sweep / select-by-delay logic.
//!
//! Round 23: extracted from `client/execute.rs` so the
//! 1100+ line `execute.rs` is a 1-page file again
//! (S4.1 advisory). The select-by-delay + sweep
//! pipeline is self-contained: it reads the daemon
//! snapshot, reconciles against the kernel's
//! controller-side proxy list, then probes the
//! surviving nodes with bounded concurrency. The
//! dispatch in `client::execute::execute_core_cmd`
//! reaches in via [`execute_delay_all`] /
//! [`lowest_latency_node`]; nothing else in the
//! `client` tree needs the internals.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicUsize, Ordering};

use caly_protocol::client::UdsClient;

use crate::client::output;

/// Per-probe timeout for the full latency sweep (`delay --all`).
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Maximum concurrent controller probes in one sweep. Bounded so a 267-node
/// subscription never opens one socket/thread per node against the kernel;
/// the kernel serves probes concurrently, but unbounded client fan-out is
/// wasteful and noisy. 32 balances wall-clock time (dead-node probes burn
/// their full per-probe timeout) against connection fan-out.
const MAX_CONCURRENT_PROBES: usize = 32;

/// Latency-sweeps the daemon snapshot's nodes against the active core
/// controller (offline). The controller-side name is derived from the core:
/// sing-box outbounds are tagged `proxy-<canonical-hex>`; Mihomo uses the
/// rendered display name. Output rows carry the node ID so they can feed
/// `core select` directly.
pub(crate) fn execute_delay_all(
    client: &mut UdsClient,
    url: Option<&str>,
    samples: Option<u32>,
    json: bool,
) -> ExitCode {
    match sweep_snapshot_delays(client, url, samples) {
        Ok(rows) => {
            print_delay_sweep(&rows, json);
            ExitCode::SUCCESS
        }
        Err(error) => output::report_failure(&error, json),
    }
}

/// Sweeps the snapshot's registered nodes against the active core controller
/// and returns the lowest-latency reachable one, or `None` when none answers.
/// Every non-reachable outcome is reported so a silent "no node answers" is
/// never confused with a naming/apply mismatch or a controller outage.
pub(crate) fn lowest_latency_node(
    client: &mut UdsClient,
    json: bool,
) -> Option<([u8; 16], String)> {
    let rows = match sweep_snapshot_delays(client, None, None) {
        Ok(rows) => rows,
        Err(error) => {
            let _ = output::report_failure(&error, json);
            return None;
        }
    };
    let reachable: Vec<(u32, [u8; 16], String)> = rows
        .iter()
        .filter_map(|row| match &row.outcome {
            DelayOutcome::Reachable { samples, .. } => {
                row.outcome
                    .median_ms()
                    .map(|ms| (ms, row.node_id, row.name.clone()))
                    .or_else(|| {
                        // Defensive: an empty
                        // `Reachable` shouldn't
                        // happen (the writer
                        // maps empty to
                        // `Unreachable`), but the
                        // contract is "reachable
                        // has at least one sample".
                        let _ = samples;
                        None
                    })
            }
            _ => None,
        })
        .collect();
    let best = reachable.iter().min_by_key(|(ms, _, _)| *ms);
    if best.is_none() {
        let unreachable = rows
            .iter()
            .filter(|row| row.outcome == DelayOutcome::Unreachable)
            .count();
        let not_in_kernel = rows
            .iter()
            .filter(|row| row.outcome == DelayOutcome::NotInKernel)
            .count();
        let failed = rows
            .iter()
            .filter(|row| row.outcome == DelayOutcome::ProbeFailed)
            .count();
        let _ = output::report_failure(
            &format!(
                "no node answered the latency probe ({unreachable} unreachable, {not_in_kernel} not in the kernel config, {failed} probe errors)"
            ),
            json,
        );
        return None;
    }
    best.map(|(_, node_id, name)| (*node_id, name.clone()))
}

/// How one node's latency probe ended. Every failure mode stays distinct so a
/// sweep never silently collapses a naming/apply mismatch or a controller
/// outage into the same "unreachable" row. The reachable variant carries
/// the full per-sample list (sorted ascending) so the JSON envelope can
/// report `samples` and `jitter_ms` alongside the median.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DelayOutcome {
    /// Kernel answered at least one probe; the
    /// `samples` are sorted ascending in milliseconds.
    /// `median` (the lower middle on even counts) is
    /// the historical single-number summary.
    Reachable {
        samples: Vec<u32>,
        /// URL the kernel answered (stable for this
        /// row; `None` only for the
        /// unreachable-from-every-URL case, but
        /// that lives in the `Unreachable` variant).
        url: String,
    },
    /// Kernel ran the probe but no URL answered.
    Unreachable,
    /// The controller name is not present in the running kernel's proxy set.
    NotInKernel,
    /// Transport/API failure while probing (controller went away, etc.).
    ProbeFailed,
}

impl DelayOutcome {
    /// One-number summary for sorting / printing.
    /// `Reachable` returns the median; every other
    /// variant returns `None` so the caller can
    /// skip the row in `core select --delay`.
    ///
    /// Round 31: delegates to the central
    /// `query::median` helper so the per-node leaf
    /// and the bulk sweep share the same
    /// lower-middle algorithm. The pre-Round 31
    /// shape indexed into the sorted `Vec` with
    /// `len / 2`, which is the upper middle on even
    /// counts (`[50, 200] → 200` instead of the
    /// canonical lower middle 50 — see
    /// `query::median`'s regression test
    /// `even_sample_count_picks_the_lower_middle`).
    fn median_ms(&self) -> Option<u32> {
        match self {
            Self::Reachable { samples, .. } => super::super::query::median(samples),
            _ => None,
        }
    }
}

/// Builds the `Reachable` outcome from a successful
/// probe. Extracted so the worker closure inside
/// `sweep_snapshot_delays` stays under the 100-line
/// clippy ceiling; the helper is trivial but the
/// closure is the hot loop that has to inline-clean.
fn build_reachable_outcome(probe: super::super::query::DelayProbe) -> DelayOutcome {
    let url = probe.url().unwrap_or("").to_owned();
    DelayOutcome::Reachable {
        samples: probe.samples().to_vec(),
        url,
    }
}

/// One row of a latency sweep: the node identity plus its probe outcome.
pub(crate) struct SweepRow {
    node_id: [u8; 16],
    name: String,
    /// Controller-side name used for the probe (and the kernel-set match).
    controller_name: String,
    outcome: DelayOutcome,
}

/// Maps the wire `run_state` code (1–6, [`wire()`] on
/// `WireRunState`: Stopped=1 … Failed=6 — NOT the declaration index)
/// to the domain enum. The previous 0-based mapping read Running as
/// Stopping and made `node ping --all` refuse a healthy core
/// (2026-08-12 audit).
fn wire_run_state(raw: i32) -> caly_domain::CoreRunState {
    let wire = match raw {
        1 => caly_protocol::protocol::v2::WireRunState::Stopped,
        2 => caly_protocol::protocol::v2::WireRunState::Starting,
        3 => caly_protocol::protocol::v2::WireRunState::Running,
        4 => caly_protocol::protocol::v2::WireRunState::Stopping,
        5 => caly_protocol::protocol::v2::WireRunState::Crashed,
        _ => caly_protocol::protocol::v2::WireRunState::Failed,
    };
    wire.into()
}

/// Refuse a sweep with an actionable message when the kernel is not running
/// (a stopped kernel must not surface as a bare controller "Connection
/// refused": 2026-08-12 user-flow audit). A switch/restart in flight can
/// leave a stale snapshot behind, so Stopping/Starting is re-read once
/// before refusing (`caly st` showed running while this guard read
/// Stopping); those in-flight states hint "retry in a moment" instead of
/// `caly core start`, because the kernel is coming up/down on its own.
fn checked_run_state(
    client: &mut UdsClient,
    core: &str,
    snapshot: &caly_protocol::protocol::v2::WirePresentationSnapshot,
) -> Result<(), String> {
    use caly_protocol::client::ClientContract;
    let mut run_state = wire_run_state(snapshot.applied.run_state);
    if matches!(
        run_state,
        caly_domain::CoreRunState::Stopping | caly_domain::CoreRunState::Starting
    ) {
        // Re-read once: the snapshot may be stale while the core is
        // switching/restarting (2026-08-12 audit).
        run_state = client
            .snapshot()
            .map_or(run_state, |fresh| wire_run_state(fresh.applied.run_state));
    }
    if run_state == caly_domain::CoreRunState::Running {
        return Ok(());
    }
    let hint = if matches!(
        run_state,
        caly_domain::CoreRunState::Stopping | caly_domain::CoreRunState::Starting
    ) {
        "retry in a moment"
    } else {
        "start it with `caly core start` and retry"
    };
    Err(format!(
        "the {core} core is not running (run state {run_state:?}); {hint}"
    ))
}

/// Probes every snapshot node through the active core controller with bounded
/// concurrency. Nodes whose controller name is absent from the kernel's real
/// proxy list are reported as `NotInKernel` without probing: probing them
/// would only yield a 404 that the old algorithm silently rendered as
/// "unreachable" (e.g. a subscription that was never applied).
/// A controller handle as built by `client::query::build_control` (used by
/// the sweep helpers to avoid spelling the `dyn KernelControl` type inline).
type Controller = Box<dyn caly_corectl::contract::KernelControl + Send>;

/// Reconcile the snapshot rows against the kernel's real proxy set before
/// probing anything: rows absent from the kernel config are marked
/// `NotInKernel` without probing (a stale/unapplied kernel config surfaces
/// One probe target: row index, kernel controller name, display name.
type ProbeTarget = (usize, String, String);

/// as such instead of a wall of silent 404s). Returns the surviving probe
/// targets plus the controller, which the caller reuses for probing so the
/// reconcile socket is not wasted.
fn reconcile_kernel_targets(
    rows: &mut [SweepRow],
    core: &str,
) -> Result<(Vec<ProbeTarget>, Controller), String> {
    let mut control = crate::client::query::build_control(core)
        .map_err(|error| format!("cannot reach the {core} controller: {error}"))?;
    let kernel_names: std::collections::BTreeSet<String> = control
        .proxy_names(PROBE_TIMEOUT)
        .map_err(|error| format!("cannot read the {core} kernel proxy list: {error}"))?
        .into_iter()
        .collect();
    let mut targets: Vec<ProbeTarget> = Vec::new();
    for (index, row) in rows.iter_mut().enumerate() {
        if kernel_names.contains(&row.controller_name) {
            // The display name rides along so a live picker can update the
            // latency column by node name while the sweep runs.
            targets.push((index, row.controller_name.clone(), row.name.clone()));
        } else {
            row.outcome = DelayOutcome::NotInKernel;
        }
    }
    Ok((targets, control))
}

/// Builds the bounded worker pool: each worker owns one controller (fresh
/// secret read) and drains the shared target queue; the reconcile controller
/// is reused for the first worker so its socket is not wasted.
fn build_control_pool(
    reconcile_control: Controller,
    core: &str,
    worker_count: usize,
) -> Result<Vec<Controller>, String> {
    let mut controls = Vec::with_capacity(worker_count);
    controls.push(reconcile_control);
    for _ in 1..worker_count {
        if let Ok(control) = crate::client::query::build_control(core) {
            controls.push(control);
        }
    }
    if controls.is_empty() {
        return Err(format!("cannot build a {core} controller for probing"));
    }
    Ok(controls)
}

fn sweep_snapshot_delays(
    client: &mut UdsClient,
    url: Option<&str>,
    samples: Option<u32>,
) -> Result<Vec<SweepRow>, String> {
    sweep_snapshot_delays_with(client, url, samples, None, None)
}

/// Live-probe variant of [`sweep_snapshot_delays`]: an interactive picker
/// passes a shared `latencies` map (node display name -> median ms) and a
/// `progress` callback (done/total/dead) so the picker's test-header row
/// and the latency column update while the sweep runs. The plain sweep
/// keeps its `\r` TTY progress line; the callback variant suppresses it
/// (the picker renders its own screen).
//
// `too_many_lines`: the sweep is one dense concurrent block (target
// reconcile, work queue, scoped workers, result collection); splitting it
// across helpers would thread six shared bindings through closures for no
// readability gain.
#[allow(clippy::too_many_lines)]
pub(crate) fn sweep_snapshot_delays_with(
    client: &mut UdsClient,
    url: Option<&str>,
    samples: Option<u32>,
    latencies: Option<&std::sync::Mutex<std::collections::HashMap<String, Option<u32>>>>,
    progress: Option<&(dyn Fn(usize, usize, usize) + Sync)>,
) -> Result<Vec<SweepRow>, String> {
    use caly_protocol::client::ClientContract;
    let snapshot = client.snapshot().map_err(|error| {
        let _ = error;
        "cannot read the daemon snapshot for the node list (is the daemon running?)".to_owned()
    })?;
    let core = super::active_core_kind(client)
        .map_or_else(|| "mihomo".to_owned(), |kind| kind.label().to_owned());
    // checked_run_state refuses a stopped kernel with an actionable message
    // (2026-08-12 user-flow audit: `node ping --all` after a failed core
    // start showed no actionable guidance).
    checked_run_state(client, &core, &snapshot)?;
    let mut rows: Vec<SweepRow> = snapshot
        .nodes
        .iter()
        .map(|node| {
            let controller_name = if core == "sing-box" {
                format!("proxy-{}", caly_domain::to_hex(node.node_id))
            } else {
                node.name.clone()
            };
            SweepRow {
                node_id: node.node_id,
                name: node.name.clone(),
                controller_name,
                outcome: DelayOutcome::Unreachable,
            }
        })
        .collect();
    let (targets, control) = reconcile_kernel_targets(&mut rows, &core)?;
    if targets.is_empty() {
        return Ok(rows);
    }
    let controls = build_control_pool(control, &core, targets.len().min(MAX_CONCURRENT_PROBES))?;
    let total = targets.len();
    let shared = std::sync::Mutex::new(targets);
    let shared = &shared;
    // O-1 (w3a-experience-audit.md): a dead-node-heavy sweep can run for
    // minutes with zero feedback. A shared completion counter renders a
    // `\r`-refreshed progress line on a TTY only; piped stderr stays
    // byte-clean for script consumers.
    let progress_tty = std::io::stderr().is_terminal();
    let completed = AtomicUsize::new(0);
    let dead = AtomicUsize::new(0);
    // Workers move these references (Copy) into their closures.
    let completed = &completed;
    let dead = &dead;
    let results: Vec<(usize, DelayOutcome)> = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for mut control in controls {
            workers.push(scope.spawn(move || {
                let mut local = Vec::new();
                loop {
                    let item = {
                        let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
                        guard.pop()
                    };
                    let Some((index, controller_name, display_name)) = item else {
                        break;
                    };
                    let outcome = match super::super::query::probe_delay(
                        &mut *control,
                        &controller_name,
                        url,
                        PROBE_TIMEOUT,
                        // The sweep defaults to 3 samples (a single
                        // sample is dominated by TCP slow-start, two is
                        // sensitive to a single hiccup); an explicit
                        // `--samples N` from `ping --all` is honoured —
                        // the grammar documents that it trades
                        // wall-clock time for jitter stability.
                        samples.unwrap_or(3).clamp(1, 5),
                    ) {
                        Ok(probe) if probe.is_reachable() => build_reachable_outcome(probe),
                        Ok(_) => DelayOutcome::Unreachable,
                        // A probe that outlived the socket budget (the
                        // kernel hung past its own timeout) is unreachable
                        // from the user's viewpoint, not a controller
                        // transport failure; connect failures stay loud.
                        Err(error)
                            if error.kind
                                == caly_corectl::contract::KernelFailureKind::DeadlineExceeded =>
                        {
                            DelayOutcome::Unreachable
                        }
                        Err(_) => DelayOutcome::ProbeFailed,
                    };
                    if matches!(
                        outcome,
                        DelayOutcome::Unreachable | DelayOutcome::ProbeFailed
                    ) {
                        dead.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some(map) = latencies {
                        map.lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(display_name, outcome.median_ms());
                    }
                    match progress {
                        Some(callback) => {
                            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                            callback(done, total, dead.load(Ordering::Relaxed));
                        }
                        None if progress_tty => {
                            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                            let dead_count = dead.load(Ordering::Relaxed);
                            eprint!("\rprobed {done}/{total} · {dead_count} unreachable");
                        }
                        None => {
                            completed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    local.push((index, outcome));
                }
                local
            }));
        }
        let collected: Vec<(usize, DelayOutcome)> = workers
            .into_iter()
            .flat_map(|worker| worker.join().ok().into_iter().flatten())
            .collect();
        if progress_tty {
            // Clear the progress line before the table renders.
            eprint!("\r\x1b[K");
        }
        collected
    });
    for (index, outcome) in results {
        rows[index].outcome = outcome;
    }
    Ok(rows)
}

mod render;

use render::print_delay_sweep;
