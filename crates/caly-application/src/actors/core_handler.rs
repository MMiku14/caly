//! CoreActor command handler with truthful terminal reporting.

use caly_domain::PresentationDelta;

use crate::{
    actor_result::ActorResultClient,
    command_bus::Command,
    routing::{CommandTarget, RoutedCommand},
    runtime::{ActorDirective, ActorHandler},
};

use super::{
    reporting::{finish_report, HandlerReportError},
    CoreCommandBackend,
};

/// Nonblocking CoreActor backend; process/API I/O belongs to its owned worker.
pub struct CoreCommandHandler<B> {
    backend: B,
    results: ActorResultClient,
    desired: caly_ports::SharedDesiredState,
}

impl<B> CoreCommandHandler<B> {
    /// Creates a handler owning `backend`, the result client, and the shared
    /// desired-state cell used to build Desired/Applied deltas.
    pub const fn new(
        backend: B,
        results: ActorResultClient,
        desired: caly_ports::SharedDesiredState,
    ) -> Self {
        Self {
            backend,
            results,
            desired,
        }
    }
}

#[derive(Debug)]
pub enum CoreHandlerError {
    WrongTarget,
    WrongCommand,
    Report(HandlerReportError),
}

impl<B: CoreCommandBackend> ActorHandler<RoutedCommand> for CoreCommandHandler<B> {
    type Error = CoreHandlerError;

    fn handle(&mut self, routed: RoutedCommand) -> Result<ActorDirective, Self::Error> {
        if routed.target != CommandTarget::CoreActor {
            return Err(CoreHandlerError::WrongTarget);
        }
        if routed.cancellation.is_cancel_requested() {
            return Ok(ActorDirective::Continue);
        }
        let operation_id = routed.envelope.operation_id;
        let outcome = match routed.envelope.command {
            Command::SelectProxy { node_id } => self.backend.select_proxy(node_id).map(|state| {
                refresh_groups(
                    &mut self.backend,
                    vec![PresentationDelta::AppliedReplaced(state)],
                )
            }),
            // W4 (`node pick`): in-group member selection routes to
            // the same CoreActor; the backend resolves kernel tags.
            Command::SelectProxyGroup { group, member } => self
                .backend
                .select_proxy_group(&group, &member)
                .map(|state| {
                    refresh_groups(
                        &mut self.backend,
                        vec![PresentationDelta::AppliedReplaced(state)],
                    )
                }),
            Command::CloseAllConnections => self
                .backend
                .close_all_connections()
                .map(|state| vec![PresentationDelta::ObservedReplaced(state)]),
            Command::SetMode { mode } => {
                // Publish both the user-intent (Desired) and the acknowledged
                // (Applied) slice so the projection reflects a coherent mode.
                let current = self.desired.value();
                let new_desired = self.desired.replace(current.with_mode(mode));
                self.backend.set_mode(mode).map(|state| {
                    vec![
                        PresentationDelta::DesiredReplaced(new_desired),
                        PresentationDelta::AppliedReplaced(state),
                    ]
                })
            }
            _ => return Err(CoreHandlerError::WrongCommand),
        };
        finish_report(&self.results, operation_id, outcome).map_err(CoreHandlerError::Report)
    }
}
/// W3b: after a selection the kernel-side group state may have changed
/// (`now` moved, membership re-derived) — refresh the proxy-group slice
/// so the projection's GROUP column and group rows stay current.
/// Best-effort: a listing failure or an empty slice keeps the previous
/// projection (the boot-seeded declared groups stay authoritative).
fn refresh_groups<B: CoreCommandBackend>(
    backend: &mut B,
    mut deltas: Vec<PresentationDelta>,
) -> Vec<PresentationDelta> {
    match backend.list_proxy_groups(std::time::Duration::from_secs(2)) {
        Ok(groups) if !groups.is_empty() => {
            deltas.push(PresentationDelta::GroupsReplaced(groups));
            deltas
        }
        _ => deltas,
    }
}
