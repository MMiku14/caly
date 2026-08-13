//! Core lifecycle command handler for the composition-selected backend.

use caly_domain::PresentationDelta;

use crate::{
    actor_result::ActorResultClient,
    command_bus::{Command, CoreAction},
    routing::{CommandTarget, RoutedCommand},
    runtime::{ActorDirective, ActorHandler},
};

use super::{
    ActorFailure, ActorFailureKind, CoreLifecycleCommandBackend,
    reporting::{HandlerReportError, report_outcome},
};

/// Concrete process lifecycle operations supplied by the kernel backend.
/// CoreLifecycle-owned handler for SwitchCore lifecycle commands.
pub struct CoreLifecycleCommandHandler<B> {
    backend: B,
    results: ActorResultClient,
}

impl<B> CoreLifecycleCommandHandler<B> {
    pub const fn new(backend: B, results: ActorResultClient) -> Self {
        Self { backend, results }
    }
}

#[derive(Debug)]
pub enum CoreLifecycleHandlerError {
    WrongTarget,
    WrongCommand,
    Report(HandlerReportError),
}

impl<B: CoreLifecycleCommandBackend> ActorHandler<RoutedCommand>
    for CoreLifecycleCommandHandler<B>
{
    type Error = CoreLifecycleHandlerError;

    fn handle(&mut self, routed: RoutedCommand) -> Result<ActorDirective, Self::Error> {
        if routed.target != CommandTarget::CoreLifecycle {
            return Err(CoreLifecycleHandlerError::WrongTarget);
        }
        if routed.cancellation.is_cancel_requested() {
            return Ok(ActorDirective::Continue);
        }
        let operation_id = routed.envelope.operation_id;
        let result = match routed.envelope.command {
            // SwitchCore targets a kernel: Start/Restart on a non-active kernel
            // switches the active core (stop previous, start target); Stop
            // always stops the active kernel.
            Command::SwitchCore { target, action } => match (target, action) {
                (caly_domain::CoreKind::Xray, _) => Err(unsupported_failure(
                    "Xray lifecycle is not implemented",
                    "select Mihomo or sing-box until the Xray backend is complete",
                )),
                (_target, CoreAction::Stop) => self.backend.stop(),
                (target, CoreAction::Start | CoreAction::Restart) => self.backend.switch_to(target),
            },
            Command::ApplyConfig { .. } | Command::SetMode { .. } | Command::SetTun { .. } => {
                // Defense in depth: admission currently rejects these commands.
                // If an internal caller bypasses policy, report terminal failure
                // instead of terminating the coordinator actor.
                Err(unsupported_failure(
                    "coordinator command is not wired into the runtime",
                    "use a currently advertised command and inspect daemon capabilities",
                ))
            }
            Command::StopDaemon => {
                // Round 17: `daemon stop` is a runtime-level
                // event. The handler reports the current
                // applied state (no mutation), then the
                // daemon runtime observes the completed
                // operation and breaks its `serve()` loop
                // (see `daemon.rs`). The `AppliedState` is
                // fetched from the lifecycle backend's
                // view (the same state `Stop` would
                // produce), so the client's `status` call
                // after `Stop` reflects the post-shutdown
                // intent.
                self.backend.stop()
            }
            Command::SelectProxy { .. }
            | Command::SelectProxyGroup { .. }
            | Command::SetSystemProxy { .. }
            | Command::SetSystemProxyPac { .. }
            | Command::RefreshSubscription { .. }
            | Command::CloseAllConnections
            | Command::ReloadConfig => {
                return Err(CoreLifecycleHandlerError::WrongCommand);
            }
        };
        let outcome = result.map(|state| vec![PresentationDelta::AppliedReplaced(state)]);
        report_outcome(&self.results, operation_id, outcome)
            .map_err(CoreLifecycleHandlerError::Report)?;
        Ok(ActorDirective::Continue)
    }
}

fn unsupported_failure(message: &'static str, action: &'static str) -> ActorFailure {
    // The inputs here are module-level `&'static str` literals that always
    // fit the bounded message size, but we still use the infallible `clamped`
    // constructor to keep the call site abort-free: the previous
    // `unwrap_or_else(|_| std::process::abort())` form would have aborted the
    // daemon if a future refactor passes a dynamic string that happens to
    // exceed 512 bytes (e.g. an interpolated OS error).
    ActorFailure::clamped(ActorFailureKind::Unsupported, message, action)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        actor_result::{ActorReport, ActorResultClient, actor_result_mailbox},
        command_bus::CommandEnvelope,
        routing::CommandTarget,
    };
    use caly_domain::{AppliedState, OperationFailureCode, OperationId};

    struct UnusedBackend;

    impl CoreLifecycleCommandBackend for UnusedBackend {
        fn start(&mut self) -> Result<AppliedState, ActorFailure> {
            Err(unsupported_failure(
                "unexpected backend call",
                "inspect test",
            ))
        }

        fn stop(&mut self) -> Result<AppliedState, ActorFailure> {
            Err(unsupported_failure(
                "unexpected backend call",
                "inspect test",
            ))
        }

        fn restart(&mut self) -> Result<AppliedState, ActorFailure> {
            Err(unsupported_failure(
                "unexpected backend call",
                "inspect test",
            ))
        }
    }

    fn assert_terminal_unsupported(command: Command) -> Result<(), &'static str> {
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let mut handler =
            CoreLifecycleCommandHandler::new(UnusedBackend, ActorResultClient::new(ingress));
        let operation_id = OperationId::from_bytes([7; 16]);
        let outcome = handler.handle(RoutedCommand {
            target: CommandTarget::CoreLifecycle,
            envelope: CommandEnvelope {
                operation_id,
                command,
            },
            cancellation: crate::operations::OperationCancellationToken::new(),
        });
        if !matches!(outcome, Ok(ActorDirective::Continue)) {
            return Err("handler terminated instead of reporting a failure");
        }
        let report = receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|_| "terminal report missing")?;
        match report {
            ActorReport::Failed {
                operation_id: reported,
                failure,
                ..
            } if reported == operation_id
                && failure.code() == OperationFailureCode::Unsupported =>
            {
                Ok(())
            }
            _ => Err("expected terminal unsupported report"),
        }
    }

    #[test]
    fn cancelled_routed_command_never_reaches_backend() -> Result<(), &'static str> {
        let (ingress, receiver) = actor_result_mailbox(1).map_err(|_| "mailbox failed")?;
        let mut handler =
            CoreLifecycleCommandHandler::new(UnusedBackend, ActorResultClient::new(ingress));
        let cancellation = crate::operations::OperationCancellationToken::new();
        assert_eq!(
            cancellation.request_cancel(),
            crate::operations::CancellationSignal::Requested
        );
        let outcome = handler.handle(RoutedCommand {
            target: CommandTarget::CoreLifecycle,
            envelope: CommandEnvelope {
                operation_id: OperationId::from_bytes([6; 16]),
                command: Command::SwitchCore {
                    target: caly_domain::CoreKind::Mihomo,
                    action: CoreAction::Start,
                },
            },
            cancellation,
        });
        assert!(matches!(outcome, Ok(ActorDirective::Continue)));
        assert!(matches!(
            receiver.receive_timeout(Duration::from_millis(1)),
            Err(crate::runtime::MailboxReceiveError::TimedOut)
        ));
        Ok(())
    }

    #[test]
    fn contract_only_coordinator_command_does_not_kill_actor() -> Result<(), &'static str> {
        assert_terminal_unsupported(Command::SetTun { enabled: true })
    }

    #[test]
    fn xray_lifecycle_does_not_kill_actor() -> Result<(), &'static str> {
        assert_terminal_unsupported(Command::SwitchCore {
            target: caly_domain::CoreKind::Xray,
            action: CoreAction::Start,
        })
    }
}
