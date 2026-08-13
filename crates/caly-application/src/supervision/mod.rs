//! Runtime topology and stale-generation supervision rules.

mod topology;

pub use topology::{Component, DependencyEdge, TopologyError, default_topology, validate_topology};

/// Rejects a stale supervisor action after a new core generation starts.
pub const fn generation_is_current(observed: u64, current: u64) -> bool {
    observed == current
}
