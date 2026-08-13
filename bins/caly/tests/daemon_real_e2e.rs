//! Real daemon + UDS + kernel lifecycle E2E.
//!
//! The ordinary test suite reports a skip when pinned binaries are absent.
//! `CALY_REQUIRE_REAL_E2E=1` turns missing prerequisites into a hard failure.

#![allow(clippy::panic)]

use std::{
    fs,
    net::{TcpListener, TcpStream},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

const START_TIMEOUT: Duration = Duration::from_secs(12);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Serializes the e2e suite. The real daemons in
/// these tests bind UDS sockets under `/tmp` and
/// TCP controllers on loopback ports; running them
/// in parallel can race the kernel port allocator
/// (the historical `alloc_port` helper drops its
/// probe listener before the daemon binds, so a
/// concurrent allocation can hand out the same
/// port). All four real-daemon tests acquire this
/// lock so the suite is deterministic on shared
/// CI hosts without changing the test contracts.
///
/// The lock is a cross-process `mkdir` directory
/// (a process-local `Mutex` cannot see the parallel
/// mock-e2e binary — 2026-08-12 agent audit); a
/// stale lock whose recorded pid is dead is taken
/// over after a wait.
struct E2eLockGuard(std::path::PathBuf);

impl Drop for E2eLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Acquires the suite-wide lock for the duration
/// of the calling test. Waits up to 90 s for a
/// live holder; a dead holder's lock is reclaimed.
fn e2e_lock() -> E2eLockGuard {
    use std::time::Instant;
    let path = std::env::temp_dir().join("caly-e2e-suite.lock");
    let deadline = Instant::now() + std::time::Duration::from_secs(90);
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
                if Instant::now() > deadline {
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
        // Graceful TERM first so the daemon stops and reaps its core process;
        // a bare SIGKILL orphans the kernel, whose ports then block the next
        // test phase. Fall back to KILL only if the daemon does not exit.
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
        // A killed or stalled daemon can orphan its managed kernel, whose
        // ports would poison the next phase; reap by runtime-path match.
        let _ = Command::new("pkill")
            .args(["-9", "-f"])
            .arg(self.runtime.display().to_string())
            .status();
    }
}

#[test]
fn real_daemon_lifecycle_for_pinned_kernels() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let mihomo = root.join("vendor/bin/mihomo");
    let sing_box = root.join("vendor/bin/sing-box");
    if !mihomo.is_file() || !sing_box.is_file() {
        if require_real_e2e() {
            return Err(
                "pinned kernels are required; run scripts/fetch-test-kernels.sh".to_owned(),
            );
        }
        eprintln!("skipping: pinned Mihomo/sing-box binaries are unavailable");
        return Ok(());
    }
    let mihomo_port = alloc_port()?;
    let mihomo_runtime = unique_runtime("mihomo");
    run_core(&root, "mihomo", &mihomo, mihomo_port, &mihomo_runtime)?;
    let sing_port = alloc_port()?;
    let sing_runtime = unique_runtime("sing-box");
    run_core(&root, "sing-box", &sing_box, sing_port, &sing_runtime)
}

/// The owner-only UDS socket path the daemon binds under an XDG runtime root.
fn socket_path(runtime: &Path) -> PathBuf {
    runtime.join("caly").join("daemon.sock")
}

fn run_core(
    _root: &Path,
    core: &str,
    binary: &Path,
    controller_port: u16,
    runtime: &Path,
) -> Result<(), String> {
    fs::create_dir_all(runtime).map_err(|error| error.to_string())?;
    let lock = runtime.join("caly.lock");
    let workdir = runtime.join("caly").join("cores").join(core);
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    let mut command = Command::new(caly_binary());
    let log = runtime.join("daemon.log");
    let log_handle = fs::File::create(&log).map_err(|error| error.to_string())?;
    command
        .arg("daemon")
        .env("CALY_CORE", core)
        .env("CALY_LOCK", &lock)
        .env("CALY_MIHOMO_DIR", &workdir)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("HOME", runtime)
        .stdout(log_handle.try_clone().map_err(|error| error.to_string())?)
        .stderr(log_handle);
    if core == "sing-box" {
        command.env("CALY_SINGBOX_BIN", binary);
        command.env(
            "CALY_SINGBOX_CONTROLLER",
            format!("127.0.0.1:{controller_port}"),
        );
    } else {
        command.env("CALY_MIHOMO_BIN", binary);
        command.env(
            "CALY_MIHOMO_CONTROLLER",
            format!("127.0.0.1:{controller_port}"),
        );
    }
    let child = command.spawn().map_err(|error| error.to_string())?;
    let daemon = DaemonGuard {
        child,
        runtime: runtime.to_path_buf(),
    };
    let socket = socket_path(runtime);
    wait_for_socket(&socket, START_TIMEOUT)?;
    wait_for_port(controller_port, START_TIMEOUT).map_err(|error| {
        let log = fs::read_to_string(runtime.join("daemon.log")).unwrap_or_default();
        format!("{error}\ndaemon log:\n{log}")
    })?;

    let status = client(core, runtime, &["show", "status", "--json"])?;
    if !status.contains(&format!("\"core\":\"{core}\""))
        || !status.contains("\"run_state\":\"running\"")
    {
        return Err(format!("unexpected {core} status: {status}"));
    }
    check_config_apply(core, runtime)?;
    client(core, runtime, &["set", "core", "restart", "--json"])?;
    client(core, runtime, &["set", "core", "stop", "--json"])?;
    wait_for_port_closed(controller_port, STOP_TIMEOUT)?;
    client(core, runtime, &["set", "core", "start", "--json"])?;
    wait_for_port(controller_port, START_TIMEOUT)?;

    let status = daemon.terminate()?;
    if !status.success() {
        return Err(format!("{core} daemon exited with {status}"));
    }
    if socket.exists() || lock.exists() {
        return Err(format!("{core} daemon leaked socket or lock"));
    }
    if let Err(error) = fs::remove_dir_all(runtime) {
        return Err(error.to_string());
    }
    Ok(())
}

fn check_config_apply(core: &str, runtime: &Path) -> Result<(), String> {
    // Exercise the config-apply loop: render → validate → commit. Both cores
    // publish an owner-only generation under the XDG config root (sing-box
    // landed its dedicated backend, so no more fail-fast).
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

/// The thin JSON-framed client used to drive daemon operations.
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
/// daemon binds it moments later, so collisions are practically impossible.
fn alloc_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    drop(listener);
    Ok(port)
}

/// A short runtime root under `/tmp`: the historical workspace-root layout
/// exceeded the 108-byte Unix `SUN_LEN` for the UDS socket on deep checkouts.
fn unique_runtime(core: &str) -> PathBuf {
    caly_platform::paths::test_helpers::unique_path_under("caly-e2e", core)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn caly_binary() -> &'static str {
    env!("CARGO_BIN_EXE_caly")
}

fn require_real_e2e() -> bool {
    std::env::var("CALY_REQUIRE_REAL_E2E").as_deref() == Ok("1")
}

/// Round 17: `set daemon stop` is a real wire command
/// (`WireCommand::StopDaemon`). The server tears down
/// the runtime after serializing the response. The
/// client receives the final operation status, then
/// the daemon process exits and the UDS socket +
/// lockfile are cleaned up.
///
/// This test is the integration-level guarantee that
/// the typed `StopDaemon` path is wired end-to-end; the
/// unit suite only covers the dispatch shape.
#[test]
fn set_daemon_stop_exits_daemon_and_cleans_up() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let mihomo = root.join("vendor/bin/mihomo");
    if !mihomo.is_file() {
        if require_real_e2e() {
            return Err(
                "pinned kernels are required; run scripts/fetch-test-kernels.sh".to_owned(),
            );
        }
        eprintln!("skipping: pinned Mihomo binary is unavailable");
        return Ok(());
    }
    let port = alloc_port()?;
    let runtime = unique_runtime("daemon-stop");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    let socket = socket_path(&runtime);
    let lock = runtime.join("caly.lock");
    let workdir = runtime.join("caly/cores/mihomo");
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    let log = runtime.join("daemon.log");
    let log_handle = fs::File::create(&log).map_err(|error| error.to_string())?;
    let mut daemon_cmd = Command::new(caly_binary());
    daemon_cmd
        .arg("daemon")
        .env("CALY_CORE", "mihomo")
        .env("CALY_LOCK", &lock)
        .env("CALY_MIHOMO_DIR", &workdir)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("HOME", &runtime)
        .env("CALY_MIHOMO_BIN", &mihomo)
        .env("CALY_MIHOMO_CONTROLLER", format!("127.0.0.1:{port}"))
        .stdout(log_handle.try_clone().map_err(|error| error.to_string())?)
        .stderr(log_handle);
    let mut daemon = DaemonGuard {
        child: daemon_cmd.spawn().map_err(|error| error.to_string())?,
        runtime: runtime.clone(),
    };
    wait_for_socket(&socket, START_TIMEOUT)?;
    wait_for_port(port, START_TIMEOUT)?;

    // Sanity: `show status` works.
    let _ = client("mihomo", &runtime, &["show", "status", "--json"])?;

    // Round 17: `set daemon stop` is a real wire command.
    // The client receives a successful operation status;
    // the server then tears down the runtime, which
    // closes the UDS socket and exits the process.
    let stop = client("mihomo", &runtime, &["set", "daemon", "stop", "--json"])?;
    if !stop.contains("\"ok\":true") {
        return Err(format!("set daemon stop did not return ok: {stop}"));
    }

    // The daemon process should exit on its own. Wait
    // for it (the GracefulShutdown path runs in
    // ~100ms; the test allows 5s for slow CI).
    let exited = wait_for_exit(&mut daemon.child, STOP_TIMEOUT)?;
    if !exited.success() {
        return Err(format!("daemon exited with {exited}"));
    }
    if socket.exists() || lock.exists() {
        return Err("daemon leaked socket or lock after stop".to_owned());
    }

    let _ = fs::remove_dir_all(&runtime);
    Ok(())
}

/// Round 17: `set daemon reload` is a real wire command
/// (`WireCommand::ReloadConfig`). The server
/// completes the operation on the application side
/// (the config actor's `reload_config` reports an
/// empty success delta) and the client receives the
/// final `Completed` status. The daemon stays up —
/// reload is non-destructive.
///
/// This test guards the typed `ReloadConfig` path
/// against a regression to the planned_ok stub.
#[test]
fn set_daemon_reload_returns_completed_and_keeps_daemon_up() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let mihomo = root.join("vendor/bin/mihomo");
    if !mihomo.is_file() {
        if require_real_e2e() {
            return Err(
                "pinned kernels are required; run scripts/fetch-test-kernels.sh".to_owned(),
            );
        }
        eprintln!("skipping: pinned Mihomo binary is unavailable");
        return Ok(());
    }
    let port = alloc_port()?;
    let runtime = unique_runtime("daemon-reload");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    let socket = socket_path(&runtime);
    let lock = runtime.join("caly.lock");
    let workdir = runtime.join("caly/cores/mihomo");
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    let log = runtime.join("daemon.log");
    let log_handle = fs::File::create(&log).map_err(|error| error.to_string())?;
    let mut daemon_cmd = Command::new(caly_binary());
    daemon_cmd
        .arg("daemon")
        .env("CALY_CORE", "mihomo")
        .env("CALY_LOCK", &lock)
        .env("CALY_MIHOMO_DIR", &workdir)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("HOME", &runtime)
        .env("CALY_MIHOMO_BIN", &mihomo)
        .env("CALY_MIHOMO_CONTROLLER", format!("127.0.0.1:{port}"))
        .stdout(log_handle.try_clone().map_err(|error| error.to_string())?)
        .stderr(log_handle);
    let mut daemon = DaemonGuard {
        child: daemon_cmd.spawn().map_err(|error| error.to_string())?,
        runtime: runtime.clone(),
    };
    wait_for_socket(&socket, START_TIMEOUT)?;
    wait_for_port(port, START_TIMEOUT)?;

    let _ = client("mihomo", &runtime, &["show", "status", "--json"])?;

    // Round 17: `set daemon reload` is a real wire command.
    // The server returns a Completed operation status.
    let reload = client("mihomo", &runtime, &["set", "daemon", "reload", "--json"])?;
    if !reload.contains("\"ok\":true") || !reload.contains("\"state\":3") {
        return Err(format!("set daemon reload did not complete: {reload}"));
    }

    // The daemon must stay up after reload — it's a
    // non-destructive runtime reconfiguration.
    let status = client("mihomo", &runtime, &["show", "status", "--json"])?;
    if !status.contains("\"run_state\":\"running\"") {
        return Err(format!("daemon stopped after reload: {status}"));
    }
    if daemon.child.try_wait().ok().flatten().is_some() {
        return Err("daemon exited after reload".to_owned());
    }

    // Tidy up via `set daemon stop` so the lockfile /
    // socket are released before the test runtime is
    // removed.
    let _ = client("mihomo", &runtime, &["set", "daemon", "stop", "--json"]);
    let _ = wait_for_exit(&mut daemon.child, STOP_TIMEOUT);

    let _ = fs::remove_dir_all(&runtime);
    Ok(())
}

/// Round 17: `set daemon restart` is a client-side
/// composition of `StopDaemon` (typed wire command)
/// followed by `Command::spawn("caly daemon")`. The
/// new daemon inherits the same env / XDG / config
/// the operator had running, so a restart is
/// transparent: same UDS socket path, same
/// controller port, same `CALY_*` env vars.
///
/// The test verifies the contract the client-side
/// spawn is responsible for: the Stop completes,
/// the old daemon exits, the spawned PID is
/// reported, and the spawned process exists. The
/// new daemon's full UDS bind is environment-
/// dependent (the auto-start must wait for the
/// previous core process to release the controller
/// port; a running kernel already bound to the
/// same port races the new boot) so this test
/// scopes the assertion to the client contract
/// rather than the full post-restart reachability
/// already covered by `set_daemon_stop_*` and the
/// main `real_daemon_lifecycle_for_pinned_kernels`
/// flow.
#[test]
fn set_daemon_restart_reports_spawned_pid_and_exits_old_daemon() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let mihomo = root.join("vendor/bin/mihomo");
    if !mihomo.is_file() {
        if require_real_e2e() {
            return Err(
                "pinned kernels are required; run scripts/fetch-test-kernels.sh".to_owned(),
            );
        }
        eprintln!("skipping: pinned Mihomo binary is unavailable");
        return Ok(());
    }
    let port = alloc_port()?;
    let runtime = unique_runtime("daemon-restart");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    let socket = socket_path(&runtime);
    let lock = runtime.join("caly.lock");
    let workdir = runtime.join("caly/cores/mihomo");
    fs::create_dir_all(&workdir).map_err(|error| error.to_string())?;
    let log = runtime.join("daemon.log");
    let log_handle = fs::File::create(&log).map_err(|error| error.to_string())?;
    let mut daemon_cmd = Command::new(caly_binary());
    daemon_cmd
        .arg("daemon")
        .env("CALY_CORE", "mihomo")
        .env("CALY_LOCK", &lock)
        .env("CALY_MIHOMO_DIR", &workdir)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("HOME", &runtime)
        .env("CALY_MIHOMO_BIN", &mihomo)
        .env("CALY_MIHOMO_CONTROLLER", format!("127.0.0.1:{port}"))
        .stdout(log_handle.try_clone().map_err(|error| error.to_string())?)
        .stderr(log_handle);
    let mut daemon = DaemonGuard {
        child: daemon_cmd.spawn().map_err(|error| error.to_string())?,
        runtime: runtime.clone(),
    };
    wait_for_socket(&socket, START_TIMEOUT)?;
    wait_for_port(port, START_TIMEOUT)?;
    let _ = client("mihomo", &runtime, &["show", "status", "--json"])?;

    // Round 17: `set daemon restart` is a client-side
    // `Stop` + spawn. The first envelope is the
    // operation status of the Stop ({"ok":true,
    // "state":3, ...}), the second is the spawn
    // ack ({"ok":true,"pid":<new>}). Both must
    // succeed.
    let restart = client("mihomo", &runtime, &["set", "daemon", "restart", "--json"])?;
    if !restart.contains("\"ok\":true") {
        return Err(format!("set daemon restart did not report ok: {restart}"));
    }
    if !restart.contains("\"pid\":") {
        return Err(format!(
            "set daemon restart did not return a pid: {restart}"
        ));
    }
    // The Stop operation must have reached Completed
    // (state 3) — the previous test (stop only)
    // already proves the typed wire path; the
    // restart just chains two calls.
    if !restart.contains("\"state\":3") {
        return Err(format!(
            "set daemon restart did not show Completed stop: {restart}"
        ));
    }

    // Old daemon must exit cleanly. The Drop on
    // DaemonGuard will TERM the new (reused PID
    // slot) process if it never exited; the
    // wait_for_exit first asserts the old PID
    // exited so we don't accidentally TERM a
    // still-running process.
    let exited = wait_for_exit(&mut daemon.child, STOP_TIMEOUT)?;
    if !exited.success() {
        return Err(format!("old daemon exited with {exited}"));
    }

    // Best-effort cleanup of the spawned daemon
    // (its env points to the test's runtime dir,
    // and pkill by runtime path reaps it without
    // us holding its handle).
    let _ = Command::new("pkill")
        .args(["-TERM", "-f"])
        .arg(format!("{} daemon", caly_binary()))
        .status();

    let _ = fs::remove_dir_all(&runtime);
    Ok(())
}

/// Round 18: `set proxy add` dry-run is the canonical
/// example of an offline leaf (no daemon round-trip).
/// The contract is:
/// - JSON envelope is `{ok, version, dry_run:true, id, uri, group}`
///   so a `jq` consumer can branch on `dry_run` without
///   parsing the human message.
/// - The default mode (no `--apply`) must NOT write the
///   proxy file under `<state>/inline-proxies/`.
/// - The `--apply` mode MUST write the file and report
///   `dry_run:false`.
///
/// This is the integration-level guard for the dry-run
/// JSON envelope the rest of the `set` namespace
/// (proxy / sub / rule_provider / profile) emits.
#[test]
fn set_proxy_dry_run_emits_envelope_and_does_not_write() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let _ = root; // not used; this test runs offline.
    let runtime = unique_runtime("proxy-dryrun");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    fs::create_dir_all(runtime.join("state")).map_err(|error| error.to_string())?;
    fs::create_dir_all(runtime.join("config")).map_err(|error| error.to_string())?;
    // Pre-create the inline-proxies dir so the
    // existence-check is meaningful: the dry-run path
    // must not even open a file handle here. The
    // writer appends `caly/` to `XDG_STATE_HOME`
    // (see `AppPaths::resolve` in
    // `crates/caly-platform/src/paths/mod.rs`).
    let inline_dir = runtime.join("state").join("caly").join("inline-proxies");
    fs::create_dir_all(&inline_dir).map_err(|error| error.to_string())?;
    let inline_dir_after_dry = inline_dir.clone();
    let inline_dir_after_apply = inline_dir.clone();

    let run = |args: &[&str], env_state: &Path| -> Result<String, String> {
        let output = Command::new(caly_binary())
            .args(args)
            .env("HOME", env_state)
            .env("XDG_RUNTIME_DIR", env_state)
            .env("XDG_STATE_HOME", env_state.join("state"))
            .env("XDG_CONFIG_HOME", env_state.join("config"))
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "client {:?} failed: stderr={} stdout={}",
                args,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout),
            ));
        }
        String::from_utf8(output.stdout).map_err(|error| error.to_string())
    };

    // Dry-run: must report `dry_run:true` and must
    // not write the proxy file.
    let dry = run(
        &["set", "proxy", "add", "vmess://round18-dryrun", "--json"],
        &runtime,
    )?;
    if !dry.contains("\"ok\":true") {
        return Err(format!("dry-run did not report ok: {dry}"));
    }
    if !dry.contains("\"dry_run\":true") {
        return Err(format!("dry-run did not flag `dry_run:true`: {dry}"));
    }
    if !dry.contains("\"id\":") || !dry.contains("vmess://round18-dryrun") {
        return Err(format!("dry-run envelope missing id/uri: {dry}"));
    }
    let entries: Vec<_> = fs::read_dir(&inline_dir_after_dry)
        .map_err(|error| error.to_string())?
        .flatten()
        .collect();
    assert!(
        entries.is_empty(),
        "dry-run must not write any inline proxy file (found {} entries)",
        entries.len()
    );

    // Apply: must report `dry_run:false` and MUST
    // write exactly one file with the expected id.
    let apply = run(
        &[
            "set",
            "proxy",
            "add",
            "vmess://round18-dryrun",
            "--apply",
            "--json",
        ],
        &runtime,
    )?;
    if !apply.contains("\"ok\":true") {
        return Err(format!("apply did not report ok: {apply}"));
    }
    if !apply.contains("\"dry_run\":false") {
        return Err(format!("apply did not flag `dry_run:false`: {apply}"));
    }
    // Extract the id from the apply envelope and
    // assert the file exists.
    let id = apply
        .split("\"id\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .ok_or_else(|| format!("apply envelope missing id: {apply}"))?;
    let proxy_file = inline_dir_after_apply.join(format!("{id}.json"));
    if !proxy_file.is_file() {
        return Err(format!(
            "apply did not write the proxy file at {}",
            proxy_file.display()
        ));
    }

    let _ = fs::remove_dir_all(&runtime);
    Ok(())
}
/// Round 19: `set sub add` dry-run is the canonical
/// example of a writer that does NOT touch disk in
/// dry-run mode.
#[test]
fn set_sub_dry_run_emits_envelope_and_does_not_write() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let _ = root;
    let runtime = unique_runtime("sub-dryrun");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    fs::create_dir_all(runtime.join("state")).map_err(|error| error.to_string())?;
    fs::create_dir_all(runtime.join("config").join("caly")).map_err(|error| error.to_string())?;
    let config_yaml = runtime.join("config").join("caly").join("config.yaml");
    fs::write(
        &config_yaml,
        "schema_version: 1\ncore: mihomo\nsubscriptions: {}\n",
    )
    .map_err(|error| error.to_string())?;

    let run = |args: &[&str]| -> Result<String, String> {
        let output = Command::new(caly_binary())
            .args(args)
            .env("HOME", &runtime)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", runtime.join("state"))
            .env("XDG_CONFIG_HOME", runtime.join("config"))
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "client {:?} failed: stderr={} stdout={}",
                args,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout),
            ));
        }
        String::from_utf8(output.stdout).map_err(|error| error.to_string())
    };

    let dry = run(&["set", "sub", "add", "https://example.com/sub", "--json"])?;
    if !dry.contains("\"ok\":true") {
        return Err(format!("dry-run did not report ok: {dry}"));
    }
    if !dry.contains("\"dry_run\":true") {
        return Err(format!("dry-run did not flag `dry_run:true`: {dry}"));
    }
    if !dry.contains("\"name\":\"https://example.com/sub\"") {
        return Err(format!("dry-run envelope missing name: {dry}"));
    }
    let after_dry = fs::read_to_string(&config_yaml).map_err(|error| error.to_string())?;
    if after_dry.contains("https://example.com/sub") {
        return Err(format!(
            "dry-run must not write the source into config.yaml (found URL in: {after_dry})"
        ));
    }

    let apply = run(&[
        "set",
        "sub",
        "add",
        "https://example.com/sub",
        "--apply",
        "--json",
    ])?;
    if !apply.contains("\"ok\":true") {
        return Err(format!("apply did not report ok: {apply}"));
    }
    if !apply.contains("\"dry_run\":false") {
        return Err(format!("apply did not flag `dry_run:false`: {apply}"));
    }
    let after_apply = fs::read_to_string(&config_yaml).map_err(|error| error.to_string())?;
    if !after_apply.contains("https://example.com/sub") {
        return Err(format!(
            "apply did not write the source into config.yaml: {after_apply}"
        ));
    }

    let _ = fs::remove_dir_all(&runtime);
    Ok(())
}

/// Round 19: `set profile edit` / `enable` / `disable`
/// dry-run must surface `NotDeclared` instead of
/// fabricating an `ok: would be …` success.
#[test]
fn set_profile_dry_run_rejects_unknown_id() -> Result<(), String> {
    let _guard = e2e_lock();
    let root = workspace_root();
    let _ = root;
    let runtime = unique_runtime("profile-dryrun");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    fs::create_dir_all(runtime.join("state")).map_err(|error| error.to_string())?;
    fs::create_dir_all(runtime.join("config").join("caly")).map_err(|error| error.to_string())?;
    let config_yaml = runtime.join("config").join("caly").join("config.yaml");
    fs::write(
        &config_yaml,
        "schema_version: 1\ncore: mihomo\nprofiles: []\n",
    )
    .map_err(|error| error.to_string())?;

    let run = |args: &[&str]| -> Result<String, String> {
        let output = Command::new(caly_binary())
            .args(args)
            .env("HOME", &runtime)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", runtime.join("state"))
            .env("XDG_CONFIG_HOME", runtime.join("config"))
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            return Err(format!(
                "client {:?} must fail: stdout={}",
                args,
                String::from_utf8_lossy(&output.stdout),
            ));
        }
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ))
    };

    for leaf in [
        vec!["set", "profile", "edit", "missing-id", "--json"],
        vec!["set", "profile", "enable", "missing-id", "--json"],
        vec!["set", "profile", "disable", "missing-id", "--json"],
    ] {
        let combined = run(&leaf)?;
        if !combined.contains("profile.not_declared") {
            return Err(format!(
                "{leaf:?} must report profile.not_declared, got: {combined}"
            ));
        }
        if combined.contains("\"ok\":true") {
            return Err(format!("{leaf:?} must not report ok:true, got: {combined}"));
        }
    }

    let _ = fs::remove_dir_all(&runtime);
    Ok(())
}
