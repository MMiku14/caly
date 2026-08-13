//! Pipeline reconcilers: event consumers that react to stage facts with the
//! minimal follow-up command (2026-08-12 pipeline design, 刀 2). The config
//! reconciler replaces the ad-hoc `config_mailbox` hook inside
//! `SubscriptionCommandHandler`: the handler only publishes
//! `SubscriptionRefreshed { changed }`, and this reconciler decides whether
//! the kernel config must be re-rendered — explicit, observable, extensible.

use std::time::Duration;

use crate::{
    command_bus::{Command, CommandEnvelope},
    events::{DaemonEvent, EventBus},
    operations::OperationCancellationToken,
    routing::{CommandTarget, RoutedCommand},
    runtime::ActorIngress,
};

/// Fixed operation id for the auto `ReloadConfig` enqueued after a
/// subscription refresh that changed nodes. Internal to the daemon: no CLI
/// waits on it, but it stays a valid id so the config actor can trace the
/// candidate it renders from.
const AUTO_RELOAD_OPERATION: caly_domain::OperationId = caly_domain::OperationId::from_bytes([
    0xA0, 0x10, 0x52, 0x45, 0x4C, 0x4F, 0x41, 0x44, 0x2D, 0x41, 0x55, 0x54, 0x4F, 0x00, 0x00, 0x01,
]);

/// Receive timeout so the thread also notices the daemon shutting down
/// (dropped sender) instead of blocking forever.
const EVENT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Reacts to pipeline events by enqueuing follow-up commands on the config
/// actor's mailbox. Spawn one per daemon lifetime; the thread ends when the
/// bus is dropped (daemon shutdown).
pub struct ConfigReconciler {
    events: std::sync::mpsc::Receiver<DaemonEvent>,
    config_mailbox: ActorIngress<RoutedCommand>,
}

impl ConfigReconciler {
    /// Spawns the reconciler thread on the given bus and config mailbox.
    pub fn spawn(bus: EventBus, config_mailbox: ActorIngress<RoutedCommand>) {
        let events = bus.subscribe();
        std::thread::spawn(move || {
            let mut reconciler = Self {
                events,
                config_mailbox,
            };
            reconciler.run();
        });
    }

    fn run(&mut self) {
        // `recv_timeout` returns `Timeout` on an idle interval — a normal
        // wake-up, not a reason to stop; only a disconnected sender ends
        // the loop (see `run_reconciler`).
        crate::events::run_reconciler(&self.events, EVENT_POLL_INTERVAL, || {
            self.enqueue_reload();
        });
        tracing::debug!("config reconciler stopped");
    }

    /// Enqueues a `ReloadConfig` so the kernel config is re-rendered from
    /// the refreshed registry. Best-effort: a full mailbox only loses the
    /// auto-apply, never the refresh result.
    fn enqueue_reload(&self) {
        let command = RoutedCommand {
            target: CommandTarget::ConfigActor,
            envelope: CommandEnvelope {
                operation_id: AUTO_RELOAD_OPERATION,
                command: Command::ReloadConfig,
            },
            cancellation: OperationCancellationToken::new(),
        };
        if let Err(error) = self.config_mailbox.try_send(command) {
            tracing::warn!(
                error = ?error,
                "auto kernel-config reload after subscription refresh was not enqueued"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::actor_mailbox;
    use std::time::Duration;

    #[test]
    fn changed_refresh_triggers_reload() {
        let bus = EventBus::new();
        let (ingress, receiver) = actor_mailbox::<RoutedCommand>(4).expect("mailbox allocation");
        ConfigReconciler::spawn(bus.clone(), ingress);
        bus.publish(DaemonEvent::SubscriptionRefreshed { changed: true });
        let reload = receiver
            .receive_timeout(Duration::from_millis(500))
            .expect("changed refresh must enqueue a ReloadConfig");
        assert_eq!(reload.target, CommandTarget::ConfigActor);
        assert!(matches!(reload.envelope.command, Command::ReloadConfig));
    }

    #[test]
    fn unchanged_refresh_never_triggers_reload() {
        let bus = EventBus::new();
        let (ingress, receiver) = actor_mailbox::<RoutedCommand>(4).expect("mailbox allocation");
        ConfigReconciler::spawn(bus.clone(), ingress);
        bus.publish(DaemonEvent::SubscriptionRefreshed { changed: false });
        // Give the reconciler ample time to (wrongly) react, then assert
        // nothing arrived.
        std::thread::sleep(Duration::from_millis(200));
        match receiver.receive_timeout(Duration::from_millis(10)) {
            Err(
                crate::runtime::MailboxReceiveError::TimedOut
                | crate::runtime::MailboxReceiveError::Closed,
            ) => {}
            Err(other) => panic!("receiver failed unexpectedly: {other:?}"),
            Ok(reload) => panic!(
                "unchanged refresh must not enqueue a reload, got {:?}",
                reload.envelope.command
            ),
        }
    }
}
