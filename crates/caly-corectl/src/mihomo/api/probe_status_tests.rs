//! Tests for `mihomo/api.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[test]
fn status_code_parses_http_versions_and_reason_phrases() {
    assert_eq!(http_status_code("HTTP/1.1 200 OK\r\n\r\n"), Some(200));
    assert_eq!(
        http_status_code("HTTP/1.0 503 Service Unavailable\r\nContent-Length: 0\r\n"),
        Some(503)
    );
    assert_eq!(http_status_code("HTTP/1.1 404 Not Found\r\n"), Some(404));
    // A body without a status head yields no status.
    assert_eq!(http_status_code("{\"delay\":42}"), None);
    assert_eq!(http_status_code(""), None);
}
