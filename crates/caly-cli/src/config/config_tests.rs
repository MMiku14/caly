//! Unit tests for the layered config read/parse surface (P8a: moved up
//! from the daemon host so offline CLI commands share one loader).

use super::*;
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| 0, |duration| duration.as_nanos());
    let dir = std::env::temp_dir().join(format!("caly-config-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

#[test]
fn absent_config_yields_no_override() -> Result<(), String> {
    let root = temp_root("absent");
    assert_eq!(
        core_override_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        None
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_core_selects_mihomo() -> Result<(), String> {
    let root = temp_root("mihomo");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\n",
    )
    .map_err(|e| e.to_string())?;
    assert_eq!(
        core_override_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        Some(CoreKind::Mihomo)
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_core_selects_sing_box() -> Result<(), String> {
    let root = temp_root("sing");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: sing-box\n",
    )
    .map_err(|e| e.to_string())?;
    assert_eq!(
        core_override_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        Some(CoreKind::SingBox)
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_xray_is_rejected_as_unsupported() {
    let root = temp_root("xray");
    fs::write(root.join("config.yaml"), "schema_version: 1\ncore: xray\n").unwrap_or_default();
    assert!(matches!(
        core_override_from(root.clone()),
        Err(DaemonConfigError::UnsupportedCore)
    ));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn invalid_config_fails_boot() {
    let root = temp_root("bad");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: [not-a-scalar\n",
    )
    .unwrap_or_default();
    assert!(matches!(
        core_override_from(root.clone()),
        Err(DaemonConfigError::Layered(_))
    ));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn controllers_from_config_falls_back_to_defaults() -> Result<(), String> {
    let root = temp_root("controllers");
    // Absent config -> built-in defaults.
    let absent = controllers_from(root.clone()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(absent.mihomo, "127.0.0.1:9090");
    assert_eq!(absent.sing_box, "127.0.0.1:9091");
    // Present config -> configured addresses.
    fs::write(
            root.join("config.yaml"),
            "schema_version: 1\ncore: mihomo\ncontrollers:\n  mihomo: 127.0.0.1:9200\n  sing_box: 127.0.0.1:9201\n",
        )
        .map_err(|e| e.to_string())?;
    let configured = controllers_from(root.clone()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(configured.mihomo, "127.0.0.1:9200");
    assert_eq!(configured.sing_box, "127.0.0.1:9201");
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn kernel_defaults_apply_without_config() {
    let root = temp_root("kernel-absent");
    let kernel = kernel_from(root.clone());
    assert_eq!(kernel.mixed_port, 7890);
    assert!(!kernel.allow_lan);
    assert_eq!(kernel.log_level, "error");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn kernel_values_load_from_config() {
    let root = temp_root("kernel-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nkernel:\n  mixed_port: 8899\n  allow_lan: true\n  log_level: warn\n",
    )
    .unwrap_or_default();
    let kernel = kernel_from(root.clone());
    assert_eq!(kernel.mixed_port, 8899);
    assert!(kernel.allow_lan);
    assert_eq!(kernel.log_level, "warn");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn kernel_restart_bounds_load_from_config() {
    let root = temp_root("restart-bounds");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nkernel:\n  restart:\n    initial_backoff_ms: 250\n    max_backoff_ms: 4000\n",
    )
    .unwrap_or_default();
    let kernel = kernel_from(root.clone());
    assert_eq!(kernel.restart.initial_backoff_ms, 250);
    assert_eq!(kernel.restart.max_backoff_ms, 4000);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn fetch_policy_defaults_and_overrides() {
    let root = temp_root("fetch-absent");
    let policy = fetch_policy_from(root.clone());
    assert_eq!(policy.connect_timeout.as_millis(), 5000);
    assert_eq!(policy.max_body_bytes, 32 * 1024 * 1024);
    assert!(!policy.redirects_allowed);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("fetch-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nsubscriptions:\n  connect_timeout_ms: 900\n  request_timeout_ms: 2000\n  max_body_mb: 4\n  follow_redirects: true\n",
    )
    .unwrap_or_default();
    let policy = fetch_policy_from(root.clone());
    assert_eq!(policy.connect_timeout.as_millis(), 900);
    assert_eq!(policy.request_timeout.as_millis(), 2000);
    assert_eq!(policy.max_body_bytes, 4 * 1024 * 1024);
    assert!(policy.redirects_allowed);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn system_proxy_endpoint_defaults_to_mixed_port() {
    let root = temp_root("proxy-endpoint-default");
    let (host, port) = system_proxy_endpoint_from(root.clone());
    assert_eq!(host, "127.0.0.1");
    assert_eq!(port, 7890);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("proxy-endpoint-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nkernel:\n  mixed_port: 8899\nsystem_proxy:\n  enabled: false\n  host: 10.0.0.2\n",
    )
    .unwrap_or_default();
    let (host, port) = system_proxy_endpoint_from(root.clone());
    assert_eq!(host, "10.0.0.2");
    assert_eq!(port, 8899);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("proxy-endpoint-port");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nsystem_proxy:\n  enabled: false\n  host: 127.0.0.1\n  port: 1080\n",
    )
    .unwrap_or_default();
    let (host, port) = system_proxy_endpoint_from(root.clone());
    assert_eq!(host, "127.0.0.1");
    assert_eq!(port, 1080);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn dns_nameservers_lists_configured_resolvers() {
    let root = temp_root("dns-nameservers");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndns:\n  enabled: false\n  nameservers:\n    - 9.9.9.9\n",
    )
    .unwrap_or_default();
    // dns_nameservers reads the raw config section even when disabled.
    let config = load_from(root.clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("config must load"));
    assert_eq!(config.dns.nameservers, vec!["9.9.9.9"]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn tun_escalation_maps_config_values() {
    use caly_platform::tun::TunEscalation;
    let root = temp_root("escalation-absent");
    assert_eq!(
        tun_escalation_from(root.clone()),
        TunEscalation::Auto
    );
    fs::remove_dir_all(&root).ok();

    for (value, expected) in [
        ("sudo", TunEscalation::Sudo),
        ("pkexec", TunEscalation::Pkexec),
        ("none", TunEscalation::None),
        ("auto", TunEscalation::Auto),
    ] {
        let root = temp_root(&format!("escalation-{value}"));
        fs::write(
            root.join("config.yaml"),
            format!(
                "schema_version: 1\ncore: mihomo\ntun:\n  enabled: false\n  escalation: {value}\n"
            ),
        )
        .unwrap_or_default();
        assert_eq!(
            tun_escalation_from(root.clone()),
            expected,
            "escalation `{value}` must map correctly"
        );
        fs::remove_dir_all(&root).ok();
    }
}
