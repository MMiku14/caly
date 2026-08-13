//! ConfigActor ownership contract (port).

use super::error::ActorFailure;

/// External immutable candidate reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigCandidate {
    pub id: [u8; 16],
}
/// Rendered but unpublished candidate owned by ConfigActor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedConfig {
    pub candidate_id: [u8; 16],
    pub generation: u64,
}
/// Atomically published configuration generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedConfig {
    pub candidate_id: [u8; 16],
    pub generation: u64,
    /// True when the rendered config was byte-identical to the already
    /// published one: nothing was written and the kernel does NOT need a
    /// restart (刀 4, 2026-08-12 pipeline design — a no-op apply must not
    /// drop connections).
    pub unchanged: bool,
}

/// ConfigActor calls available only to coordinators.
pub trait ConfigActorPort {
    fn parse_and_render(
        &mut self,
        candidate: ConfigCandidate,
    ) -> Result<PreparedConfig, ActorFailure>;
    fn discard_prepared(&mut self, prepared: PreparedConfig) -> Result<(), ActorFailure>;
    fn commit_candidate(
        &mut self,
        prepared: PreparedConfig,
    ) -> Result<CommittedConfig, ActorFailure>;
    fn rollback_commit(&mut self, committed: CommittedConfig) -> Result<(), ActorFailure>;

    /// Returns the most recently committed config text (for a hot reload,
    /// 刀 5) or `None` when nothing was committed yet.
    fn current_contents(&self) -> Option<Vec<u8>> {
        None
    }
}
