//! Subscription-intake × coreconf-renderer fusion (the daemon path).
//!
//! The pure renderers live in `caly-coreconf` (domain values in, config bytes
//! out); intake of a fetched subscription body (decode → dialable nodes)
//! lives in `caly-profile`. This module is the single meeting point: it
//! drives the coreconf renderers with intake output so the rest of the
//! backend never re-implements either half.

use std::collections::BTreeMap;

use caly_coreconf::{
    mihomo::{
        proxy_sections::{
            MihomoGroupName, MihomoProxyEntry, MihomoProxyError, MihomoProxySet, MihomoProxyTag,
        },
        render::proxy_to_entry,
    },
    sing_box::{
        SingBoxOutboundError, SingBoxRenderTuning, node_to_json_string, nodes_to_outbounds,
        sing_box_document,
    },
};
use caly_domain::{BoundedVec, DialableNode, NodeId, SubscriptionId};
use caly_subscription::{
    SubscriptionDocument, decode_document, dedupe, dedupe_name_tags, parse_any_proxy_uri,
    parse_clash_yaml, parse_sip008,
};

/// Generates a bounded sing-box JSON document from URI subscription lines.
pub fn uri_body_to_sing_box_json(
    body: Vec<u8>,
    subscription: SubscriptionId,
) -> Result<Vec<u8>, SingBoxOutboundError> {
    uri_body_to_sing_box_json_with(body, subscription, &SingBoxRenderTuning::standard())
}

/// Generates the document with config-driven header tuning (controller, secret,
/// log level, mixed inbound).
pub fn uri_body_to_sing_box_json_with(
    body: Vec<u8>,
    subscription: SubscriptionId,
    tuning: &SingBoxRenderTuning,
) -> Result<Vec<u8>, SingBoxOutboundError> {
    let document = decode_document(body).map_err(|_| SingBoxOutboundError::InvalidFormat)?;
    let nodes = dialable_nodes_for_sing_box(&document, subscription)?;
    let outbounds = nodes_to_outbounds(nodes)?;
    sing_box_document(tuning, outbounds)
}

/// One node the strict sing-box renderer skipped, with the reason the
/// operator can act on (protocol unsupported by this kernel renderer).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingBoxSkip {
    /// The node's display tag (what lists and groups show).
    pub tag: String,
    /// Stable protocol label (`wireguard`, `anytls`, `http`, …).
    pub protocol: &'static str,
}

/// Renders per-node sing-box outbound JSON objects keyed by canonical
/// `NodeId`, skipping nodes the strict renderer cannot represent, and
/// returning the skip list alongside so the caller can warn with the
/// reason (a silently shrinking node pool is a routing change the
/// operator cannot see). The registry stores the map so a config backend
/// can rebuild the outbounds array without re-parsing the subscription
/// body (mirrors the Mihomo yaml field).
pub fn uri_body_to_sing_box_outbound_map(
    body: &[u8],
    subscription: SubscriptionId,
) -> Result<(BTreeMap<NodeId, String>, Vec<SingBoxSkip>), SingBoxOutboundError> {
    let document =
        decode_document(body.to_vec()).map_err(|_| SingBoxOutboundError::InvalidFormat)?;
    sing_box_outbound_map_from_document(&document, subscription)
}

/// [`uri_body_to_sing_box_outbound_map`] on an already-decoded document —
/// the single-pass pipeline entry (`cached.rs` decodes once per refresh and
/// shares the document across the projection / mihomo / sing-box / routing
/// consumers instead of decoding the body up to four times).
pub fn sing_box_outbound_map_from_document(
    document: &SubscriptionDocument,
    subscription: SubscriptionId,
) -> Result<(BTreeMap<NodeId, String>, Vec<SingBoxSkip>), SingBoxOutboundError> {
    let mut map = BTreeMap::new();
    let mut skipped = Vec::new();
    for node in dialable_nodes_for_sing_box(document, subscription)? {
        if map.contains_key(&node.id()) {
            continue;
        }
        if let Ok(outbound) = node_to_json_string(&node) {
            map.insert(node.id(), outbound);
        } else {
            let tag = node.display(true, None).map_or_else(
                |_| format!("node-{}", caly_domain::to_hex(node.id().into_bytes())),
                |display| display.name().as_str().to_owned(),
            );
            // A finer label than the bare protocol: a shadowsocks node
            // with a v2ray-plugin/obfs parameter is *not* a plain ss
            // node — the warning must say which variant lost.
            let protocol = match node.protocol() {
                caly_domain::Protocol::Shadowsocks {
                    plugin: Some(_), ..
                } => "shadowsocks+plugin",
                // Legacy CFB ciphers: Mihomo dials them, sing-box does
                // not — name the exact cipher so the operator can see
                // why the node pool differs between cores.
                caly_domain::Protocol::Shadowsocks {
                    method: caly_domain::ShadowsocksCipher::Aes128Cfb,
                    plugin: None,
                    ..
                } => "shadowsocks+aes-128-cfb",
                caly_domain::Protocol::Shadowsocks {
                    method: caly_domain::ShadowsocksCipher::Aes256Cfb,
                    plugin: None,
                    ..
                } => "shadowsocks+aes-256-cfb",
                other => other.label(),
            };
            skipped.push(SingBoxSkip { tag, protocol });
        }
    }
    Ok((map, skipped))
}

/// Extracts dialable nodes from a decoded document, tolerating unparseable URI
/// lines and parsing Clash-YAML bodies atomically. An unusable body is empty.
fn dialable_nodes_for_sing_box(
    document: &SubscriptionDocument,
    subscription: SubscriptionId,
) -> Result<Vec<DialableNode>, SingBoxOutboundError> {
    match document {
        SubscriptionDocument::UriLines { lines, .. } => Ok(lines
            .iter()
            .filter_map(|line| parse_any_proxy_uri(line.as_str(), subscription).ok())
            .collect()),
        SubscriptionDocument::ClashYaml(body) => {
            let source = core::str::from_utf8(body.as_slice())
                .map_err(|_| SingBoxOutboundError::InvalidFormat)?;
            parse_clash_yaml(source, subscription)
                .map_err(|_| SingBoxOutboundError::UnsupportedDocument)
        }
        SubscriptionDocument::Sip008(body) => {
            let source = core::str::from_utf8(body.as_slice())
                .map_err(|_| SingBoxOutboundError::InvalidFormat)?;
            Ok(parse_sip008(source, subscription).0)
        }
        // URL lists are expanded by the daemon fetch path before rendering.
        SubscriptionDocument::UrlList(_) => Err(SingBoxOutboundError::UnsupportedDocument),
    }
}

/// Renders a Mihomo proxy section (proxies/groups/rules) from a subscription
/// body, reusing the same dialable pipeline as the sing-box renderer.
pub fn uri_body_to_mihomo_proxy_set(
    body: Vec<u8>,
    subscription: SubscriptionId,
) -> Result<MihomoProxySet, MihomoProxyError> {
    let document = decode_document(body).map_err(|_| MihomoProxyError::InvalidFormat)?;
    mihomo_proxy_set_from_document(&document, subscription)
}

/// [`uri_body_to_mihomo_proxy_set`] on an already-decoded document (single-
/// pass pipeline; see [`sing_box_outbound_map_from_document`]).
pub fn mihomo_proxy_set_from_document(
    document: &SubscriptionDocument,
    subscription: SubscriptionId,
) -> Result<MihomoProxySet, MihomoProxyError> {
    // Dedupe by NodeId with the same first-seen order the projection uses, so
    // the `#N` suffix numbering assigned below matches what list UIs show.
    let deduped = dedupe(dialable_nodes_for_mihomo(document, subscription)?)
        .map_err(|_| MihomoProxyError::InvalidFormat)?;
    let names = deduped
        .nodes
        .iter()
        .filter_map(|node| node.display(true, None).ok())
        .map(|display| display.name().as_str().to_owned())
        .collect::<Vec<_>>();
    let tags = dedupe_name_tags(names.iter().map(String::as_str));
    let mut entries: Vec<MihomoProxyEntry> = Vec::new();
    for (node, tag_text) in deduped.nodes.iter().zip(tags) {
        let Ok(tag) = MihomoProxyTag::new(tag_text) else {
            continue;
        };
        match proxy_to_entry(node, tag) {
            Ok(entry) => entries.push(entry),
            Err(MihomoProxyError::NoUsableNodes) => {}
            Err(error) => return Err(error),
        }
    }
    if entries.is_empty() {
        return Err(MihomoProxyError::NoUsableNodes);
    }
    let entries =
        BoundedVec::try_from_vec(entries).map_err(|_| MihomoProxyError::TooManyProxies)?;
    let group = MihomoGroupName::new(group_name())?;
    Ok(MihomoProxySet::from_parts(entries, group))
}

/// Extracts dialable nodes from a decoded document, tolerating unparseable URI
/// lines and parsing Clash-YAML bodies atomically. An unusable body is empty.
fn dialable_nodes_for_mihomo(
    document: &SubscriptionDocument,
    subscription: SubscriptionId,
) -> Result<Vec<DialableNode>, MihomoProxyError> {
    match document {
        SubscriptionDocument::UriLines { lines, .. } => Ok(lines
            .iter()
            .filter_map(|line| parse_any_proxy_uri(line.as_str(), subscription).ok())
            .collect()),
        SubscriptionDocument::ClashYaml(body) => {
            let source = core::str::from_utf8(body.as_slice())
                .map_err(|_| MihomoProxyError::InvalidFormat)?;
            parse_clash_yaml(source, subscription)
                .map_err(|_| MihomoProxyError::UnsupportedDocument)
        }
        SubscriptionDocument::Sip008(body) => {
            let source = core::str::from_utf8(body.as_slice())
                .map_err(|_| MihomoProxyError::InvalidFormat)?;
            Ok(parse_sip008(source, subscription).0)
        }
        // URL lists are expanded by the daemon fetch path before rendering.
        SubscriptionDocument::UrlList(_) => Err(MihomoProxyError::UnsupportedDocument),
    }
}

/// Selects the configured Clash proxy-group name with a safe default.
fn group_name() -> String {
    match std::env::var("CALY_MIHOMO_PROXY_GROUP").as_deref() {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => "AUTO".to_owned(),
    }
}

#[cfg(test)]
mod tests;
