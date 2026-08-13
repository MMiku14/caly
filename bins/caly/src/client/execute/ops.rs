//! Lifecycle and mutation operations for the daemon execute path.
//!
//! Split out of `client/execute/mod.rs` (audit #70 file-length
//! budget): each helper assigns a fresh operation id, submits the
//! built [`WireCommand`], and either reports the immediate
//! terminal status or polls to a terminal state. Human output
//! reports a semantic summary; JSON output keeps the raw
//! operation record for scripts.

use std::process::ExitCode;

use caly_protocol::{
    client::{ClientContract, UdsClient},
    protocol::v2::{
        ExecuteRequest, GetOperationStatusRequest, RawOperationState, WireCommand, WireCoreAction,
        WireCoreKind, WireMode,
    },
};

use super::super::{hex, operation_id, output};
use super::lowest_latency_node;

/// Executes a Round-17 lifecycle command whose operation completes on the
/// application side in the same `execute` call: the server returns the
/// final `WireOperationStatus` inside the `ExecuteResponse`, so the client
/// must NOT poll (the `StopDaemon` path closes the UDS socket right after
/// the response, and polling for a stale id would surface a confusing
/// `connect_failed` error). If the response is somehow non-terminal
/// (defensive), fall through to a bounded poll.
pub(super) fn execute_lifecycle_immediate(
    client: &mut UdsClient,
    json: bool,
    summary: &str,
    build: impl FnOnce([u8; 16]) -> WireCommand,
) -> ExitCode {
    let id = operation_id();
    match client.execute(ExecuteRequest {
        operation_id: id,
        command: build(id),
    }) {
        Ok(response) => {
            let status = response.operation;
            if status.state.is_terminal() {
                output::print_operation_status(&status, json, summary);
                terminal_exit_code(status.state)
            } else {
                // Defensive: the typed path is supposed to be terminal here,
                // but the server may be running an older build that returns a
                // non-terminal status. Poll for a bounded window so the
                // caller still gets a definitive answer.
                poll_operation(client, id, json, summary)
            }
        }
        Err(error) => output::report_error(error, json),
    }
}

/// The single mutation path: assign an operation id, execute the command the
/// caller builds from it, then poll to a terminal state.
pub(super) fn execute_operation_with(
    client: &mut UdsClient,
    json: bool,
    summary: &str,
    build: impl FnOnce([u8; 16]) -> WireCommand,
) -> ExitCode {
    let id = operation_id();
    match client.execute(ExecuteRequest {
        operation_id: id,
        command: build(id),
    }) {
        Ok(_) => poll_operation(client, id, json, summary),
        Err(error) => output::report_error(error, json),
    }
}

/// Executes a `SetMode` operation against the daemon.
pub(super) fn execute_set_mode(client: &mut UdsClient, mode: &str, json: bool) -> ExitCode {
    let Some(mode_value) = WireMode::from_label(mode) else {
        return output::report_usage_error("mode must be rule, global, or direct", json);
    };
    execute_operation_with(client, json, &format!("set mode to {mode}"), |_| {
        WireCommand::SetMode {
            mode: mode_value.wire(),
        }
    })
}

/// Switches the active core to `target` (stop previous kernel, start target).
pub(super) fn execute_switch_core(client: &mut UdsClient, target: &str, json: bool) -> ExitCode {
    let kind = match target {
        "sing-box" => WireCoreKind::SingBox,
        "mihomo" => WireCoreKind::Mihomo,
        other => {
            return output::report_usage_error(
                &format!("unknown core `{other}`; use mihomo or sing-box"),
                json,
            );
        }
    };
    execute_operation_with(
        client,
        json,
        &format!("switch core to {}", kind.label()),
        |_| WireCommand::SwitchCore {
            core_kind: kind.wire(),
            action: WireCoreAction::Restart.wire(),
        },
    )
}

/// Executes a core lifecycle action (start/stop/restart) for the resolved core.
///
/// Without an explicit `--core`/`CALY_CORE`, the target follows the daemon's
/// runtime active core — so `caly core restart` restarts the kernel the user
/// actually switched to instead of silently switching to (or stopping it for)
/// a hardcoded mihomo.
pub(super) fn execute_core(
    client: &mut UdsClient,
    action: WireCoreAction,
    core: Option<&str>,
    json: bool,
) -> ExitCode {
    let effective = match core {
        Some(core) => Some(core.to_owned()),
        None => active_core_kind(client).map(|kind| kind.label().to_owned()),
    };
    let kind = match resolved_core_kind(effective.as_deref()) {
        Ok(kind) => kind,
        Err(message) => {
            if json {
                println!("{}", serde_json::json!({ "ok": false, "error": message }));
            } else {
                eprintln!("error: {message}");
            }
            return ExitCode::from(2);
        }
    };
    let label = kind.label();
    let verb = match action {
        WireCoreAction::Start => "start",
        WireCoreAction::Stop => "stop",
        WireCoreAction::Restart => "restart",
    };
    execute_operation_with(client, json, &format!("{verb} {label}"), |_| {
        WireCommand::SwitchCore {
            core_kind: kind.wire(),
            action: action.wire(),
        }
    })
}

/// Reads the daemon snapshot for the runtime active core kind.
pub(super) fn active_core_kind(client: &mut UdsClient) -> Option<WireCoreKind> {
    use caly_protocol::client::ClientContract;
    let snapshot = client.snapshot().ok()?;
    WireCoreKind::from_wire(snapshot.applied.core_kind?)
}

/// Resolves the target core: `--core` flag first, then `CALY_CORE`, default
/// mihomo. Unknown values fail loudly instead of silently falling back to
/// mihomo (a typo like `--core mihomoo` must not silently act on the wrong
/// kernel).
pub(super) fn resolved_core_kind(core: Option<&str>) -> Result<WireCoreKind, String> {
    let effective = core
        .map(str::to_owned)
        .or_else(|| std::env::var("CALY_CORE").ok());
    match effective.as_deref() {
        None | Some("mihomo") => Ok(WireCoreKind::Mihomo),
        Some("sing-box") => Ok(WireCoreKind::SingBox),
        Some("xray") => Ok(WireCoreKind::Xray),
        Some(other) => Err(format!(
            "unknown core target `{other}` (expected mihomo, sing-box or xray)"
        )),
    }
}

/// Selects a proxy node: by ID/name/prefix, or automatically the lowest
/// latency node (never direct) with `--delay`.
pub(super) fn execute_select_proxy(
    client: &mut UdsClient,
    node: Option<&str>,
    delay: bool,
    poll: bool,
    json: bool,
) -> ExitCode {
    if delay {
        let Some(best) = lowest_latency_node(client, json) else {
            return ExitCode::from(2);
        };
        let label = best.1;
        return execute_operation_with(client, json, &format!("select node `{label}`"), |_| {
            WireCommand::SelectProxy { node_id: best.0 }
        });
    }
    let Some(node) = node else {
        // W2 (cli-v3-design.md §7): no argument on a live terminal
        // opens the interactive picker; piped/non-TTY gets the
        // usage error with the available entries, exit 2 (C-A).
        return select_without_argument(client, poll, json);
    };
    let Some((node_id, label)) = resolve_node(client, node, json) else {
        return ExitCode::from(2);
    };
    execute_operation_with(client, json, &format!("select node `{label}`"), |_| {
        WireCommand::SelectProxy { node_id }
    })
}

/// The `caly node select` no-argument face (§7). The picker lists
/// the online snapshot (`[protocol] name latency`); non-TTY never
/// blocks and reports the available names instead.
fn select_without_argument(client: &mut UdsClient, poll: bool, json: bool) -> ExitCode {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    let snapshot = match client.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = output::report_error(error, json);
            return ExitCode::FAILURE;
        }
    };
    let names: Vec<String> = snapshot
        .nodes
        .iter()
        .map(|node| crate::client::output::decode_display_name(&node.name))
        .collect();
    // The empty pool is reported before the gate: with nothing to
    // offer, even a script should hear "refresh a subscription",
    // not "give me a name from this empty list".
    if names.is_empty() {
        return output::report_usage_error(
            "no entries to select; refresh a subscription with `caly sub refresh` first",
            json,
        );
    }
    if !crate::client::interact::interactive_capable() {
        return output::report_usage_error(
            &format!(
                "node select requires <name> in non-interactive mode. Available: {}",
                names.join(", ")
            ),
            json,
        );
    }
    let entries: Vec<crate::client::interact_live::LiveItem> = snapshot
        .nodes
        .iter()
        .map(|node| crate::client::interact_live::LiveItem {
            name: crate::client::output::decode_display_name(&node.name),
            protocol: node.protocol.clone(),
            latency_ms: node.latency_ms,
            payload: crate::client::hex(node.node_id),
        })
        .collect();
    let latencies = Arc::new(Mutex::new(HashMap::new()));
    let ping = Arc::new(Mutex::new(
        crate::client::interact_live::PingState::default(),
    ));
    // `-p` pre-freshens latencies with a full sweep before the picker
    // opens — the same sweep the in-picker feature row re-runs, sharing
    // the `latencies` map so the picker shows live pinged delays.
    if poll {
        pre_sweep_latencies(client, &latencies);
    }
    let mut start_ping = build_start_ping(
        latencies.clone(),
        ping.clone(),
        caly_platform::paths::AppPaths::from_env().socket_path(),
    );
    let term = console::Term::stderr();
    match crate::client::interact_live::pick_live(
        &term,
        &entries,
        &ping,
        &latencies,
        &mut start_ping,
    ) {
        Ok(crate::client::interact_live::LiveOutcome::Selected(index)) => {
            let Some(entry) = entries.get(index) else {
                return output::report_usage_error(
                    "internal picker index out of range; nothing changed",
                    json,
                );
            };
            let Some(node_id) = parse_node_id(&entry.payload) else {
                return output::report_usage_error(
                    "internal picker payload is not a node id; nothing changed",
                    json,
                );
            };
            let label = &entry.name;
            execute_operation_with(client, json, &format!("select node `{label}`"), |_| {
                WireCommand::SelectProxy { node_id }
            })
        }
        Ok(crate::client::interact_live::LiveOutcome::Escaped) => {
            eprintln!("\n{}", crate::output::CANCELLED_NOTHING_CHANGED);
            ExitCode::FAILURE
        }
        Ok(crate::client::interact_live::LiveOutcome::Interrupted) => ExitCode::from(130),
        Err(_) => {
            eprintln!("picker failed; nothing changed");
            ExitCode::FAILURE
        }
    }
}

/// `-p` pre-sweep: fills the shared `latencies` map with a full delay
/// sweep before the picker opens — the same sweep the in-picker feature
/// row re-runs, so the picker opens with live pinged delays.
fn pre_sweep_latencies(
    client: &mut UdsClient,
    latencies: &std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Option<u32>>>>,
) {
    match super::delay::sweep_snapshot_delays_with(
        client,
        None,
        None,
        Some(&**latencies),
        Some(&|done, total, dead| {
            eprint!("\rprobed {done}/{total} · {dead} unreachable");
        }),
    ) {
        Ok(_) => eprint!("\r\x1b[K"),
        Err(_) => eprintln!("note: delay sweep failed; showing cached latencies"),
    }
}

/// In-picker ping launcher: Enter on the feature row spawns a background
/// sweep that publishes per-node medians into `latencies` and progress
/// into `ping`; the picker's poll-timeout redraw loop picks both up.
fn build_start_ping(
    latencies: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Option<u32>>>>,
    ping: std::sync::Arc<std::sync::Mutex<crate::client::interact_live::PingState>>,
    socket: std::path::PathBuf,
) -> impl FnMut() {
    let (lat2, ping2) = (latencies, ping);
    move || {
        let lat = lat2.clone();
        let pg = ping2.clone();
        let path = socket.clone();
        std::thread::spawn(move || {
            *pg.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                crate::client::interact_live::PingState {
                    running: true,
                    done: 0,
                    total: 0,
                    dead: 0,
                    ever_ran: true,
                    failed: false,
                };
            let Ok(mut probe) = caly_protocol::client::UdsClient::connect(path) else {
                pg.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .running = false;
                return;
            };
            let sweep = super::delay::sweep_snapshot_delays_with(
                &mut probe,
                None,
                None,
                Some(&lat),
                Some(&|done, total, dead| {
                    *pg.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                        crate::client::interact_live::PingState {
                            running: true,
                            done,
                            total,
                            dead,
                            ever_ran: true,
                            failed: false,
                        };
                }),
            );
            let mut state = pg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = false;
            // A failed sweep must surface as failure in the feature row,
            // not as a fake `0 unreachable` result.
            if sweep.is_err() {
                state.failed = true;
            }
        });
    }
}

/// Resolves a `select` argument: a 32-hex node ID is used as-is; anything
/// else is matched against the snapshot's node display names (exact match
/// first, then a unique case-insensitive match).
fn resolve_node(client: &mut UdsClient, node: &str, json: bool) -> Option<([u8; 16], String)> {
    if let Some(id) = parse_node_id(node) {
        return Some((id, node.to_owned()));
    }
    let snapshot = match client.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = output::report_error(error, json);
            return None;
        }
    };
    let exact: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.name == node || output::decode_display_name(&candidate.name) == node
        })
        .collect();
    if exact.len() == 1 {
        return Some((exact[0].node_id, exact[0].name.clone()));
    }
    if exact.len() > 1 {
        let ids = exact
            .iter()
            .map(|candidate| format!("  {} ({})", hex(candidate.node_id), candidate.protocol))
            .collect::<Vec<_>>()
            .join("\n");
        output::report_usage_error(
            &format!(
                "{} nodes share the name `{node}`; select one by ID:\n{ids}",
                exact.len()
            ),
            json,
        );
        return None;
    }
    let fuzzy: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|candidate| {
            // Compare the *decoded* display name everywhere: a
            // percent-encoded tag (`HK%2D01` in the registry, `HK-01`
            // on screen) must match the operator's typing (2026-08-12
            // agent audit — the exact branch decoded, the fuzzy/prefix
            // branches did not).
            candidate.name.eq_ignore_ascii_case(node)
                || output::decode_display_name(&candidate.name).eq_ignore_ascii_case(node)
        })
        .collect();
    if fuzzy.len() == 1 {
        return Some((fuzzy[0].node_id, fuzzy[0].name.clone()));
    }
    // Unique case-insensitive prefix match: select by a short unambiguous
    // fragment of the display name (e.g. the country flag or keyword).
    let prefix: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|candidate| {
            candidate
                .name
                .to_lowercase()
                .starts_with(&node.to_lowercase())
                || output::decode_display_name(&candidate.name)
                    .to_lowercase()
                    .starts_with(&node.to_lowercase())
        })
        .collect();
    if prefix.len() == 1 {
        return Some((prefix[0].node_id, prefix[0].name.clone()));
    }
    if prefix.len() > 1 {
        let ids = prefix
            .iter()
            .take(8)
            .map(|candidate| format!("  {} ({})", hex(candidate.node_id), candidate.name))
            .collect::<Vec<_>>()
            .join("\n");
        output::report_usage_error(
            &format!(
                "`{node}` matches {} nodes; use a longer prefix or an ID:\n{ids}",
                prefix.len()
            ),
            json,
        );
        return None;
    }
    output::report_usage_error(
        &format!("no node named `{node}`; list nodes with `caly core list-nodes`"),
        json,
    );
    None
}

/// Parses a 32-hex-character node id into its 16-byte wire form.
pub(crate) fn parse_node_id(hex_value: &str) -> Option<[u8; 16]> {
    if hex_value.len() != 32 {
        return None;
    }
    let mut id = [0_u8; 16];
    for (index, pair) in hex_value.as_bytes().chunks(2).enumerate() {
        let hi = u8::try_from((pair[0] as char).to_digit(16)?).ok()?;
        let lo = u8::try_from((pair[1] as char).to_digit(16)?).ok()?;
        id[index] = (hi << 4) | lo;
    }
    Some(id)
}

/// Polls an operation until it reaches a terminal state (or a bounded timeout).
/// `summary` describes the requested change for the human-readable result line.
fn poll_operation(client: &mut UdsClient, id: [u8; 16], json: bool, summary: &str) -> ExitCode {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match client.operation_status(GetOperationStatusRequest { operation_id: id }) {
            Ok(status) => {
                if status.state.is_terminal() {
                    output::print_operation_status(&status, json, summary);
                    return terminal_exit_code(status.state);
                }
                if std::time::Instant::now() >= deadline {
                    output::print_operation_status(&status, json, summary);
                    return ExitCode::from(1); // TimedOutStillRunning (C-A: 1)
                }
            }
            Err(error) => return output::report_error(error, json),
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Maps a terminal operation state to the CLI exit-code contract.
fn terminal_exit_code(state: RawOperationState) -> ExitCode {
    use caly_protocol::protocol::v2::WireOperationState;
    // C-A contract (cli-v3-design.md appendix B): 0 = success, 1 = any
    // execution failure (including operator cancellation); 3 is
    // reserved for doctor/dns probe failures. The v3.1 extension
    // codes 6/7 were never adopted (W3a review, BUG-3).
    match state.known() {
        Some(WireOperationState::Completed) => ExitCode::SUCCESS,
        Some(WireOperationState::Failed) | Some(WireOperationState::Cancelled) => ExitCode::from(1),
        _ => ExitCode::from(1), // still running / unknown
    }
}
