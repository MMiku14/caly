#![allow(clippy::panic)]

//! Real-kernel sing-box E2E test against a real sing-box binary.
//!
//! Drives the stack the daemon composes (`SingBoxRuntime` +
//! `LinuxProcessSpawner` + `SingBoxConfigRenderer`) against the vendored
//! `vendor/bin/sing-box` binary. Skips gracefully when the binary is absent or
//! not executable, so an unprivileged `cargo test --all` never fails.
//!
//! Run with:
//!   cargo test -p caly-corectl --test sing_box_e2e --locked -- --nocapture

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::Command,
    time::Duration,
};

use caly_coreconf::sing_box::{SingBoxBaseTuning, SingBoxConfigRenderer};
use caly_corectl::contract::KernelControl;
use caly_corectl::sing_box::{SingBoxHttpControl, SingBoxRuntime};

/// Vendored sing-box binary, overridable for packaging layouts.
fn binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_SINGBOX_BIN") {
        return PathBuf::from(value);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bin/sing-box")
}

/// True only if a real sing-box binary exists and answers `version`.
fn available() -> bool {
    let binary = binary();
    if !binary.is_file() {
        eprintln!("skipping: no sing-box binary at {}", binary.display());
        return false;
    }
    match Command::new(&binary).arg("version").output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "skipping: sing-box version failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            false
        }
        Err(error) => {
            eprintln!("skipping: cannot run sing-box: {error}");
            false
        }
    }
}

/// Per-process unique Clash API controller port for parallel runs. Each test
/// passes a distinct `offset` so concurrently-running tests never collide on
/// the same controller port within one process.
fn test_controller(offset: u16) -> (u16, String) {
    let base = 26_000 + (std::process::id() % 4_000) as u16;
    let port = base.saturating_add(offset);
    (port, format!("127.0.0.1:{port}"))
}

/// Per-test unique working directory (parallel tests must not share one).
fn working_directory(tag: &str) -> PathBuf {
    caly_platform::paths::test_helpers::unique_path_under("caly-e2e-singbox", tag)
}

/// Raw GET /version against the sing-box Clash API.
fn fetch_version(controller_port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", controller_port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .write_all(
            format!("GET /version HTTP/1.1\r\nHost: 127.0.0.1:{controller_port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    Some(body)
}

type E2eResult = Result<(), Box<dyn std::error::Error>>;

fn e2e_error(value: impl std::fmt::Debug) -> Box<dyn std::error::Error> {
    Box::<dyn std::error::Error>::from(format!("{value:?}"))
}

fn write_config(
    dir: &std::path::Path,
    controller: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let bytes = SingBoxConfigRenderer
        .render_tuned(&SingBoxBaseTuning {
            controller: controller.to_owned(),
            ..SingBoxBaseTuning::default()
        })
        .map_err(e2e_error)?;
    let config = dir.join("config.json");
    std::fs::write(&config, bytes)?;
    Ok(config)
}

struct E2eLockGuard(std::path::PathBuf);

impl Drop for E2eLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn e2e_lock() -> E2eLockGuard {
    let path = std::env::temp_dir().join("caly-e2e-suite.lock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        match std::fs::create_dir(&path) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(path.join("pid"))
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok());
                let holder_alive = holder.is_some_and(|pid| {
                    std::process::Command::new("kill")
                        .args(["-0", &pid.to_string()])
                        .status()
                        .is_ok_and(|status| status.success())
                });
                if !holder_alive {
                    let _ = std::fs::remove_dir_all(&path);
                    continue;
                }
                if std::time::Instant::now() > deadline {
                    panic!("timed out waiting for the e2e suite lock");
                } else {
                    // keep waiting for the live holder
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => panic!("cannot acquire the e2e suite lock: {error}"),
        }
    }
    let _ = std::fs::write(path.join("pid"), std::process::id().to_string());
    E2eLockGuard(path)
}

#[test]
fn sing_box_rendered_config_passes_real_binary_check() -> E2eResult {
    let _guard = e2e_lock();

    if !available() {
        return Ok(());
    }
    let (_, controller) = test_controller(0);
    let dir = working_directory("validate");
    let config = write_config(&dir, &controller)?;

    let check = Command::new(binary())
        .args(["check", "-c"])
        .arg(&config)
        .output()?;
    assert!(
        check.status.success(),
        "sing-box check rejected the rendered config: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn sing_box_dns_config_passes_real_binary_check() -> E2eResult {
    use caly_dns::{DnsMode, DnsSettingsBuilder};
    let _guard = e2e_lock();
    if !available() {
        return Ok(());
    }
    let (_, controller) = test_controller(1);
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")?
        .push_nameserver("1.1.1.1")?
        .push_fallback("tls://dns.google")?
        .push_direct("223.5.5.5")?
        .fake_ip_range("198.18.0.1/16")?
        .build()?
        .ok_or_else(|| e2e_error("dns settings disabled"))?;
    let dir = working_directory("dns-validate");
    std::fs::create_dir_all(&dir)?;
    let config = dir.join("config.json");
    let bytes = SingBoxConfigRenderer
        .render_tuned(&SingBoxBaseTuning {
            controller: controller.clone(),
            dns: Some(dns),
            ..SingBoxBaseTuning::default()
        })
        .map_err(e2e_error)?;
    std::fs::write(&config, bytes)?;

    let check = Command::new(binary())
        .args(["check", "-c"])
        .arg(&config)
        .output()?;
    assert!(
        check.status.success(),
        "sing-box check rejected the DNS config: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn sing_box_real_binary_start_ready_restart_stop() -> E2eResult {
    let _guard = e2e_lock();

    if !available() {
        return Ok(());
    }
    let (controller_port, controller) = test_controller(2);
    let dir = working_directory("lifecycle");
    let config = write_config(&dir, &controller)?;

    let mut runtime = SingBoxRuntime::new(binary(), dir.clone(), controller, config, 1, None)
        .map_err(e2e_error)?;

    // Real process start + Clash API readiness.
    runtime.start(Duration::from_secs(20)).map_err(e2e_error)?;
    let version = fetch_version(controller_port).ok_or("clash api unreachable after start")?;
    assert!(
        version.contains("\"version\""),
        "unexpected /version response: {version}"
    );
    runtime
        .health_check(Duration::from_secs(3))
        .map_err(e2e_error)?;

    // Restart into the same config generation.
    runtime
        .restart(Duration::from_secs(20))
        .map_err(e2e_error)?;
    let version_2 = fetch_version(controller_port).ok_or("clash api unreachable after restart")?;
    assert!(
        version_2.contains("\"version\""),
        "unexpected /version after restart: {version_2}"
    );

    // Stop and reap; Clash API must be unreachable afterwards.
    runtime.stop(Duration::from_secs(5)).map_err(e2e_error)?;
    assert!(
        fetch_version(controller_port).is_none(),
        "clash api still reachable after stop"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Real-kernel API probe test: exercises the Clash-compatible proxy-groups,
/// connections and traffic adapters against a live sing-box binary.
#[test]
fn sing_box_real_binary_clash_api_adapters() -> E2eResult {
    let _guard = e2e_lock();

    if !available() {
        return Ok(());
    }
    let (controller_port, controller) = test_controller(3);
    let dir = working_directory("api");
    let config = write_config(&dir, &controller)?;

    let mut runtime =
        SingBoxRuntime::new(binary(), dir.clone(), controller.clone(), config, 1, None)
            .map_err(e2e_error)?;
    runtime.start(Duration::from_secs(20)).map_err(e2e_error)?;

    // The empty renderer has no groups/connections, but the endpoints must be
    // reachable and parse into empty summaries rather than erroring.
    let mut control = SingBoxHttpControl::new(controller, None).map_err(e2e_error)?;
    let groups = control
        .proxy_groups(Duration::from_secs(3))
        .map_err(e2e_error)?;
    let connections = control
        .connections(Duration::from_secs(3))
        .map_err(e2e_error)?;
    let traffic = control.traffic(Duration::from_secs(3)).map_err(e2e_error)?;

    // Groups should be empty (no proxies configured); connections/traffic must
    // parse to bounded values without erroring.
    let _ = groups;
    assert_eq!(connections.active, 0);
    let (down, up) = traffic;
    assert!(
        down == 0 && up == 0,
        "expected zero traffic, got {down}/{up}"
    );

    runtime.stop(Duration::from_secs(5)).map_err(e2e_error)?;
    let _ = controller_port;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Raw GET `path` against the sing-box Clash API, parsed as JSON.
fn fetch_json(controller_port: u16, path: &str) -> Option<serde_json::Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", controller_port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{controller_port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    let body = body
        .split_once("\r\n\r\n")
        .map_or_else(|| body.as_str(), |(_, b)| b);
    serde_json::from_str(body).ok()
}

/// Builds a config whose structure matches `SingBoxConfigRenderer`: PROXY and
/// GLOBAL selectors plus the `clash_mode` route rules that enable mode switch.
fn write_mode_config(
    dir: &std::path::Path,
    controller: &str,
    node_tags: &[&str],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let mut outbounds: Vec<serde_json::Value> = vec![
        serde_json::json!({ "type": "selector", "tag": "PROXY", "outbounds": node_tags }),
        serde_json::json!({ "type": "selector", "tag": "GLOBAL", "outbounds": node_tags }),
        serde_json::json!({ "type": "direct", "tag": "direct" }),
    ];
    for (i, tag) in node_tags.iter().enumerate() {
        outbounds.push(serde_json::json!({
            "type": "vless", "tag": tag, "server": "1.1.1.1", "server_port": 443,
            "uuid": format!("{:032x}", i + 1),
        }));
    }
    let value = serde_json::json!({
        "log": { "level": "error" },
        "inbounds": [],
        "outbounds": outbounds,
        "route": {
            "rules": [
                { "clash_mode": "Global", "outbound": "GLOBAL" },
                { "clash_mode": "Direct", "outbound": "direct" }
            ],
            "final": "PROXY"
        },
        "experimental": { "clash_api": { "external_controller": controller } }
    });
    let config = dir.join("config.json");
    std::fs::write(&config, serde_json::to_vec(&value)?)?;
    Ok(config)
}

/// Real-kernel test: with the `clash_mode` route rules the rendered sing-box
/// config exposes all three routing modes; `set_mode` and `select_proxy` must
/// drive the live kernel through the Clash API.
#[test]
fn sing_box_real_binary_mode_switch_and_select() -> E2eResult {
    let _guard = e2e_lock();

    if !available() {
        return Ok(());
    }
    let (controller_port, controller) = test_controller(4);
    let dir = working_directory("mode-select");
    let config = write_mode_config(&dir, &controller, &["n1", "n2"])?;

    let mut runtime =
        SingBoxRuntime::new(binary(), dir.clone(), controller.clone(), config, 1, None)
            .map_err(e2e_error)?;
    runtime.start(Duration::from_secs(20)).map_err(e2e_error)?;

    let mut control = SingBoxHttpControl::new(controller, None).map_err(e2e_error)?;

    // Capabilities now advertise runtime mode switching.
    let caps = caly_corectl::contract::KernelControl::capabilities(&control);
    assert!(
        caps.is_usable(caly_domain::Capability::RuntimeModeSwitch),
        "sing-box must advertise RuntimeModeSwitch"
    );

    // Mode switch rule -> global.
    control
        .set_mode("global", Duration::from_secs(3))
        .map_err(e2e_error)?;
    let configs = fetch_json(controller_port, "/configs").ok_or("no /configs")?;
    let mode = configs
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or("no mode")?;
    assert_eq!(
        mode.to_lowercase(),
        "global",
        "sing-box mode did not switch to global"
    );

    // Proxy selection in the PROXY selector.
    control
        .select_proxy("PROXY", "n2", Duration::from_secs(3))
        .map_err(e2e_error)?;
    let proxy = fetch_json(controller_port, "/proxies/PROXY").ok_or("no /proxies/PROXY")?;
    let now = proxy
        .get("now")
        .and_then(serde_json::Value::as_str)
        .ok_or("no selection")?;
    assert_eq!(now, "n2", "sing-box selector did not switch to n2");

    // The delay-test endpoint must be reachable and return Ok (Some or None
    // depending on whether the probe host is reachable), never a transport Err.
    let delay = control.test_delay("n2", Duration::from_secs(5));
    assert!(
        delay.is_ok(),
        "sing-box delay test must reach the endpoint: {delay:?}"
    );

    runtime.stop(Duration::from_secs(5)).map_err(e2e_error)?;
    let _ = controller_port;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
