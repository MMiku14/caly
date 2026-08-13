//! TelemetryActor mailbox commands.

/// Telemetry permits explicit coalescing but never invisible loss.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryActorCommand {
    Sample,
    RecordDropped { count: u64 },
    ResetGeneration { generation: u64 },
    Shutdown,
}
