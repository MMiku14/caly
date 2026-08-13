//! Opaque 128-bit identities.

use core::{fmt, str::FromStr};

/// Number of bytes in every externally represented caly identity.
pub const ID_BYTES: usize = 16;
/// Number of lowercase hexadecimal characters in an identity.
pub const ID_HEX_LENGTH: usize = ID_BYTES * 2;

/// Error returned when parsing an identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityParseError {
    /// The input does not contain exactly 32 hexadecimal characters.
    InvalidLength,
    /// The input contains a non-hexadecimal character.
    InvalidHex,
}

impl fmt::Display for IdentityParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => formatter.write_str("identity must contain 32 hex characters"),
            Self::InvalidHex => formatter.write_str("identity contains a non-hex character"),
        }
    }
}

impl std::error::Error for IdentityParseError {}

macro_rules! opaque_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; ID_BYTES]);

        impl $name {
            /// Constructs an ID from its canonical 16-byte representation.
            pub const fn from_bytes(bytes: [u8; ID_BYTES]) -> Self {
                Self(bytes)
            }

            /// Returns the canonical byte representation.
            pub const fn into_bytes(self) -> [u8; ID_BYTES] {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({self})", stringify!($name))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl FromStr for $name {
            type Err = IdentityParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                parse_hex(value).map(Self)
            }
        }
    };
}

opaque_id!(DaemonInstanceId, "Identifies one daemon lifetime epoch.");
opaque_id!(OperationId, "Identifies an idempotent mutation operation.");
opaque_id!(
    NodeId,
    "Identifies a node by its complete canonical identity."
);
opaque_id!(
    SubscriptionId,
    "Identifies a subscription without retaining its URL."
);

fn parse_hex(value: &str) -> Result<[u8; ID_BYTES], IdentityParseError> {
    if value.len() != ID_HEX_LENGTH {
        return Err(IdentityParseError::InvalidLength);
    }
    let mut output = [0_u8; ID_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = parse_byte(pair)?;
    }
    Ok(output)
}

/// Canonical lowercase hex encoding of a 16-byte identity (32 hex chars).
/// This is the single shared implementation for the repeated per-crate hex
/// helpers, so identity serialization stays consistent and dependency-free.
pub fn to_hex(bytes: [u8; ID_BYTES]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(ID_HEX_LENGTH);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn parse_byte(pair: &[u8]) -> Result<u8, IdentityParseError> {
    let high = hex_nibble(pair[0]).ok_or(IdentityParseError::InvalidHex)?;
    let low = hex_nibble(pair[1]).ok_or(IdentityParseError::InvalidHex)?;
    Ok((high << 4) | low)
}

/// Maps one ASCII hex character to its 0–15 value.
pub const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{to_hex, IdentityParseError, NodeId};
    use core::str::FromStr;

    #[test]
    fn to_hex_matches_canonical_lowercase() {
        let bytes = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(to_hex(bytes), "00112233445566778899aabbccddeeff");
        assert_eq!(to_hex(bytes).len(), 32);
    }

    #[test]
    fn to_hex_agrees_with_display() {
        let node = NodeId::from_bytes([7; 16]);
        assert_eq!(to_hex(node.into_bytes()), node.to_string());
    }

    #[test]
    fn identity_round_trips_as_lowercase_hex() {
        let input = "00112233445566778899AABBCCDDEEFF";
        let parsed = NodeId::from_str(input);
        assert_eq!(
            parsed.map(|value| value.to_string()),
            Ok("00112233445566778899aabbccddeeff".to_owned())
        );
    }

    #[test]
    fn identity_rejects_invalid_input() {
        assert_eq!(
            NodeId::from_str("00"),
            Err(IdentityParseError::InvalidLength)
        );
        assert_eq!(
            NodeId::from_str("00112233445566778899aabbccddeefg"),
            Err(IdentityParseError::InvalidHex)
        );
    }
}
