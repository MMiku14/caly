//! Minimal bounded Mihomo REST control probe (HTTP transport + adapters).
//!
//! Decision #75 (recorded 2026-08-09): this loopback control plane keeps a
//! handwritten, synchronous, std-only HTTP client on purpose — the daemon's
//! actor threads are synchronous, the target never needs TLS/redirects, and
//! the strict byte bounds make the audit surface tiny; the bug class behind
//! the audit note (#23 CRLF/encoding, #24 non-2xx detail, #44 status-line
//! strictness, #45 chunked decoding) is fixed and tested here. reqwest stays
//! scoped to the one place that actually faces the internet — WAN
//! subscription fetching in `caly-config` — and
//! `scripts/check-dependency-discipline.sh` rejects reqwest anywhere else.

use std::{
    fmt::Write as FmtWrite,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use caly_domain::{BoundedText, CapabilitySet, NodeId};

use crate::contract::{
    ConnectionDetail, ConnectionSummary, KernelControl, KernelFailure, KernelFailureKind,
    ProxyGroup,
};

/// Synchronous, bounded HTTP probe for Mihomo's local controller.
pub struct MihomoHttpControl {
    pub(crate) address: String,
    pub(crate) secret: Option<BoundedText<4_096>>,
}

impl MihomoHttpControl {
    /// Creates a controller for a host:port endpoint.
    ///
    /// Both the address and the secret land on raw HTTP header lines
    /// (`Host:` / `Authorization:`), so CR/LF bytes in either would be a
    /// header-injection primitive; they are rejected up front.
    pub fn new(address: String, secret: Option<BoundedText<4_096>>) -> Result<Self, KernelFailure> {
        let header_unsafe = |value: &str| value.bytes().any(|byte| byte == b'\r' || byte == b'\n');
        if address.is_empty() || header_unsafe(&address) {
            return Err(crate::common::failure_with_kind(
                KernelFailureKind::InvalidConfig,
                "Mihomo controller address is empty or carries control characters",
                "configure host:port",
            ));
        }
        if let Some(secret) = &secret
            && header_unsafe(secret.as_str())
        {
            return Err(crate::common::failure_with_kind(
                KernelFailureKind::InvalidConfig,
                "Mihomo controller secret carries CR/LF characters",
                "strip the secret of line breaks",
            ));
        }
        Ok(Self { address, secret })
    }

    pub(crate) fn request(&self, path: &str, timeout: Duration) -> Result<Vec<u8>, KernelFailure> {
        self.request_method("GET", path, "", timeout)
    }

    /// Selects a named Mihomo proxy inside a named proxy group.
    pub fn select_proxy(
        &self,
        group: &str,
        node: &str,
        timeout: Duration,
    ) -> Result<(), KernelFailure> {
        // The group name lands in the URL path (percent-encode it, the same
        // discipline probe_delay applies) and the node name in a JSON body
        // (let serde_json do the escaping — hand-rolled replace chains miss
        // control characters).
        let encoded_group =
            percent_encoding::utf8_percent_encode(group, percent_encoding::NON_ALPHANUMERIC);
        let body = serde_json::json!({ "name": node }).to_string();
        self.request_method("PUT", &format!("/proxies/{encoded_group}"), &body, timeout)
            .map(|_| ())
    }

    /// Closes all active Mihomo connections.
    pub fn close_all_connections(&self, timeout: Duration) -> Result<(), KernelFailure> {
        self.request_method("DELETE", "/connections", "", timeout)
            .map(|_| ())
    }

    /// Sets the Mihomo routing mode via `PATCH /configs`.
    ///
    /// `mode` is one of `rule`, `global`, or `direct`; it is validated against
    /// the Clash-compatible set before being sent.
    pub fn set_mode(&self, mode: &str, timeout: Duration) -> Result<(), KernelFailure> {
        if !matches!(mode, "rule" | "global" | "direct") {
            return Err(crate::common::failure_with_kind(
                KernelFailureKind::InvalidConfig,
                "unsupported Mihomo routing mode",
                "use rule, global, or direct",
            ));
        }
        let body = format!("{{\"mode\":\"{mode}\"}}");
        self.request_method("PATCH", "/configs", &body, timeout)
            .map(|_| ())
    }

    pub(crate) fn request_method(
        &self,
        method: &str,
        path: &str,
        body: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, KernelFailure> {
        let address = self.resolve_address()?;
        let mut stream = Self::connect(address, timeout)?;
        self.write_request(&mut stream, method, path, body)?;
        Self::read_response(&mut stream, timeout)
    }
}

impl MihomoHttpControl {
    /// Resolves the controller `host:port` into a socket address.
    pub(crate) fn resolve_address(&self) -> Result<std::net::SocketAddr, KernelFailure> {
        self.address
            .to_socket_addrs()
            .map_err(|error| {
                crate::common::failure_with_kind(
                    KernelFailureKind::ApiUnavailable,
                    &format!("cannot resolve Mihomo controller: {error}"),
                    "check the controller address",
                )
            })?
            .next()
            .ok_or_else(|| {
                crate::common::failure_with_kind(
                    KernelFailureKind::ApiUnavailable,
                    "Mihomo controller resolved to no address",
                    "check the controller address",
                )
            })
    }

    /// Connects to the controller with bounded read/write timeouts.
    pub(crate) fn connect(
        address: std::net::SocketAddr,
        timeout: Duration,
    ) -> Result<TcpStream, KernelFailure> {
        let stream = TcpStream::connect_timeout(&address, timeout).map_err(|error| {
            crate::common::failure_with_kind(
                KernelFailureKind::ApiUnavailable,
                &format!("cannot connect to Mihomo controller: {error}"),
                "start Mihomo and verify its controller",
            )
        })?;
        stream.set_read_timeout(Some(timeout)).map_err(|error| {
            crate::common::failure_with_kind(
                KernelFailureKind::ApiUnavailable,
                &format!("cannot set controller read timeout: {error}"),
                "inspect process permissions",
            )
        })?;
        stream.set_write_timeout(Some(timeout)).map_err(|error| {
            crate::common::failure_with_kind(
                KernelFailureKind::ApiUnavailable,
                &format!("cannot set controller write timeout: {error}"),
                "inspect process permissions",
            )
        })?;
        Ok(stream)
    }

    /// Writes an HTTP/1.1 request with optional bearer auth and JSON body.
    pub(crate) fn write_request(
        &self,
        stream: &mut TcpStream,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<(), KernelFailure> {
        // Every dynamic value here is already injection-screened: address and
        // secret are CR/LF-rejected in `new`, `path` segments are
        // percent-encoded by the callers, `body` is serde_json output.
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.address
        );
        if let Some(secret) = &self.secret {
            let _ = write!(request, "Authorization: Bearer {}\r\n", secret.as_str());
        }
        if !body.is_empty() {
            let _ = write!(request, "Content-Type: application/json\r\n");
            let _ = write!(request, "Content-Length: {}\r\n", body.len());
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes()).map_err(|error| {
            crate::common::failure_with_kind(
                KernelFailureKind::ApiUnavailable,
                &format!("cannot write Mihomo request: {error}"),
                "inspect controller permissions",
            )
        })
    }

    /// Reads a bounded response and rejects non-2xx status lines.
    pub(crate) fn read_response(
        stream: &mut TcpStream,
        timeout: Duration,
    ) -> Result<Vec<u8>, KernelFailure> {
        // Audit #84: the socket read/write timeouts configured at connect are
        // per-syscall; a stuck or hostile controller could dribble 1 byte per
        // (timeout − ε) and pin the control thread forever. Enforce an
        // overall deadline across the whole response read.
        let deadline = Instant::now() + timeout;
        let mut body = Vec::new();
        // A 267-node subscription yields a ~100 KB `/proxies` document; the
        // old 64 KB cap truncated it into invalid JSON. 32 MB matches the
        // subscription body ceiling while still bounding a hostile kernel.
        let mut limited = stream.take(32 * 1_024 * 1_024);
        let mut chunk = [0u8; 16 * 1_024];
        loop {
            if Instant::now() >= deadline {
                return Err(crate::common::failure_with_kind(
                    KernelFailureKind::DeadlineExceeded,
                    "Mihomo controller response exceeded the overall read deadline",
                    "inspect Mihomo controller health",
                ));
            }
            match limited.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => body.extend_from_slice(&chunk[..count]),
                Err(error) => {
                    return Err(crate::common::failure_with_kind(
                        KernelFailureKind::ApiUnavailable,
                        &format!("cannot read Mihomo response: {error}"),
                        "inspect Mihomo controller health",
                    ));
                }
            }
        }
        // Audit #85: the status line always precedes the first CRLF and is
        // ASCII by protocol — parse it from the raw bytes up to the first
        // `\r`, never from a 4 KiB `from_utf8` window that fails wholesale
        // when the cut lands inside a multi-byte character.
        let head_end = body
            .iter()
            .position(|byte| *byte == b'\r')
            .unwrap_or(body.len());
        let head = core::str::from_utf8(&body[..head_end]).unwrap_or("");
        let Some(status) = status_code(head) else {
            return Err(crate::common::failure_with_kind(
                KernelFailureKind::DecodeRejected,
                "Mihomo controller response carries no HTTP status line",
                "inspect Mihomo controller health",
            ));
        };
        if !(200..300).contains(&status) {
            // Surface the status and a bounded body excerpt so the operator
            // sees *why* the controller rejected the call instead of a
            // context-free "non-success".
            let excerpt = head_excerpt(&body, 200);
            return Err(crate::common::failure_with_kind(
                KernelFailureKind::ApiUnavailable,
                &format!("Mihomo controller returned HTTP {status}: {excerpt}"),
                "inspect Mihomo API configuration",
            ));
        }
        Ok(body)
    }
}

/// Extracts the three-digit HTTP status code from a raw response head
/// (e.g. `HTTP/1.1 404 Not Found` -> `Some(404)`). Shared by the strict
/// `read_response` gate and the probe adapters in `api.rs`.
pub(crate) fn status_code(text: &str) -> Option<u16> {
    let head = text.split_once("\r\n").map_or(text, |(head, _)| head);
    let mut parts = head.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Returns a short, single-line excerpt of the response body for error
/// messages (control bytes replaced so a hostile kernel cannot smuggle
/// terminal escapes into logs).
fn head_excerpt(body: &[u8], limit: usize) -> String {
    let text = core::str::from_utf8(body).unwrap_or("<non-UTF-8 body>");
    let body_start = text.split_once("\r\n\r\n").map_or(text, |(_, body)| body);
    let mut excerpt: String = body_start
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(limit)
        .collect();
    if body_start.chars().count() > limit {
        excerpt.push('…');
    }
    excerpt.trim().to_owned()
}

impl KernelControl for MihomoHttpControl {
    fn capabilities(&self) -> CapabilitySet {
        mihomo_capabilities()
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.health_check(Duration::from_millis(100)) {
                Ok(()) => return Ok(()),
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn select_proxy(&mut self, _node: NodeId, _timeout: Duration) -> Result<(), KernelFailure> {
        Err(crate::common::failure_with_kind(
            KernelFailureKind::Unsupported,
            "Mihomo proxy selection is not connected",
            "use the application actor adapter",
        ))
    }

    fn health_check(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        self.request("/version", timeout).map(|_| ())
    }

    fn proxy_groups(&mut self, timeout: Duration) -> Result<Vec<ProxyGroup>, KernelFailure> {
        self.list_proxy_groups(timeout)
    }

    fn proxy_names(&mut self, timeout: Duration) -> Result<Vec<String>, KernelFailure> {
        self.list_proxy_names(timeout)
    }

    fn connections(&mut self, timeout: Duration) -> Result<ConnectionSummary, KernelFailure> {
        self.connection_summary(timeout)
    }

    fn connection_details(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<ConnectionDetail>, KernelFailure> {
        self.connection_details_impl(timeout)
    }

    fn traffic(&mut self, timeout: Duration) -> Result<(u64, u64), KernelFailure> {
        self.traffic_bytes(timeout)
    }

    fn reload_config(&mut self, config: &[u8], timeout: Duration) -> Result<(), KernelFailure> {
        let body = String::from_utf8_lossy(config);
        self.request_method("PUT", "/configs?force=true", &body, timeout)?;
        Ok(())
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

/// Parses the JSON body from an HTTP response (strips headers at the first
/// blank line, decodes a chunked transfer body, then parses the remainder).
pub(crate) fn response_json(body: &[u8]) -> Result<serde_json::Value, KernelFailure> {
    let text = core::str::from_utf8(body).map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::DecodeRejected,
            "Mihomo response is not UTF-8",
            "inspect controller output",
        )
    })?;
    let json_text = match text.split_once("\r\n\r\n") {
        Some((headers, body)) => {
            if headers
                .to_ascii_lowercase()
                .contains("transfer-encoding: chunked")
            {
                decode_chunked_body(body.as_bytes()).map_err(|()| {
                    crate::common::failure_with_kind(
                        KernelFailureKind::DecodeRejected,
                        "Mihomo response uses an invalid chunked encoding",
                        "inspect controller output",
                    )
                })?
            } else {
                body.to_owned()
            }
        }
        None => text.to_owned(),
    };
    serde_json::from_str(&json_text).map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::DecodeRejected,
            "Mihomo API returned invalid JSON",
            "inspect controller output",
        )
    })
}

/// Decodes an HTTP/1.1 chunked transfer body (hex size lines + data blocks).
/// Trailer headers after the zero-size terminator are intentionally ignored
/// (Mihomo never emits them; the JSON body is complete at that point).
fn decode_chunked_body(body: &[u8]) -> Result<String, ()> {
    fn find_crlf(from: &[u8]) -> Option<usize> {
        from.windows(2).position(|pair| pair == b"\r\n")
    }
    let mut decoded = Vec::new();
    let mut rest = body;
    loop {
        let line_end = find_crlf(rest).ok_or(())?;
        let size_line = core::str::from_utf8(&rest[..line_end]).map_err(|_| ())?;
        // Chunk extensions (`;foo=bar`) are legal after the size; ignore them.
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| ())?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return String::from_utf8(decoded).map_err(|_| ());
        }
        // Audit #83: `size + 2` overflowed for a hostile `usize::MAX` chunk
        // size (debug panic / release wraparound), leaving `&rest[..size]`
        // unbounded. Compute the requirement with checked arithmetic so the
        // slice below can never index out of range.
        let Some(need) = size.checked_add(2) else {
            return Err(());
        };
        if rest.len() < need {
            return Err(());
        }
        // Each non-final chunk must be followed by CRLF per RFC 7230.
        if &rest[size..need] != b"\r\n" {
            return Err(());
        }
        decoded.extend_from_slice(&rest[..size]);
        rest = &rest[need..];
    }
}

/// Mihomo's Clash-compatible API supports proxy groups, selection, connections,
/// traffic, rules and URL-testing. Reflect that as a truthful capability set.
pub(crate) fn mihomo_capabilities() -> CapabilitySet {
    use caly_domain::{
        BoundedVec, Capability, CapabilityStatus, ConfiguredSupport, RuntimeAvailability,
    };
    let supported = |capability| {
        CapabilityStatus::new(
            capability,
            ConfiguredSupport::Supported,
            RuntimeAvailability::NotRequired,
            None,
        )
    };
    let statuses = vec![
        supported(Capability::DnsConfiguration),
        supported(Capability::RuntimeModeSwitch),
        supported(Capability::ProxyGroups),
        supported(Capability::ProxySelection),
        supported(Capability::Connections),
        supported(Capability::ConnectionClose),
        supported(Capability::Traffic),
        supported(Capability::UrlTest),
    ];
    // The hardcoded `statuses` vector always fits the bounded capacity and
    // contains no duplicate `Capability` values. The previous
    // `unwrap_or_else(|_| process::abort)` form was a process-kill fallback
    // for a path that was never reachable; switching to the
    // `from_vec_truncated` + `from_bounded_dedup` infallible constructors
    // keeps the same behaviour for the well-formed call sites while
    // replacing the abort with a graceful truncation/dedup that survives
    // any future contributor who widens the literal past the bound.
    let values = BoundedVec::from_vec_truncated(statuses);
    CapabilitySet::from_bounded_dedup(values)
}

#[cfg(test)]
mod tests;
