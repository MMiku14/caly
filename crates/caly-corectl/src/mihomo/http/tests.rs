//! Tests for `mihomo/http.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use std::io::{Read, Write};
use std::{net::TcpListener, thread};

#[test]
fn health_probe_accepts_bounded_success_response() -> Result<(), KernelFailure> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::ApiUnavailable,
            "test listener failed",
            "retry test",
        )
    })?;
    let address = listener.local_addr().map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::ApiUnavailable,
            "test address failed",
            "retry test",
        )
    })?;
    let worker = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    let mut control = MihomoHttpControl::new(address.to_string(), None)?;
    control.health_check(Duration::from_secs(1))?;
    worker.join().map_err(|_| {
        crate::common::failure_with_kind(
            KernelFailureKind::ApiUnavailable,
            "test worker failed",
            "retry test",
        )
    })?;
    Ok(())
}

// ── audit #83: hostile chunk sizes must never overflow ─────────

#[test]
fn chunked_decoder_rejects_oversized_size_line_without_panic() {
    // `usize::MAX` as a hex size line: the pre-fix `size + 2` overflowed
    // (debug panic) or wrapped (release bounds bypass).
    let body = b"ffffffffffffffff\r\nAB\r\n";
    assert_eq!(decode_chunked_body(body), Err(()));
}

#[test]
fn chunked_decoder_accepts_a_well_formed_document() -> Result<(), String> {
    let body = b"4\r\nwiki\r\n5\r\npedia\r\n0\r\n\r\n";
    match decode_chunked_body(body) {
        Ok(text) if text == "wikipedia" => Ok(()),
        other => Err(format!("unexpected decode outcome: {other:?}")),
    }
}

#[test]
fn chunked_decoder_rejects_missing_trailing_crlf() {
    // Chunk payload not followed by CRLF is malformed (RFC 7230 §4.1).
    let body = b"2\r\nokXX\r\n";
    assert_eq!(decode_chunked_body(body), Err(()));
}

#[test]
fn chunked_decoder_tolerates_chunk_extensions() -> Result<(), String> {
    let body = b"3;foo=bar\r\nabc\r\n0\r\n\r\n";
    match decode_chunked_body(body) {
        Ok(text) if text == "abc" => Ok(()),
        other => Err(format!("unexpected decode outcome: {other:?}")),
    }
}
