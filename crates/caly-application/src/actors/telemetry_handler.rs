//! TelemetryActor mailbox handler.

use crate::runtime::{ActorDirective, ActorHandler};

use super::{ActorFailure, TelemetryActorCommand, TelemetryCommandBackend};

pub struct TelemetryCommandHandler<B>(pub B);

impl<B: TelemetryCommandBackend> ActorHandler<TelemetryActorCommand>
    for TelemetryCommandHandler<B>
{
    type Error = ActorFailure;

    fn handle(&mut self, command: TelemetryActorCommand) -> Result<ActorDirective, Self::Error> {
        match command {
            TelemetryActorCommand::Sample => self.0.sample()?,
            TelemetryActorCommand::RecordDropped { count } => self.0.record_dropped(count)?,
            TelemetryActorCommand::ResetGeneration { generation } => {
                self.0.reset_generation(generation)?;
            }
            TelemetryActorCommand::Shutdown => return Ok(ActorDirective::Stop),
        }
        Ok(ActorDirective::Continue)
    }
}
