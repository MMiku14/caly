//! Mock-kernel daemon E2E (#68): the full daemon-lifecycle contract without
//! the pinned `vendor/bin` kernels, so every CI host exercises the
//! spawn → render → readiness → command → shutdown loop instead of skipping.
//!
//! The `caly_mock_kernel` binary emulates the small contract the daemon
//! composes against (version probe, `-t`/`check` validation exit codes and
//! the Clash API endpoints used by command and telemetry paths). These tests
//! never skip; a failure here is a real regression in the daemon lifecycle,
//! not an environment shortage. Real-kernel wire fidelity stays covered by
//! `daemon_real_e2e.rs` (skip-guarded, `CALY_REQUIRE_REAL_E2E=1` to enforce).

#![allow(clippy::panic)]

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

const START_TIMEOUT: Duration = Duration::from_secs(12);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Serializes the mock suite across *processes*: a daemon under test owns
/// its mocked controller port, so two daemons in flight at once — from
/// parallel test binaries — could race the kernel port allocator. A
/// process-local `Mutex` cannot see the other binary (2026-08-12 agent
/// audit); `mkdir` is atomic across processes, and a stale lock whose
/// recorded pid is dead is taken over after a wait.
struct E2eLockGuard(std::path::PathBuf);

impl Drop for E2eLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn e2e_lock() -> E2eLockGuard {
    let path = std::env::temp_dir().join("caly-e2e-suite.lock");
    let deadline = Instant::now() + Duration::from_secs(90);
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
                        .is_some_and(|age| age > Duration::from_secs(5));
                    if stale {
                        // The previous holder died (crash / CI kill); take over.
                        let _ = std::fs::remove_dir_all(&path);
                        continue;
                    }
                }
                if Instant::now() > deadline {
                    panic!("timed out waiting for the e2e suite lock");
                } else {
                    // keep waiting for the live holder
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("cannot acquire the e2e suite lock: {error}"),
        }
    }
    let _ = std::fs::write(path.join("pid"), std::process::id().to_string());
    E2eLockGuard(path)
}

struct DaemonGuard {
    child: Child,
    runtime: PathBuf,
}

impl DaemonGuard {
    fn terminate(mut self) -> Result<ExitStatus, String> {
        signal_term(self.child.id())?;
        wait_for_exit(&mut self.child, STOP_TIMEOUT)
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = signal_term(self.child.id());
            let deadline = Instant::now() + STOP_TIMEOUT;
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        // Reap any orphaned mock kernel so its controller port cannot poison
        // a later phase (mirrors the real suite's runtime-path pkill).
        let _ = Command::new("pkill")
            .args(["-9", "-f"])
            .arg(self.runtime.display().to_string())
            .status();
    }
}

#[test]
fn mock_daemon_lifecycle_for_mihomo() -> Result<(), String> {
    let _guard = e2e_lock();
    run_mock_core("mihomo")
}

#[test]
fn mock_daemon_lifecycle_for_sing_box() -> Result<(), String> {
    let _guard = e2e_lock();
    run_mock_core("sing-box")
}

/// W2/§7 (cli-v3-design.md): `caly node select` with no argument
/// must never block a script — the harness has no TTY, so the leaf
/// reports a usage error (exit 2). The mock fleet carries zero
/// nodes, which doubles as the empty-pool contract: "no entries"
/// with the refresh hint, not a hang and not an empty menu.
#[test]
fn node_select_without_argument_never_blocks_non_tty() -> Result<(), String> {
    let _guard = e2e_lock();
    let (daemon, runtime) = start_mock_daemon("mock-non-tty-select", |_| Ok(()))?;

    let output = Command::new(caly_binary())
        .args(["node", "select"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .output()
        .map_err(|error| error.to_string())?;
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if code != 2 {
        return Err(format!(
            "non-TTY `node select` must exit 2, got {code}; stdout={} stderr={stderr}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    if !stderr.contains("no entries to select") {
        return Err(format!(
            "expected the empty-pool usage error, got: {stderr}"
        ));
    }
    let status = daemon.terminate()?;
    if !status.success() {
        return Err(format!("mock daemon exited with {status}"));
    }
    fs::remove_dir_all(&runtime).map_err(|error| error.to_string())
}

/// The daemon-lifecycle contract, identical in shape to the real-kernel
/// suite: boot, status, config apply, restart, stop/start, graceful exit —
/// with the one difference that the managed core is the mock binary, so the
/// test proves out-of-the-box coverage instead of skipping.
fn run_mock_core(core: &str) -> Result<(), String> {
    // `alloc_port` binds-and-releases; retry the whole lifecycle on a fresh
    // port when the controller never became ready OR a stray kernel answered
    // the banner probe (port contention in both cases — never a product
    // failure). Bounded to 3 attempts.
    for attempt in 1..=3 {
        let controller_port = alloc_port()?;
        match run_mock_core_once(core, controller_port) {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt < 3
                    && (error.contains("did not become ready")
                        || error.contains("answered by an unexpected implementation")) =>
            {
                eprintln!("port {controller_port} contested (attempt {attempt}/3); retrying");
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("loop always returns")
}

fn run_mock_core_once(core: &str, controller_port: u16) -> Result<(), String> {
    let runtime = unique_runtime(&format!("mock-{core}"));
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    let lock = runtime.join("caly.lock");
    let workdir = runtime.join("caly").join("cores").join(core);
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    let log = runtime.join("daemon.log");
    let log_handle = fs::File::create(&log).map_err(|error| error.to_string())?;
    let mut command = Command::new(caly_binary());
    command
        .arg("daemon")
        .env("CALY_CORE", core)
        .env("CALY_LOCK", &lock)
        .env("CALY_MIHOMO_DIR", &workdir)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("HOME", &runtime)
        .stdout(log_handle.try_clone().map_err(|error| error.to_string())?)
        .stderr(log_handle);
    if core == "sing-box" {
        command.env("CALY_SINGBOX_BIN", mock_binary());
        command.env(
            "CALY_SINGBOX_CONTROLLER",
            format!("127.0.0.1:{controller_port}"),
        );
    } else {
        command.env("CALY_MIHOMO_BIN", mock_binary());
        command.env(
            "CALY_MIHOMO_CONTROLLER",
            format!("127.0.0.1:{controller_port}"),
        );
    }
    let child = command.spawn().map_err(|error| error.to_string())?;
    let daemon = DaemonGuard {
        child,
        runtime: runtime.clone(),
    };
    let socket = socket_path(&runtime);
    wait_for_socket(&socket, START_TIMEOUT)?;
    wait_for_port(controller_port, START_TIMEOUT).map_err(|error| {
        let log = fs::read_to_string(runtime.join("daemon.log")).unwrap_or_default();
        format!("{error}\ndaemon log:\n{log}")
    })?;
    // The readiness gate must be satisfied by OUR mock — not by a stray
    // real kernel on the same port — so the suite cannot silently test the
    // wrong core implementation.
    let banner = http_get(controller_port, "/version")?;
    if !banner.contains("caly-mock-kernel") {
        return Err(format!(
            "controller answered by an unexpected implementation: {banner}"
        ));
    }

    let status = client(core, &runtime, &["show", "status", "--json"])?;
    if !status.contains(&format!("\"core\":\"{core}\""))
        || !status.contains("\"run_state\":\"running\"")
    {
        return Err(format!("unexpected {core} status: {status}"));
    }
    check_config_apply(core, &runtime)?;
    client(core, &runtime, &["set", "core", "restart", "--json"])?;
    client(core, &runtime, &["set", "core", "stop", "--json"])?;
    wait_for_port_closed(controller_port, STOP_TIMEOUT)?;
    client(core, &runtime, &["set", "core", "start", "--json"])?;
    wait_for_port(controller_port, START_TIMEOUT)?;

    let status = daemon.terminate()?;
    if !status.success() {
        return Err(format!("{core} mock daemon exited with {status}"));
    }
    if socket.exists() || lock.exists() {
        return Err(format!("{core} mock daemon leaked socket or lock"));
    }
    fs::remove_dir_all(&runtime).map_err(|error| error.to_string())
}

/// Exercises the mock binary standalone: the three invocation modes the
/// daemon composes (`version`, Mihomo `-t`, sing-box `check`) plus one
/// direct serve loop against a Mihomo-shaped config.
#[test]
fn mock_kernel_serves_the_advertised_contract() -> Result<(), String> {
    let _guard = e2e_lock();
    let version = Command::new(mock_binary())
        .arg("version")
        .output()
        .map_err(|error| error.to_string())?;
    if !version.status.success() {
        return Err("mock kernel rejected the version probe".to_owned());
    }
    let root = unique_runtime("mock-selftest");
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let port = alloc_port()?;
    let yaml = root.join("mihomo.yaml");
    let json = root.join("sing-box.json");
    // Validation modes never bind, so they run once against the first port.
    fs::write(
        &yaml,
        format!("mixed-port: 7890\nexternal-controller: 127.0.0.1:{port}\nsecret: mock\n"),
    )
    .map_err(|error| error.to_string())?;
    let validate = Command::new(mock_binary())
        .args(["-d", ".", "-f"])
        .arg(&yaml)
        .arg("-t")
        .output()
        .map_err(|error| error.to_string())?;
    if !validate.status.success() {
        return Err("mock kernel rejected the Mihomo -t validation mode".to_owned());
    }
    fs::write(
        &json,
        format!("{{\"experimental\":{{\"clash_api\":{{\"external_controller\":\"127.0.0.1:{port}\"}}}}}}"),
    )
    .map_err(|error| error.to_string())?;
    let check = Command::new(mock_binary())
        .arg("check")
        .arg("-c")
        .arg(&json)
        .output()
        .map_err(|error| error.to_string())?;
    if !check.status.success() {
        return Err("mock kernel rejected the sing-box check mode".to_owned());
    }
    let outcome = serve_with_retry(&yaml, &json);
    fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
    outcome
}

/// Serve phase of the mock selftest: spawn the mock against a fresh
/// ephemeral port per attempt, killing the previous serve before
/// re-spawning. `alloc_port` binds-and-releases, so a contested port shows
/// up as a readiness timeout; retry only that condition.
fn serve_with_retry(yaml: &Path, json: &Path) -> Result<(), String> {
    for attempt in 1..=3 {
        let port = alloc_port()?;
        fs::write(
            yaml,
            format!("mixed-port: 7890\nexternal-controller: 127.0.0.1:{port}\nsecret: mock\n"),
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            json,
            format!("{{\"experimental\":{{\"clash_api\":{{\"external_controller\":\"127.0.0.1:{port}\"}}}}}}"),
        )
        .map_err(|error| error.to_string())?;
        let mut serve = Command::new(mock_binary())
            .args(["-d", ".", "-f"])
            .arg(yaml)
            .spawn()
            .map_err(|error| error.to_string())?;
        let attempt_outcome = wait_for_port(port, START_TIMEOUT)
            .and_then(|()| http_get(port, "/version"))
            .and_then(|body| {
                if body.contains("caly-mock-kernel") {
                    Ok(())
                } else {
                    Err(format!("unexpected /version body: {body}"))
                }
            });
        let _ = serve.kill();
        let _ = serve.wait();
        match attempt_outcome {
            Ok(()) => return Ok(()),
            Err(error) if attempt < 3 && error.contains("did not become ready") => {
                eprintln!("port {port} contested (attempt {attempt}/3); retrying");
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("loop always returns")
}

/// One raw HTTP GET against the mocked controller, returning the body.
fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| error.to_string())?;
    if !response.starts_with("HTTP/1.1 200") {
        return Err(format!("GET {path} -> {response}"));
    }
    Ok(response)
}

fn check_config_apply(core: &str, runtime: &Path) -> Result<(), String> {
    let apply = client(core, runtime, &["set", "config", "apply", "--json"])?;
    if !apply.contains("\"state\":3") {
        return Err(format!("config apply did not complete: {apply}"));
    }
    let committed = if core == "mihomo" {
        runtime.join("config").join("caly").join("mihomo.yaml")
    } else {
        runtime.join("config").join("caly").join("sing-box.json")
    };
    if !committed.is_file() {
        return Err(format!(
            "config apply did not publish {}",
            committed.display()
        ));
    }
    Ok(())
}

/// The thin JSON-framed CLI client used to drive daemon operations.
fn client(core: &str, runtime: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(caly_binary())
        .args(arguments)
        .env("CALY_CORE", core)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "client {:?} failed: stderr={} stdout={}",
            arguments,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

/// The owner-only UDS socket path the daemon binds under an XDG runtime root.
fn socket_path(runtime: &Path) -> PathBuf {
    runtime.join("caly").join("daemon.sock")
}

fn wait_for_socket(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if UnixStream::connect(path).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("socket did not become ready: {}", path.display()))
}

fn wait_for_port(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("controller port {port} did not become ready"))
}

fn wait_for_port_closed(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("controller port {port} remained open"))
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    child.wait().map_err(|error| error.to_string())
}

fn signal_term(pid: u32) -> Result<(), String> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cannot signal daemon {pid}: {status}"))
    }
}

/// Allocates a free TCP port by binding a listener and releasing it; the
/// mock binds it moments later, so collisions are practically impossible.
fn alloc_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    drop(listener);
    Ok(port)
}

/// A short runtime root under `/tmp` (the workspace-root layout historically
/// exceeded the 108-byte Unix `SUN_LEN` for the UDS socket).
fn unique_runtime(name: &str) -> PathBuf {
    caly_platform::paths::test_helpers::unique_path_under("caly-e2e", name)
}

fn caly_binary() -> &'static str {
    env!("CARGO_BIN_EXE_caly")
}

fn mock_binary() -> &'static str {
    env!("CARGO_BIN_EXE_caly_mock_kernel")
}

/// Spawn the mock-backed daemon for the W2/W4 wire tests. Mirrors
/// `run_core`'s retry: `alloc_port` binds-and-releases, so another process
/// can grab the port before the daemon's mock kernel binds it. Retry the
/// whole launch on a fresh port, but ONLY on a readiness failure
/// (contention), never to mask a real regression. `pre_start` runs inside
/// each attempt so the daemon sees attempt-local config.
fn start_mock_daemon(
    runtime_name: &str,
    pre_start: impl Fn(&Path) -> Result<(), String>,
) -> Result<(DaemonGuard, PathBuf), String> {
    for attempt in 1..=3 {
        let port = alloc_port()?;
        match start_mock_daemon_once(runtime_name, &pre_start, port) {
            Ok(daemon) => {
                let runtime = daemon.runtime.clone();
                return Ok((daemon, runtime));
            }
            Err(error) if attempt < 3 && error.contains("did not become ready") => {
                eprintln!("port {port} contested (attempt {attempt}/3); retrying");
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("loop always returns")
}

fn start_mock_daemon_once(
    runtime_name: &str,
    pre_start: &impl Fn(&Path) -> Result<(), String>,
    controller_port: u16,
) -> Result<DaemonGuard, String> {
    let runtime = unique_runtime(runtime_name);
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    let lock = runtime.join("caly.lock");
    let workdir = runtime.join("caly").join("cores").join("mihomo");
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    pre_start(&runtime)?;
    let log_handle = fs::File::create(runtime.join("daemon.log")).map_err(|e| e.to_string())?;
    let mut command = Command::new(caly_binary());
    command
        .arg("daemon")
        .env("CALY_CORE", "mihomo")
        .env("CALY_LOCK", &lock)
        .env("CALY_MIHOMO_DIR", &workdir)
        .env("CALY_MIHOMO_BIN", mock_binary())
        .env(
            "CALY_MIHOMO_CONTROLLER",
            format!("127.0.0.1:{controller_port}"),
        )
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("HOME", &runtime)
        .stdout(log_handle.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_handle);
    let daemon = DaemonGuard {
        child: command.spawn().map_err(|error| error.to_string())?,
        runtime: runtime.clone(),
    };
    wait_for_socket(&socket_path(&runtime), START_TIMEOUT)?;
    wait_for_port(controller_port, START_TIMEOUT).map_err(|error| {
        let log = fs::read_to_string(runtime.join("daemon.log")).unwrap_or_default();
        format!("{error}\ndaemon log:\n{log}")
    })?;
    Ok(daemon)
}

/// W4 (cli-v3-design.md §12): the `node pick` / group `node test`
/// contract against a mock daemon. A declared config with one selector
/// group and one url-test group drives the offline type checks:
/// - omitted member on a non-TTY is a usage error (exit 2), never a hang;
/// - picking an auto-managed group is a type misuse (exit 2);
/// - a member outside the group is a validation failure (exit 1);
/// - dry-run is the default (exit 0 preview), `--apply` commits through
///   the daemon to the mock kernel's `PUT /proxies/{group}`.
#[test]
fn node_pick_and_group_test_contract() -> Result<(), String> {
    let _guard = e2e_lock();
    let (daemon, runtime) = start_mock_daemon("mock-w4-pick", w4_config)?;

    let selector = "selector-main"; // 节点选择
    let auto = "auto-test"; // 自动选择
                            // 1. Omitted member on a non-TTY: usage error, never a hang.
    let (code, _, stderr) = w4_run(&runtime, &["node", "pick", selector])?;
    if code != 2 || !stderr.contains("requires <member>") {
        return Err(format!(
            "non-TTY `node pick` must exit 2 with the member hint, got {code}: {stderr}"
        ));
    }
    // 2. Type misuse: pick on an auto-managed url-test group.
    let (code, _, stderr) = w4_run(&runtime, &["node", "pick", auto, "direct"])?;
    if code != 2 || !stderr.contains("auto-managed") {
        return Err(format!(
            "pick on a url-test group must exit 2 (auto-managed), got {code}: {stderr}"
        ));
    }
    // 3. Member validation: a member outside the group.
    let (code, _, stderr) = w4_run(&runtime, &["node", "pick", selector, "hk-99"])?;
    if code != 1 || !stderr.contains("not in group") {
        return Err(format!(
            "pick of a foreign member must exit 1, got {code}: {stderr}"
        ));
    }
    // 4. Dry-run is the default commit gate.
    let (code, stdout, _) = w4_run(&runtime, &["node", "pick", selector, "direct"])?;
    if code != 0 || !stdout.contains("would select") {
        return Err(format!(
            "dry-run pick must exit 0 with the preview, got {code}: {stdout}"
        ));
    }
    // 5. `--apply` commits through the daemon to the mock kernel.
    let (code, stdout, stderr) = w4_run(
        &runtime,
        &["node", "pick", selector, "direct", "--apply", "--json"],
    )?;
    if code != 0 || !stdout.contains("\"state\":3") {
        return Err(format!(
            "`node pick --apply` must complete the operation, got {code}: stdout={stdout} stderr={stderr}"
        ));
    }
    // 6. Group `node test`: dry-run preview for a url-test group.
    let (code, stdout, _) = w4_run(&runtime, &["node", "test", auto])?;
    if code != 0 || !stdout.contains("would test group") {
        return Err(format!(
            "group `node test` dry-run must exit 0 with the preview, got {code}: {stdout}"
        ));
    }
    // 7. Group `node test` on a selector: type misuse.
    let (code, _, stderr) = w4_run(&runtime, &["node", "test", selector])?;
    if code != 2 || !stderr.contains("no latency logic") {
        return Err(format!(
            "`node test` on a selector must exit 2, got {code}: {stderr}"
        ));
    }

    let status = daemon.terminate()?;
    if !status.success() {
        return Err(format!("mock daemon exited with {status}"));
    }
    fs::remove_dir_all(&runtime).map_err(|error| error.to_string())
}

/// Declared groups with builtin members only — no provider needed.
fn w4_config(runtime: &Path) -> Result<(), String> {
    let config_dir = runtime.join("config").join("caly");
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    fs::write(
        config_dir.join("config.yaml"),
        "schema_version: 1\n\
         core: mihomo\n\
         proxy_groups:\n\
         \x20 - name: selector-main\n\
         \x20   type: select\n\
         \x20   members:\n\
         \x20     - {kind: direct}\n\
         \x20     - {kind: reject}\n\
         \x20 - name: auto-test\n\
         \x20   type: url-test\n\
         \x20   members:\n\
         \x20     - {kind: direct}\n\
         \x20     - {kind: reject}\n\
         \x20   url_test:\n\
         \x20     url: http://cp.example.com/generate_204\n\
         \x20     interval_seconds: 300\n\
         \x20     tolerance_ms: 50\n",
    )
    .map_err(|error| error.to_string())
}

/// Runs one CLI invocation against the W4 mock runtime with the XDG
/// roots the daemon was spawned under.
fn w4_run(runtime: &Path, args: &[&str]) -> Result<(i32, String, String), String> {
    let output = Command::new(caly_binary())
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("HOME", runtime)
        .output()
        .map_err(|error| error.to_string())?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}
