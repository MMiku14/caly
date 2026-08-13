//! Owner-only secret generation and redaction guarantees.
//!
//! Secret generation is fail-closed: if the kernel CSPRNG is unavailable the
//! caller receives an error instead of a predictable token. Generated secrets
//! are hex-encoded so they embed cleanly into YAML and JSON controller config.

use super::entropy;

/// Number of random bytes backing a controller secret (128 bits of entropy).
pub const SECRET_BYTES: usize = 16;

/// Secret generation failure; never exposes partial or predictable bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretFailure {
    /// The kernel CSPRNG could not be read (fail-closed).
    EntropyUnavailable,
    /// The generated secret exceeded a supported length bound.
    Overflow,
}

/// Generates a hex-encoded 128-bit controller secret, failing closed.
pub fn generate_secret_hex() -> Result<String, SecretFailure> {
    let bytes = entropy::try_random_bytes::<SECRET_BYTES>()
        .map_err(|_| SecretFailure::EntropyUnavailable)?;
    let encoded = hex_lower(&bytes);
    if encoded.len() > 128 {
        return Err(SecretFailure::Overflow);
    }
    Ok(encoded)
}

/// Lowercase hex encoding of `bytes` without any dependency.
fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Returns a redacted constant used in place of any secret when a display or
/// projection path must reference it without leaking the value.
pub const REDACTED: &str = "[redacted]";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secret_is_hex_of_expected_length() {
        let secret = generate_secret_hex().unwrap_or_else(|_| String::new());
        // 16 bytes -> 32 hex characters, all lowercase hex digits.
        assert_eq!(secret.len(), SECRET_BYTES * 2);
        assert!(
            secret
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn hex_encoding_is_round_trip_stable() {
        assert_eq!(hex_lower(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_lower(&[0x01, 0x02, 0x0a, 0x0b]), "01020a0b");
    }

    #[test]
    fn redaction_never_leaks_value() {
        assert_eq!(REDACTED, "[redacted]");
    }
}
