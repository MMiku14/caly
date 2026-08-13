//! `caly history` — operation memory (Round 26).
//!
//! Records every *side-effecting* CLI operation (`sub enable`, `core
//! switch`, `node select`, `sysproxy on`, …) so the operator can recall
//! what was changed and replay it. Read-only leaves (`status`, `list`,
//! `show`, `config generate`/`diff`, …) are never recorded.
//!
//! Storage is a plain ring-buffer JSON file under the XDG state root
//! (`operations.json`, capped at [`MAX_ENTRIES`], atomically replaced on
//! every write). Replay re-parses the recorded argv through the normal
//! CLI path — identical to the operator typing the command again, which
//! also means a replayed operation is recorded again (shell-history
//! behaviour).

use std::path::PathBuf;
use std::process::ExitCode;

use std::os::unix::fs::OpenOptionsExt;

use crate::cli::{CliOptions, Command, HistoryCmd, SetCmd, SetConfigCmd, SetCoreCmd};

/// Ring-buffer cap: enough for a session's worth of operations without
/// unbounded growth on a busy daemon-managed machine.
const MAX_ENTRIES: usize = 100;
const FILE_NAME: &str = "operations.json";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OperationEntry {
    /// Unix seconds at execution time.
    pub ts: u64,
    /// The argv as the operator typed it (pre alias-expansion), so replay
    /// runs through the same parse path as the original invocation.
    pub command: String,
    /// Coarse domain for grouping in the list view (`sub`, `core`, …).
    pub domain: String,
}

/// Ring-buffer operation log persisted under the XDG state root.
pub struct OperationLog {
    path: PathBuf,
}

impl OperationLog {
    pub fn from_env() -> Self {
        let paths = caly_platform::paths::AppPaths::from_env();
        Self {
            path: paths.state.join(FILE_NAME),
        }
    }

    pub fn load(&self) -> Vec<OperationEntry> {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Appends an entry, trimming to the ring cap. Atomic publish so a
    /// concurrent reader never observes a half-written file.
    pub fn append(&self, entry: OperationEntry) {
        let mut entries = self.load();
        entries.push(entry);
        if entries.len() > MAX_ENTRIES {
            entries.drain(..entries.len() - MAX_ENTRIES);
        }
        self.persist(&entries);
    }

    pub fn clear(&self) {
        self.persist(&[]);
    }

    fn persist(&self, entries: &[OperationEntry]) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Staging + rename: the daemon never reads this file, but a
        // concurrent `caly history` in another terminal must not see a
        // torn write (same discipline as the PAC and recovery records).
        //
        // Recorded argv can carry subscription URLs whose query strings
        // hold tokens (same risk class as config.yaml) — owner-only 0600
        // is the first line of defence.
        let staging = self.path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(entries) {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            opts.mode(0o600);
            if let Ok(mut file) = opts.open(&staging) {
                use std::io::Write;
                let _ = file.write_all(&bytes);
                let _ = file.sync_all();
            }
            let _ = std::fs::rename(&staging, &self.path);
        }
    }
}

/// The coarse domain label for a recordable command, or `None` when the
/// command has no side effects (never recorded).
fn recordable_domain(command: &Command) -> Option<&'static str> {
    let Command::Set(set) = command else {
        return None;
    };
    match set {
        // `node select`/`node pick` are remembered under the `node` label
        // (the domain the operator typed), not the generic `core` one.
        SetCmd::Core(SetCoreCmd::Select { .. }) => Some("node"),
        SetCmd::Core(_) => Some("core"),
        SetCmd::Proxy(_) => Some("sysproxy"),
        SetCmd::Tun(_) => Some("tun"),
        SetCmd::Sub(_) => Some("sub"),
        SetCmd::Profile(_) => Some("profile"),
        SetCmd::Daemon(_) => Some("daemon"),
        SetCmd::RuleProvider(_) => Some("rule-provider"),
        SetCmd::ProxyGroup(_) => Some("proxy-group"),
        SetCmd::Entry(_) => Some("node"),
        // Read-only previews (`config generate`/`diff`) are not operations.
        SetCmd::Config(SetConfigCmd::Apply | SetConfigCmd::Default | SetConfigCmd::Edit(_)) => {
            Some("config")
        }
        SetCmd::Config(SetConfigCmd::Generate | SetConfigCmd::Diff { .. }) => None,
    }
}

/// Records the invocation when the parsed command has side effects.
/// Best-effort: a failed log write never fails the command itself.
pub fn maybe_record(raw_args: &[String], command: &Command) {
    let Some(domain) = recordable_domain(command) else {
        return;
    };
    let log = OperationLog::from_env();
    log.append(OperationEntry {
        ts: now_unix(),
        command: raw_args.join(" "),
        domain: domain.to_owned(),
    });
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `caly history list|replay|clear`.
pub fn run(cmd: HistoryCmd, options: &CliOptions) -> ExitCode {
    let log = OperationLog::from_env();
    match cmd {
        HistoryCmd::List { limit } => list(&log, options, limit),
        HistoryCmd::Replay { target } => replay(&log, options, &target),
        HistoryCmd::Clear => clear(&log, options),
    }
}

fn list(log: &OperationLog, options: &CliOptions, limit: Option<usize>) -> ExitCode {
    let entries = log.load();
    let shown = limit.unwrap_or(20).min(entries.len());
    let slice = &entries[entries.len() - shown..];
    if options.json {
        crate::output::print_ok_envelope(serde_json::json!({ "entries": slice }));
        return ExitCode::SUCCESS;
    }
    if slice.is_empty() {
        println!("no operations recorded");
        return ExitCode::SUCCESS;
    }
    let now = now_unix();
    for (i, entry) in slice.iter().enumerate() {
        println!(
            "{:>3}. {:<13} {}",
            entries.len() - shown + i + 1,
            relative(entry.ts, now),
            entry.command,
        );
    }
    println!("\nreplay: caly history replay <index> | latest   clear: caly history clear");
    ExitCode::SUCCESS
}

fn replay(log: &OperationLog, options: &CliOptions, target: &str) -> ExitCode {
    if options.json {
        return crate::output::report_error_returning(
            crate::output::CliOutput::Json,
            crate::output::CliError::new(
                crate::error::usage::INVALID,
                "replay is a terminal command; it cannot run with --json",
                "history replay",
            ),
        );
    }
    let entries = log.load();
    let index = match target {
        "latest" => entries.len().saturating_sub(1),
        other => match other.parse::<usize>() {
            Ok(i) if i >= 1 && i <= entries.len() => i - 1,
            _ => {
                eprintln!("no such recorded operation: `{other}`");
                return ExitCode::FAILURE;
            }
        },
    };
    let Some(entry) = entries.get(index) else {
        eprintln!("no recorded operations yet");
        return ExitCode::FAILURE;
    };
    let argv: Vec<String> = entry.command.split_whitespace().map(String::from).collect();
    if argv.is_empty() {
        eprintln!("recorded operation `{}` is empty", entry.command);
        return ExitCode::FAILURE;
    }
    // Replay is identical to the operator re-typing the command: the
    // full parse (incl. alias expansion) and dispatch path run again,
    // and the replayed operation is recorded once more (shell-history
    // behaviour). `history` itself is not recordable, so this cannot
    // recurse.
    match crate::cli::parse_args(argv) {
        Ok(invocation) => {
            // main() records before run(); replay bypasses main(), so the
            // record step is repeated here to keep the contract.
            crate::commands::history::maybe_record(
                &entry
                    .command
                    .split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                &invocation.command,
            );
            crate::dispatch(
                invocation.command,
                invocation.options,
                // Recorded operations are never the host command:
                // `maybe_record` only logs side-effecting CLI
                // operations, and replay cannot (must not) start a
                // daemon. The placeholder closure is unreachable in
                // practice; it fails closed if it ever runs.
                |_| {
                    eprintln!("cannot replay the daemon host command");
                    std::process::ExitCode::FAILURE
                },
            )
        }
        Err(error) => crate::error::print_and_fold(&error),
    }
}

fn clear(log: &OperationLog, options: &CliOptions) -> ExitCode {
    log.clear();
    if options.json {
        crate::output::print_ok_envelope(serde_json::json!({ "cleared": true }));
    } else {
        println!("ok: operations cleared");
    }
    ExitCode::SUCCESS
}

/// Compact relative timestamp: `30s ago` / `5m ago` / `2h ago` / `3d ago`.
fn relative(ts: u64, now: u64) -> String {
    let delta = now.saturating_sub(ts);
    match delta {
        0..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: u64, command: &str, domain: &str) -> OperationEntry {
        OperationEntry {
            ts,
            command: command.to_owned(),
            domain: domain.to_owned(),
        }
    }

    #[test]
    fn recordable_domain_marks_side_effects_only() {
        use crate::cli::{SetCoreCmd, SetSubCmd};
        // Recordable.
        assert_eq!(
            recordable_domain(&Command::Set(SetCmd::Core(SetCoreCmd::Switch(
                "mihomo".into()
            )))),
            Some("core")
        );
        assert_eq!(
            recordable_domain(&Command::Set(SetCmd::Sub(SetSubCmd::Enable {
                url: "air1".into(),
                apply: false,
                dry_run: false,
            }))),
            Some("sub")
        );
        // Read-only previews are not operations.
        assert_eq!(
            recordable_domain(&Command::Set(SetCmd::Config(SetConfigCmd::Generate))),
            None
        );
        assert_eq!(
            recordable_domain(&Command::Set(SetCmd::Config(SetConfigCmd::Diff {
                file: None
            }))),
            None
        );
        // Non-Set commands are never recorded.
        assert_eq!(
            recordable_domain(&Command::Show(crate::cli::ShowCmd::Core(
                crate::cli::ShowCoreCmd::Nodes
            ))),
            None
        );
        assert_eq!(recordable_domain(&Command::Daemon), None);
    }

    #[test]
    fn append_trims_to_ring_cap_and_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("caly-hist-ring-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let log = OperationLog { path };
        for i in 0..(MAX_ENTRIES + 25) {
            log.append(entry(i as u64, &format!("core switch core{i}"), "core"));
        }
        let entries = log.load();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries.first().unwrap().command, "core switch core25");
        assert_eq!(entries.last().unwrap().command, "core switch core124");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_empties_and_corrupt_file_loads_empty() {
        let dir = std::env::temp_dir().join(format!("caly-hist-clear-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let log = OperationLog { path: path.clone() };
        log.append(entry(1, "tun on", "tun"));
        std::fs::write(&path, b"{ not json").unwrap();
        // A corrupt log degrades to empty; it never breaks the CLI.
        assert!(log.load().is_empty());
        log.clear();
        assert!(log.load().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_labels_are_compact() {
        assert_eq!(relative(100, 130), "30s ago");
        assert_eq!(relative(100, 400), "5m ago");
        assert_eq!(relative(100, 7300), "2h ago");
        assert_eq!(relative(100, 260_100), "3d ago");
    }
}
