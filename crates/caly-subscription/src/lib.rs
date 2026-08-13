//! Subscription capability crate: fetch, parse, transform and userinfo of
//! proxy subscription bodies, plus the shared SSRF-safe host-resolution
//! helpers (`net`).
//!
//! Relocated in P6 (docs/crate-replan.md v4.1): the intake half of
//! `caly-profile::subscription` (this whole tree) and `caly-profile::net`
//! move here together — the two already formed a closed pair. Rendering the
//! parsed nodes into core configs stays in `caly-coreconf`; fusing intake
//! with rendering stays in `caly-backends`. `reqwest` is the WAN boundary
//! and is whitelisted to this crate only (decision #75, relabeled in P6).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny

mod chain;
mod clash;
mod classifier;
mod format;
mod http;
mod id;
pub mod net;
mod normalize;
mod pipeline;
mod policy;
mod sip008;
mod special_uri;
mod transform;
mod uri;
mod userinfo;

pub use chain::{ChainError, validate_chains};
pub use clash::{
    ClashImport, ClashParseError, clash_routing_from_body, clash_routing_from_document,
    parse_clash_config, parse_clash_yaml,
};
pub use classifier::PublicAddressClassifier;
pub use format::{
    FormatError, MAX_SUBSCRIPTION_BODY_BYTES, SubscriptionBody, SubscriptionDocument,
    SubscriptionFormat, SubscriptionLine, SubscriptionLines, decode_document,
};
pub use http::{FetchError, FetchResult, FetchValidators, fetch_pinned};
pub use id::subscription_id_for_url;
pub use normalize::{NormalizeError, derive_tags, normalize_display_name};
pub use pipeline::{
    DedupeResult, NodeIds, PipelineError, SubscriptionDiff, SubscriptionNodes,
    SubscriptionProjection, dedupe, dedupe_name_tags, diff, parse_document_to_display_lossy,
    parse_uri_body_to_display, parse_uri_body_to_display_lossy,
};
pub use policy::{
    AddressClassifier, FetchPolicy, MAX_REDIRECT_DEPTH, ResolutionError, ResolvedAddresses,
    SubscriptionScheme, validate_resolved,
};
pub use sip008::parse_sip008;
pub use transform::{
    TransformFingerprint, TransformInputs, TransformRule, TransformRules, fingerprint,
    should_retransform,
};
pub use uri::{UriParseError, parse_any_proxy_uri, parse_proxy_uri};
pub use userinfo::{SubscriptionUserInfo, parse_subscription_userinfo, render_usage_human};
