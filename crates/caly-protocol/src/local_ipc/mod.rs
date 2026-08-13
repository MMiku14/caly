//! Local IPC endpoint descriptions.
//!
//! Concrete Unix socket and Windows named-pipe operations remain in
//! `caly-platform`; this module contains only protocol-side connection intent.

use caly_domain::BoundedText;

/// Maximum logical local endpoint-name length.
pub const LOCAL_ENDPOINT_NAME_MAX_BYTES: usize = 128;
/// Validated logical endpoint name.
pub type LocalEndpointName = BoundedText<LOCAL_ENDPOINT_NAME_MAX_BYTES>;

/// Which daemon transport a client intends to use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalEndpoint {
    /// Platform default endpoint resolved by `caly-platform`.
    PlatformDefault,
    /// Explicit logical endpoint name, validated by the platform boundary.
    Named(LocalEndpointName),
}
