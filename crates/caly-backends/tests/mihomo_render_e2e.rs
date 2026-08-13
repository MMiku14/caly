//! E2E: render subscription bodies to a Mihomo YAML config and validate with
//! the real Mihomo binary (`mihomo -t`). Skips cleanly when the pinned binary
//! is absent. Exercises the render → validate leg of the apply loop.

use std::path::{Path, PathBuf};
use std::process::Command;

use caly_backends::subscription::render_compose::uri_body_to_mihomo_proxy_set;
use caly_coreconf::mihomo::proxy_sections::render_proxy_sections;
use caly_domain::SubscriptionId;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mihomo_binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_MIHOMO_BIN") {
        PathBuf::from(value)
    } else {
        manifest().join("../../vendor/bin/mihomo")
    }
}

/// Assembles a full Mihomo config from the base fields plus a proxy section.
fn assemble(proxy_section: &str) -> String {
    format!(
        "mixed-port: 7890\nallow-lan: false\nmode: rule\nexternal-controller: 127.0.0.1:9090\n{proxy_section}"
    )
}

/// Writes the config to a unique temp dir and runs `mihomo -t` against it.
fn validate_with_mihomo(binary: &Path, yaml: &str) -> Result<(), String> {
    let dir = caly_platform::paths::test_helpers::unique_path_under("caly-mihomo-e2e", "validate");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let config = dir.join("config.yaml");
    std::fs::write(&config, yaml).map_err(|e| e.to_string())?;
    let check = Command::new(binary)
        .args(["-t", "-f"])
        .arg(&config)
        .output()
        .map_err(|e| format!("cannot run mihomo: {e}"))?;
    let _ = std::fs::remove_dir_all(&dir);
    if check.status.success() {
        Ok(())
    } else {
        Err(format!(
            "mihomo -t rejected the rendered config (exit {:?}):\nstdout:\n{}\nstderr:\n{}\n---config---\n{yaml}",
            check.status.code(),
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        ))
    }
}

#[test]
fn synthetic_subscription_renders_config_the_binary_accepts() -> Result<(), String> {
    let binary = mihomo_binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return Ok(());
    }
    let ss = "ss://Y2hhY2hhMjAtaWV0Zi1wb2x5MTMwNTpzZWNyZXRwYXNz@example.com:8443#ss-node";
    let body = format!(
        "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls#vless-node\n\
         trojan://secret-pass@example.com:443?security=tls#trojan-node\n{ss}\n"
    )
    .into_bytes();
    let id = SubscriptionId::from_bytes([13; 16]);
    let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("render: {e:?}"))?;
    assert_eq!(set.len(), 3);
    let section = render_proxy_sections(&set);
    let yaml = assemble(&section);
    validate_with_mihomo(&binary, &yaml)
}

#[test]
fn real_fixture_renders_config_the_binary_accepts() -> Result<(), String> {
    let binary = mihomo_binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return Ok(());
    }
    let body = std::fs::read(manifest().join("../../fixtures/subscription-20260803.txt"))
        .map_err(|_| "fixture not found")?;
    let id = SubscriptionId::from_bytes([14; 16]);
    let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("render: {e:?}"))?;
    assert!(!set.is_empty(), "fixture must yield at least one proxy");
    let section = render_proxy_sections(&set);
    let yaml = assemble(&section);
    validate_with_mihomo(&binary, &yaml)
}
