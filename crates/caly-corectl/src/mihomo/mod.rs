//! Mihomo-specific adapter boundary.

mod api;
mod http;
mod runtime;

pub use http::MihomoHttpControl;
pub use runtime::{MihomoRuntime, MihomoRuntimeError, MihomoRuntimeEvent};

use std::path::{Path, PathBuf};

use caly_domain::{BoundedText, CoreKind};
use caly_platform::process::{ProcessArgument, ProcessArguments, SpawnSpec};

use crate::contract::{KernelFailure, KernelFailureKind, RenderedConfigRef, SpawnSpecFactory};

/// Builds bounded Mihomo validation and runtime process specifications.
#[derive(Clone, Debug)]
pub struct MihomoSpawnSpecFactory {
    binary: PathBuf,
    working_directory: PathBuf,
}

impl MihomoSpawnSpecFactory {
    /// Creates a factory for one Mihomo binary and its owned working directory.
    pub fn new(binary: PathBuf, working_directory: PathBuf) -> Result<Self, KernelFailure> {
        if binary.as_os_str().is_empty() || working_directory.as_os_str().is_empty() {
            return Err(crate::common::failure_with_kind(
                KernelFailureKind::InvalidConfig,
                "Mihomo binary and working directory must not be empty",
                "provide absolute paths owned by the daemon",
            ));
        }
        Ok(Self {
            binary,
            working_directory,
        })
    }

    /// Returns the configured binary path.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl SpawnSpecFactory for MihomoSpawnSpecFactory {
    fn core_kind(&self) -> CoreKind {
        CoreKind::Mihomo
    }

    fn build_spawn_spec(&self, config: &RenderedConfigRef) -> Result<SpawnSpec, KernelFailure> {
        self.build_spec(config, false)
    }

    fn build_validation_spec(
        &self,
        config: &RenderedConfigRef,
    ) -> Result<SpawnSpec, KernelFailure> {
        self.build_spec(config, true)
    }
}

impl MihomoSpawnSpecFactory {
    fn build_spec(
        &self,
        config: &RenderedConfigRef,
        validation_only: bool,
    ) -> Result<SpawnSpec, KernelFailure> {
        let config_path = bounded_path(&config.path, "rendered Mihomo config path")?;
        let working_directory = bounded_path(&self.working_directory, "Mihomo working directory")?;
        let mut arguments = ProcessArguments::new();
        push_argument(&mut arguments, "-d")?;
        push_argument(&mut arguments, working_directory.as_str())?;
        push_argument(&mut arguments, "-f")?;
        push_argument(&mut arguments, config_path.as_str())?;
        if validation_only {
            push_argument(&mut arguments, "-t")?;
        }
        Ok(SpawnSpec {
            executable: self.binary.clone(),
            arguments,
            working_directory: self.working_directory.clone(),
            kill_on_owner_drop: true,
            label: "mihomo".to_owned(),
        })
    }
}

fn push_argument(arguments: &mut ProcessArguments, value: &str) -> Result<(), KernelFailure> {
    let value = ProcessArgument::new(value.to_owned()).map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::InvalidConfig,
            "Mihomo process argument exceeds the bounded limit",
            "shorten the binary, working-directory, or config path",
        )
    })?;
    arguments.try_push(value).map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::InvalidConfig,
            "Mihomo process argument list exceeded the bounded limit",
            "reduce the number of process arguments",
        )
    })
}

fn bounded_path(path: &Path, label: &'static str) -> Result<BoundedText<4_096>, KernelFailure> {
    BoundedText::new(path.display().to_string()).map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::InvalidConfig,
            "Mihomo path exceeds the bounded limit",
            label,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::SpawnSpecFactory;

    #[test]
    fn mihomo_specs_are_bounded_and_distinguish_validation() -> Result<(), KernelFailure> {
        let factory = MihomoSpawnSpecFactory::new(
            PathBuf::from("/usr/bin/mihomo"),
            PathBuf::from("/var/lib/caly/mihomo"),
        )?;
        let config = RenderedConfigRef {
            generation: 3,
            path: PathBuf::from("/var/lib/caly/mihomo/config.yaml"),
        };
        let runtime = factory.build_spawn_spec(&config)?;
        let validation = factory.build_validation_spec(&config)?;
        assert_eq!(factory.core_kind(), CoreKind::Mihomo);
        assert!(!runtime.arguments.iter().any(|arg| arg.as_str() == "-t"));
        assert!(validation.arguments.iter().any(|arg| arg.as_str() == "-t"));
        assert!(runtime.kill_on_owner_drop);
        Ok(())
    }
}
