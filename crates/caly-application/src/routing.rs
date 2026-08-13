//! Deterministic command ownership routing.

use crate::{
    command_bus::{Command, CommandEnvelope},
    operations::OperationCancellationToken,
};

/// Sole owner/coordinator selected for a mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandTarget {
    CoreLifecycle,
    ConfigActor,
    CoreActor,
    SubscriptionActor,
    PlatformActor,
}

/// Command paired with its statically selected owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutedCommand {
    pub target: CommandTarget,
    pub envelope: CommandEnvelope,
    pub cancellation: OperationCancellationToken,
}

/// Backpressure/closure failure returns the complete command.
#[derive(Debug, Eq, PartialEq)]
pub enum RouteDispatchError {
    ResourceExhausted(RoutedCommand),
    TargetClosed(RoutedCommand),
}

/// Bounded owner-mailbox fanout supplied by runtime wiring.
pub trait CommandSink {
    fn try_dispatch(&mut self, command: RoutedCommand) -> Result<(), RouteDispatchError>;
}

/// Pure exhaustive routing; adding a Command requires choosing one owner.
pub fn route(envelope: CommandEnvelope, cancellation: OperationCancellationToken) -> RoutedCommand {
    let target = match &envelope.command {
        Command::ApplyConfig { .. } => CommandTarget::ConfigActor,
        Command::SwitchCore { .. } => CommandTarget::CoreLifecycle,
        Command::SetTun { .. } => CommandTarget::PlatformActor,
        Command::SelectProxy { .. }
        | Command::SelectProxyGroup { .. }
        | Command::CloseAllConnections
        | Command::SetMode { .. } => CommandTarget::CoreActor,
        Command::RefreshSubscription { .. } => CommandTarget::SubscriptionActor,
        Command::SetSystemProxy { .. } | Command::SetSystemProxyPac { .. } => {
            CommandTarget::PlatformActor
        }
        // Round 17: daemon shutdown is a lifecycle event
        // (it tears down the runtime), so it routes to
        // `CoreLifecycle`. Reload is a config-class
        // event (it re-applies the current candidate),
        // so it routes to `ConfigActor`. Both handlers
        // are no-ops today; the actual side-effects live
        // in the daemon runtime (`daemon.rs`).
        Command::StopDaemon => CommandTarget::CoreLifecycle,
        Command::ReloadConfig => CommandTarget::ConfigActor,
    };
    RoutedCommand {
        target,
        envelope,
        cancellation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::OperationCancellationToken;

    fn routed(command: crate::command_bus::Command) -> CommandTarget {
        route(
            CommandEnvelope {
                operation_id: caly_domain::OperationId::from_bytes([1; 16]),
                command,
            },
            OperationCancellationToken::new(),
        )
        .target
    }

    #[test]
    fn set_tun_routes_to_platform_actor() {
        assert_eq!(
            routed(Command::SetTun { enabled: true }),
            CommandTarget::PlatformActor
        );
    }

    #[test]
    fn set_system_proxy_routes_to_platform_actor() {
        assert_eq!(
            routed(Command::SetSystemProxy { enabled: true }),
            CommandTarget::PlatformActor
        );
    }
}
