//! Secret value objects with redacted diagnostics.

use core::{fmt, marker::PhantomData};

use serde::{de, Deserialize, Deserializer};

/// Error returned when constructing a secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretError {
    /// Empty credentials are not accepted.
    Empty,
    /// The secret exceeds the bounded in-memory representation.
    TooLong {
        /// Maximum accepted UTF-8 byte length.
        max_bytes: usize,
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
    },
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("secret must not be empty"),
            Self::TooLong {
                max_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "secret is {actual_bytes} bytes; reduce it to at most {max_bytes} bytes"
            ),
        }
    }
}

impl std::error::Error for SecretError {}

/// An owned secret that never reveals content through `Debug` or `Display`.
///
/// This type deliberately does not implement `Clone`, `AsRef<str>`, generic
/// serialization, or conversion back into `String`. Exposure is scoped to a
/// closure so callers cannot retain a direct borrow beyond that invocation.
pub struct SecretText<const MAX_BYTES: usize>(String);

impl<const MAX_BYTES: usize> SecretText<MAX_BYTES> {
    /// Validates and owns a secret value.
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretError::Empty);
        }
        if value.len() > MAX_BYTES {
            return Err(SecretError::TooLong {
                max_bytes: MAX_BYTES,
                actual_bytes: value.len(),
            });
        }
        Ok(Self(value))
    }

    /// Invokes a closure with temporary access to the secret.
    pub fn with_exposed<T>(&self, operation: impl FnOnce(&str) -> T) -> T {
        operation(&self.0)
    }

    /// Returns only the UTF-8 byte length, never the content.
    pub fn len_bytes(&self) -> usize {
        self.0.len()
    }
}

impl<const MAX_BYTES: usize> fmt::Debug for SecretText<MAX_BYTES> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretText([REDACTED])")
    }
}

impl<const MAX_BYTES: usize> fmt::Display for SecretText<MAX_BYTES> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl<'de, const MAX_BYTES: usize> Deserialize<'de> for SecretText<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(SecretVisitor::<MAX_BYTES>(PhantomData))
    }
}

struct SecretVisitor<const MAX_BYTES: usize>(PhantomData<()>);

impl<const MAX_BYTES: usize> de::Visitor<'_> for SecretVisitor<MAX_BYTES> {
    type Value = SecretText<MAX_BYTES>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a non-empty secret of at most {MAX_BYTES} bytes")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        SecretText::new(value.to_owned()).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        SecretText::new(value).map_err(E::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::SecretText;

    #[test]
    fn diagnostics_are_redacted() {
        let result = SecretText::<64>::new("credential");
        assert_eq!(
            result.map(|value| format!("{value:?}|{value}")),
            Ok("SecretText([REDACTED])|[REDACTED]".to_owned())
        );
    }

    #[test]
    fn exposure_is_explicit() {
        let result = SecretText::<64>::new("credential");
        assert_eq!(result.map(|value| value.with_exposed(str::len)), Ok(10));
    }

    #[test]
    fn deserialization_enforces_secret_capacity() {
        let result = serde_json::from_str::<SecretText<4>>("\"12345\"");
        assert!(result.is_err());
    }
}
