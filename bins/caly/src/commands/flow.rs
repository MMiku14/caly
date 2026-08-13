//! `caly flow` — the live traffic processing flow.
//!
//! Every active connection is rendered with the routing metadata the kernel
//! reports for it: which rule matched, which outbound chain it is dialed
//! through. This is the data-plane half of the pipeline observability work
//! (2026-08-12 pipeline design): the CLI polls the kernel's
//! Clash-compatible `/connections` directly, the daemon stays untouched,
//! and the renderer reuses the narrow-terminal table logic.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use caly_corectl::contract::ConnectionDetail;

use crate::cli::CliOptions;
use crate::client::query::{QUERY_TIMEOUT, build_control};
use crate::output::{CliOutput, table_mode};

/// Watch-mode redraw interval.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// Consecutive query failures tolerated in watch mode before giving up
/// (transient kernel restarts must not kill `caly flow --watch`).
const WATCH_MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// Single-shot mode: `--watch` repeats the query forever.
fn is_watch(watch: bool) -> bool {
    watch
}

/// Resolves the controller target exactly like the read-only query path:
/// `--core` flag → `CALY_CORE` → daemon snapshot label → `mihomo`.
fn resolve_core(options: &CliOptions) -> String {
    options.core.clone().unwrap_or_else(|| {
        std::env::var("CALY_CORE").unwrap_or_else(|_| {
            crate::client::active_core_kind_label().unwrap_or_else(|| "mihomo".to_owned())
        })
    })
}

/// Runs one flow snapshot and renders it (or the whole watch loop).
pub fn run(options: CliOptions, output: CliOutput, watch: bool) -> ExitCode {
    let core = resolve_core(&options);
    let json = output.is_json();
    let mut consecutive_failures = 0u32;
    loop {
        let mut control = match build_control(&core) {
            Ok(control) => control,
            Err(error) => {
                return crate::client::output::report_failure(
                    &format!("flow query failed: {error}"),
                    json,
                );
            }
        };
        match control.connection_details(QUERY_TIMEOUT) {
            Ok(details) => {
                consecutive_failures = 0;
                if json {
                    output.success("flow", serde_json::json!({ "connections": details }));
                    return ExitCode::SUCCESS;
                }
                render_flow_table(&details, output);
            }
            Err(error) => {
                if !is_watch(watch) {
                    return crate::client::output::report_failure(
                        &format!("flow query failed: {error}"),
                        json,
                    );
                }
                // Watch mode survives transient kernel outages: keep polling
                // and only give up after a sustained outage (2026-08-12
                // agent audit).
                consecutive_failures += 1;
                if consecutive_failures >= WATCH_MAX_CONSECUTIVE_FAILURES {
                    return crate::client::output::report_failure(
                        &format!("flow query keeps failing: {error}"),
                        json,
                    );
                }
            }
        }
        if !is_watch(watch) {
            return ExitCode::SUCCESS;
        }
        // TTY watch mode clears between frames; piped output prints each
        // snapshot sequentially so it stays greppable.
        if std::io::stdout().is_terminal() {
            print!("\x1b[2J\x1b[H");
        }
        std::thread::sleep(WATCH_INTERVAL);
    }
}

/// Compact human byte formatting (e.g. `1.2 MB`, `340 KB`), mirroring the
/// display style of the rest of the CLI. Display-only: values beyond the
/// f64 mantissa (≈9 PB) render approximately, which is irrelevant for a
/// live connection table.
#[allow(clippy::cast_precision_loss)]
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Renders the connection table with the narrow-terminal contraction used
/// across the CLI (`render_table`). Columns: host:port, net/proto, rule,
/// outbound chain, up/down bytes.
fn render_flow_table(details: &[ConnectionDetail], output: CliOutput) {
    if details.is_empty() {
        output.info("no active connections");
        return;
    }
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(details.len());
    for connection in details {
        let target = if connection.host.is_empty() {
            "-".to_owned()
        } else {
            format!("{}:{}", connection.host, connection.destination_port)
        };
        let protocol = if connection.protocol_type.is_empty() {
            connection.network.clone()
        } else {
            format!("{}/{}", connection.network, connection.protocol_type)
        };
        let rule = if connection.rule.is_empty() {
            "-".to_owned()
        } else if connection.rule_payload.is_empty() {
            connection.rule.clone()
        } else {
            format!("{},{}", connection.rule, connection.rule_payload)
        };
        let chain = if connection.chain.is_empty() {
            if connection.outbound.is_empty() {
                "-".to_owned()
            } else {
                connection.outbound.clone()
            }
        } else {
            connection.chain.join(" → ")
        };
        rows.push(vec![
            target,
            protocol,
            rule,
            chain,
            human_bytes(connection.upload_bytes),
            human_bytes(connection.download_bytes),
        ]);
    }
    let headers: [&str; 6] = ["HOST:PORT", "NET/PROTO", "RULE", "OUTBOUND", "UP", "DOWN"];
    let lines = crate::output::render_table(table_mode(None), &headers, &rows, None);
    for line in lines {
        println!("{line}");
    }
}

/// Resolves one connection by exact id, then by `host:port` (host matched
/// case-insensitively), and renders its full processing chain.
pub fn trace(options: CliOptions, output: CliOutput, selector: &str, watch: bool) -> ExitCode {
    if watch {
        // A trace is a single snapshot; `--watch` has nothing to redraw
        // (2026-08-12 flow agent audit: the flag used to be dropped
        // silently).
        eprintln!("note: `--watch` does not apply to `flow trace`; showing one snapshot");
    }
    let core = resolve_core(&options);
    let json = output.is_json();
    let mut control = match build_control(&core) {
        Ok(control) => control,
        Err(error) => {
            return crate::client::output::report_failure(
                &format!("flow trace failed: {error}"),
                json,
            );
        }
    };
    let details = match control.connection_details(QUERY_TIMEOUT) {
        Ok(details) => details,
        Err(error) => {
            return crate::client::output::report_failure(
                &format!("flow trace failed: {error}"),
                json,
            );
        }
    };
    let found = details
        .iter()
        .find(|connection| connection.id == selector)
        .or_else(|| {
            // `host:port` selector: host is case-insensitive, port must
            // match exactly; the first match wins (flows to the same
            // target are rare and the chain is usually identical).
            let (host, port) = split_host_port(selector)?;
            details.iter().find(|connection| {
                connection.host.eq_ignore_ascii_case(host) && connection.destination_port == port
            })
        });
    let Some(connection) = found else {
        return crate::client::output::report_failure(
            &format!("no active connection matches `{selector}`"),
            json,
        );
    };
    if json {
        output.success("flow", serde_json::json!(connection));
        return ExitCode::SUCCESS;
    }
    render_trace(connection);
    ExitCode::SUCCESS
}

/// Splits a `host:port` selector; the port must parse as a u16.
fn split_host_port(selector: &str) -> Option<(&str, u16)> {
    let (host, port) = selector.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    // Port 0 is not a real endpoint; treat it as an invalid selector
    // (2026-08-12 boundary audit).
    if port == 0 {
        return None;
    }
    Some((host, port))
}

/// Renders one connection's processing chain as labeled lines:
/// inbound → sniff → dns → rule → group → node, plus traffic totals.
fn render_trace(connection: &ConnectionDetail) {
    println!("connection: {}", connection.id);
    if !connection.inbound_name.is_empty() {
        println!("inbound:    {}", connection.inbound_name);
    }
    if !connection.process_path.is_empty() {
        println!("process:    {}", connection.process_path);
    }
    let sniff = if connection.protocol_type.is_empty() {
        connection.network.clone()
    } else {
        format!("{}/{}", connection.network, connection.protocol_type)
    };
    println!("sniff:      {sniff}");
    println!(
        "target:     {}:{}",
        connection.host, connection.destination_port
    );
    let rule = if connection.rule.is_empty() {
        "-".to_owned()
    } else if connection.rule_payload.is_empty() {
        connection.rule.clone()
    } else {
        format!("{},{}", connection.rule, connection.rule_payload)
    };
    println!("rule:       {rule}");
    let chain = if connection.chain.is_empty() {
        if connection.outbound.is_empty() {
            "-".to_owned()
        } else {
            connection.outbound.clone()
        }
    } else {
        connection.chain.join(" → ")
    };
    println!("chain:      {chain}");
    println!(
        "traffic:    ↑{} ↓{}",
        human_bytes(connection.upload_bytes),
        human_bytes(connection.download_bytes)
    );
}

#[cfg(test)]
mod tests {
    use super::{human_bytes, split_host_port};

    #[test]
    fn splits_host_port_selectors() {
        assert_eq!(
            split_host_port("example.com:443"),
            Some(("example.com", 443))
        );
        assert_eq!(split_host_port("1.2.3.4:80"), Some(("1.2.3.4", 80)));
        // rsplit takes the LAST colon, so a bare IPv6 host parses too.
        assert_eq!(split_host_port("::1:443"), Some(("::1", 443)));
        // Missing port / non-numeric port: not a valid selector.
        assert_eq!(split_host_port("example.com"), None);
        assert_eq!(split_host_port("example.com:notaport"), None);
    }

    #[test]
    fn formats_human_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1_048_576), "1.0 MB");
        assert_eq!(human_bytes(5_242_880), "5.0 MB");
        assert_eq!(human_bytes(1_073_741_824), "1.0 GB");
    }
}
