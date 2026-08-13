//! ConfigActor command handler for `ApplyConfig`: parse+render a candidate,
//! commit it, restart the active core against the new generation, and publish
//! an `AppliedReplaced` carrying the new config generation. On any failure it
//! reports a terminal failure without terminating the actor.

use caly_domain::{AppliedState, CoreKind, CoreRunState, PresentationDelta};

/// Budget for a hot config reload (`PUT /configs?force=true`); a hung kernel
/// control socket must not stall the apply pipeline forever (刀 5).
const HOT_RELOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

use crate::{
    actor_result::ActorResultClient,
    command_bus::Command,
    routing::{CommandTarget, RoutedCommand},
    runtime::{ActorDirective, ActorHandler},
};
use caly_ports::CoreLifecycleCommandBackend;

use super::{
    reporting::{finish_report, HandlerReportError},
    ConfigActorPort, ConfigCandidate, PreparedConfig,
};

/// Owns the config-apply flow over a `ConfigActorPort` backend plus the shared
/// core lifecycle handle used to load the committed generation.
pub struct ConfigCommandHandler<B, L: CoreLifecycleCommandBackend> {
    backend: B,
    lifecycle: L,
    active: caly_ports::SharedActiveCore,
    results: ActorResultClient,
}

impl<B, L: CoreLifecycleCommandBackend> ConfigCommandHandler<B, L> {
    /// Creates a handler owning `backend`, the lifecycle handle and results.
    pub const fn new(
        backend: B,
        lifecycle: L,
        active: caly_ports::SharedActiveCore,
        results: ActorResultClient,
    ) -> Self {
        Self {
            backend,
            lifecycle,
            active,
            results,
        }
    }
}

/// Config command handler error; nothing here should kill the actor.
#[derive(Debug)]
pub enum ConfigCommandHandlerError {
    WrongTarget,
    WrongCommand,
    Report(HandlerReportError),
}

impl<B: ConfigActorPort, L: CoreLifecycleCommandBackend> ActorHandler<RoutedCommand>
    for ConfigCommandHandler<B, L>
{
    type Error = ConfigCommandHandlerError;

    fn handle(&mut self, routed: RoutedCommand) -> Result<ActorDirective, Self::Error> {
        if routed.target != CommandTarget::ConfigActor {
            return Err(ConfigCommandHandlerError::WrongTarget);
        }
        if routed.cancellation.is_cancel_requested() {
            return Ok(ActorDirective::Continue);
        }
        let operation_id = routed.envelope.operation_id;
        let candidate_id = match routed.envelope.command {
            Command::ApplyConfig { candidate_id } => candidate_id,
            Command::ReloadConfig => return self.reload_config(operation_id),
            _ => return Err(ConfigCommandHandlerError::WrongCommand),
        };
        self.apply_and_report(candidate_id, operation_id)
    }
}

impl<B: ConfigActorPort, L: CoreLifecycleCommandBackend> ConfigCommandHandler<B, L> {
    /// `ReloadConfig` drives the full re-apply flow (#42): re-read the current
    /// on-disk layered config, render and validate a fresh candidate, commit
    /// it, and reload the active core — the same transactional path as
    /// `ApplyConfig`, with the operation id doubled as the candidate id so a
    /// reload's generation is traceable back to its RPC.
    fn reload_config(
        &mut self,
        operation_id: caly_domain::OperationId,
    ) -> Result<ActorDirective, ConfigCommandHandlerError> {
        self.apply_and_report(operation_id.into_bytes(), operation_id)
    }

    fn apply_and_report(
        &mut self,
        candidate_id: [u8; 16],
        operation_id: caly_domain::OperationId,
    ) -> Result<ActorDirective, ConfigCommandHandlerError> {
        let outcome = self.apply_config(candidate_id);
        finish_report(&self.results, operation_id, outcome)
            .map_err(ConfigCommandHandlerError::Report)
    }

    /// Renders, validates and commits a candidate, then reloads the active
    /// core so the committed generation takes effect.
    fn apply_config(
        &mut self,
        candidate_id: [u8; 16],
    ) -> Result<Vec<PresentationDelta>, super::ActorFailure> {
        // The active backend is selected by the RUNTIME active core (see
        // `ActiveConfigBackend`), so a `core switch` is honored on apply.
        let candidate = ConfigCandidate { id: candidate_id };
        let prepared = self.backend.parse_and_render(candidate)?;
        // A failed commit must not leave a prepared (unpublished) candidate behind.
        let committed = match self.backend.commit_candidate(prepared) {
            Ok(value) => value,
            Err(failure) => {
                let _ = self.backend.discard_prepared(PreparedConfig {
                    candidate_id,
                    generation: 0,
                });
                return Err(failure);
            }
        };
        let generation = committed.generation;
        // Reload the running core against the committed file. Restart is
        // idempotent (a stopped core is started). A failed reload runs a real
        // transaction: the previous generation is rolled back so a later
        // manual start loads the config the user last saw, never a half-applied
        // candidate. 刀 4 (2026-08-12 pipeline design): a byte-identical
        // render (committed.unchanged) skips the restart entirely — a no-op
        // apply must not drop connections.
        if committed.unchanged {
            tracing::info!(
                generation,
                "config apply: no changes; kernel restart skipped"
            );
        } else {
            // 刀 5 (2026-08-12 pipeline design): prefer a hot reload — push
            // the published config through `PUT /configs?force=true` so
            // existing connections stay up. Only when the kernel has no
            // reload surface (sing-box) or the reload fails do we fall back
            // to a process restart (which also covers a stopped core).
            let hot_reloaded = self.backend.current_contents().is_some_and(|contents| {
                self.lifecycle
                    .hot_reload(&contents, HOT_RELOAD_TIMEOUT)
                    .is_ok()
            });
            if hot_reloaded {
                tracing::info!(
                    generation,
                    "config apply: hot reloaded the kernel without a restart"
                );
            } else if let Err(error) = self.lifecycle.restart() {
                let rollback = self.backend.rollback_commit(committed);
                let mut message = format!(
                    "config apply failed: the core could not reload the new config ({error}); previous generation restored"
                );
                if let Err(rollback_error) = rollback {
                    use std::fmt::Write as _;
                    let _ = write!(message, "; rollback also failed: {rollback_error}");
                }
                return Err(failure(
                    &message,
                    "fix the config and retry `caly config apply`",
                ));
            }
        }
        // Build a running applied state reflecting the newly committed generation.
        let state = AppliedState::new(
            Some(self.active.lock().map_or(CoreKind::Mihomo, |kind| *kind)),
            CoreRunState::Running,
            None,
            Some(generation),
        )
        .map_err(|_| {
            failure(
                "config applied state is invalid",
                "inspect config generation",
            )
        })?;
        Ok(vec![PresentationDelta::AppliedReplaced(state)])
    }
}

fn failure(message: &str, action: &str) -> super::ActorFailure {
    super::ActorFailure::infrastructure(message, action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_result::{actor_result_mailbox, ActorReport};
    use crate::command_bus::CommandEnvelope;
    use crate::operations::OperationCancellationToken;
    use std::time::Duration;

    /// Deterministic ConfigActorPort test double.
    struct FakeConfig {
        fail_commit: bool,
        generation: u64,
        rolled_back: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl crate::actors::ConfigActorPort for FakeConfig {
        fn parse_and_render(
            &mut self,
            candidate: crate::actors::ConfigCandidate,
        ) -> Result<crate::actors::PreparedConfig, crate::actors::ActorFailure> {
            Ok(crate::actors::PreparedConfig {
                candidate_id: candidate.id,
                generation: self.generation,
            })
        }
        fn discard_prepared(
            &mut self,
            _prepared: crate::actors::PreparedConfig,
        ) -> Result<(), crate::actors::ActorFailure> {
            Ok(())
        }
        fn commit_candidate(
            &mut self,
            prepared: crate::actors::PreparedConfig,
        ) -> Result<crate::actors::CommittedConfig, crate::actors::ActorFailure> {
            if self.fail_commit {
                return Err(failure("commit failed", "retry"));
            }
            Ok(crate::actors::CommittedConfig {
                candidate_id: prepared.candidate_id,
                generation: prepared.generation,
                unchanged: false,
            })
        }
        fn rollback_commit(
            &mut self,
            _committed: crate::actors::CommittedConfig,
        ) -> Result<(), crate::actors::ActorFailure> {
            self.rolled_back.set(true);
            Ok(())
        }
    }

    fn handle_apply(
        backend: FakeConfig,
        fail_restart: bool,
    ) -> Result<crate::actor_result::ActorReport, String> {
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let mut handler = ConfigCommandHandler::new(
            backend,
            FakeLifecycle { fail_restart },
            std::sync::Arc::new(std::sync::Mutex::new(caly_domain::CoreKind::Mihomo)),
            crate::actor_result::ActorResultClient::new(ingress),
        );
        let op_id = caly_domain::OperationId::from_bytes([1; 16]);
        let outcome = handler.handle(crate::routing::RoutedCommand {
            target: CommandTarget::ConfigActor,
            envelope: CommandEnvelope {
                operation_id: op_id,
                command: Command::ApplyConfig {
                    candidate_id: [9; 16],
                },
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
    fn apply_config_reports_completed_with_generation() -> Result<(), String> {
        let backend = FakeConfig {
            fail_commit: false,
            generation: 3,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        match handle_apply(backend, false)? {
            ActorReport::Completed { deltas, .. } => {
                assert_eq!(deltas.len(), 1);
                match &deltas[0] {
                    PresentationDelta::AppliedReplaced(state) => {
                        assert_eq!(state.config_generation(), Some(3));
                    }
                    _ => return Err("expected AppliedReplaced".to_owned()),
                }
            }
            _ => return Err("expected completed".to_owned()),
        }
        Ok(())
    }

    #[test]
    fn failed_commit_reports_terminal_failure() -> Result<(), String> {
        let backend = FakeConfig {
            fail_commit: true,
            generation: 1,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        match handle_apply(backend, false)? {
            ActorReport::Failed { .. } => Ok(()),
            _ => Err("expected failed report".to_owned()),
        }
    }

    #[test]
    fn reload_failure_rolls_back_the_committed_generation() -> Result<(), String> {
        let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
        let backend = FakeConfig {
            fail_commit: false,
            generation: 5,
            rolled_back: rolled_back.clone(),
        };
        match handle_apply(backend, true)? {
            ActorReport::Failed { .. } => {}
            _ => return Err("expected failed report after reload failure".to_owned()),
        }
        assert!(
            rolled_back.get(),
            "a failed core reload must roll back the committed generation"
        );
        Ok(())
    }

    #[test]
    fn successful_reload_does_not_roll_back() -> Result<(), String> {
        let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
        let backend = FakeConfig {
            fail_commit: false,
            generation: 2,
            rolled_back: rolled_back.clone(),
        };
        match handle_apply(backend, false)? {
            ActorReport::Completed { .. } => {}
            _ => return Err("expected completed report".to_owned()),
        }
        assert!(!rolled_back.get(), "a successful reload must not roll back");
        Ok(())
    }

    #[test]
    fn sing_box_apply_uses_the_same_render_commit_path() -> Result<(), String> {
        // The configured core selects the active backend; the handler itself
        // no longer fails fast for sing-box (dispatch moved to
        // `ActiveConfigBackend`), so a sing-box apply flows through the same
        // render -> commit -> reload path and reports completed.
        let (ingress, receiver) = actor_result_mailbox(2).map_err(|_| "mailbox failed")?;
        let mut handler = ConfigCommandHandler::new(
            FakeConfig {
                fail_commit: false,
                generation: 4,
                rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
            },
            FakeLifecycle {
                fail_restart: false,
            },
            std::sync::Arc::new(std::sync::Mutex::new(caly_domain::CoreKind::Mihomo)),
            crate::actor_result::ActorResultClient::new(ingress),
        );
        let outcome = handler.handle(crate::routing::RoutedCommand {
            target: CommandTarget::ConfigActor,
            envelope: CommandEnvelope {
                operation_id: caly_domain::OperationId::from_bytes([2; 16]),
                command: Command::ApplyConfig {
                    candidate_id: [9; 16],
                },
            },
            cancellation: OperationCancellationToken::new(),
        });
        if !matches!(outcome, Ok(ActorDirective::Continue)) {
            return Err("handler terminated".to_owned());
        }
        match receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|_| "no report".to_owned())?
        {
            ActorReport::Completed { .. } => Ok(()),
            _ => Err("expected completed report for sing-box apply".to_owned()),
        }
    }

    /// Deterministic lifecycle double recording every reload request.
    struct FakeLifecycle {
        fail_restart: bool,
    }

    impl CoreLifecycleCommandBackend for FakeLifecycle {
        fn start(&mut self) -> Result<caly_domain::AppliedState, crate::actors::ActorFailure> {
            caly_domain::AppliedState::new(
                Some(CoreKind::Mihomo),
                caly_domain::CoreRunState::Running,
                None,
                Some(1),
            )
            .map_err(|_| failure("state", "retry"))
        }
        fn stop(&mut self) -> Result<caly_domain::AppliedState, crate::actors::ActorFailure> {
            caly_domain::AppliedState::new(None, caly_domain::CoreRunState::Stopped, None, None)
                .map_err(|_| failure("state", "retry"))
        }
        fn restart(&mut self) -> Result<caly_domain::AppliedState, crate::actors::ActorFailure> {
            if self.fail_restart {
                return Err(failure("reload failed", "retry"));
            }
            self.start()
        }
    }
}
