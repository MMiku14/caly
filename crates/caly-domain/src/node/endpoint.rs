//! Validated proxy endpoint values.

use core::{fmt, num::NonZeroU16};

use crate::{BoundedText, TextError};

/// Maximum endpoint host length in ASCII bytes.
pub const HOST_MAX_BYTES: usize = 253;

/// Error returned when constructing an endpoint host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError {
    /// Empty or oversized host text.
    Text(TextError),
    /// Domain currently requires an ASCII IP literal or DNS name.
    NonAscii,
    /// Whitespace is forbidden inside an endpoint host.
    Whitespace,
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(error) => error.fmt(formatter),
            Self::NonAscii => formatter.write_str("endpoint host must be ASCII; use its IDNA form"),
            Self::Whitespace => formatter.write_str("endpoint host must not contain whitespace"),
        }
    }
}

impl std::error::Error for HostError {}

/// Canonical lowercase host used for dialing and identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EndpointHost(BoundedText<HOST_MAX_BYTES>);

impl EndpointHost {
    /// Validates and canonicalizes an endpoint host.
    pub fn new(value: impl Into<String>) -> Result<Self, HostError> {
        let value = value.into();
        if !value.is_ascii() {
            return Err(HostError::NonAscii);
        }
        if value.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(HostError::Whitespace);
        }
        BoundedText::new(value.to_ascii_lowercase())
            .map(Self)
            .map_err(HostError::Text)
    }

    /// Returns canonical host text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Complete non-zero network endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    host: EndpointHost,
    port: NonZeroU16,
}

impl Endpoint {
    /// Constructs a complete endpoint.
    pub const fn new(host: EndpointHost, port: NonZeroU16) -> Self {
        Self { host, port }
    }
    /// Returns the canonical host.
    pub const fn host(&self) -> &EndpointHost {
        &self.host
    }
    /// Returns the non-zero port.
    pub const fn port(&self) -> NonZeroU16 {
        self.port
    }
}
