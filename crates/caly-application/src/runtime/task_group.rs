//! Owned task name plumbing.

/// Maximum long-lived tasks owned by one daemon runtime.
pub const MAX_OWNED_TASKS: usize = 64;
/// Owned task name.
pub type TaskName = caly_domain::BoundedText<64>;
