//! Protocol label maps shared by the mihomo YAML and sing-box JSON
//! renderers. Both kernels accept the same wire labels; one source of
//! truth keeps the two renderers from drifting (golden tests pin output).

use caly_domain::{CongestionControl, ShadowsocksCipher, VmessCipher};

/// Maps a Domain VMess cipher to its kernel cipher label.
pub(crate) fn vmess_cipher(cipher: VmessCipher) -> &'static str {
    match cipher {
        VmessCipher::Auto => "auto",
        VmessCipher::Aes128Gcm => "aes-128-gcm",
        VmessCipher::Chacha20Poly1305 => "chacha20-poly1305",
        VmessCipher::None => "none",
    }
}

/// Maps a Domain congestion-control choice to its kernel label.
pub(crate) fn congestion_label(congestion: CongestionControl) -> &'static str {
    match congestion {
        CongestionControl::Bbr => "bbr",
        CongestionControl::Cubic => "cubic",
        CongestionControl::NewReno => "new_reno",
    }
}

/// Maps a Shadowsocks cipher to its kernel label. Both kernels accept the
/// legacy `aes-128-cfb`/`aes-256-cfb` labels, but sing-box cannot dial
/// them; callers filter via [`ss_cipher_supported`].
pub(crate) fn ss_cipher_label(cipher: ShadowsocksCipher) -> &'static str {
    match cipher {
        ShadowsocksCipher::Aes128Gcm => "aes-128-gcm",
        ShadowsocksCipher::Aes256Gcm => "aes-256-gcm",
        ShadowsocksCipher::Chacha20IetfPoly1305 => "chacha20-ietf-poly1305",
        ShadowsocksCipher::Xchacha20IetfPoly1305 => "xchacha20-ietf-poly1305",
        ShadowsocksCipher::Aes128Cfb => "aes-128-cfb",
        ShadowsocksCipher::Aes256Cfb => "aes-256-cfb",
        ShadowsocksCipher::None => "none",
    }
}

/// Whether sing-box can dial this Shadowsocks cipher (the legacy CFB modes
/// are rejected by sing-box's shadowsocks implementation).
pub(crate) fn ss_cipher_supported(cipher: ShadowsocksCipher) -> bool {
    !matches!(
        cipher,
        ShadowsocksCipher::Aes128Cfb | ShadowsocksCipher::Aes256Cfb
    )
}
