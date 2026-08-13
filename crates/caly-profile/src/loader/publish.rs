//! Manifest-last configuration publication and bounded rollback history.

use std::collections::VecDeque;

use caly_domain::BoundedVec;
use caly_platform::{PlatformFailure, paths::SafeName};

use crate::schema::MAX_CONFIG_BYTES;

/// Capacity-enforced persisted configuration artifact.
pub type ConfigArtifact = BoundedVec<u8, MAX_CONFIG_BYTES>;

/// Immutable complete configuration generation.
pub struct ConfigGeneration {
    pub id: [u8; 16],
    pub base: ConfigArtifact,
    pub rendered: ConfigArtifact,
    pub active_profile: Option<SafeName>,
}

/// Durable store: generation first, active manifest last.
pub trait ConfigGenerationStore {
    fn current(&mut self) -> Result<Option<[u8; 16]>, PlatformFailure>;
    fn write_generation(&mut self, generation: &ConfigGeneration) -> Result<(), PlatformFailure>;
    fn activate(&mut self, generation: [u8; 16]) -> Result<(), PlatformFailure>;
    fn generation_exists(&mut self, generation: [u8; 16]) -> Result<bool, PlatformFailure>;
}

/// Publication failure states whether an orphan generation may remain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishFailure {
    pub primary: PlatformFailure,
    pub orphan_generation: Option<[u8; 16]>,
}

/// Owner of active manifest and bounded rollback order.
pub struct ConfigPublisher<S> {
    store: S,
    current: Option<[u8; 16]>,
    history: VecDeque<[u8; 16]>,
    history_capacity: usize,
}

impl<S: ConfigGenerationStore> ConfigPublisher<S> {
    pub fn open(mut store: S, history_capacity: usize) -> Result<Self, PlatformFailure> {
        let current = store.current()?;
        Ok(Self {
            store,
            current,
            history: VecDeque::with_capacity(history_capacity),
            history_capacity,
        })
    }

    /// Writes a complete immutable generation before atomically switching active manifest.
    pub fn publish(&mut self, generation: ConfigGeneration) -> Result<(), PublishFailure> {
        let id = generation.id;
        self.store
            .write_generation(&generation)
            .map_err(|primary| PublishFailure {
                primary,
                orphan_generation: None,
            })?;
        self.store.activate(id).map_err(|primary| PublishFailure {
            primary,
            orphan_generation: Some(id),
        })?;
        if let Some(previous) = self.current.replace(id) {
            self.push_history(previous);
        }
        Ok(())
    }

    /// Reactivates the newest retained generation without rewriting its files.
    pub fn rollback(&mut self) -> Result<[u8; 16], RollbackFailure> {
        let target = *self.history.back().ok_or(RollbackFailure::HistoryEmpty)?;
        if !self
            .store
            .generation_exists(target)
            .map_err(RollbackFailure::Store)?
        {
            return Err(RollbackFailure::GenerationMissing(target));
        }
        self.store
            .activate(target)
            .map_err(RollbackFailure::Store)?;
        self.history.pop_back();
        if let Some(previous) = self.current.replace(target) {
            self.push_history(previous);
        }
        Ok(target)
    }

    pub const fn current(&self) -> Option<[u8; 16]> {
        self.current
    }

    fn push_history(&mut self, generation: [u8; 16]) {
        if self.history_capacity == 0 {
            return;
        }
        if self.history.len() == self.history_capacity {
            self.history.pop_front();
        }
        self.history.push_back(generation);
    }
}

/// Rollback rejection without partial in-memory commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RollbackFailure {
    HistoryEmpty,
    GenerationMissing([u8; 16]),
    Store(PlatformFailure),
}
