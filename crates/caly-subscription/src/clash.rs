//! Typed Clash YAML proxy conversion into complete Domain nodes.

use core::num::NonZeroU16;

use caly_domain::{
    CongestionControl, Credential, DialableNode, Endpoint, EndpointHost, MAX_PROXY_GROUP_MEMBERS,
    MAX_PROXY_GROUPS, NodeBuilder, NodeSource, Protocol, ProtocolText, ProxyGroup,
    ProxyGroupMember, ProxyGroupName, ProxyGroupNodeTag, ProxyGroupType, ProxyGroupUrl,
    RoutingRule, ShadowsocksCipher, SubscriptionId, TlsConfig, Transport, TransportText,
    TransportTextList, UrlTestConfig, VmessCipher, sanitized_display_name,
};
use serde::Deserialize;

use super::format::SubscriptionDocument;
use super::pipeline::MAX_SUBSCRIPTION_NODES;

#[derive(Deserialize)]
struct ClashDocument {
    proxies: Vec<ClashProxy>,
    #[serde(default, rename = "proxy-groups")]
    proxy_groups: Vec<ClashProxyGroup>,
    #[serde(default)]
    rules: Vec<String>,
}

/// One `proxy-groups:` entry. Only the fields caly can render are typed;
/// Mihomo-specific extras (`strategy`, `timeout`, `lazy`, `filter`, …) are
/// tolerated and ignored — dropping them keeps an otherwise usable import
/// alive instead of rejecting the whole file.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ClashProxyGroup {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    proxies: Vec<String>,
    url: Option<String>,
    interval: Option<u32>,
    tolerance: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ClashProxy {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    server: String,
    port: u16,
    uuid: Option<String>,
    password: Option<String>,
    username: Option<String>,
    cipher: Option<String>,
    #[serde(default)]
    alter_id: u16,
    flow: Option<String>,
    network: Option<String>,
    #[serde(default)]
    tls: bool,
    servername: Option<String>,
    sni: Option<String>,
    #[serde(rename = "ws-opts")]
    ws_opts: Option<ClashWsOptions>,
    #[serde(rename = "grpc-opts")]
    grpc_opts: Option<ClashGrpcOptions>,
    up: Option<u32>,
    down: Option<u32>,
    obfs_password: Option<String>,
    congestion_controller: Option<String>,
    private_key: Option<String>,
    public_key: Option<String>,
    reserved: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct ClashWsOptions {
    path: Option<String>,
    headers: Option<ClashWsHeaders>,
}

#[derive(Deserialize)]
struct ClashWsHeaders {
    #[serde(rename = "Host")]
    host: Option<String>,
}

#[derive(Deserialize)]
struct ClashGrpcOptions {
    #[serde(rename = "grpc-service-name")]
    service_name: Option<String>,
}

/// Indexed Clash conversion failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClashParseError {
    YamlRejected,
    TooManyNodes,
    TooManyGroups,
    InvalidName(usize),
    InvalidEndpoint(usize),
    MissingField { index: usize, field: &'static str },
    UnsupportedProtocol { index: usize },
    UnsupportedCipher { index: usize },
    InvalidTransport { index: usize },
    InvalidTls { index: usize },
    NodeRejected(usize),
    DuplicateGroupName { index: usize },
    InvalidGroupName(usize),
    UnsupportedGroupType { index: usize },
    TooManyGroupMembers { index: usize },
    InvalidGroupMember { index: usize },
    InvalidProbeUrl { index: usize },
    InvalidRule { index: usize },
    UnknownRulePolicy { index: usize },
}

/// Everything a complete Clash YAML carries: the `proxies:` node list, the
/// `proxy-groups:` selectors the operator grouped them into, and the `rules:`
/// policy table. Top-level *daemon* keys (`mixed-port`, `dns`, `hosts`, …)
/// are deliberately out of scope: they configure the Mihomo process, not the
/// subscription content; caly's own schema owns those concerns.
pub struct ClashImport {
    pub proxies: Vec<DialableNode>,
    pub proxy_groups: Vec<ProxyGroup>,
    pub rules: Vec<RoutingRule>,
}

/// Parses the `proxies` list and rejects the entire document on any bad node.
/// `proxy-groups:` / `rules:` (when present) are left to [`parse_clash_config`].
pub fn parse_clash_yaml(
    source: &str,
    subscription: SubscriptionId,
) -> Result<Vec<DialableNode>, ClashParseError> {
    let document = read_document(source)?;
    if document.proxies.len() > MAX_SUBSCRIPTION_NODES {
        return Err(ClashParseError::TooManyNodes);
    }
    document
        .proxies
        .into_iter()
        .enumerate()
        .map(|(index, proxy)| convert_proxy(index, &proxy, subscription))
        .collect()
}

fn read_document(source: &str) -> Result<ClashDocument, ClashParseError> {
    serde_norway::from_str(source).map_err(|_| ClashParseError::YamlRejected)
}

/// Full-document import: nodes **and** the operator's `proxy-groups:` and
/// `rules:` (previously both were silently dropped, so importing a Clash
/// config lost the whole grouping/routing intent). Rule policies must resolve
/// to a declared group, one of the node `name:` tags, or the built-in
/// `DIRECT` / `REJECT` tokens — anything else is a broken reference the live
/// core would also refuse, so the import fails loudly (`UnknownRulePolicy`).
pub fn parse_clash_config(
    source: &str,
    subscription: SubscriptionId,
) -> Result<ClashImport, ClashParseError> {
    let document = read_document(source)?;
    if document.proxies.len() > MAX_SUBSCRIPTION_NODES {
        return Err(ClashParseError::TooManyNodes);
    }
    if document.proxy_groups.len() > MAX_PROXY_GROUPS {
        return Err(ClashParseError::TooManyGroups);
    }
    // The member classifier needs the FULL group name set up front: Clash
    // allows forward references (`节点选择` listing `自动选择` declared two
    // entries later), and duplicates must fail before any conversion.
    let mut group_names = std::collections::HashSet::with_capacity(document.proxy_groups.len());
    for (index, group) in document.proxy_groups.iter().enumerate() {
        if !group_names.insert(group.name.clone()) {
            return Err(ClashParseError::DuplicateGroupName { index });
        }
    }
    let proxies = document
        .proxies
        .iter()
        .enumerate()
        .map(|(index, proxy)| convert_proxy(index, proxy, subscription))
        .collect::<Result<Vec<_>, _>>()?;
    let proxy_groups = document
        .proxy_groups
        .iter()
        .enumerate()
        .map(|(index, group)| convert_group(index, group, &group_names))
        .collect::<Result<Vec<_>, _>>()?;
    let rules = convert_rules(&document.rules, &group_names, &document.proxies)?;
    Ok(ClashImport {
        proxies,
        proxy_groups,
        rules,
    })
}

/// Clash reference semantics: `DIRECT` / `REJECT` are built-ins, a name that
/// matches a declared group is a nested-group reference, everything else is a
/// node tag. (Clash resolves nodes/groups in one namespace.)
fn convert_group_member(
    index: usize,
    name: &str,
    group_names: &std::collections::HashSet<String>,
) -> Result<ProxyGroupMember, ClashParseError> {
    match name {
        "DIRECT" => Ok(ProxyGroupMember::Direct),
        "REJECT" => Ok(ProxyGroupMember::Reject),
        _ if group_names.contains(name) => Ok(ProxyGroupMember::Group {
            name: ProxyGroupName::new(name.to_owned())
                .map_err(|_| ClashParseError::InvalidGroupMember { index })?,
        }),
        _ => Ok(ProxyGroupMember::Node {
            tag: ProxyGroupNodeTag::new(name.to_owned())
                .map_err(|_| ClashParseError::InvalidGroupMember { index })?,
        }),
    }
}

fn convert_group(
    index: usize,
    group: &ClashProxyGroup,
    group_names: &std::collections::HashSet<String>,
) -> Result<ProxyGroup, ClashParseError> {
    let kind = match group.kind.as_str() {
        "select" => ProxyGroupType::Select,
        "url-test" => ProxyGroupType::UrlTest,
        "fallback" => ProxyGroupType::Fallback,
        "load-balance" => ProxyGroupType::LoadBalance,
        "relay" => ProxyGroupType::Relay,
        _ => return Err(ClashParseError::UnsupportedGroupType { index }),
    };
    if group.proxies.len() > MAX_PROXY_GROUP_MEMBERS {
        return Err(ClashParseError::TooManyGroupMembers { index });
    }
    let name = ProxyGroupName::new(group.name.clone())
        .map_err(|_| ClashParseError::InvalidGroupName(index))?;
    let members = group
        .proxies
        .iter()
        .map(|member| convert_group_member(index, member, group_names))
        .collect::<Result<Vec<_>, _>>()?;
    let url_test = if kind.needs_url() {
        Some(convert_probe(index, group)?)
    } else {
        None
    };
    Ok(ProxyGroup {
        name,
        kind,
        members,
        url_test,
    })
}

/// `url-test` / `fallback` / `load-balance` probe block. `url` defaults to
/// Mihomo's `http://www.gstatic.com/generate_204` when omitted; the same
/// http(s)-or-reject rule the schema validator enforces applies here so a
/// `url: google.com` typo cannot ride into the rendered config.
fn convert_probe(index: usize, group: &ClashProxyGroup) -> Result<UrlTestConfig, ClashParseError> {
    const DEFAULT_URL: &str = "http://www.gstatic.com/generate_204";
    let url = group.url.as_deref().unwrap_or(DEFAULT_URL);
    let parsed = url::Url::parse(url).map_err(|_| ClashParseError::InvalidProbeUrl { index })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ClashParseError::InvalidProbeUrl { index });
    }
    Ok(UrlTestConfig {
        url: ProxyGroupUrl::new(url.to_owned())
            .map_err(|_| ClashParseError::InvalidProbeUrl { index })?,
        interval_seconds: group.interval.unwrap_or(300),
        tolerance_ms: group.tolerance.unwrap_or(50),
    })
}

fn convert_rules(
    lines: &[String],
    group_names: &std::collections::HashSet<String>,
    proxies: &[ClashProxy],
) -> Result<Vec<RoutingRule>, ClashParseError> {
    let mut rules = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let rule = RoutingRule::from_clash_line(line)
            .map_err(|_| ClashParseError::InvalidRule { index })?;
        if let caly_domain::RulePolicy::Proxy(target) = &rule.policy {
            let resolved = group_names.contains(target.as_str())
                || proxies.iter().any(|proxy| proxy.name == target.as_str());
            if !resolved {
                return Err(ClashParseError::UnknownRulePolicy { index });
            }
        }
        rules.push(rule);
    }
    Ok(rules)
}

fn convert_proxy(
    index: usize,
    proxy: &ClashProxy,
    subscription: SubscriptionId,
) -> Result<DialableNode, ClashParseError> {
    let name = sanitized_display_name(proxy.name.clone())
        .map_err(|_| ClashParseError::InvalidName(index))?;
    let host = EndpointHost::new(proxy.server.clone())
        .map_err(|_| ClashParseError::InvalidEndpoint(index))?;
    let port = NonZeroU16::new(proxy.port).ok_or(ClashParseError::InvalidEndpoint(index))?;
    let protocol = clash_protocol(index, proxy)?;
    let mut builder = NodeBuilder::new(
        name,
        Endpoint::new(host, port),
        protocol,
        NodeSource::Subscription(subscription),
    );
    if let Some(transport) = clash_transport(index, proxy)? {
        builder = builder.with_transport(transport);
    }
    if proxy.tls {
        builder = builder.with_tls(clash_tls(index, proxy)?);
    }
    builder
        .build()
        .map_err(|_| ClashParseError::NodeRejected(index))
}

fn clash_protocol(index: usize, proxy: &ClashProxy) -> Result<Protocol, ClashParseError> {
    match proxy.kind.to_ascii_lowercase().as_str() {
        "vmess" => Ok(Protocol::Vmess {
            user_id: required_secret(index, "uuid", proxy.uuid.as_deref())?,
            alter_id: proxy.alter_id,
            security: VmessCipher::Auto,
        }),
        "vless" => Ok(Protocol::Vless {
            user_id: required_secret(index, "uuid", proxy.uuid.as_deref())?,
            flow: optional_text(proxy.flow.as_deref())?,
        }),
        "trojan" => Ok(Protocol::Trojan {
            password: required_secret(index, "password", proxy.password.as_deref())?,
        }),
        "ss" | "shadowsocks" => Ok(Protocol::Shadowsocks {
            method: clash_cipher(index, proxy.cipher.as_deref())?,
            password: required_secret(index, "password", proxy.password.as_deref())?,
            plugin: None,
        }),
        "hysteria2" | "hy2" => Ok(Protocol::Hysteria2 {
            password: required_secret(index, "password", proxy.password.as_deref())?,
            up_mbps: proxy.up,
            down_mbps: proxy.down,
            obfuscation: optional_secret(proxy.obfs_password.as_deref())?,
        }),
        "tuic" => Ok(Protocol::Tuic {
            user_id: required_secret(index, "uuid", proxy.uuid.as_deref())?,
            password: required_secret(index, "password", proxy.password.as_deref())?,
            congestion: congestion(index, proxy.congestion_controller.as_deref())?,
        }),
        "wireguard" => Ok(Protocol::WireGuard {
            private_key: required_secret(index, "private-key", proxy.private_key.as_deref())?,
            peer_public_key: required_secret(index, "public-key", proxy.public_key.as_deref())?,
            reserved: reserved(index, proxy.reserved.as_deref())?,
        }),
        "http" => Ok(Protocol::Http {
            username: optional_secret(proxy.username.as_deref())?,
            password: optional_secret(proxy.password.as_deref())?,
        }),
        "socks5" | "socks" => Ok(Protocol::Socks5 {
            username: optional_secret(proxy.username.as_deref())?,
            password: optional_secret(proxy.password.as_deref())?,
        }),
        _ => Err(ClashParseError::UnsupportedProtocol { index }),
    }
}

fn clash_transport(index: usize, proxy: &ClashProxy) -> Result<Option<Transport>, ClashParseError> {
    match proxy.network.as_deref().unwrap_or("tcp") {
        "tcp" => Ok(Some(Transport::Tcp)),
        "ws" => {
            let options = proxy.ws_opts.as_ref();
            let path = options
                .and_then(|value| value.path.as_deref())
                .unwrap_or("/");
            let host = options
                .and_then(|value| value.headers.as_ref())
                .and_then(|value| value.host.as_deref());
            Ok(Some(Transport::WebSocket {
                path: transport_text(index, path)?,
                host: host.map(|value| transport_text(index, value)).transpose()?,
                // Clash YAML ws-opts has no early-data concept (audit #64).
                early_data: None,
            }))
        }
        "grpc" => {
            let service = proxy
                .grpc_opts
                .as_ref()
                .and_then(|value| value.service_name.as_deref())
                .unwrap_or("grpc");
            Ok(Some(Transport::Grpc {
                service_name: transport_text(index, service)?,
            }))
        }
        "quic" => Ok(Some(Transport::Quic)),
        _ => Err(ClashParseError::InvalidTransport { index }),
    }
}

fn clash_tls(index: usize, proxy: &ClashProxy) -> Result<TlsConfig, ClashParseError> {
    let sni = proxy.servername.as_deref().or(proxy.sni.as_deref());
    let sni = sni
        .map(|value| transport_text(index, value))
        .transpose()
        .map_err(|_| ClashParseError::InvalidTls { index })?;
    Ok(TlsConfig::new(
        sni,
        TransportTextList::new(),
        false,
        None,
        None,
    ))
}

fn required_secret(
    index: usize,
    field: &'static str,
    value: Option<&str>,
) -> Result<Credential, ClashParseError> {
    optional_secret(value)?.ok_or(ClashParseError::MissingField { index, field })
}

fn optional_secret(value: Option<&str>) -> Result<Option<Credential>, ClashParseError> {
    value
        .map(|value| Credential::new(value.to_owned()).map_err(|_| ClashParseError::YamlRejected))
        .transpose()
}

fn optional_text(value: Option<&str>) -> Result<Option<ProtocolText>, ClashParseError> {
    value
        .map(|value| ProtocolText::new(value.to_owned()).map_err(|_| ClashParseError::YamlRejected))
        .transpose()
}

fn transport_text(index: usize, value: &str) -> Result<TransportText, ClashParseError> {
    TransportText::new(value.to_owned()).map_err(|_| ClashParseError::InvalidTransport { index })
}

fn clash_cipher(index: usize, value: Option<&str>) -> Result<ShadowsocksCipher, ClashParseError> {
    match value {
        Some("aes-128-gcm") => Ok(ShadowsocksCipher::Aes128Gcm),
        Some("aes-256-gcm") => Ok(ShadowsocksCipher::Aes256Gcm),
        Some("chacha20-ietf-poly1305") => Ok(ShadowsocksCipher::Chacha20IetfPoly1305),
        Some("xchacha20-ietf-poly1305") => Ok(ShadowsocksCipher::Xchacha20IetfPoly1305),
        Some("aes-128-cfb") => Ok(ShadowsocksCipher::Aes128Cfb),
        Some("aes-256-cfb") => Ok(ShadowsocksCipher::Aes256Cfb),
        Some("none") => Ok(ShadowsocksCipher::None),
        _ => Err(ClashParseError::UnsupportedCipher { index }),
    }
}

fn congestion(_index: usize, value: Option<&str>) -> Result<CongestionControl, ClashParseError> {
    match value.unwrap_or("bbr") {
        "bbr" => Ok(CongestionControl::Bbr),
        "cubic" => Ok(CongestionControl::Cubic),
        "new_reno" | "new-reno" => Ok(CongestionControl::NewReno),
        _ => Err(ClashParseError::YamlRejected),
    }
}

fn reserved(index: usize, value: Option<&[u8]>) -> Result<Option<[u8; 3]>, ClashParseError> {
    let Some(value) = value else { return Ok(None) };
    if value.len() != 3 {
        return Err(ClashParseError::MissingField {
            index,
            field: "reserved[3]",
        });
    }
    Ok(Some([value[0], value[1], value[2]]))
}

/// Extracts the subscription author's full routing surface — the declared
/// proxy groups plus the rule table — from a raw subscription body.
///
/// This is the entry point of the "subscriptions own the routing" decision
/// (2026-08-09 规划): when a source document declares `proxy-groups`, those
/// groups are rendered into the kernel verbatim (alongside their `rules`)
/// and the implicit `url-test` fallback group is omitted entirely. A URI
/// list carries no routing, and a body that does not parse as a complete
/// Clash document gets no routing either — both return `None` here so the
/// caller falls back to the implicit group, mirroring the best-effort
/// indexing contract of the node registry. A `proxy-groups`-less Clash body
/// behaves the same way: rules without a group skeleton would dangle.
pub fn clash_routing_from_body(
    body: &[u8],
    subscription: SubscriptionId,
) -> Option<(Vec<ProxyGroup>, Vec<RoutingRule>)> {
    let source = core::str::from_utf8(body).ok()?;
    let import = parse_clash_config(source, subscription).ok()?;
    if import.proxy_groups.is_empty() {
        return None;
    }
    Some((import.proxy_groups, import.rules))
}

/// [`clash_routing_from_body`] on an already-decoded document (single-pass
/// pipeline; only a Clash-YAML document can carry routing — a URI-line or
/// SIP008 body returns `None`, mirroring the body variant's UTF-8 gate).
pub fn clash_routing_from_document(
    document: &SubscriptionDocument,
    subscription: SubscriptionId,
) -> Option<(Vec<ProxyGroup>, Vec<RoutingRule>)> {
    let SubscriptionDocument::ClashYaml(body) = document else {
        return None;
    };
    let source = core::str::from_utf8(body.as_slice()).ok()?;
    let import = parse_clash_config(source, subscription).ok()?;
    if import.proxy_groups.is_empty() {
        return None;
    }
    Some((import.proxy_groups, import.rules))
}
