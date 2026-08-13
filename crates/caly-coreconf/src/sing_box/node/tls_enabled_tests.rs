//! Tests for `subscription/sing_box/node.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use caly_domain::SubscriptionId;
use serde_json::json;

#[test]
fn tls_block_always_has_enabled_true() -> Result<(), String> {
    let subscription = SubscriptionId::from_bytes([11; 16]);
    for uri in [
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com&fp=chrome&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk",
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&sni=example.com",
    ] {
        let node = caly_subscription::parse_any_proxy_uri(uri, subscription)
            .map_err(|e| format!("{e:?}"))?;
        let json = match node_to_json(&node) {
            Ok(json) => json,
            Err(error) => panic!("render: {error:?}"),
        };
        assert_eq!(
            json["tls"]["enabled"],
            json!(true),
            "tls block must carry enabled:true (nil-config panic guard): {json}"
        );
    }
    Ok(())
}
