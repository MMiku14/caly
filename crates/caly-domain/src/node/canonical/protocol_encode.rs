//! Protocol-specific canonical identity encoding.

use super::super::{
    CongestionControl, Credential, Protocol, ShadowsocksCipher, ShadowsocksPlugin, VmessCipher,
};
use super::{optional_text, CanonicalEncoder};

pub(super) fn encode(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    encode_protocol(writer, protocol);
}

fn encode_protocol(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    match protocol {
        Protocol::Vmess { .. } => encode_vmess_variant(writer, protocol),
        Protocol::Vless { .. } => encode_vless_variant(writer, protocol),
        Protocol::Trojan { .. } => encode_trojan_variant(writer, protocol),
        Protocol::Shadowsocks { .. } => encode_shadowsocks_variant(writer, protocol),
        Protocol::Hysteria2 { .. } => encode_hysteria2_variant(writer, protocol),
        Protocol::Tuic { .. } => encode_tuic_variant(writer, protocol),
        Protocol::WireGuard { .. } => encode_wireguard_variant(writer, protocol),
        Protocol::Http { .. } => encode_user_password_variant(writer, "http", protocol),
        Protocol::Socks5 { .. } => encode_user_password_variant(writer, "socks5", protocol),
        Protocol::ShadowTls { .. } => encode_shadow_tls_variant(writer, protocol),
        Protocol::AnyTls { .. } => encode_any_tls_variant(writer, protocol),
    }
}

fn encode_vmess_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::Vmess {
        user_id,
        alter_id,
        security,
    } = protocol
    else {
        return;
    };
    encode_vmess_protocol(writer, user_id, *alter_id, *security);
}

fn encode_vless_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::Vless { user_id, flow } = protocol else {
        return;
    };
    writer.text("vless");
    writer.secret(user_id);
    optional_text(writer, flow.as_ref());
}

fn encode_trojan_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::Trojan { password } = protocol else {
        return;
    };
    writer.text("trojan");
    writer.secret(password);
}

fn encode_shadowsocks_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::Shadowsocks {
        method,
        password,
        plugin,
    } = protocol
    else {
        return;
    };
    encode_shadowsocks(writer, *method, password, plugin.as_ref());
}

fn encode_hysteria2_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::Hysteria2 {
        password,
        up_mbps,
        down_mbps,
        obfuscation,
    } = protocol
    else {
        return;
    };
    encode_hysteria2(writer, password, *up_mbps, *down_mbps, obfuscation.as_ref());
}

fn encode_tuic_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::Tuic {
        user_id,
        password,
        congestion,
    } = protocol
    else {
        return;
    };
    writer.text("tuic");
    writer.secret(user_id);
    writer.secret(password);
    encode_congestion(writer, *congestion);
}

fn encode_wireguard_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::WireGuard {
        private_key,
        peer_public_key,
        reserved,
    } = protocol
    else {
        return;
    };
    writer.text("wireguard");
    writer.secret(private_key);
    writer.secret(peer_public_key);
    writer.optional(reserved.as_ref(), |out, value| out.bytes(value));
}

fn encode_user_password_variant(writer: &mut CanonicalEncoder, tag: &str, protocol: &Protocol) {
    let (username, password) = match protocol {
        Protocol::Http { username, password } | Protocol::Socks5 { username, password } => {
            (username.as_ref(), password.as_ref())
        }
        _ => return,
    };
    encode_user_password(writer, tag, username, password);
}

fn encode_shadow_tls_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::ShadowTls {
        password,
        version,
        sni,
    } = protocol
    else {
        return;
    };
    writer.text("shadow-tls");
    writer.secret(password);
    writer.u8(*version);
    optional_text(writer, sni.as_ref());
}

fn encode_any_tls_variant(writer: &mut CanonicalEncoder, protocol: &Protocol) {
    let Protocol::AnyTls { password, sni } = protocol else {
        return;
    };
    writer.text("anytls");
    writer.secret(password);
    optional_text(writer, sni.as_ref());
}

fn encode_vmess_protocol(
    writer: &mut CanonicalEncoder,
    user_id: &Credential,
    alter_id: u16,
    security: VmessCipher,
) {
    writer.text("vmess");
    writer.secret(user_id);
    writer.u16(alter_id);
    encode_vmess(writer, security);
}

fn encode_shadowsocks(
    writer: &mut CanonicalEncoder,
    method: ShadowsocksCipher,
    password: &Credential,
    plugin: Option<&ShadowsocksPlugin>,
) {
    writer.text("shadowsocks");
    encode_ss(writer, method);
    writer.secret(password);
    writer.optional(plugin, |out, value| {
        out.text(value.name().as_str());
        out.text(value.options().as_str());
    });
}

fn encode_hysteria2(
    writer: &mut CanonicalEncoder,
    password: &Credential,
    up_mbps: Option<u32>,
    down_mbps: Option<u32>,
    obfuscation: Option<&Credential>,
) {
    writer.text("hysteria2");
    writer.secret(password);
    optional_u32(writer, up_mbps);
    optional_u32(writer, down_mbps);
    writer.optional(obfuscation, CanonicalEncoder::secret);
}

fn encode_user_password(
    writer: &mut CanonicalEncoder,
    tag: &str,
    username: Option<&Credential>,
    password: Option<&Credential>,
) {
    writer.text(tag);
    writer.optional(username, CanonicalEncoder::secret);
    writer.optional(password, CanonicalEncoder::secret);
}

fn optional_u32(writer: &mut CanonicalEncoder, value: Option<u32>) {
    writer.optional(value.as_ref(), |out, number| out.u32(*number));
}

fn encode_vmess(writer: &mut CanonicalEncoder, value: VmessCipher) {
    writer.u8(match value {
        VmessCipher::Auto => 0,
        VmessCipher::Aes128Gcm => 1,
        VmessCipher::Chacha20Poly1305 => 2,
        VmessCipher::None => 3,
    });
}

fn encode_ss(writer: &mut CanonicalEncoder, value: ShadowsocksCipher) {
    writer.u8(match value {
        ShadowsocksCipher::Aes128Gcm => 0,
        ShadowsocksCipher::Aes256Gcm => 1,
        ShadowsocksCipher::Chacha20IetfPoly1305 => 2,
        ShadowsocksCipher::Xchacha20IetfPoly1305 => 3,
        ShadowsocksCipher::Aes128Cfb => 4,
        ShadowsocksCipher::None => 5,
        ShadowsocksCipher::Aes256Cfb => 6,
    });
}

fn encode_congestion(writer: &mut CanonicalEncoder, value: CongestionControl) {
    writer.u8(match value {
        CongestionControl::Bbr => 0,
        CongestionControl::Cubic => 1,
        CongestionControl::NewReno => 2,
    });
}
