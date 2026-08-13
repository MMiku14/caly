//! sing-box lifecycle owner with optional Clash API readiness.

use std::{path::PathBuf, time::Duration};

use caly_domain::BoundedText;
use caly_platform::process::{
    LinuxProcessSpawner, OwnedProcessTree, ProcessExit, ProcessSpawner, stop_and_reap,
};

use super::{SingBoxHttpControl, SingBoxSpawnSpecFactory};
use crate::common::{failure, wait_ready_detecting_exit};
use crate::contract::{KernelControl, KernelFailure, RenderedConfigRef, SpawnSpecFactory};

/// sing-box lifecycle event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SingBoxRuntimeEvent {
    Exited { generation: u64, exit: ProcessExit },
}

/// Owns one sing-box process tree and Clash-compatible API probe.
pub struct SingBoxRuntime {
    factory: SingBoxSpawnSpecFactory,
    spawner: LinuxProcessSpawner,
    control: SingBoxHttpControl,
    config: RenderedConfigRef,
    tree: Option<<LinuxProcessSpawner as ProcessSpawner>::Tree>,
    generation: u64,
}

impl SingBoxRuntime {
    /// Creates a stopped sing-box runtime.
    pub fn new(
        binary: PathBuf,
        working_directory: PathBuf,
        controller: String,
        config: PathBuf,
        generation: u64,
        secret: Option<BoundedText<4_096>>,
    ) -> Result<Self, KernelFailure> {
        let factory = SingBoxSpawnSpecFactory::new(binary, working_directory)?;
        let control = SingBoxHttpControl::new(controller, secret)?;
        Ok(Self {
            factory,
            spawner: LinuxProcessSpawner,
            control,
            config: RenderedConfigRef {
                generation,
                path: config,
            },
            tree: None,
            generation,
        })
    }

    /// Starts sing-box, waits for Clash API readiness, then checks health.
    pub fn start(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        crate::common::ensure_executable(self.factory.binary(), "sing-box")?;
        if self.tree.is_some() {
            return Err(failure(
                "sing-box is already running",
                "stop it before starting",
            ));
        }
        let spec = self.factory.build_spawn_spec(&self.config)?;
        let mut tree = self
            .spawner
            .spawn_owned(spec, self.generation)
            .map_err(|error| {
                failure(
                    &format!("sing-box spawn failed: {error:?}"),
                    "inspect binary and permissions",
                )
            })?;
        if let Err(error) = wait_ready_detecting_exit::<LinuxProcessSpawner, SingBoxHttpControl>(
            &mut tree,
            &mut self.control,
            timeout,
            "sing-box",
        ) {
            let _ = stop_and_reap(&mut tree, timeout);
            return Err(error);
        }
        self.tree = Some(tree);
        Ok(())
    }

    /// Stops and reaps sing-box.
    pub fn stop(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        let mut tree = self
            .tree
            .take()
            .ok_or_else(|| failure("sing-box is not running", "start sing-box first"))?;
        stop_and_reap(&mut tree, timeout)
            .map(|_| ())
            .map_err(|error| {
                failure(
                    &format!("sing-box stop failed: {error:?}"),
                    "inspect process ownership",
                )
            })
    }

    /// Restarts sing-box using the current config generation.
    pub fn restart(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        if self.tree.is_some() {
            self.stop(timeout)?;
        }
        self.start(timeout)
    }

    /// Performs a Clash-compatible health check.
    pub fn health_check(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        self.control.health_check(timeout)
    }

    /// Returns whether a process tree is currently owned (running).
    pub const fn is_running(&self) -> bool {
        self.tree.is_some()
    }

    /// Polls and reaps an abnormal exit.
    pub fn poll_exit(&mut self) -> Result<Option<SingBoxRuntimeEvent>, KernelFailure> {
        let Some(tree) = self.tree.as_mut() else {
            return Ok(None);
        };
        let Some(exit) = tree.poll_exit().map_err(|error| {
            failure(
                &format!("sing-box poll failed: {error:?}"),
                "inspect process ownership",
            )
        })?
        else {
            return Ok(None);
        };
        self.tree = None;
        Ok(Some(SingBoxRuntimeEvent::Exited {
            generation: self.generation,
            exit,
        }))
    }
}
