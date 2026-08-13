//! Static wait-dependency DAG validation.

use caly_domain::BoundedVec;

/// Runtime component participating in synchronous request/reply waits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Component {
    ConfigActor = 0,
    CoreActor = 1,
    SubscriptionActor = 2,
    PlatformActor = 3,
    TelemetryActor = 4,
    EventSequencer = 5,
    StateProjector = 6,
    OperationOwner = 7,
}

const COMPONENT_COUNT: usize = 8;
/// Maximum declared wait edges.
pub const MAX_DEPENDENCY_EDGES: usize = 32;

/// `from` may synchronously wait for `to`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyEdge {
    pub from: Component,
    pub to: Component,
}

/// Invalid runtime dependency topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyError {
    SelfDependency(Component),
    Cycle,
}

/// Validates that synchronous waits cannot form an actor deadlock cycle.
pub fn validate_topology(
    edges: &BoundedVec<DependencyEdge, MAX_DEPENDENCY_EDGES>,
) -> Result<(), TopologyError> {
    let mut indegree = [0_u8; COMPONENT_COUNT];
    for edge in edges {
        if edge.from == edge.to {
            return Err(TopologyError::SelfDependency(edge.from));
        }
        let target = edge.to as usize;
        indegree[target] = indegree[target].saturating_add(1);
    }
    let mut removed = [false; COMPONENT_COUNT];
    let mut count = 0_usize;
    loop {
        let next = (0..COMPONENT_COUNT).find(|index| !removed[*index] && indegree[*index] == 0);
        let Some(node) = next else { break };
        removed[node] = true;
        count += 1;
        for edge in edges.iter().filter(|edge| edge.from as usize == node) {
            indegree[edge.to as usize] = indegree[edge.to as usize].saturating_sub(1);
        }
    }
    if count == COMPONENT_COUNT {
        Ok(())
    } else {
        Err(TopologyError::Cycle)
    }
}

/// Intended v2 synchronous wait graph.
pub fn default_topology(
) -> Result<BoundedVec<DependencyEdge, MAX_DEPENDENCY_EDGES>, caly_domain::CapacityError> {
    BoundedVec::try_from_vec(vec![DependencyEdge {
        from: Component::EventSequencer,
        to: Component::StateProjector,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_graph_is_acyclic() -> Result<(), Box<dyn std::error::Error>> {
        let graph = default_topology()?;
        assert_eq!(validate_topology(&graph), Ok(()));
        Ok(())
    }

    #[test]
    fn actor_cycle_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let graph = BoundedVec::try_from_vec(vec![
            DependencyEdge {
                from: Component::ConfigActor,
                to: Component::CoreActor,
            },
            DependencyEdge {
                from: Component::CoreActor,
                to: Component::ConfigActor,
            },
        ])?;
        assert_eq!(validate_topology(&graph), Err(TopologyError::Cycle));
        Ok(())
    }
}
