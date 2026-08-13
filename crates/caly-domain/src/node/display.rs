//! Redacted node projection for presentation and wire boundaries.

use crate::{BoundedText, NodeId};

/// Maximum node display-name length in UTF-8 bytes.
pub const NODE_DISPLAY_NAME_MAX_BYTES: usize = 256;
/// Maximum protocol-label length in UTF-8 bytes.
pub const NODE_PROTOCOL_LABEL_MAX_BYTES: usize = 32;

/// Validated user-facing node name.
pub type NodeDisplayName = BoundedText<NODE_DISPLAY_NAME_MAX_BYTES>;

/// Builds a display name safe to print on an operator terminal.
///
/// Audit #98: subscription-controlled names (URI `#fragment`, Clash `name:`)
/// used to flow into `caly nodes` / TUI output verbatim — C0/C1 control
/// characters included — letting a hostile subscription spray ANSI escapes,
/// clear screens, or forge output (`\r` overwrites). Control characters are
/// stripped here; a name that is empty after stripping is rejected.
/// Newlines/tabs are controls too, which additionally guarantees the name
/// stays single-line in every renderer.
pub fn sanitized_display_name(
    value: impl Into<String>,
) -> Result<NodeDisplayName, crate::TextError> {
    let filtered: String = value.into().chars().filter(|ch| !ch.is_control()).collect();
    NodeDisplayName::new(filtered)
}
/// Stable safe protocol label, such as `vmess` or `shadowsocks`.
pub type NodeProtocolLabel = BoundedText<NODE_PROTOCOL_LABEL_MAX_BYTES>;

/// Credential-free node data allowed in snapshots and thin clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayNode {
    id: NodeId,
    name: NodeDisplayName,
    protocol: NodeProtocolLabel,
    available: bool,
    latency_ms: Option<u32>,
}

impl DisplayNode {
    /// Constructs a redacted display projection.
    pub const fn new(
        id: NodeId,
        name: NodeDisplayName,
        protocol: NodeProtocolLabel,
        available: bool,
        latency_ms: Option<u32>,
    ) -> Self {
        Self {
            id,
            name,
            protocol,
            available,
            latency_ms,
        }
    }

    /// Returns the stable selection identity.
    pub const fn id(&self) -> NodeId {
        self.id
    }
    /// Returns the user-facing name.
    pub const fn name(&self) -> &NodeDisplayName {
        &self.name
    }
    /// Returns a safe protocol label.
    pub const fn protocol(&self) -> &NodeProtocolLabel {
        &self.protocol
    }
    /// Returns observed availability, not configured capability.
    pub const fn is_available(&self) -> bool {
        self.available
    }
    /// Returns the latest successful latency when one exists.
    pub const fn latency_ms(&self) -> Option<u32> {
        self.latency_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_display_name_strips_control_sequences() {
        // Audit #98: ESC/CSI payloads from a hostile subscription fragment
        // must never reach an operator terminal.
        let name = sanitized_display_name("node-\u{1b}[2J\u{1b}[31mA\u{7}")
            .unwrap_or_else(|error| panic!("sanitization failed: {error}"));
        assert_eq!(name.as_str(), "node-[2J[31mA");
    }

    #[test]
    fn sanitized_display_name_rejects_control_only_input() {
        assert!(sanitized_display_name("\u{1b}\u{7}\u{0}").is_err());
    }
}
