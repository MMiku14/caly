//! Test-support stand-in for the managed mihomo / sing-box kernels (#68).
//!
//! The real-kernel E2E suites skip (or hard-fail under
//! `CALY_REQUIRE_REAL_E2E=1`) when the pinned `vendor/bin` binaries are
//! absent, which leaves CI hosts without network-fetched kernels with zero
//! daemon-lifecycle coverage. This binary emulates the small contract the
//! daemon actually composes against:
//!
//! - `<bin> version` prints a version line and exits 0;
//! - Mihomo config validation (`-d <dir> -f <config> -t`) and sing-box
//!   `check -c <config>` exit 0 when the config file is readable;
//! - serve mode (`-d <dir> -f <config>` / `run -c <config>`) parses the
//!   rendered config for the Clash API controller address
//!   (`external-controller:` in the Mihomo YAML, `"external_controller"` in
//!   the sing-box JSON), binds it, and serves the handful of endpoints the
//!   daemon's command and telemetry paths use (`/version`, `/proxies`,
//!   `/connections`, `/configs`, `/traffic`, `/logs`, `/memory`) until it is
//!   signalled, exactly like the real controller.

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::ExitCode,
    thread,
    time::Duration,
};

/// Upper bound on one inbound HTTP request; the daemon's hand-rolled client
/// sends small bodies (node select / config patch) only.
const MAX_REQUEST_BYTES: usize = 64 * 1024;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.iter().any(|argument| argument == "version") {
        println!("caly-mock-kernel 1.0 (mihomo/sing-box test double)");
        return ExitCode::SUCCESS;
    }
    let validation = arguments.first().is_some_and(|first| first == "check")
        || arguments.iter().any(|argument| argument == "-t");
    let Some(config) = config_path(&arguments) else {
        eprintln!("caly-mock-kernel: no -f or -c config argument in {arguments:?}");
        return ExitCode::from(2);
    };
    let body = match fs::read_to_string(&config) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("caly-mock-kernel: cannot read {config}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if validation {
        // Real kernels only parse the document here; readability is the
        // whole contract the daemon's apply pipeline relies on.
        return ExitCode::SUCCESS;
    }
    let Some(controller) = controller_address(&body) else {
        eprintln!("caly-mock-kernel: no external-controller address in {config}");
        return ExitCode::from(2);
    };
    let listener = match TcpListener::bind(controller.as_str()) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("caly-mock-kernel: cannot bind {controller}: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("caly-mock-kernel: Clash API listening on {controller}");
    serve(&listener);
    ExitCode::SUCCESS
}

/// Extracts the config path from the kernel argument list: the value after
/// `-f` (Mihomo) or `-c` (sing-box).
fn config_path(arguments: &[String]) -> Option<String> {
    let mut index = 0;
    while index + 1 < arguments.len() {
        if arguments[index] == "-f" || arguments[index] == "-c" {
            return Some(arguments[index + 1].clone());
        }
        index += 1;
    }
    None
}

/// Locates the Clash API controller address in a rendered config, whichever
/// of the two core formats it is: YAML `external-controller: host:port` or
/// JSON `"external_controller": "host:port"`.
fn controller_address(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(value) = line.trim_start().strip_prefix("external-controller:") {
            let value = value.trim().trim_matches('\'').trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    let key = body.find("\"external_controller\"")?;
    let after_key = body.get(key + "\"external_controller\"".len()..)?;
    let colon = after_key.find(':')?;
    let after_colon = after_key.get(colon + 1..)?.trim_start();
    let quoted = after_colon.strip_prefix('"')?;
    let end = quoted.find('"')?;
    let value = quoted.get(..end)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// Accept loop: every connection gets its own short-lived thread so a held
/// SSE stream never blocks the readiness probes of another path.
fn serve(listener: &TcpListener) {
    loop {
        match listener.accept() {
            Ok((stream, _peer)) => {
                drop(
                    thread::Builder::new()
                        .name("caly-mock-kernel-conn".to_owned())
                        .spawn(move || {
                            if let Err(error) = handle(stream) {
                                eprintln!("caly-mock-kernel: connection error: {error}");
                            }
                        }),
                );
            }
            Err(error) => {
                eprintln!("caly-mock-kernel: accept failed: {error}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Reads one request (headers plus a Content-Length body), routes it, and
/// answers. Every response carries `Connection: close`; the daemon's client
/// opens a fresh stream per call.
fn handle(mut stream: std::net::TcpStream) -> std::io::Result<()> {
    let request = read_request(&mut stream)?;
    let mut words = request.split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let path = words.next().unwrap_or_default().to_owned();
    match (method.as_str(), route(&path)) {
        ("GET", "/traffic") => serve_sse(stream, "{\"up\":0,\"down\":0}"),
        ("GET", "/logs") => serve_sse(stream, "{\"type\":\"info\",\"payload\":\"mock log\"}"),
        ("GET", "/version") => respond_json(
            stream,
            "200 OK",
            "{\"version\":\"caly-mock-kernel-1.0\",\"meta\":true}",
        ),
        ("GET", "/proxies") => respond_json(stream, "200 OK", "{\"proxies\":{}}"),
        ("PUT", "/proxies") => respond_empty(stream, "204 No Content"),
        ("PUT", "/configs") => respond_empty(stream, "204 No Content"),
        ("GET", "/delay") => respond_json(stream, "200 OK", "{\"delay\":1}"),
        ("GET", "/connections") => respond_json(
            stream,
            "200 OK",
            concat!(
                "{\"downloadTotal\":5242880,\"uploadTotal\":1048576,\"connections\":[",
                "{\"id\":\"conn-1\",\"metadata\":{\"network\":\"tcp\",\"type\":\"http\",",
                "\"destinationIP\":\"93.184.216.34\",\"destinationPort\":\"443\",",
                "\"host\":\"example.com\",\"rule\":\"DomainSuffix\",",
                "\"inboundName\":\"mixed\",\"processPath\":\"/usr/bin/curl\",",
                "\"rulePayload\":\"example.com\",\"outbound\":\"HK-01\",",
                "\"chain\":[\"PROXY\",\"HK-01\"]},\"upload\":1048576,\"download\":5242880},",
                "{\"id\":\"conn-2\",\"metadata\":{\"network\":\"udp\",\"type\":\"quic\",",
                "\"destinationIP\":\"8.8.8.8\",\"destinationPort\":\"443\",",
                "\"host\":\"dns.google\",\"rule\":\"MATCH\",\"rulePayload\":\"\",",
                "\"outbound\":\"JP-01\",\"chain\":[\"PROXY\",\"JP-01\"]},",
                "\"upload\":2048,\"download\":4096}]}",
            ),
        ),
        ("DELETE", "/connections") => respond_empty(stream, "204 No Content"),
        ("GET", "/configs") => respond_json(
            stream,
            "200 OK",
            "{\"port\":7890,\"socks-port\":7891,\"mode\":\"rule\",\"log-level\":\"info\"}",
        ),
        ("PATCH", "/configs") => respond_empty(stream, "204 No Content"),
        ("GET", "/memory") => respond_json(stream, "200 OK", "{\"inuse\":0,\"oslimit\":0}"),
        _ => respond_json(
            stream,
            "404 Not Found",
            "{\"message\":\"mock: unknown endpoint\"}",
        ),
    }
}

/// Reduces a request path to its first functional segment so suffix routes
/// (`/proxies/<name>/delay?t=x`) share one arm.
fn route(path: &str) -> &str {
    let clean = path.split('?').next().unwrap_or_default();
    if clean.starts_with("/proxies/") && clean.ends_with("/delay") {
        return "/delay";
    }
    // Audit #88: `starts_with("/proxies")` also matched `/proxiesXYZ`;
    // require an exact hit or a `/`-bounded prefix.
    if clean == "/proxies" || clean.starts_with("/proxies/") {
        return "/proxies";
    }
    for known in [
        "/version",
        "/connections",
        "/configs",
        "/traffic",
        "/logs",
        "/memory",
    ] {
        if clean == known {
            return known;
        }
    }
    clean
}

/// Reads headers (capped at `MAX_REQUEST_BYTES`) and drains the declared
/// body so the client sees a full response instead of a reset.
fn read_request(stream: &mut std::net::TcpStream) -> std::io::Result<String> {
    let mut buffer = Vec::with_capacity(4 * 1024);
    let mut scratch = [0_u8; 4 * 1024];
    loop {
        let read = stream.read(&mut scratch)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(scratch.get(..read).unwrap_or_default());
        if let Some(headers_end) = find_subslice(&buffer, b"\r\n\r\n") {
            let content_length = header_content_length(&buffer, headers_end);
            // Audit #88: the declared length is attacker-controlled — reject
            // it against the mock bound *before* it enters any arithmetic,
            // then use checked arithmetic (a giant `content_length` used to
            // overflow `headers_end + 4 + content_length` in debug builds).
            if content_length > MAX_REQUEST_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "declared request body exceeds the mock bound",
                ));
            }
            let needed = headers_end
                .checked_add(4)
                .and_then(|base| base.checked_add(content_length))
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "request size overflow")
                })?;
            if buffer.len() >= needed {
                break;
            }
        }
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request exceeds the mock bound",
            ));
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn header_content_length(buffer: &[u8], headers_end: usize) -> usize {
    let headers = String::from_utf8_lossy(buffer.get(..headers_end).unwrap_or_default());
    for line in headers.lines() {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:")
            && let Ok(parsed) = value.trim().parse::<usize>()
        {
            return parsed;
        }
    }
    0
}

fn write_head(
    mut stream: &std::net::TcpStream,
    status: &str,
    content_type: &str,
    content_length: Option<usize>,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n"
    )?;
    if let Some(length) = content_length {
        write!(stream, "Content-Length: {length}\r\n")?;
    }
    stream.write_all(b"\r\n")
}

fn respond_json(stream: std::net::TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write_head(&stream, status, "application/json", Some(body.len()))?;
    (&stream).write_all(body.as_bytes())
}

fn respond_empty(stream: std::net::TcpStream, status: &str) -> std::io::Result<()> {
    write_head(&stream, status, "application/json", Some(0))
}

/// Streams `data: <frame>` SSE events immediately and then once per second
/// until the telemetry reader hangs up, mirroring the live `/traffic` shape.
fn serve_sse(mut stream: std::net::TcpStream, frame: &str) -> std::io::Result<()> {
    write_head(&stream, "200 OK", "text/event-stream", None)?;
    loop {
        stream.write_all(b"data: ")?;
        stream.write_all(frame.as_bytes())?;
        stream.write_all(b"\r\n\r\n")?;
        stream.flush()?;
        thread::sleep(Duration::from_secs(1));
    }
}
