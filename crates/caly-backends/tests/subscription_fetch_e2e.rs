//! Real-network subscription fetch E2E against a local HTTP server.
//!
//! Starts a bounded local HTTP server serving subscription bodies, then drives
//! `fetch_pinned` over the real HTTP stack and verifies the decoded projection.
//! A separate test renders a minimal vless/trojan subscription to sing-box JSON
//! and validates it with the real `sing-box check -c` binary when present.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // #53: integration-test helpers use unwrap/expect freely
use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener},
    path::PathBuf,
    process::Command,
    thread,
};

use caly_backends::subscription::render_compose::uri_body_to_sing_box_json;
use caly_subscription::{
    AddressClassifier, FetchPolicy, FetchResult, ResolvedAddresses, fetch_pinned,
    parse_uri_body_to_display_lossy,
};

/// A classifier that accepts the local loopback server for the E2E.
struct LoopbackClassifier;
impl AddressClassifier for LoopbackClassifier {
    fn is_globally_routable(&self, address: IpAddr) -> bool {
        address.is_loopback()
    }
}

/// Minimal single-connection HTTP/1.1 responder that serves a fixed body.
fn spawn_fixture_server(body: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut buf = [0_u8; 2048];
            let _ = stream.read(&mut buf);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    addr
}

fn manifest() -> PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap()
}

/// Serves `body` over a local HTTP server and fetches it through the real stack.
fn fetch_bytes(body: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let addr = spawn_fixture_server(body);
    let url = format!("http://{addr}/subscription");
    let addresses =
        ResolvedAddresses::try_from_vec(vec![addr.ip()]).map_err(|_| "too many addresses")?;
    let validators = caly_subscription::FetchValidators {
        etag: None,
        last_modified: None,
    };
    let result = {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime
            .block_on(fetch_pinned(
                &url,
                addresses,
                &LoopbackClassifier,
                FetchPolicy::direct_default(),
                &validators,
            ))
            .map_err(|error| format!("fetch failed: {error:?}"))?
    };
    let FetchResult::Updated { body: fetched, .. } = result else {
        return Err("expected Updated fetch result".into());
    };
    Ok(fetched.into_vec())
}

#[test]
fn real_http_fetch_decodes_fixture_projection() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = manifest().join("../../fixtures/subscription-20260803.txt");
    let body = std::fs::read(&fixture)?;
    let fetched = fetch_bytes(body)?;
    assert!(!fetched.is_empty(), "fetched body must not be empty");

    // Decode the fetched bytes into a node projection (lossy; rejects are counted).
    let id = caly_domain::SubscriptionId::from_bytes([9; 16]);
    let projection = parse_uri_body_to_display_lossy(fetched, id).map_err(|e| format!("{e:?}"))?;
    assert!(
        !projection.nodes.is_empty(),
        "fixture must project at least one node"
    );
    Ok(())
}

#[test]
fn render_and_validate_sing_box_config_with_real_binary() -> Result<(), Box<dyn std::error::Error>>
{
    // Minimal subscription with only protocols the strict sing-box renderer supports.
    let mini = b"vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls#vless-test\ntrojan://secret@example.com:443?security=tls#trojan-test\n";
    let fetched = fetch_bytes(mini.to_vec())?;

    let id = caly_domain::SubscriptionId::from_bytes([10; 16]);
    let json = uri_body_to_sing_box_json(fetched, id)
        .map_err(|error| format!("sing-box render failed: {error:?}"))?;
    let text = String::from_utf8_lossy(&json);
    assert!(
        text.contains("\"PROXY\""),
        "rendered sing-box config must contain a PROXY selector"
    );

    // Validate with the real binary when present.
    let binary = manifest().join("../../vendor/bin/sing-box");
    if binary.is_file() {
        let dir = caly_platform::paths::test_helpers::unique_path_under("caly-sub-e2e", "fetch");
        std::fs::create_dir_all(&dir)?;
        let config = dir.join("config.json");
        std::fs::write(&config, &json)?;
        let output = Command::new(&binary)
            .arg("check")
            .arg("-c")
            .arg(&config)
            .output()?;
        assert!(
            output.status.success(),
            "sing-box check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

#[test]
fn fetch_rejects_non_public_resolution_with_public_classifier()
-> Result<(), Box<dyn std::error::Error>> {
    // The default public classifier must reject loopback, proving the SSRF guard.
    struct Public;
    impl AddressClassifier for Public {
        fn is_globally_routable(&self, address: IpAddr) -> bool {
            match address {
                IpAddr::V4(value) => !value.is_loopback() && !value.is_private(),
                IpAddr::V6(value) => !value.is_loopback(),
            }
        }
    }
    let addr: SocketAddr = "127.0.0.1:1".parse()?;
    let addresses =
        ResolvedAddresses::try_from_vec(vec![addr.ip()]).map_err(|_| "too many addresses")?;
    let url = format!("http://{addr}/x");
    let validators = caly_subscription::FetchValidators {
        etag: None,
        last_modified: None,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(fetch_pinned(
        &url,
        addresses,
        &Public,
        FetchPolicy::direct_default(),
        &validators,
    ));
    assert!(
        result.is_err(),
        "loopback must be rejected by public classifier"
    );
    Ok(())
}
