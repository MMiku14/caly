//! SubscriptionActor mailbox commands.

/// Subscription ownership requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionActorCommand {
    Refresh {
        operation: caly_domain::OperationId,
        subscription: caly_domain::SubscriptionId,
        generation: u64,
    },
    Remove {
        operation: caly_domain::OperationId,
        subscription: caly_domain::SubscriptionId,
        generation: u64,
    },
    Shutdown,
}
