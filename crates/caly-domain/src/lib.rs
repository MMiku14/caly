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
    CursorDisposition, EventCursor, EventSequence, SequenceExhausted, classify_cursor,
};
pub use identity::{
    DaemonInstanceId, IdentityParseError, NodeId, OperationId, SubscriptionId, to_hex,
};
pub use node::{
    CongestionControl, Credential, DialableNode, DisplayNode, Endpoint, EndpointHost, HostError,
    NODE_DISPLAY_NAME_MAX_BYTES, NodeBuilder, NodeDisplayName, NodeProtocolLabel, NodeSource,
    NodeTag, NodeTags, NodeValidationError, Protocol, ProtocolText, RealityConfig,
    ShadowsocksCipher, ShadowsocksPlugin, TlsConfig, Transport, TransportText, TransportTextList,
    VmessCipher, WebSocketEarlyData, sanitized_display_name,
};
pub use operation::{
    OperationFailure, OperationFailureCode, OperationState, OperationStatus, OperationStatusError,
    UnixMillis,
};
pub use profile::{
    MAX_PROFILES, PROFILE_BODY_MAX_BYTES, PROFILE_DESCRIPTION_MAX_BYTES, PROFILE_ID_MAX_BYTES,
    PROFILE_NAME_MAX_BYTES, Profile, ProfileBody, ProfileDescription, ProfileError, ProfileId,
    ProfileName, ProfileSource, is_path_safe_component, validate_id,
};
pub use proxy_group::{
    MAX_PROXY_GROUP_MEMBERS, MAX_PROXY_GROUPS, PROXY_GROUP_NAME_MAX_BYTES,
    PROXY_GROUP_URL_MAX_BYTES, ProxyGroup, ProxyGroupError, ProxyGroupMember, ProxyGroupName,
    ProxyGroupNodeTag, ProxyGroupType, ProxyGroupUrl, UrlTestConfig,
};
pub use rule::{
    GEOSITE_NAME_MAX_BYTES, GeoipCode, GeositeName, INLINE_RULE_PAYLOAD_MAX_BYTES,
    MAX_RULE_PROVIDERS, MAX_RULES, PROCESS_NAME_MAX_BYTES, RULE_PROVIDER_NAME_MAX_BYTES,
    RULE_TEXT_MAX_BYTES, RoutingRule, RuleError, RuleFlags, RuleMatch, RulePolicy, RuleProvider,
    RuleProviderBehavior, RuleProviderFormat, RuleProviderName, RuleProviderSource, RuleText,
    match_host,
};
pub use secret::{SecretError, SecretText};
pub use settings::Controllers;
pub use state::{
    AppliedState, AppliedStateError, CoreKind, CoreRunState, DesiredState, ObservedState,
    PlatformEffectView, PresentationDelta, PresentationSnapshot, ProxyGroupView, ProxyMode,
    SnapshotNodes, SnapshotRevision,
};
pub use text::{BoundedText, TextError};
pub use tun::{MAX_TUN_MTU, MIN_TUN_MTU, TunConfig, TunError, TunStack};
