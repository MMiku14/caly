//! Strict typed command admission conversion.

use caly_application::command_bus::{Command, CommandEnvelope, CoreAction};
use caly_domain::{CoreKind, OperationId, ProxyMode};
use caly_protocol::protocol::v2::{ExecuteRequest, WireCommand};

/// Command rejection before Application admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandDecodeError {
    UnknownCommand(i32),
    UnknownCore(i32),
    /// Audit #99: an unrecognised core *action* discriminant used to be
    /// reported as `UnknownCommand`, conflating two distinct decode bugs.
    UnknownAction(i32),
    UnknownMode(i32),
}

/// Converts only recognized typed commands.
pub fn command_from_wire(request: ExecuteRequest) -> Result<CommandEnvelope, CommandDecodeError> {
    let operation_id = OperationId::from_bytes(request.operation_id);
    let command = match request.command {
        WireCommand::ApplyConfig { candidate_id } => Command::ApplyConfig { candidate_id },
        WireCommand::SwitchCore {
            core_kind: raw,
            action: raw_action,
        } => {
            let (target, action) = core(raw, raw_action)?;
            Command::SwitchCore { target, action }
        }
        WireCommand::SelectProxy { node_id } => Command::SelectProxy {
            node_id: caly_domain::NodeId::from_bytes(node_id),
        },
        // W4 (`node pick`): group/member names travel verbatim.
        WireCommand::SelectProxyGroup { group, member } => Command::SelectProxyGroup {
            group: std::sync::Arc::from(group),
            member: std::sync::Arc::from(member),
        },
        WireCommand::SetMode { mode: raw } => Command::SetMode {
            mode: proxy_mode(raw)?,
        },
        WireCommand::SetTun { enabled } => Command::SetTun { enabled },
        WireCommand::SetSystemProxy { enabled } => Command::SetSystemProxy { enabled },
        WireCommand::SetSystemProxyPac { url } => Command::SetSystemProxyPac {
            url: std::sync::Arc::from(url),
        },
        WireCommand::RefreshSubscription {
            subscription_id,
            force,
        } => Command::RefreshSubscription {
            subscription_id: caly_domain::SubscriptionId::from_bytes(subscription_id),
            force,
            // W2-β2b: `scheduled` is daemon-internal (the periodic
            // timer submits the application command directly,
            // never via wire).
            scheduled: false,
        },
        WireCommand::CloseAllConnections => Command::CloseAllConnections,
        WireCommand::StopDaemon => Command::StopDaemon,
        WireCommand::ReloadConfig => Command::ReloadConfig,
        WireCommand::Unknown { raw_kind, .. } => {
            return Err(CommandDecodeError::UnknownCommand(raw_kind.0));
        }
    };
    Ok(CommandEnvelope {
        operation_id,
        command,
    })
}

fn core(raw: i32, action: i32) -> Result<(CoreKind, CoreAction), CommandDecodeError> {
    use caly_protocol::protocol::v2::{WireCoreAction, WireCoreKind};
    let target = WireCoreKind::from_wire(raw)
        .map(CoreKind::from)
        .ok_or(CommandDecodeError::UnknownCore(raw))?;
    let action = WireCoreAction::from_wire(action)
        .map(|wire| match wire {
            WireCoreAction::Start => CoreAction::Start,
            WireCoreAction::Stop => CoreAction::Stop,
            WireCoreAction::Restart => CoreAction::Restart,
        })
        .ok_or(CommandDecodeError::UnknownAction(action))?;
    Ok((target, action))
}

fn proxy_mode(raw: i32) -> Result<ProxyMode, CommandDecodeError> {
    use caly_protocol::protocol::v2::WireMode;
    WireMode::from_wire(raw)
        .map(ProxyMode::from)
        .ok_or(CommandDecodeError::UnknownMode(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_application::command_bus::Command;
    use caly_protocol::protocol::v2::{RawCommandKind, WirePayload};

    #[test]
    fn typed_command_converts_without_default() -> Result<(), CommandDecodeError> {
        let request = ExecuteRequest {
            operation_id: [1; 16],
            command: WireCommand::SetMode { mode: 2 },
        };
        let converted = command_from_wire(request)?;
        assert_eq!(
            converted.command,
            Command::SetMode {
                mode: ProxyMode::Global
            }
        );
        Ok(())
    }

    #[test]
    fn unknown_core_is_rejected() {
        let request = ExecuteRequest {
            operation_id: [1; 16],
            command: WireCommand::SwitchCore {
                core_kind: 999,
                action: 1,
            },
        };
        assert_eq!(
            command_from_wire(request),
            Err(CommandDecodeError::UnknownCore(999))
        );
    }

    #[test]
    fn unknown_command_is_preserved_then_rejected() {
        let request = ExecuteRequest {
            operation_id: [1; 16],
            command: WireCommand::Unknown {
                raw_kind: RawCommandKind(777),
                payload: WirePayload::new(),
            },
        };
        assert_eq!(
            command_from_wire(request),
            Err(CommandDecodeError::UnknownCommand(777))
        );
    }

    /// W4: `node pick` — the group/member names travel verbatim and the
    /// raw kind round-trips as wire 11 (appended after the frozen 1–10
    /// set, never renumbered).
    #[test]
    fn select_proxy_group_decodes_verbatim_names() -> Result<(), CommandDecodeError> {
        let request = ExecuteRequest {
            operation_id: [1; 16],
            command: WireCommand::SelectProxyGroup {
                group: "节点选择".to_owned(),
                member: "DIRECT".to_owned(),
            },
        };
        let converted = command_from_wire(request)?;
        assert_eq!(
            converted.command,
            Command::SelectProxyGroup {
                group: std::sync::Arc::from("节点选择"),
                member: std::sync::Arc::from("DIRECT"),
            }
        );
        // Raw discriminant 11 maps to the typed kind.
        assert_eq!(
            RawCommandKind(11).known(),
            Some(caly_protocol::protocol::v2::CommandKind::SelectProxyGroup)
        );
        Ok(())
    }
}
