//! Offline subscription parsing diagnostics (`caly sub check`).
//!
//! Reads a subscription file (Clash YAML, direct URI lines, or Base64 URI
//! aggregate), detects its format, and reports the parsed node count and any
//! rejected lines. This is a pure, offline preview of what the daemon's
//! subscription pipeline would ingest.

use std::path::Path;

use caly_domain::SubscriptionId;
use caly_subscription::{SubscriptionFormat, decode_document, parse_uri_body_to_display_lossy};

/// Outcome of parsing a subscription file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionSummary {
    pub format: String,
    pub node_count: usize,
    pub rejected_lines: usize,
    /// Recognized `ssr://` nodes skipped (SSR is not representable).
    pub ssr_skipped: usize,
    pub names: Vec<String>,
    /// Optional Clash subscription quota metadata (upload/download/total/expire).
    pub userinfo: caly_subscription::SubscriptionUserInfo,
}

/// Reads and parses `path`, returning a human-readable summary. An optional
/// `subscription-userinfo` header value supplies quota metadata.
pub fn inspect_subscription_with_userinfo(
    path: &Path,
    userinfo_header: Option<&str>,
) -> Result<SubscriptionSummary, String> {
    let body =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    inspect_subscription(body, userinfo_header)
}

/// Reads proxy URIs from the system clipboard and parses them as a synthetic
/// document. Tries Wayland (`wl-paste`) first, then X11 (`xclip`, `xsel`).
/// Returns an error that also suggests `caly sub check <file>` as a fallback.
/// Reads proxy URIs from the system clipboard and parses them as a synthetic
/// document. Tries Wayland (`wl-paste`) first, then X11 (`xclip`, `xsel`).
/// Returns an error that also suggests `caly sub check <file>` as a fallback.
pub fn read_clipboard() -> Result<SubscriptionSummary, String> {
    const TOOLS: [(&str, &[&str]); 3] = [
        ("wl-paste", &[]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    read_clipboard_with(&TOOLS)
}

/// Core implementation of `read_clipboard` over an injectable candidate list,
/// so the fallback-error path is unit-testable without mutating `PATH`.
fn read_clipboard_with(tools: &[(&str, &[&str])]) -> Result<SubscriptionSummary, String> {
    use std::process::{Command, Stdio};
    let mut errors = Vec::new();
    for &(tool, args) in tools {
        let output = Command::new(tool)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                return inspect_subscription(output.stdout, None);
            }
            Ok(_) => errors.push(tool.to_owned()),
            Err(_) => errors.push(format!("{tool}: not found")),
        }
    }
    Err(format!(
        "no clipboard tool available (tried {}); paste the URIs into a file and run `caly sub check <file>`",
        errors.join(", ")
    ))
}

/// Parses an in-memory subscription body (from a file or clipboard) into a
/// summary, sharing the offline pipeline with `caly sub check`.
pub fn inspect_subscription(
    body: Vec<u8>,
    userinfo_header: Option<&str>,
) -> Result<SubscriptionSummary, String> {
    let userinfo = userinfo_header.map_or_else(
        || caly_subscription::SubscriptionUserInfo {
            upload_bytes: None,
            download_bytes: None,
            total_bytes: None,
            expire_unix: None,
        },
        caly_subscription::parse_subscription_userinfo,
    );
    // URL-list documents carry no nodes offline; the daemon fetches and merges
    // the children during refresh. Report the entries instead of parsing.
    if let Ok(caly_subscription::SubscriptionDocument::UrlList(lines)) =
        decode_document(body.clone())
    {
        return Ok(SubscriptionSummary {
            format: "url-list".to_owned(),
            node_count: 0,
            rejected_lines: 0,
            ssr_skipped: 0,
            names: lines.iter().map(|line| line.as_str().to_owned()).collect(),
            userinfo,
        });
    }
    let format = detect_format(&body).unwrap_or_else(|| "unknown".to_owned());
    // A stable, all-zero subscription id for an offline preview.
    let subscription = SubscriptionId::from_bytes([0; 16]);
    let projection = match parse_uri_body_to_display_lossy(body, subscription) {
        Ok(projection) => projection,
        Err(caly_subscription::PipelineError::SsrOnly(count)) => {
            return Ok(SubscriptionSummary {
                format: "ssr-only".to_owned(),
                node_count: 0,
                rejected_lines: 0,
                ssr_skipped: count,
                names: Vec::new(),
                userinfo,
            });
        }
        Err(error) => return Err(format!("subscription parse failed: {error:?}")),
    };
    let names = projection
        .nodes
        .iter()
        .map(|node| node.name().as_str().to_owned())
        .collect();
    Ok(SubscriptionSummary {
        format,
        node_count: projection.nodes.len(),
        rejected_lines: projection.rejected_lines,
        ssr_skipped: projection.ssr_skipped,
        names,
        userinfo,
    })
}

/// Outcome of the W3a tree-format parse (`sub parse`).
///
/// `Tree` carries the full entry tree (three-zone human layout or the
/// §6.1 JSON contract) plus the bounded diagnostics URI-line documents
/// retain; `Summary` is the pre-W3a shape kept for documents that have
/// no tree at all (`url-list` — a fetchable source with no offline
/// nodes — and `ssr-only`).
#[derive(Debug)]
pub enum ParseOutcome {
    /// Full entry tree (Clash YAML / URI lines / base64).
    Tree {
        tree: crate::entry_tree::EntryTree,
        rejected_lines: usize,
        ssr_skipped: usize,
        userinfo: caly_subscription::SubscriptionUserInfo,
    },
    /// Non-tree legacy shape (url-list / ssr-only).
    Summary(SubscriptionSummary),
}

impl ParseOutcome {
    /// Whether the parsed document is usable (mirrors the summary
    /// semantics: dialable nodes, or a URL list the daemon expands).
    pub fn is_usable(&self) -> bool {
        match self {
            Self::Tree { tree, .. } => tree.protocol_count() > 0,
            Self::Summary(summary) => is_usable(summary),
        }
    }
}

/// W3a (`cli-v3-design.md` G1/§5.3/§6.1): parses a subscription file into
/// the entry tree. Clash YAML gets the full three-zone tree; URI-line and
/// base64 documents degrade to the protocol-only listing; url-list and
/// ssr-only keep the legacy summary shape. The optional `userinfo` header
/// value supplies quota metadata (kept in the JSON envelope for
/// backward compatibility).
pub fn parse_subscription_tree(
    path: &Path,
    userinfo_header: Option<&str>,
) -> Result<ParseOutcome, String> {
    let body =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_subscription_tree_body(body, userinfo_header)
}

/// [`parse_subscription_tree`] over an in-memory body (unit-testable).
pub fn parse_subscription_tree_body(
    body: Vec<u8>,
    userinfo_header: Option<&str>,
) -> Result<ParseOutcome, String> {
    let userinfo = userinfo_header.map_or_else(
        || caly_subscription::SubscriptionUserInfo {
            upload_bytes: None,
            download_bytes: None,
            total_bytes: None,
            expire_unix: None,
        },
        caly_subscription::parse_subscription_userinfo,
    );
    let subscription = SubscriptionId::from_bytes([0; 16]);
    match decode_document(body.clone()) {
        // URL-list documents carry no nodes offline; the daemon fetches
        // and merges the children during refresh. Keep the legacy
        // summary render.
        Ok(caly_subscription::SubscriptionDocument::UrlList(lines)) => {
            Ok(ParseOutcome::Summary(SubscriptionSummary {
                format: "url-list".to_owned(),
                node_count: 0,
                rejected_lines: 0,
                ssr_skipped: 0,
                names: lines.iter().map(|line| line.as_str().to_owned()).collect(),
                userinfo,
            }))
        }
        // Clash YAML: the full tree — groups, ungrouped nodes, rules.
        Ok(caly_subscription::SubscriptionDocument::ClashYaml(yaml)) => {
            let source = String::from_utf8(yaml.to_vec())
                .map_err(|_| "subscription parse failed: Clash body is not UTF-8".to_owned())?;
            let import = caly_subscription::parse_clash_config(&source, subscription)
                .map_err(|error| format!("subscription parse failed: {error:?}"))?;
            Ok(ParseOutcome::Tree {
                tree: crate::entry_tree::from_clash_import(&import, "clash-yaml"),
                rejected_lines: 0,
                ssr_skipped: 0,
                userinfo,
            })
        }
        // URI lines / base64 aggregates: the degenerate protocol-only
        // listing (no group zone, no rule zone — G-§3.2).
        Ok(caly_subscription::SubscriptionDocument::UriLines { source_format, .. }) => {
            match parse_uri_body_to_display_lossy(body, subscription) {
                Ok(projection) => {
                    let nodes: Vec<(String, String)> = projection
                        .nodes
                        .iter()
                        .map(|node| {
                            (
                                node.name().as_str().to_owned(),
                                crate::entry_tree::badge_kind(node.protocol().as_str()),
                            )
                        })
                        .collect();
                    let format = match source_format {
                        caly_subscription::SubscriptionFormat::UriLines => "uri-lines",
                        caly_subscription::SubscriptionFormat::Base64UriLines => "base64-uri-lines",
                        // The classifier only yields UriLines for these two.
                        caly_subscription::SubscriptionFormat::ClashYaml => "clash-yaml",
                    };
                    Ok(ParseOutcome::Tree {
                        tree: crate::entry_tree::from_uri_nodes(format, nodes),
                        rejected_lines: projection.rejected_lines,
                        ssr_skipped: projection.ssr_skipped,
                        userinfo,
                    })
                }
                Err(caly_subscription::PipelineError::SsrOnly(count)) => {
                    Ok(ParseOutcome::Summary(SubscriptionSummary {
                        format: "ssr-only".to_owned(),
                        node_count: 0,
                        rejected_lines: 0,
                        ssr_skipped: count,
                        names: Vec::new(),
                        userinfo,
                    }))
                }
                Err(error) => Err(format!("subscription parse failed: {error:?}")),
            }
        }
        // Sip008 and undecodable documents keep the pre-W3a summary
        // path (the tree faces only cover Clash YAML and URI lines).
        _ => {
            let summary = inspect_subscription(body, userinfo_header)?;
            Ok(ParseOutcome::Summary(summary))
        }
    }
}

/// Renders a tree parse outcome as human text: the entry tree plus a
/// trailing diagnostic note when URI-line documents rejected or skipped
/// lines (never on the JSON face).
pub fn render_tree_human(outcome: &ParseOutcome) -> String {
    match outcome {
        ParseOutcome::Tree {
            tree,
            rejected_lines,
            ssr_skipped,
            ..
        } => {
            let mut out = crate::entry_tree::render_human(tree);
            if *rejected_lines > 0 {
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!("# {rejected_lines} line(s) rejected\n"),
                );
            }
            if *ssr_skipped > 0 {
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!("# {ssr_skipped} SSR node(s) skipped (not representable)\n"),
                );
            }
            out
        }
        ParseOutcome::Summary(summary) => render_human(summary),
    }
}

/// Renders a tree parse outcome as §6.1 JSON. The quota fields are
/// appended at the top level only when the userinfo header carried a
/// total (backward-compatible with the pre-W3a `sub parse --json`
/// envelope).
pub fn render_tree_json(outcome: &ParseOutcome) -> String {
    match outcome {
        ParseOutcome::Tree { tree, userinfo, .. } => {
            let mut value = crate::entry_tree::render_json(tree);
            if let Some(total) = userinfo.total_bytes {
                let used = userinfo
                    .upload_bytes
                    .unwrap_or(0)
                    .saturating_add(userinfo.download_bytes.unwrap_or(0));
                value["quota_total"] = serde_json::json!(total);
                value["quota_used"] = serde_json::json!(used);
                value["quota_remaining"] = serde_json::json!(total.saturating_sub(used));
            }
            value.to_string()
        }
        ParseOutcome::Summary(summary) => render_json(summary),
    }
}

/// Detects the top-level subscription format without failing on parse details.
fn detect_format(body: &[u8]) -> Option<String> {
    use caly_subscription::SubscriptionDocument;
    match decode_document(body.to_vec()) {
        Ok(SubscriptionDocument::ClashYaml(_)) => Some("clash-yaml".to_owned()),
        Ok(SubscriptionDocument::UrlList(_)) => Some("url-list".to_owned()),
        Ok(SubscriptionDocument::Sip008(_)) => Some("sip008".to_owned()),
        Ok(SubscriptionDocument::UriLines { source_format, .. }) => Some(match source_format {
            SubscriptionFormat::UriLines => "uri-lines".to_owned(),
            SubscriptionFormat::Base64UriLines => "base64-uri-lines".to_owned(),
            SubscriptionFormat::ClashYaml => "clash-yaml".to_owned(),
        }),
        Err(_) => None,
    }
}

/// Renders a summary as human-readable text.
pub fn render_human(summary: &SubscriptionSummary) -> String {
    let mut out = format!(
        "format:      {}\nnodes:       {}\nrejected:    {}\n",
        summary.format, summary.node_count, summary.rejected_lines
    );
    if summary.format == "ssr-only" {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "note:        all {} nodes are SSR, which caly cannot represent;\n             convert the subscription to a supported format\n",
                summary.ssr_skipped
            ),
        );
    } else if summary.ssr_skipped > 0 {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "ssr-skipped: {} (SSR nodes are recognized but not representable)\n",
                summary.ssr_skipped
            ),
        );
    }
    if summary.format == "url-list" {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "entries:     {} (child subscriptions; the daemon fetches and merges them on refresh)\n",
                summary.names.len()
            ),
        );
    }
    let usage = caly_subscription::render_usage_human(&summary.userinfo);
    if !usage.is_empty() {
        out.push_str(&usage);
        out.push('\n');
    }
    for (index, name) in summary.names.iter().enumerate() {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("  {index}: {name}\n"));
    }
    out
}

/// Renders a summary as one-line JSON.
pub fn render_json(summary: &SubscriptionSummary) -> String {
    let mut value = serde_json::json!({
        "format": summary.format,
        "nodes": summary.node_count,
        "rejected": summary.rejected_lines,
        "ssr_skipped": summary.ssr_skipped,
        "names": summary.names,
    });
    if let Some(total) = summary.userinfo.total_bytes {
        let used = summary
            .userinfo
            .upload_bytes
            .unwrap_or(0)
            .saturating_add(summary.userinfo.download_bytes.unwrap_or(0));
        let remaining = total.saturating_sub(used);
        value["quota_total"] = serde_json::json!(total);
        value["quota_used"] = serde_json::json!(used);
        value["quota_remaining"] = serde_json::json!(remaining);
    }
    value.to_string()
}

/// Returns whether the subscription parsed with any nodes and no hard failure.
/// Returns whether the subscription parsed into something the refresh path can
/// use: dialable nodes, or a URL list the daemon expands at fetch time.
pub fn is_usable(summary: &SubscriptionSummary) -> bool {
    summary.node_count > 0 || summary.format == "url-list"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_file(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("caly-sub-{label}-{}", std::process::id()))
    }

    fn write_then_inspect(label: &str, body: &str) -> Result<SubscriptionSummary, String> {
        let path = unique_file(label);
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        let summary = inspect_subscription_with_userinfo(&path, None);
        let _ = std::fs::remove_file(&path);
        summary
    }

    /// Builds a valid shadowsocks URI from a method:password pair.
    fn ss_uri(method: &str, password: &str, host: &str, port: u16, name: &str) -> String {
        use base64::{Engine as _, engine::general_purpose};
        let credentials = general_purpose::STANDARD.encode(format!("{method}:{password}"));
        format!("ss://{credentials}@{host}:{port}#{name}")
    }

    #[test]
    fn parses_direct_uri_lines() -> Result<(), String> {
        let a = ss_uri("aes-256-gcm", "pw1", "example.com", 8388, "node-a");
        let b = ss_uri("aes-128-gcm", "pw2", "example.org", 443, "node-b");
        let summary = write_then_inspect("direct", &format!("{a}\n{b}"))?;
        assert_eq!(summary.format, "uri-lines");
        assert!(summary.node_count >= 2);
        assert!(is_usable(&summary));
        assert!(summary.names.iter().any(|n| n == "node-a"));
        Ok(())
    }

    #[test]
    fn detects_base64_aggregate() -> Result<(), String> {
        use base64::{Engine as _, engine::general_purpose};
        let a = ss_uri("aes-256-gcm", "pw1", "example.com", 8388, "a");
        let b = ss_uri("aes-128-gcm", "pw2", "example.org", 443, "b");
        let encoded = general_purpose::STANDARD.encode(format!("{a}\n{b}"));
        let summary = write_then_inspect("b64", &encoded)?;
        assert_eq!(summary.format, "base64-uri-lines");
        assert!(summary.node_count >= 2);
        Ok(())
    }

    #[test]
    fn render_human_and_json_are_well_formed() -> Result<(), String> {
        let uri = ss_uri("aes-256-gcm", "pw1", "example.com", 8388, "node-x");
        let summary = write_then_inspect("render", &uri)?;
        let human = render_human(&summary);
        assert!(human.contains("format:"));
        assert!(human.contains("node-x"));
        let json = render_json(&summary);
        assert!(json.starts_with('{') && json.contains("\"nodes\""));
        Ok(())
    }

    #[test]
    fn unusable_when_no_nodes() {
        let summary = write_then_inspect("garbage", "this is not a subscription");
        // Either it parses to zero nodes or errors; both are fine, but if it
        // parses it must be unusable.
        if let Ok(summary) = summary {
            assert!(!is_usable(&summary));
        }
    }

    #[test]
    fn imports_mixed_protocol_uri_list_in_memory() -> Result<(), String> {
        // Mirrors `caly sub import`: an in-memory body with the protocols a
        // clipboard paste typically carries (vless/vmess/ss/hysteria2/trojan).
        let vless = "vless://b6f3b292-00c9-430e-bc4c-b1294bd895c0@185.126.67.76:443?security=reality&type=tcp&packetEncoding=none&sni=cdn-de-4.ai-apiroute.cc&fp=firefox&flow=xtls-rprx-vision&sid=b512d49930874a31&pbk=abc#DE_1";
        let vmess_line = "vmess://eyJhZGQiOiIxODUuMTI2LjY3Ljc2IiwicG9ydCI6NDQzLCJpZCI6ImI2ZjNiMjkyLTAwYzktNDMwZS1iYzRjLWIxMjk0YmQ4OTVjMCIsImFpZCI6MCwibmV0IjoidGNwIiwidGxzIjoiIiwicHMiOiJVUzEifQ==";
        let hy2 = "hysteria2://H7mP2xY9kJ4nQ8wR5tF6vB3z@vpn-tw-002.fastervpn.world:443?insecure=1&sni=vpn-tw-002.fastervpn.world#TW_1";
        let ss = ss_uri("aes-256-gcm", "pw1", "example.com", 8388, "node-a");
        let trojan = "trojan://ND91608427@nearby-egret.rooster465.autos:443?sni=nearby-egret.rooster465.autos&type=tcp#JP_2";
        let bad = "ssr://ssrnotrepresentable";
        let body = format!("{vless}\n{vmess_line}\n{hy2}\n{ss}\n{trojan}\n{bad}\nnot-a-uri");
        let summary = inspect_subscription(body.into_bytes(), None)?;
        assert_eq!(summary.format, "uri-lines");
        assert!(summary.node_count >= 5, "nodes={}", summary.node_count);
        assert_eq!(summary.rejected_lines, 1, "the bare text line rejects");
        assert_eq!(summary.ssr_skipped, 1);
        assert!(summary.names.contains(&"DE_1".to_owned()));
        assert!(summary.names.contains(&"TW_1".to_owned()));
        assert!(summary.names.contains(&"JP_2".to_owned()));
        Ok(())
    }

    #[test]
    fn import_rejects_clipboard_without_tool_gracefully() {
        // `read_clipboard` degrades to a helpful error when no paste tool is
        // resolvable; it must never panic.
        let error = read_clipboard_with(&[("definitely-not-a-real-tool", &[])])
            .err()
            .unwrap_or_else(|| "clipboard read unexpectedly succeeded".to_owned());
        assert!(
            error.contains("no clipboard tool available"),
            "unexpected error: {error}"
        );
    }

    // ── W3a tree parse (`sub parse`) ────────────────────────────────

    /// Small Clash document: one selector with a nested urltest group,
    /// two nodes and one rule.
    const TREE_YAML: &str = r#"
proxies:
  - {name: hk-01, type: vmess, server: a.example.com, port: 443, uuid: "11111111-1111-1111-1111-111111111111", alterId: 0, cipher: auto}
  - {name: us-02, type: ss, server: b.example.com, port: 8388, cipher: aes-256-gcm, password: pw}
proxy-groups:
  - {name: 节点选择, type: select, proxies: [自动选择, hk-01, DIRECT]}
  - {name: 自动选择, type: url-test, proxies: [hk-01, us-02], url: "http://cp.example.com/generate_204", interval: 600, tolerance: 200}
rules:
  - DOMAIN-SUFFIX,example.com,节点选择
"#;

    #[test]
    fn clash_yaml_parses_to_full_tree() -> Result<(), String> {
        let outcome = parse_subscription_tree_body(TREE_YAML.as_bytes().to_vec(), None)?;
        match &outcome {
            ParseOutcome::Tree {
                tree,
                rejected_lines,
                ssr_skipped,
                ..
            } => {
                assert_eq!(tree.format, "clash-yaml");
                assert_eq!(tree.groups.len(), 2);
                assert_eq!(tree.protocol_count(), 2);
                assert_eq!(tree.rules.len(), 1);
                assert_eq!(*rejected_lines, 0);
                assert_eq!(*ssr_skipped, 0);
                let human = render_tree_human(&outcome);
                assert!(human.contains("[selector]     节点选择"), "human: {human}");
                assert!(human.contains("rules zone"), "human: {human}");
                assert!(
                    !human.contains("rejected"),
                    "clean doc has no note: {human}"
                );
                let json = render_tree_json(&outcome);
                assert!(json.contains("\"counts\""), "json: {json}");
                assert!(json.contains("\"ok\":true"), "json: {json}");
            }
            ParseOutcome::Summary(_) => panic!("expected tree, got summary"),
        }
        Ok(())
    }

    #[test]
    fn uri_lines_degrade_to_protocol_listing_with_notes() -> Result<(), String> {
        let ss = ss_uri("aes-256-gcm", "pw1", "example.com", 8388, "node-a");
        let body = format!("{ss}\nnot-a-uri\nssr://skipped");
        let outcome = parse_subscription_tree_body(body.as_bytes().to_vec(), None)?;
        match &outcome {
            ParseOutcome::Tree {
                tree,
                rejected_lines,
                ssr_skipped,
                ..
            } => {
                assert_eq!(tree.format, "uri-lines");
                assert!(!tree.has_groups());
                assert_eq!(tree.ungrouped.len(), 1);
                assert_eq!(*rejected_lines, 1);
                assert_eq!(*ssr_skipped, 1);
                let human = render_tree_human(&outcome);
                assert!(human.contains("# 1 line(s) rejected"), "human: {human}");
                assert!(human.contains("# 1 SSR node(s) skipped"), "human: {human}");
                // The listing carries no zone title (degenerate form).
                assert!(!human.contains("未入组节点"), "human: {human}");
                let json = render_tree_json(&outcome);
                assert!(!json.contains("rejected"), "notes never reach JSON: {json}");
            }
            ParseOutcome::Summary(_) => panic!("expected tree, got summary"),
        }
        Ok(())
    }

    #[test]
    fn url_list_keeps_the_legacy_summary_shape() -> Result<(), String> {
        let body = "https://a.example.com/sub\nhttps://b.example.com/sub";
        let outcome = parse_subscription_tree_body(body.as_bytes().to_vec(), None)?;
        match &outcome {
            ParseOutcome::Summary(summary) => {
                assert_eq!(summary.format, "url-list");
                assert_eq!(summary.names.len(), 2);
                assert!(outcome.is_usable());
                let human = render_tree_human(&outcome);
                assert!(human.contains("entries:"), "human: {human}");
            }
            ParseOutcome::Tree { .. } => panic!("expected summary, got tree"),
        }
        Ok(())
    }

    #[test]
    fn ssr_only_document_keeps_the_legacy_summary_shape() -> Result<(), String> {
        let body = "ssr://ssrnotrepresentable\nssr://anotherone";
        let outcome = parse_subscription_tree_body(body.as_bytes().to_vec(), None)?;
        match &outcome {
            ParseOutcome::Summary(summary) => {
                assert_eq!(summary.format, "ssr-only");
                assert_eq!(summary.ssr_skipped, 2);
                assert!(!outcome.is_usable());
            }
            ParseOutcome::Tree { .. } => panic!("expected summary, got tree"),
        }
        Ok(())
    }

    #[test]
    fn unusable_tree_exits_nonzero_but_renders() -> Result<(), String> {
        // A Clash document with no nodes is a parse success but unusable;
        // the tree render still works and `is_usable` is false.
        let body = "proxies: []\nproxy-groups: []\nrules: []\n";
        let outcome = parse_subscription_tree_body(body.as_bytes().to_vec(), None)?;
        match &outcome {
            ParseOutcome::Tree { .. } => {
                assert!(!outcome.is_usable());
                assert_eq!(render_tree_human(&outcome), "(no entries)\n");
            }
            ParseOutcome::Summary(_) => panic!("expected tree, got summary"),
        }
        Ok(())
    }
}
