//! Runtime ownership, cancellation and ordered shutdown.

mod actor_loop;
mod cancellation;
mod fatal;
mod shutdown;
mod shutdown_runner;
mod task_group;
mod tokio_executor;

pub use actor_loop::{
    ActorDirective, ActorHandler, ActorIngress, ActorLoopExit, ActorReceiver,
    InvalidMailboxCapacity, MailboxReceiveError, MailboxSendError, actor_mailbox, run_actor,
};
pub use cancellation::CancellationToken;
pub use fatal::{FatalFault, FatalRecordOutcome, RuntimeGuard};
pub use shutdown::{ShutdownComplete, ShutdownDriver, ShutdownPhase};
pub use shutdown_runner::{
    MAX_SHUTDOWN_FAILURES, ShutdownActions, ShutdownFailure, ShutdownFailures, ShutdownRunError,
    run_shutdown,
};
pub use task_group::{MAX_OWNED_TASKS, TaskName};
pub use tokio_executor::{TokioSpawnError, TokioTaskFailure, TokioTaskGroup};
