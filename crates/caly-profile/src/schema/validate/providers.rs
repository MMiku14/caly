//! Validation for the `providers:` section (proxy content providers).

use std::collections::HashSet;

use crate::schema::settings::ProviderKind;
use crate::schema::{AppConfig, ConfigError};

/// Inline node URIs must look like proxy URIs. `vmess`/`ss`/`ssr` carry a
/// base64 payload: the payload must be base64-shaped (correct alphabet and
/// length), where `ss`/`ssr` payloads end at the first `@` (userinfo
/// split). Other schemes just need a non-empty rest. This mirrors the
/// historical validator (2026-08-13 restore — the validator was dropped
/// during the CLI provider-domain merge and is required by
/// `inline_nodes_provider_parses`).
fn is_valid_node_uri(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    if !matches!(
        scheme,
        "vmess"
            | "ss"
            | "ssr"
            | "trojan"
            | "vless"
            | "hysteria2"
            | "socks"
            | "socks5"
            | "http"
            | "https"
    ) {
        return false;
    }
    if matches!(scheme, "vmess" | "ss" | "ssr") {
        // `ss://`/`ssr://` split userinfo at the first `@`; `vmess://` is
        // one pure base64 document.
        let payload = rest.split('@').next().unwrap_or(rest);
        if payload.is_empty() || payload.len() % 4 != 0 {
            return false;
        }
        if !payload
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        {
            return false;
        }
    }
    true
}

/// Validates the `providers:` section: non-empty unique names, and inline
/// node lists that are non-empty with well-formed URIs.
pub fn validate_providers(config: &AppConfig) -> Result<(), ConfigError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for provider in &config.providers {
        if provider.name.trim().is_empty() {
            return Err(ConfigError::InvalidProvider {
                name: provider.name.clone(),
                reason: "provider name must not be empty".to_owned(),
            });
        }
        if !seen.insert(provider.name.as_str()) {
            return Err(ConfigError::InvalidProvider {
                name: provider.name.clone(),
                reason: "duplicate provider name".to_owned(),
            });
        }
        if let ProviderKind::InlineNodes(nodes) = &provider.kind {
            if nodes.is_empty() {
                return Err(ConfigError::InvalidProvider {
                    name: provider.name.clone(),
                    reason: "inline provider must declare at least one node".to_owned(),
                });
            }
            for node in nodes {
                if !is_valid_node_uri(node) {
                    return Err(ConfigError::InvalidProvider {
                        name: provider.name.clone(),
                        reason: format!("malformed node URI `{node}`"),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_valid_node_uri;

    #[test]
    fn vmess_payload_must_be_base64_shaped() {
        assert!(is_valid_node_uri(
            "vmess://eyJhZGRyIjoiMS4yLjMuNCIsInBvcnQiOiI4NDQzIiwiYWlkIjoiMCIsImlkIjoiYQ=="
        ));
        assert!(
            !is_valid_node_uri("vmess://abc"),
            "short non-base64 payload"
        );
        assert!(!is_valid_node_uri("vmess://"), "empty payload");
    }

    #[test]
    fn ss_payload_splits_at_userinfo() {
        assert!(is_valid_node_uri(
            "ss://Y2hhY2hhMjAtaWV0Zi1wb2x5MTMwNTpzZWNyZXRwYXNz@example.com:8443#ss-node"
        ));
        assert!(!is_valid_node_uri("ss://not-base64@example.com:8443"));
    }

    #[test]
    fn unknown_schemes_are_rejected() {
        assert!(!is_valid_node_uri("gopher://example.com"));
        assert!(!is_valid_node_uri("example.com:8388"));
    }
}
