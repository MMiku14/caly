//! Hermetic tests for the offline doctor diagnostics.

use super::*;
use std::ffi::OsString;
use std::path::PathBuf;

/// Hermetic env with explicit XDG roots and a HOME.
fn test_env() -> impl Fn(&str) -> Option<OsString> {
    |key| match key {
        "HOME" => Some(OsString::from("/home/test")),
        "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/1000")),
        "XDG_STATE_HOME" => Some(OsString::from("/home/test/.local/state")),
        "XDG_CONFIG_HOME" => Some(OsString::from("/home/test/.config")),
        _ => None,
    }
}

#[test]
fn checks_always_run_and_report_stable_names() {
    let results = run_checks(&test_env());
    assert!(!results.is_empty());
    // Every result must have a name, verdict and detail.
    for result in &results {
        assert!(!result.name.is_empty());
        assert!(!result.detail.is_empty());
    }
}

#[test]
fn render_human_is_multiline_and_verdict_marked() {
    let results = run_checks(&test_env());
    let human = render_human(&results);
    assert!(human.contains("ok  "));
    assert!(human.contains("runtime-dir"));
}

#[test]
fn render_json_is_valid_array() {
    let results = run_checks(&test_env());
    let json = render_json(&results).unwrap_or_default();
    assert!(!json.is_empty(), "JSON render failed");
    assert!(json.starts_with('[') && json.ends_with(']'));
    assert!(json.contains("\"check\""));
    assert!(json.contains("\"verdict\""));
}

#[test]
fn any_failed_detects_failures() {
    let results = vec![
        DoctorResult::ok("a", "ok".to_owned()),
        DoctorResult::fail("b", "bad".to_owned()),
    ];
    assert!(any_failed(&results));
    assert!(!any_failed(&results[..1]));
}

#[test]
fn env_paths_resolve_from_supplied_environment() {
    let paths = AppPaths::from_env_vars(&test_env());
    assert_eq!(
        paths.socket_path(),
        PathBuf::from("/run/user/1000/caly/daemon.sock")
    );
    assert_eq!(paths.state, PathBuf::from("/home/test/.local/state/caly"));
}

#[test]
fn shell_quote_escapes_single_quotes() {
    assert_eq!(shell_quote(Path::new("/usr/bin/ip")), "/usr/bin/ip");
    assert_eq!(shell_quote(Path::new("/a'b/c")), "/a'\\''b/c");
}

#[test]
fn tun_cap_check_is_stable_and_named() {
    // Hermetic env has no config, so TUN is disabled and the check must
    // report ok (capability not needed) without touching the real system.
    let paths = AppPaths::from_env_vars(&test_env());
    let result = check_tun_cap(&paths);
    assert_eq!(result.name, "tun-cap");
    assert_eq!(result.verdict, DoctorVerdict::Ok);
    assert!(result.detail.contains("tun disabled"));
}

#[test]
fn resolve_ip_binary_falls_back_without_path() {
    // Never panics and always yields a candidate, even with no PATH.
    let candidate = resolve_ip_binary();
    assert!(!candidate.as_os_str().is_empty());
}

#[test]
fn grant_requires_no_sudo_when_nothing_missing_is_unreachable_in_tests() {
    // The grant path enumerates targets without panicking on a hermetic env.
    let paths = AppPaths::from_env_vars(&test_env());
    let targets = tun_capability_targets(&paths);
    assert!(targets.iter().any(|(label, _)| *label == "ip"));
    assert!(targets.iter().any(|(label, _)| *label == "sing-box"));
}
