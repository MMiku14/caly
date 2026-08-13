//! PlatformActor mailbox commands.

/// Platform side-effect requests; concrete snapshots stay in Infrastructure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformActorCommand {
    EngageSystemProxy { operation: caly_domain::OperationId },
    RestoreSystemProxy { operation: caly_domain::OperationId },
    EngageTun { operation: caly_domain::OperationId },
    RestoreTun { operation: caly_domain::OperationId },
    RecoverPendingEffects,
    Shutdown,
}
