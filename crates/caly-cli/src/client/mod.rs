//! CLI-side UDS client commands.

pub mod config_generate;
mod config_writer;
pub mod context;
pub mod execute;
pub mod inline_proxy;
pub(crate) mod interact;
pub(crate) mod interact_live;
pub mod legacy;
pub(crate) mod output;
mod output_capabilities;
pub mod profile;
pub mod proxy_group;
pub(crate) mod query;
pub mod resource_writer;
pub mod rule_provider;
mod rules;
pub mod subscription;
mod yaml_surgery;

pub use query::{Query, run_query};

use std::{path::PathBuf, process::ExitCode};

use caly_protocol::client::{ClientContract, ClientError, UdsClient};

use legacy::{ConfigCmd, CoreCmd, SysCmd};

/// Client-facing command surface (maps the tree leaves to
/// protocol calls). The variant carries the typed
/// per-family subcommand so the dispatch in [`run_client`]
/// can keep the connection-handshake / error-reporting path
/// in one place.
#[derive(Clone, Debug)]
pub enum ClientCommand {
    Status,
    Core(CoreCmd),
    RefreshSubscription {
        /// W2-β2b: `None` = every enabled source (all-zero wire id);
        /// `Some` pins one source (`sub refresh <name-or-url>`).
        subscription_id: Option<[u8; 16]>,
        force: bool,
        asynchronous: bool,
    },
    Sys(SysCmd),
    Config(ConfigCmd),
    /// Round 17: `set daemon stop`. The server tears
    /// down the runtime after the response is
    /// serialized. No payload.
    StopDaemon,
    /// Round 17: `set daemon reload`. The server
    /// re-reads `config.yaml` and re-applies the
    /// current candidate. No payload.
    ReloadConfig,
    /// W4 (`node pick --apply`): pick a named member inside a named
    /// selector group through the daemon's supervised operation path.
    PickProxyGroup {
        group: String,
        member: String,
    },
}

/// Best-effort daemon snapshot read for the currently active core label
/// (`mihomo`/`sing-box`). Falls back to `None` when the daemon is unreachable
/// or reports no core, keeping offline queries usable without a daemon.
pub(super) fn active_core_kind_label() -> Option<String> {
    use caly_protocol::protocol::v2::WireCoreKind;
    let socket = std::env::var_os("CALY_SOCKET").map_or_else(
        || caly_platform::paths::AppPaths::from_env().socket_path(),
        std::path::PathBuf::from,
    );
    let mut client = UdsClient::connect(socket).ok()?;
    execute::handshake(&mut client).ok()?;
    let snapshot = client.snapshot().ok()?;
    WireCoreKind::from_wire(snapshot.applied.core_kind?).map(|kind| {
        match kind {
            WireCoreKind::Mihomo => "mihomo",
            WireCoreKind::SingBox => "sing-box",
            WireCoreKind::Xray => "xray",
        }
        .to_owned()
    })
}

/// Executes one bounded client command against the configured UDS.
pub fn run_client(command: ClientCommand, options: crate::cli::CliOptions) -> ExitCode {
    match run_client_collect(&command, &options) {
        Ok(code) => code,
        Err(error) => output::report_error(error, options.json),
    }
}

/// Round 14: Result-returning twin of [`run_client`]. Same
/// offline short-circuits + daemon-RPC pipeline, but reports
/// failures as a typed [`ClientError`] so the new
/// `Refreshable` trait (and any future typed caller) can fold
/// the outcome into a unified error envelope without going
/// through `output::report_error` first.
///
/// The two functions are not redundant: `run_client` keeps
/// the existing call sites (status / show / set core / set
/// sys / set config / bridge) on a stable
/// `ExitCode`-returning contract. New callers — primarily the
/// 3 `Refreshable` impls — use `run_client_collect` directly.
/// Result-and-exit-code twin used by both call sites: `Ok` carries the
/// operation's own `ExitCode` (the `execute::*` helpers report
/// operation-level failures as `ExitCode::FAILURE`; swallowing that code
/// used to make every failed `set core start` / `config apply` look like a
/// shell-level success — see the CLI exit-code contract). `Err` is the
/// typed transport/handshake failure the caller folds into an envelope.
pub fn run_client_collect(
    command: &ClientCommand,
    options: &crate::cli::CliOptions,
) -> Result<ExitCode, ClientError> {
    // Reject an invalid `--core` target before any daemon
    // connection, so a typo fails fast and clearly instead of
    // silently mapping to mihomo. The protocol `ClientError`
    // does not model "usage error" — pick the closest existing
    // variant, `DecodeRejected`, with a precise `reason` so the
    // caller can surface the right error envelope.
    if let Some(core) = options.core.as_deref()
        && core != "mihomo"
        && core != "sing-box"
    {
        return Err(ClientError::DecodeRejected {
            reason: format!("invalid --core target `{core}` (expected mihomo or sing-box)"),
            suggested_action: "use --core mihomo or --core sing-box".to_owned(),
        });
    }
    // Read-only Clash API queries run offline against the
    // controller, so they do not require a daemon connection
    // or a mutation operation. They never fail (the offline
    // path can't transport a `ClientError`), so we treat them
    // as `Ok(())` and rely on `run_query`'s internal report.
    if let ClientCommand::Core(core_cmd) = command {
        if let Some(query) = execute::core_query(core_cmd) {
            let core = match options.core.as_deref() {
                Some(core) => core.to_owned(),
                None => std::env::var("CALY_CORE").unwrap_or_else(|_| {
                    active_core_kind_label().unwrap_or_else(|| "mihomo".to_owned())
                }),
            };
            // Delegate the rendering to `run_query` (which owns the
            // JSON / human split) and propagate its `ExitCode` — a failed
            // controller query must surface as a non-zero process exit.
            return Ok(run_query(&core, query, options.json));
        }
        // Routing-rule commands run fully offline against the
        // configured rules. Same exit-code propagation as above.
        if matches!(core_cmd, CoreCmd::Rules | CoreCmd::RuleMatch(_)) {
            return Ok(rules::run_rules(core_cmd, options.json));
        }
    }
    // `config` leaves that never need a daemon connection run
    // offline. They have no `ClientError` failure mode, so
    // success is the only typed outcome.
    if let ClientCommand::Config(config_cmd) = command {
        match config_cmd {
            ConfigCmd::Generate
            | ConfigCmd::Default
            | ConfigCmd::Validate
            | ConfigCmd::Path
            | ConfigCmd::Files
            | ConfigCmd::Edit(_) => return Ok(ExitCode::SUCCESS),
            ConfigCmd::Check(_) | ConfigCmd::Apply => {}
        }
    }
    let socket = options.socket.clone().unwrap_or_else(|| {
        std::env::var_os("CALY_SOCKET").map_or_else(
            || caly_platform::paths::AppPaths::from_env().socket_path(),
            PathBuf::from,
        )
    });
    let mut client = execute::connect_with_retry(&socket)?;
    execute::handshake(&mut client)?;
    let core = options.core.as_deref();
    // The `execute::*` helpers print their own envelopes and
    // return `ExitCode`; propagate that code to the caller so
    // operation-level failure (a refused start, a failed apply)
    // is a non-zero process exit instead of a swallowed success.
    Ok(match command {
        ClientCommand::Status => output::print_status(&mut client, options.json),
        ClientCommand::Core(core_cmd) => execute::execute_core_cmd(
            &mut client,
            core_cmd.clone(),
            core,
            options.json,
            options.format,
        ),
        ClientCommand::RefreshSubscription {
            subscription_id,
            force,
            asynchronous,
        } => execute::execute_refresh_subscription(
            &mut client,
            options.json,
            *subscription_id,
            *force,
            *asynchronous,
        ),
        ClientCommand::Sys(sys_cmd) => {
            execute::execute_sys_cmd(&mut client, sys_cmd.clone(), options.json)
        }
        ClientCommand::Config(config_cmd) => {
            execute::execute_config_cmd(&mut client, config_cmd.clone(), options.json)
        }
        ClientCommand::StopDaemon => execute::execute_stop_daemon(&mut client, options.json),
        ClientCommand::ReloadConfig => execute::execute_reload_config(&mut client, options.json),
        // W4: the offline tree validated the group/member spelling;
        // the daemon resolves kernel tags and persists the selection.
        ClientCommand::PickProxyGroup { group, member } => {
            execute::execute_pick_proxy_group(&mut client, group, member, options.json)
        }
    })
}

/// Generates a random 128-bit operation id.
///
/// Operation ids only need uniqueness (they are not security tokens): 128
/// random bits from the platform CSPRNG give negligible collision probability
/// and carry no time/pid structure, so adjacent operations are never related.
pub(crate) fn operation_id() -> [u8; 16] {
    caly_platform::entropy::random_bytes::<16>()
}

/// Hex-encodes a 128-bit identity for display and wire JSON.
pub(crate) fn hex(value: [u8; 16]) -> String {
    caly_domain::to_hex(value)
}

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod profile_tests;
