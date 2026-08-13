//! Human and JSON presentation of daemon snapshots and operation status.
//!
//! All JSON is produced through `serde_json` (never hand-assembled strings),
//! and all wire discriminants render through the protocol's typed wire layer.

use std::process::ExitCode;

/// Whether stdout is a real terminal (colored tabular output only then; piped
/// output stays plain and script-friendly). Shared by the online node table,
/// the offline group table and `sub list` rows.
pub(crate) fn ansi_enabled() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Wraps `text` in an ANSI color class by latency when stdout is a
/// terminal. W2 (cli-v3-design.md §5.2): the palette is >300 ms
/// yellow / >1000 ms red, no green band — the v1 200/500
/// green/yellow thresholds shipped before the spec was pinned.
fn latency_color(ms: Option<u32>, text: &str) -> String {
    if !ansi_enabled() {
        return text.to_owned();
    }
    match ms {
        Some(value) if value > 1000 => crate::output::paint("31", text),
        Some(value) if value > 300 => crate::output::paint("33", text),
        _ => text.to_owned(),
    }
}

/// §5.2 status badge: green ● on the selected path leaf, grey ○
/// otherwise. ◆ (group rows) arrives with the W3b corectl
/// enrichment — the wire projection carries no group membership.
fn status_badge(selected: bool) -> String {
    if !ansi_enabled() {
        return if selected {
            "●".to_owned()
        } else {
            "○".to_owned()
        };
    }
    if selected {
        crate::output::paint("32", "●")
    } else {
        crate::output::paint("90", "○")
    }
}

use caly_protocol::{
    client::{ClientContract, ClientError, UdsClient},
    protocol::v2::{
        WireCoreKind, WireMode, WireOperationState, WireOperationStatus, WirePresentationSnapshot,
        WireRunState,
    },
};
use serde_json::json;

use super::hex;
use super::output_capabilities::print_capabilities;

fn mode_label(value: i32) -> String {
    WireMode::from_wire(value).map_or_else(
        || format!("unknown({value})"),
        |mode| mode.label().to_owned(),
    )
}

fn core_label(value: i32) -> String {
    WireCoreKind::from_wire(value).map_or_else(
        || format!("unknown({value})"),
        |core| core.label().to_owned(),
    )
}

fn run_state_label(value: i32) -> String {
    WireRunState::from_wire(value).map_or_else(
        || format!("unknown({value})"),
        |state| state.label().to_owned(),
    )
}

/// Renders `caly status` as human text or a single JSON line.
pub(super) fn print_status(client: &mut UdsClient, json: bool) -> ExitCode {
    let output = crate::output::CliOutput::from_json_flag(json);
    match client.snapshot() {
        Ok(snapshot) => {
            render_status_snapshot(&snapshot, json, None);
            ExitCode::SUCCESS
        }
        Err(error) => crate::output::report_error_returning(output, client_error(error, "status")),
    }
}

/// W2/R5: the rendering half of [`print_status`], extracted so
/// `commands::status` can race its probe against a wall-clock
/// budget and still emit byte-identical output on success.
///
/// `kernel_selected` optionally carries the running kernel's live
/// selection display name (proxy-group `now`), which wins over the
/// daemon's last-applied record — `status` must reflect what the
/// kernel actually routes through (2026-08-12 status audit).
pub(crate) fn render_status_snapshot(
    snapshot: &WirePresentationSnapshot,
    json: bool,
    kernel_selected: Option<&str>,
) {
    if json {
        // The status snapshot is a deeply nested object, not
        // the standard `{ok, version, ...}` envelope, so we
        // print the raw snapshot rather than wrapping it.
        println!("{}", status_json(snapshot));
        return;
    }
    println!("daemon:      {}", hex(snapshot.daemon_instance_id));
    println!("revision:    {}", snapshot.revision);
    println!("sequence:    {}", snapshot.cursor.sequence);
    println!();
    print_desired(snapshot);
    print_applied(snapshot, kernel_selected);
    print_observed(snapshot);
    print_platform(snapshot);
    println!();
    println!("nodes: {}", snapshot.nodes.len());
    print_capabilities(snapshot);
}

/// Converts a `ClientError` into the new `CliError` envelope
/// (stable `code` + `hint` + `command`).
pub(super) fn client_error(error: ClientError, command: &str) -> crate::output::CliError {
    use crate::error::core;
    match error {
        ClientError::TransportUnavailable => core::connect_failed("", command),
        ClientError::DeadlineExceeded => {
            core::operation_failed("the daemon did not respond before the deadline", command)
                .with_hint("check `caly status` and daemon logs, then retry")
        }
        ClientError::IncompatibleVersion => core::handshake_failed(
            "the client and daemon protocol versions are incompatible",
            command,
        )
        .with_hint("upgrade or restart caly so client and daemon use the same version"),
        ClientError::AuthenticationRejected => {
            core::handshake_failed("the daemon rejected this client session", command)
                .with_hint("restart the daemon and ensure the socket belongs to the same user")
        }
        ClientError::ResourceExhausted => {
            core::operation_failed("the daemon is busy and its request queue is full", command)
                .with_hint("wait briefly and retry; do not start multiple competing clients")
        }
        ClientError::DecodeRejected {
            reason,
            suggested_action,
        } => core::handshake_failed(&reason, command).with_hint(suggested_action),
    }
}

/// Renders a terminal operation status as human text or JSON. `summary`
/// describes the requested change so a successful mutation reports what
/// happened instead of only an opaque operation id.
pub(super) fn print_operation_status(status: &WireOperationStatus, json: bool, summary: &str) {
    if json {
        let failure = status.failure.as_ref().map(|failure| {
            json!({
                "code": failure.code.0,
                "message": failure.message,
                "hint": failure.suggested_action,
            })
        });
        // `ok` is the unified boolean a script-friendly JSON envelope
        // expects (matches the `report_*_returning` envelopes and the
        // `set daemon stop` / `set daemon restart` e2e contract). The
        // `state` integer is still present for legacy readers.
        let ok = matches!(status.state.known(), Some(WireOperationState::Completed));
        println!(
            "{}",
            json!({
                "ok": ok,
                "operation_id": hex(status.operation_id),
                "state": status.state.0,
                "created_at_ms": status.created_at_unix_ms,
                "updated_at_ms": status.updated_at_unix_ms,
                "failure": failure,
            })
        );
    } else if matches!(status.state.known(), Some(WireOperationState::Completed)) {
        println!("ok: {summary}");
    } else {
        eprintln!(
            "operation {} {}: {summary}",
            hex(status.operation_id),
            status.state.label()
        );
        if let Some(failure) = &status.failure {
            eprintln!(
                "error: {}\nhint: {}",
                failure.message, failure.suggested_action
            );
        }
    }
}

/// Renders `caly node list` (v1 `core list-nodes`) from the daemon
/// snapshot. W2 (cli-v3-design.md §5.1/§5.2, Q6): TTY renders the
/// TYPE/NAME/DELAY/STATUS table with ●○ badges; piped renders TSV
/// (`type, name, delay_ms, selected, available`) with a header row;
/// `--format` pins either. The GROUP column and ◆ group rows land
/// with the W3b corectl enrichment (the wire projection has no
/// group membership yet).
pub(super) fn print_nodes(
    client: &mut UdsClient,
    json: bool,
    format: Option<crate::cli::OutputFormat>,
) -> ExitCode {
    let output = crate::output::CliOutput::from_json_flag(json);
    match client.snapshot() {
        Ok(snapshot) if json => {
            let nodes: Vec<serde_json::Value> = snapshot
                .nodes
                .iter()
                .map(|node| {
                    json!({
                        "id": hex(node.node_id),
                        "name": decode_display_name(&node.name),
                        "protocol": node.protocol,
                        "available": node.available,
                        "latency_ms": node.latency_ms,
                    })
                })
                .collect();
            // W3b: the kernel proxy-group slice rides on the same snapshot.
            let groups: Vec<serde_json::Value> = snapshot
                .proxy_groups
                .iter()
                .map(|group| {
                    json!({
                        "name": group.name,
                        "kind": group.kind,
                        "selected": group.selected,
                        "members": group.members,
                    })
                })
                .collect();
            println!(
                "{}",
                json!({ "ok": true, "count": nodes.len(), "nodes": nodes, "groups": groups })
            );
            ExitCode::SUCCESS
        }
        Ok(snapshot) => {
            let mode = crate::output::table_mode(format);
            // TSV is a script contract: it carries raw values and
            // never the human-only hint line. The table keeps the
            // actionable empty-state hint (§8).
            if snapshot.nodes.is_empty() {
                if mode == crate::output::TableMode::Tsv {
                    crate::output::print_table(
                        mode,
                        &["type", "name", "delay_ms", "selected", "available"],
                        &[],
                    );
                } else {
                    println!("no nodes (refresh a subscription with `caly sub refresh`)");
                }
                return ExitCode::SUCCESS;
            }
            render_nodes_table(&snapshot, mode);
            ExitCode::SUCCESS
        }
        Err(error) => {
            crate::output::report_error_returning(output, client_error(error, "core list-nodes"))
        }
    }
}

/// Renders the node table for a non-empty snapshot: node rows with an
/// optional GROUP column plus the W3b group rows (kind, selection, member
/// count) ahead of them when kernel group data is present. TSV keeps its
/// fixed five-column script contract — never extended.
fn render_nodes_table(snapshot: &WirePresentationSnapshot, mode: crate::output::TableMode) {
    let mut rows = Vec::with_capacity(snapshot.nodes.len());
    // W3b: reverse index node name -> containing groups, so the
    // GROUP column can render kernel-side membership. Absent group
    // data (boot before the first selection refresh) degrades to no
    // GROUP column at all — the table never invents membership.
    let groups_by_node = group_index(snapshot);
    let show_groups = !groups_by_node.is_empty();
    for node in &snapshot.nodes {
        let selected = snapshot.desired.selected_node_id == Some(node.node_id);
        if mode == crate::output::TableMode::Tsv {
            rows.push(vec![
                node.protocol.clone(),
                decode_display_name(&node.name),
                node.latency_ms
                    .map_or_else(String::new, |ms| ms.to_string()),
                selected.to_string(),
                node.available.to_string(),
            ]);
        } else {
            let delay = node.latency_ms.map_or_else(
                || "-".to_owned(),
                |ms| latency_color(node.latency_ms, &format!("{ms} ms")),
            );
            let mut row = vec![
                format!("[{}]", node.protocol),
                decode_display_name(&node.name),
                delay,
                status_badge(selected),
            ];
            if show_groups {
                let groups = groups_by_node
                    .get(decode_display_name(&node.name).as_str())
                    .map_or_else(String::new, |names| names.join(", "));
                row.push(groups);
            }
            rows.push(row);
        }
    }
    match mode {
        crate::output::TableMode::Tsv => crate::output::print_table(
            mode,
            &["type", "name", "delay_ms", "selected", "available"],
            &rows,
        ),
        crate::output::TableMode::Table => {
            if show_groups {
                // Group rows (§5.2): one line per kernel group with
                // kind, selection, and member count, followed by the
                // node rows.
                let mut group_rows: Vec<Vec<String>> = Vec::new();
                for group in &snapshot.proxy_groups {
                    group_rows.push(vec![
                        format!("[{}]", group.kind.to_lowercase()),
                        decode_display_name(&group.name),
                        String::new(),
                        group
                            .selected
                            .as_ref()
                            .map_or_else(String::new, |m| format!("-> {m}")),
                        format!("{} members", group.members.len()),
                    ]);
                }
                let mut all = group_rows;
                all.extend(rows);
                let headers = vec!["TYPE", "NAME", "DELAY", "STATUS", "GROUP"];
                crate::output::print_table(mode, &headers, &all);
            } else {
                crate::output::print_table(mode, &["TYPE", "NAME", "DELAY", "STATUS"], &rows);
            }
        }
    }
}

/// Builds `node name -> [containing group names]` from the snapshot's
/// kernel proxy-group slice (W3b).
fn group_index(
    snapshot: &WirePresentationSnapshot,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut index: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for group in &snapshot.proxy_groups {
        for member in &group.members {
            index
                .entry(member.clone())
                .or_default()
                .push(group.name.clone());
        }
    }
    index
}

/// Reports a usage/validation error (bad arguments, unknown names): exit 2.
pub(crate) fn report_usage_error(message: &str, json: bool) -> ExitCode {
    crate::output::report_error_returning(
        crate::output::CliOutput::from_json_flag(json),
        crate::output::CliError::new(crate::error::usage::INVALID, message, "core (any leaf)"),
    )
}

/// Reports a runtime failure without daemon-specific detail: exit 1.
pub(crate) fn report_failure(message: &str, json: bool) -> ExitCode {
    crate::output::report_error_returning(
        crate::output::CliOutput::from_json_flag(json),
        crate::output::CliError::new(
            crate::error::core::RUNTIME_FAILED,
            message,
            "core (any leaf)",
        ),
    )
}

/// Reports a configuration check failure with a stable code.
pub(super) fn report_config_check_failed(detail: &str, json: bool) -> ExitCode {
    crate::output::report_error_returning(
        crate::output::CliOutput::from_json_flag(json),
        crate::error::config::check_failed(detail, "config check"),
    )
}

fn print_desired(snapshot: &WirePresentationSnapshot) {
    println!("desired:");
    println!("  mode:             {}", mode_label(snapshot.desired.mode));
    println!("  tun:              {}", snapshot.desired.tun_requested);
    println!(
        "  system-proxy:     {}",
        snapshot.desired.system_proxy_requested
    );
    println!();
}

fn print_applied(snapshot: &WirePresentationSnapshot, kernel_selected: Option<&str>) {
    println!("applied:");
    println!(
        "  core:             {}",
        snapshot
            .applied
            .core_kind
            .map_or_else(|| "none".to_owned(), core_label)
    );
    println!(
        "  run-state:        {}",
        run_state_label(snapshot.applied.run_state)
    );
    println!(
        "  config-generation:{}",
        snapshot
            .applied
            .config_generation
            .map_or_else(|| "-".to_owned(), |value| value.to_string())
    );
    // The running kernel's live selection (group `now`) is the authority;
    // the daemon's applied record only fills in when the kernel is offline
    // or reports nothing (2026-08-12 status audit: group-path picks and
    // kernel-side switches left the applied projection stale/None).
    if let Some(name) = kernel_selected {
        println!("  selected-node:    {name}");
    } else if let Some(node_id) = snapshot.applied.selected_node_id {
        let name = snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .map_or_else(
                || "-".to_owned(),
                |node| decode_display_name(node.name.as_str()),
            );
        println!("  selected-node:    {}  {}", hex(node_id), name);
    }
    println!();
}

fn print_observed(snapshot: &WirePresentationSnapshot) {
    println!("observed:");
    println!(
        "  up:               {} B/s",
        snapshot.observed.upload_bytes_per_second
    );
    println!(
        "  down:             {} B/s",
        snapshot.observed.download_bytes_per_second
    );
    println!(
        "  connections:      {}",
        snapshot.observed.active_connections
    );
    println!(
        "  restarts:         {}",
        snapshot.observed.core_restart_count
    );
    println!(
        "  restart-backoff:  {} ms",
        snapshot.observed.core_restart_backoff_ms
    );
    println!();
}

fn print_platform(snapshot: &WirePresentationSnapshot) {
    println!("platform:");
    println!("  proxy-engaged:    {}", snapshot.platform.proxy_engaged);
    println!("  tun-engaged:      {}", snapshot.platform.tun_engaged);
    println!("  recovery-pending: {}", snapshot.platform.recovery_pending);
    println!(
        "  degraded:         {}",
        snapshot.platform.degraded_reason.as_deref().unwrap_or("-")
    );
    println!();
}

/// Serializes the snapshot as one JSON line. Absent optional fields stay
/// `null` so field types are stable for script consumers.
fn status_json(snapshot: &WirePresentationSnapshot) -> String {
    status_value(snapshot).to_string()
}

/// Builds the snapshot as a `serde_json::Value` (inlined from the old
/// `observed_json` / `platform_json` helpers — both helpers had a single
/// caller and a 5/3-field shape, so inlining keeps the JSON tree in one
/// place without a function-call seam).
fn status_value(snapshot: &WirePresentationSnapshot) -> serde_json::Value {
    json!({
        "daemon": hex(snapshot.daemon_instance_id),
        "revision": snapshot.revision,
        "sequence": snapshot.cursor.sequence,
        "desired": {
            "mode": mode_label(snapshot.desired.mode),
            "tun": snapshot.desired.tun_requested,
            "system_proxy": snapshot.desired.system_proxy_requested,
        },
        "applied": {
            "core": snapshot.applied.core_kind.map(core_label),
            "run_state": run_state_label(snapshot.applied.run_state),
            "config_generation": snapshot.applied.config_generation,
            "selected_node": snapshot.applied.selected_node_id.map(|node_id| {
                let name = snapshot
                    .nodes
                    .iter()
                    .find(|node| node.node_id == node_id)
                    .map_or_else(|| "-".to_owned(), |node| decode_display_name(node.name.as_str()));
                json!({ "id": hex(node_id), "name": name })
            }),
        },
        "observed": {
            "up_bps": snapshot.observed.upload_bytes_per_second,
            "down_bps": snapshot.observed.download_bytes_per_second,
            "connections": snapshot.observed.active_connections,
            "restarts": snapshot.observed.core_restart_count,
            "restart_backoff_ms": snapshot.observed.core_restart_backoff_ms,
        },
        "nodes": snapshot.nodes.len(),
        "platform": {
            "proxy_engaged": snapshot.platform.proxy_engaged,
            "tun_engaged": snapshot.platform.tun_engaged,
            "recovery_pending": snapshot.platform.recovery_pending,
        },
    })
}

/// Decodes a percent-encoded display name for humans. Idempotent for plain
/// text; some subscription sources ship URL-encoded tags, so decoding at the
/// presentation layer keeps `list-nodes`/`status`/delay output readable while
/// the registry keeps the raw tag for controller matching. The decoded
/// output is then sanitized: a hostile tag can percent-encode `%1B[`
/// (ANSI forgery), `%0A` (TSV row injection) or bidi overrides — the
/// tree/picker sanitizer runs here so every display consumer is safe.
pub(crate) fn decode_display_name(name: &str) -> String {
    use percent_encoding::percent_decode_str;
    let decoded = match percent_decode_str(name).decode_utf8() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => name.to_owned(),
    };
    crate::entry_tree::strip_controls(&decoded)
}

// --- Compatibility shims for the old `output::report_*` names ----------
// These wrappers preserve the existing call sites in
// `client/execute.rs` and `client/mod.rs` so the refactor
// to the new `output::report_error_returning` + `error::*`
// catalog can land incrementally. New code should use the
// `crate::output` / `crate::error::*` helpers directly.

/// Renders a `ClientError` to the appropriate channel and
/// returns the exit code. Replaces the old
/// `output::report_error(ClientError, json)` signature.
pub(super) fn report_error(error: ClientError, json: bool) -> ExitCode {
    crate::output::report_error_returning(
        crate::output::CliOutput::from_json_flag(json),
        client_error(error, "core (any leaf)"),
    )
}
