//! Tests for `client/execute/mod.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::resolved_core_kind;
use caly_protocol::protocol::v2::WireCoreKind;

#[test]
fn accepts_known_targets() -> Result<(), String> {
    assert_eq!(
        resolved_core_kind(None).map_err(|e| e.clone())?,
        WireCoreKind::Mihomo
    );
    assert_eq!(
        resolved_core_kind(Some("mihomo")).map_err(|e| e.clone())?,
        WireCoreKind::Mihomo
    );
    assert_eq!(
        resolved_core_kind(Some("sing-box")).map_err(|e| e.clone())?,
        WireCoreKind::SingBox
    );
    Ok(())
}

#[test]
fn rejects_unknown_targets_loudly() {
    // A typo must fail instead of silently acting on mihomo.
    assert!(resolved_core_kind(Some("mihomoo")).is_err());
    assert!(resolved_core_kind(Some("singbox")).is_err());
    assert!(resolved_core_kind(Some("")).is_err());
}
