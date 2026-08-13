//! Tests for `daemon.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[tokio::test]
async fn fatal_signal_completes_transport_shutdown_future() {
    let (sender, receiver) = tokio::sync::watch::channel(None);
    sender.send_replace(Some(caly_application::runtime::FatalFault::Projection));
    let completed = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        wait_for_fatal(receiver),
    )
    .await;
    assert!(completed.is_ok());
}

// ── audit #81: the periodic-refresh operation id must never panic ──

#[test]
fn periodic_operation_id_constructs_and_carries_millis_and_tag() {
    // Pre-#81 `copy_from_slice(b"caly.tick")` panicked on every tick:
    // the tag literal is 9 bytes wide while `bytes[8..]` is exactly 8.
    let id = super::periodic_operation_id();
    let bytes = id.into_bytes();
    assert_eq!(&bytes[8..], b"caly.tik");
    let millis = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
    assert!(millis > 0, "millis tag must be embedded in the id");
}
