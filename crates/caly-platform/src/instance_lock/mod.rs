//! Owner-token single-instance lock contract.

pub mod identity;
pub mod linux;
mod reclaim;

pub use identity::{current_process_start_id, process_start_id};
pub use linux::{LinuxInstanceLock, LinuxInstanceLockBackend};

use std::path::PathBuf;

use crate::PlatformFailure;

/// Lock identity robust against PID reuse when backend supplies process start identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockOwner {
    pub pid: u32,
    pub process_start_id: u64,
    pub owner_token: [u8; 16],
}

/// Owned lock that removes only its own token on release.
pub trait InstanceLock: Send {
    fn owner(&self) -> LockOwner;
    fn release(self: Box<Self>) -> Result<(), PlatformFailure>;
}

/// OS lock acquisition backend.
pub trait InstanceLockBackend {
    fn acquire(
        &mut self,
        path: PathBuf,
        owner: LockOwner,
    ) -> Result<Box<dyn InstanceLock>, PlatformFailure>;
}
