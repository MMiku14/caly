//! Operation wire DTOs preserving raw enum values.

use super::{WireId, WirePayload};

/// Known v2 mutation command categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    ApplyConfig,
    SwitchCore,
    SelectProxy,
    /// W4 (cli-v3-design.md §12): in-group member pick inside a
    /// selector group (`node pick <group> <member> --apply`).
    /// Wire discriminant 11, appended after the frozen 1–10 set.
    SelectProxyGroup,
    /// PAC mode for the desktop system proxy (`sysproxy pac`): the
    /// desktop is switched to its `auto` mode pointing at a PAC URL
    /// (caly-generated `file://` or an operator-supplied one). Wire
    /// discriminant 12, appended after the frozen 1–11 set.
    SetSystemProxyPac,
    SetMode,
    SetTun,
    SetSystemProxy,
    RefreshSubscription,
    CloseConnections,
    /// Round 17: daemon-side shutdown request. The
    /// server responds with the operation status, then
    /// breaks its `serve()` loop and exits 0.
    StopDaemon,
    /// Round 17: daemon-side reload request. The
    /// server re-reads `config.yaml` and re-applies
    /// the current candidate (synonym for `ApplyConfig`
    /// with the latest `candidate_id`).
    ReloadConfig,
}

/// Raw command discriminant; unknown values remain representable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawCommandKind(pub i32);

impl RawCommandKind {
    /// Converts a known command without fabricating a fallback.
    pub const fn known(self) -> Option<CommandKind> {
        match self.0 {
            1 => Some(CommandKind::ApplyConfig),
            2 => Some(CommandKind::SwitchCore),
            3 => Some(CommandKind::SelectProxy),
            4 => Some(CommandKind::SetMode),
            5 => Some(CommandKind::SetTun),
            6 => Some(CommandKind::SetSystemProxy),
            7 => Some(CommandKind::RefreshSubscription),
            8 => Some(CommandKind::CloseConnections),
            9 => Some(CommandKind::StopDaemon),
            10 => Some(CommandKind::ReloadConfig),
            // W4: `node pick` in-group member selection (wire 11, appended
            // after the frozen 1–10 set — never renumber existing values).
            11 => Some(CommandKind::SelectProxyGroup),
            // `sysproxy pac` (2026-08-12).
            12 => Some(CommandKind::SetSystemProxyPac),
            _ => None,
        }
    }
}
/// Raw operation-state discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawOperationState(pub i32);

impl RawOperationState {
    /// Typed view of the discriminant; unknown values stay untyped.
    pub const fn known(self) -> Option<super::wire::WireOperationState> {
        super::wire::WireOperationState::from_wire(self.0)
    }

    /// Terminal states end the operation lifecycle permanently.
    pub const fn is_terminal(self) -> bool {
        match super::wire::WireOperationState::from_wire(self.0) {
            Some(state) => state.is_terminal(),
            None => false,
        }
    }

    /// Presentation label; unknown values render as `unknown(<raw>)`.
    pub fn label(self) -> String {
        self.known().map_or_else(
            || format!("unknown({})", self.0),
            |state| state.label().to_owned(),
        )
    }
}
/// Raw operation-failure discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawFailureCode(pub i32);
/// Raw cancellation outcome discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawCancelOutcome(pub i32);

/// Known cancellation result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelOutcome {
    Requested,
    AlreadyTerminal,
    TooLateToCancel,
}

impl RawCancelOutcome {
    /// Converts a known value without selecting a fallback.
    pub const fn known(self) -> Option<CancelOutcome> {
        match self.0 {
            1 => Some(CancelOutcome::Requested),
            2 => Some(CancelOutcome::AlreadyTerminal),
            3 => Some(CancelOutcome::TooLateToCancel),
            _ => None,
        }
    }
}

/// Typed mutation payload; unknown future commands preserve kind and bytes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WireCommand {
    ApplyConfig {
        #[serde(with = "crate::json_serde::hex_id")]
        candidate_id: WireId,
    },
    SwitchCore {
        core_kind: i32,
        action: i32,
    },
    SelectProxy {
        #[serde(with = "crate::json_serde::hex_id")]
        node_id: WireId,
    },
    /// W4: pick a named member inside a named proxy group. Names
    /// travel verbatim (the offline entry tree is the authority for
    /// group/member spelling); the daemon resolves kernel-side tags.
    SelectProxyGroup {
        group: String,
        member: String,
    },
    SetMode {
        mode: i32,
    },
    SetTun {
        enabled: bool,
    },
    SetSystemProxy {
        enabled: bool,
    },
    /// `sysproxy pac <url>`: switch the desktop proxy to auto mode with
    /// the given PAC URL. The URL travels verbatim (`file://` or
    /// `http(s)://`); the backend decides how to consume it.
    SetSystemProxyPac {
        url: String,
    },
    RefreshSubscription {
        #[serde(with = "crate::json_serde::hex_id")]
        subscription_id: WireId,
        /// W2-β2b (`sub refresh --force`): refetch the body even
        /// when the stored validators would answer 304. Defaults
        /// false so pre-β2b clients keep their exact encoding.
        #[serde(default)]
        force: bool,
    },
    CloseAllConnections,
    /// Round 17: server-side shutdown request.
    /// No payload. The server returns the operation
    /// status (which completes immediately), then
    /// breaks its serve loop and exits.
    StopDaemon,
    /// Round 17: server-side reload request. No
    /// payload. The server re-reads `config.yaml`
    /// and re-applies the current candidate.
    ReloadConfig,
    Unknown {
        raw_kind: RawCommandKind,
        payload: WirePayload,
    },
}

/// Idempotent mutation request.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecuteRequest {
    #[serde(with = "crate::json_serde::hex_id")]
    pub operation_id: WireId,
    pub command: WireCommand,
}

/// Immediate operation acknowledgement, not assumed completion.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecuteResponse {
    pub operation: WireOperationStatus,
}
/// Explicit cancellation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelOperationRequest {
    #[serde(with = "crate::json_serde::hex_id")]
    pub operation_id: WireId,
}
/// Cancellation acknowledgement with current operation state.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelOperationResponse {
    pub operation: WireOperationStatus,
    pub outcome: RawCancelOutcome,
}
/// Status lookup after timeout, reconnect, or duplicate submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetOperationStatusRequest {
    #[serde(with = "crate::json_serde::hex_id")]
    pub operation_id: WireId,
}

/// Bounded conversion is required before this wire failure enters Domain.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireOperationFailure {
    pub code: RawFailureCode,
    pub message: String,
    pub suggested_action: String,
}

/// Operation status transported without interpreting unknown enum values.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireOperationStatus {
    #[serde(with = "crate::json_serde::hex_id")]
    pub operation_id: WireId,
    pub command_kind: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub state: RawOperationState,
    pub failure: Option<WireOperationFailure>,
}
