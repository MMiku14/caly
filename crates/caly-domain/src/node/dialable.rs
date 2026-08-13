//! Complete dialable node and builder.

use crate::{BoundedText, BoundedVec, NodeId};

use super::{
    canonical, validation, DisplayNode, Endpoint, NodeDisplayName, NodeProtocolLabel, NodeSource,
    NodeValidationError, Protocol, TlsConfig, Transport,
};

/// Maximum number of normalized node tags.
pub const MAX_NODE_TAGS: usize = 32;
/// Maximum normalized tag length.
pub const NODE_TAG_MAX_BYTES: usize = 64;
/// Normalized node tag.
pub type NodeTag = BoundedText<NODE_TAG_MAX_BYTES>;
/// Bounded tag set representation.
pub type NodeTags = BoundedVec<NodeTag, MAX_NODE_TAGS>;

/// Complete node used only by configuration and kernel-rendering owners.
///
/// It deliberately has no `Clone`, equality, or serialization implementation
/// because it contains credentials. Use [`Self::display`] at client boundaries.
#[derive(Debug)]
pub struct DialableNode {
    id: NodeId,
    name: NodeDisplayName,
    endpoint: Endpoint,
    protocol: Protocol,
    transport: Option<Transport>,
    tls: Option<TlsConfig>,
    dialer_proxy: Option<NodeId>,
    tags: NodeTags,
    source: NodeSource,
}

impl DialableNode {
    /// Returns the complete canonical identity hash.
    pub const fn id(&self) -> NodeId {
        self.id
    }
    /// Returns the dial endpoint.
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    /// Returns protocol-specific dial fields.
    pub const fn protocol(&self) -> &Protocol {
        &self.protocol
    }
    /// Returns optional framing transport.
    pub const fn transport(&self) -> Option<&Transport> {
        self.transport.as_ref()
    }
    /// Returns optional TLS identity.
    pub const fn tls(&self) -> Option<&TlsConfig> {
        self.tls.as_ref()
    }
    /// Returns a chained upstream node identity.
    pub const fn dialer_proxy(&self) -> Option<&NodeId> {
        self.dialer_proxy.as_ref()
    }
    /// Returns normalized non-identity tags.
    pub const fn tags(&self) -> &NodeTags {
        &self.tags
    }
    /// Returns credential-free provenance.
    pub const fn source(&self) -> &NodeSource {
        &self.source
    }

    /// Creates a credential-free presentation projection.
    pub fn display(
        &self,
        available: bool,
        latency_ms: Option<u32>,
    ) -> Result<DisplayNode, crate::TextError> {
        let protocol = NodeProtocolLabel::new(self.protocol.label().to_owned())?;
        Ok(DisplayNode::new(
            self.id,
            self.name.clone(),
            protocol,
            available,
            latency_ms,
        ))
    }
}

/// Builder that computes identity only after every dial-affecting field exists.
#[derive(Debug)]
pub struct NodeBuilder {
    name: NodeDisplayName,
    endpoint: Endpoint,
    protocol: Protocol,
    transport: Option<Transport>,
    tls: Option<TlsConfig>,
    dialer_proxy: Option<NodeId>,
    tags: NodeTags,
    source: NodeSource,
}

impl NodeBuilder {
    /// Starts a complete node candidate with required fields.
    pub fn new(
        name: NodeDisplayName,
        endpoint: Endpoint,
        protocol: Protocol,
        source: NodeSource,
    ) -> Self {
        Self {
            name,
            endpoint,
            protocol,
            transport: None,
            tls: None,
            dialer_proxy: None,
            tags: NodeTags::new(),
            source,
        }
    }

    /// Adds a transport before identity calculation.
    #[must_use]
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = Some(transport);
        self
    }
    /// Adds TLS settings before identity calculation.
    #[must_use]
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }
    /// Validates all fields, then computes SHA-256/128 over canonical identity.
    pub fn build(self) -> Result<DialableNode, NodeValidationError> {
        validation::validate(&self.protocol, &self.tags)?;
        let mut node = DialableNode {
            id: NodeId::from_bytes([0; 16]),
            name: self.name,
            endpoint: self.endpoint,
            protocol: self.protocol,
            transport: self.transport,
            tls: self.tls,
            dialer_proxy: self.dialer_proxy,
            tags: self.tags,
            source: self.source,
        };
        node.id = canonical::node_id(&node);
        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU16;

    use super::*;
    use crate::{EndpointHost, SecretText, TransportText, TransportTextList};

    fn node(
        name: &str,
        password: &str,
        sni: &str,
    ) -> Result<DialableNode, Box<dyn std::error::Error>> {
        let host = EndpointHost::new("EXAMPLE.COM")?;
        let port = NonZeroU16::new(443).ok_or(NodeValidationError::ZeroBandwidth)?;
        let endpoint = Endpoint::new(host, port);
        let protocol = Protocol::Trojan {
            password: SecretText::new(password)?,
        };
        let tls = TlsConfig::new(
            Some(TransportText::new(sni)?),
            TransportTextList::new(),
            false,
            None,
            None,
        );
        Ok(NodeBuilder::new(
            NodeDisplayName::new(name)?,
            endpoint,
            protocol,
            NodeSource::Manual,
        )
        .with_tls(tls)
        .build()?)
    }

    #[test]
    fn display_name_does_not_change_identity() -> Result<(), Box<dyn std::error::Error>> {
        let first = node("Hong Kong", "secret", "edge.example")?;
        let renamed = node("Renamed", "secret", "edge.example")?;
        assert_eq!(first.id(), renamed.id());
        Ok(())
    }

    #[test]
    fn every_dial_identity_field_changes_identity() -> Result<(), Box<dyn std::error::Error>> {
        let base = node("Node", "secret", "edge.example")?;
        let password = node("Node", "different", "edge.example")?;
        let sni = node("Node", "secret", "other.example")?;
        assert_ne!(base.id(), password.id());
        assert_ne!(base.id(), sni.id());
        Ok(())
    }

    #[test]
    fn diagnostics_do_not_reveal_credentials() -> Result<(), Box<dyn std::error::Error>> {
        let rendered = format!("{:?}", node("Node", "never-print-me", "edge.example")?);
        assert!(!rendered.contains("never-print-me"));
        assert!(rendered.contains("[REDACTED]"));
        Ok(())
    }
}
