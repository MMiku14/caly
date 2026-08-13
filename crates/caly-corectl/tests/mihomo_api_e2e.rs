#![allow(clippy::panic)]

//! Real-kernel E2E for Mihomo's rich Clash API (proxies/connections/traffic).

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

fn available() -> bool {
    let binary = binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return false;
    }
    matches!(Command::new(&binary).arg("-v").output(), Ok(output) if output.status.success())
}

fn test_settings() -> (MihomoConfigSettings, String) {
    let base = 31_000 + (std::process::id() % 4_000) as u16;
    let settings = MihomoConfigSettings {
        mixed_port: base,
        external_controller_port: base + 1,
        allow_lan: false,
        ..MihomoConfigSettings::default()
    };
    (settings, format!("127.0.0.1:{}", base + 1))
}

fn working_directory(tag: &str) -> PathBuf {
    caly_platform::paths::test_helpers::unique_path_under("caly-e2e-mihomo-api", tag)
}

type E2eResult = Result<(), Box<dyn std::error::Error>>;

fn e2e_error(value: impl std::fmt::Debug) -> Box<dyn std::error::Error> {
    Box::<dyn std::error::Error>::from(format!("{value:?}"))
}

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
                    // A missing/unparseable pid does NOT mean the holder is
                    // dead: an acquirer creates the dir and only then writes
                    // the pid file, so a waiter polling inside that window
                    // would steal the lock from a LIVE holder and two suites
                    // would run concurrently. Only take over a dir whose pid
                    // is missing AND whose mtime is older than a few seconds
                    // (holder died inside the create -> write window).
                    let stale = std::fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(5));
                    if stale {
                        let _ = std::fs::remove_dir_all(&path);
                        continue;
                    }
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
fn mihomo_real_binary_exposes_rich_clash_api() -> E2eResult {
    use caly_corectl::contract::KernelControl;
    let _guard = e2e_lock();
    if !available() {
        return Ok(());
    }
    let (settings, controller) = test_settings();
    let controller_port = settings.external_controller_port;
    let dir = working_directory("api");
    let config = dir.join("config.yaml");
    std::fs::create_dir_all(&dir)?;
    publish_config(&config, &settings)?;
    let factory = MihomoSpawnSpecFactory::new(binary(), dir.clone()).map_err(e2e_error)?;
    let control = MihomoHttpControl::new(controller.clone(), None).map_err(e2e_error)?;
    let mut runtime = MihomoRuntime::new(factory, LinuxProcessSpawner, control);
    let rendered = RenderedConfigRef {
        generation: 1,
        path: config.clone(),
    };
    start_and_verify(&mut runtime, &rendered, controller_port, "api-start")?;

    let mut api = MihomoHttpControl::new(controller, None).map_err(e2e_error)?;
    // capabilities reflect the Clash API surface.
    let caps = api.capabilities();
    assert!(caps.is_usable(caly_domain::Capability::Connections));
    assert!(caps.is_usable(caly_domain::Capability::Traffic));

    // /proxies returns the default groups even with no nodes configured.
    // The vendored mihomo registers its default groups ~100ms AFTER the
    // control API starts answering /version; a query in that window returns
    // HTTP 200 with an empty map. Poll briefly instead of asserting on the
    // first response (bounded readiness wait, not a retry that hides failure).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut groups = api
        .proxy_groups(Duration::from_secs(2))
        .map_err(e2e_error)?;
    while !groups.iter().any(|g| g.name == "GLOBAL" || g.name == "DIRECT")
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
        groups = api
            .proxy_groups(Duration::from_secs(2))
            .map_err(e2e_error)?;
    }
    assert!(
        groups
            .iter()
            .any(|g| g.name == "GLOBAL" || g.name == "DIRECT"),
        "default groups never registered; last /proxies response: {groups:?}"
    );

    // /connections and /traffic respond with a valid summary.
    let connections = api.connections(Duration::from_secs(2)).map_err(e2e_error)?;
    assert_eq!(connections.active, 0);
    let traffic = api.traffic(Duration::from_secs(2)).map_err(e2e_error)?;
    let _ = (traffic.0, traffic.1);

    stop_and_verify(&mut runtime, controller_port)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

type TestRuntime = MihomoRuntime<LinuxProcessSpawner, MihomoHttpControl>;

fn start_and_verify(
    runtime: &mut TestRuntime,
    rendered: &RenderedConfigRef,
    controller_port: u16,
    _phase: &str,
) -> E2eResult {
    runtime
        .start(rendered, 1, Duration::from_secs(20))
        .map_err(e2e_error)?;
    let version =
        fetch_version(controller_port).ok_or("version endpoint unreachable after start")?;
    assert!(
        version.contains("\"version\""),
        "unexpected /version: {version}"
    );
    runtime
        .health_check(Duration::from_secs(3))
        .map_err(e2e_error)
}

fn stop_and_verify(runtime: &mut TestRuntime, controller_port: u16) -> E2eResult {
    let exit = runtime.stop(Duration::from_secs(5)).map_err(e2e_error)?;
    assert!(
        exit.code.is_some() || exit.signalled,
        "unexpected stop: {exit:?}"
    );
    assert!(
        fetch_version(controller_port).is_none(),
        "controller still reachable after stop"
    );
    Ok(())
}
