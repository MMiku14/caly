//! JSON wire envelopes for the v2 local control plane.
//!
//! The wire DTOs in `protocol/v2` are transported directly as compact JSON.
//! A request frame carries an optional 32-hex-char session token; a response
//! frame is either a typed result or a structured error with a stable code.
//! Watch events stream as repeated `WatchEvent` result frames on a dedicated
//! connection until the peer disconnects.

use super::protocol::v2::{
    CancelOperationRequest, CancelOperationResponse, ExecuteRequest, ExecuteResponse,
    GetOperationStatusRequest, GetSnapshotRequest, HandshakeRequest, HandshakeResponse,
    WatchEventsRequest, WatchResponse, WireOperationStatus, WirePresentationSnapshot,
};

/// Every daemon method, one discriminant per frame.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonRequest {
    Handshake(HandshakeRequest),
    Execute(ExecuteRequest),
    CancelOperation(CancelOperationRequest),
    GetOperationStatus(GetOperationStatusRequest),
    GetSnapshot(GetSnapshotRequest),
    WatchEvents(WatchEventsRequest),
}

/// Typed daemon reply, mirroring `JsonRequest` one-to-one.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonResponse {
    Handshake(HandshakeResponse),
    Execute(ExecuteResponse),
    CancelOperation(CancelOperationResponse),
    GetOperationStatus(WireOperationStatus),
    GetSnapshot(WirePresentationSnapshot),
    WatchEvent(WatchResponse),
}

/// Client-to-daemon envelope: optional session token plus one request.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientFrame {
    /// 32-hex-char session token; absent only for the handshake.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub request: JsonRequest,
}

/// Daemon-to-client envelope: exactly one result or one structured error.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ServerFrame {
    /// Typed success payload; boxed to keep the frame small on the stack.
    Result(Box<JsonResponse>),
    Error(WireError),
}

/// Structured transport error with a stable numeric code.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireError {
    pub code: u32,
    pub message: String,
    pub suggested_action: Option<String>,
}

/// Stable wire error discriminants shared by daemon and thin clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireErrorCode {
    Unauthenticated,
    PermissionDenied,
    ResourceExhausted,
    InvalidArgument,
    IncompatibleVersion,
    HandshakeRequired,
    ApplicationUnavailable,
}

impl WireErrorCode {
    /// Numeric wire representation.
    pub const fn code(self) -> u32 {
        match self {
            Self::Unauthenticated => 1,
            Self::PermissionDenied => 2,
            Self::ResourceExhausted => 3,
            Self::InvalidArgument => 4,
            Self::IncompatibleVersion => 5,
            Self::HandshakeRequired => 6,
            Self::ApplicationUnavailable => 7,
        }
    }

    /// Strict numeric decode; unknown values are rejected.
    pub const fn from_code(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Unauthenticated),
            2 => Some(Self::PermissionDenied),
            3 => Some(Self::ResourceExhausted),
            4 => Some(Self::InvalidArgument),
            5 => Some(Self::IncompatibleVersion),
            6 => Some(Self::HandshakeRequired),
            7 => Some(Self::ApplicationUnavailable),
            _ => None,
        }
    }
}

/// Builds a structured wire error from a known discriminant.
pub fn wire_error(code: WireErrorCode, message: impl Into<String>) -> WireError {
    WireError {
        code: code.code(),
        message: message.into(),
        suggested_action: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v2::{DecodeLimits, FeatureList, ProtocolVersion};

    #[test]
    fn request_frames_round_trip_without_session() -> Result<(), String> {
        let frame = ClientFrame {
            session: None,
            request: JsonRequest::Handshake(HandshakeRequest {
                client_version: ProtocolVersion::V2_0,
                auth_token: None,
                requested_features: FeatureList::new(),
            }),
        };
        let json = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
        assert!(
            !json.contains("session"),
            "absent session must not be serialized"
        );
        let back: ClientFrame = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert_eq!(frame, back);
        Ok(())
    }

    #[test]
    fn request_frames_carry_hex_session_token() -> Result<(), String> {
        let frame = ClientFrame {
            session: Some("0102030405060708090a0b0c0d0e0f10".to_owned()),
            request: JsonRequest::GetSnapshot(GetSnapshotRequest),
        };
        let json = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
        assert!(json.contains("\"session\":\"0102030405060708090a0b0c0d0e0f10\""));
        let back: ClientFrame = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert_eq!(frame, back);
        Ok(())
    }

    #[test]
    fn error_frames_round_trip_with_known_code() -> Result<(), String> {
        let frame = ServerFrame::Error(wire_error(
            WireErrorCode::ResourceExhausted,
            "bounded resource exhausted",
        ));
        let json = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
        let back: ServerFrame = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert_eq!(frame, back);
        assert_eq!(
            WireErrorCode::from_code(3),
            Some(WireErrorCode::ResourceExhausted)
        );
        assert_eq!(WireErrorCode::from_code(999), None);
        Ok(())
    }

    #[test]
    fn snapshot_round_trips_through_json() -> Result<(), String> {
        let snapshot = WirePresentationSnapshot {
            daemon_instance_id: [7; 16],
            revision: 3,
            cursor: crate::protocol::v2::WireEventCursor {
                daemon_instance_id: [8; 16],
                sequence: 11,
            },
            desired: crate::protocol::v2::WireDesiredState {
                mode: 1,
                selected_node_id: Some([9; 16]),
                active_subscription_id: None,
                tun_requested: false,
                system_proxy_requested: false,
            },
            applied: crate::protocol::v2::WireAppliedState {
                core_kind: Some(1),
                run_state: 3,
                selected_node_id: None,
                config_generation: None,
            },
            observed: crate::protocol::v2::WireObservedState {
                upload_bytes_per_second: 0,
                download_bytes_per_second: 0,
                active_connections: 0,
                telemetry_dropped: 0,
                core_restart_count: 0,
                core_restart_backoff_ms: 0,
            },
            platform: crate::protocol::v2::WirePlatformEffect {
                proxy_engaged: false,
                tun_engaged: false,
                recovery_pending: false,
                degraded_reason: None,
            },
            capabilities: caly_domain::BoundedVec::new(),
            nodes: caly_domain::BoundedVec::new(),
            proxy_groups: caly_domain::BoundedVec::new(),
        };
        let json = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
        let back: WirePresentationSnapshot =
            serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert_eq!(snapshot, back);
        Ok(())
    }

    #[test]
    fn decode_limits_negotiate_round_trip() -> Result<(), String> {
        let limits = DecodeLimits::v2_default();
        let json = serde_json::to_string(&limits).map_err(|e| e.to_string())?;
        let back: DecodeLimits = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        assert_eq!(limits, back);
        Ok(())
    }
}
