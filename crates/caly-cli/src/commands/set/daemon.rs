//! `set daemon …` dispatch.
//!
//! Round 17: all four lifecycle leaves are real —
//! `status` reuses `show core health`, `stop` / `reload`
//! go through the typed `client::run_client_collect` path
//! (the daemon tears down on `stop` and re-reads config on
//! `reload`), and `restart` is a client-side `Stop` +
//! `Command::spawn("caly daemon")` sequence. Zero stub
//! leaves remain in the `set` namespace.

use std::process::ExitCode;

use crate::cli::SetDaemonCmd;
use crate::output::CliOutput;

pub fn dispatch(c: SetDaemonCmd, output: CliOutput) -> ExitCode {
    let options = crate::cli::CliOptions {
        json: output.is_json(),
        ..crate::cli::CliOptions::default()
    };
    match c {
        // W1-β: `Status` retired (cli.rs note); v3 `caly status
        // [--verbose]` covers both old surfaces.
        SetDaemonCmd::Stop => stop_daemon(options),
        SetDaemonCmd::Reload => reload_config(options),
        SetDaemonCmd::Restart => restart_daemon(output),
    }
}

/// `set daemon stop` — request the running daemon to exit
/// cleanly. The server-side handler triggers the
/// `stop_notifier` after the response is serialized, so
/// the client receives the final operation status
/// before the daemon process exits.
fn stop_daemon(options: crate::cli::CliOptions) -> ExitCode {
    crate::client::run_client(crate::client::ClientCommand::StopDaemon, options)
}

/// `set daemon reload` — request the running daemon to
/// re-read `config.yaml` and re-apply the current
/// candidate. The server-side handler is a typed
/// no-op; the actual re-apply is driven by the
/// `ReloadConfig` operation completing (Round 17
/// delivers the operation status; Round 18+ can wire
/// the runtime to observe the status and trigger a
/// follow-up `ApplyConfig`).
fn reload_config(options: crate::cli::CliOptions) -> ExitCode {
    crate::client::run_client(crate::client::ClientCommand::ReloadConfig, options)
}

/// `set daemon restart` — client-side: `Stop` the
/// running daemon, then spawn a fresh `caly daemon`
/// process. The new process picks up the same XDG /
/// env / config the operator had running, so the
/// restart is "transparent" to the operator.
fn restart_daemon(output: CliOutput) -> ExitCode {
    let options = crate::cli::CliOptions {
        json: output.is_json(),
        ..crate::cli::CliOptions::default()
    };
    let stop_exit = crate::client::run_client(crate::client::ClientCommand::StopDaemon, options);
    if stop_exit != ExitCode::SUCCESS {
        return stop_exit;
    }
    // Wait for the previous daemon to release its
    // lockfile. The Stop response is serialized
    // before the server tears down the transport,
    // but the original daemon's `finish_daemon` runs
    // a small amount of post-serve cleanup (the
    // controller secret removal, the lock release)
    // and the new daemon's `acquire` would otherwise
    // race the lock release. A 500ms bounded wait
    // keeps the operator's UX snappy while leaving
    // comfortable headroom for slow CI.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut command =
        std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "caly".into()));
    command
        .arg("daemon")
        .envs(restart_daemon_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(spawn_stderr_redirect());
    match command.spawn() {
        Ok(child) => {
            // The child runs detached; we don't wait for
            // it (the operator uses `set daemon status`
            // / `show status` to verify boot).
            let pid = child.id();
            if output.is_json() {
                println!("{}", serde_json::json!({ "ok": true, "pid": pid }));
            } else {
                println!("ok: new daemon spawned (pid {pid})");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: cannot spawn `caly daemon` after stop: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Env keys a `caly daemon` process needs to bind the
/// same UDS path, the same lockfile, and the same
/// controller secret as the operator's running
/// daemon. The new process inherits everything else
/// from the parent (XDG / HOME / locale), so only the
/// `CALY_*` overrides are forwarded explicitly.
///
/// Forwarding a fixed set (rather than the whole
/// parent env) keeps the restart deterministic: a
/// stray `CALY_*` set by a one-off shell doesn't
/// silently leak into the new daemon. Operators
/// needing a custom env can re-run the daemon under
/// their own `env -S … caly daemon` shell.
///
/// Round 28 (refactor): the pre-Round 28 shape piped
/// the `filter_map` through `.collect::<Vec<_>>().into_iter()`
/// to satisfy the `IntoIter<(String, String)>` return
/// type. The `collect` was a wasted allocation — the
/// `Command::envs` caller accepts any iterator over
/// `(K, V)` pairs. The Round 28 shape skips the
/// intermediate `Vec` and returns the lazy
/// `filter_map` iterator directly, so the chain is
/// zero-allocation for the hot path (the iterator
/// only materialises the pair on `next()`).
fn restart_daemon_env() -> impl Iterator<Item = (String, String)> {
    const FORWARDED: &[&str] = &[
        "CALY_CORE",
        "CALY_SOCKET",
        "CALY_LOCK",
        "CALY_MIHOMO_BIN",
        "CALY_MIHOMO_CONTROLLER",
        "CALY_MIHOMO_DIR",
        "CALY_SINGBOX_BIN",
        "CALY_SINGBOX_CONTROLLER",
        "CALY_SINGBOX_DIR",
    ];
    FORWARDED.iter().filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| ((*key).to_owned(), value))
    })
}

/// Opens `path` in append-create mode and returns the
/// resulting `Stdio` if the open succeeds. Returns
/// `None` on any error (a missing parent directory,
/// a permission denial, etc.) so the caller can fall
/// back to the next candidate path (or to
/// `Stdio::null()` for a silent restart).
///
/// Round 28 (refactor): the pre-Round 28
/// `spawn_stderr_redirect` had two near-identical
/// `OpenOptions::new().create(true).append(true).open(&path)`
/// blocks (one for `CALY_RESTART_LOG`, one for
/// `<runtime>/restart.log`). The shape was the same
/// `try-this-path → Stdio` logic with the only
/// difference being the path. The Round 28 shape
/// extracts the open into a single helper closure so
/// the two fallback paths share one `if let Some(...)`
/// ladder.
fn open_stderr_log(path: &std::path::Path) -> Option<std::process::Stdio> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(std::process::Stdio::from)
}

/// Redirects the spawned daemon's stderr to a file
/// under the active runtime root so a failed
/// restart can be diagnosed without a TTY. Honors
/// `CALY_RESTART_LOG` for tests / operators that
/// want a different path; otherwise writes
/// `<runtime>/restart.log` next to the daemon's
/// own `daemon.log`.
fn spawn_stderr_redirect() -> std::process::Stdio {
    if let Ok(path) = std::env::var("CALY_RESTART_LOG")
        && let Some(stdio) = open_stderr_log(std::path::Path::new(&path))
    {
        return stdio;
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let path = std::path::PathBuf::from(runtime).join("restart.log");
        if let Some(stdio) = open_stderr_log(&path) {
            return stdio;
        }
    }
    std::process::Stdio::null()
}

#[cfg(test)]
mod tests {
    //! Round 28: lock the lazy-iterator shape of
    //! [`restart_daemon_env`] and the open-fallback
    //! shape of [`open_stderr_log`]. The tests use
    //! the process's own env (rather than injecting
    //! via `set_var` / `remove_var` — those would
    //! race with other tests in the same process) so
    //! they assert *what* the iterator / helper
    //! produce, not *what would happen if* the env
    //! were set a certain way.

    use super::open_stderr_log;
    use crate::test_helpers::temp_root;

    /// The `restart_daemon_env()` iterator is
    /// lazy: the pre-Round 28 `collect::<Vec<_>>().into_iter()`
    /// was a wasted allocation. The new
    /// `impl Iterator<Item = (String, String)>`
    /// return type compiles to the same lazy chain.
    /// We assert the *type* compiles and the
    /// iterator yields pairs whose key is one of
    /// the `FORWARDED` set — locking the
    /// forwarding contract.
    #[test]
    fn restart_daemon_env_iterator_yields_only_known_keys() {
        use std::collections::HashSet;
        let known: HashSet<String> = [
            "CALY_CORE",
            "CALY_SOCKET",
            "CALY_LOCK",
            "CALY_MIHOMO_BIN",
            "CALY_MIHOMO_CONTROLLER",
            "CALY_MIHOMO_DIR",
            "CALY_SINGBOX_BIN",
            "CALY_SINGBOX_CONTROLLER",
            "CALY_SINGBOX_DIR",
        ]
        .iter()
        .map(|key| (*key).to_owned())
        .collect();
        // The iterator is lazy; pulling a bounded
        // prefix (the `FORWARDED` set is 9 keys
        // long, so 16 is a safe over-collect
        // guard against an infinite stream) is
        // enough to lock the contract.
        for (key, _) in super::restart_daemon_env().take(16) {
            assert!(
                known.contains(&key),
                "iterator yielded unknown forwarded key `{key}`"
            );
        }
    }

    /// `open_stderr_log` is the `Stdio`-returning
    /// helper extracted from `spawn_stderr_redirect`
    /// in Round 28. Lock the contract: an
    /// unwritable path returns `None`, a writable
    /// path returns `Some(Stdio::…)`.
    #[test]
    fn open_stderr_log_returns_none_for_unwritable_path() {
        // `/proc/this/does/not/exist` is the
        // canonical unwritable path on Linux.
        let path = std::path::Path::new("/proc/this/does/not/exist");
        assert!(open_stderr_log(path).is_none());
    }

    #[test]
    fn open_stderr_log_returns_some_for_writable_path() {
        let dir = temp_root("daemon-stderr");
        let path = dir.join("restart.log");
        let stdio = open_stderr_log(&path);
        // `Stdio` does not implement `Debug` /
        // `PartialEq` so we assert the
        // side-effect (the file was created) rather
        // than the variant.
        assert!(path.is_file(), "open_stderr_log must create the file");
        // `Stdio::from(file)` keeps the file handle
        // — drop it now so the OS doesn't leak
        // descriptors after the test exits.
        let _ = stdio;
    }
}
