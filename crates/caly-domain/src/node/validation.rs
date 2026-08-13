//! Dialable-node invariants checked before identity calculation.

use core::fmt;

use super::{NodeTags, Protocol};

/// Error returned before a dialable node can be finalized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeValidationError {
    /// A normalized tag occurs more than once.
    DuplicateTag,
    /// A bandwidth limit was explicitly set to zero.
    ZeroBandwidth,
    /// `ShadowTLS` supports only explicit versions 1 through 3.
    InvalidShadowTlsVersion,
}

impl NodeValidationError {
    /// Returns a concrete next action for the configuration owner.
    pub const fn suggested_action(self) -> &'static str {
        match self {
            Self::DuplicateTag => "deduplicate normalized node tags and retry",
            Self::ZeroBandwidth => "remove the bandwidth limit or set it above zero",
            Self::InvalidShadowTlsVersion => "set the ShadowTLS version to 1, 2, or 3",
        }
    }
}

impl fmt::Display for NodeValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::DuplicateTag => "node contains a duplicate normalized tag",
            Self::ZeroBandwidth => "node contains an explicit zero bandwidth limit",
            Self::InvalidShadowTlsVersion => "node contains an unsupported ShadowTLS version",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for NodeValidationError {}

pub(super) fn validate(protocol: &Protocol, tags: &NodeTags) -> Result<(), NodeValidationError> {
    validate_tags(tags)?;
    match protocol {
        Protocol::Hysteria2 {
            up_mbps, down_mbps, ..
        } if up_mbps == &Some(0) || down_mbps == &Some(0) => {
            Err(NodeValidationError::ZeroBandwidth)
        }
        Protocol::ShadowTls { version, .. } if !(1..=3).contains(version) => {
            Err(NodeValidationError::InvalidShadowTlsVersion)
        }
        _ => Ok(()),
    }
}

fn validate_tags(tags: &NodeTags) -> Result<(), NodeValidationError> {
    for (index, tag) in tags.iter().enumerate() {
        if tags[index + 1..].iter().any(|candidate| candidate == tag) {
            return Err(NodeValidationError::DuplicateTag);
        }
    }
    Ok(())
}
