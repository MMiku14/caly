//! Kernel integration split between specification and runtime control.

use std::time::Duration;

use caly_domain::{BoundedText, CapabilitySet, CoreKind, NodeId};
use caly_platform::{PlatformFailure, process::SpawnSpec};

/// Immutable rendered-config reference; path ownership remains Infrastructure.
pub struct RenderedConfigRef {
    pub generation: u64,
    pub path: std::path::PathBuf,
}

/// Builds a process request but never owns the resulting process tree.
pub trait SpawnSpecFactory {
    fn core_kind(&self) -> CoreKind;
    fn build_spawn_spec(&self, config: &RenderedConfigRef) -> Result<SpawnSpec, KernelFailure>;
    fn build_validation_spec(&self, config: &RenderedConfigRef)
    -> Result<SpawnSpec, KernelFailure>;
}

/// A proxy group: its kind, the currently selected member, and its members.
///
/// `members` carries the kernel-side membership (`all` from mihomo
/// `/proxies`) — the W3b enrichment that lets the CLI show group rows and
/// the GROUP column on online node lists without re-deriving membership
/// from the declared config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyGroup {
    pub name: String,
    /// Kernel group kind: `Selector`, `URLTest`, `Fallback`, `LoadBalance`.
    pub kind: String,
    pub selected: Option<String>,
    pub members: Vec<String>,
}

/// A live connection summary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConnectionSummary {
    /// Number of active (upload+download bytes > 0) connections.
    pub active: u32,
    /// Total bytes downloaded by active connections.
    pub download_bytes: u64,
    /// Total bytes uploaded by active connections.
    pub upload_bytes: u64,
}

/// One live connection with the routing metadata needed to render the
/// traffic processing flow (`caly flow`): which rule matched, which group
/// chain selected which node. Both kernels speak the Clash-compatible
/// `/connections` shape, so this is kernel-agnostic.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct ConnectionDetail {
    /// Kernel connection id (stable across polls for the connection's life).
    pub id: String,
    /// Display host (metadata `host`, falling back to the destination IP).
    pub host: String,
    pub destination_port: u16,
    /// `tcp` | `udp`.
    pub network: String,
    /// Sniffed protocol (`http` | `tls` | `quic` | …).
    pub protocol_type: String,
    /// Inbound name (`mixed`, `tun`, …) when the kernel reports it.
    pub inbound_name: String,
    /// Local process path for the owning connection, when reported.
    pub process_path: String,
    /// Matched rule kind (`DomainSuffix`, `MATCH`, …).
    pub rule: String,
    pub rule_payload: String,
    /// Final outbound name/tag the connection is actually dialed through.
    pub outbound: String,
    /// Group chain, innermost last (e.g. `["PROXY", "HK-01"]`).
    pub chain: Vec<String>,
    pub upload_bytes: u64,
    pub download_bytes: u64,
}

/// Runtime API control only; it does not own a child process.
pub trait KernelControl {
    fn capabilities(&self) -> CapabilitySet;
    fn wait_ready(&mut self, timeout: Duration) -> Result<(), KernelFailure>;
    fn select_proxy(&mut self, node: NodeId, timeout: Duration) -> Result<(), KernelFailure>;
    fn health_check(&mut self, timeout: Duration) -> Result<(), KernelFailure>;

    /// Lists configured proxy groups and their current selection.
    fn proxy_groups(&mut self, timeout: Duration) -> Result<Vec<ProxyGroup>, KernelFailure> {
        let _ = timeout;
        Err(unsupported("proxy groups"))
    }

    /// Lists the names of real (dialable) proxies from `GET /proxies`,
    /// excluding selector/special groups — the input set for a full latency
    /// sweep (`delay --all`).
    fn proxy_names(&mut self, timeout: Duration) -> Result<Vec<String>, KernelFailure> {
        let _ = timeout;
        Err(unsupported("proxy names"))
    }

    /// Returns a live connection summary.
    fn connections(&mut self, timeout: Duration) -> Result<ConnectionSummary, KernelFailure> {
        let _ = timeout;
        Err(unsupported("connections"))
    }

    /// Returns every live connection with its routing metadata (the
    /// traffic processing flow behind `caly flow`). Implementations without
    /// detail support degrade to an empty list.
    fn connection_details(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<ConnectionDetail>, KernelFailure> {
        let _ = timeout;
        // Err (not an empty list) on unsupported kernels: the CLI must
        // distinguish "no live connections" from "this kernel exposes no
        // connection detail surface" (2026-08-12 agent audit).
        Err(unsupported("connection details"))
    }

    /// Hot-reloads the kernel config without restarting the process
    /// (刀 5, 2026-08-12 pipeline design): the published config text is
    /// pushed via the Clash-compatible `PUT /configs?force=true` endpoint,
    /// so existing connections stay up. Implementations without a reload
    /// surface return an error and the caller falls back to a restart.
    fn reload_config(&mut self, config: &[u8], timeout: Duration) -> Result<(), KernelFailure> {
        let _ = (config, timeout);
        Err(unsupported("config reload"))
    }

    /// Returns the current per-second download/upload byte rates (the
    /// `down`/`up` snapshot of the Clash `/traffic` stream). Implementations
    /// without per-second fields degrade to their cumulative totals.
    fn traffic(&mut self, timeout: Duration) -> Result<(u64, u64), KernelFailure> {
        let _ = timeout;
        Err(unsupported("traffic"))
    }

    /// Tests the latency of a named proxy via `GET /proxies/{name}/delay`
    /// against the default probe target ([`DEFAULT_DELAY_URL`]).
    ///
    /// Returns `Ok(Some(ms))` when the probe succeeds, `Ok(None)` when the
    /// proxy is reachable at the API level but the latency probe failed (e.g.
    /// the target host is unreachable), and `Err` only on a transport-level
    /// failure talking to the controller.
    fn test_delay(&mut self, name: &str, timeout: Duration) -> Result<Option<u32>, KernelFailure> {
        self.test_delay_url(name, DEFAULT_DELAY_URL, timeout)
    }

    /// Tests the latency of a named proxy against a caller-supplied target
    /// URL. Kernels that cannot honor a custom probe target fall back to the
    /// default-target behaviour.
    fn test_delay_url(
        &mut self,
        name: &str,
        _url: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, KernelFailure> {
        self.test_delay(name, timeout)
    }
}

/// Default latency-probe target (`generate_204`), reachable through most
/// proxies; users may override it per command.
pub const DEFAULT_DELAY_URL: &str = "https://www.google.com/generate_204";

/// Builds a consistent "API not provided by this kernel" failure.
pub fn unsupported(api: &str) -> KernelFailure {
    KernelFailure {
        kind: KernelFailureKind::Unsupported,
        message: BoundedText::from_nonempty_clamped(
            format!("this kernel does not expose {api}"),
            "unsupported",
        ),
        suggested_action: BoundedText::from_nonempty_clamped(
            "enable the kernel's Clash-compatible API".to_owned(),
            "inspect the kernel",
        ),
        platform: None,
    }
}

/// Safe kernel failure category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelFailure {
    pub kind: KernelFailureKind,
    pub message: BoundedText<1_024>,
    pub suggested_action: BoundedText<512>,
    pub platform: Option<PlatformFailure>,
}

impl core::fmt::Display for KernelFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{} ({})", self.message, self.suggested_action)
    }
}

impl KernelFailure {
    /// Constructs a kernel failure, UTF-8-safely clamping long/empty text to the
    /// bounded fields. This is the single shared constructor so the per-module
    /// `failure` helpers collapse onto one responsibility and never abort the
    /// daemon on an oversized dynamic message.
    pub fn new(kind: KernelFailureKind, message: &str, suggested_action: &str) -> Self {
        Self {
            kind,
            message: BoundedText::from_nonempty_clamped(
                clamp(message, 1_024, "kernel failure"),
                "kernel failure",
            ),
            suggested_action: BoundedText::from_nonempty_clamped(
                clamp(suggested_action, 512, "inspect kernel configuration"),
                "inspect kernel configuration",
            ),
            platform: None,
        }
    }
}

/// Clamps text to `max` bytes without splitting UTF-8; falls back for empty input.
fn clamp(value: &str, max: usize, fallback: &str) -> String {
    if value.is_empty() {
        return fallback.to_owned();
    }
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelFailureKind {
    Unsupported,
    InvalidConfig,
    ApiUnavailable,
    DeadlineExceeded,
    ProcessLifecycle,
    DecodeRejected,
}
