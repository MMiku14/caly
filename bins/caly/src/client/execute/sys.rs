//! `sys` family leaves: system proxy and the hot TUN toggle.
//!
//! Split out of `client/execute/mod.rs` (audit #70 file-length
//! budget). The TUN toggle persists its intent as a small layered
//! override fragment so the core renderer picks it up on the next
//! apply without editing the user's hand-written config files.

use std::process::ExitCode;

use caly_platform::paths::AppPaths;
use caly_protocol::{client::UdsClient, protocol::v2::WireCommand};

use super::super::output;
use super::ops::execute_operation;
use crate::client::legacy::SysCmd;

/// The layered override fragment `caly sys tun` writes so the TUN intent
/// survives daemon restarts without editing the user's 60-tun.yaml: it sorts
/// after that file, so its `tun.enabled` wins in the layered merge.
const TUN_TOGGLE_OVERRIDE: &str = "65-tun-toggle.yaml";

/// Persists the hot TUN intent into the layered config directory. Returns an
/// exit code on failure; the caller aborts before touching the daemon.
fn patch_tun_enabled(enabled: bool, json: bool) -> Result<(), ExitCode> {
    let directory = AppPaths::from_env().config.join("config.d");
    let contents = format!(
        "# Hot TUN toggle written by `caly sys tun`; layered over\n\
         # config.d/60-tun.yaml so the core renderer sees tun.enabled={enabled}\n\
         # without editing the user's config. Delete this file to hand the\n\
         # TUN setting back to the layered config files.\n\
         tun:\n  enabled: {enabled}\n"
    );
    let path = directory.join(TUN_TOGGLE_OVERRIDE);
    std::fs::write(&path, contents).map_err(|error| {
        output::report_failure(
            &format!(
                "cannot persist TUN toggle {}: {error} (inspect config-directory ownership)",
                path.display()
            ),
            json,
        )
    })?;
    Ok(())
}

/// Executes a `sys` family leaf.
pub(crate) fn execute_sys_cmd(client: &mut UdsClient, cmd: SysCmd, json: bool) -> ExitCode {
    match cmd {
        SysCmd::Proxy(enabled) => {
            let summary = if enabled {
                "enable system proxy"
            } else {
                "disable system proxy"
            };
            execute_operation(client, json, summary, false, |_| {
                WireCommand::SetSystemProxy { enabled }
            })
        }
        SysCmd::ProxyPac(url) => {
            execute_operation(client, json, "enable system proxy PAC mode", false, |_| {
                WireCommand::SetSystemProxyPac { url }
            })
        }
        SysCmd::Tun(enabled) => {
            // Hot TUN toggle: no daemon restart needed. The core renderer
            // re-reads the layered TUN config on every apply, so persist the
            // intent into a small override fragment layered after
            // config.d/60-tun.yaml, re-render+commit the core config, then
            // engage/restore the platform TUN device and routes. Ordering
            // matters: the engine must expose the tun inbound before the
            // device comes up (no silent half-engagement), and the inbound
            // goes away before the device is restored on shutdown.
            if let Err(code) = patch_tun_enabled(enabled, json) {
                return code;
            }
            let applied =
                execute_operation(client, json, "apply configuration for TUN", false, |id| {
                    WireCommand::ApplyConfig { candidate_id: id }
                });
            if applied != ExitCode::SUCCESS {
                return applied;
            }
            let summary = if enabled { "engage TUN" } else { "restore TUN" };
            let outcome = execute_operation(client, json, summary, false, |_| {
                WireCommand::SetTun { enabled }
            });
            // Rollback transaction: a failed engage must never leave the
            // committed tun-inbound config in effect — the core would keep
            // hijacking traffic into a device the platform layer did not
            // confirm, black-holing the network (the half-engaged state that
            // broke direct connectivity in the field). Reversing the enable
            // sequence restores the invariant: toggle off, then re-apply so
            // the core restarts without the tun inbound, releasing the
            // device and tearing down auto-route rules.
            if enabled && outcome != ExitCode::SUCCESS {
                let _ = patch_tun_enabled(false, json);
                let reverted = execute_operation(
                    client,
                    json,
                    "revert TUN configuration after failed engage",
                    false,
                    |id| WireCommand::ApplyConfig { candidate_id: id },
                );
                let message = if reverted == ExitCode::SUCCESS {
                    "TUN engage failed; the configuration was reverted, so routing is restored (fix the cause and retry `caly set tun on`)"
                } else {
                    "TUN engage failed AND the config revert failed; the core may still route traffic through the TUN device — run `caly set tun off` to restore"
                };
                crate::output::CliOutput::from_json_flag(json).info(message);
            }
            outcome
        }
    }
}
