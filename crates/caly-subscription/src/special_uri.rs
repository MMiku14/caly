//! VMess Base64 JSON and SIP002 Shadowsocks parsing.

use core::num::NonZeroU16;

use base64::{Engine as _, engine::general_purpose};
use caly_domain::{
    Credential, DialableNode, Endpoint, EndpointHost, NodeBuilder, NodeDisplayName, NodeSource,
    Protocol, ShadowsocksCipher, ShadowsocksPlugin, SubscriptionId, TlsConfig, Transport,
    TransportText, TransportTextList, VmessCipher, sanitized_display_name,
};
use serde_json::Value;
use url::Url;

use percent_encoding::percent_decode_str;

use super::UriParseError;

pub fn parse_special_uri(
    source: &str,
    subscription: SubscriptionId,
) -> Result<DialableNode, UriParseError> {
    if let Some(payload) = source.strip_prefix("vmess://") {
        parse_vmess(payload, subscription)
    } else if let Some(payload) = source.strip_prefix("ss://") {
        parse_shadowsocks(payload, subscription)
    } else {
        Err(UriParseError::UnsupportedScheme)
    }
}

fn parse_vmess(payload: &str, subscription: SubscriptionId) -> Result<DialableNode, UriParseError> {
    let decoded = decode_base64_text(payload)?;
    let value: Value = serde_json::from_str(&decoded).map_err(|_| UriParseError::JsonRejected)?;
    let host = json_text(&value, "add")?;
    let port = json_port(&value)?;
    let user_id =
        Credential::new(json_text(&value, "id")?).map_err(|_| UriParseError::InvalidSecret)?;
    let alter_id = json_u16_optional(&value, "aid")?.unwrap_or(0);
    let security = vmess_cipher(json_optional_text(&value, "scy").as_deref())?;
    let protocol = Protocol::Vmess {
        user_id,
        alter_id,
        security,
    };
    let endpoint = endpoint(host, port)?;
    let name = node_name(json_optional_text(&value, "ps"), "vmess")?;
    let mut builder = NodeBuilder::new(
        name,
        endpoint,
        protocol,
        NodeSource::Subscription(subscription),
    );
    if let Some(transport) = vmess_transport(&value)? {
        builder = builder.with_transport(transport);
    }
    if vmess_tls_enabled(&value) {
        builder = builder.with_tls(vmess_tls(&value)?);
    }
    builder.build().map_err(|_| UriParseError::NodeRejected)
}

fn parse_shadowsocks(
    payload: &str,
    subscription: SubscriptionId,
) -> Result<DialableNode, UriParseError> {
    let (without_fragment, fragment) = payload
        .split_once('#')
        .map_or((payload, None), |(body, name)| {
            (body, Some(name.to_owned()))
        });
    let decoded = if without_fragment.contains('@') {
        without_fragment.to_owned()
    } else {
        decode_base64_text(without_fragment)?
    };
    let (credential, address) = decoded.rsplit_once('@').ok_or(UriParseError::InvalidUrl)?;
    // Some sources URL-encode the whole userinfo (`%2B` etc.) before
    // base64; try the raw base64 first, then percent-decode and retry
    // before falling back to the plaintext credential.
    let credential = decode_base64_text(credential)
        .or_else(|_| {
            percent_decode_str(credential)
                .decode_utf8()
                .map(std::borrow::Cow::into_owned)
                .map_err(|_| UriParseError::InvalidUrl)
                .and_then(|decoded| decode_base64_text(&decoded))
        })
        .unwrap_or_else(|_| credential.to_owned());
    let (method, password) = credential
        .split_once(':')
        .ok_or(UriParseError::MissingCredential)?;
    // The password itself may carry percent-encoded bytes (`%2B` for
    // `+` is common in airport feeds); leaving them verbatim renders a
    // node that can never dial.
    let password = percent_decode_str(password)
        .decode_utf8()
        .map_or_else(|_| password.to_owned(), std::borrow::Cow::into_owned);
    let url = Url::parse(&format!("ss://placeholder@{address}"))
        .map_err(|_| UriParseError::InvalidUrl)?;
    let host = url.host_str().ok_or(UriParseError::MissingHost)?.to_owned();
    let port = url.port().ok_or(UriParseError::MissingPort)?;
    let endpoint = endpoint(host, port)?;
    // The `?plugin=` parameter (obfs-local / v2ray-plugin) is a real
    // transport requirement, not decoration: dropping it would render a
    // node that can never dial (the plugin carries the actual relay
    // mode). Parse it into the protocol so the renderers can reject the
    // variant explicitly instead of silently emitting a dead outbound.
    let plugin = match url.query_pairs().find(|(key, _)| key == "plugin") {
        Some((_, value)) => {
            let value = value.as_ref();
            let (name, options) = value.split_once(';').unwrap_or((value, ""));
            Some(ShadowsocksPlugin::new(
                caly_domain::ProtocolText::new(name.to_owned())
                    .map_err(|_| UriParseError::InvalidUrl)?,
                caly_domain::ProtocolText::new(options.to_owned())
                    .map_err(|_| UriParseError::InvalidUrl)?,
            ))
        }
        None => None,
    };
    let protocol = Protocol::Shadowsocks {
        method: shadowsocks_cipher(method)?,
        password: Credential::new(password.clone()).map_err(|_| UriParseError::InvalidSecret)?,
        plugin,
    };
    let name = node_name(fragment, "shadowsocks")?;
    NodeBuilder::new(
        name,
        endpoint,
        protocol,
        NodeSource::Subscription(subscription),
    )
    .build()
    .map_err(|_| UriParseError::NodeRejected)
}

fn endpoint(host: String, port: u16) -> Result<Endpoint, UriParseError> {
    let host = EndpointHost::new(host).map_err(|_| UriParseError::InvalidHost)?;
    let port = NonZeroU16::new(port).ok_or(UriParseError::MissingPort)?;
    Ok(Endpoint::new(host, port))
}

fn node_name(value: Option<String>, fallback: &str) -> Result<NodeDisplayName, UriParseError> {
    let raw = value
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_owned());
    // URL-shaped schemes percent-decode their fragment names; VMess/SS names
    // come in as raw fragments or JSON payloads, so decode them the same way
    // to keep the display name, registry tag and kernel tag identical.
    let decoded = match percent_decode_str(&raw).decode_utf8() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => raw,
    };
    sanitized_display_name(decoded).map_err(|_| UriParseError::InvalidName)
}

fn decode_base64_text(value: &str) -> Result<String, UriParseError> {
    let value = value.trim();
    for engine in [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(bytes) = engine.decode(value) {
            return String::from_utf8(bytes).map_err(|_| UriParseError::InvalidUtf8);
        }
    }
    Err(UriParseError::Base64Rejected)
}

fn json_text(value: &Value, key: &'static str) -> Result<String, UriParseError> {
    json_optional_text(value, key).ok_or(UriParseError::MissingParameter(key))
}

/// Reads an optional string/number field; empty strings count as absent, since
/// real-world VMess payloads routinely carry `"host":""`/`"path":""` placeholders.
fn json_optional_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|field| match field {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn json_port(value: &Value) -> Result<u16, UriParseError> {
    json_optional_text(value, "port")
        .ok_or(UriParseError::MissingPort)?
        .parse()
        .map_err(|_| UriParseError::MissingPort)
}

fn json_u16_optional(value: &Value, key: &str) -> Result<Option<u16>, UriParseError> {
    json_optional_text(value, key)
        .map(|value| value.parse().map_err(|_| UriParseError::InvalidText))
        .transpose()
}

fn vmess_cipher(value: Option<&str>) -> Result<VmessCipher, UriParseError> {
    match value.unwrap_or("auto") {
        "auto" => Ok(VmessCipher::Auto),
        "aes-128-gcm" => Ok(VmessCipher::Aes128Gcm),
        "chacha20-poly1305" => Ok(VmessCipher::Chacha20Poly1305),
        "none" | "zero" => Ok(VmessCipher::None),
        _ => Err(UriParseError::UnsupportedCipher),
    }
}

pub(crate) fn shadowsocks_cipher(value: &str) -> Result<ShadowsocksCipher, UriParseError> {
    match value {
        "aes-128-gcm" => Ok(ShadowsocksCipher::Aes128Gcm),
        "aes-256-gcm" => Ok(ShadowsocksCipher::Aes256Gcm),
        "chacha20-ietf-poly1305" => Ok(ShadowsocksCipher::Chacha20IetfPoly1305),
        "xchacha20-ietf-poly1305" => Ok(ShadowsocksCipher::Xchacha20IetfPoly1305),
        "aes-128-cfb" => Ok(ShadowsocksCipher::Aes128Cfb),
        "aes-256-cfb" => Ok(ShadowsocksCipher::Aes256Cfb),
        "none" => Ok(ShadowsocksCipher::None),
        _ => Err(UriParseError::UnsupportedCipher),
    }
}

fn vmess_transport(value: &Value) -> Result<Option<Transport>, UriParseError> {
    match json_optional_text(value, "net").as_deref().unwrap_or("tcp") {
        "tcp" => Ok(Some(Transport::Tcp)),
        "ws" => Ok(Some(Transport::WebSocket {
            path: TransportText::new(
                json_optional_text(value, "path").unwrap_or_else(|| "/".to_owned()),
            )
            .map_err(|_| UriParseError::InvalidText)?,
            host: json_optional_text(value, "host")
                .map(TransportText::new)
                .transpose()
                .map_err(|_| UriParseError::InvalidText)?,
            // VMess share links do not carry Xray early-data parameters.
            early_data: None,
        })),
        "grpc" => Ok(Some(Transport::Grpc {
            service_name: TransportText::new(
                json_optional_text(value, "path").unwrap_or_else(|| "grpc".to_owned()),
            )
            .map_err(|_| UriParseError::InvalidText)?,
        })),
        // VMess `net: http`/`h2` is HTTP/2 framing; the comma-separated `host`
        // field carries the authority list.
        "http" | "h2" => {
            let path = TransportText::new(
                json_optional_text(value, "path").unwrap_or_else(|| "/".to_owned()),
            )
            .map_err(|_| UriParseError::InvalidText)?;
            let mut hosts = TransportTextList::new();
            if let Some(host) = json_optional_text(value, "host") {
                for part in host
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                {
                    let label = TransportText::new(part.to_owned())
                        .map_err(|_| UriParseError::InvalidText)?;
                    hosts
                        .try_push(label)
                        .map_err(|_| UriParseError::InvalidText)?;
                }
            }
            Ok(Some(Transport::Http2 { path, hosts }))
        }
        _ => Err(UriParseError::InvalidText),
    }
}

fn vmess_tls_enabled(value: &Value) -> bool {
    matches!(
        json_optional_text(value, "tls").as_deref(),
        Some("tls" | "reality")
    )
}

fn vmess_tls(value: &Value) -> Result<TlsConfig, UriParseError> {
    let sni = json_optional_text(value, "sni")
        .or_else(|| json_optional_text(value, "host"))
        .map(TransportText::new)
        .transpose()
        .map_err(|_| UriParseError::InvalidText)?;
    // VMess JSON carries ALPN as a comma-separated `alpn` field as well
    // (`"alpn": "h2,http/1.1"`); mirror the URL-shaped parser.
    let mut alpn = TransportTextList::new();
    if let Some(raw) = json_optional_text(value, "alpn") {
        for part in raw
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let label =
                TransportText::new(part.to_owned()).map_err(|_| UriParseError::InvalidText)?;
            alpn.try_push(label)
                .map_err(|_| UriParseError::InvalidText)?;
        }
    }
    Ok(TlsConfig::new(sni, alpn, false, None, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_any_proxy_uri;

    fn sub() -> SubscriptionId {
        SubscriptionId::from_bytes([0; 16])
    }

    fn vmess_uri(json: &str) -> String {
        use base64::Engine as _;
        format!(
            "vmess://{}",
            base64::engine::general_purpose::STANDARD.encode(json.as_bytes())
        )
    }

    #[test]
    fn vmess_with_empty_host_placeholder_parses() -> Result<(), String> {
        // Real-world payloads carry `"host":""`; the empty placeholder must be
        // treated as absent instead of rejecting the node.
        let uri = vmess_uri(
            r#"{"add":"203.0.113.5","aid":"0","host":"","id":"00000000-0000-0000-0000-000000000001","net":"tcp","path":"","port":"443","ps":"node","tls":"tls","v":"2"}"#,
        );
        let node = parse_any_proxy_uri(&uri, sub())
            .map_err(|error| format!("vmess parse failed: {error:?}"))?;
        assert_eq!(node.endpoint().host().as_str(), "203.0.113.5");
        assert!(node.tls().is_some());
        Ok(())
    }

    #[test]
    fn shadowsocks_fragment_name_is_percent_decoded() -> Result<(), String> {
        // SIP002 names arrive percent-encoded (`#%E7%BE%8E%E5%9C%8B...`); the
        // display name must be decoded exactly like URL-shaped schemes so the
        // registry tag, kernel tag and list output stay consistent.
        let ss_uri = "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQ@203.0.113.9:8388#%E7%BE%8E%E5%9C%8B%E8%8A%82%E7%82%B9";
        let node = parse_any_proxy_uri(ss_uri, sub())
            .map_err(|error| format!("ss parse failed: {error:?}"))?;
        let display = node
            .display(true, None)
            .map_err(|error| format!("display projects: {error:?}"))?;
        assert_eq!(
            display.name().as_str(),
            "美國节点",
            "ss fragment name must be percent-decoded"
        );
        Ok(())
    }
}
