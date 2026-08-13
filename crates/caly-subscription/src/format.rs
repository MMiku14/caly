//! Bounded subscription format detection and document decoding.

use base64::{engine::general_purpose, Engine as _};
use caly_domain::{BoundedText, BoundedVec};

/// Subscription body and decoded aggregate ceilings.
pub const MAX_SUBSCRIPTION_BODY_BYTES: usize = 32 * 1_024 * 1_024;
pub const MAX_SUBSCRIPTION_LINES: usize = 10_000;
pub const MAX_SUBSCRIPTION_LINE_BYTES: usize = 8 * 1_024;

pub type SubscriptionBody = BoundedVec<u8, MAX_SUBSCRIPTION_BODY_BYTES>;
pub type SubscriptionLine = BoundedText<MAX_SUBSCRIPTION_LINE_BYTES>;
pub type SubscriptionLines = BoundedVec<SubscriptionLine, MAX_SUBSCRIPTION_LINES>;

/// Supported top-level subscription documents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionFormat {
    ClashYaml,
    UriLines,
    Base64UriLines,
}

/// Decoded document without yet constructing dialable nodes.
pub enum SubscriptionDocument {
    ClashYaml(SubscriptionBody),
    UriLines {
        source_format: SubscriptionFormat,
        lines: SubscriptionLines,
    },
    /// Plain-text subscription whose lines are themselves subscription URLs;
    /// the daemon fetches and merges each child before node parsing.
    UrlList(SubscriptionLines),
    /// SIP008 JSON document: an array of Shadowsocks server objects, or a
    /// `{"version":1,"servers":[...]}` envelope.
    Sip008(SubscriptionBody),
}

/// Detection/decode failure; no empty-success fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    Empty,
    BodyTooLarge,
    InvalidUtf8,
    UnknownFormat,
    Base64Rejected,
    DecodedBodyTooLarge,
    TooManyLines,
    LineTooLong,
}

/// Detects Clash YAML, direct URI lines, or a Base64 URI aggregate.
pub fn decode_document(body: Vec<u8>) -> Result<SubscriptionDocument, FormatError> {
    let bounded = SubscriptionBody::try_from_vec(body).map_err(|_| FormatError::BodyTooLarge)?;
    if bounded.is_empty() {
        return Err(FormatError::Empty);
    }
    let source = core::str::from_utf8(bounded.as_slice()).map_err(|_| FormatError::InvalidUtf8)?;
    if is_clash_yaml(source) {
        return Ok(SubscriptionDocument::ClashYaml(bounded));
    }
    if is_sip008_json(source) {
        return Ok(SubscriptionDocument::Sip008(bounded));
    }
    if is_url_list(source) {
        return Ok(SubscriptionDocument::UrlList(parse_lines(source)?));
    }
    if contains_supported_uri(source) {
        return Ok(SubscriptionDocument::UriLines {
            source_format: SubscriptionFormat::UriLines,
            lines: parse_lines(source)?,
        });
    }
    let decoded = decode_base64_aggregate(source)?;
    let decoded_text = core::str::from_utf8(&decoded).map_err(|_| FormatError::InvalidUtf8)?;
    // Base64 aggregates may carry a Clash YAML document or a SIP008 JSON
    // payload, not only URI lines.
    if is_clash_yaml(decoded_text) {
        let body = SubscriptionBody::try_from_vec(decoded)
            .map_err(|_| FormatError::DecodedBodyTooLarge)?;
        return Ok(SubscriptionDocument::ClashYaml(body));
    }
    if is_sip008_json(decoded_text) {
        let body = SubscriptionBody::try_from_vec(decoded)
            .map_err(|_| FormatError::DecodedBodyTooLarge)?;
        return Ok(SubscriptionDocument::Sip008(body));
    }
    if !contains_supported_uri(decoded_text) {
        return Err(FormatError::UnknownFormat);
    }
    Ok(SubscriptionDocument::UriLines {
        source_format: SubscriptionFormat::Base64UriLines,
        lines: parse_lines(decoded_text)?,
    })
}

/// Cheap line-level sniff for a Clash document (audit #118): a top-level
/// (unindented) `proxies:` mapping key. YAML requires the colon of a mapping
/// key to be followed by end-of-line or whitespace, which this mirrors. The
/// dedicated Clash parser downstream stays the semantic authority — a sniffed
/// match whose YAML turns out invalid fails there rather than being
/// misrouted. This replaces the previous full-document parse, which built an
/// entire value tree of up to `MAX_SUBSCRIPTION_BODY_BYTES` purely for
/// detection and then parsed it a second time downstream.
fn is_clash_yaml(source: &str) -> bool {
    source.lines().any(|line| {
        !line.starts_with(char::is_whitespace)
            && line
                .strip_prefix("proxies:")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    })
}

/// Cheap sniff for a SIP008 payload (audit #118), replacing the previous
/// full-document parse for the same reason as [`is_clash_yaml`]. A JSON
/// document starts with `{`/`[` and carries quoted `"server"` plus
/// `"method"`/`"password"` keys. A YAML-shaped SIP008 body (historically
/// accepted through the YAML-superset parser) exposes either a top-level
/// `servers:` envelope key or a top-level sequence whose entries carry
/// `server:` plus `method:`/`password:` fields. The dedicated SIP008 parser
/// downstream re-validates every entry, so this sniff only decides routing.
fn is_sip008_json(source: &str) -> bool {
    let trimmed = source.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return trimmed.contains("\"server\"")
            && (trimmed.contains("\"method\"") || trimmed.contains("\"password\""));
    }
    let mut envelope = false;
    let sequence = trimmed.starts_with('-');
    let mut server_field = false;
    let mut credential_field = false;
    for line in trimmed.lines() {
        let body = line.trim_start();
        if !line.starts_with(char::is_whitespace)
            && body
                .strip_prefix("servers:")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            envelope = true;
            continue;
        }
        // Strip leading sequence dashes so `- server: …` entry lines count.
        let mut fields = body;
        while let Some(rest) = fields.strip_prefix("- ") {
            fields = rest.trim_start();
        }
        server_field |= fields.starts_with("server:");
        credential_field |= fields.starts_with("method:") || fields.starts_with("password:");
    }
    (envelope || sequence) && server_field && credential_field
}

/// Detects a plain-text URL-list subscription: every significant line is an
/// HTTP(S) URL and no line carries a fragment (real proxy node URIs use `#`
/// for their display name, subscription URLs do not).
fn is_url_list(source: &str) -> bool {
    let mut any = false;
    for line in source.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let is_url =
            (line.starts_with("http://") || line.starts_with("https://")) && !line.contains('#');
        if !is_url {
            return false;
        }
        any = true;
    }
    any
}

fn contains_supported_uri(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim();
        supported_scheme(line.split_once("://").map(|(scheme, _)| scheme))
    })
}

fn supported_scheme(scheme: Option<&str>) -> bool {
    matches!(
        scheme,
        Some(
            "ss" | "ssr"
                | "vmess"
                | "vless"
                | "trojan"
                | "hysteria2"
                | "hy2"
                | "tuic"
                | "wireguard"
                | "http"
                | "https"
                | "socks"
                | "socks5"
        )
    )
}

/// Tries every common base64 alphabet in order (STANDARD, STANDARD_NO_PAD,
/// URL_SAFE, URL_SAFE_NO_PAD) and returns the first successful decode.
pub(crate) fn decode_base64_any(value: &[u8]) -> Option<Vec<u8>> {
    for engine in [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(value) {
            return Some(decoded);
        }
    }
    None
}

fn decode_base64_aggregate(source: &str) -> Result<Vec<u8>, FormatError> {
    // Comment/banner lines (`# 收集整理测试: ...` is ubiquitous in
    // airport feeds) are not base64; filtering them mirrors the
    // per-line parse path so an ad header cannot reject the whole
    // document (2026-08-12 agent audit).
    let compact: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(|line| line.chars())
        .filter(|value| !value.is_ascii_whitespace())
        .collect();
    let estimated = compact.len().saturating_mul(3).saturating_div(4);
    if estimated > MAX_SUBSCRIPTION_BODY_BYTES {
        return Err(FormatError::DecodedBodyTooLarge);
    }
    match decode_base64_any(compact.as_bytes()) {
        Some(decoded) if decoded.len() <= MAX_SUBSCRIPTION_BODY_BYTES => Ok(decoded),
        Some(_) => Err(FormatError::DecodedBodyTooLarge),
        None => Err(FormatError::Base64Rejected),
    }
}

fn parse_lines(source: &str) -> Result<SubscriptionLines, FormatError> {
    let mut lines = SubscriptionLines::new();
    for line in source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.starts_with('#') {
            continue;
        }
        let value = SubscriptionLine::new(line.to_owned()).map_err(|_| FormatError::LineTooLong)?;
        lines
            .try_push(value)
            .map_err(|_| FormatError::TooManyLines)?;
    }
    if lines.is_empty() {
        return Err(FormatError::Empty);
    }
    Ok(lines)
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    #[test]
    fn node_singbox_fixture_decodes_as_base64_uri_lines() {
        let body = include_bytes!("../../../fixtures/subscription-20260803.txt").to_vec();
        let result = decode_document(body);
        assert!(matches!(result, Ok(SubscriptionDocument::UriLines {
            source_format: SubscriptionFormat::Base64UriLines,
            lines,
        }) if !lines.is_empty()));
    }

    fn b64(text: &str) -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(text.as_bytes())
            .into_bytes()
    }

    #[test]
    fn base64_encoded_clash_yaml_decodes_to_clash_document() {
        let yaml = "proxies:\n  - name: n\n    type: ss\n    server: s\n    port: 1\n    cipher: aes-128-gcm\n    password: p\n";
        let result = decode_document(b64(yaml));
        assert!(matches!(result, Ok(SubscriptionDocument::ClashYaml(_))));
    }

    #[test]
    fn sip008_array_and_envelope_are_detected() {
        let array = r#"[{"server":"s","server_port":1,"method":"aes-128-gcm","password":"p"}]"#;
        assert!(matches!(
            decode_document(array.as_bytes().to_vec()),
            Ok(SubscriptionDocument::Sip008(_))
        ));
        let envelope = r#"{"version":1,"servers":[{"server":"s","server_port":1,"method":"aes-128-gcm","password":"p"}]}"#;
        assert!(matches!(
            decode_document(envelope.as_bytes().to_vec()),
            Ok(SubscriptionDocument::Sip008(_))
        ));
        // Base64-encoded SIP008 is detected after decoding.
        assert!(matches!(
            decode_document(b64(envelope)),
            Ok(SubscriptionDocument::Sip008(_))
        ));
    }

    #[test]
    fn ssr_lines_are_recognized_as_uri_line_documents() {
        let body = "ssr://c3MtcGxhY2Vob2xkZXI\n";
        assert!(matches!(
            decode_document(body.as_bytes().to_vec()),
            Ok(SubscriptionDocument::UriLines { .. })
        ));
    }

    #[test]
    fn yaml_shaped_sip008_envelope_is_detected_without_full_parse() {
        let yaml = "version: 1\nservers:\n  - server: s\n    server_port: 1\n    method: aes-128-gcm\n    password: p\n";
        assert!(matches!(
            decode_document(yaml.as_bytes().to_vec()),
            Ok(SubscriptionDocument::Sip008(_))
        ));
    }

    #[test]
    fn clash_document_with_flow_style_proxies_is_detected() {
        assert!(matches!(
            decode_document(b"proxies: []\n".to_vec()),
            Ok(SubscriptionDocument::ClashYaml(_))
        ));
    }

    #[test]
    fn url_list_with_server_like_hosts_is_not_misdetected_as_sip008() {
        let body = "https://server1.example.com/sub\nhttps://method.example.org/list\n";
        assert!(matches!(
            decode_document(body.as_bytes().to_vec()),
            Ok(SubscriptionDocument::UrlList(_))
        ));
    }
}
