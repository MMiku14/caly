//! Tests for `subscription/sing_box/node.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use caly_domain::{DialableNode, SubscriptionId};
use serde_json::json;

fn node_from_uri(uri: &str) -> Result<DialableNode, String> {
    let subscription = SubscriptionId::from_bytes([9; 16]);
    caly_subscription::parse_any_proxy_uri(uri, subscription).map_err(|e| format!("{e:?}"))
}

fn rendered(node: &DialableNode) -> Value {
    match node_to_json(node) {
        Ok(json) => json,
        Err(error) => panic!("render must succeed: {error:?}"),
    }
}

#[test]
fn reality_fingerprint_renders_utls_block() -> Result<(), String> {
    let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com&fp=chrome&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk";
    let node = node_from_uri(uri)?;
    let json = rendered(&node);
    let utls = json["tls"]["utls"].clone();
    assert_eq!(utls["enabled"], json!(true));
    assert_eq!(utls["fingerprint"], json!("chrome"));
    assert!(json["tls"]["reality"]["public_key"].is_string());
    Ok(())
}

#[test]
fn unknown_fingerprint_falls_back_to_random() -> Result<(), String> {
    let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&sni=example.com&fp=qq&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk";
    let node = node_from_uri(uri)?;
    let json = rendered(&node);
    assert_eq!(json["tls"]["utls"]["fingerprint"], json!("random"));
    Ok(())
}

#[test]
fn no_fp_param_renders_no_utls_block() -> Result<(), String> {
    let uri =
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&sni=example.com";
    let node = node_from_uri(uri)?;
    let json = rendered(&node);
    assert!(json["tls"]["utls"].is_null(), "no fp means no utls block");
    Ok(())
}

#[test]
fn websocket_early_data_renders_sing_box_fields() -> Result<(), String> {
    // Audit #64: `ed`/`eh` must land in the sing-box ws transport, not be
    // silently dropped between parse and render.
    let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&type=ws&path=%2Fstream&ed=2048&eh=Sec-WebSocket-Protocol#ws-ed";
    let node = node_from_uri(uri)?;
    let json = rendered(&node);
    assert_eq!(json["transport"]["type"], json!("ws"));
    assert_eq!(json["transport"]["max_early_data"], json!(2048));
    assert_eq!(
        json["transport"]["early_data_header_name"],
        json!("Sec-WebSocket-Protocol")
    );
    // A node without `ed` must not sprout the fields either.
    let uri = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls&type=ws&path=%2Fstream#ws-plain";
    let node = node_from_uri(uri)?;
    let json = rendered(&node);
    assert!(json["transport"]["max_early_data"].is_null());
    Ok(())
}

#[test]
fn fingerprint_mapping_is_total() {
    assert_eq!(sing_box_fingerprint("chrome"), "chrome");
    assert_eq!(sing_box_fingerprint("firefox"), "firefox");
    assert_eq!(sing_box_fingerprint("edge"), "edge");
    assert_eq!(sing_box_fingerprint("random"), "random");
    assert_eq!(sing_box_fingerprint("qq"), "random");
    assert_eq!(sing_box_fingerprint(""), "random");
}

/// Plain HTTP proxies render as first-class sing-box `http` outbounds
/// (2026-08-12: previously skipped wholesale — a real airport feed's
/// http/socks5 rows shrank the sing-box pool by dozens of nodes).
#[test]
fn http_proxy_renders_http_outbound() -> Result<(), String> {
    let node = node_from_uri("http://user:pass@proxy.example.com:8080#HTTP - proxy")?;
    let json = rendered(&node);
    assert_eq!(json["type"], "http");
    assert_eq!(json["server"], "proxy.example.com");
    assert_eq!(json["server_port"], 8080);
    assert_eq!(json["username"], "user");
    assert_eq!(json["password"], "pass");
    // sing-box's http outbound has NO UDP path (and no udp_over_tcp field —
    // that option exists only on socks); the renderer must keep http nodes
    // out of UDP routes instead. Forcing the field here made sing-box
    // reject the whole config (2026-08-12 fallback audit).
    assert!(
        json.get("udp_over_tcp").is_none(),
        "http outbound must not carry udp_over_tcp (unknown field to sing-box)"
    );
    assert!(json["tag"].as_str().unwrap().starts_with("proxy-"));
    Ok(())
}

/// HTTP proxies without credentials omit the optional fields entirely.
#[test]
fn http_proxy_without_credentials_omits_auth_fields() -> Result<(), String> {
    let node = node_from_uri("http://proxy.example.com:8080#bare")?;
    let json = rendered(&node);
    assert_eq!(json["type"], "http");
    assert!(json.get("username").is_none());
    assert!(json.get("password").is_none());
    Ok(())
}

/// Socks5 proxies render as sing-box `socks` outbounds.
#[test]
fn socks5_proxy_renders_socks_outbound() -> Result<(), String> {
    let node = node_from_uri("socks5://user:pass@relay.example.com:1080#SOCKS - relay")?;
    let json = rendered(&node);
    assert_eq!(json["type"], "socks");
    // Socks carries UDP either via the service's UDP associate or via the
    // TCP tunnel; the tunnel is forced on so UDP survives servers without
    // associate (2026-08-12 fallback audit: "UDP is not supported by
    // outbound").
    assert_eq!(
        json["udp_over_tcp"], true,
        "socks outbound must enable the UDP-over-TCP tunnel"
    );
    assert_eq!(json["server"], "relay.example.com");
    assert_eq!(json["server_port"], 1080);
    assert_eq!(json["username"], "user");
    assert_eq!(json["password"], "pass");
    Ok(())
}
