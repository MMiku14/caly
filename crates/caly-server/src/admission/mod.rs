//! Handshake admission for the network-facing transports.
//!
//! The owner-only UDS transport is guarded by the socket's file
//! permissions (0600) and peer credentials (see
//! [`crate::uds`]); it admits without a token. The TCP listener
//! (`daemon.listen`) is reachable by anything that can route to
//! the address, so when `daemon.auth_token` is configured the
//! daemon must demand the exact token in the handshake before
//! any command frame runs. This module is the single comparison
//! site so the policy cannot drift between the TCP and UDS
//! paths: UDS constructs an open admission, TCP a token one.

/// Admission policy for one service instance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TokenAdmission {
    /// No token is demanded (the UDS transport's default).
    #[default]
    Open,
    /// The handshake must carry this exact token.
    Token(String),
}

impl TokenAdmission {
    /// Builds the policy from the optional configured token; a
    /// blank token is rejected by the caller (the schema keeps
    /// `auth_token: ""` from silently opening the listener).
    pub fn from_configured(token: Option<String>) -> Self {
        token.map_or(Self::Open, Self::Token)
    }

    /// Whether admission is token-free (UDS default).
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    /// Admits a handshake presenting `presented`. A token-backed
    /// policy rejects a missing token outright; a presented token
    /// is compared in full without early byte-exit so the timing
    /// side channel stays negligible for remote probes.
    pub fn admit(&self, presented: Option<&str>) -> bool {
        match self {
            Self::Open => true,
            Self::Token(expected) => {
                presented.is_some_and(|presented| keys_equal(expected, presented))
            }
        }
    }
}

/// Whole-string comparison with a fixed byte fold: the running
/// difference accumulator never short-circuits on the first
/// mismatched byte, unlike `==` on `str`, which exits as soon as
/// a difference is found.
fn keys_equal(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    if expected.len() != presented.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in expected.iter().zip(presented) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_admits_everything() {
        let admission = TokenAdmission::Open;
        assert!(admission.is_open());
        assert!(admission.admit(None));
        assert!(admission.admit(Some("anything")));
    }

    #[test]
    fn token_policy_requires_an_exact_match() {
        let admission = TokenAdmission::Token("s3cret".to_owned());
        assert!(!admission.is_open());
        assert!(!admission.admit(None));
        assert!(!admission.admit(Some("wrong")));
        assert!(!admission.admit(Some("s3cret2")));
        assert!(!admission.admit(Some("S3cret")));
        assert!(admission.admit(Some("s3cret")));
    }

    #[test]
    fn from_configured_maps_none_to_open() {
        assert_eq!(TokenAdmission::from_configured(None), TokenAdmission::Open);
        assert_eq!(
            TokenAdmission::from_configured(Some("t".to_owned())),
            TokenAdmission::Token("t".to_owned())
        );
    }
}
