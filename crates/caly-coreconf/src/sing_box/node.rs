//! sing-box node outbound rendering: protocol fields, TLS, reality, transport.
//! Typed-model form (P3b): each protocol outbound is a serde struct; field
//! declaration order is alphabetical, matching the byte order the pre-typed
//! `serde_json::Value` (BTreeMap) assembly produced, so re-serialization is
//! byte-identical for every representable node.
//!
//! Kept separate from `document.rs` (document assembly) so each file stays
//! within the project's line limits.

use caly_domain::{DialableNode, Protocol, ShadowsocksCipher, VmessCipher};
use serde::Serialize;
use serde_json::Value;

use super::SingBoxOutboundError;

pub fn node_to_json(node: &DialableNode) -> Result<Value, SingBoxOutboundError> {
    let outbound = node_outbound(node)?;
    serde_json::to_value(&outbound).map_err(|_| SingBoxOutboundError::Serialization)
}

/// Renders one node as a compact JSON object string (tag `proxy-<id>`), the
/// per-node form stored by the shared registry so config backends can rebuild
/// the outbounds array without re-parsing the subscription body.
pub fn node_to_json_string(node: &DialableNode) -> Result<String, SingBoxOutboundError> {
    serde_json::to_string(&node_outbound(node)?).map_err(|_| SingBoxOutboundError::Serialization)
}

/// Builds the typed outbound for one dialable node.
fn node_outbound(node: &DialableNode) -> Result<NodeOutbound, SingBoxOutboundError> {
    let id_text = caly_domain::to_hex(node.id().into_bytes());
    let tag = format!("proxy-{id_text}");
    let server = node.endpoint().host().as_str().to_owned();
    let server_port = node.endpoint().port().get();
    match node.protocol() {
        Protocol::Vless { user_id, flow } => {
            let outbound = VlessOutbound {
                flow: flow.as_ref().map(|flow| flow.as_str().to_owned()),
                server,
                server_port,
                tag,
                tls: tls_json(node),
                transport: transport_json(node),
                kind: "vless",
                uuid: user_id.with_exposed(str::to_owned),
            };
            Ok(NodeOutbound::Vless(Box::new(outbound)))
        }
        Protocol::Trojan { password } => Ok(NodeOutbound::Trojan(TrojanOutbound {
            password: password.with_exposed(str::to_owned),
            server,
            server_port,
            tag,
            tls: tls_json(node),
            transport: transport_json(node),
            kind: "trojan",
        })),
        Protocol::Vmess {
            user_id,
            alter_id,
            security,
        } => Ok(NodeOutbound::Vmess(Box::new(VmessOutbound {
            alter_id: *alter_id,
            security: vmess_cipher(*security),
            server,
            server_port,
            tag,
            tls: tls_json(node),
            transport: transport_json(node),
            kind: "vmess",
            uuid: user_id.with_exposed(str::to_owned),
        }))),
        Protocol::Shadowsocks {
            password,
            method,
            plugin,
        } => node_outbound_ss(node, *method, password, plugin.as_ref()),
        Protocol::Hysteria2 {
            password,
            up_mbps,
            down_mbps,
            obfuscation,
        } => Ok(NodeOutbound::Hysteria2(Box::new(Hysteria2Outbound {
            down_mbps: *down_mbps,
            obfs: obfuscation.as_ref().map(|obfs| ObfsJson {
                password: obfs.with_exposed(str::to_owned),
                kind: "salamander",
            }),
            password: password.with_exposed(str::to_owned),
            server,
            server_port,
            tag,
            // hysteria2 is a TLS-only transport; sing-box rejects an outbound
            // without a `tls` block even when the URI carries no explicit SNI.
            tls: tls_or_host_fallback(node),
            kind: "hysteria2",
            up_mbps: *up_mbps,
        }))),
        Protocol::Tuic {
            user_id,
            password,
            congestion,
        } => Ok(NodeOutbound::Tuic(Box::new(TuicOutbound {
            congestion_control: congestion_label(*congestion),
            password: password.with_exposed(str::to_owned),
            server,
            server_port,
            tag,
            // tuic is TLS-only for the same reason as hysteria2.
            tls: tls_or_host_fallback(node),
            kind: "tuic",
            uuid: user_id.with_exposed(str::to_owned),
        }))),
        // Plain HTTP / socks5 proxies are first-class sing-box outbounds.
        Protocol::Http { username, password } => Ok(node_outbound_http(
            node,
            "http",
            username.as_ref(),
            password.as_ref(),
        )),
        Protocol::Socks5 { username, password } => Ok(node_outbound_http(
            node,
            "socks",
            username.as_ref(),
            password.as_ref(),
        )),
        // ShadowTls is a shadowsocks *transport* wrapper, not a dialable
        // outbound shape on its own; AnyTls has no sing-box outbound.
        Protocol::ShadowTls { .. } | Protocol::AnyTls { .. } => {
            Err(SingBoxOutboundError::UnsupportedNode)
        }
        Protocol::WireGuard { .. } => Err(SingBoxOutboundError::UnsupportedNode),
    }
}

/// http / socks branch of [`node_outbound`] (extracted for the
/// 100-line budget): plain proxy outbounds with optional credentials.
fn node_outbound_http(
    node: &DialableNode,
    kind: &'static str,
    username: Option<&caly_domain::Credential>,
    password: Option<&caly_domain::Credential>,
) -> NodeOutbound {
    let server = node.endpoint().host().as_str().to_owned();
    let server_port = node.endpoint().port().get();
    let tag = format!("proxy-{}", caly_domain::to_hex(node.id().into_bytes()));
    let username = username.map(|cred| cred.with_exposed(str::to_owned));
    let password = password.map(|cred| cred.with_exposed(str::to_owned));
    // `https://` proxies carry a TLS config in the node; dropping it
    // would silently downgrade them to plaintext (agent audit 2026-08-12).
    let tls = tls_json(node);
    if kind == "http" {
        NodeOutbound::Http(HttpOutbound {
            password,
            server,
            server_port,
            tag,
            tls,
            username,
            kind,
        })
    } else {
        NodeOutbound::Socks(SocksOutbound {
            password,
            server,
            server_port,
            tag,
            tls,
            udp_over_tcp: true,
            username,
            kind,
        })
    }
}

/// Shadowsocks branch of [`node_outbound`] (extracted for the
/// 100-line budget): sing-box has no ss plugin (obfs-local /
/// v2ray-plugin) support, so a plugin node is rejected explicitly —
/// rendering a plain ss outbound would emit a dead proxy that looks
/// healthy.
fn node_outbound_ss(
    node: &DialableNode,
    method: caly_domain::ShadowsocksCipher,
    password: &caly_domain::Credential,
    plugin: Option<&caly_domain::ShadowsocksPlugin>,
) -> Result<NodeOutbound, SingBoxOutboundError> {
    if plugin.is_some() {
        return Err(SingBoxOutboundError::UnsupportedNode);
    }
    let Some(label) = ss_method(method) else {
        return Err(SingBoxOutboundError::UnsupportedNode);
    };
    Ok(NodeOutbound::Shadowsocks(ShadowsocksOutbound {
        method: label,
        password: password.with_exposed(str::to_owned),
        server: node.endpoint().host().as_str().to_owned(),
        server_port: node.endpoint().port().get(),
        tag: format!("proxy-{}", caly_domain::to_hex(node.id().into_bytes())),
        kind: "shadowsocks",
    }))
}

/// One sing-box outbound (per-protocol shapes; `type` discriminates).
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum NodeOutbound {
    Vless(Box<VlessOutbound>),
    Trojan(TrojanOutbound),
    Vmess(Box<VmessOutbound>),
    Shadowsocks(ShadowsocksOutbound),
    Hysteria2(Box<Hysteria2Outbound>),
    Tuic(Box<TuicOutbound>),
    Http(HttpOutbound),
    Socks(SocksOutbound),
}

/// `vless` outbound (uuid, optional XTLS flow, TLS, transport).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct VlessOutbound {
    #[serde(skip_serializing_if = "Option::is_none")]
    flow: Option<String>,
    server: String,
    server_port: u16,
    tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<TlsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<TransportJson>,
    #[serde(rename = "type")]
    kind: &'static str,
    uuid: String,
}

/// `trojan` outbound (password, TLS, transport).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TrojanOutbound {
    password: String,
    server: String,
    server_port: u16,
    tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<TlsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<TransportJson>,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// `vmess` outbound (uuid, alterId, security, TLS, transport).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct VmessOutbound {
    alter_id: u16,
    security: &'static str,
    server: String,
    server_port: u16,
    tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<TlsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<TransportJson>,
    #[serde(rename = "type")]
    kind: &'static str,
    uuid: String,
}

/// `shadowsocks` outbound (method label, password).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ShadowsocksOutbound {
    method: &'static str,
    password: String,
    server: String,
    server_port: u16,
    tag: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// `http` outbound (optional credentials, optional TLS for `https://`).
///
/// UDP cannot be carried by the `http` outbound at all: sing-box has no
/// UDP path for plain HTTP proxies (no `udp_over_tcp` field — that option
/// exists only on `socks`), so a route selecting it for UDP traffic fails
/// with "UDP is not supported by outbound". The renderer must keep http
/// nodes out of UDP routes instead (see the route-layer UDP fallback).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct HttpOutbound {
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    server: String,
    server_port: u16,
    tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<TlsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// `socks` outbound (optional credentials, optional TLS).
///
/// `udp_over_tcp` is always true: sing-box's socks outbound carries UDP
/// either over the service's UDP associate or over a TCP tunnel, and the
/// tunnel is the broadly supported fallback — without it, a route sending
/// UDP to a socks node whose server lacks UDP associate fails with "UDP is
/// not supported by outbound" (2026-08-12 fallback audit). Note the http
/// outbound has NO such option; only socks gets the UDP path.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SocksOutbound {
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    server: String,
    server_port: u16,
    tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<TlsJson>,
    udp_over_tcp: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// `hysteria2` outbound (password, optional up/down and salamander
/// obfuscation; TLS is mandatory at the sing-box schema level).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Hysteria2Outbound {
    #[serde(skip_serializing_if = "Option::is_none")]
    down_mbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    obfs: Option<ObfsJson>,
    password: String,
    server: String,
    server_port: u16,
    tag: String,
    tls: TlsJson,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    up_mbps: Option<u32>,
}

/// `tuic` outbound (uuid, password, congestion control; TLS mandatory).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TuicOutbound {
    congestion_control: &'static str,
    password: String,
    server: String,
    server_port: u16,
    tag: String,
    tls: TlsJson,
    #[serde(rename = "type")]
    kind: &'static str,
    uuid: String,
}

/// Salamander obfuscation block (hysteria2).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ObfsJson {
    password: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// Emits sing-box `tls`/`reality` settings when the node carries TLS, or
/// `None` when it does not.
///
/// `enabled: true` is mandatory: without it this sing-box line constructs a
/// nil TLS config and every outbound TLS handshake panics with a nil pointer
/// dereference in `ClientHandshake` — the reason the same subscription works
/// in v2rayN (Xray) but every TLS node died in caly.
fn tls_json(node: &DialableNode) -> Option<TlsJson> {
    node.tls().map(tls_json_from)
}

/// The hysteria2/tuic fallback: emit a TLS block keyed by the URI SNI when
/// present, otherwise by the server host, so the outbound is never TLS-less.
fn tls_or_host_fallback(node: &DialableNode) -> TlsJson {
    tls_json(node).unwrap_or_else(|| TlsJson {
        enabled: true,
        insecure: None,
        reality: None,
        server_name: Some(node.endpoint().host().as_str().to_owned()),
        utls: None,
    })
}

/// sing-box `tls` block (`enabled`, optional SNI / insecure / uTLS / reality).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TlsJson {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    insecure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reality: Option<RealityJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    utls: Option<UtlsJson>,
}

/// Reality handshake identity (public key, optional short id).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RealityJson {
    public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    short_id: Option<String>,
}

/// uTLS fingerprint block.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct UtlsJson {
    enabled: bool,
    fingerprint: &'static str,
}

fn tls_json_from(tls: &caly_domain::TlsConfig) -> TlsJson {
    TlsJson {
        enabled: true,
        insecure: tls.allow_insecure().then_some(true),
        // Reality handshake identity must be preserved, otherwise the outbound
        // cannot complete the handshake even though TLS is enabled.
        reality: tls.reality().map(|reality| RealityJson {
            public_key: reality.public_key().with_exposed(str::to_owned),
            short_id: reality
                .short_id()
                .map(|sid| sid.with_exposed(str::to_owned)),
        }),
        server_name: tls.sni().map(|sni| sni.as_str().to_owned()),
        // Reality (and browser-mimicking TLS) handshakes need a uTLS fingerprint:
        // without it sing-box dials with the Go standard-library fingerprint,
        // which Reality servers reject (and this sing-box release panics on the
        // nil uTLS config). The `fp` URI parameter is preserved end-to-end;
        // unknown labels fall back to `random` so a weird subscription value
        // can never produce an invalid outbound.
        utls: tls.fingerprint().map(|fingerprint| UtlsJson {
            enabled: true,
            fingerprint: sing_box_fingerprint(fingerprint.as_str()),
        }),
    }
}

/// Emits sing-box `transport` settings when the node uses a pluggable framing.
fn transport_json(node: &DialableNode) -> Option<TransportJson> {
    match node.transport()? {
        caly_domain::Transport::Tcp => None,
        caly_domain::Transport::WebSocket {
            path,
            host,
            early_data,
        } => Some(TransportJson::WebSocket(WebSocketTransport {
            early_data_header_name: early_data
                .as_ref()
                .map(|early| early.header_name().as_str().to_owned()),
            headers: host.as_ref().map(|host| WsHeadersJson {
                host: host.as_str().to_owned(),
            }),
            // sing-box understands Xray early data natively; dropping it here
            // would silently break the 0-RTT handshake the subscription
            // advertised (audit #64).
            max_early_data: early_data
                .as_ref()
                .map(caly_domain::WebSocketEarlyData::max_bytes),
            path: path.as_str().to_owned(),
            kind: "ws",
        })),
        caly_domain::Transport::Grpc { service_name } => Some(TransportJson::Grpc(GrpcTransport {
            service_name: service_name.as_str().to_owned(),
            kind: "grpc",
        })),
        caly_domain::Transport::Http2 { path, hosts } => {
            Some(TransportJson::HttpUpgrade(HttpUpgradeTransport {
                host: hosts.iter().next().map(|host| host.as_str().to_owned()),
                path: path.as_str().to_owned(),
                kind: "httpupgrade",
            }))
        }
        caly_domain::Transport::Quic => Some(TransportJson::Quic(QuicTransport { kind: "quic" })),
    }
}

/// sing-box transport block (ws/grpc/httpupgrade/quic shapes).
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum TransportJson {
    WebSocket(WebSocketTransport),
    Grpc(GrpcTransport),
    HttpUpgrade(HttpUpgradeTransport),
    Quic(QuicTransport),
}

/// `ws` transport (path, optional Host header, optional Xray early data).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct WebSocketTransport {
    #[serde(skip_serializing_if = "Option::is_none")]
    early_data_header_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<WsHeadersJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_early_data: Option<u32>,
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// WebSocket `headers` block (only `Host` is rendered).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct WsHeadersJson {
    #[serde(rename = "Host")]
    host: String,
}

/// `grpc` transport.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct GrpcTransport {
    service_name: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// `httpupgrade` transport (sing-box's name for HTTP/2 framing).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct HttpUpgradeTransport {
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// `quic` transport (marker block).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct QuicTransport {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// Maps a subscription fingerprint label onto the sing-box uTLS enum.
/// Known labels pass through; anything unknown falls back to `random` so an
/// exotic subscription value can never yield an invalid outbound.
fn sing_box_fingerprint(label: &str) -> &'static str {
    match label {
        "chrome" => "chrome",
        "firefox" => "firefox",
        "safari" => "safari",
        "ios" => "ios",
        "edge" => "edge",
        "random" => "random",
        "randomized" => "randomized",
        _ => "random",
    }
}

/// Maps a Domain VMess cipher to its sing-box cipher label.
fn vmess_cipher(cipher: VmessCipher) -> &'static str {
    match cipher {
        VmessCipher::Auto => "auto",
        VmessCipher::Aes128Gcm => "aes-128-gcm",
        VmessCipher::Chacha20Poly1305 => "chacha20-poly1305",
        VmessCipher::None => "none",
    }
}

/// Maps a Shadowsocks cipher to its sing-box label; `None` is returned for
/// ciphers sing-box cannot dial (e.g. legacy `aes-128-cfb`), so the node is
/// skipped instead of producing a rejected document.
fn ss_method(cipher: ShadowsocksCipher) -> Option<&'static str> {
    match cipher {
        ShadowsocksCipher::Aes128Gcm => Some("aes-128-gcm"),
        ShadowsocksCipher::Aes256Gcm => Some("aes-256-gcm"),
        ShadowsocksCipher::Chacha20IetfPoly1305 => Some("chacha20-ietf-poly1305"),
        ShadowsocksCipher::Xchacha20IetfPoly1305 => Some("xchacha20-ietf-poly1305"),
        ShadowsocksCipher::None => Some("none"),
        ShadowsocksCipher::Aes128Cfb | ShadowsocksCipher::Aes256Cfb => None,
    }
}

/// Maps a Domain congestion-control choice to its sing-box label.
fn congestion_label(congestion: caly_domain::CongestionControl) -> &'static str {
    match congestion {
        caly_domain::CongestionControl::Bbr => "bbr",
        caly_domain::CongestionControl::Cubic => "cubic",
        caly_domain::CongestionControl::NewReno => "new_reno",
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tls_enabled_tests;
