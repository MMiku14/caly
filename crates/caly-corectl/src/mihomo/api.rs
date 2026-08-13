//! Rich Mihomo Clash-compatible API adapters (proxy groups, connections,
//! traffic). Kept separate from the minimal HTTP transport in `http.rs` so
//! each file stays within the project's line limits.

use std::{io::Read, time::Duration};

use crate::contract::{ConnectionDetail, ConnectionSummary, KernelFailure, ProxyGroup};

use super::http::MihomoHttpControl;

impl MihomoHttpControl {
    /// Lists proxy groups and their current selection from `GET /proxies`.
    pub fn list_proxy_groups(&self, timeout: Duration) -> Result<Vec<ProxyGroup>, KernelFailure> {
        let body = self.request("/proxies", timeout)?;
        let json = super::http::response_json(&body)?;
        Ok(parse_proxy_groups(&json))
    }

    /// Summarizes active connections from `GET /connections`.
    pub fn connection_summary(
        &self,
        timeout: Duration,
    ) -> Result<ConnectionSummary, KernelFailure> {
        let body = self.request("/connections", timeout)?;
        let json = super::http::response_json(&body)?;
        let Some(connections) = json
            .get("connections")
            .and_then(serde_json::Value::as_array)
        else {
            return Ok(ConnectionSummary {
                active: 0,
                download_bytes: 0,
                upload_bytes: 0,
            });
        };
        let mut download = 0_u64;
        let mut upload = 0_u64;
        for connection in connections {
            upload = upload.saturating_add(numeric(connection, "upload"));
            download = download.saturating_add(numeric(connection, "download"));
        }
        // Every entry the kernel lists under `/connections` is a *live*
        // connection — filtering by `download > 0` used to drop freshly
        // established or upload-only sessions from the active count.
        let active = u32::try_from(connections.len()).unwrap_or(u32::MAX);
        Ok(ConnectionSummary {
            active,
            download_bytes: download,
            upload_bytes: upload,
        })
    }

    /// Returns every live connection with routing metadata, parsed from the
    /// same `/connections` payload as [`Self::connection_summary`]. Fields
    /// live under `metadata` in the Clash-compatible shape (rule/outbound
    /// also appear at the top level — metadata wins, top level is the
    /// fallback so both kernels render consistently).
    pub fn connection_details_impl(
        &self,
        timeout: Duration,
    ) -> Result<Vec<ConnectionDetail>, KernelFailure> {
        let body = self.request("/connections", timeout)?;
        let json = super::http::response_json(&body)?;
        Ok(parse_connections_json(&json))
    }

    /// Returns the current per-second (download, upload) byte rates from
    /// `GET /traffic`.
    ///
    /// The endpoint is an SSE stream; frame semantics differ by kernel
    /// (W3a B-1, w3a-debug-traffic.md):
    ///
    /// - Mihomo frames carry cumulative totals (`upTotal`/`downTotal`)
    ///   plus per-second rates — the totals are returned as-is.
    /// - sing-box frames are 100 ms window *deltas* with no totals: a
    ///   single first-frame read shows 0 whenever the command runs
    ///   between traffic bursts. The delta frames are accumulated over
    ///   a bounded window (min(timeout, 2 s)) so `caly traffic` reports
    ///   the recent window volume instead of a misleading 0.
    ///
    /// The connection is closed instead of draining to EOF (which would
    /// block forever); each frame read loops until a whole JSON frame
    /// has arrived — a single `read()` is *not* guaranteed to return a
    /// full frame.
    pub fn traffic_bytes(&self, timeout: Duration) -> Result<(u64, u64), KernelFailure> {
        let address = self.resolve_address()?;
        let mut stream = MihomoHttpControl::connect(address, timeout)?;
        self.write_request(&mut stream, "GET", "/traffic", "")?;
        let first = read_first_sse_frame(&mut stream)?;
        // Cumulative totals (Mihomo) win: exact semantics, zero waiting.
        if let Some((download, upload)) = parse_traffic_total(&first) {
            return Ok((download, upload));
        }
        // Delta frames (sing-box): accumulate over a bounded window.
        let window = timeout.min(Duration::from_secs(2));
        let deadline = std::time::Instant::now() + window;
        let mut download = 0_u64;
        let mut upload = 0_u64;
        if let Some((down, up)) = parse_traffic_rate(&first) {
            download += down;
            upload += up;
        }
        while std::time::Instant::now() < deadline {
            match read_first_sse_frame(&mut stream) {
                Ok(frame) => {
                    if let Some((down, up)) = parse_traffic_rate(&frame) {
                        download = download.saturating_add(down);
                        upload = upload.saturating_add(up);
                    }
                }
                // A stalled stream ends the window; the accumulated
                // volume is still a truthful "recent traffic" figure.
                Err(_) => break,
            }
        }
        Ok((download, upload))
    }

    /// Tests the latency of a named proxy via `GET /proxies/{name}/delay`.
    ///
    /// The endpoint probes a fixed `generate_204` URL and returns `{"delay":n}`
    /// on success; a non-2xx status (e.g. the target host is unreachable) is a
    /// valid HTTP outcome and maps to `Ok(None)` rather than a transport error.
    pub fn probe_delay(
        &self,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, KernelFailure> {
        // The kernel-side probe timeout follows the caller's budget (bounded)
        // so slow-but-reachable nodes are not missed by a hardcoded 3s probe.
        let delay_timeout_ms = u64::from(timeout.as_millis().clamp(100, 15_000) as u16);
        let encoded =
            percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
        // The probe URL must be percent-encoded: sing-box's Clash API rejects a
        // raw `://` in the query ("Body invalid") while the encoded form works.
        let encoded_url =
            percent_encoding::utf8_percent_encode(url, percent_encoding::NON_ALPHANUMERIC);
        let path = format!("/proxies/{encoded}/delay?url={encoded_url}&timeout={delay_timeout_ms}");
        let address = self.resolve_address()?;
        // Keep the socket timeout comfortably above the probe timeout so a slow
        // target yields a 503 response (mapped to Ok(None)) rather than a socket
        // read timeout (mapped to Err).
        let socket_timeout = timeout.saturating_add(Duration::from_secs(2));
        let mut stream = MihomoHttpControl::connect(address, socket_timeout)?;
        self.write_request(&mut stream, "GET", &path, "")?;
        let mut buffer = Vec::new();
        // Audit #86: bound the probe response like every other controller
        // read (32 MiB ceiling); an unbounded `read_to_end` on a loopback
        // socket can allocate tens of GB before the socket timeout bites.
        stream
            .take(32 * 1_024 * 1_024)
            .read_to_end(&mut buffer)
            .map_err(|error| {
                // A read timeout while awaiting the probe result means the kernel
                // did not answer within the probe budget (sing-box can hang past
                // its own `timeout` param when the dial stalls); a reset/EOF at
                // this stage likewise means the probe did not complete. Both are
                // classified as DeadlineExceeded so callers can treat them like an
                // unreachable node instead of a controller transport failure
                // (connect-phase failures stay ApiUnavailable/loud).
                let kind = if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::BrokenPipe
                ) {
                    crate::contract::KernelFailureKind::DeadlineExceeded
                } else {
                    crate::contract::KernelFailureKind::ApiUnavailable
                };
                crate::common::failure_with_kind(
                    kind,
                    &format!("cannot read Mihomo delay response: {error}"),
                    "inspect Mihomo controller health",
                )
            })?;
        let text = core::str::from_utf8(&buffer).unwrap_or("");
        let Some(status) = http_status_code(text) else {
            // No HTTP status line: the kernel closed the probe without
            // answering (sing-box drops the connection for outbounds whose
            // dial fails instantly). The probe did not complete — deadline,
            // not a decode/transport failure.
            return Err(crate::common::failure_with_kind(
                crate::contract::KernelFailureKind::DeadlineExceeded,
                "kernel closed the delay probe without an HTTP response",
                "inspect the kernel controller state",
            ));
        };
        if !(200..300).contains(&status) {
            // A 404 means the name is not configured in the running kernel
            // (a naming/apply mismatch on our side), which must be loud — never
            // silently reported as an "unreachable" node.
            if status == 404 {
                return Err(crate::common::failure_with_kind(
                    crate::contract::KernelFailureKind::ApiUnavailable,
                    &format!("proxy `{name}` is not configured in the running kernel config"),
                    "run `caly config apply` to publish subscription nodes, then retry",
                ));
            }
            // 5xx: the kernel ran the probe and the target did not answer
            // (e.g. 503/504 after the kernel-side timeout) — a genuine
            // unreachable outcome, not a transport failure.
            if (500..600).contains(&status) {
                return Ok(None);
            }
            return Err(crate::common::failure_with_kind(
                crate::contract::KernelFailureKind::ApiUnavailable,
                &format!("kernel delay endpoint returned HTTP {status} for proxy `{name}`"),
                "inspect the kernel controller state",
            ));
        }
        let json_text = match text.split_once("\r\n\r\n") {
            Some((_, body)) => body,
            None => text,
        };
        let json = serde_json::from_str::<serde_json::Value>(json_text).map_err(|_| {
            crate::common::failure_with_kind(
                crate::contract::KernelFailureKind::DecodeRejected,
                "Mihomo delay response is invalid JSON",
                "inspect controller output",
            )
        })?;
        Ok(json
            .get("delay")
            .and_then(numeric_value)
            .and_then(|value| u32::try_from(value).ok()))
    }

    /// Lists the names of real (dialable) proxies from `GET /proxies` — the
    /// input set for a full latency sweep (`delay --all`).
    ///
    /// Mihomo lists every proxy as a top-level entry; sing-box only exposes
    /// selectors plus special entries and keeps the nodes inside each
    /// selector's `all` list, so both shapes are collected and de-duplicated.
    pub fn list_proxy_names(&self, timeout: Duration) -> Result<Vec<String>, KernelFailure> {
        let body = self.request("/proxies", timeout)?;
        let json = super::http::response_json(&body)?;
        Ok(proxy_names_from_json(&json))
    }
}

/// Extracts dialable proxy names from a parsed `GET /proxies` document.
///
/// Pure and shared between the network path and the test-suite mirror (the
/// tests used to carry a third copy of this parser, which let the mapping
/// drift — there is now exactly one implementation).
fn proxy_names_from_json(json: &serde_json::Value) -> Vec<String> {
    let Some(proxies) = json.get("proxies").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let is_special = |proxy_type: &str| {
        matches!(
            proxy_type,
            "Selector"
                | "URLTest"
                | "Fallback"
                | "LoadBalance"
                | "Direct"
                | "Reject"
                | "Compatible"
                | "Pass"
        )
    };
    let mut names = std::collections::BTreeSet::new();
    for (name, value) in proxies {
        let proxy_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if matches!(
            proxy_type,
            "Selector" | "URLTest" | "Fallback" | "LoadBalance"
        ) {
            // sing-box exposes its nodes only as selector members.
            if let Some(all) = value.get("all").and_then(serde_json::Value::as_array) {
                for member in all {
                    if let Some(tag) = member.as_str()
                        && !tag.is_empty()
                        && tag != "direct"
                    {
                        names.insert(tag.to_owned());
                    }
                }
            }
        } else if !is_special(proxy_type) {
            names.insert(name.clone());
        }
    }
    // Deterministic output for the sweep table.
    names.into_iter().collect()
}

/// Extracts the three-digit HTTP status code from a raw response head
/// (delegates to the shared transport parser in `http.rs`).
fn http_status_code(text: &str) -> Option<u16> {
    super::http::status_code(text)
}

/// Reads from the stream until the first *complete* SSE data frame
/// (`data: {…}\n`) has been collected. A single TCP `read()` does not
/// guarantee a full frame; the loop re-scans the accumulator after every
/// chunk and fails loudly on EOF / an over-long frame prefix instead of
/// handing half a JSON document to the parser.
fn read_first_sse_frame(stream: &mut impl Read) -> Result<String, KernelFailure> {
    const MAX_FRAME_BYTES: usize = 64 * 1_024;
    let mut buffer: Vec<u8> = Vec::with_capacity(1_024);
    let mut chunk = [0_u8; 4 * 1_024];
    loop {
        if let Some(frame) = first_complete_frame(&buffer) {
            return Ok(frame);
        }
        if buffer.len() >= MAX_FRAME_BYTES {
            return Err(crate::common::failure_with_kind(
                crate::contract::KernelFailureKind::DecodeRejected,
                "core traffic frame exceeded the 64 KiB snapshot bound",
                "inspect controller output",
            ));
        }
        let read = stream.read(&mut chunk).map_err(|error| {
            crate::common::failure_with_kind(
                crate::contract::KernelFailureKind::ApiUnavailable,
                &format!("cannot read core traffic snapshot: {error}"),
                "inspect controller health",
            )
        })?;
        if read == 0 {
            // EOF before a complete frame: one last scan covers kernels that
            // close the stream right after flushing the snapshot.
            return first_complete_frame(&buffer).ok_or_else(|| {
                crate::common::failure_with_kind(
                    crate::contract::KernelFailureKind::DecodeRejected,
                    "core traffic stream closed before a complete frame",
                    "inspect controller output",
                )
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Extracts the first SSE/JSON data line that has fully arrived (starts
/// with `{` and ends with `}`). Returns `None` while only a partial frame
/// is buffered; HTTP response headers preceding the stream are skipped.
fn first_complete_frame(buffer: &[u8]) -> Option<String> {
    let text = core::str::from_utf8(buffer).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("data: ").unwrap_or(line);
        if line.starts_with('{') {
            // One JSON frame is one line; an unterminated line means the
            // frame is still in flight — keep reading instead of failing.
            return line.ends_with('}').then(|| line.to_owned());
        }
    }
    None
}

/// Decodes one `/traffic` SSE frame into its JSON object.
fn parse_traffic_json(text: &str) -> Result<serde_json::Value, KernelFailure> {
    let json_text = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("data: ").or(Some(line)))
        .find(|line| line.starts_with('{'))
        .unwrap_or("");
    serde_json::from_str::<serde_json::Value>(json_text).map_err(|_| {
        crate::common::failure_with_kind(
            crate::contract::KernelFailureKind::DecodeRejected,
            "core traffic snapshot is invalid JSON",
            "inspect controller output",
        )
    })
}

/// Parses a `/traffic` data frame into per-second (download, upload) rates.
///
/// `/traffic` differs between kernels: Mihomo emits `{"up":N,"down":N,
/// "upTotal":N,"downTotal":N}` where `up`/`down` are the *per-second*
/// Parses a `/traffic` data frame into per-second (download, upload) rates.
/// Per-second fields win over the cumulative totals so telemetry records a
/// rate, not an ever-growing counter. Test helper — the production path
/// distinguishes Mihomo totals from sing-box delta frames directly.
#[cfg(test)]
fn parse_traffic_frame(text: &str) -> Result<(u64, u64), KernelFailure> {
    let json = parse_traffic_json(text)?;
    let download = traffic_field(&json, &["down", "downloadTotal", "downTotal"]).unwrap_or(0);
    let upload = traffic_field(&json, &["up", "uploadTotal", "upTotal"]).unwrap_or(0);
    Ok((download, upload))
}

/// Extracts the cumulative totals from a frame (Mihomo only): returns
/// `None` for delta-only frames (sing-box) so the caller falls back to
/// window accumulation.
fn parse_traffic_total(text: &str) -> Option<(u64, u64)> {
    let json = parse_traffic_json(text).ok()?;
    let download = traffic_field(&json, &["downTotal"])?;
    let upload = traffic_field(&json, &["upTotal"])?;
    Some((download, upload))
}

/// Extracts the per-window delta from a frame (both kernels emit
/// `up`/`down`; sing-box's are 100 ms increments).
fn parse_traffic_rate(text: &str) -> Option<(u64, u64)> {
    let json = parse_traffic_json(text).ok()?;
    Some((
        traffic_field(&json, &["down"])?,
        traffic_field(&json, &["up"])?,
    ))
}

/// Parses every live connection with its routing metadata from a
/// Clash-compatible `/connections` document. Missing/invalid fields degrade
/// to defaults so a kernel schema drift never fails the whole list.
fn parse_connections_json(json: &serde_json::Value) -> Vec<ConnectionDetail> {
    let Some(connections) = json
        .get("connections")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    connections.iter().map(parse_connection_json).collect()
}

/// Parses one `/connections` entry; metadata wins, top level falls back,
/// missing fields degrade to defaults so a kernel schema drift never fails
/// the whole list.
fn parse_connection_json(connection: &serde_json::Value) -> ConnectionDetail {
    let metadata = connection.get("metadata");
    let host = metadata
        .and_then(|m| m.get("host"))
        .and_then(serde_json::Value::as_str)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            metadata
                .and_then(|m| m.get("destinationIP"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let destination_port = metadata
        .and_then(|m| m.get("destinationPort"))
        .and_then(serde_json::Value::as_str)
        .and_then(|port| port.parse().ok())
        .unwrap_or(0);
    ConnectionDetail {
        id: connection
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        host,
        destination_port,
        network: metadata
            .and_then(|m| m.get("network"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        protocol_type: metadata
            .and_then(|m| m.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        inbound_name: metadata
            .and_then(|m| m.get("inboundName"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        process_path: metadata
            .and_then(|m| m.get("processPath"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        rule: metadata
            .and_then(|m| m.get("rule"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| connection.get("rule").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .to_owned(),
        rule_payload: metadata
            .and_then(|m| m.get("rulePayload"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                connection
                    .get("rulePayload")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or_default()
            .to_owned(),
        outbound: metadata
            .and_then(|m| m.get("outbound"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                connection
                    .get("outbound")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or_default()
            .to_owned(),
        chain: metadata
            .and_then(|m| m.get("chain"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| {
                connection
                    .get("chains")
                    .and_then(serde_json::Value::as_array)
            })
            .map(|array| {
                array
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        upload_bytes: numeric(connection, "upload"),
        download_bytes: numeric(connection, "download"),
    }
}

/// Reads a traffic byte count from the first matching field name.
fn traffic_field(node: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| node.get(*key).and_then(numeric_value))
}

/// Reads a non-negative numeric field as `u64`.
fn numeric(node: &serde_json::Value, key: &str) -> u64 {
    node.get(key).and_then(numeric_value).unwrap_or(0)
}

/// Extracts a `u64` from a JSON number (handles both signed and unsigned).
fn numeric_value(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
}

/// Parses the `proxies` object of `GET /proxies` into kernel proxy
/// groups (Selector/URLTest/Fallback/LoadBalance), carrying the
/// kernel-side membership (`all`) and current selection (`now`). Pure
/// for testability: a missing `all` degrades to an empty membership.
pub(crate) fn parse_proxy_groups(json: &serde_json::Value) -> Vec<ProxyGroup> {
    let Some(proxies) = json.get("proxies").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut groups = Vec::new();
    for (name, value) in proxies {
        let is_group = matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("Selector" | "URLTest" | "Fallback" | "LoadBalance")
        );
        if !is_group {
            continue;
        }
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Selector")
            .to_owned();
        let selected = value
            .get("now")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let members = value
            .get("all")
            .and_then(serde_json::Value::as_array)
            .map(|members| {
                members
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        groups.push(ProxyGroup {
            name: name.clone(),
            kind,
            selected,
            members,
        });
    }
    groups
}

#[cfg(test)]
mod proxy_groups_tests {
    use super::parse_proxy_groups;
    use crate::contract::ProxyGroup;

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("test payload must be valid JSON")
    }

    #[test]
    fn parses_groups_with_kind_selection_and_members() {
        // W3b enrichment: `all` membership + `type` kind ride along with
        // the existing `now` selection, so the CLI can render group rows
        // and the GROUP column without re-deriving membership.
        let groups = parse_proxy_groups(&json(
            r#"{"proxies":{
                "auto": {"type":"URLTest","now":"HK-01","all":["HK-01","JP-01"]},
                "GLOBAL": {"type":"Selector","now":"HK-01","all":["HK-01","JP-01","DIRECT"]},
                "HK-01": {"type":"Shadowsocks","history":[]}
            }}"#,
        ));
        assert_eq!(
            groups,
            vec![
                // serde_json objects iterate in key order (BTreeMap):
                // `GLOBAL` sorts before `auto`.
                ProxyGroup {
                    name: "GLOBAL".to_owned(),
                    kind: "Selector".to_owned(),
                    selected: Some("HK-01".to_owned()),
                    members: vec!["HK-01".to_owned(), "JP-01".to_owned(), "DIRECT".to_owned()],
                },
                ProxyGroup {
                    name: "auto".to_owned(),
                    kind: "URLTest".to_owned(),
                    selected: Some("HK-01".to_owned()),
                    members: vec!["HK-01".to_owned(), "JP-01".to_owned()],
                },
            ]
        );
    }

    #[test]
    fn missing_all_degrades_to_empty_membership() {
        let groups = parse_proxy_groups(&json(
            r#"{"proxies":{"auto": {"type":"Fallback","now":"JP-01"}}}"#,
        ));
        assert_eq!(
            groups,
            vec![ProxyGroup {
                name: "auto".to_owned(),
                kind: "Fallback".to_owned(),
                selected: Some("JP-01".to_owned()),
                members: Vec::new(),
            }]
        );
    }

    #[test]
    fn missing_proxies_object_is_empty() {
        assert!(parse_proxy_groups(&json("{}")).is_empty());
    }
}

#[cfg(test)]
mod probe_status_tests;

#[cfg(test)]
mod proxy_names_tests;

#[cfg(test)]
mod connection_details_tests {
    use super::parse_connections_json;

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("test payload must be valid JSON")
    }

    #[test]
    fn parses_metadata_with_fallbacks() {
        let details = parse_connections_json(&json(
            r#"{"connections":[{
                "id":"conn-1",
                "metadata":{
                    "network":"tcp","type":"http",
                    "destinationIP":"93.184.216.34","destinationPort":"443",
                    "host":"example.com","rule":"DomainSuffix",
                    "inboundName":"mixed","processPath":"/usr/bin/curl",
                    "rulePayload":"example.com","outbound":"HK-01",
                    "chain":["PROXY","HK-01"]
                },
                "upload":1048576,"download":5242880
            }]}"#,
        ));
        assert_eq!(details.len(), 1);
        let detail = &details[0];
        assert_eq!(detail.id, "conn-1");
        assert_eq!(detail.host, "example.com");
        assert_eq!(detail.destination_port, 443);
        assert_eq!(detail.network, "tcp");
        assert_eq!(detail.protocol_type, "http");
        assert_eq!(detail.inbound_name, "mixed");
        assert_eq!(detail.process_path, "/usr/bin/curl");
        assert_eq!(detail.rule, "DomainSuffix");
        assert_eq!(detail.rule_payload, "example.com");
        assert_eq!(detail.outbound, "HK-01");
        assert_eq!(detail.chain, vec!["PROXY".to_owned(), "HK-01".to_owned()]);
        assert_eq!(detail.upload_bytes, 1_048_576);
        assert_eq!(detail.download_bytes, 5_242_880);
    }

    #[test]
    fn host_falls_back_to_destination_ip() {
        let details = parse_connections_json(&json(
            r#"{"connections":[{
                "id":"c","metadata":{
                    "network":"udp","type":"quic",
                    "destinationIP":"8.8.8.8","destinationPort":"443",
                    "host":"","rule":"MATCH","rulePayload":"",
                    "outbound":"JP-01","chain":["PROXY","JP-01"]
                },"upload":0,"download":0
            }]}"#,
        ));
        assert_eq!(details[0].host, "8.8.8.8");
        assert_eq!(details[0].network, "udp");
    }

    #[test]
    fn top_level_fields_are_the_fallback() {
        // sing-box via the Clash-compat surface can put rule/outbound at the
        // top level instead of inside metadata.
        let details = parse_connections_json(&json(
            r#"{"connections":[{
                "id":"c",
                "metadata":{"network":"tcp","type":"tls","destinationIP":"1.1.1.1","destinationPort":"8443","host":"x.test"},
                "rule":"DomainSuffix","rulePayload":"test","outbound":"NODE-1",
                "chains":["GROUP","NODE-1"],
                "upload":1,"download":2
            }]}"#,
        ));
        let detail = &details[0];
        assert_eq!(detail.rule, "DomainSuffix");
        assert_eq!(detail.rule_payload, "test");
        assert_eq!(detail.outbound, "NODE-1");
        assert_eq!(detail.chain, vec!["GROUP".to_owned(), "NODE-1".to_owned()]);
    }

    #[test]
    fn missing_connections_array_is_empty() {
        assert!(parse_connections_json(&json(r#"{"downloadTotal":0}"#)).is_empty());
    }
}

#[cfg(test)]
mod tests;
