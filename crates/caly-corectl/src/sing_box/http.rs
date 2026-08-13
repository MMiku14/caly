//! sing-box Clash-compatible API probe.
//!
//! sing-box ships an optional Clash-compatible controller (`external_controller`
//! in the `experimental` block). Its REST surface is a subset of Mihomo's, so we
//! delegate to the shared `MihomoHttpControl` transport and adapters for proxy
//! groups, connections, traffic and selection. A shared controller secret is
//! carried through to authenticate every request.

use std::time::Duration;

use crate::{
    contract::{ConnectionDetail, ConnectionSummary, KernelControl, KernelFailure, ProxyGroup},
    mihomo::MihomoHttpControl,
};
use caly_domain::{BoundedText, CapabilitySet, NodeId};

/// sing-box's Clash-compatible API is partial: groups and proxy listing work,
/// but connection/traffic streaming and full URL-test semantics may be limited.
fn sing_box_capabilities() -> CapabilitySet {
    use crate::common::{capability_set, capability_status};
    use caly_domain::{Capability, ConfiguredSupport};
    let statuses = vec![
        capability_status(Capability::DnsConfiguration, ConfiguredSupport::Supported),
        capability_status(Capability::RuntimeModeSwitch, ConfiguredSupport::Supported),
        capability_status(Capability::ProxyGroups, ConfiguredSupport::Supported),
        capability_status(Capability::ProxySelection, ConfiguredSupport::Supported),
        capability_status(Capability::UrlTest, ConfiguredSupport::Supported),
        capability_status(Capability::ConnectionClose, ConfiguredSupport::Supported),
        capability_status(Capability::Connections, ConfiguredSupport::Partial),
        capability_status(Capability::Traffic, ConfiguredSupport::Partial),
    ];
    capability_set(statuses)
}

/// sing-box's optional Clash-compatible controller.
pub struct SingBoxHttpControl {
    inner: MihomoHttpControl,
}

impl SingBoxHttpControl {
    /// Creates a controller for a host:port endpoint with an optional shared
    /// auth secret.
    pub fn new(address: String, secret: Option<BoundedText<4_096>>) -> Result<Self, KernelFailure> {
        Ok(Self {
            inner: MihomoHttpControl::new(address, secret)?,
        })
    }

    /// Selects a named sing-box proxy inside a named proxy group (Clash API).
    pub fn select_proxy(
        &self,
        group: &str,
        node: &str,
        timeout: Duration,
    ) -> Result<(), KernelFailure> {
        self.inner.select_proxy(group, node, timeout)
    }

    /// Closes all active sing-box connections (Clash API).
    pub fn close_all_connections(&self, timeout: Duration) -> Result<(), KernelFailure> {
        self.inner.close_all_connections(timeout)
    }

    /// Sets the sing-box routing mode via `PATCH /configs`.
    ///
    /// sing-box exposes mode switching only when the rendered config contains
    /// the `clash_mode` route rules (`Global` → `GLOBAL`, `Direct` → `direct`);
    /// otherwise `mode-list` is `["rule"]` and the request is a no-op.
    pub fn set_mode(&self, mode: &str, timeout: Duration) -> Result<(), KernelFailure> {
        self.inner.set_mode(mode, timeout)
    }

    /// Tests the latency of a named proxy via `GET /proxies/{name}/delay`
    /// (same endpoint and semantics as Mihomo's Clash API).
    pub fn probe_delay(
        &self,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, KernelFailure> {
        self.inner.probe_delay(name, url, timeout)
    }
}

impl KernelControl for SingBoxHttpControl {
    fn capabilities(&self) -> CapabilitySet {
        sing_box_capabilities()
    }
    fn wait_ready(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        self.inner.wait_ready(timeout)
    }
    fn health_check(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        self.inner.health_check(timeout)
    }
    fn select_proxy(&mut self, _node: NodeId, _timeout: Duration) -> Result<(), KernelFailure> {
        // The two messages are static literals well within the 1 KiB bound,
        // so `from_nonempty_clamped` is infallible here. The previous
        // `unwrap_or_else(|_| process::abort)` form was a process-kill
        // fallback for an unreachable path; the infallible constructor
        // keeps the same behaviour for the well-formed call sites and
        // surfaces a stable fallback (`_`) for any future refactor that
        // accidentally widens the input.
        Err(KernelFailure {
            kind: crate::contract::KernelFailureKind::Unsupported,
            message: BoundedText::from_nonempty_clamped(
                "sing-box proxy selection requires group mapping".to_owned(),
                "unsupported",
            ),
            suggested_action: BoundedText::from_nonempty_clamped(
                "select via the application actor with group mapping".to_owned(),
                "use application actor",
            ),
            platform: None,
        })
    }
    fn proxy_groups(&mut self, timeout: Duration) -> Result<Vec<ProxyGroup>, KernelFailure> {
        self.inner.list_proxy_groups(timeout)
    }

    fn proxy_names(&mut self, timeout: Duration) -> Result<Vec<String>, KernelFailure> {
        self.inner.list_proxy_names(timeout)
    }
    fn connections(&mut self, timeout: Duration) -> Result<ConnectionSummary, KernelFailure> {
        self.inner.connection_summary(timeout)
    }
    fn connection_details(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<ConnectionDetail>, KernelFailure> {
        // sing-box speaks the Clash-compatible control API through the same
        // mihomo client, so the detail parsing is shared.
        self.inner.connection_details(timeout)
    }
    fn traffic(&mut self, timeout: Duration) -> Result<(u64, u64), KernelFailure> {
        self.inner.traffic_bytes(timeout)
    }

    fn reload_config(&mut self, config: &[u8], timeout: Duration) -> Result<(), KernelFailure> {
        // sing-box's Clash-compat surface has no config-write endpoint, so
        // this fails (404) and the caller falls back to a restart — a
        // process restart is the only reload path sing-box offers.
        self.inner.reload_config(config, timeout)
    }

    fn test_delay(&mut self, name: &str, timeout: Duration) -> Result<Option<u32>, KernelFailure> {
        self.probe_delay(name, crate::contract::DEFAULT_DELAY_URL, timeout)
    }

    fn test_delay_url(
        &mut self,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, KernelFailure> {
        self.probe_delay(name, url, timeout)
    }
}
