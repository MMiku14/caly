//! Bounded DNS reachability probe plus the wire codec.
//!
//! Sends a minimal DNS `A` query (UDP, or TCP with the RFC 1035 two-octet
//! length framing) to a nameserver and classifies the outcome. Used for
//! diagnostics (per-nameserver health) without a full DNS client library.
//! The query transaction id is injected by the caller (crate-replan §5.12:
//! deterministic testability), so this crate stays free of
//! `caly-platform::entropy`.
//!
//! F1/B3: dispatch is scheme-aware through the structured `Nameserver`.
//! UDP and TCP are really probed; TLS/HTTPS/QUIC answer honestly with
//! `UnsupportedTransport` (this build carries no TLS/HTTP stack — the
//! pre-fix code silently mangled every schemed value into a UDP attempt
//! and reported Error, which read as "server broken", not "unsupported").

use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    time::Duration,
};

use crate::nameserver::{Nameserver, NameserverKind};

/// Default DNS service port when a nameserver has no explicit port.
pub const DNS_DEFAULT_PORT: u16 = 53;
/// Reasonable per-attempt read timeout.
pub const DNS_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Classified result of a single nameserver probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsProbeOutcome {
    /// The resolver answered with a usable (no-error) response, optionally
    /// carrying the first A-record address from the answer section.
    Resolved(Option<IpAddr>),
    /// The resolver refused the query or returned an error rcode.
    Refused,
    /// No response arrived within the timeout.
    Timeout,
    /// The transport is honestly not probeable in this build: TLS/HTTPS/
    /// QUIC servers (no TLS/HTTP stack here) and the `local` pseudo-server
    /// (no socket address) (F1/B3).
    UnsupportedTransport,
    /// The nameserver/domain input was invalid or the socket failed.
    Error,
}

/// Probes one nameserver for the given hostname. B3: the nameserver is
/// parsed through the structured model and dispatched by scheme — UDP/TCP
/// are probed, transports without a stack in this build answer
/// `UnsupportedTransport` instead of a misleading UDP-mangled Error.
pub fn probe_nameserver(
    nameserver: &str,
    domain: &str,
    timeout: Duration,
    transaction_id: [u8; 2],
) -> DnsProbeOutcome {
    let Ok(server) = Nameserver::new(nameserver) else {
        return DnsProbeOutcome::Error;
    };
    match server.kind() {
        NameserverKind::Udp => probe_udp(server.address(), domain, timeout, transaction_id),
        NameserverKind::Tcp => probe_tcp(server.address(), domain, timeout, transaction_id),
        NameserverKind::Local
        | NameserverKind::Tls
        | NameserverKind::Https
        | NameserverKind::Quic => DnsProbeOutcome::UnsupportedTransport,
    }
}

/// UDP probe: one datagram out; datagrams that fail the transaction-id/QR
/// checks are dropped while budget remains (#124) — an early stray or
/// spoofed packet must not foreclose the real answer.
fn probe_udp(
    nameserver: &str,
    domain: &str,
    timeout: Duration,
    transaction_id: [u8; 2],
) -> DnsProbeOutcome {
    let started = std::time::Instant::now();
    let address = match resolve_for_probe(nameserver, timeout) {
        Ok(address) => address,
        Err(outcome) => return outcome,
    };
    let Ok(query) = build_query(domain, transaction_id) else {
        return DnsProbeOutcome::Error;
    };
    // Bind in the target's address family: an IPv4 wildcard socket cannot
    // reach an IPv6 resolver, so a v6 nameserver always reported Error.
    let bind_address: SocketAddr = if address.is_ipv6() {
        SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)
    };
    let Ok(socket) = UdpSocket::bind(bind_address) else {
        return DnsProbeOutcome::Error;
    };
    if socket.connect(address).is_err() {
        return DnsProbeOutcome::Error;
    }
    if socket.send(&query).is_err() {
        return DnsProbeOutcome::Error;
    }
    let deadline = started + timeout;
    let mut buffer = [0u8; 512];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return DnsProbeOutcome::Timeout;
        }
        if socket.set_read_timeout(Some(remaining)).is_err() {
            return DnsProbeOutcome::Error;
        }
        match socket.recv(&mut buffer) {
            Ok(read) => {
                if let Some(outcome) = classify_datagram(&query, &buffer[..read]) {
                    return outcome;
                }
                // Non-matching datagram: drop and keep waiting out the budget.
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return DnsProbeOutcome::Timeout;
            }
            // e.g. ECONNREFUSED (ICMP port unreachable): the host answered but
            // nothing serves :53 — that is a reachable-but-broken server, an
            // Error, never a Timeout (the pre-fix code lumped every recv error
            // into Timeout).
            Err(_) => return DnsProbeOutcome::Error,
        }
    }
}

/// TCP probe (F1): RFC 1035 §4.2.2 framing — a two-octet big-endian length
/// prefixes the message on the wire; the response framing is identical.
fn probe_tcp(
    nameserver: &str,
    domain: &str,
    timeout: Duration,
    transaction_id: [u8; 2],
) -> DnsProbeOutcome {
    let started = std::time::Instant::now();
    let address = match resolve_for_probe(nameserver, timeout) {
        Ok(address) => address,
        Err(outcome) => return outcome,
    };
    let Ok(query) = build_query(domain, transaction_id) else {
        return DnsProbeOutcome::Error;
    };
    // #121: the budget is shared across resolve/connect/read — a slow
    // resolver shrinks what the connection may still spend.
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return DnsProbeOutcome::Timeout;
    }
    let Ok(stream) = TcpStream::connect_timeout(&address, remaining) else {
        return DnsProbeOutcome::Error;
    };
    if stream.set_read_timeout(Some(remaining)).is_err()
        || stream.set_write_timeout(Some(remaining)).is_err()
    {
        return DnsProbeOutcome::Error;
    }
    let mut stream = stream;
    let length = u16::try_from(query.len()).unwrap_or(0);
    if query.is_empty() || length as usize != query.len() {
        return DnsProbeOutcome::Error;
    }
    let mut frame = Vec::with_capacity(query.len() + 2);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&query);
    if stream.write_all(&frame).is_err() {
        return DnsProbeOutcome::Error;
    }
    let mut prefix = [0u8; 2];
    if let Err(error) = stream.read_exact(&mut prefix) {
        return tcp_read_outcome(&error);
    }
    let expected = u16::from_be_bytes(prefix) as usize;
    let mut response = vec![0u8; expected];
    if expected == 0 {
        return DnsProbeOutcome::Error;
    }
    match stream.read_exact(&mut response) {
        Ok(()) => classify_response(&query, &response),
        Err(error) => tcp_read_outcome(&error),
    }
}

/// Maps a TCP read failure like the UDP arm: a clean timeout stays Timeout;
/// a refused/reset/garbled exchange is Error.
fn tcp_read_outcome(error: &io::Error) -> DnsProbeOutcome {
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) {
        DnsProbeOutcome::Timeout
    } else {
        DnsProbeOutcome::Error
    }
}

/// One probe exchange: the classified outcome plus its wall-clock latency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DnsProbeReport {
    pub outcome: DnsProbeOutcome,
    pub latency: Duration,
}

/// Probes one nameserver, also reporting round-trip latency. For non-resolved
/// outcomes the latency still reflects when the exchange settled (error fast,
/// timeout at the bound), which is the value an operator compares across
/// servers.
pub fn probe_nameserver_timed(
    nameserver: &str,
    domain: &str,
    timeout: Duration,
    transaction_id: [u8; 2],
) -> DnsProbeReport {
    let started = std::time::Instant::now();
    let outcome = probe_nameserver(nameserver, domain, timeout, transaction_id);
    DnsProbeReport {
        outcome,
        latency: started.elapsed(),
    }
}

/// Resolves `host[:port]` for a probe within `budget`, mapping a blown
/// budget onto Timeout so the caller's outcome set stays truthful (#121).
fn resolve_for_probe(nameserver: &str, budget: Duration) -> Result<SocketAddr, DnsProbeOutcome> {
    parse_nameserver_within(nameserver, budget).map_err(|error| {
        if matches!(error.kind(), io::ErrorKind::TimedOut) {
            DnsProbeOutcome::Timeout
        } else {
            DnsProbeOutcome::Error
        }
    })
}

/// Parses `host` or `host:port` into a socket address, defaulting to 53.
/// IP literals resolve synchronously; a domain form needs the system
/// resolver, whose syscall is unbounded — so it runs on a one-shot helper
/// thread raced against the probe budget (#121: pre-fix the getaddrinfo
/// hang was charged neither to `timeout` nor to a bounded thread).
fn parse_nameserver_within(value: &str, budget: Duration) -> io::Result<SocketAddr> {
    parse_nameserver_with(value, budget, |text| {
        text.to_socket_addrs()
            .map(std::iter::Iterator::collect::<Vec<_>>)
    })
}

/// Resolver-injected form (§5.12: deterministic tests for a syscall that
/// cannot be black-holed on demand).
fn parse_nameserver_with(
    value: &str,
    budget: Duration,
    resolve: impl FnOnce(String) -> io::Result<Vec<SocketAddr>> + Send + 'static,
) -> io::Result<SocketAddr> {
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DNS_DEFAULT_PORT));
    }
    // #123: a bare bracketed v6 literal carries no port for the system
    // resolver to chew on — strip the brackets, default the port.
    if let Some(ip) = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .and_then(|inner| inner.parse::<IpAddr>().ok())
    {
        return Ok(SocketAddr::new(ip, DNS_DEFAULT_PORT));
    }
    // A bare domain has no port either; `to_socket_addrs` requires one.
    let host_port = if value.contains(':') {
        value.to_owned()
    } else {
        format!("{value}:{DNS_DEFAULT_PORT}")
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // One-shot, detached: a wedged getaddrinfo strands only this
        // helper, which dies with the process like any parked thread.
        let _ = sender.send(resolve(host_port));
    });
    match receiver.recv_timeout(budget) {
        Ok(Ok(addresses)) => addresses
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no address")),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "resolution exceeded the probe budget",
        )),
    }
}

/// Interprets a response by matching the transaction id and reading the
/// rcode. Terminal form used where the stream cannot carry stray packets
/// (TCP): any mismatch is an Error, nothing more can arrive.
fn classify_response(query: &[u8], response: &[u8]) -> DnsProbeOutcome {
    classify_datagram(query, response).unwrap_or(DnsProbeOutcome::Error)
}

/// #124: datagram form — returns `None` for packets that fail the
/// transaction-id/QR/shape checks so the UDP loop can drop them and keep
/// waiting out the budget (a stray or spoofed early packet must not
/// foreclose the real answer; pre-fix the first packet was terminal).
fn classify_datagram(query: &[u8], response: &[u8]) -> Option<DnsProbeOutcome> {
    if response.len() < 12 || query.len() < 2 {
        return None;
    }
    if response[0] != query[0] || response[1] != query[1] {
        return None;
    }
    // Audit #90: without the QR check, an attacker echoing our *query*
    // back at us (same transaction id, rcode 0) was classified Resolved.
    if response[2] & 0x80 == 0 {
        return None;
    }
    let rcode = response[3] & 0x0f;
    if rcode != 0 {
        return Some(DnsProbeOutcome::Refused);
    }
    Some(DnsProbeOutcome::Resolved(parse_first_a(response)))
}

/// Extracts the first `A` record address from a DNS response answer section.
/// Returns `None` when there is no A answer or the encoding is unexpected.
fn parse_first_a(response: &[u8]) -> Option<IpAddr> {
    let qdcount = u16::from_be_bytes([response[4], response[5]]);
    let ancount = u16::from_be_bytes([response[6], response[7]]);
    if ancount == 0 {
        return None;
    }
    // Audit #90: skip exactly QDCOUNT questions — the pre-fix code assumed
    // one, so a response carrying zero or two questions was mis-aligned.
    let mut offset = 12;
    for _ in 0..qdcount {
        offset = skip_question(response, offset)?;
    }
    for _ in 0..ancount {
        offset = skip_name(response, offset)?;
        if offset + 10 > response.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([response[offset], response[offset + 1]]);
        let rdlength = u16::from_be_bytes([response[offset + 8], response[offset + 9]]) as usize;
        let rdata = offset + 10;
        if rtype == 1 && rdlength == 4 && rdata + 4 <= response.len() {
            return Some(IpAddr::V4(std::net::Ipv4Addr::new(
                response[rdata],
                response[rdata + 1],
                response[rdata + 2],
                response[rdata + 3],
            )));
        }
        offset = rdata + rdlength;
    }
    None
}

/// Skips the DNS question section starting at `offset`.
fn skip_question(response: &[u8], offset: usize) -> Option<usize> {
    let after_name = skip_name(response, offset)?;
    // QTYPE (2) + QCLASS (2)
    after_name
        .checked_add(4)
        .filter(|end| *end <= response.len())
}

/// Skips a DNS name (labels or a compression pointer).
fn skip_name(response: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        if offset >= response.len() {
            return None;
        }
        let length = response[offset];
        if length == 0 {
            return offset.checked_add(1);
        }
        if length & 0xc0 == 0xc0 {
            // Compression pointer: 2 bytes total, name ends.
            return offset.checked_add(2);
        }
        offset = offset.checked_add(1 + length as usize)?;
        if offset > response.len() {
            return None;
        }
    }
}

/// Builds a minimal DNS query (header + `A` question, IN class). The
/// transaction id is caller-injected (Audit #90: fixed ids made blind
/// off-path forgery free, so callers draw one from the kernel CSPRNG per
/// probe; §5.12 sanctions the injection so this crate needs no entropy).
fn build_query(domain: &str, transaction_id: [u8; 2]) -> io::Result<Vec<u8>> {
    let mut query = Vec::with_capacity(64);
    query.extend_from_slice(&transaction_id);
    query.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    query.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    query.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // AN/NS/AR counts
    // Audit #117: bound the TOTAL encoded name (DNS wire names cap at 255
    // bytes) and reject non-ASCII labels rather than emitting raw UTF-8
    // that no resolver can interpret (IDNA is out of scope for a probe).
    let mut encoded = Vec::with_capacity(domain.len() + 1);
    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad label"));
        }
        let length = u8::try_from(label.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad label"))?;
        encoded.push(length);
        encoded.extend_from_slice(label.as_bytes());
    }
    encoded.push(0);
    if encoded.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "domain name too long",
        ));
    }
    query.extend_from_slice(&encoded);
    query.extend_from_slice(&1u16.to_be_bytes()); // QTYPE: A
    query.extend_from_slice(&1u16.to_be_bytes()); // QCLASS: IN
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed transaction id for the wire tests (caller-injected since P4,
    /// so the tests are deterministic by construction).
    const TXID: [u8; 2] = [0xAB, 0xCD];

    #[test]
    fn query_contains_question_for_domain() -> Result<(), std::io::Error> {
        let query = build_query("example.com", TXID)?;
        // #90: the production caller draws the transaction id from the kernel
        // CSPRNG per query; the wire tests pin a constant one (§5.12).
        assert_eq!(&query[0..2], TXID);
        assert_eq!(&query[12..25], b"\x07example\x03com\x00");
        Ok(())
    }

    #[test]
    fn bad_domain_label_is_rejected() {
        assert!(build_query("", TXID).is_err());
        assert!(build_query("a..b", TXID).is_err());
    }

    #[test]
    fn matching_transaction_classifies_resolved() -> Result<(), std::io::Error> {
        let query = build_query("example.com", TXID)?;
        let mut response = [0u8; 512];
        response[0] = query[0];
        response[1] = query[1];
        response[2] = 0x80; // QR set, rcode 0
        assert_eq!(
            classify_response(&query, &response[..12]),
            DnsProbeOutcome::Resolved(None)
        );
        Ok(())
    }

    #[test]
    fn echoed_query_without_qr_bit_is_rejected() -> Result<(), std::io::Error> {
        // Audit #90: an off-path attacker can echo our query verbatim (same
        // id, rcode 0, QR=0) — it must never classify as Resolved.
        let query = build_query("example.com", TXID)?;
        let echoed = query.clone();
        assert_eq!(classify_response(&query, &echoed), DnsProbeOutcome::Error);
        Ok(())
    }

    #[test]
    fn query_rejects_overlong_domain_and_non_ascii_label() {
        // Audit #117.
        let long = format!("{}.{}", "a".repeat(63), "b".repeat(200));
        assert!(build_query(&long, TXID).is_err());
        assert!(build_query("例え.jp", TXID).is_err());
    }

    #[test]
    fn mismatched_transaction_is_an_error() -> Result<(), std::io::Error> {
        let query = build_query("example.com", TXID)?;
        let mut response = [0u8; 512];
        response[0] = 0x00;
        response[1] = 0x01;
        assert_eq!(
            classify_response(&query, &response[..12]),
            DnsProbeOutcome::Error
        );
        Ok(())
    }

    #[test]
    fn refused_rcode_is_classified() -> Result<(), std::io::Error> {
        let query = build_query("example.com", TXID)?;
        let mut response = [0u8; 512];
        response[0] = query[0];
        response[1] = query[1];
        response[2] = 0x80; // QR
        response[3] = 0x85; // rcode 5 (REFUSED)
        assert_eq!(
            classify_response(&query, &response[..12]),
            DnsProbeOutcome::Refused
        );
        Ok(())
    }

    #[test]
    fn parses_first_a_record_address() -> Result<(), std::io::Error> {
        let query = build_query("example.com", TXID)?;
        // A minimal response: header + the echoed question + one A answer
        // (name pointer 0xc00c, type A, class IN, ttl 300, rdlength 4, 1.2.3.4).
        let mut response = vec![0u8; 512];
        response[..12].copy_from_slice(&query[..12]);
        response[2] = 0x80; // QR
        response[3] = 0; // rcode 0
        response[6] = 0;
        response[7] = 1; // ANCOUNT = 1
        // Copy the question section (echoed) so the answer lands after it.
        let qlen = 12 + query[12..].len();
        response[12..qlen].copy_from_slice(&query[12..]);
        response[qlen..qlen + 2].copy_from_slice(&[0xc0, 0x0c]); // NAME ptr
        response[qlen + 2..qlen + 4].copy_from_slice(&[0x00, 0x01]); // TYPE A
        response[qlen + 4..qlen + 6].copy_from_slice(&[0x00, 0x01]); // CLASS IN
        response[qlen + 6..qlen + 10].fill(0); // TTL
        response[qlen + 10..qlen + 12].copy_from_slice(&[0x00, 0x04]); // RDLENGTH
        response[qlen + 12..qlen + 16].copy_from_slice(&[1, 2, 3, 4]); // A data
        let classified = classify_response(&query, &response[..qlen + 16]);
        assert_eq!(
            classified,
            DnsProbeOutcome::Resolved(Some(IpAddr::V4(std::net::Ipv4Addr::new(1, 2, 3, 4))))
        );
        Ok(())
    }

    #[test]
    fn probe_reaches_a_local_resolver_over_udp() -> Result<(), std::io::Error> {
        let server = UdpSocket::bind("127.0.0.1:0")?;
        let address = server.local_addr()?;
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 512];
            if let Ok((_, peer)) = server.recv_from(&mut buffer) {
                let mut response = [0u8; 512];
                response[..12].copy_from_slice(&buffer[..12]); // echo header incl id
                response[2] = 0x80; // QR set, rcode 0
                response[3] = 0;
                let _ = server.send_to(&response[..12], peer);
            }
        });
        let outcome = probe_nameserver(
            &address.to_string(),
            "example.com",
            Duration::from_secs(2),
            TXID,
        );
        let _ = handle.join();
        assert_eq!(outcome, DnsProbeOutcome::Resolved(None));
        Ok(())
    }

    #[test]
    fn probe_reaches_an_ipv6_loopback_resolver_when_available() -> Result<(), std::io::Error> {
        // Environments without a v6 loopback cannot run this check; bind
        // failure downgrades the test to a pass instead of a false failure.
        let Ok(server) = UdpSocket::bind("[::1]:0") else {
            return Ok(());
        };
        let address = server.local_addr()?;
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 512];
            if let Ok((_, peer)) = server.recv_from(&mut buffer) {
                let mut response = [0u8; 512];
                response[..12].copy_from_slice(&buffer[..12]);
                response[2] = 0x80;
                let _ = server.send_to(&response[..12], peer);
            }
        });
        let outcome = probe_nameserver(
            &address.to_string(),
            "example.com",
            Duration::from_secs(2),
            TXID,
        );
        let _ = handle.join();
        assert_eq!(
            outcome,
            DnsProbeOutcome::Resolved(None),
            "v6 nameserver must be reachable via a v6-family bind"
        );
        Ok(())
    }

    #[test]
    fn b3_schemed_nameservers_dispatch_instead_of_udp_mangling() {
        // B3 pre-fix: a `tls://` or `https://` value was fed to the UDP
        // socket parser, failed there, and surfaced as Error — reading as
        // "server broken". The structured dispatch answers honestly.
        assert_eq!(
            probe_nameserver(
                "tls://dns.google",
                "example.com",
                Duration::from_secs(1),
                TXID
            ),
            DnsProbeOutcome::UnsupportedTransport
        );
        assert_eq!(
            probe_nameserver(
                "https://dns.google/dns-query",
                "example.com",
                Duration::from_secs(1),
                TXID
            ),
            DnsProbeOutcome::UnsupportedTransport
        );
        assert_eq!(
            probe_nameserver("local", "example.com", Duration::from_secs(1), TXID),
            DnsProbeOutcome::UnsupportedTransport
        );
    }

    #[test]
    fn f1_tcp_probe_round_trips_with_length_framing() -> Result<(), std::io::Error> {
        // F1: a real DNS-over-TCP exchange against a local echo, asserting
        // both the wire framing and the classification come back Resolved.
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = server.local_addr()?;
        let handle = std::thread::spawn(move || {
            if let Ok((mut peer, _)) = server.accept() {
                let mut prefix = [0u8; 2];
                if peer.read_exact(&mut prefix).is_ok() {
                    let length = u16::from_be_bytes(prefix) as usize;
                    let mut query = vec![0u8; length];
                    if peer.read_exact(&mut query).is_ok() {
                        let mut payload = query;
                        if payload.len() >= 12 {
                            payload[2] = 0x80; // QR set, rcode 0
                        }
                        let mut frame = Vec::with_capacity(payload.len() + 2);
                        frame.extend_from_slice(
                            &u16::try_from(payload.len()).unwrap().to_be_bytes(),
                        );
                        frame.extend_from_slice(&payload);
                        let _ = peer.write_all(&frame);
                    }
                }
            }
        });
        let outcome = probe_nameserver(
            &format!("tcp://{address}"),
            "example.com",
            Duration::from_secs(2),
            TXID,
        );
        let _ = handle.join();
        assert_eq!(outcome, DnsProbeOutcome::Resolved(None));
        Ok(())
    }

    #[test]
    fn refused_udp_port_is_error_not_timeout() {
        // A closed loopback UDP port answers with ICMP port-unreachable:
        // classify it as Error (the pre-fix code reported Timeout).
        let outcome = probe_nameserver("127.0.0.1:9", "example.com", Duration::from_secs(2), TXID);
        assert_eq!(
            outcome,
            DnsProbeOutcome::Error,
            "ICMP port-unreachable is a refused service, not silence"
        );
    }

    #[test]
    fn a121_domain_resolution_is_budget_bounded() {
        // #121: a wedged system resolver must cost one Timeout, never an
        // unbounded hang. The injected resolver sleeps far past the budget;
        // the helper thread is the only thing stranded.
        let started = std::time::Instant::now();
        let result = parse_nameserver_with("dns.google", Duration::from_millis(50), |text| {
            assert_eq!(text, "dns.google:53", "bare domain gains the default port");
            std::thread::sleep(Duration::from_secs(3));
            Ok(Vec::new())
        });
        assert!(matches!(
            result.as_ref().map_err(std::io::Error::kind),
            Err(io::ErrorKind::TimedOut)
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "resolution raced the budget instead of waiting out the wedge"
        );
    }

    #[test]
    fn a121_bare_domain_and_bracketed_v6_resolve_with_default_ports() {
        // #121 half: a bare domain is handed to the system resolver with
        // the default port already applied; #123 half: a bare bracketed
        // v6 literal never reaches the resolver at all.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_writer = seen.clone();
        let expected: SocketAddr = "203.0.113.7:53".parse().expect("fixture parse");
        let produced = expected;
        let address = parse_nameserver_with("dns.google", Duration::from_secs(1), move |text| {
            seen_writer.lock().expect("fixture lock").push(text);
            Ok(vec![produced])
        })
        .expect("injected resolver answers");
        assert_eq!(address, expected);
        assert_eq!(
            seen.lock().expect("fixture lock").as_slice(),
            ["dns.google:53"]
        );
        let v6 = parse_nameserver_within("[::1]", Duration::from_secs(1))
            .expect("bare bracketed v6 is a literal with the default port");
        let expected_v6: SocketAddr = "[::1]:53".parse().expect("fixture parse");
        assert_eq!(v6, expected_v6);
    }

    #[test]
    fn a124_stray_datagram_is_dropped_and_the_real_answer_wins() -> Result<(), std::io::Error> {
        // #124 pre-fix: the junk datagram (wrong txid) was terminal Error.
        let server = UdpSocket::bind("127.0.0.1:0")?;
        let address = server.local_addr()?;
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 512];
            if let Ok((read, peer)) = server.recv_from(&mut buffer) {
                // Junk first: valid shape, wrong transaction id.
                let mut junk = [0u8; 12];
                junk.copy_from_slice(&buffer[..12]);
                junk[0] ^= 0xFF;
                junk[2] = 0x80;
                let _ = server.send_to(&junk, peer);
                std::thread::sleep(Duration::from_millis(60));
                if read >= 12 {
                    let mut response = [0u8; 512];
                    response[..12].copy_from_slice(&buffer[..12]);
                    response[2] = 0x80;
                    response[3] = 0;
                    let _ = server.send_to(&response[..12], peer);
                }
            }
        });
        let outcome = probe_nameserver(
            &address.to_string(),
            "example.com",
            Duration::from_secs(2),
            TXID,
        );
        let _ = handle.join();
        assert_eq!(outcome, DnsProbeOutcome::Resolved(None));
        Ok(())
    }

    #[test]
    fn timed_probe_reports_latency() -> Result<(), std::io::Error> {
        let server = UdpSocket::bind("127.0.0.1:0")?;
        let address = server.local_addr()?;
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 512];
            if let Ok((_, peer)) = server.recv_from(&mut buffer) {
                let mut response = [0u8; 512];
                response[..12].copy_from_slice(&buffer[..12]);
                response[2] = 0x80;
                let _ = server.send_to(&response[..12], peer);
            }
        });
        let report = probe_nameserver_timed(
            &address.to_string(),
            "example.com",
            Duration::from_secs(2),
            TXID,
        );
        let _ = handle.join();
        assert_eq!(report.outcome, DnsProbeOutcome::Resolved(None));
        assert!(report.latency < Duration::from_secs(2));
        Ok(())
    }
}
