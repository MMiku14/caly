//! Bounded NodeId dedupe, subscription normalization, and presentation projection.

use std::collections::{BTreeMap, BTreeSet};

use caly_domain::{
    BoundedVec, DialableNode, DisplayNode, NodeDisplayName, SnapshotNodes, SubscriptionId,
};

use super::{
    chain::{validate_chains, ChainError},
    clash::parse_clash_yaml,
    format::{decode_document, FormatError, SubscriptionDocument},
    uri::parse_any_proxy_uri,
};

pub const MAX_SUBSCRIPTION_NODES: usize = 10_000;
pub type SubscriptionNodes = BoundedVec<DialableNode, MAX_SUBSCRIPTION_NODES>;
pub type NodeIds = BoundedVec<caly_domain::NodeId, MAX_SUBSCRIPTION_NODES>;

/// Deterministic first-seen dedupe result.
pub struct DedupeResult {
    pub nodes: SubscriptionNodes,
    pub duplicate_count: usize,
}

/// Added/removed identity diff; metadata-only changes are not false additions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionDiff {
    pub added: NodeIds,
    pub removed: NodeIds,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PipelineError {
    TooManyNodes,
    UnsupportedDocument,
    Format(FormatError),
    Uri,
    /// Every recognized line was `ssr://`; SSR is not representable, so no
    /// usable node exists. Carries the number of skipped SSR entries.
    SsrOnly(usize),
    Display,
    Clash,
    /// The chain (dialer-proxy dependency) graph is invalid.
    Chain(ChainError),
}

impl core::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyNodes => formatter.write_str("subscription has too many nodes"),
            Self::UnsupportedDocument => formatter.write_str("document format is not supported"),
            Self::Format(error) => write!(formatter, "subscription decode failed: {error:?}"),
            Self::Uri => formatter.write_str("no parseable proxy nodes"),
            Self::SsrOnly(count) => write!(
                formatter,
                "all {count} nodes are SSR, which is not representable"
            ),
            Self::Display => formatter.write_str("node display projection failed"),
            Self::Clash => formatter.write_str("Clash YAML parsing failed"),
            Self::Chain(error) => write!(formatter, "chain validation failed: {error}"),
        }
    }
}

/// Redacted subscription projection with explicit rejected-line count.
pub struct SubscriptionProjection {
    pub nodes: SnapshotNodes,
    pub rejected_lines: usize,
    /// Recognized `ssr://` lines: SSR is not representable, so they are
    /// skipped deliberately rather than counted as parse rejections.
    pub ssr_skipped: usize,
}

/// Decodes URI-line/base64 or Clash-YAML subscriptions into redacted display
/// nodes (the strict, fail-on-any-line variant).
pub fn parse_uri_body_to_display(
    body: Vec<u8>,
    subscription: SubscriptionId,
) -> Result<SnapshotNodes, PipelineError> {
    let document = decode_document(body).map_err(PipelineError::Format)?;
    let nodes = document_dialable(document, subscription)?;
    project_display(nodes)
}

/// Decodes a subscription while retaining bounded diagnostics for rejected
/// lines. URI-line bodies count individually rejected lines; Clash-YAML bodies
/// parse as a whole document (a parse failure rejects the document).
pub fn parse_uri_body_to_display_lossy(
    body: Vec<u8>,
    subscription: SubscriptionId,
) -> Result<SubscriptionProjection, PipelineError> {
    let document = decode_document(body).map_err(PipelineError::Format)?;
    parse_document_to_display_lossy(&document, subscription)
}

/// [`parse_uri_body_to_display_lossy`] on an already-decoded document —
/// the single-pass pipeline entry: the daemon decodes a subscription body
/// once per refresh and shares the document across the projection, Mihomo
/// render, sing-box render and routing consumers (2026-08-12 refactor;
/// the body used to be decoded up to four times per refresh).
pub fn parse_document_to_display_lossy(
    document: &SubscriptionDocument,
    subscription: SubscriptionId,
) -> Result<SubscriptionProjection, PipelineError> {
    let (nodes, rejected_lines, ssr_skipped) = match document {
        SubscriptionDocument::UriLines { lines, .. } => {
            let mut nodes = Vec::new();
            let mut rejected = 0_usize;
            let mut ssr = 0_usize;
            for line in lines {
                // SSR is recognized but not representable; skip it without
                // counting it as a parse rejection.
                if line.as_str().starts_with("ssr://") {
                    ssr = ssr.saturating_add(1);
                    continue;
                }
                match parse_any_proxy_uri(line.as_str(), subscription) {
                    Ok(node) => nodes.push(node),
                    Err(_) => rejected = rejected.saturating_add(1),
                }
            }
            (nodes, rejected, ssr)
        }
        SubscriptionDocument::ClashYaml(body) => {
            let source = core::str::from_utf8(body.as_slice()).map_err(|_| PipelineError::Clash)?;
            (
                parse_clash_yaml(source, subscription).map_err(|_| PipelineError::Clash)?,
                0,
                0,
            )
        }
        SubscriptionDocument::Sip008(body) => {
            let source = core::str::from_utf8(body.as_slice()).map_err(|_| PipelineError::Uri)?;
            let (nodes, skipped) = super::parse_sip008(source, subscription);
            (nodes, skipped, 0)
        }
        // URL lists are expanded by the daemon fetch path before node
        // parsing; reaching this point means an unsupported nested list.
        SubscriptionDocument::UrlList(_) => return Err(PipelineError::UnsupportedDocument),
    };
    if nodes.is_empty() {
        if ssr_skipped > 0 && rejected_lines == 0 {
            return Err(PipelineError::SsrOnly(ssr_skipped));
        }
        return Err(PipelineError::Uri);
    }
    let display = project_display(nodes)?;
    Ok(SubscriptionProjection {
        nodes: display,
        rejected_lines,
        ssr_skipped,
    })
}

/// Extracts dialable nodes from a decoded document (URI lines or Clash YAML).
fn document_dialable(
    document: SubscriptionDocument,
    subscription: SubscriptionId,
) -> Result<Vec<DialableNode>, PipelineError> {
    match document {
        // URL lists are expanded by the daemon fetch path before parsing.
        SubscriptionDocument::UrlList(_) => Err(PipelineError::UnsupportedDocument),
        SubscriptionDocument::Sip008(body) => {
            let source = core::str::from_utf8(body.as_slice()).map_err(|_| PipelineError::Uri)?;
            Ok(super::parse_sip008(source, subscription).0)
        }
        SubscriptionDocument::UriLines { lines, .. } => lines
            .iter()
            .map(|line| {
                parse_any_proxy_uri(line.as_str(), subscription).map_err(|_| PipelineError::Uri)
            })
            .collect(),
        SubscriptionDocument::ClashYaml(body) => {
            let source = core::str::from_utf8(body.as_slice()).map_err(|_| PipelineError::Clash)?;
            parse_clash_yaml(source, subscription).map_err(|_| PipelineError::Clash)
        }
    }
}

/// Deduplicates dialable nodes and projects them as redacted display nodes.
///
/// Repeated display names receive a deterministic numeric suffix (`#2`, ...)
/// in first-seen order so the projection, registry and kernel config all use
/// the exact same canonical tag (see [`dedupe_name_tags`]); list UIs and
/// `caly core delay|select` therefore address the same names the kernel does.
fn project_display(nodes: Vec<DialableNode>) -> Result<SnapshotNodes, PipelineError> {
    // Chain validation (dialer-proxy dependencies) runs on the raw node set
    // before dedupe/projection: a missing or cyclic dialer reference is a
    // source-format problem and must surface early (2026-08-13 pipeline
    // review — this gate previously existed as `validate_chains` but was
    // never wired into the pipeline).
    validate_chains(&nodes).map_err(PipelineError::Chain)?;
    let deduped = dedupe(nodes)?;
    let projected = deduped
        .nodes
        .iter()
        .map(|node| node.display(true, None).map_err(|_| PipelineError::Display))
        .collect::<Result<Vec<_>, _>>()?;
    let names = projected
        .iter()
        .map(|node| node.name().as_str().to_owned())
        .collect::<Vec<_>>();
    let tags = dedupe_name_tags(names.iter().map(String::as_str));
    let display = projected
        .into_iter()
        .zip(tags)
        .map(|(node, tag)| {
            let name = NodeDisplayName::new(tag).map_err(|_| PipelineError::Display)?;
            Ok::<_, PipelineError>(DisplayNode::new(
                node.id(),
                name,
                node.protocol().clone(),
                node.is_available(),
                node.latency_ms(),
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    SnapshotNodes::try_from_vec(display).map_err(|_| PipelineError::TooManyNodes)
}

/// Assigns deterministic `#N` suffixes to repeated display names in first-seen
/// order, keeping human-readable names unique without losing the base name.
///
/// Both the projection (list UIs) and the Mihomo/registry renderer call this
/// over the same post-NodeId-dedupe order, so the suffix numbering they see is
/// identical for every node.
///
/// Audit #101: the suffix budget is reserved *before* appending — a base name
/// at the 256-byte display cap used to become a 259-byte tag that every
/// downstream `NodeDisplayName::new`/`MihomoProxyTag::new` rejected (whole
/// refresh failed, or nodes were silently dropped on the Mihomo path).
pub fn dedupe_name_tags<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    names
        .into_iter()
        .map(|name| {
            let seen = counts.entry(name).or_insert(0);
            *seen += 1;
            if *seen == 1 {
                name.to_owned()
            } else {
                let suffix = format!(" #{seen}");
                let budget = caly_domain::NODE_DISPLAY_NAME_MAX_BYTES.saturating_sub(suffix.len());
                let mut base: String = name
                    .chars()
                    .scan(0_usize, |used, ch| {
                        let width = ch.len_utf8();
                        if *used + width <= budget {
                            *used += width;
                            Some(ch)
                        } else {
                            None
                        }
                    })
                    .collect();
                base.push_str(&suffix);
                base
            }
        })
        .collect()
}

/// Deduplicates by complete canonical NodeId, preserving first input order.
pub fn dedupe(nodes: Vec<DialableNode>) -> Result<DedupeResult, PipelineError> {
    if nodes.len() > MAX_SUBSCRIPTION_NODES {
        return Err(PipelineError::TooManyNodes);
    }
    let mut unique = BTreeMap::new();
    let mut order = Vec::new();
    let mut duplicate_count = 0;
    for node in nodes {
        let id = node.id();
        match unique.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                order.push(id);
                entry.insert(node);
            }
            std::collections::btree_map::Entry::Occupied(_) => duplicate_count += 1,
        }
    }
    let ordered = order
        .into_iter()
        .filter_map(|id| unique.remove(&id))
        .collect();
    let nodes =
        SubscriptionNodes::try_from_vec(ordered).map_err(|_| PipelineError::TooManyNodes)?;
    Ok(DedupeResult {
        nodes,
        duplicate_count,
    })
}

/// Computes an identity-only bounded diff.
pub fn diff(
    previous: &[caly_domain::NodeId],
    current: &SubscriptionNodes,
) -> Result<SubscriptionDiff, PipelineError> {
    let previous: BTreeSet<_> = previous.iter().copied().collect();
    let current: BTreeSet<_> = current.iter().map(DialableNode::id).collect();
    let added = current.difference(&previous).copied().collect();
    let removed = previous.difference(&current).copied().collect();
    Ok(SubscriptionDiff {
        added: NodeIds::try_from_vec(added).map_err(|_| PipelineError::TooManyNodes)?,
        removed: NodeIds::try_from_vec(removed).map_err(|_| PipelineError::TooManyNodes)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub() -> SubscriptionId {
        SubscriptionId::from_bytes([0; 16])
    }

    fn ss_uri(name: &str) -> String {
        use base64::Engine as _;
        let credential =
            base64::engine::general_purpose::STANDARD.encode("aes-256-gcm:pw".as_bytes());
        format!("ss://{credential}@192.0.2.1:8388#{name}")
    }

    #[test]
    fn dedupe_suffix_stays_within_the_display_byte_cap() {
        // Audit #101: two 256-byte identical names must not produce a
        // rejectable (over-cap) suffixed tag.
        let long = "我".repeat(85); // 255 bytes
        let long = format!("{long}x"); // 256 bytes, at NODE_DISPLAY_NAME_MAX_BYTES
        let tags = dedupe_name_tags([long.as_str(), long.as_str()]);
        assert!(tags[1].len() <= caly_domain::NODE_DISPLAY_NAME_MAX_BYTES);
        assert!(tags[1].ends_with(" #2"));
        assert!(std::str::from_utf8(tags[1].as_bytes()).is_ok());
    }

    #[test]
    fn ssr_lines_are_skipped_and_counted_in_mixed_documents() {
        let body = format!("{}\nssr://cGxhY2Vob2xkZXI\n", ss_uri("keep"));
        let projection = parse_uri_body_to_display_lossy(body.into_bytes(), sub())
            .unwrap_or_else(|e| panic!("parse failed: {e:?}"));
        assert_eq!(projection.nodes.len(), 1);
        assert_eq!(projection.rejected_lines, 0);
        assert_eq!(projection.ssr_skipped, 1);
    }

    #[test]
    fn pure_ssr_documents_report_ssr_only() {
        let body = "ssr://cGxhY2Vob2xkZXI\nssr://YW5vdGhlcg\n";
        let result = parse_uri_body_to_display_lossy(body.as_bytes().to_vec(), sub());
        assert!(matches!(result, Err(PipelineError::SsrOnly(2))));
    }

    /// A minimal Clash YAML subscription with two representable proxies.
    fn clash_yaml_body() -> Vec<u8> {
        br#"proxies:
  - name: "hk-01"
    type: ss
    server: example.com
    port: 8388
    cipher: aes-128-gcm
    password: secret
  - name: "jp-02"
    type: trojan
    server: example.org
    port: 443
    password: pass
"#
        .to_vec()
    }

    #[test]
    fn clash_yaml_is_projected_to_display_nodes() -> Result<(), String> {
        let id = SubscriptionId::from_bytes([9; 16]);
        let nodes =
            parse_uri_body_to_display(clash_yaml_body(), id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(nodes.len(), 2, "clash yaml must project both proxies");
        Ok(())
    }

    #[test]
    fn clash_yaml_lossy_projection_reports_no_rejected_lines() -> Result<(), String> {
        let id = SubscriptionId::from_bytes([10; 16]);
        let projection =
            parse_uri_body_to_display_lossy(clash_yaml_body(), id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(projection.nodes.len(), 2);
        assert_eq!(projection.rejected_lines, 0);
        Ok(())
    }
    #[test]
    fn supplied_subscription_projects_redacted_nodes() {
        let body = include_bytes!("../../../fixtures/subscription-20260803.txt").to_vec();
        let projection = parse_uri_body_to_display_lossy(body, SubscriptionId::from_bytes([3; 16]));
        assert!(projection.is_ok());
        let projection = projection.map_or_else(|_| SnapshotNodes::new(), |value| value.nodes);
        assert!(!projection.is_empty());
    }

    #[test]
    fn duplicate_names_receive_deterministic_numeric_suffixes() {
        let names = ["a", "b", "a", "a", "b", "c"];
        let tags = dedupe_name_tags(names.iter().copied());
        assert_eq!(
            tags,
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "a #2".to_owned(),
                "a #3".to_owned(),
                "b #2".to_owned(),
                "c".to_owned()
            ]
        );
    }
}
