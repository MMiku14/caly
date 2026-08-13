//! sing-box node/group mapping from credential-free subscription projections.

use std::collections::HashMap;

use caly_domain::{DisplayNode, NodeId, SnapshotNodes};

/// Stable node-to-sing-box tag mapping.
#[derive(Default)]
pub struct SingBoxNodeRegistry {
    tags: HashMap<NodeId, String>,
}

impl SingBoxNodeRegistry {
    /// Rebuilds tags from a subscription projection without exposing credentials.
    pub fn replace_from_projection(&mut self, nodes: &SnapshotNodes) {
        self.tags.clear();
        for (index, node) in nodes.iter().enumerate() {
            self.tags.insert(
                node.id(),
                format!("proxy-{index}-{}", node.protocol().as_str()),
            );
        }
    }

    /// Returns the generated outbound tag for a node.
    pub fn tag_for(&self, id: NodeId) -> Option<&str> {
        self.tags.get(&id).map(String::as_str)
    }

    /// Returns the number of mapped nodes.
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Returns whether no nodes are mapped.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

impl SingBoxNodeRegistry {
    /// Registers one node using an explicit tag, useful for provider mapping.
    pub fn register(&mut self, node: &DisplayNode, tag: String) {
        self.tags.insert(node.id(), tag);
    }
}
