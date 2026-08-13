//! Core lifecycle command port.

use std::time::Duration;

use caly_domain::{AppliedState, CoreKind};

use super::error::ActorFailure;

/// Concrete process lifecycle operations supplied by the kernel backend.
pub trait CoreLifecycleCommandBackend {
    fn start(&mut self) -> Result<AppliedState, ActorFailure>;
    fn stop(&mut self) -> Result<AppliedState, ActorFailure>;
    fn restart(&mut self) -> Result<AppliedState, ActorFailure>;

    /// Hot-reloads the active core's config without restarting the process
    /// (刀 5, 2026-08-12 pipeline design): the published config text is
    /// pushed through the kernel's reload surface so existing connections
    /// stay up. Default: unsupported — callers fall back to a restart.
    fn hot_reload(&self, config: &[u8], timeout: Duration) -> Result<(), String> {
        let _ = (config, timeout);
        Err("kernel has no hot reload surface; falling back to a restart".to_owned())
    }

    /// Switches the active core to `target`, stopping the previous kernel.
    /// Single-core backends fall back to a restart of the active kernel.
    fn switch_to(&mut self, target: CoreKind) -> Result<AppliedState, ActorFailure> {
        let _ = target;
        self.restart()
    }
}
