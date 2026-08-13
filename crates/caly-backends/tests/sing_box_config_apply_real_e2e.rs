//! E2E: the full sing-box subscription → render → validate → apply loop
//! against the real kernel binary. Feeds a subscription body through the
//! cached subscription backend (which indexes the shared proxy registry with
//! per-node sing-box outbounds), then drives the SingBoxConfigBackend to
//! parse+render+validate and commit. Skips when the pinned binary is absent.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use caly_backends::{CachedSubscriptionBackend, config::sing_box::SingBoxConfigBackend};
use caly_domain::SubscriptionId;
use caly_platform::paths::test_helpers::unique_path_under;
use caly_ports::{ConfigActorPort, ConfigCandidate, SubscriptionCommandBackend};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sing_box_binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_SINGBOX_BIN") {
        PathBuf::from(value)
    } else {
        manifest().join("../../vendor/bin/sing-box")
    }
}

fn unique_dir(tag: &str) -> PathBuf {
    unique_path_under("caly-singbox-apply-e2e", tag)
}

#[test]
fn subscription_refresh_drives_validated_sing_box_config_apply() -> Result<(), String> {
    let binary = sing_box_binary();
    if !binary.is_file() {
        eprintln!("skipping: no sing-box binary at {}", binary.display());
        return Ok(());
    }
    let dir = unique_dir("singbox-loop");
    let workdir = dir.join("work");
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
    let destination = dir.join("sing-box.json");

    // 1. Build a real fixture body (base64 URI-line subscription).
    let body = std::fs::read(manifest().join("../../fixtures/subscription-20260803.txt"))
        .map_err(|_| "fixture not found")?;
    let id = SubscriptionId::from_bytes([22; 16]);
    let registry: caly_backends::CoreNodeRegistry =
        Arc::new(Mutex::new(std::collections::BTreeMap::new()));

    // 2. Feed + refresh the subscription so the registry indexes both the
    //    Mihomo YAML and the per-node sing-box outbound JSON.
    let mut subscription = CachedSubscriptionBackend::new().with_node_registry(registry.clone());
    subscription.put_source(id, body);
    let nodes = subscription
        .refresh(id, caly_ports::RefreshMode::default())
        .map_err(|e| format!("subscription refresh failed: {e:?}"))?
        .nodes;
    assert!(!nodes.is_empty(), "fixture must project nodes");
    let mapping = registry.lock().map_err(|_| "poisoned")?;
    assert!(mapping.len() >= nodes.len());
    let singbox_indexed = mapping.values().filter(|p| p.singbox.is_some()).count();
    assert!(
        singbox_indexed > 0,
        "registry must carry per-node sing-box outbounds for the fixture"
    );
    drop(mapping);

    // 3. Apply: render → validate (sing-box check in parse_and_render) → commit.
    let mut config = SingBoxConfigBackend::new(destination.clone())
        .with_registry(registry)
        .with_validation(binary.clone(), workdir, Duration::from_secs(20));
    let prepared = config
        .parse_and_render(ConfigCandidate { id: [9; 16] })
        .map_err(|e| format!("parse_and_render failed: {e:?}"))?;
    let committed = config
        .commit_candidate(prepared)
        .map_err(|e| format!("commit failed: {e:?}"))?;
    assert_eq!(committed.generation, 1);

    // 4. The committed document is valid JSON with registry outbounds and the
    //    clash API controller; real-kernel `sing-box check` already passed in
    //    parse_and_render.
    let text = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("committed config is not JSON: {e}"))?;
    let outbounds = value["outbounds"]
        .as_array()
        .ok_or_else(|| "committed config has no outbounds".to_owned())?;
    assert!(
        outbounds.len() >= 2,
        "expected registry outbounds + direct/selectors"
    );
    assert!(
        text.contains("proxy-"),
        "registry outbound tags must render"
    );
    assert!(
        text.contains("clash_api"),
        "clash api controller must render"
    );

    // 5. Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
