//! Daemon operation execution for CLI leaves.
//!
//! Maps each mutation leaf to its wire command, executes it with a fresh
//! operation id, and polls to a terminal state. Human output reports a
//! semantic summary of the requested change; JSON output keeps the raw
//! operation record for scripts. All wire discriminants come from the
//! protocol's typed wire layer (`caly_protocol::protocol::v2`).
//!
//! Round 23: the latency probe / sweep / select-by-delay
//! logic moved to [`delay`] so the top-level file is a
//! 1-page dispatch table again (S4.1 advisory). The
//! remaining `execute_*` functions here are the
//! daemon-RPC core: config / sys / core lifecycle /
//! daemon lifecycle / refresh.

use std::process::ExitCode;

use caly_protocol::{
    client::{ClientContract, ClientError, UdsClient},
    protocol::v2::{
        ExecuteRequest, HandshakeRequest, ProtocolVersion, WireCommand, WireCoreAction,
        all_features,
    },
};

use super::{Query, output};
use crate::client::legacy::{ConfigCmd, CoreCmd};

mod delay;
pub(super) mod ops;
mod sys;

pub(super) use delay::execute_delay_all;
pub(super) use delay::lowest_latency_node;
pub(crate) use sys::execute_sys_cmd;

#[cfg(test)]
use ops::resolved_core_kind;
use ops::{
    active_core_kind, execute_core, execute_lifecycle_immediate, execute_operation_with,
    execute_select_proxy, execute_set_mode, execute_switch_core,
};

/// Validates a single configuration file offline (`config check <file>`).
pub(crate) fn check_single_config_file(path: &std::path::Path, json: bool) -> ExitCode {
    match crate::cli::check_config_file(path) {
        Ok(()) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "configuration": "valid"})
                );
            } else {
                println!("configuration is valid");
            }
            ExitCode::SUCCESS
        }
        Err(error) => output::report_config_check_failed(&error.to_string(), json),
    }
}

/// Executes a `config` family leaf.
pub(super) fn execute_config_cmd(client: &mut UdsClient, cmd: ConfigCmd, json: bool) -> ExitCode {
    match cmd {
        ConfigCmd::Apply => execute_operation_with(client, json, "apply configuration", |id| {
            WireCommand::ApplyConfig { candidate_id: id }
        }),
        // Intercepted as offline commands before connecting; defensive.
        ConfigCmd::Generate
        | ConfigCmd::Default
        | ConfigCmd::Validate
        | ConfigCmd::Check(_)
        | ConfigCmd::Path
        | ConfigCmd::Files
        | ConfigCmd::Edit(_) => ExitCode::FAILURE,
    }
}

/// Maps a `core` command leaf to an offline read-only query, if it is one.
pub(super) fn core_query(cmd: &CoreCmd) -> Option<Query> {
    match cmd {
        CoreCmd::ProxyGroups => Some(Query::ProxyGroups),
        CoreCmd::ListConnections => Some(Query::Connections),
        CoreCmd::Traffic => Some(Query::Traffic),
        CoreCmd::Delay { all: true, .. } => None,
        CoreCmd::Delay {
            name: Some(name),
            url,
            samples,
            ..
        } => Some(Query::UrlTest {
            name: name.clone(),
            url: url.clone(),
            samples: crate::client::query::resolve_samples(*samples),
        }),
        CoreCmd::Delay {
            all: false,
            name: None,
            ..
        } => {
            // Unreachable: clap rejects delay with neither --all nor a name.
            None
        }
        _ => None,
    }
}

/// Executes a `core` family leaf. `core` is the resolved `--core` target;
/// only lifecycle leaves (start/stop/restart) use it.
pub(super) fn execute_core_cmd(
    client: &mut UdsClient,
    cmd: CoreCmd,
    core: Option<&str>,
    json: bool,
    format: Option<crate::cli::OutputFormat>,
) -> ExitCode {
    match cmd {
        CoreCmd::Start => execute_core(client, WireCoreAction::Start, core, json),
        CoreCmd::Stop => execute_core(client, WireCoreAction::Stop, core, json),
        CoreCmd::Restart => execute_core(client, WireCoreAction::Restart, core, json),
        CoreCmd::Switch(target) => execute_switch_core(client, &target, json),
        CoreCmd::Select { node, delay, poll } => {
            execute_select_proxy(client, node.as_deref(), delay, poll, json)
        }
        CoreCmd::Delay {
            all: true,
            url,
            samples,
            ..
        } => execute_delay_all(client, url.as_deref(), samples, json),
        CoreCmd::CloseConnections => {
            execute_operation_with(client, json, "close all connections", |_| {
                WireCommand::CloseAllConnections
            })
        }
        CoreCmd::Mode(mode) => execute_set_mode(client, &mode, json),
        CoreCmd::ListNodes => output::print_nodes(client, json, format),
        // These are intercepted as offline queries before connecting.
        CoreCmd::ProxyGroups
        | CoreCmd::ListConnections
        | CoreCmd::Traffic
        | CoreCmd::Delay { .. }
        | CoreCmd::Rules
        | CoreCmd::RuleMatch(_) => ExitCode::FAILURE,
    }
}

/// W4 (`node pick --apply`): commits a group-member selection through
/// the daemon's supervised operation path. The offline tree has already
/// validated the group kind and the member spelling; the daemon resolves
/// kernel-side tags (Mihomo display name vs sing-box `proxy-<hex>`) and
/// persists the selection.
pub(super) fn execute_pick_proxy_group(
    client: &mut UdsClient,
    group: &str,
    member: &str,
    json: bool,
) -> ExitCode {
    execute_operation_with(
        client,
        json,
        &format!("pick `{member}` in group `{group}`"),
        |_| WireCommand::SelectProxyGroup {
            group: group.to_owned(),
            member: member.to_owned(),
        },
    )
}

/// Executes the configured subscription refresh.///
/// W2-β2b (§4.3): `subscription_id: None` targets every enabled
/// source (the daemon's all-zero batch id); `force` clears the
/// cached validators server-side so the fetch is unconditional;
/// `asynchronous` submits and returns the ack instead of polling
/// for the terminal state (`sub refresh --async`).
pub(super) fn execute_refresh_subscription(
    client: &mut UdsClient,
    json: bool,
    subscription_id: Option<[u8; 16]>,
    force: bool,
    asynchronous: bool,
) -> ExitCode {
    let subscription_id = subscription_id.unwrap_or([0_u8; 16]);
    if asynchronous {
        let id = super::operation_id();
        return match client.execute(ExecuteRequest {
            operation_id: id,
            command: WireCommand::RefreshSubscription {
                subscription_id,
                force,
            },
        }) {
            // Submitted: report the ack and exit 0 without polling.
            // §7 keeps the cancellation contract for pickers only;
            // a fire-and-forget refresh simply prints its receipt.
            Ok(_) => {
                if json {
                    let line = serde_json::json!({
                        "ok": true,
                        "command": "sub refresh",
                        "async": true,
                        "force": force,
                    });
                    println!("{line}");
                } else {
                    println!("ok: refresh subscription submitted (async)");
                }
                ExitCode::SUCCESS
            }
            Err(error) => output::report_error(error, json),
        };
    }
    execute_operation_with(client, json, "refresh subscription", move |_| {
        WireCommand::RefreshSubscription {
            subscription_id,
            force,
        }
    })
}

/// Round 17: `set daemon stop`. The server
/// tears down the runtime after the response is
/// serialized; the client gets the final operation
/// status before the daemon process exits, so
/// polling is unnecessary (and would race the
/// `stop_notifier` that closes the UDS socket right
/// after this response is written).
pub(super) fn execute_stop_daemon(client: &mut UdsClient, json: bool) -> ExitCode {
    execute_lifecycle_immediate(client, json, "stop daemon", |_id| WireCommand::StopDaemon)
}

/// Round 17: `set daemon reload`. The server
/// re-reads `config.yaml` and re-applies the current
/// candidate; the operation completes immediately
/// on the application side, so the response is
/// already terminal and polling is unnecessary.
pub(super) fn execute_reload_config(client: &mut UdsClient, json: bool) -> ExitCode {
    execute_lifecycle_immediate(client, json, "reload config", |_id| {
        WireCommand::ReloadConfig
    })
}

/// Performs the v2 handshake over an established client connection.
pub(crate) fn handshake(client: &mut UdsClient) -> Result<(), ClientError> {
    client.handshake(HandshakeRequest {
        client_version: ProtocolVersion::V2_0,
        requested_features: all_features(),
        auth_token: crate::daemon_config::auth_token(),
    })?;
    Ok(())
}

/// R5 (cli-v3-design.md §14): connect + handshake + one snapshot
/// raced against a wall-clock budget on a helper thread. A wedged
/// socket (accepts the connection, never answers frames)
/// previously parked `caly status` behind the protocol's 30 s
/// REQUEST_TIMEOUT; status now degrades in milliseconds. Notes:
/// - No connect retry here: status is the *triage* surface, so a
///   refusal must surface at once (the startup-race absorption
///   stays with [`connect_with_retry`]'s callers).
/// - On timeout the helper thread is abandoned; it unwinds when
///   the protocol deadline fires and the process exits right
///   after the digest, so no state leaks.
pub(crate) fn probe_snapshot_with_budget(
    socket: &std::path::Path,
    budget: std::time::Duration,
) -> Result<caly_protocol::protocol::v2::WirePresentationSnapshot, String> {
    let socket = socket.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = (|| {
            let mut client = UdsClient::connect(socket)?;
            handshake(&mut client)?;
            client.snapshot()
        })();
        let outcome = outcome.map_err(|error| error.to_string());
        // The receiver is gone once the budget fired; that send
        // failure is expected and carries no information.
        let _ = tx.send(outcome);
    });
    rx.recv_timeout(budget).unwrap_or_else(|_| {
        Err(format!(
            "status probe exceeded the {} ms budget",
            budget.as_millis()
        ))
    })
}

/// Connects to the UDS, retrying briefly on `TransportUnavailable` to absorb a
/// daemon-startup race. Non-transport failures surface immediately.
pub(crate) fn connect_with_retry(socket: &std::path::Path) -> Result<UdsClient, ClientError> {
    const MAX_ATTEMPTS: u32 = 5;
    const RETRY_MS: u64 = 200;
    let mut last = ClientError::TransportUnavailable;
    for attempt in 0..MAX_ATTEMPTS {
        match UdsClient::connect(socket.to_path_buf()) {
            Ok(client) => return Ok(client),
            Err(error) => {
                last = error;
                if !matches!(last, ClientError::TransportUnavailable) || attempt + 1 == MAX_ATTEMPTS
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
            }
        }
    }
    Err(last)
}

/// Connects to the daemon, performs the v2 handshake, and
/// reads the presentation snapshot. The single helper
/// the read-only `show <resource>` leaves go through so
/// the connect + handshake + snapshot boilerplate
/// (three error paths, three `eprintln!`s, three
/// `ExitCode::from(1)`s) is owned in one place.
///
/// `CALY_SOCKET` overrides the default socket path; the
/// default is `AppPaths::from_env().socket_path()`.
///
/// Returns the snapshot on success; on failure, the
/// helper has already printed the typed error envelope
/// and returned the right exit code, so the dispatch
/// can `return match with_uds_snapshot() { ... }` or
/// `return with_uds_snapshot()` for the simple "render
/// the snapshot" path.
pub(crate) fn with_uds_snapshot<F>(on_snapshot: F) -> ExitCode
where
    F: FnOnce(UdsClient, caly_protocol::protocol::v2::WirePresentationSnapshot) -> ExitCode,
{
    use caly_protocol::client::ClientContract;
    use caly_protocol::protocol::v2::WirePresentationSnapshot;
    let socket = std::env::var_os("CALY_SOCKET").map_or_else(
        || caly_platform::paths::AppPaths::from_env().socket_path(),
        std::path::PathBuf::from,
    );
    let mut client = match UdsClient::connect(socket) {
        Ok(c) => c,
        Err(error) => {
            return crate::output::report_error_returning(
                crate::output::CliOutput::Human,
                crate::output::CliError::new(
                    crate::error::daemon::UNREACHABLE,
                    format!("cannot connect to daemon: {error}"),
                    "snapshot",
                ),
            );
        }
    };
    if let Err(error) = handshake(&mut client) {
        return crate::output::report_error_returning(
            crate::output::CliOutput::Human,
            crate::output::CliError::new(
                crate::error::core::OPERATION_FAILED,
                format!("handshake failed: {error}"),
                "snapshot",
            ),
        );
    }
    let snapshot: WirePresentationSnapshot = match client.snapshot() {
        Ok(s) => s,
        Err(error) => {
            return crate::output::report_error_returning(
                crate::output::CliOutput::Human,
                crate::output::CliError::new(
                    crate::error::core::OPERATION_FAILED,
                    format!("snapshot read failed: {error}"),
                    "snapshot",
                ),
            );
        }
    };
    on_snapshot(client, snapshot)
}

#[cfg(test)]
mod core_target_tests;
