//! Bounded handshake session registry for shared Tonic services.

use std::collections::HashMap;

use caly_domain::UnixMillis;
use caly_protocol::protocol::v2::{FeatureList, ProtocolVersion, WireId};

pub const MAX_PROTOCOL_SESSIONS: usize = 1_024;
/// Session lifetime in milliseconds before a token is considered expired.
pub const SESSION_TTL_MS: u64 = 5 * 60 * 1_000;

/// Returns the current wall-clock time as `UnixMillis`.
pub fn unix_millis() -> UnixMillis {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    UnixMillis::new(millis)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolSession {
    pub token: WireId,
    pub version: ProtocolVersion,
    pub features: FeatureList,
    pub expires_at: UnixMillis,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    CapacityReached,
    TokenCollision,
    InvalidToken,
    Missing,
    Expired,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            SessionError::CapacityReached => "session capacity reached",
            SessionError::TokenCollision => "session token collision",
            SessionError::InvalidToken => "invalid session token",
            SessionError::Missing => "session missing",
            SessionError::Expired => "session expired",
        };
        f.write_str(message)
    }
}

/// Single-owner bounded registry; entropy/token generation remains composition-owned.
pub struct SessionRegistry {
    sessions: HashMap<WireId, ProtocolSession>,
    capacity: usize,
}

impl SessionRegistry {
    pub fn new(capacity: usize) -> Result<Self, SessionError> {
        if capacity == 0 || capacity > MAX_PROTOCOL_SESSIONS {
            return Err(SessionError::CapacityReached);
        }
        Ok(Self {
            sessions: HashMap::with_capacity(capacity),
            capacity,
        })
    }

    pub fn insert(&mut self, session: ProtocolSession) -> Result<(), SessionError> {
        if session.token == [0; 16] {
            return Err(SessionError::InvalidToken);
        }
        if self.sessions.contains_key(&session.token) {
            return Err(SessionError::TokenCollision);
        }
        if self.sessions.len() == self.capacity {
            return Err(SessionError::CapacityReached);
        }
        self.sessions.insert(session.token, session);
        Ok(())
    }

    /// Registers a session, refreshing its expiry if the token is already held.
    ///
    /// The transport uses a shared daemon session token for the multi-process
    /// client model: every fresh client process handshakes and re-arm the same
    /// token, so an existing entry must be refreshed rather than rejected.
    pub fn refresh(&mut self, session: ProtocolSession) -> Result<(), SessionError> {
        if session.token == [0; 16] {
            return Err(SessionError::InvalidToken);
        }
        if !self.sessions.contains_key(&session.token) && self.sessions.len() == self.capacity {
            return Err(SessionError::CapacityReached);
        }
        self.sessions.insert(session.token, session);
        Ok(())
    }

    /// Validates and removes expired state before returning a session copy.
    pub fn validate(
        &mut self,
        token: WireId,
        now: UnixMillis,
    ) -> Result<ProtocolSession, SessionError> {
        let session = self.sessions.get(&token).ok_or(SessionError::Missing)?;
        if now >= session.expires_at {
            self.sessions.remove(&token);
            return Err(SessionError::Expired);
        }
        Ok(session.clone())
    }

    pub fn remove(&mut self, token: WireId) -> bool {
        self.sessions.remove(&token).is_some()
    }

    /// Returns the number of currently held sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns whether the registry currently holds no sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn purge_expired(&mut self, now: UnixMillis) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|_, session| now < session.expires_at);
        before.saturating_sub(self.sessions.len())
    }
}

/// Parses exactly 32 hexadecimal metadata characters.
pub fn parse_session_token(value: &str) -> Result<WireId, SessionError> {
    if value.len() != 32 {
        return Err(SessionError::InvalidToken);
    }
    let mut token = [0_u8; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = nibble(pair[0]).ok_or(SessionError::InvalidToken)?;
        let low = nibble(pair[1]).ok_or(SessionError::InvalidToken)?;
        token[index] = (high << 4) | low;
    }
    Ok(token)
}

const fn nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_client_cannot_reuse_another_unknown_token() -> Result<(), SessionError> {
        let mut registry = SessionRegistry::new(2)?;
        registry.insert(ProtocolSession {
            token: [1; 16],
            version: ProtocolVersion::V2_0,
            features: FeatureList::new(),
            expires_at: UnixMillis::new(100),
        })?;
        assert_eq!(
            registry.validate([2; 16], UnixMillis::new(1)),
            Err(SessionError::Missing)
        );
        assert!(registry.validate([1; 16], UnixMillis::new(1)).is_ok());
        Ok(())
    }

    #[test]
    fn expired_session_is_removed() -> Result<(), SessionError> {
        let mut registry = SessionRegistry::new(1)?;
        registry.insert(ProtocolSession {
            token: [3; 16],
            version: ProtocolVersion::V2_0,
            features: FeatureList::new(),
            expires_at: UnixMillis::new(5),
        })?;
        assert_eq!(
            registry.validate([3; 16], UnixMillis::new(5)),
            Err(SessionError::Expired)
        );
        assert_eq!(
            registry.validate([3; 16], UnixMillis::new(6)),
            Err(SessionError::Missing)
        );
        Ok(())
    }

    #[test]
    fn refresh_re_arms_an_existing_token() -> Result<(), SessionError> {
        let mut registry = SessionRegistry::new(1)?;
        let session = ProtocolSession {
            token: [7; 16],
            version: ProtocolVersion::V2_0,
            features: FeatureList::new(),
            expires_at: UnixMillis::new(5),
        };
        registry.insert(session.clone())?;
        // A repeated handshake must refresh rather than collide.
        let renewed = ProtocolSession {
            expires_at: UnixMillis::new(50),
            ..session
        };
        registry.refresh(renewed)?;
        assert_eq!(registry.len(), 1);
        assert!(registry.validate([7; 16], UnixMillis::new(10)).is_ok());
        Ok(())
    }

    #[test]
    fn refresh_rejects_zero_token() -> Result<(), SessionError> {
        let mut registry = SessionRegistry::new(1)?;
        assert_eq!(
            registry.refresh(ProtocolSession {
                token: [0; 16],
                version: ProtocolVersion::V2_0,
                features: FeatureList::new(),
                expires_at: UnixMillis::new(10),
            }),
            Err(SessionError::InvalidToken)
        );
        Ok(())
    }

    #[test]
    fn purge_expired_drops_only_stale_sessions() -> Result<(), SessionError> {
        let mut registry = SessionRegistry::new(2)?;
        registry.insert(ProtocolSession {
            token: [1; 16],
            version: ProtocolVersion::V2_0,
            features: FeatureList::new(),
            expires_at: UnixMillis::new(5),
        })?;
        registry.insert(ProtocolSession {
            token: [2; 16],
            version: ProtocolVersion::V2_0,
            features: FeatureList::new(),
            expires_at: UnixMillis::new(50),
        })?;
        assert_eq!(registry.purge_expired(UnixMillis::new(10)), 1);
        assert_eq!(registry.len(), 1);
        Ok(())
    }
}
