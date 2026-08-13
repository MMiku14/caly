//! `caly tool dns` — DNS reachability diagnostic.
//!
//! Probes the configured (or default public) nameservers for a domain IN
//! PARALLEL — one bounded thread per server — and reports each outcome with
//! its round-trip latency. Uses the bounded `caly_dns::probe` client so it
//! works without a full DNS library; the probe transaction id comes from the
//! platform CSPRNG (caly-dns receives it caller-injected, crate-replan §5.12).

use caly_dns::MAX_DNS_SERVERS;
use caly_dns::probe::{DNS_PROBE_TIMEOUT, DnsProbeOutcome, DnsProbeReport, probe_nameserver_timed};
use std::process::ExitCode;

/// Default nameservers to probe when `CALY_DNS_NAMESERVERS` is unset.
const DEFAULT_NAMESERVERS: &[&str] = &["8.8.8.8", "1.1.1.1", "223.5.5.5"];
/// Probe fan-out bound: matches the domain group bound; longer env/config
/// lists are truncated with a notice (pre-fix: unbounded sequential probing —
/// N dead servers cost N × the timeout).
const MAX_PROBED_NAMESERVERS: usize = MAX_DNS_SERVERS;

/// One probed nameserver row (ordered output model for both renderers).
struct ProbeRow {
    nameserver: String,
    status: &'static str,
    detail: Option<String>,
    latency_ms: u64,
}

/// Runs a DNS reachability diagnostic for `domain`.
///
/// A `Resolved(None)` outcome means the nameserver answered with
/// an empty answer section (NXDOMAIN-style or no A records): that
/// is NOT a working resolver for the operator's purposes, so it no
/// longer flips `any_resolved`. When `json` is set the per-server
/// outcomes are emitted as one stable object, keeping `caly tool
/// dns --json` machine-consumable.
pub fn run_dns(domain: Option<String>, json: bool) -> ExitCode {
    let domain = domain.unwrap_or_else(|| "example.com".to_owned());
    let (nameservers, truncated) = cap_nameservers(configured_nameservers());
    if !json {
        println!("dns probe for {domain}:");
        if truncated {
            eprintln!(
                "dns: server list exceeds {MAX_PROBED_NAMESERVERS}; probing the first {MAX_PROBED_NAMESERVERS}"
            );
        }
    }
    let rows = probe_all(&nameservers, &domain);
    let any_resolved = rows.iter().any(|row| row.status == "resolved");
    if json {
        print_json(&domain, &rows, any_resolved, truncated);
    } else {
        print_human(&rows);
    }
    if any_resolved {
        ExitCode::SUCCESS
    } else {
        eprintln!("dns: no configured nameserver resolved {domain}");
        ExitCode::from(3)
    }
}

/// Probes every nameserver concurrently over scoped threads; output order
/// stays the input order regardless of completion order.
fn probe_all(nameservers: &[String], domain: &str) -> Vec<ProbeRow> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for (index, nameserver) in nameservers.iter().enumerate() {
            let sender = sender.clone();
            scope.spawn(move || {
                // Audit #90: a fresh CSPRNG transaction id per probe keeps
                // blind off-path forgery out of reach; entropy failure
                // fails closed into an Error outcome.
                let report = match caly_platform::entropy::try_random_bytes::<2>() {
                    Ok(transaction_id) => probe_nameserver_timed(
                        nameserver,
                        domain,
                        DNS_PROBE_TIMEOUT,
                        transaction_id,
                    ),
                    Err(_) => DnsProbeReport {
                        outcome: DnsProbeOutcome::Error,
                        latency: std::time::Duration::ZERO,
                    },
                };
                let _ = sender.send((index, report));
            });
        }
        drop(sender);
        let mut reports: Vec<Option<DnsProbeReport>> = vec![None; nameservers.len()];
        for (index, report) in receiver {
            reports[index] = Some(report);
        }
        nameservers
            .iter()
            .enumerate()
            .map(|(index, nameserver)| row_from(nameserver.clone(), reports[index]))
            .collect()
    })
}

/// Maps the raw report onto an output row; a missing report means the probe
/// thread itself failed (never expected — surfaced, never silently dropped).
fn row_from(nameserver: String, report: Option<DnsProbeReport>) -> ProbeRow {
    let Some(report) = report else {
        return ProbeRow {
            nameserver,
            status: "error",
            detail: Some("probe thread failed to report".to_owned()),
            latency_ms: 0,
        };
    };
    let latency_ms = u64::try_from(report.latency.as_millis()).unwrap_or(u64::MAX);
    let (status, detail) = match &report.outcome {
        DnsProbeOutcome::Resolved(Some(address)) => ("resolved", Some(address.to_string())),
        DnsProbeOutcome::Resolved(None) => ("empty-answer", None),
        DnsProbeOutcome::Timeout => ("timeout", None),
        // F1: TLS/HTTPS/QUIC and `local` are labeled, never probed-by-mangling.
        DnsProbeOutcome::UnsupportedTransport => (
            "unsupported",
            Some("transport not probeable in this build (udp/tcp only)".to_owned()),
        ),
        other => ("error", Some(format!("{other:?}"))),
    };
    ProbeRow {
        nameserver,
        status,
        detail,
        latency_ms,
    }
}

fn print_human(rows: &[ProbeRow]) {
    for row in rows {
        match row.status {
            "resolved" => println!(
                "  {:<24} {:>5} ms  {}",
                row.nameserver,
                row.latency_ms,
                row.detail.as_deref().unwrap_or("")
            ),
            "empty-answer" => println!(
                "  {:<24} {:>5} ms  no A records (empty answer)",
                row.nameserver, row.latency_ms
            ),
            other => println!(
                "  {:<24} {:>5} ms  {}",
                row.nameserver,
                row.latency_ms,
                row.detail.as_deref().unwrap_or(other)
            ),
        }
    }
}

fn print_json(domain: &str, rows: &[ProbeRow], any_resolved: bool, truncated: bool) {
    let results: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let mut value = serde_json::json!({
                "nameserver": row.nameserver,
                "status": row.status,
                "latency_ms": row.latency_ms,
            });
            if let Some(detail) = &row.detail {
                value["detail"] = serde_json::Value::String(detail.clone());
            }
            value
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "ok": any_resolved,
            "domain": domain,
            "results": results,
            "resolved": any_resolved,
            "truncated": truncated,
        })
    );
}

/// Bounds the probed set so a pathological env/config list cannot fan out
/// unbounded threads.
fn cap_nameservers(nameservers: Vec<String>) -> (Vec<String>, bool) {
    if nameservers.len() <= MAX_PROBED_NAMESERVERS {
        return (nameservers, false);
    }
    let mut capped = nameservers;
    capped.truncate(MAX_PROBED_NAMESERVERS);
    (capped, true)
}

/// Resolves the nameservers to probe: `CALY_DNS_NAMESERVERS` first, then the
/// `dns` section of the layered config, then built-in public defaults.
fn configured_nameservers() -> Vec<String> {
    nameservers_from(|| std::env::var("CALY_DNS_NAMESERVERS").ok())
}

fn nameservers_from(get: impl Fn() -> Option<String>) -> Vec<String> {
    let parsed = get().map_or_else(Vec::new, parse_nameserver_list);
    if !parsed.is_empty() {
        return parsed;
    }
    let configured = crate::daemon_config::dns_nameservers();
    if !configured.is_empty() {
        return configured;
    }
    DEFAULT_NAMESERVERS
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
}

fn parse_nameserver_list(value: String) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nameservers_parse_comma_list() {
        assert_eq!(
            parse_nameserver_list("1.1.1.1, 9.9.9.9".to_owned()),
            vec!["1.1.1.1", "9.9.9.9"]
        );
        assert!(parse_nameserver_list("   ".to_owned()).is_empty());
    }

    #[test]
    fn defaults_are_public_resolvers() {
        assert_eq!(DEFAULT_NAMESERVERS[0], "8.8.8.8");
    }

    #[test]
    fn overlong_server_lists_are_capped_with_a_flag() {
        let nameservers: Vec<String> = (0..=MAX_PROBED_NAMESERVERS)
            .map(|index| format!("10.0.{index}.1"))
            .collect();
        let (capped, truncated) = cap_nameservers(nameservers);
        assert!(truncated);
        assert_eq!(capped.len(), MAX_PROBED_NAMESERVERS);
        let (untouched, not_truncated) = cap_nameservers(vec!["1.1.1.1".to_owned()]);
        assert!(!not_truncated);
        assert_eq!(untouched.len(), 1);
    }

    #[test]
    fn row_from_marks_missing_report_as_error() {
        let row = row_from("1.1.1.1".to_owned(), None);
        assert_eq!(row.status, "error");
        assert!(row.detail.is_some());
    }
}
