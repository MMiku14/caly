//! Bounded text values used at domain boundaries.

use core::{fmt, marker::PhantomData};

use serde::{de, Deserialize, Deserializer};

/// Error returned when text violates a domain size invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextError {
    /// The value is empty even though content is required.
    Empty,
    /// The UTF-8 byte length exceeds the declared capacity.
    TooLong {
        /// Maximum accepted UTF-8 byte length.
        max_bytes: usize,
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
    },
    /// The value is non-empty but violates a structural rule that
    /// the size invariant does not express (e.g. an identifier that
    /// must be path-safe). Callers attach the specific rule in the
    /// error path that emits the variant.
    Invalid,
}

impl fmt::Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("text must not be empty"),
            Self::TooLong {
                max_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "text is {actual_bytes} bytes; reduce it to at most {max_bytes} bytes"
            ),
            Self::Invalid => formatter.write_str("text violates a structural rule"),
        }
    }
}

impl std::error::Error for TextError {}

/// Non-empty UTF-8 text with a compile-time byte capacity.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BoundedText<const MAX_BYTES: usize>(String);

impl<const MAX_BYTES: usize> BoundedText<MAX_BYTES> {
    /// Validates and owns a bounded value.
    pub fn new(value: impl Into<String>) -> Result<Self, TextError> {
        let value = value.into();
        if value.is_empty() {
            return Err(TextError::Empty);
        }
        if value.len() > MAX_BYTES {
            return Err(TextError::TooLong {
                max_bytes: MAX_BYTES,
                actual_bytes: value.len(),
            });
        }
        Ok(Self(value))
    }

    /// Infallibly owns display text: empty input falls back to `fallback`, and
    /// over-long input is truncated at a UTF-8 character boundary so a runtime
    /// value can never abort the daemon. Intended for task names, phase labels
    /// and error reasons — never for values whose truncation would change
    /// meaning.
    pub fn from_nonempty_clamped(value: String, fallback: &'static str) -> Self {
        if value.is_empty() {
            return Self(fallback.to_owned());
        }
        let mut truncated = value;
        if truncated.len() > MAX_BYTES {
            truncated.truncate(MAX_BYTES);
            while !truncated.is_char_boundary(truncated.len()) {
                truncated.pop();
            }
        }
        Self(truncated)
    }

    /// Borrows the validated text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the UTF-8 byte length.
    pub fn len_bytes(&self) -> usize {
        self.0.len()
    }
}

impl<const MAX_BYTES: usize> fmt::Debug for BoundedText<MAX_BYTES> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("BoundedText").field(&self.0).finish()
    }
}

impl<const MAX_BYTES: usize> fmt::Display for BoundedText<MAX_BYTES> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de, const MAX_BYTES: usize> Deserialize<'de> for BoundedText<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(BoundedTextVisitor::<MAX_BYTES>(PhantomData))
    }
}

impl<const MAX_BYTES: usize> serde::Serialize for BoundedText<MAX_BYTES> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

struct BoundedTextVisitor<const MAX_BYTES: usize>(PhantomData<()>);

impl<const MAX_BYTES: usize> de::Visitor<'_> for BoundedTextVisitor<MAX_BYTES> {
    type Value = BoundedText<MAX_BYTES>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "non-empty UTF-8 text of at most {MAX_BYTES} bytes"
        )
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        BoundedText::new(value.to_owned()).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        BoundedText::new(value).map_err(E::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedText, TextError};

    #[test]
    fn rejects_empty_and_oversized_values() {
        assert_eq!(BoundedText::<4>::new(""), Err(TextError::Empty));
        assert_eq!(
            BoundedText::<4>::new("12345"),
            Err(TextError::TooLong {
                max_bytes: 4,
                actual_bytes: 5,
            })
        );
    }

    #[test]
    fn capacity_counts_utf8_bytes() {
        assert!(BoundedText::<3>::new("猫").is_ok());
        assert!(BoundedText::<2>::new("猫").is_err());
    }

    #[test]
    fn deserialization_reuses_text_invariants() {
        let empty = serde_json::from_str::<BoundedText<4>>("\"\"");
        let oversized = serde_json::from_str::<BoundedText<4>>("\"12345\"");
        assert!(empty.is_err());
        assert!(oversized.is_err());
    }
}
