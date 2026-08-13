//! Typed wire-discriminant mappings: the single source of truth for the integer
//! semantics shared by daemon encoding, server decoding and thin clients.
//!
//! Every integer↔enum↔label conversion lives here exactly once. Encoders,
//! decoders and presentation layers derive from these types instead of
//! re-implementing matches, so a discriminant change is a one-site edit that
//! the compiler enforces everywhere.

/// Routing mode discriminant (`WireCommand::SetMode`, desired-state mode).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireMode {
    Rule,
    Global,
    Direct,
}

impl WireMode {
    pub const fn wire(self) -> i32 {
        match self {
            Self::Rule => 1,
            Self::Global => 2,
            Self::Direct => 3,
        }
    }

    pub const fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Rule),
            2 => Some(Self::Global),
            3 => Some(Self::Direct),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    /// Parses the canonical CLI/config label.
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "rule" => Some(Self::Rule),
            "global" => Some(Self::Global),
            "direct" => Some(Self::Direct),
            _ => None,
        }
    }
}

/// Managed-core discriminant (`SwitchCore`, applied-state core).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireCoreKind {
    Mihomo,
    SingBox,
    Xray,
}

impl WireCoreKind {
    pub const fn wire(self) -> i32 {
        match self {
            Self::Mihomo => 1,
            Self::SingBox => 2,
            Self::Xray => 3,
        }
    }

    pub const fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Mihomo),
            2 => Some(Self::SingBox),
            3 => Some(Self::Xray),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Mihomo => "mihomo",
            Self::SingBox => "sing-box",
            Self::Xray => "xray",
        }
    }
}

/// Core lifecycle action discriminant (`SwitchCore.action`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireCoreAction {
    Start,
    Stop,
    Restart,
}

impl WireCoreAction {
    pub const fn wire(self) -> i32 {
        match self {
            Self::Start => 1,
            Self::Stop => 2,
            Self::Restart => 3,
        }
    }

    pub const fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Start),
            2 => Some(Self::Stop),
            3 => Some(Self::Restart),
            _ => None,
        }
    }
}

/// Core run-state discriminant (applied state).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireRunState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
    Failed,
}

impl WireRunState {
    pub const fn wire(self) -> i32 {
        match self {
            Self::Stopped => 1,
            Self::Starting => 2,
            Self::Running => 3,
            Self::Stopping => 4,
            Self::Crashed => 5,
            Self::Failed => 6,
        }
    }

    pub const fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Stopped),
            2 => Some(Self::Starting),
            3 => Some(Self::Running),
            4 => Some(Self::Stopping),
            5 => Some(Self::Crashed),
            6 => Some(Self::Failed),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Crashed => "crashed",
            Self::Failed => "failed",
        }
    }
}

/// Operation lifecycle discriminant (`WireOperationStatus.state`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireOperationState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl WireOperationState {
    pub const fn wire(self) -> i32 {
        match self {
            Self::Pending => 1,
            Self::Running => 2,
            Self::Completed => 3,
            Self::Failed => 4,
            Self::Cancelled => 5,
        }
    }

    pub const fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Pending),
            2 => Some(Self::Running),
            3 => Some(Self::Completed),
            4 => Some(Self::Failed),
            5 => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Terminal states end an operation's lifecycle permanently.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Domain↔wire bridges. The protocol crate already owns the conversion layer,
/// so these live beside the raw discriminant types they explain.
mod domain_bridge {
    use super::{WireCoreKind, WireMode, WireOperationState, WireRunState};
    use caly_domain::{CoreKind, CoreRunState, OperationState, ProxyMode};

    impl From<ProxyMode> for WireMode {
        fn from(value: ProxyMode) -> Self {
            match value {
                ProxyMode::Rule => Self::Rule,
                ProxyMode::Global => Self::Global,
                ProxyMode::Direct => Self::Direct,
            }
        }
    }

    impl From<WireMode> for ProxyMode {
        fn from(value: WireMode) -> Self {
            match value {
                WireMode::Rule => Self::Rule,
                WireMode::Global => Self::Global,
                WireMode::Direct => Self::Direct,
            }
        }
    }

    impl From<CoreKind> for WireCoreKind {
        fn from(value: CoreKind) -> Self {
            match value {
                CoreKind::Mihomo => Self::Mihomo,
                CoreKind::SingBox => Self::SingBox,
                CoreKind::Xray => Self::Xray,
            }
        }
    }

    impl From<WireCoreKind> for CoreKind {
        fn from(value: WireCoreKind) -> Self {
            match value {
                WireCoreKind::Mihomo => Self::Mihomo,
                WireCoreKind::SingBox => Self::SingBox,
                WireCoreKind::Xray => Self::Xray,
            }
        }
    }

    impl From<CoreRunState> for WireRunState {
        fn from(value: CoreRunState) -> Self {
            match value {
                CoreRunState::Stopped => Self::Stopped,
                CoreRunState::Starting => Self::Starting,
                CoreRunState::Running => Self::Running,
                CoreRunState::Stopping => Self::Stopping,
                CoreRunState::Crashed => Self::Crashed,
                CoreRunState::Failed => Self::Failed,
            }
        }
    }

    impl From<WireRunState> for CoreRunState {
        fn from(value: WireRunState) -> Self {
            match value {
                WireRunState::Stopped => Self::Stopped,
                WireRunState::Starting => Self::Starting,
                WireRunState::Running => Self::Running,
                WireRunState::Stopping => Self::Stopping,
                WireRunState::Crashed => Self::Crashed,
                WireRunState::Failed => Self::Failed,
            }
        }
    }

    impl From<OperationState> for WireOperationState {
        fn from(value: OperationState) -> Self {
            match value {
                OperationState::Pending => Self::Pending,
                OperationState::Running => Self::Running,
                OperationState::Completed => Self::Completed,
                OperationState::Failed => Self::Failed,
                OperationState::Cancelled => Self::Cancelled,
            }
        }
    }

    impl From<WireOperationState> for OperationState {
        fn from(value: WireOperationState) -> Self {
            match value {
                WireOperationState::Pending => Self::Pending,
                WireOperationState::Running => Self::Running,
                WireOperationState::Completed => Self::Completed,
                WireOperationState::Failed => Self::Failed,
                WireOperationState::Cancelled => Self::Cancelled,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::{CoreKind, OperationState, ProxyMode};

    #[test]
    fn wire_values_round_trip() {
        for value in 1..=3 {
            let mode = WireMode::from_wire(value).unwrap();
            assert_eq!(mode.wire(), value);
        }
        for value in 1..=3 {
            let core = WireCoreKind::from_wire(value).unwrap();
            assert_eq!(core.wire(), value);
        }
        for value in 1..=6 {
            let state = WireRunState::from_wire(value).unwrap();
            assert_eq!(state.wire(), value);
        }
        for value in 1..=5 {
            let state = WireOperationState::from_wire(value).unwrap();
            assert_eq!(state.wire(), value);
        }
    }

    #[test]
    fn unknown_wire_values_are_rejected() {
        assert!(WireMode::from_wire(0).is_none());
        assert!(WireMode::from_wire(4).is_none());
        assert!(WireCoreKind::from_wire(999).is_none());
        assert!(WireRunState::from_wire(7).is_none());
        assert!(WireOperationState::from_wire(6).is_none());
    }

    #[test]
    fn terminal_states_match_operation_semantics() {
        assert!(!WireOperationState::Pending.is_terminal());
        assert!(!WireOperationState::Running.is_terminal());
        assert!(WireOperationState::Completed.is_terminal());
        assert!(WireOperationState::Failed.is_terminal());
        assert!(WireOperationState::Cancelled.is_terminal());
    }

    #[test]
    fn labels_parse_back_to_modes() {
        assert_eq!(WireMode::from_label("rule"), Some(WireMode::Rule));
        assert_eq!(WireMode::from_label("global"), Some(WireMode::Global));
        assert_eq!(WireMode::from_label("direct"), Some(WireMode::Direct));
        assert_eq!(WireMode::from_label("nope"), None);
        for mode in [WireMode::Rule, WireMode::Global, WireMode::Direct] {
            assert_eq!(WireMode::from_label(mode.label()), Some(mode));
        }
    }

    #[test]
    fn domain_bridges_round_trip() {
        for mode in [ProxyMode::Rule, ProxyMode::Global, ProxyMode::Direct] {
            let wire: WireMode = mode.into();
            let back: ProxyMode = wire.into();
            assert_eq!(mode, back);
        }
        for kind in [CoreKind::Mihomo, CoreKind::SingBox, CoreKind::Xray] {
            let wire: WireCoreKind = kind.into();
            let back: CoreKind = wire.into();
            assert_eq!(kind, back);
        }
        for state in [
            OperationState::Pending,
            OperationState::Running,
            OperationState::Completed,
            OperationState::Failed,
            OperationState::Cancelled,
        ] {
            let wire: WireOperationState = state.into();
            let back: OperationState = wire.into();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn raw_operation_state_delegates_to_typed_mapping() {
        use super::super::RawOperationState;
        assert_eq!(
            RawOperationState(3).known(),
            Some(WireOperationState::Completed)
        );
        assert!(RawOperationState(3).is_terminal());
        assert!(!RawOperationState(2).is_terminal());
        assert_eq!(RawOperationState(9).known(), None);
        assert_eq!(RawOperationState(4).label(), "failed");
    }
}
