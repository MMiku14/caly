//! Tests for `daemon.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[test]
fn session_token_differs_from_instance_id() {
    let instance = [7u8; 16];
    let token = session_token_for(instance);
    assert_ne!(
        token, instance,
        "the session token must never equal the public daemon-instance id (#28)"
    );
}

#[test]
fn session_token_is_deterministic_per_secret() {
    let instance = [9u8; 16];
    assert_eq!(session_token_for(instance), session_token_for(instance));
}
