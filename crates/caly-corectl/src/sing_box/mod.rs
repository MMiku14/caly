//! sing-box-specific adapter boundary.
//!
//! Rendering of sing-box configuration (base tuning, subscription document,
//! DNS fragment, node/group outbounds) lives in `caly-coreconf`; this module
//! keeps the process boundary: spawn specs plus runtime/control adapters.

mod http;
mod runtime;

pub use http::SingBoxHttpControl;
pub use runtime::{SingBoxRuntime, SingBoxRuntimeEvent};

use std::path::{Path, PathBuf};

use caly_domain::CoreKind;
use caly_platform::process::{ProcessArgument, ProcessArguments, SpawnSpec};

use crate::contract::{KernelFailure, RenderedConfigRef, SpawnSpecFactory};

/// Builds bounded sing-box run/check process specifications.
#[derive(Clone, Debug)]
pub struct SingBoxSpawnSpecFactory {
    binary: PathBuf,
    working_directory: PathBuf,
}

impl SingBoxSpawnSpecFactory {
    pub fn new(binary: PathBuf, working_directory: PathBuf) -> Result<Self, KernelFailure> {
        if binary.as_os_str().is_empty() || working_directory.as_os_str().is_empty() {
            return Err(crate::common::config_failure(
                "sing-box binary and working directory must not be empty",
                "provide owned paths",
            ));
        }
        Ok(Self {
            binary,
            working_directory,
        })
    }

    /// Returns the configured sing-box binary path.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl SpawnSpecFactory for SingBoxSpawnSpecFactory {
    fn core_kind(&self) -> CoreKind {
        CoreKind::SingBox
    }

    fn build_spawn_spec(&self, config: &RenderedConfigRef) -> Result<SpawnSpec, KernelFailure> {
        self.build(config, false)
    }

    fn build_validation_spec(
        &self,
        config: &RenderedConfigRef,
    ) -> Result<SpawnSpec, KernelFailure> {
        self.build(config, true)
    }
}

impl SingBoxSpawnSpecFactory {
    fn build(
        &self,
        config: &RenderedConfigRef,
        validation: bool,
    ) -> Result<SpawnSpec, KernelFailure> {
        let mut args = ProcessArguments::new();
        if validation {
            push(&mut args, "check")?;
        } else {
            push(&mut args, "run")?;
        }
        push(&mut args, "-c")?;
        push(&mut args, &config.path.display().to_string())?;
        Ok(SpawnSpec {
            executable: self.binary.clone(),
            arguments: args,
            working_directory: self.working_directory.clone(),
            kill_on_owner_drop: true,
            label: "sing-box".to_owned(),
        })
    }
}

fn push(args: &mut ProcessArguments, value: &str) -> Result<(), KernelFailure> {
    let value = ProcessArgument::new(value.to_owned()).map_err(|_| {
        crate::common::config_failure("sing-box argument is too long", "shorten the path")
    })?;
    args.try_push(value).map_err(|_| {
        crate::common::config_failure("sing-box argument list is full", "reduce process arguments")
    })
}

#[cfg(test)]
mod spawn_tests {
    use super::*;
    use crate::contract::SpawnSpecFactory;

    #[test]
    fn sing_box_factory_distinguishes_run_and_check() -> Result<(), KernelFailure> {
        let factory = SingBoxSpawnSpecFactory::new(
            PathBuf::from("vendor/bin/sing-box"),
            PathBuf::from("/tmp"),
        )?;
        let config = RenderedConfigRef {
            generation: 1,
            path: PathBuf::from("/tmp/config.json"),
        };
        assert_eq!(factory.core_kind(), CoreKind::SingBox);
        assert_eq!(
            factory.build_spawn_spec(&config)?.arguments[0].as_str(),
            "run"
        );
        assert_eq!(
            factory.build_validation_spec(&config)?.arguments[0].as_str(),
            "check"
        );
        Ok(())
    }
}
