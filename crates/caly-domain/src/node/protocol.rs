//! Complete protocol-specific dial identities.

use crate::{BoundedText, SecretText};

/// Maximum credential length accepted by the Domain model.
pub const CREDENTIAL_MAX_BYTES: usize = 4_096;
/// Maximum protocol option/plugin text length.
pub const PROTOCOL_TEXT_MAX_BYTES: usize = 1_024;

/// Protocol credential.
pub type Credential = SecretText<CREDENTIAL_MAX_BYTES>;
/// Safe bounded protocol option text.
pub type ProtocolText = BoundedText<PROTOCOL_TEXT_MAX_BYTES>;

/// Complete protocol fields required before node identity is computed.
#[derive(Debug)]
pub enum Protocol {
    Vmess {
        user_id: Credential,
        alter_id: u16,
        security: VmessCipher,
    },
    Vless {
        user_id: Credential,
        flow: Option<ProtocolText>,
    },
    Trojan {
        password: Credential,
    },
    Shadowsocks {
        method: ShadowsocksCipher,
        password: Credential,
        plugin: Option<ShadowsocksPlugin>,
    },
    Hysteria2 {
        password: Credential,
        up_mbps: Option<u32>,
        down_mbps: Option<u32>,
        obfuscation: Option<Credential>,
    },
    Tuic {
        user_id: Credential,
        password: Credential,
        congestion: CongestionControl,
    },
    WireGuard {
        private_key: Credential,
        peer_public_key: Credential,
        reserved: Option<[u8; 3]>,
    },
    Http {
        username: Option<Credential>,
        password: Option<Credential>,
    },
    Socks5 {
        username: Option<Credential>,
        password: Option<Credential>,
    },
    ShadowTls {
        password: Credential,
        version: u8,
        sni: Option<ProtocolText>,
    },
    AnyTls {
        password: Credential,
        sni: Option<ProtocolText>,
    },
}

impl Protocol {
    /// Returns a safe stable label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Vmess { .. } => "vmess",
            Self::Vless { .. } => "vless",
            Self::Trojan { .. } => "trojan",
            Self::Shadowsocks { .. } => "shadowsocks",
            Self::Hysteria2 { .. } => "hysteria2",
            Self::Tuic { .. } => "tuic",
            Self::WireGuard { .. } => "wireguard",
            Self::Http { .. } => "http",
            Self::Socks5 { .. } => "socks5",
            Self::ShadowTls { .. } => "shadow-tls",
            Self::AnyTls { .. } => "anytls",
        }
    }
}

/// `VMess` payload cipher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmessCipher {
    Auto,
    Aes128Gcm,
    Chacha20Poly1305,
    None,
}
/// Shadowsocks cipher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowsocksCipher {
    Aes128Gcm,
    Aes256Gcm,
    Chacha20IetfPoly1305,
    Xchacha20IetfPoly1305,
    Aes128Cfb,
    Aes256Cfb,
    None,
}
/// Shadowsocks plugin identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowsocksPlugin {
    name: ProtocolText,
    options: ProtocolText,
}

impl ShadowsocksPlugin {
    /// Constructs complete plugin settings.
    pub const fn new(name: ProtocolText, options: ProtocolText) -> Self {
        Self { name, options }
    }
    pub(crate) const fn name(&self) -> &ProtocolText {
        &self.name
    }
    pub(crate) const fn options(&self) -> &ProtocolText {
        &self.options
    }
    /// Plugin display name (`obfs-local`, `v2ray-plugin`, …).
    pub fn label(&self) -> &str {
        self.name.as_str()
    }
    /// Plugin options text (`mode=websocket;tls=false`); empty when the
    /// URI carried a bare `plugin=<name>`.
    pub fn option_text(&self) -> &str {
        self.options.as_str()
    }
}

/// TUIC congestion-control algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CongestionControl {
    Bbr,
    Cubic,
    NewReno,
}
