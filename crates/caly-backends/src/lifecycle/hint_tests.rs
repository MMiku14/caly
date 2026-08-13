//! Tests for `lifecycle.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::tun_cap_net_admin_hint;

#[test]
fn no_hint_when_tun_disabled() {
    assert!(tun_cap_net_admin_hint(false, "sing-box", "anything").is_empty());
}

#[test]
fn cap_net_admin_hint_for_plain_failure() {
    let hint = tun_cap_net_admin_hint(true, "sing-box", "process exited before becoming ready");
    assert!(hint.contains("CAP_NET_ADMIN"), "{hint}");
    assert!(hint.contains("caly doctor --fix"), "{hint}");
}

#[test]
fn route_conflict_hint_when_stderr_mentions_file_exists() {
    let message = "process exited before becoming ready (child stderr: \
                   start service: post-start inbound/tun[tun-in]: starting TUN interface: \
                   set routes: add route 172.18.0.0/30: file exists)";
    let hint = tun_cap_net_admin_hint(true, "sing-box", message);
    assert!(hint.contains("another process owns"), "{hint}");
    assert!(!hint.contains("CAP_NET_ADMIN"), "{hint}");
}

#[test]
fn route_conflict_hint_for_address_in_use() {
    let hint = tun_cap_net_admin_hint(true, "mihomo", "tun: address already in use");
    assert!(hint.contains("another process owns"), "{hint}");
}

#[test]
fn plain_failure_with_route_word_keeps_cap_hint() {
    // A route word alone (without add-route/tun context) is not a conflict.
    let hint = tun_cap_net_admin_hint(true, "mihomo", "routing table missing");
    assert!(hint.contains("CAP_NET_ADMIN"), "{hint}");
}

#[test]
fn route_conflict_hint_is_case_insensitive() {
    // Audit #26: core error capitalization must not change the verdict.
    let hint = tun_cap_net_admin_hint(true, "mihomo", "tun: add route: File Exists");
    assert!(hint.contains("another process owns"), "{hint}");
}
