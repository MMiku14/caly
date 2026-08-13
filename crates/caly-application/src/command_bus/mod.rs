//! Bounded mutation ingress owned by the application runtime.

use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    time::Duration,
};

use caly_domain::{NodeId, OperationId, ProxyMode, SubscriptionId};

/// Core lifecycle action selected by an operation command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreAction {
    Start,
    Stop,
    Restart,
}

/// Mutation command routed to one owner or coordinator.
///
/// Not `Copy` since W4: `SelectProxyGroup` carries `Arc<str>` names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    ApplyConfig {
        candidate_id: [u8; 16],
    },
    SwitchCore {
        target: caly_domain::CoreKind,
        action: CoreAction,
    },
    SelectProxy {
        node_id: NodeId,
    },
    /// W4 (`node pick`): pick a named member inside a named selector
    /// group. Names are `Arc<str>` to keep [`Command`] `Copy`.
    SelectProxyGroup {
        group: Arc<str>,
        member: Arc<str>,
    },
    SetMode {
        mode: ProxyMode,
    },
    SetTun {
        enabled: bool,
    },
    SetSystemProxy {
        enabled: bool,
    },
    /// `sysproxy pac <url>`: desktop proxy auto mode with a PAC URL
    /// (2026-08-12). Routed to the PlatformActor like `SetSystemProxy`.
    SetSystemProxyPac {
        url: Arc<str>,
    },
    RefreshSubscription {
        subscription_id: SubscriptionId,
        /// W2-β2b: `sub refresh --force` / periodic-timer rulings —
        /// see `caly_ports::RefreshMode`.
        force: bool,
        scheduled: bool,
    },
    CloseAllConnections,
    /// Round 17: server-side shutdown. The application
    /// handler is a no-op (the runtime observes the
    /// completed `Stop` and breaks its `serve()` loop).
    /// Routed to `CoreLifecycle` because a daemon
    /// shutdown is a lifecycle-class event.
    StopDaemon,
    /// Round 17: server-side reload. The application
    /// handler is a no-op (the runtime re-reads
    /// `config.yaml` and re-applies the current
    /// candidate; the typed `ApplyConfig` flow is the
    /// authoritative re-apply path).
    /// Routed to `ConfigActor` for symmetry.
    ReloadConfig,
}

/// Idempotent command envelope accepted by the daemon.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEnvelope {
    pub operation_id: OperationId,
    pub command: Command,
}

/// Mutation ingress rejection; no command is silently dropped.
#[derive(Debug, Eq, PartialEq)]
pub enum CommandIngressError {
    InvalidCapacity,
    ResourceExhausted(CommandEnvelope),
    Closed(CommandEnvelope),
}

impl core::fmt::Display for CommandIngressError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidCapacity => formatter.write_str("command capacity must be above zero"),
            Self::ResourceExhausted(_) => formatter
                .write_str("command bus is full; query the existing operation or retry later"),
            Self::Closed(_) => formatter
                .write_str("command ingress is closed; reconnect after daemon startup completes"),
        }
    }
}

impl std::error::Error for CommandIngressError {}

/// Cloneable bounded ingress handle.
#[derive(Clone, Debug)]
pub struct CommandIngress(SyncSender<CommandEnvelope>);

impl Command {
    /// Whether the command contract supports cooperative cancellation while
    /// Running and before its explicit commit point. Current daemon admission
    /// blocks these commands until their owners are fully wired.
    pub const fn supports_running_cancellation(&self) -> bool {
        matches!(
            self,
            Self::ApplyConfig { .. } | Self::SetMode { .. } | Self::SetTun { .. }
        )
    }

    /// Stable operation kind used in status and idempotency diagnostics.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ApplyConfig { .. } => "config.apply",
            Self::SwitchCore { .. } => "core.switch",
            Self::SelectProxy { .. } => "proxy.select",
            Self::SelectProxyGroup { .. } => "proxy.group.select",
            Self::SetMode { .. } => "proxy.mode.set",
            Self::SetTun { .. } => "tun.set",
            Self::SetSystemProxy { .. } => "system_proxy.set",
            Self::SetSystemProxyPac { .. } => "system_proxy.pac",
            Self::RefreshSubscription { .. } => "subscription.refresh",
            Self::CloseAllConnections => "connections.close_all",
            Self::StopDaemon => "daemon.stop",
            Self::ReloadConfig => "config.reload",
        }
    }
}

impl CommandIngress {
    /// Enqueues without waiting; full capacity is visible to the caller.
    pub fn try_submit(&self, envelope: CommandEnvelope) -> Result<(), CommandIngressError> {
        self.0.try_send(envelope).map_err(|error| match error {
            TrySendError::Full(value) => CommandIngressError::ResourceExhausted(value),
            TrySendError::Disconnected(value) => CommandIngressError::Closed(value),
        })
    }
}

/// Single-owner command receiver.
pub struct CommandReceiver(Receiver<CommandEnvelope>);

impl CommandReceiver {
    /// Receives with a finite poll boundary for cancellation/shutdown checks.
    pub fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<CommandEnvelope, CommandReceiveError> {
        if timeout.is_zero() {
            return Err(CommandReceiveError::InvalidTimeout);
        }
        self.0.recv_timeout(timeout).map_err(|error| match error {
            RecvTimeoutError::Timeout => CommandReceiveError::TimedOut,
            RecvTimeoutError::Disconnected => CommandReceiveError::Closed,
        })
    }
}

/// Command receive outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandReceiveError {
    TimedOut,
    Closed,
    InvalidTimeout,
}

impl core::fmt::Display for CommandReceiveError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "command receive ended: {self:?}; check cancellation or shutdown state"
        )
    }
}

impl std::error::Error for CommandReceiveError {}

/// Creates a positive-capacity command bus.
pub fn command_bus(
    capacity: usize,
) -> Result<(CommandIngress, CommandReceiver), CommandIngressError> {
    if capacity == 0 {
        return Err(CommandIngressError::InvalidCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((CommandIngress(sender), CommandReceiver(receiver)))
}
