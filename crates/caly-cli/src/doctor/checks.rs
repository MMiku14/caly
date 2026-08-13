//! Offline diagnostic checks for `caly doctor`.
//!
//! Split out of `doctor.rs` (audit #70 file-length budget):
//! each check runs without a running daemon and reports one
//! [`super::DoctorResult`]; [`run_checks`] executes the full
//! list, converting an unexpected panic into
//! [`super::DoctorVerdict::Warning`] so a misbehaving
//! filesystem probe can never take the whole report down.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use caly_backends::platform::detect_desktop_mode;
use caly_platform::{paths::AppPaths, uds::validate_uds_path};

use super::{DoctorDetail, DoctorResult, bundled_core_binary, is_executable};

/// One offline diagnostic implementation keyed by a stable check name.
type DoctorCheck = fn(&AppPaths) -> DoctorResult;

/// Runs all offline diagnostics against the resolved runtime paths.
pub fn run_checks(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Vec<DoctorResult> {
    let paths = AppPaths::from_env_vars(env);
    let checks: [(&str, DoctorCheck); 10] = [
        ("runtime-dir", check_runtime_dir),
        ("state-dir", check_state_dir),
        ("config-dir", check_config_dir),
        ("socket", check_socket_path),
        ("uds-safe", |paths| check_uds_safety(&paths.socket_path())),
        ("system-proxy", |_| check_desktop_proxy()),
        ("core-workdir", check_core_work_dir),
        ("core-binaries", check_core_binaries),
        ("tun-device", check_tun_device),
        ("tun-cap", check_tun_cap),
    ];
    checks
        .into_iter()
        .map(|(name, check)| {
            catch_unwind(AssertUnwindSafe(|| check(&paths))).unwrap_or_else(|_| {
                DoctorResult::fail(name, "check panicked during execution".to_owned())
            })
        })
        .collect()
}

fn check_runtime_dir(paths: &AppPaths) -> DoctorResult {
    let exists = paths.runtime.exists();
    let result = if exists {
        DoctorResult::ok("runtime-dir", format!("{} exists", paths.runtime.display()))
    } else {
        DoctorResult::warn(
            "runtime-dir",
            format!(
                "{} does not exist (created on daemon start)",
                paths.runtime.display()
            ),
        )
    };
    result.with_data(DoctorDetail::Path {
        path: paths.runtime.clone(),
        exists,
    })
}

fn check_state_dir(paths: &AppPaths) -> DoctorResult {
    let exists = paths.state.exists();
    let result = if exists {
        DoctorResult::ok("state-dir", format!("{} exists", paths.state.display()))
    } else {
        DoctorResult::warn(
            "state-dir",
            format!(
                "{} does not exist (created on first write)",
                paths.state.display()
            ),
        )
    };
    result.with_data(DoctorDetail::Path {
        path: paths.state.clone(),
        exists,
    })
}

fn check_config_dir(paths: &AppPaths) -> DoctorResult {
    let exists = paths.config.exists();
    let result = if exists {
        DoctorResult::ok("config-dir", format!("{} exists", paths.config.display()))
    } else {
        DoctorResult::warn(
            "config-dir",
            format!("{} does not exist (defaults apply)", paths.config.display()),
        )
    };
    result.with_data(DoctorDetail::Path {
        path: paths.config.clone(),
        exists,
    })
}

fn check_socket_path(paths: &AppPaths) -> DoctorResult {
    let socket = paths.socket_path();
    let exists = socket.exists();
    let result = if exists {
        DoctorResult::ok("socket", format!("{} present", socket.display()))
    } else {
        DoctorResult::warn(
            "socket",
            format!("{} not present (daemon not running)", socket.display()),
        )
    };
    result.with_data(DoctorDetail::Path {
        path: socket,
        exists,
    })
}

fn check_uds_safety(socket: &Path) -> DoctorResult {
    let safe = validate_uds_path(socket).is_ok();
    let result = if safe {
        DoctorResult::ok("uds-safe", format!("{} is safe to bind", socket.display()))
    } else {
        DoctorResult::fail(
            "uds-safe",
            format!(
                "{} is a non-owner or non-socket node; refusing to overwrite",
                socket.display()
            ),
        )
    };
    result.with_data(DoctorDetail::UdsPath {
        path: socket.to_path_buf(),
        safe_to_bind: safe,
    })
}

fn check_desktop_proxy() -> DoctorResult {
    let mode = detect_desktop_mode();
    let mode_name = format!("{mode:?}");
    let supported = !matches!(mode, caly_backends::platform::DesktopProxyMode::Unsupported);
    let result = if supported {
        DoctorResult::ok(
            "system-proxy",
            "a supported desktop proxy backend is available".to_owned(),
        )
    } else {
        DoctorResult::warn(
            "system-proxy",
            "no supported desktop proxy backend (GNOME/KDE/niri) detected".to_owned(),
        )
    };
    result.with_data(DoctorDetail::Desktop {
        mode: mode_name,
        supported_backends: vec!["gnome".to_owned(), "kde".to_owned(), "niri".to_owned()],
    })
}

fn check_core_work_dir(paths: &AppPaths) -> DoctorResult {
    let path = paths.core_work_dir();
    let exists = path.exists();
    let result = if exists {
        DoctorResult::ok("core-workdir", format!("{} exists", path.display()))
    } else {
        DoctorResult::warn(
            "core-workdir",
            format!("{} does not exist (created on core start)", path.display()),
        )
    };
    result.with_data(DoctorDetail::Path { path, exists })
}

/// Verifies that both managed core executables resolve to an existing,
/// executable file. A missing kernel is the most common daemon start failure,
/// so doctor surfaces it before the user hits it at runtime.
fn check_core_binaries(paths: &AppPaths) -> DoctorResult {
    let configured =
        crate::config::core_binaries_from(paths.config.clone()).unwrap_or_default();
    let mut all_ok = true;
    let mut lines = Vec::new();
    for (name, configured_path) in [
        ("mihomo", configured.mihomo),
        ("sing-box", configured.sing_box),
    ] {
        let path = configured_path.unwrap_or_else(|| bundled_core_binary(name, &paths.data));
        if is_executable(&path) {
            lines.push(format!("{name}: {} (executable)", path.display()));
        } else {
            all_ok = false;
            lines.push(format!(
                "{name}: {} (missing or not executable)",
                path.display()
            ));
        }
    }
    let detail = lines.join("; ");
    if all_ok {
        DoctorResult::ok("core-binaries", detail)
    } else {
        // A missing core executable is a hard daemon-start
        // failure, not a cosmetic warning: grading it Warn
        // let `doctor` exit 0 while `caly daemon` could not
        // possibly come up, which defeats the preflight.
        DoctorResult::fail("core-binaries", detail)
    }
}

/// Warns when the config enables TUN but the kernel TUN device node is
/// missing/unreadable, because the core process will then exit at start.
/// Resolves the kernel TUN device node the core needs when TUN is enabled.
fn check_tun_device(paths: &AppPaths) -> DoctorResult {
    let enabled = crate::config::load_from(paths.config.clone())
        .ok()
        .flatten()
        .is_some_and(|config| config.tun.enabled);
    if !enabled {
        return DoctorResult::ok(
            "tun-device",
            "tun disabled in config (device not needed)".to_owned(),
        );
    }
    let dev = Path::new("/dev/net/tun");
    if dev.exists() {
        DoctorResult::ok("tun-device", "/dev/net/tun present".to_owned())
    } else {
        DoctorResult::warn(
            "tun-device",
            "tun.enabled is true but /dev/net/tun is missing (the core will fail to start)"
                .to_owned(),
        )
    }
}

/// Warns when TUN is enabled and any of the processes that need it lack
/// `CAP_NET_ADMIN`: the `ip` binary (engaging the interface) and the managed
/// core binaries (opening the TUN device). Missing grants are the most common
/// silent TUN failure, so doctor names the exact binaries and points at
/// `caly doctor --fix` for a single-password remediation.
pub(super) fn check_tun_cap(paths: &AppPaths) -> DoctorResult {
    let enabled = crate::config::load_from(paths.config.clone())
        .ok()
        .flatten()
        .is_some_and(|config| config.tun.enabled);
    if !enabled {
        return DoctorResult::ok(
            "tun-cap",
            "tun disabled in config (capability not needed)".to_owned(),
        );
    }
    let mut missing = Vec::new();
    let mut detail = Vec::new();
    for (label, path) in tun_capability_targets(paths) {
        let has = has_cap_net_admin(&path);
        detail.push(format!(
            "{}: {} ({})",
            label,
            path.display(),
            if has { "cap" } else { "no cap" }
        ));
        if !has {
            missing.push(label);
        }
    }
    let joined = detail.join("; ");
    if missing.is_empty() {
        DoctorResult::ok("tun-cap", joined)
    } else {
        DoctorResult::warn(
            "tun-cap",
            format!(
                "{joined}; run `caly doctor --fix` to grant CAP_NET_ADMIN with one sudo prompt"
            ),
        )
    }
}

/// The binaries that need `CAP_NET_ADMIN` when TUN is enabled: the `ip`
/// command the daemon runs to create/configure the interface, plus both
/// managed core binaries (whichever is active opens the TUN device).
pub(super) fn tun_capability_targets(paths: &AppPaths) -> Vec<(&'static str, PathBuf)> {
    let configured =
        crate::config::core_binaries_from(paths.config.clone()).unwrap_or_default();
    let mut targets = Vec::new();
    targets.push(("ip", resolve_ip_binary()));
    for (name, configured_path) in [
        ("mihomo", configured.mihomo),
        ("sing-box", configured.sing_box),
    ] {
        let path = configured_path.unwrap_or_else(|| bundled_core_binary(name, &paths.data));
        targets.push((name, path));
    }
    targets
}

/// Resolves the `ip` executable the daemon uses, preferring the PATH lookup
/// (the daemon spawns `ip` by name), falling back to `/usr/sbin/ip` and
/// `/sbin/ip` where the command lives on typical Linux distributions.
pub(super) fn resolve_ip_binary() -> PathBuf {
    let path_var = std::env::var_os("PATH");
    if let Some(search) = path_var {
        for directory in std::env::split_paths(&search) {
            let candidate = directory.join("ip");
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    for fallback in ["/usr/sbin/ip", "/sbin/ip"] {
        if is_executable(Path::new(fallback)) {
            return PathBuf::from(fallback);
        }
    }
    PathBuf::from("ip")
}

/// Whether a binary currently carries `cap_net_admin` (checked via `getcap`;
/// missing `getcap` or a missing binary reports false).
///
/// The output is parsed field-wise (`<path> <caps>`), never with a
/// substring `contains` — a raw substring match could be satisfied
/// by a capability *name* like `cap_net_admin_stat` embedded in an
/// unrelated set, or by locale-decorated output, and silently
/// misgrade a binary that lacks the capability.
pub(super) fn has_cap_net_admin(path: &Path) -> bool {
    let output = Command::new("getcap")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    parse_getcap_line(&String::from_utf8_lossy(&output.stdout))
}

/// Parses one `getcap` line (`/path/to/bin cap_net_admin,cap_net_raw=ep`)
/// and reports whether the effective set carries `cap_net_admin`. The
/// capability list is split on commas; each entry is compared against
/// the exact capability name (optionally followed by the `=` flags
/// suffix), so near-miss names never match.
fn parse_getcap_line(line: &str) -> bool {
    let line = line.trim();
    // The capability list is the LAST whitespace-separated token of
    // the row (getcap emits exactly one row per queried file; an
    // empty line or a bare path therefore simply yields the path
    // token, which never names a capability).
    let Some(caps) = line.split_whitespace().last() else {
        return false;
    };
    caps.split(',').map(str::trim).any(|cap| {
        let name = cap.split('=').next().map_or(cap, str::trim);
        name == "cap_net_admin"
    })
}

#[cfg(test)]
mod getcap_parse_tests;
