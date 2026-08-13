//! PlatformActor system-side-effect command handler.

use caly_domain::PresentationDelta;

use crate::{
    actor_result::ActorResultClient,
    command_bus::Command,
    routing::{CommandTarget, RoutedCommand},
    runtime::{ActorDirective, ActorHandler},
};

use super::{
    PlatformCommandBackend, TunCommandBackend,
    reporting::{HandlerReportError, report_outcome},
};

/// Nonblocking PlatformActor backend owning recovery transactions and the TUN
/// lifecycle (system-proxy via `backend`, TUN via `tun`).
pub struct PlatformCommandHandler<B, T> {
    backend: B,
    tun: T,
    desired: caly_ports::SharedDesiredState,
    results: ActorResultClient,
}

impl<B, T> PlatformCommandHandler<B, T> {
    pub const fn new(
        backend: B,
        tun: T,
        desired: caly_ports::SharedDesiredState,
        results: ActorResultClient,
    ) -> Self {
        Self {
            backend,
            tun,
            desired,
            results,
        }
    }
}

#[derive(Debug)]
pub enum PlatformHandlerError {
    WrongTarget,
    WrongCommand,
    Report(HandlerReportError),
}

impl<B: PlatformCommandBackend, T: TunCommandBackend> ActorHandler<RoutedCommand>
    for PlatformCommandHandler<B, T>
{
    type Error = PlatformHandlerError;

    fn handle(&mut self, routed: RoutedCommand) -> Result<ActorDirective, Self::Error> {
        if routed.target != CommandTarget::PlatformActor {
            return Err(PlatformHandlerError::WrongTarget);
        }
        if routed.cancellation.is_cancel_requested() {
            return Ok(ActorDirective::Continue);
        }
        let operation_id = routed.envelope.operation_id;
        match routed.envelope.command {
            Command::SetSystemProxy { enabled } => {
                let outcome = self.backend.set_system_proxy(enabled);
                // Commit the intent to the shared cell only on success: an
                // unconditional replace lets a failed command pollute the
                // cell, and the phantom flag leaks into the next successful
                // command's DesiredReplaced delta (`tun on` failing would
                // make a later `sysproxy on` publish tun_requested=true).
                let desired = self.commit_desired(&outcome, |d| d.with_system_proxy(enabled));
                self.finish(operation_id, outcome, desired)
            }
            Command::SetSystemProxyPac { url } => {
                let outcome = self.backend.set_system_proxy_pac(&url);
                // PAC mode engages the system proxy; the intent flag
                // stays the same boolean so `sysproxy status` reports
                // the proxy as engaged.
                let desired = self.commit_desired(&outcome, |d| d.with_system_proxy(true));
                self.finish(operation_id, outcome, desired)
            }
            Command::SetTun { enabled } => {
                let outcome = self.tun.set_tun(enabled);
                let desired = self.commit_desired(&outcome, |d| d.with_tun_requested(enabled));
                self.finish(operation_id, outcome, desired)
            }
            _ => return Err(PlatformHandlerError::WrongCommand),
        }
        .map_err(PlatformHandlerError::Report)?;
        Ok(ActorDirective::Continue)
    }
}

impl<B: PlatformCommandBackend, T: TunCommandBackend> PlatformCommandHandler<B, T> {
    /// Commits an intent update to the shared desired cell, but only when the
    /// backend accepted the command.
    ///
    /// On failure the cell is left untouched and the caller publishes no
    /// `DesiredReplaced` delta (the failure outcome carries none), so the
    /// projection can never display an intent that never engaged.
    fn commit_desired(
        &mut self,
        outcome: &Result<caly_domain::PlatformEffectView, super::ActorFailure>,
        update: impl FnOnce(caly_domain::DesiredState) -> caly_domain::DesiredState,
    ) -> caly_domain::DesiredState {
        match outcome {
            Ok(_) => self.desired.replace(update(self.desired.value())),
            Err(_) => self.desired.value(),
        }
    }

    fn finish(
        &mut self,
        operation_id: caly_domain::OperationId,
        outcome: Result<caly_domain::PlatformEffectView, super::ActorFailure>,
        desired: caly_domain::DesiredState,
    ) -> Result<(), HandlerReportError> {
        report_outcome(
            &self.results,
            operation_id,
            // Publish both the user intent (Desired) and the acknowledged
            // platform slice so `status` shows what the user asked for, not
            // just what engaged (mirrors the mode owner's dual delta).
            outcome.map(|state| {
                vec![
                    PresentationDelta::DesiredReplaced(desired),
                    PresentationDelta::PlatformReplaced(state),
                ]
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_result::{ActorReport, actor_result_mailbox};
    use crate::command_bus::CommandEnvelope;
    use crate::operations::OperationCancellationToken;
    use std::time::Duration;

    struct FakeProxy {
        fail: bool,
    }
    impl PlatformCommandBackend for FakeProxy {
        fn set_system_proxy_pac(
            &mut self,
            _url: &str,
        ) -> Result<caly_domain::PlatformEffectView, super::super::ActorFailure> {
            self.set_system_proxy(true)
        }

        fn set_system_proxy(
            &mut self,
            _enabled: bool,
        ) -> Result<caly_domain::PlatformEffectView, super::super::ActorFailure> {
            if self.fail {
                Err(super::super::ActorFailure::clamped(
                    super::super::ActorFailureKind::Infrastructure,
                    "proxy failed",
                    "retry",
                ))
            } else {
                Ok(caly_domain::PlatformEffectView::proxy(true))
            }
        }
    }

    struct FakeTun {
        fail: bool,
        engaged: bool,
    }
    impl TunCommandBackend for FakeTun {
        fn set_tun(
            &mut self,
            enabled: bool,
        ) -> Result<caly_domain::PlatformEffectView, super::super::ActorFailure> {
            if self.fail {
                return Err(super::super::ActorFailure::clamped(
                    super::super::ActorFailureKind::Infrastructure,
                    "tun unavailable",
                    "require CAP_NET_ADMIN and /dev/net/tun",
                ));
            }
            self.engaged = enabled;
            Ok(caly_domain::PlatformEffectView::tun(enabled))
        }
    }

    fn handle(command: crate::command_bus::Command) -> Result<ActorReport, String> {
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let mut handler = PlatformCommandHandler::new(
            FakeProxy { fail: false },
            FakeTun {
                fail: false,
                engaged: false,
            },
            caly_ports::SharedDesiredState::default(),
            crate::actor_result::ActorResultClient::new(ingress),
        );
        let op_id = caly_domain::OperationId::from_bytes([5; 16]);
        let outcome = handler.handle(crate::routing::RoutedCommand {
            target: CommandTarget::PlatformActor,
            envelope: CommandEnvelope {
                operation_id: op_id,
                command,
            },
            cancellation: OperationCancellationToken::new(),
        });
        if !matches!(outcome, Ok(ActorDirective::Continue)) {
            return Err("handler terminated".to_owned());
        }
        receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|_| "no report".to_owned())
    }

    #[test]
    fn set_tun_reports_completed_with_engaged_state() -> Result<(), String> {
        match handle(Command::SetTun { enabled: true })? {
            ActorReport::Completed { deltas, .. } => {
                assert_eq!(deltas.len(), 2, "Desired + Platform deltas expected");
                match (&deltas[0], &deltas[1]) {
                    (
                        PresentationDelta::DesiredReplaced(desired),
                        PresentationDelta::PlatformReplaced(state),
                    ) => {
                        assert!(desired.tun_requested(), "TUN intent must be recorded");
                        assert!(state.tun_engaged(), "TUN must be engaged");
                    }
                    _ => return Err("expected DesiredReplaced + PlatformReplaced".to_owned()),
                }
            }
            _ => return Err("expected completed".to_owned()),
        }
        Ok(())
    }

    #[test]
    fn failed_tun_reports_terminal_failure() -> Result<(), String> {
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let mut handler = PlatformCommandHandler::new(
            FakeProxy { fail: false },
            FakeTun {
                fail: true,
                engaged: false,
            },
            caly_ports::SharedDesiredState::default(),
            crate::actor_result::ActorResultClient::new(ingress),
        );
        let op_id = caly_domain::OperationId::from_bytes([6; 16]);
        handler
            .handle(crate::routing::RoutedCommand {
                target: CommandTarget::PlatformActor,
                envelope: CommandEnvelope {
                    operation_id: op_id,
                    command: Command::SetTun { enabled: true },
                },
                cancellation: OperationCancellationToken::new(),
            })
            .map_err(|_| "handler terminated")?;
        match receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|_| "no report")?
        {
            ActorReport::Failed { .. } => Ok(()),
            _ => Err("expected failed report".to_owned()),
        }
    }

    #[test]
    fn failed_command_does_not_pollute_desired_cell() -> Result<(), String> {
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let mut handler = PlatformCommandHandler::new(
            FakeProxy { fail: false },
            FakeTun {
                fail: true,
                engaged: false,
            },
            caly_ports::SharedDesiredState::default(),
            crate::actor_result::ActorResultClient::new(ingress),
        );
        let op_id = caly_domain::OperationId::from_bytes([7; 16]);
        // `tun on` fails: the shared cell must not record the phantom intent
        // (the pre-fix unconditional replace polluted the cell and the flag
        // leaked into the next successful command's DesiredReplaced).
        handler
            .handle(crate::routing::RoutedCommand {
                target: CommandTarget::PlatformActor,
                envelope: CommandEnvelope {
                    operation_id: op_id,
                    command: Command::SetTun { enabled: true },
                },
                cancellation: OperationCancellationToken::new(),
            })
            .map_err(|_| "handler terminated")?;
        matches!(
            receiver
                .receive_timeout(Duration::from_millis(10))
                .map_err(|_| "no report")?,
            ActorReport::Failed { .. }
        );
        assert!(!handler.desired.value().tun_requested());
        assert!(!handler.desired.value().system_proxy_requested());

        // A subsequent successful `sysproxy on` must publish a DesiredReplaced
        // without the phantom TUN flag.
        let op_id = caly_domain::OperationId::from_bytes([8; 16]);
        handler
            .handle(crate::routing::RoutedCommand {
                target: CommandTarget::PlatformActor,
                envelope: CommandEnvelope {
                    operation_id: op_id,
                    command: Command::SetSystemProxy { enabled: true },
                },
                cancellation: OperationCancellationToken::new(),
            })
            .map_err(|_| "handler terminated")?;
        match receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|_| "no report")?
        {
            ActorReport::Completed { deltas, .. } => match &deltas[0] {
                PresentationDelta::DesiredReplaced(desired) => {
                    assert!(desired.system_proxy_requested());
                    assert!(!desired.tun_requested(), "phantom TUN flag leaked");
                    Ok(())
                }
                _ => Err("expected DesiredReplaced first".to_owned()),
            },
            _ => Err("expected completed report".to_owned()),
        }
    }
}
