//! Runtime topology and stale-generation supervision rules.

mod topology;

pub use topology::{default_topology, validate_topology, Component, DependencyEdge, TopologyError};
