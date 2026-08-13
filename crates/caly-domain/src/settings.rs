//! Core-agnostic runtime settings resolved from the application configuration.

/// Per-core Clash-compatible controller endpoints.
///
/// Resolved with precedence: config file > `CALY_MIHOMO_CONTROLLER` /
/// `CALY_SINGBOX_CONTROLLER` environment > built-in loopback defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Controllers {
    /// Mihomo `external-controller` address (host:port).
    pub mihomo: String,
    /// sing-box `external_controller` address (host:port).
    pub sing_box: String,
}

impl Controllers {
    /// The built-in loopback controller endpoints.
    pub fn defaults() -> Self {
        Self {
            mihomo: "127.0.0.1:9090".to_owned(),
            sing_box: "127.0.0.1:9091".to_owned(),
        }
    }
}
