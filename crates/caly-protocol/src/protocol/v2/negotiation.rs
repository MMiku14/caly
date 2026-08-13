//! Deterministic handshake negotiation.

use core::fmt;

use super::{
    DecodeLimits, FeatureList, HandshakeRequest, HandshakeResponse, ProtocolVersion, RawFeature,
    WireId,
};

/// Handshake failure before any application request is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeError {
    IncompatibleMajor { client: u16, server: u16 },
    FeatureCapacityExceeded,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompatibleMajor { client, server } => write!(
                formatter,
                "client protocol major {client} is incompatible with server major {server}; upgrade the older peer"
            ),
            Self::FeatureCapacityExceeded => formatter.write_str(
                "negotiated feature list exceeded its bound; reduce advertised features",
            ),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Negotiates version/features while preserving unknown requests.
pub fn negotiate_handshake(
    request: HandshakeRequest,
    server_version: ProtocolVersion,
    supported: &FeatureList,
    daemon_instance_id: WireId,
    session_token: WireId,
    limits: DecodeLimits,
) -> Result<HandshakeResponse, HandshakeError> {
    let version = server_version.negotiate(request.client_version).ok_or(
        HandshakeError::IncompatibleMajor {
            client: request.client_version.major,
            server: server_version.major,
        },
    )?;
    let mut enabled = FeatureList::new();
    let mut unknown = FeatureList::new();
    for requested in &request.requested_features {
        if requested.known().is_none() {
            push_feature(&mut unknown, *requested)?;
        } else if supported.iter().any(|candidate| candidate == requested) {
            push_feature(&mut enabled, *requested)?;
        }
    }
    Ok(HandshakeResponse {
        negotiated_version: version,
        daemon_instance_id,
        enabled_features: enabled,
        unknown_requested_features: unknown,
        limits,
        session_token,
    })
}

fn push_feature(target: &mut FeatureList, value: RawFeature) -> Result<(), HandshakeError> {
    target
        .try_push(value)
        .map_err(|_| HandshakeError::FeatureCapacityExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v2::{Feature, all_features};

    #[test]
    fn unknown_feature_is_preserved_not_enabled() -> Result<(), Box<dyn std::error::Error>> {
        let request = HandshakeRequest {
            client_version: ProtocolVersion::V2_0,
            auth_token: None,
            requested_features: FeatureList::try_from_vec(vec![RawFeature(999)])?,
        };
        let response = negotiate_handshake(
            request,
            ProtocolVersion::V2_0,
            &FeatureList::new(),
            [1; 16],
            [2; 16],
            DecodeLimits::v2_default(),
        )?;
        assert!(response.enabled_features.is_empty());
        assert_eq!(
            response.unknown_requested_features.as_slice(),
            &[RawFeature(999)]
        );
        Ok(())
    }

    #[test]
    fn all_features_advertises_every_implemented_capability() {
        let features = all_features();
        assert_eq!(features.len(), 5);
        for feature in [
            Feature::Operations,
            Feature::OperationCancellation,
            Feature::EventReplay,
            Feature::FullSnapshotRecovery,
            Feature::UnknownEnumPreservation,
        ] {
            assert!(
                features.iter().any(|raw| raw.known() == Some(feature)),
                "missing advertised feature {feature:?}"
            );
        }
    }

    #[test]
    fn requested_capabilities_are_enabled_when_supported() -> Result<(), Box<dyn std::error::Error>>
    {
        let request = HandshakeRequest {
            client_version: ProtocolVersion::V2_0,
            auth_token: None,
            requested_features: all_features(),
        };
        let response = negotiate_handshake(
            request,
            ProtocolVersion::V2_0,
            &all_features(),
            [1; 16],
            [2; 16],
            DecodeLimits::v2_default(),
        )?;
        assert_eq!(response.enabled_features.len(), 5);
        assert!(response.unknown_requested_features.is_empty());
        Ok(())
    }
}
