//! Linux process-group backend using `setsid` and `kill` without a shell.
//!
//! The backend intentionally launches the executable through `setsid`, making
//! the child a process-group leader. Signals are then sent to the whole group.
//! This keeps the Rust crate free of unsafe libc calls while preserving the
//! process-tree ownership contract.

use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use super::{OwnedProcessTree, ProcessContainment, ProcessExit, ProcessSpawner, SpawnSpec};
use crate::PlatformFailure;
use crate::bounded_text as bounded;
use caly_domain::BoundedText;

/// Linux process spawner backed by `setsid`.
#[derive(Default)]
pub struct LinuxProcessSpawner;

/// Maximum bytes of child stderr retained for lifecycle diagnostics.
const MAX_CAPTURED_STDERR: usize = 4_096;
/// Maximum current stderr buffer size before we start truncating from the
/// front (kept above the retained tail so a flood cannot drop the end).
const MAX_STDERR_BUFFER: usize = 64 * 1024;

/// Maximum bytes of one forwarded kernel log line; longer unterminated runs
/// are forced into chunks so a pathological single line cannot grow the
/// pending buffer without bound.
const MAX_KERNEL_LINE: usize = 8 * 1024;
/// Read granularity for the kernel log drains.
const DRAIN_CHUNK: usize = 1024;

/// Bounded capture of a child's stderr, read by a dedicated drain thread and
/// retained as a tail (newest bytes win).
#[derive(Default)]
struct StderrCapture {
    buffer: Vec<u8>,
}

impl StderrCapture {
    fn push(&mut self, bytes: &[u8]) {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_STDERR_BUFFER {
            // Truncate from the front, keeping a hair above the tail cap so a
            // single trailing write is never dropped mid-line.
            let keep = MAX_CAPTURED_STDERR.saturating_add(MAX_CAPTURED_STDERR);
            let drop = (self.buffer.len() + bytes.len()) - keep;
            self.buffer.drain(..drop);
        }
        self.buffer.extend_from_slice(bytes);
    }

    fn tail(&self) -> String {
        let start = self.buffer.len().saturating_sub(MAX_CAPTURED_STDERR);
        String::from_utf8_lossy(&self.buffer[start..]).into_owned()
    }
}

/// Forwards a kernel output stream into the daemon log as `[kernel:{label}]`
/// lines (2026-08-12 daemon-log audit: kernel logs used to be discarded —
/// stdout went to null, stderr into a bounded capture only). The stderr
/// stream additionally mirrors its raw bytes into a bounded capture so a
/// core that exits before becoming ready can still surface its real error
/// in lifecycle diagnostics. Lines are the unit: a trailing partial line is
/// flushed when the stream closes. The thread ends when the pipe closes
/// (child exit or stop); dropping the capture handle just stops capturing.
fn spawn_kernel_drain<R: Read + Send + 'static>(
    stream: R,
    label: String,
    pid: u32,
    capture: Option<Arc<Mutex<StderrCapture>>>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = [0u8; DRAIN_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(capture) = &capture
                        && let Ok(mut capture) = capture.lock()
                    {
                        capture.push(&buf[..n]);
                    }
                    pending.extend_from_slice(&buf[..n]);
                    drain_pending_lines(&mut pending, &label, pid);
                }
                Err(_) => break,
            }
        }
        if !pending.is_empty() {
            emit_kernel_line(&pending, &label, pid);
        }
    });
}

/// Forwards every complete `\n`-terminated line in `pending`, then forces
/// oversized unterminated runs into chunks.
fn drain_pending_lines(pending: &mut Vec<u8>, label: &str, pid: u32) {
    while let Some(position) = pending.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = pending.drain(..=position).collect();
        line.pop(); // trailing \n
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        emit_kernel_line(&line, label, pid);
    }
    while pending.len() > MAX_KERNEL_LINE {
        let chunk: Vec<u8> = pending.drain(..MAX_KERNEL_LINE).collect();
        emit_kernel_line(&chunk, label, pid);
    }
}

/// Maximum kernel log lines forwarded per second per daemon; a chatty
/// kernel (debug-level or a connection storm) must not flood the daemon log
/// (刀 6 CPU audit).
const KERNEL_LOG_MAX_RATE: u32 = 100;

/// Emits one kernel log line into the daemon log with the `[kernel:{label}]`
/// marker and the child pid as a structured field, so kernel lines are
/// distinguishable from daemon lines at a glance and by `grep '\[kernel'`.
/// Rate-limited to [`KERNEL_LOG_MAX_RATE`] lines/second across all kernels:
/// excess lines are dropped (a debug counter makes the drop visible without
/// flooding the log itself).
fn emit_kernel_line(bytes: &[u8], label: &str, pid: u32) {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static WINDOW_START: AtomicU64 = AtomicU64::new(0);
    static WINDOW_COUNT: AtomicU32 = AtomicU32::new(0);
    static DROPPED: AtomicU32 = AtomicU32::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let window = WINDOW_START.load(Ordering::Relaxed);
    // Reset the window only when time moved FORWARD: a clock step-back must
    // not reopen the flood gate (2026-08-12 boundary audit).
    if now > window {
        WINDOW_START.store(now, Ordering::Relaxed);
        WINDOW_COUNT.store(0, Ordering::Relaxed);
    }
    if WINDOW_COUNT.fetch_add(1, Ordering::Relaxed) >= KERNEL_LOG_MAX_RATE {
        let dropped = DROPPED.fetch_add(1, Ordering::Relaxed);
        if dropped == 0 {
            tracing::debug!(
                "kernel log lines are being dropped (rate limit {KERNEL_LOG_MAX_RATE}/s)"
            );
        }
        return;
    }
    tracing::info!(
        target: "caly::kernel",
        kernel_pid = pid,
        "[kernel:{label}] {}",
        String::from_utf8_lossy(bytes)
    );
}

/// Owned Linux process group.
pub struct LinuxOwnedProcessTree {
    child: Child,
    generation: u64,
    process_group_pid: u32,
    /// `/proc` starttime captured at spawn; the group-signal path refuses to
    /// signal a group whose start time drifted (recycled PGID, #110).
    process_group_start_time: Option<u64>,
    stderr: Arc<Mutex<StderrCapture>>,
}

impl ProcessSpawner for LinuxProcessSpawner {
    type Tree = LinuxOwnedProcessTree;

    fn spawn_owned(
        &mut self,
        spec: SpawnSpec,
        generation: u64,
    ) -> Result<Self::Tree, PlatformFailure> {
        let mut command = Command::new("setsid");
        command
            .arg("--wait")
            .arg(absolute_executable(&spec.executable))
            .args(spec.arguments.iter().map(BoundedText::as_str))
            .current_dir(spec.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            failure(
                "spawn-process",
                format!("cannot start process group: {error}"),
                "verify the executable, working directory, and setsid availability",
            )
        })?;
        let pid = child.id();
        let process_group_pid = discover_process_group_id(pid).ok_or_else(|| {
            failure(
                "discover-process-group",
                format!("setsid wrapper {pid} has no child process group"),
                "verify procfs availability and process permissions",
            )
        })?;
        // Audit #110: capture the group leader's start time so a later
        // `kill -<sig> -- -<pgid>` can refuse to hit a recycled group.
        let process_group_start_time = process_start_time(process_group_pid);
        // Forward both kernel output streams into the daemon log as
        // `[kernel:{label}]` lines; the stderr stream additionally mirrors
        // into a bounded capture so a core that exits before becoming ready
        // can surface its real error in lifecycle diagnostics. Each drain
        // thread ends when its pipe closes (child exit or stop); dropping
        // the capture handle just stops capturing additional lines.
        let label = spec.label.clone();
        let stderr = Arc::new(Mutex::new(StderrCapture::default()));
        if let Some(stream) = child.stdout.take() {
            spawn_kernel_drain(stream, label.clone(), pid, None);
        }
        if let Some(stream) = child.stderr.take() {
            spawn_kernel_drain(stream, label.clone(), pid, Some(Arc::clone(&stderr)));
        }
        Ok(LinuxOwnedProcessTree {
            child,
            generation,
            process_group_pid,
            process_group_start_time,
            stderr,
        })
    }
}

impl OwnedProcessTree for LinuxOwnedProcessTree {
    fn containment(&self) -> ProcessContainment {
        ProcessContainment::UnixProcessGroup
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn stop_gracefully(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ProcessExit>, PlatformFailure> {
        if let Some(status) = self.child.try_wait().map_err(wait_failure)? {
            return Ok(Some(process_exit(status)));
        }
        signal_group(
            self.process_group_pid,
            "TERM",
            self.process_group_start_time,
        )?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().map_err(wait_failure)? {
                return Ok(Some(process_exit(status)));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn force_kill_tree(&mut self) -> Result<(), PlatformFailure> {
        signal_group(
            self.process_group_pid,
            "KILL",
            self.process_group_start_time,
        )
    }

    fn poll_exit(&mut self) -> Result<Option<ProcessExit>, PlatformFailure> {
        self.child
            .try_wait()
            .map(|status| status.map(process_exit))
            .map_err(wait_failure)
    }

    fn wait_reaped(&mut self) -> Result<ProcessExit, PlatformFailure> {
        self.child.wait().map(process_exit).map_err(wait_failure)
    }

    fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map_or_else(|_| String::new(), |capture| capture.tail())
    }
}

fn discover_process_group_id(wrapper_pid: u32) -> Option<u32> {
    let children_path = format!("/proc/{wrapper_pid}/task/{wrapper_pid}/children");
    for _ in 0..20 {
        if let Ok(children) = std::fs::read_to_string(&children_path)
            && let Some(child) = children.split_whitespace().next()
            && let Ok(pid) = child.parse::<u32>()
        {
            let stat_path = format!("/proc/{pid}/stat");
            if let Ok(stat) = std::fs::read_to_string(stat_path)
                && let Some(after_command) = stat.rsplit_once(')')
                && let Some(group) = after_command
                    .1
                    .split_whitespace()
                    .nth(2)
                    .and_then(|value| value.parse::<u32>().ok())
            {
                return Some(group);
            }
            return Some(pid);
        }
        thread::sleep(Duration::from_millis(5));
    }
    let stat_path = format!("/proc/{wrapper_pid}/stat");
    std::fs::read_to_string(stat_path)
        .ok()
        .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.to_owned()))
        .and_then(|rest| rest.split_whitespace().nth(2)?.parse::<u32>().ok())
        .or(Some(wrapper_pid))
}

/// Resolves a relative executable against the spawner's current directory.
///
/// The child process runs with `working_directory` as its cwd, so a relative
/// executable would otherwise be resolved against the core's data directory
/// instead of the daemon's launch directory.
fn absolute_executable(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_owned();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

/// Reads the start-time field of `/proc/<pid>/stat` (field 22; index 19 in
/// the post-comm segment).
fn process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

fn signal_group(
    pid: u32,
    signal: &str,
    expected_start_time: Option<u64>,
) -> Result<(), PlatformFailure> {
    // Audit #110: a crashed core's PGID can be recycled before a later
    // TERM/KILL; never signal a group whose start time no longer matches
    // the one we captured at spawn.
    match (expected_start_time, process_start_time(pid)) {
        (Some(expected), Some(actual)) if actual != expected => {
            return Err(failure(
                "signal-process-group",
                format!(
                    "process group {pid} start time changed ({expected} -> {actual}); refusing to signal a possibly recycled group"
                ),
                "inspect the core process state before retrying",
            ));
        }
        _ => {}
    }
    let group = format!("-{pid}");
    let status = Command::new("kill")
        .args([format!("-{signal}"), "--".to_owned(), group])
        .status()
        .map_err(|error| {
            failure(
                "signal-process-group",
                format!("cannot invoke kill: {error}"),
                "verify the Linux kill utility and process permissions",
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(failure(
            "signal-process-group",
            format!("kill returned status {status}"),
            "inspect process ownership and daemon shutdown state",
        ))
    }
}

fn process_exit(status: ExitStatus) -> ProcessExit {
    ProcessExit {
        code: status.code(),
        signalled: !status.success() && status.code().is_none(),
    }
}

fn wait_failure(error: std::io::Error) -> PlatformFailure {
    failure(
        "wait-process",
        format!("cannot wait for process: {error}"),
        "inspect process ownership and runtime permissions",
    )
}

fn failure(operation: &'static str, message: String, action: &'static str) -> PlatformFailure {
    PlatformFailure {
        operation: bounded(operation, "process-operation"),
        resource: bounded("linux-process-group".to_owned(), "process-group"),
        message: bounded(message, "Linux process operation failed"),
        suggested_action: bounded(action, "inspect the daemon runtime"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::stop_and_reap;
    use caly_domain::BoundedVec;
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn linux_backend_starts_and_reaps_a_process_group() {
        let Ok(argument) = BoundedText::new("10") else {
            return;
        };
        let Ok(arguments) = BoundedVec::try_from_vec(vec![argument]) else {
            return;
        };
        let spec = SpawnSpec {
            executable: PathBuf::from("/bin/sleep"),
            arguments,
            working_directory: PathBuf::from("/tmp"),
            kill_on_owner_drop: true,
            label: "probe".to_owned(),
        };
        let mut spawner = LinuxProcessSpawner;
        let Ok(mut tree) = spawner.spawn_owned(spec, 7) else {
            return;
        };
        assert_eq!(tree.containment(), ProcessContainment::UnixProcessGroup);
        assert_eq!(tree.generation(), 7);
        let result = stop_and_reap(&mut tree, Duration::from_millis(100));
        assert!(result.is_ok(), "{result:?}");
    }

    /// A minimal tracing subscriber that collects event text, so the
    /// forwarded kernel lines can be asserted. Only the forwarding test
    /// installs one (globally — the drain threads use the global dispatcher).
    struct Collector(Arc<Mutex<Vec<String>>>);
    /// Collects the formatted message plus any `name=value` fields, so the
    /// forwarded `[kernel:probe] … kernel_pid=…` lines can be asserted.
    #[derive(Default)]
    struct EventText(String);
    impl tracing::field::Visit for EventText {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.0.push_str(value);
            }
        }
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                let _ = std::fmt::write(&mut self.0, format_args!("{value:?}"));
            } else {
                let _ = std::fmt::write(&mut self.0, format_args!(" {}={value:?}", field.name()));
            }
        }
        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            let _ = std::fmt::write(&mut self.0, format_args!(" {}={value}", field.name()));
        }
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            let _ = std::fmt::write(&mut self.0, format_args!(" {}={value}", field.name()));
        }
        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            let _ = std::fmt::write(&mut self.0, format_args!(" {}={value}", field.name()));
        }
    }
    impl tracing::Subscriber for Collector {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut text = EventText::default();
            event.record(&mut text);
            self.0.lock().unwrap().push(text.0);
        }
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    #[test]
    fn captures_stderr_tail_of_a_dying_child() {
        // `/bin/sh -c 'echo boo >&2; exit 3'` writes a deterministic stderr line
        // then exits; the tree must retain it so lifecycle diagnostics can
        // surface the real failure cause instead of a generic timeout.
        let Ok(script) = BoundedText::new("echo caly-stderr-probe >&2; exit 3") else {
            return;
        };
        let Ok(dash_c) = BoundedText::new("-c") else {
            return;
        };
        let Ok(arguments) = BoundedVec::try_from_vec(vec![dash_c, script]) else {
            return;
        };
        let spec = SpawnSpec {
            executable: PathBuf::from("/bin/sh"),
            arguments,
            working_directory: PathBuf::from("/tmp"),
            kill_on_owner_drop: true,
            label: "probe".to_owned(),
        };
        let mut spawner = LinuxProcessSpawner;
        let Ok(mut tree) = spawner.spawn_owned(spec, 8) else {
            return;
        };
        // Wait for the child to exit (and its stderr pipe to drain).
        for _ in 0..200 {
            if let Ok(Some(_exit)) = tree.poll_exit() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        // Give the drain thread a moment to flush the pipe.
        thread::sleep(Duration::from_millis(50));
        let tail = tree.stderr_tail();
        assert!(tail.contains("caly-stderr-probe"), "tail was: {tail:?}");
        let _ = stop_and_reap(&mut tree, Duration::from_millis(100));
    }

    #[test]
    fn forwards_kernel_stdout_and_stderr_with_marker() {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        // The drain threads run on fresh threads, which use the *global*
        // dispatcher — `set_default` would only cover this thread and the
        // forwarded lines would vanish. Install the collector globally;
        // when another test already owns the global slot, skip quietly.
        if tracing::subscriber::set_global_default(Collector(Arc::clone(&lines))).is_err() {
            return;
        }

        // One child writes a deterministic line to each stream; both must
        // surface as `[kernel:probe]` lines (2026-08-12 daemon-log audit:
        // kernel logs used to be discarded, stdout to null).
        let Ok(script) = BoundedText::new("echo kernel-out-line; echo kernel-err-line >&2; exit 0")
        else {
            return;
        };
        let Ok(dash_c) = BoundedText::new("-c") else {
            return;
        };
        let Ok(arguments) = BoundedVec::try_from_vec(vec![dash_c, script]) else {
            return;
        };
        let spec = SpawnSpec {
            executable: PathBuf::from("/bin/sh"),
            arguments,
            working_directory: PathBuf::from("/tmp"),
            kill_on_owner_drop: true,
            label: "probe".to_owned(),
        };
        let mut spawner = LinuxProcessSpawner;
        let Ok(mut tree) = spawner.spawn_owned(spec, 9) else {
            return;
        };
        let _ = stop_and_reap(&mut tree, Duration::from_millis(500));
        // Wait for both drain threads to flush their pipes.
        for _ in 0..200 {
            let count = lines.lock().unwrap().len();
            if count >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let all = lines.lock().unwrap().join("\n");
        assert!(
            all.contains("[kernel:probe] kernel-out-line"),
            "stdout line missing; collected: {all}"
        );
        assert!(
            all.contains("[kernel:probe] kernel-err-line"),
            "stderr line missing; collected: {all}"
        );
    }
}
