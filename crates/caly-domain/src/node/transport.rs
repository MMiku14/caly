//! Pluggable transport and TLS identity values.

use crate::{BoundedText, BoundedVec, SecretText};

/// Maximum path/service/host-label text length.
pub const TRANSPORT_TEXT_MAX_BYTES: usize = 1_024;
/// Maximum H2 hosts or ALPN entries.
pub const MAX_TRANSPORT_LABELS: usize = 16;
/// Maximum Reality key/short-id secret length.
pub const REALITY_SECRET_MAX_BYTES: usize = 512;

/// Validated transport text.
pub type TransportText = BoundedText<TRANSPORT_TEXT_MAX_BYTES>;
/// Bounded transport text list.
pub type TransportTextList = BoundedVec<TransportText, MAX_TRANSPORT_LABELS>;

/// WebSocket early-data settings (Xray `ed`/`eh` subscription parameters).
/// Carried end-to-end so renderers that support it (sing-box:
/// `max_early_data` / `early_data_header_name`) can emit it instead of
/// silently dropping the handshake accelerator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebSocketEarlyData {
    header_name: TransportText,
    max_bytes: u32,
}

impl WebSocketEarlyData {
    /// Xray convention: `eh` names the header; `ed` is the byte budget. A
    /// `max_bytes` of 0 disables early data entirely (callers should treat it
    /// as "absent"), so the value stored here is always non-zero.
    #[must_use]
    pub fn new(header_name: TransportText, max_bytes: u32) -> Self {
        Self {
            header_name,
            max_bytes,
        }
    }

    /// Header carrying the early payload (default in the ecosystem:
    /// `Sec-WebSocket-Protocol`).
    #[must_use]
    pub fn header_name(&self) -> &TransportText {
        &self.header_name
    }

    /// Maximum early payload bytes advertised to the server.
    #[must_use]
    pub fn max_bytes(&self) -> u32 {
        self.max_bytes
    }
}

/// Wire transport used by protocols that support pluggable framing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transport {
    /// Plain TCP framing.
    Tcp,
    /// WebSocket framing.
    WebSocket {
        path: TransportText,
        host: Option<TransportText>,
        early_data: Option<WebSocketEarlyData>,
    },
    /// gRPC framing.
    Grpc { service_name: TransportText },
    /// HTTP/2 framing.
    Http2 {
        path: TransportText,
        hosts: TransportTextList,
    },
    /// QUIC framing.
    Quic,
}

/// TLS and Reality settings that affect dialing identity.
#[derive(Debug)]
pub struct TlsConfig {
    sni: Option<TransportText>,
    alpn: TransportTextList,
    allow_insecure: bool,
    fingerprint: Option<TransportText>,
    reality: Option<RealityConfig>,
}

impl TlsConfig {
    /// Constructs complete TLS settings.
    pub const fn new(
        sni: Option<TransportText>,
        alpn: TransportTextList,
        allow_insecure: bool,
        fingerprint: Option<TransportText>,
        reality: Option<RealityConfig>,
    ) -> Self {
        Self {
            sni,
            alpn,
            allow_insecure,
            fingerprint,
            reality,
        }
    }

    /// Returns the TLS server-name indicator, if configured.
    pub const fn sni(&self) -> Option<&TransportText> {
        self.sni.as_ref()
    }
    /// Returns the ALPN label list.
    pub const fn alpn(&self) -> &TransportTextList {
        &self.alpn
    }
    /// Returns whether certificate verification is skipped.
    pub const fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }
    /// Returns the TLS client-fingerprint label.
    pub const fn fingerprint(&self) -> Option<&TransportText> {
        self.fingerprint.as_ref()
    }
    /// Returns the Reality handshake identity, when in Reality mode.
    pub const fn reality(&self) -> Option<&RealityConfig> {
        self.reality.as_ref()
    }
}

/// Reality handshake identity.
#[derive(Debug)]
pub struct RealityConfig {
    public_key: SecretText<REALITY_SECRET_MAX_BYTES>,
    /// Reality short id is optional; many subscriptions omit it.
    short_id: Option<SecretText<REALITY_SECRET_MAX_BYTES>>,
    spider_x: Option<TransportText>,
}

impl RealityConfig {
    /// Constructs Reality settings without exposing key material.
    pub const fn new(
        public_key: SecretText<REALITY_SECRET_MAX_BYTES>,
        short_id: Option<SecretText<REALITY_SECRET_MAX_BYTES>>,
        spider_x: Option<TransportText>,
    ) -> Self {
        Self {
            public_key,
            short_id,
            spider_x,
        }
    }

    /// Returns the Reality public key (secret; expose only via `with_exposed`).
    pub const fn public_key(&self) -> &SecretText<REALITY_SECRET_MAX_BYTES> {
        &self.public_key
    }
    /// Returns the optional Reality short id (secret; expose only via `with_exposed`).
    pub const fn short_id(&self) -> Option<&SecretText<REALITY_SECRET_MAX_BYTES>> {
        self.short_id.as_ref()
    }
    /// Returns the Reality spider X (optional).
    pub const fn spider_x(&self) -> Option<&TransportText> {
        self.spider_x.as_ref()
    }
}
