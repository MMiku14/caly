//! Single-owner process-tree lifecycle boundary.

pub mod linux;

pub use linux::{LinuxOwnedProcessTree, LinuxProcessSpawner};

use std::{path::PathBuf, time::Duration};

use caly_domain::{BoundedText, BoundedVec};

use crate::PlatformFailure;

/// Maximum argument count for a managed process.
pub const MAX_PROCESS_ARGUMENTS: usize = 128;
/// Maximum one argument length.
pub type ProcessArgument = BoundedText<4_096>;
/// Bounded process arguments; no shell command string exists.
pub type ProcessArguments = BoundedVec<ProcessArgument, MAX_PROCESS_ARGUMENTS>;

/// Platform-neutral spawn request.
pub struct SpawnSpec {
    pub executable: PathBuf,
    pub arguments: ProcessArguments,
    pub working_directory: PathBuf,
    pub kill_on_owner_drop: bool,
    /// Human-readable kernel name (`mihomo` / `sing-box`) used to mark the
    /// child's stdout/stderr lines when they are forwarded into the daemon
    /// log as `[kernel:{label}]` lines (2026-08-12 daemon-log audit).
    pub label: String,
}

/// Required OS process-tree containment mechanism.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessContainment {
    UnixProcessGroup,
    WindowsJobObject,
}

/// The sole owner of a spawned process tree.
pub trait OwnedProcessTree: Send {
    fn containment(&self) -> ProcessContainment;
    fn generation(&self) -> u64;
    fn stop_gracefully(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ProcessExit>, PlatformFailure>;
    fn force_kill_tree(&mut self) -> Result<(), PlatformFailure>;
    fn poll_exit(&mut self) -> Result<Option<ProcessExit>, PlatformFailure>;
    fn wait_reaped(&mut self) -> Result<ProcessExit, PlatformFailure>;

    /// Captured tail of the child's stderr (bounded, may be empty). The Linux
    /// backend pipes the core's stderr so a process that exits before becoming
    /// ready can report the real failure reason (e.g. a TUN route conflict)
    /// instead of a generic timeout. Backends that do not capture stderr return
    /// an empty tail by default.
    fn stderr_tail(&self) -> String {
        String::new()
    }
}

/// Process termination observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    pub code: Option<i32>,
    pub signalled: bool,
}

/// OS backend that creates exactly one owned process-tree handle.
pub trait ProcessSpawner {
    type Tree: OwnedProcessTree;
    fn spawn_owned(
        &mut self,
        spec: SpawnSpec,
        generation: u64,
    ) -> Result<Self::Tree, PlatformFailure>;
}

/// Stops and reaps a whole process tree without dropping either error.
pub fn stop_and_reap(
    tree: &mut impl OwnedProcessTree,
    graceful_timeout: Duration,
) -> Result<ProcessExit, StopTreeFailure> {
    match tree.stop_gracefully(graceful_timeout) {
        Ok(Some(exit)) => return Ok(exit),
        Ok(None) => {}
        Err(graceful) => return force_and_reap(tree, Some(graceful)),
    }
    force_and_reap(tree, None)
}

fn force_and_reap(
    tree: &mut impl OwnedProcessTree,
    graceful: Option<PlatformFailure>,
) -> Result<ProcessExit, StopTreeFailure> {
    let forced = tree.force_kill_tree().err();
    match tree.wait_reaped() {
        Ok(exit) if graceful.is_none() && forced.is_none() => Ok(exit),
        Ok(_) => Err(StopTreeFailure {
            graceful,
            forced,
            reap: None,
        }),
        Err(reap) => Err(StopTreeFailure {
            graceful,
            forced,
            reap: Some(reap),
        }),
    }
}

/// Every failed lifecycle step is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StopTreeFailure {
    pub graceful: Option<PlatformFailure>,
    pub forced: Option<PlatformFailure>,
    pub reap: Option<PlatformFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree {
        graceful_exit: Option<ProcessExit>,
        calls: Vec<&'static str>,
    }

    impl OwnedProcessTree for Tree {
        fn containment(&self) -> ProcessContainment {
            ProcessContainment::UnixProcessGroup
        }

        fn generation(&self) -> u64 {
            1
        }

        fn stop_gracefully(
            &mut self,
            _timeout: Duration,
        ) -> Result<Option<ProcessExit>, PlatformFailure> {
            self.calls.push("graceful");
            Ok(self.graceful_exit)
        }

        fn force_kill_tree(&mut self) -> Result<(), PlatformFailure> {
            self.calls.push("kill-tree");
            Ok(())
        }

        fn poll_exit(&mut self) -> Result<Option<ProcessExit>, PlatformFailure> {
            Ok(self.graceful_exit)
        }

        fn wait_reaped(&mut self) -> Result<ProcessExit, PlatformFailure> {
            self.calls.push("reap");
            Ok(ProcessExit {
                code: None,
                signalled: true,
            })
        }
    }

    #[test]
    fn graceful_timeout_forces_tree_kill_and_reap() -> Result<(), StopTreeFailure> {
        let mut tree = Tree {
            graceful_exit: None,
            calls: Vec::new(),
        };
        let exit = stop_and_reap(&mut tree, Duration::from_millis(1))?;
        assert!(exit.signalled);
        assert_eq!(tree.calls, vec!["graceful", "kill-tree", "reap"]);
        Ok(())
    }

    #[test]
    fn graceful_exit_skips_force_kill() -> Result<(), StopTreeFailure> {
        let expected = ProcessExit {
            code: Some(0),
            signalled: false,
        };
        let mut tree = Tree {
            graceful_exit: Some(expected),
            calls: Vec::new(),
        };
        assert_eq!(stop_and_reap(&mut tree, Duration::from_secs(1))?, expected);
        assert_eq!(tree.calls, vec!["graceful"]);
        Ok(())
    }
}
