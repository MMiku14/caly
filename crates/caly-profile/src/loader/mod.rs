//! Layered configuration loader resource budgets.

mod file;
mod layered;
mod publish;

pub use file::{ConfigFileError, load_config_file, load_json_file};
pub use layered::{
    InMemoryProfileResolver, LayeredConfigError, LayeredConfigPaths, ProfileBodyResolver,
    load_layered_yaml, load_layered_yaml_strict, load_layered_yaml_with,
};
pub use publish::{
    ConfigArtifact, ConfigGeneration, ConfigGenerationStore, ConfigPublisher, PublishFailure,
    RollbackFailure,
};

/// Hard limits applied before recursive merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoaderLimits {
    pub max_fragment_files: usize,
    pub max_total_bytes: usize,
    pub max_merge_depth: usize,
}

impl LoaderLimits {
    /// Conservative defaults inherited from the old-project audit.
    pub const fn secure_default() -> Self {
        Self {
            max_fragment_files: 128,
            max_total_bytes: 32 * 1_024 * 1_024,
            max_merge_depth: 64,
        }
    }
}

/// Checked cumulative loader accounting.
pub struct LoaderBudget {
    limits: LoaderLimits,
    files: usize,
    bytes: usize,
}

impl LoaderBudget {
    pub const fn new(limits: LoaderLimits) -> Self {
        Self {
            limits,
            files: 0,
            bytes: 0,
        }
    }

    pub fn charge_file(&mut self, bytes: usize) -> Result<(), LoaderBudgetError> {
        let files = self
            .files
            .checked_add(1)
            .ok_or(LoaderBudgetError::ArithmeticOverflow)?;
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(LoaderBudgetError::ArithmeticOverflow)?;
        if files > self.limits.max_fragment_files {
            return Err(LoaderBudgetError::TooManyFiles);
        }
        if total > self.limits.max_total_bytes {
            return Err(LoaderBudgetError::TooManyBytes);
        }
        self.files = files;
        self.bytes = total;
        Ok(())
    }

    pub const fn check_depth(&self, depth: usize) -> Result<(), LoaderBudgetError> {
        if depth > self.limits.max_merge_depth {
            return Err(LoaderBudgetError::MergeTooDeep);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoaderBudgetError {
    ArithmeticOverflow,
    TooManyFiles,
    TooManyBytes,
    MergeTooDeep,
}

impl core::fmt::Display for LoaderBudgetError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ArithmeticOverflow => formatter.write_str("loader arithmetic overflow"),
            Self::TooManyFiles => formatter.write_str("loader exceeded the file count limit"),
            Self::TooManyBytes => formatter.write_str("loader exceeded the total byte limit"),
            Self::MergeTooDeep => formatter.write_str("merge exceeded the depth limit"),
        }
    }
}

impl std::error::Error for LoaderBudgetError {}
