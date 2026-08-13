//! SubscriptionActor refresh handler.

use caly_domain::PresentationDelta;

use crate::{
    actor_result::ActorResultClient,
    command_bus::Command,
    events::{DaemonEvent, EventBus},
    routing::{CommandTarget, RoutedCommand},
    runtime::{ActorDirective, ActorHandler},
};

use super::{
    reporting::{finish_report, HandlerReportError},
    SubscriptionCommandBackend,
};
/// Nonblocking backend owning fetch/parse/cache generation work.
pub struct SubscriptionCommandHandler<B> {
    backend: B,
    results: ActorResultClient,

    /// Event bus for pipeline-stage facts: a refresh that changed nodes
    /// publishes `SubscriptionRefreshed { changed: true }`, and the config
    /// reconciler (刀 2, 2026-08-12 pipeline design) reacts by re-rendering
    /// the kernel config — the explicit publish→subscribe contract that
    /// replaced the ad-hoc config mailbox hook. A default (empty) bus is
    /// harmless for tests and manual compositions.
    events: EventBus,
}

impl<B> SubscriptionCommandHandler<B> {
    pub fn new(backend: B, results: ActorResultClient) -> Self {
        Self {
            backend,
            results,
            events: EventBus::new(),
        }
    }

    /// Wires the handler to a shared bus so pipeline consumers (the config
    /// reconciler) observe refresh outcomes.
    #[must_use]
    pub fn with_event_bus(mut self, events: EventBus) -> Self {
        self.events = events;
        self
    }
}

#[derive(Debug)]
pub enum SubscriptionHandlerError {
    WrongTarget,
    WrongCommand,
    Report(HandlerReportError),
}

impl<B: SubscriptionCommandBackend> ActorHandler<RoutedCommand> for SubscriptionCommandHandler<B> {
    type Error = SubscriptionHandlerError;

    fn handle(&mut self, routed: RoutedCommand) -> Result<ActorDirective, Self::Error> {
        if routed.target != CommandTarget::SubscriptionActor {
            return Err(SubscriptionHandlerError::WrongTarget);
        }
        if routed.cancellation.is_cancel_requested() {
            return Ok(ActorDirective::Continue);
        }
        let operation_id = routed.envelope.operation_id;
        let Command::RefreshSubscription {
            subscription_id,
            force,
            scheduled,
        } = routed.envelope.command
        else {
            return Err(SubscriptionHandlerError::WrongCommand);
        };
        let outcome = self
            .backend
            .refresh(
                subscription_id,
                caly_ports::RefreshMode { force, scheduled },
            )
            .map(|refresh| {
                // Pipeline event (刀 2): a refresh that actually changed
                // nodes publishes the fact; the config reconciler decides
                // to re-render (no manual `caly config apply`, and the
                // 304/unchanged ticks of the periodic timer never restart
                // the core).
                self.events.publish(DaemonEvent::SubscriptionRefreshed {
                    changed: refresh.changed,
                });
                // 刀 6 (常驻路径 audit): an unchanged/quiet refresh must
                // not publish a NodesReplaced — the projector would replace
                // the node table with a duplicate (or an EMPTY table on
                // quiet ticks when no source was due), wasting the
                // projection chain and clearing the visible node list
                // between real refreshes.
                if refresh.changed {
                    vec![PresentationDelta::NodesReplaced(refresh.nodes)]
                } else {
                    Vec::new()
                }
            });
        finish_report(&self.results, operation_id, outcome)
            .map_err(SubscriptionHandlerError::Report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_result::{actor_result_mailbox, ActorReport};
    use crate::command_bus::CommandEnvelope;
    use crate::operations::OperationCancellationToken;
    use std::time::Duration;

    /// Deterministic `SubscriptionCommandBackend` test double.
    struct FakeBackend {
        changed: bool,
        fail: bool,
    }

    impl SubscriptionCommandBackend for FakeBackend {
        fn refresh(
            &mut self,
            _subscription: caly_domain::SubscriptionId,
            _mode: caly_ports::RefreshMode,
        ) -> Result<caly_ports::RefreshOutcome, crate::actors::ActorFailure> {
            if self.fail {
                return Err(crate::actors::ActorFailure::infrastructure(
                    "refresh failed",
                    "retry",
                ));
            }
            Ok(caly_ports::RefreshOutcome {
                nodes: caly_domain::SnapshotNodes::try_from_vec(Vec::new())
                    .expect("empty snapshot is a valid snapshot"),
                changed: self.changed,
            })
        }
    }

    fn routed_refresh(op_id: [u8; 16]) -> RoutedCommand {
        RoutedCommand {
            target: CommandTarget::SubscriptionActor,
            envelope: CommandEnvelope {
                operation_id: caly_domain::OperationId::from_bytes(op_id),
                command: Command::RefreshSubscription {
                    subscription_id: caly_domain::SubscriptionId::from_bytes([7; 16]),
                    force: false,
                    scheduled: false,
                },
            },
            cancellation: OperationCancellationToken::new(),
        }
    }

    /// Drives one refresh through the handler; returns the result report and
    /// an event-bus subscription so tests can assert on the published
    /// pipeline fact (刀 2: the handler publishes, the reconciler reacts).
    fn handle_refresh(
        backend: FakeBackend,
    ) -> Result<(ActorReport, std::sync::mpsc::Receiver<DaemonEvent>), String> {
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let bus = EventBus::new();
        let events = bus.subscribe();
        let mut handler = SubscriptionCommandHandler::new(
            backend,
            crate::actor_result::ActorResultClient::new(ingress),
        )
        .with_event_bus(bus);
        let outcome = handler.handle(routed_refresh([1; 16]));
        if !matches!(outcome, Ok(ActorDirective::Continue)) {
            return Err("handler terminated".to_owned());
        }
        let report = receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|_| "no report".to_owned())?;
        Ok((report, events))
    }

    #[test]
    fn changed_refresh_publishes_event() -> Result<(), String> {
        let (report, events) = handle_refresh(FakeBackend {
            changed: true,
            fail: false,
        })?;
        assert!(
            matches!(report, ActorReport::Completed { .. }),
            "refresh must still report completed"
        );
        let event = events
            .recv_timeout(Duration::from_millis(10))
            .map_err(|_| "changed refresh must publish an event".to_owned())?;
        assert!(
            matches!(event, DaemonEvent::SubscriptionRefreshed { changed: true }),
            "changed refresh must publish changed=true, got {event:?}"
        );
        Ok(())
    }

    #[test]
    fn unchanged_refresh_publishes_unchanged_event() -> Result<(), String> {
        let (report, events) = handle_refresh(FakeBackend {
            changed: false,
            fail: false,
        })?;
        assert!(
            matches!(report, ActorReport::Completed { .. }),
            "refresh must still report completed"
        );
        // The reconciler gates the re-render on `changed`; the handler must
        // still publish the fact so consumers can observe the refresh.
        let event = events
            .recv_timeout(Duration::from_millis(10))
            .map_err(|_| "refresh must publish an event".to_owned())?;
        assert!(
            matches!(event, DaemonEvent::SubscriptionRefreshed { changed: false }),
            "unchanged refresh must publish changed=false, got {event:?}"
        );
        Ok(())
    }

    #[test]
    fn failed_refresh_publishes_nothing() -> Result<(), String> {
        let (report, events) = handle_refresh(FakeBackend {
            changed: true,
            fail: true,
        })?;
        assert!(
            matches!(report, ActorReport::Failed { .. }),
            "failed refresh must report a failed operation"
        );
        // The bus drops with the handler at scope end; both Timeout and
        // Disconnected mean no event was published.
        match events.recv_timeout(Duration::from_millis(10)) {
            Err(_) => Ok(()),
            Ok(event) => Err(format!(
                "failed refresh must not publish an event, got {event:?}"
            )),
        }
    }
}
