//! `caly doctor` — offline daemon/environment diagnostics.
//!
//! Each check runs without a running daemon and reports a stable verdict, so a
//! user can diagnose a broken install or runtime path before starting the
//! daemon. Output is human text by default and structured JSON with `--json`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

use caly_platform::paths::AppPaths;

#[cfg(test)]
use checks::{check_tun_cap, resolve_ip_binary};
use checks::{has_cap_net_admin, tun_capability_targets};

/// Stable diagnostic verdict for one check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorVerdict {
    Ok,
    Warn,
    Fail,
}

/// Machine-readable evidence attached to an individual diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctorDetail {
    Path {
        path: PathBuf,
        exists: bool,
    },
    UdsPath {
        path: PathBuf,
        safe_to_bind: bool,
    },
    Desktop {
        mode: String,
        supported_backends: Vec<String>,
    },
}

/// One diagnostic result with a stable identifier, human summary and optional
/// structured evidence for scripts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorResult {
    #[serde(rename = "check")]
    pub name: String,
    pub category: &'static str,
    pub verdict: DoctorVerdict,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<DoctorDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl DoctorResult {
    fn make(name: &str, verdict: DoctorVerdict, detail: String) -> Self {
        let category = match name {
            "runtime-dir" | "state-dir" | "config-dir" | "core-workdir" => "filesystem",
            "socket" | "uds-safe" => "security",
            "system-proxy" => "desktop",
            _ => "runtime",
        };
        let remediation = match (&verdict, name) {
            (DoctorVerdict::Warn, "config-dir") => Some("run `caly config generate` to create a documented config".to_owned()),
            (DoctorVerdict::Warn, "runtime-dir" | "state-dir" | "core-workdir") => Some("start `caly daemon`; required directories are created automatically".to_owned()),
            (DoctorVerdict::Warn, "socket") => Some("start `caly daemon` if you intend to use client commands".to_owned()),
            (DoctorVerdict::Warn, "system-proxy") => Some("use GNOME, KDE Plasma, or niri; otherwise configure the system proxy manually".to_owned()),
            (DoctorVerdict::Warn, "core-binaries") | (DoctorVerdict::Fail, "core-binaries") => Some("run scripts/fetch-test-kernels.sh, or point core_binaries at installed executables in the config".to_owned()),
            (DoctorVerdict::Warn, "tun-device") => Some("load the tun kernel module (modprobe tun) or run in an environment that exposes /dev/net/tun; or set tun.enabled to false".to_owned()),
            (DoctorVerdict::Warn, "tun-cap") => Some("run `caly doctor --fix` to grant CAP_NET_ADMIN to ip and the core binaries with a single sudo prompt".to_owned()),
            (DoctorVerdict::Fail, "uds-safe") => Some("remove the unsafe socket path only after confirming no daemon owns it, then restart caly".to_owned()),
            (DoctorVerdict::Fail, _) => Some("inspect the diagnostic detail and report a reproducible failure".to_owned()),
            _ => None,
        };
        Self {
            name: name.to_owned(),
            category,
            verdict,
            detail,
            data: None,
            remediation,
        }
    }

    fn with_data(mut self, data: DoctorDetail) -> Self {
        self.data = Some(data);
        self
    }

    fn ok(name: &str, detail: String) -> Self {
        Self::make(name, DoctorVerdict::Ok, detail)
    }
    fn warn(name: &str, detail: String) -> Self {
        Self::make(name, DoctorVerdict::Warn, detail)
    }
    fn fail(name: &str, detail: String) -> Self {
        Self::make(name, DoctorVerdict::Fail, detail)
    }
}

mod checks;

pub use checks::run_checks;

/// Grants `CAP_NET_ADMIN` to every binary that needs it for TUN (the `ip`
/// binary plus the managed core binaries), through ONE sudo prompt. All
/// `setcap` calls run inside a single escalated `sh -c`, so the user
/// authenticates once; sudo's credential cache covers the whole batch.
/// Idempotent: binaries that already carry the capability are skipped.
pub fn grant_tun_capabilities() -> Result<(), String> {
    let paths = AppPaths::from_env();
    let targets = tun_capability_targets(&paths);
    let mut missing = Vec::new();
    for (label, path) in &targets {
        if !path.exists() {
            println!("doctor: {label} {} — missing, skipping", path.display());
            continue;
        }
        if has_cap_net_admin(path) {
            println!(
                "doctor: {label} {} — already has CAP_NET_ADMIN",
                path.display()
            );
        } else {
            println!("doctor: {label} {} — needs CAP_NET_ADMIN", path.display());
            missing.push(path.clone());
        }
    }
    if missing.is_empty() {
        println!("doctor: nothing to grant");
        return Ok(());
    }
    // One escalated batch: quote every path as a single shell word.
    let grants = missing
        .iter()
        .map(|path| format!("setcap cap_net_admin=+ep '{}'", shell_quote(path)))
        .collect::<Vec<_>>()
        .join(" && ");
    let status = Command::new("sudo")
        .args(["sh", "-c"])
        .arg(grants)
        .status()
        .map_err(|error| format!("cannot run sudo: {error}"))?;
    if !status.success() {
        return Err(
            "sudo setcap failed (check the password and that setcap is installed)".to_owned(),
        );
    }
    println!(
        "doctor: granted CAP_NET_ADMIN to {} binary(ies)",
        missing.len()
    );
    Ok(())
}

/// Quotes a path for embedding into a shell word, escaping single quotes.
fn shell_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

/// Mirrors the composition-root fallback resolution for a managed
/// core executable, in priority order:
///
/// 1. `vendor/bin/<name>` next to any ancestor of the running
///    executable (covers the dev tree: `target/debug/caly`'s
///    ancestors include the repository root that
///    `scripts/fetch-test-kernels.sh` populates).
/// 2. A `PATH` lookup (distro-packaged / manually installed
///    `mihomo` / `sing-box`).
/// 3. `<XDG_DATA_HOME>/caly/vendor/bin/<name>` — the stable,
///    CWD-independent install location used as the eventual
///    report path when nothing resolvable exists yet.
///
/// The previous shape's final fallback was the bare CWD-relative
/// `vendor/bin/<name>`, so doctor's core-binaries verdict changed
/// with the directory the operator happened to run it from (#51).
pub(crate) fn bundled_core_binary(name: &str, data_dir: &Path) -> PathBuf {
    if let Ok(executable) = std::env::current_exe() {
        for directory in executable.ancestors() {
            let candidate = directory.join("vendor").join("bin").join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(found) = lookup_path(name) {
        return found;
    }
    data_dir.join("vendor").join("bin").join(name)
}

/// Searches `$PATH` for an executable named `name` (a real
/// `is_file` + executable-bit probe, not a shell-out to `which`).
pub(crate) fn lookup_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path_var) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Renders the diagnostics as human-readable lines.
pub fn render_human(results: &[DoctorResult]) -> String {
    let mut out = String::new();
    for result in results {
        let mark = match result.verdict {
            DoctorVerdict::Ok => "ok  ",
            DoctorVerdict::Warn => "warn",
            DoctorVerdict::Fail => "fail",
        };
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!("{mark}  {:<16} {}\n", result.name, result.detail),
        );
    }
    out
}

/// Renders the diagnostics as one-line JSON.
pub fn render_json(results: &[DoctorResult]) -> Result<String, serde_json::Error> {
    serde_json::to_string(results)
}

/// Returns whether any check failed (used for the process exit code).
pub fn any_failed(results: &[DoctorResult]) -> bool {
    results.iter().any(|r| r.verdict == DoctorVerdict::Fail)
}

#[cfg(test)]
mod doctor_tests;
