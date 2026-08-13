//! Node types split by dialable and display responsibilities.

mod canonical;
mod dialable;
mod display;
mod endpoint;
mod protocol;
mod source;
mod transport;
mod validation;

pub use dialable::{DialableNode, NodeBuilder, NodeTag, NodeTags};
pub use display::{
    sanitized_display_name, DisplayNode, NodeDisplayName, NodeProtocolLabel,
    NODE_DISPLAY_NAME_MAX_BYTES,
};
pub use endpoint::{Endpoint, EndpointHost, HostError};
pub use protocol::{
    CongestionControl, Credential, Protocol, ProtocolText, ShadowsocksCipher, ShadowsocksPlugin,
    VmessCipher,
};
pub use source::NodeSource;
pub use transport::{
    RealityConfig, TlsConfig, Transport, TransportText, TransportTextList, WebSocketEarlyData,
};
pub use validation::NodeValidationError;
