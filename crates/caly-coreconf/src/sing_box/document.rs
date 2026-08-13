//! sing-box document assembly with config-driven tuning, typed-model form.
//!
//! Document assembly is pure: the caller (a backend fusing subscription
//! intake) supplies dialable nodes or pre-rendered outbounds; this module
//! never decodes a subscription body itself.
//!
//! Typed-model note (P3b): every struct declares fields in alphabetical
//! order, matching the byte order the pre-typed `serde_json::Value`
//! (BTreeMap) assembly produced, so the subscription document stays
//! byte-identical. The shared blocks (`SingBoxDocument`, `Inbound`,
//! `RouteBlock`, ...) are `pub(crate)` so the subscription-less base
//! renderer in `mod.rs` assembles the same typed shapes.

use serde::Serialize;
use serde_json::Value;

use caly_dns::DnsSettings;
use caly_domain::{DialableNode, TunConfig};

use crate::rules::{RouteRule, RuleSetSource};

use super::{dns_render, node_to_json, SingBoxOutboundError, SniffOptions};

/// Runtime values rendered into the sing-box subscription document header.
/// The bools are independent render switches (inbound binding, sniffing,
/// REJECT support), kept flat to mirror the document fields they emit.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct SingBoxRenderTuning {
    /// `external_controller` address (host:port).
    pub controller: String,
    /// Controller auth secret; empty omits the field.
    pub secret: String,
    /// Kernel log level (sing-box understands trace|debug|info|warn|error).
    pub log_level: String,
    /// Mixed inbound port; 0 renders no inbound.
    pub mixed_port: u16,
    /// Bind the inbound to LAN instead of loopback only.
    pub allow_lan: bool,
    /// LAN bind address when `allow_lan` is true (`"*"` = all interfaces).
    pub bind_address: String,
    /// Optional TUN inbound (stack/auto-route/strict-route from config).
    pub tun: Option<TunConfig>,
    /// Interface name for the TUN inbound.
    pub tun_interface: String,
    /// Bounded DNS settings; `None` omits the block. The resolver wiring for
    /// domain-hosted servers (`route.default_domain_resolver`) is derived
    /// here at render time.
    pub dns: Option<DnsSettings>,
    /// Render `sniff: true` on the mixed inbound (domain recovery).
    pub sniff: bool,
    /// Render `sniff_override_destination: true` alongside sniffing.
    pub sniff_override_destination: bool,
    /// Shared rule renderer output appended after the `clash_mode` rules.
    pub route_rules: Vec<RouteRule>,
    /// `route.rule_set` sources (remote geo rule-sets); empty omits the key.
    pub rule_sets: Vec<RuleSetSource>,
    /// Outbound tag for `route.final` (MATCH semantics; defaults to `PROXY`).
    pub route_final: String,
    /// Add the built-in `block` outbound (required by REJECT rules).
    pub block_outbound: bool,
}

impl SingBoxRenderTuning {
    /// Document-header defaults matching historical behavior.
    pub fn standard() -> Self {
        Self {
            controller: "127.0.0.1:9091".to_owned(),
            secret: String::new(),
            log_level: "error".to_owned(),
            mixed_port: 7890,
            allow_lan: false,
            bind_address: "*".to_owned(),
            tun: None,
            tun_interface: String::new(),
            dns: None,
            sniff: false,
            sniff_override_destination: false,
            route_rules: Vec::new(),
            rule_sets: Vec::new(),
            route_final: "PROXY".to_owned(),
            block_outbound: false,
        }
    }
}

/// Accepts the generic kernel levels; anything unknown falls back to error.
pub(crate) fn normalize_log_level(level: &str) -> &'static str {
    match level {
        "trace" => "trace",
        "debug" => "debug",
        "info" => "info",
        "warn" => "warn",
        _ => "error",
    }
}

/// serde skip helper: optional-in-output boolean flags. The reference
/// signature is mandated by `skip_serializing_if`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The whole sing-box configuration document. Top-level field order is
/// alphabetical, the byte order the pre-typed `Value` assembly emitted.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SingBoxDocument {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dns: Option<dns_render::DnsBlock>,
    pub(crate) experimental: ExperimentalBlock,
    pub(crate) inbounds: Vec<Inbound>,
    pub(crate) log: LogBlock,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) outbounds: Vec<Value>,
    pub(crate) route: RouteBlock,
}

/// `log` block.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LogBlock {
    pub(crate) level: &'static str,
}

/// `experimental` block (only the Clash API is surfaced).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExperimentalBlock {
    pub(crate) clash_api: ClashApi,
}

/// `experimental.clash_api` block; an empty `secret` is omitted, matching
/// the historical acceptance of a secret-less controller.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ClashApi {
    pub(crate) external_controller: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) secret: String,
}

/// One inbound (`mixed`, `redirect`/`tproxy` or `tun`; `type` discriminates).
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Inbound {
    Plain(PlainInbound),
    Tun(TunInbound),
}

/// `mixed` or transparent `redirect`/`tproxy` inbound — identical shape,
/// the `type` field discriminates.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PlainInbound {
    pub(crate) listen: String,
    pub(crate) listen_port: u16,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) sniff: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) sniff_override_destination: bool,
    pub(crate) tag: &'static str,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
}

/// `tun` inbound; the interface always owns the fixed v4/v6 addresses so the
/// kernel accepts routes onto it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TunInbound {
    pub(crate) address: [&'static str; 2],
    pub(crate) auto_route: bool,
    pub(crate) interface_name: String,
    pub(crate) mtu: u16,
    pub(crate) stack: &'static str,
    pub(crate) strict_route: bool,
    pub(crate) tag: &'static str,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
}

/// `route` block. `rules`/`rule_set` are omitted when empty (the
/// subscription-less base config can legitimately carry only `final`).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RouteBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default_domain_resolver: Option<String>,
    #[serde(rename = "final")]
    pub(crate) final_outbound: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) rule_set: Vec<RuleSetSource>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) rules: Vec<RouteRuleEntry>,
}

/// One `route.rules` entry: a `clash_mode` selector rule or a shared rule
/// renderer object.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum RouteRuleEntry {
    ClashMode(ClashModeRule),
    Rule(RouteRule),
}

/// `clash_mode` runtime-mode rule (Global/Direct switches via the Clash API).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ClashModeRule {
    pub(crate) clash_mode: &'static str,
    pub(crate) outbound: &'static str,
}

/// Built-in `direct`/`block` outbound (alphabetical field order).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct BuiltinOutbound {
    pub(crate) tag: &'static str,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
}

/// The built-in `direct` outbound.
pub(crate) const DIRECT_OUTBOUND: BuiltinOutbound = BuiltinOutbound {
    tag: "direct",
    kind: "direct",
};

/// The built-in `block` outbound (REJECT target).
pub(crate) const BLOCK_OUTBOUND: BuiltinOutbound = BuiltinOutbound {
    tag: "block",
    kind: "block",
};

/// The fixed TUN interface addresses (v4 /30 plus the v6 /126 ULA).
pub(crate) const TUN_ADDRESSES: [&str; 2] = ["172.18.0.1/30", "fdfe:dcba:9876::1/126"];

/// Chooses the mixed-inbound bind address: loopback unless LAN-binding is
/// enabled (`"*"`/empty binds every interface).
pub(crate) fn mixed_listen(allow_lan: bool, bind_address: &str) -> &str {
    if !allow_lan {
        "127.0.0.1"
    } else if bind_address == "*" || bind_address.is_empty() {
        "0.0.0.0"
    } else {
        bind_address
    }
}

/// The mixed inbound, or `None` when the port disables it.
/// The mixed inbound, or `None` when the port disables it.
pub(crate) fn mixed_inbound(
    port: u16,
    allow_lan: bool,
    bind_address: &str,
    sniff: SniffOptions,
) -> Option<Inbound> {
    plain_inbound(
        mixed_listen(allow_lan, bind_address),
        port,
        "caly-mixed-in",
        "mixed",
        sniff,
    )
}

/// The transparent redirect/tproxy inbound, or `None` when the port
/// disables it.
pub(crate) fn transparent_inbound(port: u16, tproxy: bool, sniff: SniffOptions) -> Option<Inbound> {
    plain_inbound(
        "0.0.0.0",
        port,
        "caly-transparent-in",
        if tproxy { "tproxy" } else { "redirect" },
        sniff,
    )
}

/// Builds a plain (mixed/transparent) inbound, or `None` when the port
/// disables it.
fn plain_inbound(
    listen: &str,
    port: u16,
    tag: &'static str,
    kind: &'static str,
    sniff: SniffOptions,
) -> Option<Inbound> {
    if port == 0 {
        return None;
    }
    Some(Inbound::Plain(PlainInbound {
        listen: listen.to_owned(),
        listen_port: port,
        sniff: sniff.enabled,
        sniff_override_destination: sniff.enabled && sniff.override_destination,
        tag,
        kind,
    }))
}

/// The TUN inbound (stack/auto-route/strict-route plus the required address
/// so the interface owns an IP).
pub(crate) fn tun_inbound(tun: &TunConfig, interface: &str) -> Inbound {
    Inbound::Tun(TunInbound {
        address: TUN_ADDRESSES,
        auto_route: tun.auto_route(),
        interface_name: interface.to_owned(),
        mtu: tun.mtu(),
        stack: tun.stack().label(),
        strict_route: tun.strict_route(),
        tag: "tun-in",
        kind: "tun",
    })
}

/// Renders the bounded DNS settings into the typed block plus the resolver
/// tag the caller wires into `route.default_domain_resolver`.
pub(crate) fn dns_block(
    dns: Option<&DnsSettings>,
) -> (Option<dns_render::DnsBlock>, Option<String>) {
    match dns.map(dns_render::render_dns_object) {
        Some(rendered) => (Some(rendered.block), rendered.domain_resolver),
        None => (None, None),
    }
}

/// Assembles the full sing-box document around a caller-supplied outbound
/// list: adds the direct/block outbounds, the PROXY/GLOBAL selectors, and the
/// config-driven route/inbounds/clash_api/DNS blocks. Shared by the
/// subscription-document renderer and the config backend (which rebuilds the
/// outbound list from the shared registry instead of re-parsing a body).
pub fn sing_box_document(
    tuning: &SingBoxRenderTuning,
    subscription_outbounds: Vec<Value>,
) -> Result<Vec<u8>, SingBoxOutboundError> {
    let mut outbounds = subscription_outbounds;
    let tags: Vec<String> = outbounds
        .iter()
        .filter_map(|value| value.get("tag").and_then(Value::as_str).map(str::to_owned))
        .collect();
    outbounds.push(
        serde_json::to_value(DIRECT_OUTBOUND).map_err(|_| SingBoxOutboundError::Serialization)?,
    );
    if tuning.block_outbound {
        outbounds.push(
            serde_json::to_value(BLOCK_OUTBOUND)
                .map_err(|_| SingBoxOutboundError::Serialization)?,
        );
    }
    append_selectors(&mut outbounds, &tags)?;
    let (dns, resolver_tag) = dns_block(tuning.dns.as_ref());
    let mut rules: Vec<RouteRuleEntry> = vec![
        RouteRuleEntry::ClashMode(ClashModeRule {
            clash_mode: "Global",
            outbound: "GLOBAL",
        }),
        RouteRuleEntry::ClashMode(ClashModeRule {
            clash_mode: "Direct",
            outbound: "direct",
        }),
    ];
    rules.extend(tuning.route_rules.iter().cloned().map(RouteRuleEntry::Rule));
    let sniff = SniffOptions {
        enabled: tuning.sniff,
        override_destination: tuning.sniff_override_destination,
    };
    let mut inbounds = Vec::new();
    if let Some(mixed) = mixed_inbound(
        tuning.mixed_port,
        tuning.allow_lan,
        &tuning.bind_address,
        sniff,
    ) {
        inbounds.push(mixed);
    }
    if let Some(tun) = &tuning.tun {
        inbounds.push(tun_inbound(tun, &tuning.tun_interface));
    }
    let document = SingBoxDocument {
        dns,
        experimental: ExperimentalBlock {
            clash_api: ClashApi {
                external_controller: tuning.controller.clone(),
                secret: tuning.secret.clone(),
            },
        },
        inbounds,
        log: LogBlock {
            level: normalize_log_level(&tuning.log_level),
        },
        outbounds,
        route: RouteBlock {
            default_domain_resolver: resolver_tag,
            final_outbound: tuning.route_final.clone(),
            rule_set: tuning.rule_sets.clone(),
            rules,
        },
    };
    serde_json::to_vec(&document).map_err(|_| SingBoxOutboundError::Serialization)
}

/// Renders deduplicated outbounds from parsed dialable nodes, skipping nodes
/// the strict sing-box renderer cannot represent.
pub fn nodes_to_outbounds(nodes: Vec<DialableNode>) -> Result<Vec<Value>, SingBoxOutboundError> {
    let mut outbounds = Vec::new();
    // Deduplicate by canonical node identity: a subscription can carry two
    // lines that normalize to the same NodeId, and sing-box rejects duplicate
    // outbound tags (`proxy-<id>`). This mirrors the Mihomo registry path.
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        if !seen.insert(node.id()) {
            continue;
        }
        // A real subscription mixes protocols; skip nodes the strict sing-box
        // renderer cannot represent rather than failing the whole render.
        match node_to_json(&node) {
            Ok(outbound) => outbounds.push(outbound),
            Err(SingBoxOutboundError::UnsupportedNode) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(outbounds)
}

/// Appends the standard PROXY/GLOBAL selector outbounds over the node tags.
fn append_selectors(
    outbounds: &mut Vec<Value>,
    tags: &[String],
) -> Result<(), SingBoxOutboundError> {
    let members = tags
        .iter()
        .cloned()
        .chain(std::iter::once("direct".to_owned()))
        .collect::<Vec<_>>();
    for tag in ["PROXY", "GLOBAL"] {
        let selector = super::groups::SelectorOutbound {
            outbounds: members.clone(),
            tag: tag.to_owned(),
            kind: "selector",
        };
        outbounds
            .push(serde_json::to_value(selector).map_err(|_| SingBoxOutboundError::Serialization)?);
    }
    Ok(())
}

#[cfg(test)]
mod document_tests {
    use super::*;

    #[test]
    fn standard_tuning_keeps_historical_defaults() {
        let tuning = SingBoxRenderTuning::standard();
        assert_eq!(tuning.controller, "127.0.0.1:9091");
        assert_eq!(tuning.mixed_port, 7890);
        assert!(tuning.dns.is_none());
    }
}
