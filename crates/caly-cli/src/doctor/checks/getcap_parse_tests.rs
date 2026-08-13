//! Tests for `doctor.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[test]
fn parse_getcap_line_matches_exact_capability_only() {
    assert!(parse_getcap_line("/usr/bin/mihomo cap_net_admin=ep"));
    assert!(parse_getcap_line(
        "/usr/bin/sing-box cap_net_bind_service,cap_net_admin=ep"
    ));
    assert!(!parse_getcap_line(
        "/usr/bin/sing-box cap_net_bind_service=ep"
    ));
    assert!(!parse_getcap_line("/opt/bin/x cap_net_admin_stat=ep"));
    assert!(!parse_getcap_line("/usr/bin/mihomo"));
    assert!(!parse_getcap_line(""));
}
