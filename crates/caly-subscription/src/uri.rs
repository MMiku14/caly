//! Strict common proxy URI parsing into complete Domain nodes.

use core::num::NonZeroU16;

use caly_domain::{
    Credential, DialableNode, Endpoint, EndpointHost, NodeBuilder, NodeDisplayName, NodeSource,
    Protocol, ProtocolText, RealityConfig, SubscriptionId, TlsConfig, Transport, TransportText,
    TransportTextList, sanitized_display_name,
};
use percent_encoding::percent_decode_str;
use url::Url;

/// URI parse rejection; unsupported formats are never silently skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UriParseError {
    InvalidUrl,
    InvalidUtf8,
    MissingHost,
    MissingPort,
    MissingCredential,
    MissingParameter(&'static str),
    InvalidHost,
    InvalidName,
    InvalidText,
    InvalidSecret,
    UnsupportedScheme,
    Base64Rejected,
    JsonRejected,
    UnsupportedCipher,
    NodeRejected,
}

/// Dispatches special and URL-shaped proxy URI formats.
pub fn parse_any_proxy_uri(
    source: &str,
    subscription: SubscriptionId,
) -> Result<DialableNode, UriParseError> {
    if source.starts_with("vmess://") || source.starts_with("ss://") {
        super::special_uri::parse_special_uri(source, subscription)
    } else {
        parse_proxy_uri(source, subscription)
    }
}

/// Parses URL-shaped proxy schemes. VMess and SIP002 Shadowsocks use dedicated parsers.
pub fn parse_proxy_uri(
    source: &str,
    subscription: SubscriptionId,
) -> Result<DialableNode, UriParseError> {
    let url = Url::parse(source).map_err(|_| UriParseError::InvalidUrl)?;
    let endpoint = endpoint(&url)?;
    let protocol = protocol(&url)?;
    let name = display_name(&url)?;
    let mut builder = NodeBuilder::new(
        name,
        endpoint,
        protocol,
        NodeSource::Subscription(subscription),
    );
    if let Some(tls) = tls(&url)? {
        builder = builder.with_tls(tls);
    }
    if let Some(transport) = transport(&url)? {
        builder = builder.with_transport(transport);
    }
    builder.build().map_err(|_| UriParseError::NodeRejected)
}

/// Parses a URL-shaped transport from the `type`/`network` query parameter.
/// Supports ws, grpc, httpupgrade and quic; plain TCP when unset.
fn transport(url: &Url) -> Result<Option<Transport>, UriParseError> {
    let kind = query(url, "type").or_else(|| query(url, "network"));
    match kind.as_deref().unwrap_or("tcp") {
        "tcp" => Ok(Some(Transport::Tcp)),
        "ws" | "websocket" => Ok(Some(Transport::WebSocket {
            path: TransportText::new(query(url, "path").unwrap_or_else(|| "/".to_owned()))
                .map_err(|_| UriParseError::InvalidText)?,
            host: query(url, "host")
                .map(TransportText::new)
                .transpose()
                .map_err(|_| UriParseError::InvalidText)?,
            early_data: ws_early_data(url)?,
        })),
        "grpc" => Ok(Some(Transport::Grpc {
            service_name: TransportText::new(
                query(url, "serviceName")
                    .or_else(|| query(url, "service-name"))
                    .unwrap_or_else(|| "grpc".to_owned()),
            )
            .map_err(|_| UriParseError::InvalidText)?,
        })),
        "httpupgrade" => Ok(Some(Transport::Http2 {
            path: TransportText::new(query(url, "path").unwrap_or_else(|| "/".to_owned()))
                .map_err(|_| UriParseError::InvalidText)?,
            hosts: TransportTextList::try_from_vec(match query(url, "host") {
                // A malformed host must fail the URI, not silently drop the
                // whole transport field (never silently skip unsupported input).
                Some(host) => {
                    vec![TransportText::new(host).map_err(|_| UriParseError::InvalidText)?]
                }
                None => Vec::new(),
            })
            .map_err(|_| UriParseError::InvalidText)?,
        })),
        "quic" => Ok(Some(Transport::Quic)),
        _ => Err(UriParseError::InvalidText),
    }
}

fn endpoint(url: &Url) -> Result<Endpoint, UriParseError> {
    let host = url.host_str().ok_or(UriParseError::MissingHost)?;
    let host = EndpointHost::new(host.to_owned()).map_err(|_| UriParseError::InvalidHost)?;
    let port = url
        .port()
        .and_then(NonZeroU16::new)
        .ok_or(UriParseError::MissingPort)?;
    Ok(Endpoint::new(host, port))
}

fn protocol(url: &Url) -> Result<Protocol, UriParseError> {
    match url.scheme() {
        "vless" => Ok(Protocol::Vless {
            user_id: required_username(url)?,
            flow: query_text(url, "flow")?,
        }),
        "trojan" => Ok(Protocol::Trojan {
            password: required_username(url)?,
        }),
        "hysteria2" | "hy2" => Ok(Protocol::Hysteria2 {
            password: required_username(url)?,
            up_mbps: query_u32(url, "upmbps")?,
            down_mbps: query_u32(url, "downmbps")?,
            obfuscation: query_secret(url, "obfs-password")?,
        }),
        "tuic" => Ok(Protocol::Tuic {
            user_id: required_username(url)?,
            password: required_password(url)?,
            congestion: congestion(url)?,
        }),
        "wireguard" => Ok(Protocol::WireGuard {
            private_key: required_query_secret(url, "private-key")?,
            peer_public_key: required_query_secret(url, "public-key")?,
            reserved: None,
        }),
        "http" | "https" => Ok(Protocol::Http {
            username: optional_username(url)?,
            password: optional_password(url)?,
        }),
        "socks" | "socks5" => Ok(Protocol::Socks5 {
            username: optional_username(url)?,
            password: optional_password(url)?,
        }),
        "vmess" | "ss" => Err(UriParseError::UnsupportedScheme),
        _ => Err(UriParseError::UnsupportedScheme),
    }
}

fn display_name(url: &Url) -> Result<NodeDisplayName, UriParseError> {
    let value = match url.fragment() {
        Some(fragment) if !fragment.is_empty() => decode(fragment)?,
        _ => format!(
            "{}-{}",
            url.scheme(),
            url.host_str().ok_or(UriParseError::MissingHost)?
        ),
    };
    sanitized_display_name(value).map_err(|_| UriParseError::InvalidName)
}

fn tls(url: &Url) -> Result<Option<TlsConfig>, UriParseError> {
    let security = query(url, "security");
    let enabled = security
        .as_deref()
        .is_some_and(|value| value == "tls" || value == "reality")
        || url.scheme() == "https";
    if !enabled {
        return Ok(None);
    }
    let sni = query(url, "sni")
        .or_else(|| query(url, "peer"))
        .map(|value| TransportText::new(value).map_err(|_| UriParseError::InvalidText))
        .transpose()?;
    // Audit #64: accept the ecosystem's boolean spellings explicitly. Unknown
    // values parse to false (refusing to weaken certificate validation on a
    // typo is the safe direction), matching v2rayN's own leniency.
    let allow_insecure = query(url, "allowInsecure")
        .as_deref()
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"));
    // TLS client fingerprint (`fp`): sing-box's Reality handshake REQUIRES a
    // uTLS fingerprint — without it the client dials with the Go standard
    // library fingerprint and the server rejects the handshake (worse, this
    // sing-box release panics on the nil TLS config). v2rayN/Xray default to
    // `chrome`, which is why the same subscription works there but not here.
    let fingerprint = query(url, "fp")
        .map(|value| TransportText::new(value).map_err(|_| UriParseError::InvalidText))
        .transpose()?;
    // Reality handshake identity; without these the node cannot complete the
    // Reality handshake even though the protocol is vless/TLS.
    let reality = if security.as_deref() == Some("reality") {
        let public_key = reality_secret(url, "pbk")?;
        // `sid` (short id) is optional in Reality; many subscriptions omit it.
        let short_id = optional_reality_secret(url, "sid")?;
        let spider_x = query_text(url, "spx")?;
        Some(RealityConfig::new(public_key, short_id, spider_x))
    } else {
        None
    };
    // `alpn=h2,http/1.1` — comma-separated protocol list carried by many
    // v2rayN/Xray subscriptions; without it the domain model always emitted
    // an empty ALPN and the rendered node lost the negotiated protocol.
    let alpn = match query(url, "alpn") {
        Some(value) => parse_alpn_list(&value)?,
        None => TransportTextList::new(),
    };
    Ok(Some(TlsConfig::new(
        sni,
        alpn,
        allow_insecure,
        fingerprint,
        reality,
    )))
}

/// Parses Xray-style websocket early data (`ed=2048&eh=Sec-WebSocket-Protocol`).
/// `ed` is the byte budget — absent or 0 disables early data; `eh` names the
/// carrying header and defaults to the ecosystem-wide `Sec-WebSocket-Protocol`.
/// A present-but-non-numeric `ed` rejects the URI rather than silently
/// dropping the accelerator (audit #64: no unsupported input skipped quietly).
fn ws_early_data(url: &Url) -> Result<Option<caly_domain::WebSocketEarlyData>, UriParseError> {
    let Some(value) = query(url, "ed") else {
        return Ok(None);
    };
    let max_bytes: u32 = value.parse().map_err(|_| UriParseError::InvalidText)?;
    if max_bytes == 0 {
        return Ok(None);
    }
    let header = query(url, "eh").unwrap_or_else(|| "Sec-WebSocket-Protocol".to_owned());
    Ok(Some(caly_domain::WebSocketEarlyData::new(
        TransportText::new(header).map_err(|_| UriParseError::InvalidText)?,
        max_bytes,
    )))
}

/// Parses a comma-separated `alpn` query value (`h2,http/1.1`) into the
/// bounded text list; empty entries are dropped, over-long entries reject
/// the whole URI (never silently truncate negotiated protocols).
fn parse_alpn_list(value: &str) -> Result<TransportTextList, UriParseError> {
    let entries = value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| TransportText::new(entry.to_owned()).map_err(|_| UriParseError::InvalidText))
        .collect::<Result<Vec<_>, _>>()?;
    TransportTextList::try_from_vec(entries).map_err(|_| UriParseError::InvalidText)
}

fn required_username(url: &Url) -> Result<Credential, UriParseError> {
    optional_username(url)?.ok_or(UriParseError::MissingCredential)
}

fn required_password(url: &Url) -> Result<Credential, UriParseError> {
    optional_password(url)?.ok_or(UriParseError::MissingCredential)
}

fn optional_username(url: &Url) -> Result<Option<Credential>, UriParseError> {
    if url.username().is_empty() {
        return Ok(None);
    }
    Credential::new(decode(url.username())?)
        .map(Some)
        .map_err(|_| UriParseError::InvalidSecret)
}

fn optional_password(url: &Url) -> Result<Option<Credential>, UriParseError> {
    url.password()
        .map(|value| {
            decode(value)
                .and_then(|value| Credential::new(value).map_err(|_| UriParseError::InvalidSecret))
        })
        .transpose()
}

fn required_query_secret(url: &Url, key: &'static str) -> Result<Credential, UriParseError> {
    query_secret(url, key)?.ok_or(UriParseError::MissingParameter(key))
}

fn query_secret(url: &Url, key: &str) -> Result<Option<Credential>, UriParseError> {
    query(url, key)
        .map(|value| Credential::new(value).map_err(|_| UriParseError::InvalidSecret))
        .transpose()
}

/// Reads a Reality handshake secret (`pbk`/`sid`) into the bounded Reality
/// field, requiring a non-empty value (empty `sid` is still accepted).
fn reality_secret(url: &Url, key: &str) -> Result<caly_domain::SecretText<512>, UriParseError> {
    let value = query(url, key).ok_or(UriParseError::MissingParameter("reality-key"))?;
    caly_domain::SecretText::new(value).map_err(|_| UriParseError::InvalidSecret)
}

/// Reads an optional Reality secret (`sid`); an absent value yields `None` so a
/// Reality node without a short id still parses.
fn optional_reality_secret(
    url: &Url,
    key: &str,
) -> Result<Option<caly_domain::SecretText<512>>, UriParseError> {
    query(url, key)
        .map(|value| caly_domain::SecretText::new(value).map_err(|_| UriParseError::InvalidSecret))
        .transpose()
}

fn query_text(url: &Url, key: &str) -> Result<Option<ProtocolText>, UriParseError> {
    query(url, key)
        .map(|value| ProtocolText::new(value).map_err(|_| UriParseError::InvalidText))
        .transpose()
}

fn query_u32(url: &Url, key: &str) -> Result<Option<u32>, UriParseError> {
    query(url, key)
        .map(|value| value.parse().map_err(|_| UriParseError::InvalidText))
        .transpose()
}

fn congestion(url: &Url) -> Result<caly_domain::CongestionControl, UriParseError> {
    let value = query(url, "congestion_control").unwrap_or_else(|| "bbr".to_owned());
    match value.as_str() {
        "bbr" => Ok(caly_domain::CongestionControl::Bbr),
        "cubic" => Ok(caly_domain::CongestionControl::Cubic),
        "new_reno" | "new-reno" => Ok(caly_domain::CongestionControl::NewReno),
        _ => Err(UriParseError::InvalidText),
    }
}

fn query(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| std::borrow::Cow::into_owned(value))
}

fn decode(value: &str) -> Result<String, UriParseError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| UriParseError::InvalidText)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vless_reality_uri() -> &'static str {
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk&sid=afeed89ae23b36ed#reality-node"
    }

    fn vless_plain_uri() -> &'static str {
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&sni=example.com#plain-node"
    }

    #[test]
    fn reality_node_parses_and_has_distinct_identity() -> Result<(), String> {
        let subscription = SubscriptionId::from_bytes([1; 16]);
        let reality =
            parse_any_proxy_uri(vless_reality_uri(), subscription).map_err(|e| format!("{e:?}"))?;
        // A Reality handshake must not be silently dropped.
        assert!(
            reality.tls().is_some(),
            "reality node must carry TLS settings"
        );
        let plain =
            parse_any_proxy_uri(vless_plain_uri(), subscription).map_err(|e| format!("{e:?}"))?;
        // Reality is dial-affecting, so the canonical identity must differ from
        // the equivalent plain-TLS node; otherwise a Reality node would collide.
        assert_ne!(
            reality.id(),
            plain.id(),
            "reality identity must differ from plain TLS"
        );
        Ok(())
    }

    #[test]
    fn reality_missing_pbk_is_rejected() {
        let subscription = SubscriptionId::from_bytes([2; 16]);
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com";
        assert!(parse_any_proxy_uri(uri, subscription).is_err());
    }

    #[test]
    fn reality_without_sid_still_parses() -> Result<(), String> {
        // `sid` (short id) is optional; a Reality node without it must parse.
        let subscription = SubscriptionId::from_bytes([3; 16]);
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk#reality-no-sid";
        let node = parse_any_proxy_uri(uri, subscription).map_err(|e| format!("{e:?}"))?;
        assert!(node.tls().is_some(), "reality node must carry TLS");
        Ok(())
    }

    #[test]
    fn fingerprint_from_fp_param_is_preserved() -> Result<(), String> {
        // The `fp` query parameter must land in TlsConfig::fingerprint: it is
        // the uTLS label sing-box needs for a Reality handshake. Losing it is
        // the root cause of "works in v2rayN, dead in caly" for Reality nodes.
        let subscription = SubscriptionId::from_bytes([4; 16]);
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com&fp=chrome&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk";
        let node = parse_any_proxy_uri(uri, subscription).map_err(|e| format!("{e:?}"))?;
        let fp = node.tls().and_then(|tls| tls.fingerprint());
        assert!(
            fp.is_some_and(|fp| fp.as_str() == "chrome"),
            "fp=chrome must survive parsing, got {fp:?}"
        );
        Ok(())
    }

    #[test]
    fn plain_tls_without_fp_has_no_fingerprint() -> Result<(), String> {
        let subscription = SubscriptionId::from_bytes([5; 16]);
        let node =
            parse_any_proxy_uri(vless_plain_uri(), subscription).map_err(|e| format!("{e:?}"))?;
        assert!(
            node.tls().is_some_and(|tls| tls.fingerprint().is_none()),
            "no fp param means no fingerprint"
        );
        Ok(())
    }

    #[test]
    fn websocket_early_data_params_are_preserved() -> Result<(), String> {
        // Audit #64: Xray `ed`/`eh` must survive parsing; previously the URI
        // accepted but silently dropped them.
        let subscription = SubscriptionId::from_bytes([6; 16]);
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&type=ws&path=%2Fstream&ed=2048&eh=Sec-WebSocket-Protocol#ws-ed";
        let node = parse_any_proxy_uri(uri, subscription).map_err(|e| format!("{e:?}"))?;
        let Some(Transport::WebSocket { early_data, .. }) = node.transport() else {
            return Err("expected a websocket transport".to_owned());
        };
        let early = early_data
            .as_ref()
            .ok_or_else(|| "ed=2048 must keep early data".to_owned())?;
        assert_eq!(early.max_bytes(), 2048);
        assert_eq!(early.header_name().as_str(), "Sec-WebSocket-Protocol");
        Ok(())
    }

    #[test]
    fn websocket_ed_defaults_header_and_zero_disables() -> Result<(), String> {
        let subscription = SubscriptionId::from_bytes([7; 16]);
        // `ed` without `eh` defaults to the ecosystem header name.
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&type=ws&ed=1024#ws-ed-default";
        let node = parse_any_proxy_uri(uri, subscription).map_err(|e| format!("{e:?}"))?;
        let Some(Transport::WebSocket { early_data, .. }) = node.transport() else {
            return Err("expected a websocket transport".to_owned());
        };
        let early = early_data
            .as_ref()
            .ok_or_else(|| "ed=1024 must keep early data".to_owned())?;
        assert_eq!(early.header_name().as_str(), "Sec-WebSocket-Protocol");
        // `ed=0` means "disabled" in the Xray ecosystem.
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&type=ws&ed=0#ws-ed-zero";
        let node = parse_any_proxy_uri(uri, subscription).map_err(|e| format!("{e:?}"))?;
        let Some(Transport::WebSocket { early_data, .. }) = node.transport() else {
            return Err("expected a websocket transport".to_owned());
        };
        assert!(
            early_data.is_none(),
            "ed=0 must disable early data entirely"
        );
        Ok(())
    }

    #[test]
    fn websocket_non_numeric_ed_is_rejected() {
        let subscription = SubscriptionId::from_bytes([8; 16]);
        let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&type=ws&ed=lots#ws-ed-bad";
        assert!(
            parse_any_proxy_uri(uri, subscription).is_err(),
            "a non-numeric ed must reject the URI, not silently drop the field"
        );
    }

    #[test]
    fn allow_insecure_accepts_explicit_boolean_spellings() -> Result<(), String> {
        let subscription = SubscriptionId::from_bytes([9; 16]);
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("0", false),
            ("false", false),
            ("bogus", false),
        ] {
            let uri = format!(
                "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&allowInsecure={value}#insecure-{value}"
            );
            let node = parse_any_proxy_uri(&uri, subscription).map_err(|e| format!("{e:?}"))?;
            let actual = node
                .tls()
                .ok_or_else(|| "TLS must be present".to_owned())?
                .allow_insecure();
            assert_eq!(actual, expected, "allowInsecure={value}");
        }
        Ok(())
    }
}
