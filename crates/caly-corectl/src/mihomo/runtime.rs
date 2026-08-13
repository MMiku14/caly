//! Mihomo process lifecycle, readiness and abnormal-exit reporting.

use std::time::Duration;

use caly_domain::BoundedText;
use caly_platform::{
    PlatformFailure,
    process::{OwnedProcessTree, ProcessExit, ProcessSpawner, stop_and_reap},
};

use crate::common::{failure, wait_ready_detecting_exit};
use crate::contract::{KernelControl, KernelFailure, RenderedConfigRef, SpawnSpecFactory};

/// Runtime lifecycle failure retaining the failed phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MihomoRuntimeError {
    Kernel(KernelFailure),
    Platform(PlatformFailure),
    Stop(PlatformFailure),
}

impl core::fmt::Display for MihomoRuntimeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel(error) => {
                write!(formatter, "{} ({})", error.message, error.suggested_action)
            }
            Self::Platform(error) | Self::Stop(error) => {
                write!(formatter, "{} ({})", error.message, error.suggested_action)
            }
        }
    }
}

/// Runtime event emitted when the owned Mihomo process exits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MihomoRuntimeEvent {
    Exited { generation: u64, exit: ProcessExit },
}

/// Owns one Mihomo process tree and its kernel API control port.
pub struct MihomoRuntime<S: ProcessSpawner, C> {
    factory: MihomoSpawnSpecFactory,
    spawner: S,
    control: C,
    tree: Option<S::Tree>,
    generation: Option<u64>,
}

use super::MihomoSpawnSpecFactory;

impl<S, C> MihomoRuntime<S, C>
where
    S: ProcessSpawner,
    C: KernelControl,
{
    /// Creates a stopped Mihomo runtime.
    pub const fn new(factory: MihomoSpawnSpecFactory, spawner: S, control: C) -> Self {
        Self {
            factory,
            spawner,
            control,
            tree: None,
            generation: None,
        }
    }

    /// Starts Mihomo and waits for the API control port to become ready.
    pub fn start(
        &mut self,
        config: &RenderedConfigRef,
        generation: u64,
        timeout: Duration,
    ) -> Result<(), MihomoRuntimeError> {
        crate::common::ensure_executable(self.factory.binary(), "Mihomo")
            .map_err(MihomoRuntimeError::Kernel)?;
        if self.tree.is_some() {
            return Err(MihomoRuntimeError::Kernel(failure(
                "Mihomo is already running",
                "stop the current Mihomo process before starting another generation",
            )));
        }
        let spec = self
            .factory
            .build_spawn_spec(config)
            .map_err(MihomoRuntimeError::Kernel)?;
        let mut tree = self
            .spawner
            .spawn_owned(spec, generation)
            .map_err(MihomoRuntimeError::Platform)?;
        if let Err(error) =
            wait_ready_detecting_exit::<S, C>(&mut tree, &mut self.control, timeout, "Mihomo")
        {
            let stop_error = stop_and_reap(&mut tree, timeout).err();
            return Err(
                stop_error.map_or(MihomoRuntimeError::Kernel(error), |failure| {
                    MihomoRuntimeError::Stop(platform_from_stop(failure))
                }),
            );
        }
        self.tree = Some(tree);
        self.generation = Some(generation);
        Ok(())
    }

    /// Performs a bounded Mihomo API health check.
    pub fn health_check(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        self.control.health_check(timeout)
    }

    /// Stops and reaps Mihomo, preserving lifecycle errors.
    pub fn stop(&mut self, timeout: Duration) -> Result<ProcessExit, MihomoRuntimeError> {
        let mut tree = self.tree.take().ok_or_else(|| {
            MihomoRuntimeError::Kernel(failure(
                "Mihomo is not running",
                "start Mihomo before requesting stop",
            ))
        })?;
        self.generation = None;
        stop_and_reap(&mut tree, timeout)
            .map_err(|failure| MihomoRuntimeError::Stop(platform_from_stop(failure)))
    }

    /// Stops the current generation, then starts the requested generation.
    pub fn restart(
        &mut self,
        config: &RenderedConfigRef,
        generation: u64,
        timeout: Duration,
    ) -> Result<(), MihomoRuntimeError> {
        crate::common::ensure_executable(self.factory.binary(), "Mihomo")
            .map_err(MihomoRuntimeError::Kernel)?;
        if self.tree.is_some() {
            self.stop(timeout)?;
        }
        self.start(config, generation, timeout)
    }

    /// Observes and reaps an exited process, if one has exited.
    pub fn poll_exit(&mut self) -> Result<Option<MihomoRuntimeEvent>, MihomoRuntimeError> {
        let Some(tree) = self.tree.as_mut() else {
            return Ok(None);
        };
        let Some(exit) = tree.poll_exit().map_err(MihomoRuntimeError::Platform)? else {
            return Ok(None);
        };
        let generation = self.generation.take().unwrap_or(0);
        self.tree = None;
        Ok(Some(MihomoRuntimeEvent::Exited { generation, exit }))
    }

    /// Returns whether a process tree is currently owned.
    pub const fn is_running(&self) -> bool {
        self.tree.is_some()
    }

    /// Returns the current generation, if running.
    pub const fn generation(&self) -> Option<u64> {
        self.generation
    }
}

fn platform_from_stop(failure: caly_platform::process::StopTreeFailure) -> PlatformFailure {
    failure
        .reap
        .or(failure.forced)
        .or(failure.graceful)
        .unwrap_or_else(|| PlatformFailure {
            operation: bounded("stop-mihomo"),
            resource: bounded("mihomo-process"),
            message: bounded("Mihomo stop failed"),
            suggested_action: bounded("inspect the Mihomo process owner"),
        })
}

/// Builds a `BoundedText<MAX>` from a static literal. The bounded
/// constructor's two failure paths (empty string / over-length) are
/// unreachable for every call site in this file: the inputs are short
/// labels like `"stop-mihomo"` or `"mihomo-process"`. `from_nonempty_clamped`
/// keeps the helper abort-free while preserving the same behaviour for the
/// well-formed call sites and providing a graceful fallback for any future
/// refactor that accidentally widens the input.
fn bounded<const MAX: usize>(value: &'static str) -> BoundedText<MAX> {
    BoundedText::from_nonempty_clamped(value.to_owned(), "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::KernelControl;
    use caly_domain::{BoundedVec, CapabilitySet, NodeId};
    use caly_platform::process::LinuxProcessSpawner;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::SystemTime};

    struct Control;
    impl KernelControl for Control {
        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::new(BoundedVec::new()).unwrap()
        }
        fn wait_ready(&mut self, _timeout: Duration) -> Result<(), KernelFailure> {
            Ok(())
        }
        fn select_proxy(&mut self, _node: NodeId, _timeout: Duration) -> Result<(), KernelFailure> {
            Ok(())
        }
        fn health_check(&mut self, _timeout: Duration) -> Result<(), KernelFailure> {
            Ok(())
        }
    }

    #[test]
    fn runtime_starts_health_checks_restarts_and_stops_owned_process()
    -> Result<(), MihomoRuntimeError> {
        let script = std::env::temp_dir().join(format!(
            "caly-mihomo-runtime-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |value| value.as_nanos())
        ));
        if fs::write(&script, b"#!/bin/sh\nsleep 10\n").is_err()
            || fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).is_err()
        {
            return Ok(());
        }
        let factory = MihomoSpawnSpecFactory::new(script.clone(), PathBuf::from("/tmp"))
            .map_err(MihomoRuntimeError::Kernel)?;
        let mut runtime = MihomoRuntime::new(factory, LinuxProcessSpawner, Control);
        let config = RenderedConfigRef {
            generation: 1,
            path: PathBuf::from("/tmp/mihomo-config.yaml"),
        };
        runtime.start(&config, 1, Duration::from_secs(1))?;
        assert!(runtime.is_running());
        assert_eq!(runtime.generation(), Some(1));
        runtime
            .health_check(Duration::from_millis(1))
            .map_err(MihomoRuntimeError::Kernel)?;
        runtime.restart(&config, 2, Duration::from_secs(1))?;
        assert_eq!(runtime.generation(), Some(2));
        runtime.stop(Duration::from_millis(100))?;
        assert!(!runtime.is_running());
        let _ = fs::remove_file(script);
        Ok(())
    }
}
