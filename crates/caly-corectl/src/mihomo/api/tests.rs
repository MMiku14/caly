//! Tests for `mihomo/api.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[test]
fn traffic_frame_prefers_per_second_rates_over_totals() {
    // Mihomo emits both; the per-second snapshot must win so telemetry
    // shows a rate instead of an ever-growing counter.
    let mihomo = serde_json::json!({ "up": 60, "down": 120, "upTotal": 500, "downTotal": 700 });
    let (download, upload) = parse_traffic_frame(&mihomo.to_string()).unwrap();
    assert_eq!((download, upload), (120, 60));
}

#[test]
fn traffic_frame_falls_back_to_totals_for_total_only_kernels() {
    let classic = r#"{"downloadTotal": 9, "uploadTotal": 4}"#;
    let (download, upload) = parse_traffic_frame(classic).unwrap();
    assert_eq!((download, upload), (9, 4));
}

#[test]
fn traffic_field_accepts_all_naming_conventions() {
    let sing = serde_json::json!({ "up": 11, "down": 22 });
    assert_eq!(
        traffic_field(&sing, &["down", "downloadTotal", "downTotal"]),
        Some(22)
    );
    assert_eq!(
        traffic_field(&sing, &["up", "uploadTotal", "upTotal"]),
        Some(11)
    );
    // Missing fields fall back to None -> caller uses 0.
    assert_eq!(
        traffic_field(
            &serde_json::json!({}),
            &["down", "downloadTotal", "downTotal"]
        ),
        None
    );
}

#[test]
fn sse_frame_scanner_waits_for_a_complete_json_line() {
    // HTTP headers + a partial frame: not complete yet.
    let partial = b"HTTP/1.1 200 OK\r\n\r\ndata: {\"up\":1";
    assert!(first_complete_frame(partial).is_none());
    // Completed frame (headers skipped, data: prefix stripped).
    let complete = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"up\":1,\"down\":2}\n\n";
    assert_eq!(
        first_complete_frame(complete).as_deref(),
        Some("{\"up\":1,\"down\":2}")
    );
    // Bare JSON without the SSE prefix works as well.
    assert_eq!(
        first_complete_frame(b"{\"down\":9}\n").as_deref(),
        Some("{\"down\":9}")
    );
}

#[test]
fn sse_reader_assembles_a_frame_split_across_chunks() {
    // One byte at a time: the worst fragmentation a TCP stack can produce.
    struct Dribble<'a> {
        bytes: &'a [u8],
    }
    impl Read for Dribble<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.bytes.is_empty() {
                return Ok(0);
            }
            out[0] = self.bytes[0];
            self.bytes = &self.bytes[1..];
            Ok(1)
        }
    }
    let wire = b"HTTP/1.1 200 OK\r\n\r\ndata: {\"up\":7,\"down\":8}\n\n";
    let mut stream = Dribble { bytes: wire };
    let frame = read_first_sse_frame(&mut stream).unwrap();
    assert_eq!(frame, "{\"up\":7,\"down\":8}");
    let (download, upload) = parse_traffic_frame(&frame).unwrap();
    assert_eq!((download, upload), (8, 7));
}

#[test]
fn total_fields_take_priority_over_delta_frames() {
    // Mihomo frame: cumulative totals win (exact semantics).
    let mihomo =
        serde_json::json!({"up": 2048, "down": 4096, "upTotal": 100_000, "downTotal": 200_000});
    let totals = parse_traffic_total(&mihomo.to_string()).expect("totals");
    assert_eq!(totals, (200_000, 100_000));
    // Delta-only frame (sing-box): no totals → None, rate available.
    let singbox = serde_json::json!({"up": 512, "down": 1024});
    assert!(parse_traffic_total(&singbox.to_string()).is_none());
    assert_eq!(parse_traffic_rate(&singbox.to_string()), Some((1024, 512)));
}
