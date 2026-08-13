//! Mihomo per-proxy YAML rendering (protocol branches and shared helpers).

use caly_domain::{BoundedText, DialableNode, Protocol};

use super::proxy_sections::{MihomoProxyEntry, MihomoProxyError, MihomoProxyTag};

pub fn proxy_to_entry(
    node: &DialableNode,
    tag: MihomoProxyTag,
) -> Result<MihomoProxyEntry, MihomoProxyError> {
    let server = node.endpoint().host().as_str();
    let port = node.endpoint().port().get();
    let mut lines = Vec::new();
    lines.push(format!("    - name: {}", yaml_quote(tag.as_str())));
    match node.protocol() {
        Protocol::Vless { user_id, flow } => {
            mihomo_vless_lines(server, port, user_id, flow.as_ref(), &mut lines);
            mihomo_tls_lines(node, &mut lines);
            mihomo_transport_lines(node, &mut lines);
        }
        Protocol::Vmess {
            user_id,
            alter_id,
            security,
        } => {
            mihomo_vmess_lines(server, port, user_id, *alter_id, *security, &mut lines);
            mihomo_tls_lines(node, &mut lines);
            mihomo_transport_lines(node, &mut lines);
        }
        Protocol::Trojan { password } => {
            mihomo_trojan_lines(server, port, password, &mut lines);
            mihomo_tls_lines(node, &mut lines);
            mihomo_transport_lines(node, &mut lines);
        }
        Protocol::Shadowsocks {
            method,
            password,
            plugin,
        } => mihomo_ss_lines(server, port, *method, password, plugin.as_ref(), &mut lines),
        Protocol::Hysteria2 {
            password,
            up_mbps,
            down_mbps,
            obfuscation,
        } => mihomo_hysteria2_lines(
            server,
            port,
            password,
            *up_mbps,
            *down_mbps,
            obfuscation.as_ref(),
            &mut lines,
        ),
        Protocol::Tuic {
            user_id,
            password,
            congestion,
        } => mihomo_tuic_lines(server, port, user_id, password, *congestion, &mut lines),
        Protocol::Http { username, password } => {
            mihomo_auth_lines(
                "http",
                server,
                port,
                username.as_ref(),
                password.as_ref(),
                &mut lines,
            );
            mihomo_tls_lines(node, &mut lines);
        }
        Protocol::Socks5 { username, password } => {
            mihomo_auth_lines(
                "socks5",
                server,
                port,
                username.as_ref(),
                password.as_ref(),
                &mut lines,
            );
            mihomo_tls_lines(node, &mut lines);
        }
        _ => return Err(MihomoProxyError::NoUsableNodes),
    }
    let yaml = lines.join("\n");
    let yaml = BoundedText::new(yaml).map_err(MihomoProxyError::Text)?;
    Ok(MihomoProxyEntry {
        id: node.id(),
        tag,
        yaml,
    })
}

/// Appends the type/server/port trio shared by every protocol branch
/// (fixed emission order keeps re-renders byte-identical).
fn base_lines(lines: &mut Vec<String>, kind: &str, server: &str, port: u16) {
    lines.push(format!("      type: {kind}"));
    lines.push(format!("      server: {server}"));
    lines.push(format!("      port: {port}"));
}

/// Appends Mihomo `http`/`socks5` proxy YAML lines (type, server,
/// optional auth) — the two protocols share every field.
fn mihomo_auth_lines(
    kind: &str,
    server: &str,
    port: u16,
    username: Option<&caly_domain::Credential>,
    password: Option<&caly_domain::Credential>,
    lines: &mut Vec<String>,
) {
    base_lines(lines, kind, server, port);
    if let Some(username) = username {
        username.with_exposed(|value| lines.push(format!("      username: {}", yaml_quote(value))));
    }
    if let Some(password) = password {
        password.with_exposed(|value| lines.push(format!("      password: {}", yaml_quote(value))));
    }
}

/// Appends Mihomo `vless` YAML lines (uuid, optional XTLS flow).
fn mihomo_vless_lines(
    server: &str,
    port: u16,
    user_id: &caly_domain::Credential,
    flow: Option<&caly_domain::ProtocolText>,
    lines: &mut Vec<String>,
) {
    base_lines(lines, "vless", server, port);
    user_id.with_exposed(|uuid| lines.push(format!("      uuid: {uuid}")));
    lines.push("      udp: true".to_owned());
    if let Some(flow) = flow {
        lines.push(format!("      flow: {}", flow.as_str()));
    }
}

/// Appends Mihomo `vmess` YAML lines (uuid, alterId, cipher).
fn mihomo_vmess_lines(
    server: &str,
    port: u16,
    user_id: &caly_domain::Credential,
    alter_id: u16,
    security: caly_domain::VmessCipher,
    lines: &mut Vec<String>,
) {
    base_lines(lines, "vmess", server, port);
    user_id.with_exposed(|uuid| lines.push(format!("      uuid: {uuid}")));
    lines.push(format!("      alterId: {alter_id}"));
    lines.push(format!(
        "      cipher: {}",
        crate::labels::vmess_cipher(security)
    ));
}

/// Appends Mihomo `trojan` YAML lines (password).
fn mihomo_trojan_lines(
    server: &str,
    port: u16,
    password: &caly_domain::Credential,
    lines: &mut Vec<String>,
) {
    base_lines(lines, "trojan", server, port);
    password.with_exposed(|value| lines.push(format!("      password: {}", yaml_quote(value))));
    lines.push("      udp: true".to_owned());
}

/// Appends Mihomo `ss` (Shadowsocks) YAML lines (cipher, password,
/// optional plugin). Mihomo supports obfs-local / v2ray-plugin via
/// `plugin:` + `plugin-opts:`; dropping the parameter would render a
/// node that can never dial (2026-08-12: the parser now carries the
/// plugin into the protocol, so the renderer must not lose it).
fn mihomo_ss_lines(
    server: &str,
    port: u16,
    method: caly_domain::ShadowsocksCipher,
    password: &caly_domain::Credential,
    plugin: Option<&caly_domain::ShadowsocksPlugin>,
    lines: &mut Vec<String>,
) {
    base_lines(lines, "ss", server, port);
    lines.push(format!(
        "      cipher: {}",
        crate::labels::ss_cipher_label(method)
    ));
    password.with_exposed(|value| lines.push(format!("      password: {}", yaml_quote(value))));
    if let Some(plugin) = plugin {
        lines.push(format!("      plugin: {}", yaml_quote(plugin.label())));
        if !plugin.option_text().is_empty() {
            lines.push(format!(
                "      plugin-opts: {}",
                yaml_quote(plugin.option_text())
            ));
        }
    }
}

/// Appends Mihomo `hysteria2` YAML lines (password, optional up/down and
/// salamander obfuscation).
fn mihomo_hysteria2_lines(
    server: &str,
    port: u16,
    password: &caly_domain::Credential,
    up_mbps: Option<u32>,
    down_mbps: Option<u32>,
    obfuscation: Option<&caly_domain::Credential>,
    lines: &mut Vec<String>,
) {
    base_lines(lines, "hysteria2", server, port);
    password.with_exposed(|value| lines.push(format!("      password: {}", yaml_quote(value))));
    if let Some(up) = up_mbps {
        lines.push(format!("      up: {up}"));
    }
    if let Some(down) = down_mbps {
        lines.push(format!("      down: {down}"));
    }
    if let Some(obfs) = obfuscation {
        obfs.with_exposed(|value| {
            lines.push("      obfs: salamander".to_owned());
            lines.push(format!("      obfs-password: {}", yaml_quote(value)));
        });
    }
}

/// Appends Mihomo `tuic` YAML lines (uuid, password, congestion controller).
fn mihomo_tuic_lines(
    server: &str,
    port: u16,
    user_id: &caly_domain::Credential,
    password: &caly_domain::Credential,
    congestion: caly_domain::CongestionControl,
    lines: &mut Vec<String>,
) {
    base_lines(lines, "tuic", server, port);
    user_id.with_exposed(|uuid| lines.push(format!("      uuid: {uuid}")));
    password.with_exposed(|value| lines.push(format!("      password: {}", yaml_quote(value))));
    lines.push(format!(
        "      congestion-controller: {}",
        crate::labels::congestion_label(congestion)
    ));
}

/// Appends Mihomo TLS/Reality YAML lines when the node carries TLS.
fn mihomo_tls_lines(node: &DialableNode, lines: &mut Vec<String>) {
    let Some(tls) = node.tls() else {
        return;
    };
    lines.push("      tls: true".to_owned());
    if let Some(sni) = tls.sni() {
        lines.push(format!("      servername: {}", yaml_quote(sni.as_str())));
    }
    if let Some(reality) = tls.reality() {
        lines.push("      reality-opts:".to_owned());
        reality.public_key().with_exposed(|key| {
            lines.push(format!("        public-key: {}", yaml_quote(key)));
        });
        if let Some(sid) = reality.short_id() {
            sid.with_exposed(|sid| {
                lines.push(format!("        short-id: {}", yaml_quote(sid)));
            });
        }
    }
}

/// Appends Mihomo transport YAML lines when the node uses a pluggable framing.
fn mihomo_transport_lines(node: &DialableNode, lines: &mut Vec<String>) {
    let Some(transport) = node.transport() else {
        return;
    };
    match transport {
        caly_domain::Transport::Tcp => {}
        caly_domain::Transport::WebSocket { path, host, .. } => {
            lines.push("      network: ws".to_owned());
            lines.push("      ws-opts:".to_owned());
            lines.push(format!("        path: {}", yaml_quote(path.as_str())));
            if let Some(host) = host {
                lines.push("        headers:".to_owned());
                lines.push(format!("          Host: {}", yaml_quote(host.as_str())));
            }
            // NOTE: mihomo's ws transport has no early-data fields (Clash Meta
            // never adopted Xray's `ed`/`eh`); the node still works, just
            // without the 0-RTT acceleration (audit #64).
        }
        caly_domain::Transport::Grpc { service_name } => {
            lines.push("      network: grpc".to_owned());
            lines.push("      grpc-opts:".to_owned());
            lines.push(format!(
                "        grpc-service-name: {}",
                yaml_quote(service_name.as_str())
            ));
        }
        caly_domain::Transport::Http2 { path, hosts } => {
            lines.push("      network: httpupgrade".to_owned());
            lines.push("      http-upgrade-opts:".to_owned());
            lines.push(format!("        path: {}", yaml_quote(path.as_str())));
            if let Some(host) = hosts.iter().next() {
                lines.push(format!("        host: {}", yaml_quote(host.as_str())));
            }
        }
        caly_domain::Transport::Quic => {
            lines.push("      network: quic".to_owned());
        }
    }
}

/// Double-quotes YAML scalar text with JSON-style escaping.
///
/// Escapes every character that is unsafe inside a YAML double-quoted scalar:
/// the quote/backslash pair, the named whitespace controls, and every other
/// control character (C0 + DEL) as the canonical `\xNN` form. Omitting a
/// control character would emit invalid YAML (mihomo's yaml.v3 rejects raw
/// control bytes), so a hostile node/group name can only ever degrade to a
/// validation failure, never to structure injection.
pub(crate) fn yaml_quote(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\x{:02x}", u32::from(ch));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod hostile_tests;
