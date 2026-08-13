//! State acknowledged by the managed proxy core.

use core::fmt;

use crate::NodeId;

/// Supported proxy-core family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreKind {
    /// Mihomo core.
    Mihomo,
    /// sing-box core.
    SingBox,
    /// Xray core.
    Xray,
}

/// Truthful managed-core lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreRunState {
    /// No owned process tree exists.
    Stopped,
    /// A process tree is being started but is not verified.
    Starting,
    /// The owned process tree passed readiness verification.
    Running,
    /// An orderly stop is in progress.
    Stopping,
    /// The process exited unexpectedly.
    Crashed,
    /// Start, stop, or verification failed.
    Failed,
}

/// Pure view of configuration accepted by the current core generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedState {
    core: Option<CoreKind>,
    run_state: CoreRunState,
    selected_node: Option<NodeId>,
    config_generation: Option<u64>,
}

/// Error returned when applied state claims an impossible runtime combination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppliedStateError {
    /// Active runtime state requires a concrete core.
    ActiveWithoutCore,
    /// Stopped state must not retain applied runtime details.
    StoppedWithRuntimeDetails,
}

impl fmt::Display for AppliedStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveWithoutCore => formatter.write_str("active runtime state requires a core"),
            Self::StoppedWithRuntimeDetails => {
                formatter.write_str("stopped runtime must not retain applied details")
            }
        }
    }
}

impl std::error::Error for AppliedStateError {}

impl AppliedState {
    /// Constructs the initial state with no owned core process.
    pub const fn stopped() -> Self {
        Self {
            core: None,
            run_state: CoreRunState::Stopped,
            selected_node: None,
            config_generation: None,
        }
    }

    /// Validates a runtime-applied state projection.
    pub fn new(
        core: Option<CoreKind>,
        run_state: CoreRunState,
        selected_node: Option<NodeId>,
        config_generation: Option<u64>,
    ) -> Result<Self, AppliedStateError> {
        if run_state == CoreRunState::Stopped
            && (core.is_some() || selected_node.is_some() || config_generation.is_some())
        {
            return Err(AppliedStateError::StoppedWithRuntimeDetails);
        }
        if run_state != CoreRunState::Stopped && core.is_none() {
            return Err(AppliedStateError::ActiveWithoutCore);
        }
        Ok(Self {
            core,
            run_state,
            selected_node,
            config_generation,
        })
    }

    /// Returns the actual lifecycle state.
    pub const fn run_state(&self) -> CoreRunState {
        self.run_state
    }
    /// Returns the core acknowledged by runtime, not desired configuration.
    pub const fn core(&self) -> Option<CoreKind> {
        self.core
    }
    /// Returns the selection acknowledged by the core.
    pub const fn selected_node(&self) -> Option<NodeId> {
        self.selected_node
    }
    /// Returns the configuration generation acknowledged by the core.
    pub const fn config_generation(&self) -> Option<u64> {
        self.config_generation
    }
}
