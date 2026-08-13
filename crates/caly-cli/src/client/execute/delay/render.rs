//! Sweep rendering: text rows and the JSON projection.
//!
//! Split out of `client/execute/delay.rs` (audit #70 file-length
//! budget): the human table (`print_delay_sweep`) and the
//! per-row JSON shape (`sweep_row_to_json`) share the
//! sorted-by-name ordering and the star / parens annotations.

use super::super::super::hex;
use super::super::super::query::{jitter_ms, stdev_ms};
use super::{DelayOutcome, SweepRow};

/// Renders one sweep row as a JSON object. The
/// `delay_ms` field is the median (kept for
/// historical `jq .delay_ms` consumers); `jitter_ms`,
/// `min_ms`, `max_ms`, `stdev_ms`, and the full
/// `samples` array are additive so a script
/// consumer can see the spread and the per-sample
/// distribution. `controller_name` is added to
/// every row so a script can grep the kernel-side
/// name even when the snapshot's `name` is a
/// hash-derived label (sing-box).
///
/// Round 31: the JSON envelope now mirrors the
/// per-node `print_delay` shape — the
/// `min_ms` / `max_ms` / `stdev_ms` fields are the
/// same values the per-node `core delay` path
/// reports, so a script consumer can run
/// `core delay <node>` and get the exact same
/// distribution numbers as `delay --all` (the
/// pre-Round 31 shape only reported the median
/// and `jitter` / `samples` in the sweep, so a
/// script consumer had to redo the math to
/// compare the two paths).
pub(super) fn sweep_row_to_json(row: &SweepRow) -> serde_json::Value {
    use serde_json::json;
    let mut value = match &row.outcome {
        DelayOutcome::Reachable { samples, url } => {
            let median = row.outcome.median_ms();
            let jitter = jitter_ms(samples);
            // Round 31: min / max / stdev are
            // derived from the same `samples`
            // vector the JSON envelope
            // already carries. The pre-Round 31
            // shape had only `median_ms` /
            // `jitter_ms` / `samples`; the new
            // shape adds `min_ms` / `max_ms` /
            // `stdev_ms` so a script consumer
            // can read the full distribution
            // shape without re-deriving the
            // numbers from `samples`.
            let min_ms = samples.first().copied();
            let max_ms = samples.last().copied();
            // Round 31: min / max / stdev are
            // derived from the same `samples`
            // vector the JSON envelope
            // already carries. The pre-Round 31
            // shape had only `median_ms` /
            // `jitter_ms` / `samples`; the new
            // shape adds `min_ms` / `max_ms` /
            // `stdev_ms` so a script consumer
            // can read the full distribution
            // shape without re-deriving the
            // numbers from `samples`. The
            // spread helpers are the same
            // free functions `query::DelayProbe`
            // delegates to (the `SweepRow` shape
            // keeps the samples inside
            // `DelayOutcome::Reachable`, not in a
            // `DelayProbe`, so the methods are
            // not directly reachable here).
            let stdev_ms = stdev_ms(samples);
            json!({
                "id": hex(row.node_id),
                "name": row.name.as_str(),
                "status": "ok",
                "median_ms": median,
                "delay_ms": median,
                "min_ms": min_ms,
                "max_ms": max_ms,
                "jitter_ms": jitter,
                "stdev_ms": stdev_ms,
                "samples": samples,
                "url": url,
            })
        }
        DelayOutcome::Unreachable => json!({
            "id": hex(row.node_id),
            "name": row.name.as_str(),
            "status": "unreachable",
            "median_ms": null,
            "delay_ms": null,
        }),
        DelayOutcome::NotInKernel => json!({
            "id": hex(row.node_id),
            "name": row.name.as_str(),
            "status": "not_in_kernel",
            "median_ms": null,
            "delay_ms": null,
        }),
        DelayOutcome::ProbeFailed => json!({
            "id": hex(row.node_id),
            "name": row.name.as_str(),
            "status": "error",
            "median_ms": null,
            "delay_ms": null,
        }),
    };
    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "controller_name".to_owned(),
            serde_json::Value::String(row.controller_name.clone()),
        );
    }
    value
}

/// Renders the delay sweep: reachable first (ascending by median), then
/// unreachable in name order, then nodes missing from the kernel config.
/// Every row includes the node ID for `core select`. The JSON form
/// carries `samples` and `jitter_ms` per reachable row so a script
/// consumer can see the full distribution; the human form prints the
/// median and a one-character jitter glyph.
pub(super) fn print_delay_sweep(rows: &[SweepRow], json: bool) {
    use serde_json::json;
    let mut reachable: Vec<&SweepRow> = rows
        .iter()
        .filter(|row| matches!(row.outcome, DelayOutcome::Reachable { .. }))
        .collect();
    // Sort by the median (the lower middle), not the
    // min — a node with [50, 200, 800] sorts at 200,
    // the historical `min` policy would have placed
    // it at 50 next to a node that answered 50, 50,
    // 50. The operator's `core select --delay`
    // choice should be the stable one, not the lucky
    // sample.
    reachable.sort_by_key(|row| row.outcome.median_ms().unwrap_or(u32::MAX));
    let unreachable = sorted_by_name(rows, DelayOutcome::Unreachable);
    let not_in_kernel = sorted_by_name(rows, DelayOutcome::NotInKernel);
    let probe_failed = sorted_by_name(rows, DelayOutcome::ProbeFailed);
    if json {
        let rows_json: Vec<serde_json::Value> = rows.iter().map(sweep_row_to_json).collect();
        println!(
            "{}",
            json!({
                "probes": rows_json,
                "total": rows.len(),
                "reachable": reachable.len(),
                "unreachable": unreachable.len(),
                "not_in_kernel": not_in_kernel.len(),
                "probe_errors": probe_failed.len(),
            })
        );
        return;
    }
    let suffix = if not_in_kernel.is_empty() {
        String::new()
    } else {
        format!(", {} not in kernel config", not_in_kernel.len())
    };
    let failed_suffix = if probe_failed.is_empty() {
        String::new()
    } else {
        format!(", {} probe errors", probe_failed.len())
    };
    println!(
        "probes: {} total, {} reachable, {} unreachable{suffix}{failed_suffix} (run `caly node select --delay` for the fastest)",
        rows.len(),
        reachable.len(),
        unreachable.len()
    );
    for row in &reachable {
        let DelayOutcome::Reachable { samples, .. } = &row.outcome else {
            continue;
        };
        let median = row.outcome.median_ms().unwrap_or(0);
        // One-character jitter glyph:
        // `.` — steady (jitter <= 10ms, or single sample)
        // `~` — mild   (jitter <= 25% of median)
        // `!` — spiky  (jitter > 25% of median)
        // The operator learns at a glance which
        // "fast" nodes are actually reliable.
        let jitter = jitter_ms(samples).unwrap_or(0);
        let glyph = if samples.len() < 2 {
            '.'
        } else if median == 0 {
            '~'
        } else if jitter * 4 <= median {
            '.'
        } else if jitter * 4 <= median * 5 {
            '~'
        } else {
            '!'
        };
        print_sweep_row(row, &format!("{median:>6} ms {glyph}"), "");
    }
    for row in &unreachable {
        print_sweep_row(row, "-", "");
    }
    for row in &not_in_kernel {
        print_sweep_row(row, "!", "  not in kernel config");
    }
    for row in &probe_failed {
        print_sweep_row(row, "?", "  probe error");
    }
    if !not_in_kernel.is_empty() {
        println!(
            "hint: nodes not in the running kernel config — run `caly config apply` to publish subscription nodes, then retry `caly node ping --all`"
        );
    }
}

/// Rows with the given outcome, name-ordered (deterministic sweep tables).
pub(super) fn sorted_by_name(rows: &[SweepRow], outcome: DelayOutcome) -> Vec<&SweepRow> {
    let mut selected: Vec<&SweepRow> = rows.iter().filter(|row| row.outcome == outcome).collect();
    selected.sort_by(|a, b| a.name.cmp(&b.name));
    selected
}

/// Prints one sweep row: `<marker>  <32-hex ID>  <name>[annotation]`.
/// The marker column is fixed-width so every row's ID column aligns.
/// Width 10 accommodates `9999 ms !` (the `!` glyph marks
/// spiky jitter; the per-row human rendering adds a single
/// trailing character so 7 was the historical width and 10
/// covers the four-digit case without clipping).
fn print_sweep_row(row: &SweepRow, marker: &str, annotation: &str) {
    println!(
        "  {marker:<10}  {}  {}{}",
        hex(row.node_id),
        row.name.as_str(),
        annotation
    );
}

#[cfg(test)]
mod sweep_median_tests;
