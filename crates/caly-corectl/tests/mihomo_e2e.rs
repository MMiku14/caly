#![allow(clippy::panic)]

//! Real-kernel Mihomo E2E test against a real Mihomo binary.
//!
//! Drives the exact stack the daemon composes (`MihomoRuntime` +
//! `LinuxProcessSpawner` + `MihomoHttpControl` + `MihomoConfigRenderer`)
//! against the vendored `vendor/bin/mihomo` binary. Skips gracefully when the
//! binary is absent or not executable, so an unprivileged `cargo test --all`
//! never fails on this file.
//!
//! Run with:
//!   cargo test -p caly-corectl --test mihomo_e2e --locked -- --nocapture

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::Command,
    time::Duration,
};

use caly_coreconf::mihomo::{MihomoConfigRenderer, MihomoConfigSettings};
use caly_corectl::{
    contract::RenderedConfigRef,
    mihomo::{MihomoHttpControl, MihomoRuntime, MihomoSpawnSpecFactory},
};
use caly_platform::{
    fs::{AtomicFileContents, AtomicWritePlan, LinuxAtomicFileBackend, atomic_write},
    process::LinuxProcessSpawner,
};

/// Vendored Mihomo binary, overridable for packaging layouts.
/// Test-local equivalent of the pre-P3a `MihomoConfigRenderer::publish`
/// (publication moved out of the pure renderer with the caly-coreconf split):
/// render, then atomic-write through the platform backend with the
/// generation-1 staging name.
fn publish_config(
    config: &std::path::Path,
    settings: &MihomoConfigSettings,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = MihomoConfigRenderer.render(settings).map_err(e2e_error)?;
    let contents =
        AtomicFileContents::try_from_vec(bytes.as_slice().to_vec()).map_err(e2e_error)?;
    let mut temporary = config.as_os_str().to_os_string();
    temporary.push(".tmp.1");
    atomic_write(
        &mut LinuxAtomicFileBackend,
        AtomicWritePlan {
            destination: config.to_path_buf(),
            temporary: PathBuf::from(temporary),
            contents,
        },
    )
    .map_err(e2e_error)
}

fn binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_MIHOMO_BIN") {
        return PathBuf::from(value);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bin/mihomo")
}

/// True only if a real Mihomo binary exists and answers `-v`.
fn available() -> bool {
    let binary = binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return false;
    }
    match Command::new(&binary).arg("-v").output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "skipping: mihomo -v failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            false
        }
        Err(error) => {
            eprintln!("skipping: cannot run mihomo: {error}");
            false
        }
    }
}

/// Per-process unique ports so parallel test runs cannot collide.
fn test_settings() -> (MihomoConfigSettings, String) {
    let base = 21_000 + (std::process::id() % 4_000) as u16;
    let settings = MihomoConfigSettings {
        mixed_port: base,
        external_controller_port: base + 1,
        allow_lan: false,
        ..MihomoConfigSettings::default()
    };
    (settings, format!("127.0.0.1:{}", base + 1))
}

/// Ensures a usable `geoip.metadb` exists inside `dir` for `mihomo -t`.
///
/// Mihomo downloads this database when the validated config references GEOIP
/// data (e.g. a DNS `fallback-filter` with `geoip`); its Go HTTP stack can be
/// blocked where the system stack (curl/wget) still works, and the download is
/// large enough to blow bounded validator timeouts. We therefore pre-provision
/// the database: a system copy (`/etc/mihomo/geoip.metadb`) or the shared
/// download cache, each validated with a real `mihomo -t` probe before reuse
/// so a stale or truncated file cannot silently trigger a re-download hang.
/// Returns `Err` when no usable database is available; callers must turn that
/// into an explicit skip.
fn ensure_geoip_database(dir: &std::path::Path) -> Result<(), String> {
    let target = dir.join("geoip.metadb");
    if target.is_file() {
        return Ok(());
    }
    let cache_dir = std::env::temp_dir().join("caly-geoip-cache");
    let cached = cache_dir.join("geoip.metadb");
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/etc/mihomo/geoip.metadb")];
    if cached.is_file() {
        candidates.push(cached.clone());
    }
    for source in candidates {
        if source_geoip_metadb_usable(dir, &source) {
            std::fs::copy(&source, &target).map_err(|e| format!("copy: {e}"))?;
            return Ok(());
        }
    }
    // Both the system database and any cached copy were absent or unusable;
    // attempt a fresh download into the cache, then validate it before use.
    // The download lands in a pid-unique staging file and is atomically
    // renamed into the shared cache, so a concurrent reader (the daemon's
    // `provision_geoip_metadb` or a parallel test) never observes a
    // half-written file (2026-08-12 agent audit).
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("cache dir: {e}"))?;
    let staging = cache_dir.join(format!("geoip.metadb.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&staging);
    let url = "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb";
    let downloaded = Command::new("curl")
        .args(["-sSL", "--max-time", "120", "-o"])
        .arg(&staging)
        .arg(url)
        .status()
        .map_err(|_| "curl is not available".to_owned())?
        .success()
        || Command::new("wget")
            .args(["-q", "-O"])
            .arg(&staging)
            .arg(url)
            .status()
            .map_err(|_| "wget is not available".to_owned())?
            .success();
    if !downloaded || !source_geoip_metadb_usable(dir, &staging) {
        let _ = std::fs::remove_file(&staging);
        return Err(
            "no usable geoip.metadb: system database absent and download failed/invalid".to_owned(),
        );
    }
    // Publish atomically; a stale cache file from an earlier crash is
    // replaced, and readers only ever see a complete file.
    std::fs::rename(&staging, &cached).map_err(|e| format!("publish cache: {e}"))?;
    std::fs::copy(&cached, &target).map_err(|e| format!("copy: {e}"))?;
    Ok(())
}

/// Runs `mihomo -t` with a minimal config whose `fallback-filter` references
/// GEOIP data, returning whether the given database file is usable. This
/// exercises the exact code path the DNS e2e renders, so a truncated or stale
/// database is detected before it can hang a real validate with a re-download.
fn source_geoip_metadb_usable(dir: &std::path::Path, source: &std::path::Path) -> bool {
    if source.metadata().map_or(0, |m| m.len()) < 1_000_000 {
        return false;
    }
    let _ = std::fs::create_dir_all(dir);
    if std::fs::copy(source, dir.join("geoip.metadb")).is_err() {
        return false;
    }
    let probe = dir.join("geoip.probe.yaml");
    if std::fs::write(
        &probe,
        "mixed-port: 7890\nmode: rule\ndns:\n  enable: true\n  enhanced-mode: fake-ip\n  fallback:\n    - 1.1.1.1\n  fallback-filter:\n    geoip: true\n    geoip-code: CN\n",
    )
    .is_err()
    {
        return false;
    }
    let binary = binary();
    let status = if binary.is_file() {
        Command::new(&binary)
            .args(["-d"])
            .arg(dir)
            .args(["-f"])
            .arg(&probe)
            .arg("-t")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    } else {
        return false;
    };
    let _ = std::fs::remove_file(&probe);
    matches!(status, Ok(status) if status.success())
}

/// Per-test unique working directory (parallel tests must not share one).
fn working_directory(tag: &str) -> PathBuf {
    caly_platform::paths::test_helpers::unique_path_under("caly-e2e-mihomo", tag)
}

/// Raw GET /version against the Mihomo REST controller.
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

/// Concrete runtime stack used across the lifecycle E2E helpers.
type TestRuntime = MihomoRuntime<LinuxProcessSpawner, MihomoHttpControl>;

fn e2e_error(value: impl std::fmt::Debug) -> Box<dyn std::error::Error> {
    Box::<dyn std::error::Error>::from(format!("{value:?}"))
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
fn mihomo_rendered_config_passes_real_binary_validation() -> E2eResult {
    let _guard = e2e_lock();

    if !available() {
        return Ok(());
    }
    let (settings, _) = test_settings();
    let dir = working_directory("validate");
    let config = dir.join("config.yaml");
    std::fs::create_dir_all(&dir)?;
    publish_config(&config, &settings)?;

    let test = Command::new(binary())
        .args(["-t", "-d"])
        .arg(&dir)
        .arg("-f")
        .arg(&config)
        .output()?;
    assert!(
        test.status.success(),
        "mihomo -t rejected the rendered config: {}",
        String::from_utf8_lossy(&test.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn mihomo_dns_config_passes_real_binary_validation() -> E2eResult {
    use caly_dns::{DnsMode, DnsSettingsBuilder};
    let _guard = e2e_lock();
    if !available() {
        return Ok(());
    }
    let (settings, _) = test_settings();
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")?
        .push_nameserver("1.1.1.1")?
        .push_fallback("tls://dns.google")?
        .push_default("223.5.5.5")?
        .fake_ip_range("198.18.0.1/16")?
        .build()?
        .ok_or_else(|| e2e_error("dns settings disabled"))?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        ..settings
    };
    let dir = working_directory("dns-validate");
    let config = dir.join("config.yaml");
    std::fs::create_dir_all(&dir)?;
    // The fallback-filter uses geoip: CN; pre-provision the database so the
    // validator never attempts its own download (which can hang or be blocked).
    if let Err(reason) = ensure_geoip_database(&dir) {
        eprintln!("skipping: {reason}");
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(());
    }
    publish_config(&config, &settings)?;

    let test = Command::new(binary())
        .args(["-t", "-d"])
        .arg(&dir)
        .arg("-f")
        .arg(&config)
        .output()?;
    assert!(
        test.status.success(),
        "mihomo -t rejected the DNS config: {}",
        String::from_utf8_lossy(&test.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn mihomo_real_binary_start_ready_restart_stop() -> E2eResult {
    let _guard = e2e_lock();

    if !available() {
        return Ok(());
    }
    let (settings, controller) = test_settings();
    let controller_port = settings.external_controller_port;
    let dir = working_directory("lifecycle");
    let config = dir.join("config.yaml");
    std::fs::create_dir_all(&dir)?;
    publish_config(&config, &settings)?;

    let factory = MihomoSpawnSpecFactory::new(binary(), dir.clone()).map_err(e2e_error)?;
    let control = MihomoHttpControl::new(controller, None).map_err(e2e_error)?;
    let mut runtime = MihomoRuntime::new(factory, LinuxProcessSpawner, control);
    let rendered = RenderedConfigRef {
        generation: 1,
        path: config.clone(),
    };

    start_and_verify(&mut runtime, &rendered, controller_port, "start")?;

    let rendered_2 = RenderedConfigRef {
        generation: 2,
        path: config.clone(),
    };
    restart_and_verify(&mut runtime, &rendered_2, controller_port)?;

    stop_and_verify(&mut runtime, controller_port)?;

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn start_and_verify(
    runtime: &mut TestRuntime,
    rendered: &RenderedConfigRef,
    controller_port: u16,
    phase: &str,
) -> E2eResult {
    runtime
        .start(rendered, 1, Duration::from_secs(20))
        .map_err(e2e_error)?;
    let version =
        fetch_version(controller_port).ok_or("version endpoint unreachable after start")?;
    assert!(
        version.contains("\"version\""),
        "unexpected /version after {phase}: {version}"
    );
    runtime
        .health_check(Duration::from_secs(3))
        .map_err(e2e_error)
}

fn restart_and_verify(
    runtime: &mut TestRuntime,
    rendered: &RenderedConfigRef,
    controller_port: u16,
) -> E2eResult {
    runtime
        .restart(rendered, 2, Duration::from_secs(20))
        .map_err(e2e_error)?;
    let version =
        fetch_version(controller_port).ok_or("version endpoint unreachable after restart")?;
    assert!(
        version.contains("\"version\""),
        "unexpected /version after restart: {version}"
    );
    Ok(())
}

fn stop_and_verify(runtime: &mut TestRuntime, controller_port: u16) -> E2eResult {
    let exit = runtime.stop(Duration::from_secs(5)).map_err(e2e_error)?;
    assert!(
        exit.code.is_some() || exit.signalled,
        "unexpected stop outcome: {exit:?}"
    );
    assert!(
        fetch_version(controller_port).is_none(),
        "controller still reachable after stop"
    );
    Ok(())
}
