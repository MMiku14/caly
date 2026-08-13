//! Serde helpers for the JSON wire representation.
//!
//! 128-bit wire identities are transported as 32-character lowercase hex
//! strings so the on-wire JSON stays human-readable and debuggable (the
//! protocol's replacement goal for binary gRPC framing).

use caly_domain::hex_nibble;
use serde::{Deserialize, Deserializer};

/// Strict 32-hex-char identity parser; rejects any other spelling.
fn parse_hex(value: &str) -> Result<[u8; 16], String> {
    if value.len() != 32 {
        return Err(format!(
            "identity must be exactly 32 hex characters, got {}",
            value.len()
        ));
    }
    let mut bytes = [0_u8; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])
            .ok_or_else(|| "identity contains a non-hex character".to_owned())?;
        let low = hex_nibble(pair[1])
            .ok_or_else(|| "identity contains a non-hex character".to_owned())?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

/// `[u8; 16]` identity as a hex string.
pub mod hex_id {
    use super::{Deserialize, Deserializer, parse_hex};
    use serde::Serializer;

    pub fn serialize<S>(value: &[u8; 16], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&caly_domain::to_hex(*value))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 16], D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        parse_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// `Option<[u8; 16]>` identity: `null` or a 32-character hex string.
pub mod hex_id_opt {
    use super::{Deserialize, Deserializer, hex_id, parse_hex};
    use serde::Serializer;

    pub fn serialize<S>(value: &Option<[u8; 16]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(id) => hex_id::serialize(id, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 16]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = Option::<String>::deserialize(deserializer)?;
        text.map(|value| parse_hex(&value).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::{hex_id, hex_id_opt};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Sample {
        #[serde(with = "hex_id")]
        id: [u8; 16],
        #[serde(with = "hex_id_opt")]
        maybe: Option<[u8; 16]>,
    }

    #[test]
    fn ids_round_trip_as_hex_strings() {
        let value = Sample {
            id: [0xAB; 16],
            maybe: Some([1; 16]),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"id\":\"abababababababababababababababab\""));
        let back: Sample = serde_json::from_str(&json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn missing_optional_id_serializes_as_null() {
        let value = Sample {
            id: [2; 16],
            maybe: None,
        };
        let json = serde_json::to_string(&value).unwrap();
        let back: Sample = serde_json::from_str(&json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn malformed_hex_is_rejected() {
        let bad = r#"{"id":"not-hex","maybe":null}"#;
        assert!(serde_json::from_str::<Sample>(bad).is_err());
        let short = r#"{"id":"ab","maybe":null}"#;
        assert!(serde_json::from_str::<Sample>(short).is_err());
    }
}
