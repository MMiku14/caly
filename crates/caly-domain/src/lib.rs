//! Pure domain model for caly.
//!
//! This crate contains no async runtime, filesystem, network, process,
//! platform, path, RPC, wall-clock acquisition or random-number generation.
//! Outer owners supply timestamps and generated/hash-derived identities.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
mod capability;
mod collection;
mod event;
mod identity;
mod node;
mod operation;
mod profile;
mod proxy_group;
mod rule;
mod secret;
mod settings;
mod state;
mod text;
mod tun;

pub use capability::{
    Capability, CapabilitySet, CapabilityStatus, ConfiguredSupport, DuplicateCapability,
    RuntimeAvailability,
};
pub use collection::{BoundedExtendError, BoundedPushError, BoundedVec, CapacityError};
pub use event::{
    classify_cursor, CursorDisposition, EventCursor, EventSequence, SequenceExhausted,
};
pub use identity::{
    hex_nibble, to_hex, DaemonInstanceId, IdentityParseError, NodeId, OperationId, SubscriptionId,
};
pub use node::{
    sanitized_display_name, CongestionControl, Credential, DialableNode, DisplayNode, Endpoint,
    EndpointHost, HostError, NodeBuilder, NodeDisplayName, NodeProtocolLabel, NodeSource, NodeTag,
    NodeTags, NodeValidationError, Protocol, ProtocolText, RealityConfig, ShadowsocksCipher,
    ShadowsocksPlugin, TlsConfig, Transport, TransportText, TransportTextList, VmessCipher,
    WebSocketEarlyData, NODE_DISPLAY_NAME_MAX_BYTES,
};
pub use operation::{
    OperationFailure, OperationFailureCode, OperationState, OperationStatus, OperationStatusError,
    UnixMillis,
};
pub use profile::{
    is_path_safe_component, validate_id, Profile, ProfileBody, ProfileDescription, ProfileError,
    ProfileId, ProfileName, ProfileSource, MAX_PROFILES, PROFILE_BODY_MAX_BYTES,
    PROFILE_DESCRIPTION_MAX_BYTES, PROFILE_ID_MAX_BYTES, PROFILE_NAME_MAX_BYTES,
};
pub use proxy_group::{
    ProxyGroup, ProxyGroupError, ProxyGroupMember, ProxyGroupName, ProxyGroupNodeTag,
    ProxyGroupType, ProxyGroupUrl, UrlTestConfig, MAX_PROXY_GROUPS, MAX_PROXY_GROUP_MEMBERS,
    PROXY_GROUP_NAME_MAX_BYTES, PROXY_GROUP_URL_MAX_BYTES,
};
pub use rule::{
    match_host, GeoipCode, GeositeName, RoutingRule, RuleError, RuleFlags, RuleMatch, RulePolicy,
    RuleProvider, RuleProviderBehavior, RuleProviderFormat, RuleProviderName, RuleProviderSource,
    RuleText, GEOSITE_NAME_MAX_BYTES, INLINE_RULE_PAYLOAD_MAX_BYTES, MAX_RULES, MAX_RULE_PROVIDERS,
    PROCESS_NAME_MAX_BYTES, RULE_PROVIDER_NAME_MAX_BYTES, RULE_TEXT_MAX_BYTES,
};
pub use secret::{SecretError, SecretText};
pub use settings::Controllers;
pub use state::{
    AppliedState, AppliedStateError, CoreKind, CoreRunState, DesiredState, ObservedState,
    PlatformEffectView, PresentationDelta, PresentationSnapshot, ProxyGroupView, ProxyMode,
    SnapshotNodes, SnapshotRevision,
};
pub use text::{BoundedText, TextError};
pub use tun::{TunConfig, TunError, TunStack, MAX_TUN_MTU, MIN_TUN_MTU};
