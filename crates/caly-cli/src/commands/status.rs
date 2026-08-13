//! `caly status` — daemon snapshot with the R5 probe budget.

use std::process::ExitCode;
use std::time::Duration;

use crate::output::CliOutput;

/// R5 (cli-v3-design.md §14): `caly status` is the triage surface
/// and must never hang on a wedged socket. Connect + handshake +
/// one snapshot race this budget on a helper thread.
const PROBE_BUDGET: Duration = Duration::from_millis(500);

pub fn run(options: crate::cli::CliOptions, _output: CliOutput) -> ExitCode {
    let socket = options.socket.clone().unwrap_or_else(|| {
        std::env::var_os("CALY_SOCKET").map_or_else(
            || caly_platform::paths::AppPaths::from_env().socket_path(),
            std::path::PathBuf::from,
        )
    });
    match crate::client::execute::probe_snapshot_with_budget(&socket, PROBE_BUDGET) {
        Ok(snapshot) => {
            // The running kernel's live selection is the authority for the
            // `selected-node` line: the daemon's applied projection only
            // records `select_proxy`/group picks it witnessed, so kernel-side
            // switches and restarts left `status` stale (2026-08-12 audit).
            // Best-effort: offline kernel or query failure keeps the snapshot
            // value (renderer falls back automatically).
            let kernel_selected = kernel_selected_display(&snapshot);
            crate::client::output::render_status_snapshot(
                &snapshot,
                options.json,
                kernel_selected.as_deref(),
            );
            ExitCode::SUCCESS
        }
        Err(reason) => offline_digest(&socket, options.json, &reason),
    }
}

/// Resolves the running kernel's live selection (proxy-group `now`) onto a
/// display name, or `None` when the kernel is offline, reports no selection,
/// or the selection does not map to a snapshot node (builtins like
/// DIRECT/REJECT pass through verbatim).
fn kernel_selected_display(
    snapshot: &caly_protocol::protocol::v2::WirePresentationSnapshot,
) -> Option<String> {
    // M3 (2026-08-12 boundary audit): the kernel query can block up to
    // 2×QUERY_TIMEOUT on a wedged control socket — far beyond status's
    // 500ms probe budget. Run it on a helper thread and give up after the
    // budget; the renderer then falls back to the snapshot value.
    let core_kind = snapshot.applied.core_kind;
    let nodes: Vec<([u8; 16], String)> = snapshot
        .nodes
        .iter()
        .map(|node| (node.node_id, node.name.as_str().to_owned()))
        .collect();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(kernel_selected_display_sync(core_kind, &nodes));
    });
    receiver
        .recv_timeout(std::time::Duration::from_millis(500))
        .ok()
        .flatten()
}

fn kernel_selected_display_sync(
    core_kind: Option<i32>,
    nodes: &[([u8; 16], String)],
) -> Option<String> {
    use std::str::FromStr;
    let core = caly_protocol::protocol::v2::WireCoreKind::from_wire(core_kind?)?;
    let label = core.label().to_owned();
    let mut control = crate::client::query::build_control(&label).ok()?;
    let groups = control
        .proxy_groups(crate::client::query::QUERY_TIMEOUT)
        .ok()?;
    let now = groups.iter().find_map(|group| group.selected.clone())?;
    if core == caly_protocol::protocol::v2::WireCoreKind::SingBox {
        // The Clash-compat surface reports the outbound tag `proxy-<hex>`,
        // whose hex is exactly the node id; map back to the display name.
        let hex = now.strip_prefix("proxy-")?;
        let node_id = caly_domain::NodeId::from_str(hex).ok()?;
        let name = nodes
            .iter()
            .find(|(id, _)| *id == node_id.into_bytes())
            .map(|(_, name)| crate::client::output::decode_display_name(name));
        return Some(name.unwrap_or(now));
    }
    // Mihomo reports the plain display name; pass it through verbatim.
    Some(now)
}

/// The R5 degraded face: an offline digest (daemon / socket /
/// context profile) on stdout for humans, plus the standard error
/// envelope on stderr; the JSON envelope gains `profile` and
/// `socket` keys so a script need not re-probe. The exit code
/// stays 1 (C-A contract: an unreachable daemon is a failure,
/// never a clean degraded zero).
fn offline_digest(socket: &std::path::Path, json: bool, reason: &str) -> ExitCode {
    let paths = caly_platform::paths::AppPaths::from_env();
    let profile = crate::client::context::current(&paths);
    let error = crate::error::CliError::new(crate::error::daemon::UNREACHABLE, reason, "status")
        .with_hint("start it with `caly daemon`, or check `caly doctor`");
    if json {
        let mut value = error.to_json();
        value["profile"] = serde_json::json!(profile);
        value["socket"] = serde_json::json!(socket.display().to_string());
        eprintln!("{value}");
        return ExitCode::FAILURE;
    }
    println!("daemon:   unreachable");
    println!("socket:   {}", socket.display());
    println!(
        "profile:  {}",
        profile
            .as_deref()
            .unwrap_or("(no context; see `caly profile list`)"),
    );
    crate::output::report_error(CliOutput::Human, &error);
    ExitCode::FAILURE
}
