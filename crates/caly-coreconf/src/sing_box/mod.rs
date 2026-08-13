//! sing-box configuration rendering (pure).
//!
//! Everything here turns domain values into sing-box JSON bytes; spawning,
//! checking and controlling the sing-box process lives in `caly-corectl`.
//! The subscription-intake half (decoding a fetched body into dialable nodes)
//! lives in `caly-profile`; fusing the two is `caly-backends`' job.
//!
//! Typed-model note (P3b): the base renderer assembles the same
//! `document::SingBoxDocument` typed tree as the subscription document
//! renderer, so both rendering paths share one structural definition.

mod dns_render;
mod document;
mod groups;
mod node;
mod registry;

pub use document::{SingBoxRenderTuning, nodes_to_outbounds, sing_box_document};
pub use groups::proxy_group_to_sing_box_outbound;
pub use node::{node_to_json, node_to_json_string};
pub use registry::SingBoxNodeRegistry;

use caly_dns::DnsSettings;
use caly_domain::TunConfig;
use serde_json::Value;

use crate::rules::{RouteRule, RuleSetSource};
use crate::{RenderFailure, config_failure};

/// Inbound sniffing options (domain recovery for bare-IP traffic).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SniffOptions {
    /// Render `sniff: true` on the mixed/transparent inbounds.
    pub enabled: bool,
    /// Render `sniff_override_destination: true` alongside it.
    pub override_destination: bool,
}

/// Config-driven tuning for the subscription-less sing-box renderer. One
/// struct replaces a telescoping parameter list so new tuning (sniffing,
/// route rules, ...) no longer grows the signature.
#[derive(Clone, Debug)]
pub struct SingBoxBaseTuning {
    /// `experimental.clash_api.external_controller` (host:port).
    pub controller: String,
    /// Optional bounded DNS block rendered into the document.
    pub dns: Option<DnsSettings>,
    /// Clash API secret; `None` omits it, an empty string is rejected.
    pub secret: Option<String>,
    /// Kernel log level (trace|debug|info|warn|error).
    pub log_level: String,
    /// Mixed inbound port; 0 renders no inbound.
    pub mixed_port: u16,
    /// Bind the mixed inbound to LAN instead of loopback only.
    pub allow_lan: bool,
    /// LAN bind address when `allow_lan` is true (`"*"` = all interfaces).
    pub bind_address: String,
    /// Optional TUN inbound (stack/auto-route/strict-route).
    pub tun: Option<TunConfig>,
    /// Interface name for the TUN inbound.
    pub tun_interface: String,
    /// Transparent inbound port; 0 disables.
    pub transparent_port: u16,
    /// Use a tproxy inbound instead of redirect.
    pub transparent_tproxy: bool,
    /// Inbound sniffing options.
    pub sniff: SniffOptions,
    /// Shared rule renderer output; empty omits the `rules` key.
    pub route_rules: Vec<RouteRule>,
    /// `route.rule_set` sources (remote geo rule-sets); empty omits the key.
    pub rule_sets: Vec<RuleSetSource>,
    /// Outbound tag for `route.final`.
    pub route_final: String,
    /// Add the built-in `block` outbound (required by REJECT rules).
    pub block_outbound: bool,
}

impl Default for SingBoxBaseTuning {
    fn default() -> Self {
        Self {
            controller: "127.0.0.1:9091".to_owned(),
            dns: None,
            secret: None,
            log_level: "error".to_owned(),
            mixed_port: 0,
            allow_lan: false,
            bind_address: "*".to_owned(),
            tun: None,
            tun_interface: String::new(),
            transparent_port: 0,
            transparent_tproxy: false,
            sniff: SniffOptions::default(),
            route_rules: Vec::new(),
            rule_sets: Vec::new(),
            route_final: "direct".to_owned(),
            block_outbound: false,
        }
    }
}

/// Minimal JSON config renderer for sing-box validation and lifecycle smoke.
#[derive(Default)]
pub struct SingBoxConfigRenderer;

impl SingBoxConfigRenderer {
    /// Renders a config from the full tuning struct: controller/DNS/secret,
    /// log level, the local mixed inbound, an optional transparent
    /// redirect/tproxy inbound, optional inbound sniffing, an optional TUN
    /// inbound, and config-driven `route.rules`/`route.final` (both
    /// pre-rendered by the shared rule renderer so this renderer stays free of
    /// rule semantics). This is the single rendering entry point; convenience
    /// variants were collapsed into it (convergence, 2026-08-06).
    pub fn render_tuned(&self, tuning: &SingBoxBaseTuning) -> Result<Vec<u8>, RenderFailure> {
        // W3a 兜底: a TUN inbound without a DNS block blackholes name
        // resolution — the kernel answers DNS itself, but with no
        // resolver it cannot. Inject the built-in default (fake-ip +
        // public upstreams) when the operator configured no DNS section.
        let fallback_dns: Option<caly_dns::DnsSettings> =
            if tuning.tun.is_some() && tuning.dns.is_none() {
                caly_dns::default_tun_dns().map_err(|error| {
                    config_failure("cannot build the fallback TUN DNS", &error.to_string())
                })?
            } else {
                None
            };
        let (dns, resolver_tag) =
            document::dns_block(tuning.dns.as_ref().or(fallback_dns.as_ref()));
        let mut outbounds: Vec<Value> = Vec::new();
        outbounds.push(builtin_value(document::DIRECT_OUTBOUND)?);
        if tuning.block_outbound {
            outbounds.push(builtin_value(document::BLOCK_OUTBOUND)?);
        }
        let mut inbounds = Vec::new();
        if let Some(mixed) = document::mixed_inbound(
            tuning.mixed_port,
            tuning.allow_lan,
            &tuning.bind_address,
            tuning.sniff,
        ) {
            inbounds.push(mixed);
        }
        if let Some(transparent) = document::transparent_inbound(
            tuning.transparent_port,
            tuning.transparent_tproxy,
            tuning.sniff,
        ) {
            inbounds.push(transparent);
        }
        if let Some(tun) = &tuning.tun {
            inbounds.push(document::tun_inbound(tun, &tuning.tun_interface));
        }
        let document = document::SingBoxDocument {
            dns,
            experimental: document::ExperimentalBlock {
                clash_api: document::ClashApi {
                    external_controller: tuning.controller.clone(),
                    secret: tuning.secret.clone().unwrap_or_default(),
                },
            },
            inbounds,
            log: document::LogBlock {
                level: document::normalize_log_level(&tuning.log_level),
            },
            outbounds,
            route: document::RouteBlock {
                default_domain_resolver: resolver_tag,
                final_outbound: tuning.route_final.clone(),
                rule_set: tuning.rule_sets.clone(),
                rules: tuning
                    .route_rules
                    .iter()
                    .cloned()
                    .map(document::RouteRuleEntry::Rule)
                    .collect(),
            },
        };
        serde_json::to_vec(&document).map_err(|_| {
            crate::config_failure(
                "sing-box JSON config is invalid",
                "regenerate the configuration",
            )
        })
    }

    pub fn validate_bytes(&self, bytes: &[u8]) -> Result<(), RenderFailure> {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .map(|_| ())
            .map_err(|_| {
                crate::config_failure(
                    "sing-box JSON config is invalid",
                    "regenerate the configuration",
                )
            })
    }
}

/// Serializes one built-in outbound; derived serde over static string fields
/// cannot realistically fail, but `to_value` is Result-typed.
fn builtin_value(outbound: document::BuiltinOutbound) -> Result<Value, RenderFailure> {
    serde_json::to_value(outbound).map_err(|_| {
        crate::config_failure(
            "sing-box JSON config is invalid",
            "regenerate the configuration",
        )
    })
}

/// sing-box outbound/subscription rendering failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SingBoxOutboundError {
    InvalidFormat,
    UnsupportedDocument,
    UnsupportedNode,
    Serialization,
}

impl core::fmt::Display for SingBoxOutboundError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidFormat => formatter.write_str("subscription body is not decodable"),
            Self::UnsupportedDocument => {
                formatter.write_str("document format is not renderable for sing-box")
            }
            Self::UnsupportedNode => formatter.write_str("node is not representable"),
            Self::Serialization => formatter.write_str("sing-box JSON rendering failed"),
        }
    }
}

#[cfg(test)]
mod config_tests;
