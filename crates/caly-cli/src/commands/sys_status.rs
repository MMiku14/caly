//! `caly sysproxy status` / `caly tun status` — offline declared-state
//! projections (W2, cli-v3-design.md C-L′).
//!
//! The projection layers three views of the platform side effect:
//!
//! - `declared` — the operator's intent from `config.yaml`
//!   (`system_proxy.*`, `tun.*`);
//! - `actual` — the desktop's live proxy state, queried locally through
//!   the same capture surface the daemon uses before engagement
//!   (GNOME `gsettings get` / KDE `kreadconfig` / niri environment
//!   file). Best-effort: an unreadable desktop degrades to `unknown`
//!   and never fails the command;
//! - `recovery` — whether a durable recovery record exists under
//!   the XDG state root. A record means the platform side effect is
//!   currently managed by caly (captured at enable time, restored
//!   on the next daemon start/disable); its absence means no
//!   pending side effect is recorded.
//!
//! The text view adds a `diagnosis` line when the three views
//! disagree (e.g. declared enabled but the desktop proxy is off).

use std::process::ExitCode;

use caly_backends::platform::LinuxSystemProxyBackend;
use caly_platform::paths::AppPaths;
use caly_profile::schema::{AppConfig, SystemProxyConfig};

/// `caly sysproxy status`.
pub fn run_sysproxy(options: &crate::cli::CliOptions) -> ExitCode {
    let paths = AppPaths::from_env();
    let (declared, config) = match load_config(&paths, options.json) {
        Ok(Some(config)) => (config.system_proxy.clone(), Some(config)),
        // Fresh install: no config.yaml — the projection renders
        // the all-disabled defaults, matching daemon behaviour.
        Ok(None) => (SystemProxyConfig::default(), None),
        Err(()) => return ExitCode::FAILURE,
    };
    let declared = &declared;
    let (host, port) = crate::config::system_proxy_endpoint_from_config(config.as_ref());
    let expected_endpoint = format!("{host}:{port}");
    let actual = desktop_proxy_state(&host, port);
    let record = paths.recovery_record_path();
    let recorded = record.is_file();
    let diagnosis = diagnose(declared, &actual, recorded, &expected_endpoint);
    if options.json {
        crate::output::print_ok_envelope(serde_json::json!({
            "declared": {
                "enabled": declared.enabled,
                "host": declared.host,
                "port": declared.port,
            },
            "actual": {
                "mode": actual.0,
                "endpoint": actual.1,
            },
            "diagnosis": diagnosis,
            "recovery_record": recorded,
        }));
        return ExitCode::SUCCESS;
    }
    let mut rows = vec![
        (
            "declared",
            if declared.enabled {
                "enabled".to_owned()
            } else {
                "disabled".to_owned()
            },
        ),
        (
            "endpoint",
            format!(
                "{}:{}",
                declared.host,
                declared
                    .port
                    .map_or_else(|| "auto (kernel.mixed_port)".to_owned(), |p| p.to_string()),
            ),
        ),
        ("actual", actual_label(&actual)),
        ("recovery", recovery_label(&record)),
        ("record", record.display().to_string()),
    ];
    if let Some(message) = diagnosis {
        rows.push(("diagnosis", message));
    }
    print_key_values(&rows);
    ExitCode::SUCCESS
}

/// Queries the desktop's live proxy state as `(mode, endpoint)` — the same
/// capture surface the daemon uses before engagement. Best-effort: an
/// unusable backend degrades to `("unknown", "")`; the status view must
/// never fail because desktop tooling is missing.
fn desktop_proxy_state(host: &str, port: u16) -> (String, String) {
    match LinuxSystemProxyBackend::new(host.to_owned(), port) {
        Ok(mut backend) => backend.capture_original_state(),
        Err(_) => ("unknown".to_owned(), String::new()),
    }
}

/// Compares declared intent, the live desktop state, and the recovery
/// record; `None` means the three views agree and no action is needed.
/// `expected_endpoint` is the `host:port` the daemon would write.
fn diagnose(
    declared: &SystemProxyConfig,
    actual: &(String, String),
    recorded: bool,
    expected_endpoint: &str,
) -> Option<String> {
    let (mode, endpoint) = actual;
    match (declared.enabled, mode.as_str()) {
        // Declared enabled, desktop manual: caly is engaged (or another
        // manual tool is) — a matching recovery record confirms caly owns
        // the side effect; an endpoint mismatch means engagement is stale.
        (true, "manual") => {
            if !recorded {
                Some(
                    "desktop proxy is on but caly has no recovery record — it may have been \
                     engaged outside caly"
                        .to_owned(),
                )
            } else if !endpoint.is_empty() && endpoint != expected_endpoint {
                Some(format!(
                    "desktop proxy points at {endpoint} — caly has not engaged this endpoint"
                ))
            } else {
                None
            }
        }
        (true, "none") => Some(
            "declared enabled but the desktop proxy is off — start the daemon to engage it"
                .to_owned(),
        ),
        (true, "auto") => Some(
            "declared manual but the desktop is in PAC (auto) mode — the proxy URL may be set \
             outside caly"
                .to_owned(),
        ),
        (false, "manual") => Some(
            "declared disabled but the desktop proxy is on — it was enabled outside caly"
                .to_owned(),
        ),
        (false, "auto") => Some(
            "declared disabled but the desktop is in PAC (auto) mode — the proxy URL may be set \
             outside caly"
                .to_owned(),
        ),
        (_, "unknown") => Some("desktop proxy state is unreadable".to_owned()),
        // (false, none): disabled and off — consistent.
        _ => None,
    }
}

/// Renders the live desktop state as a human-readable label.
fn actual_label(actual: &(String, String)) -> String {
    match actual.0.as_str() {
        "manual" if !actual.1.is_empty() => format!("manual {}", actual.1),
        "manual" => "manual (no endpoint)".to_owned(),
        "auto" => "auto (PAC)".to_owned(),
        "none" => "disabled".to_owned(),
        _ => "unknown".to_owned(),
    }
}

/// `caly tun status`.
pub fn run_tun(options: &crate::cli::CliOptions) -> ExitCode {
    let paths = AppPaths::from_env();
    let declared = match load_config(&paths, options.json) {
        Ok(Some(config)) => config.tun,
        Ok(None) => caly_profile::schema::TunConfig::default(),
        Err(()) => return ExitCode::FAILURE,
    };
    let declared = &declared;
    // The tun recovery record shares the proxy record's directory; the
    // filename lives in caly-platform (`tun_recovery_record_path`) so the
    // composition store open and this offline projection can never drift.
    let record = paths.tun_recovery_record_path();
    if options.json {
        crate::output::print_ok_envelope(serde_json::json!({
            "declared": {
                "enabled": declared.enabled,
                "mtu": declared.mtu,
                "stack": format!("{:?}", declared.stack).to_lowercase(),
                "auto_route": declared.auto_route,
                "strict_route": declared.strict_route,
                "escalation": declared.escalation,
            },
            "recovery_record": record.is_file(),
        }));
        return ExitCode::SUCCESS;
    }
    print_key_values(&[
        (
            "declared",
            if declared.enabled {
                "enabled".to_owned()
            } else {
                "disabled".to_owned()
            },
        ),
        ("mtu", declared.mtu.to_string()),
        ("stack", format!("{:?}", declared.stack).to_lowercase()),
        ("auto-route", declared.auto_route.to_string()),
        ("strict-route", declared.strict_route.to_string()),
        ("escalation", declared.escalation.clone()),
        ("recovery", recovery_label(&record)),
        ("record", record.display().to_string()),
    ]);
    ExitCode::SUCCESS
}

/// Loads the layered config (`None` = fresh install, no
/// config.yaml); a corrupt config is reported through the
/// standard envelope and surfaces as `Err(())` → exit 1.
fn load_config(paths: &AppPaths, json: bool) -> Result<Option<AppConfig>, ()> {
    crate::config::load_from(paths.config.clone()).map_err(|error| {
        crate::output::report_error(
            crate::output::CliOutput::from_json_flag(json),
            &crate::error::CliError::new(
                "config.load_failed",
                error.to_string(),
                "sysproxy/tun status",
            )
            .with_hint("run `caly config validate` to diagnose the config file"),
        );
    })
}

fn recovery_label(path: &std::path::Path) -> String {
    if !path.is_file() {
        return "none recorded".to_owned();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return "recorded".to_owned();
    };
    // Phase-aware label: the record is written at `Applied` after an
    // engage/restore and flipped to `Restoring` while restore-first is
    // re-applying — a stuck `Restoring` record means a crash mid-recovery.
    // Proxy and TUN records share the phase field; try both shapes.
    let phase = serde_json::from_slice::<caly_platform::recovery::ProxyRecoveryRecord>(&bytes)
        .map(|r| r.phase)
        .or_else(|_| {
            serde_json::from_slice::<caly_platform::recovery::TunRecoveryRecord>(&bytes)
                .map(|r| r.phase)
        })
        .ok();
    match phase {
        Some(caly_platform::recovery::RecoveryPhase::Restoring) => {
            "recorded (restoring — crash recovery interrupted)".to_owned()
        }
        Some(caly_platform::recovery::RecoveryPhase::Applied) => {
            "recorded (managed by caly; restored on disable/start)".to_owned()
        }
        // Legacy or unparsable record: keep the generic label.
        _ => "recorded".to_owned(),
    }
}

fn print_key_values(rows: &[(&str, String)]) {
    let width = rows
        .iter()
        .map(|(key, _)| key.len())
        .max()
        .unwrap_or_default();
    for (key, value) in rows {
        println!("{key:<width$}  {value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(enabled: bool) -> SystemProxyConfig {
        SystemProxyConfig {
            enabled,
            host: "127.0.0.1".to_owned(),
            port: Some(7890),
        }
    }

    fn actual(mode: &str, endpoint: &str) -> (String, String) {
        (mode.to_owned(), endpoint.to_owned())
    }

    #[test]
    fn diagnose_consistent_states_are_silent() {
        // Engaged by caly: manual + matching endpoint + recovery record.
        assert_eq!(
            diagnose(
                &declared(true),
                &actual("manual", "127.0.0.1:7890"),
                true,
                "127.0.0.1:7890"
            ),
            None
        );
        // Declared disabled and the desktop is off.
        assert_eq!(
            diagnose(
                &declared(false),
                &actual("none", ""),
                false,
                "127.0.0.1:7890"
            ),
            None
        );
    }

    #[test]
    fn diagnose_declared_enabled_but_not_engaged() {
        assert!(diagnose(
            &declared(true),
            &actual("none", ""),
            false,
            "127.0.0.1:7890"
        )
        .is_some());
        assert!(diagnose(
            &declared(true),
            &actual("auto", "file:///x"),
            true,
            "127.0.0.1:7890"
        )
        .is_some());
        // Manual but no recovery record — likely engaged outside caly.
        let d = diagnose(
            &declared(true),
            &actual("manual", "127.0.0.1:7890"),
            false,
            "127.0.0.1:7890",
        )
        .expect("missing record must be flagged");
        assert!(d.contains("no recovery record"), "{d}");
        // Manual but pointing elsewhere.
        let d = diagnose(
            &declared(true),
            &actual("manual", "10.0.0.1:3128"),
            true,
            "127.0.0.1:7890",
        )
        .expect("endpoint mismatch must be flagged");
        assert!(d.contains("points at 10.0.0.1:3128"), "{d}");
    }

    #[test]
    fn diagnose_declared_disabled_but_desktop_engaged() {
        assert!(diagnose(
            &declared(false),
            &actual("manual", "10.0.0.1:3128"),
            false,
            "127.0.0.1:7890"
        )
        .is_some());
        assert!(diagnose(
            &declared(false),
            &actual("auto", "file:///x"),
            true,
            "127.0.0.1:7890"
        )
        .is_some());
    }

    #[test]
    fn diagnose_unreadable_desktop_is_flagged_not_fatal() {
        let d = diagnose(
            &declared(true),
            &actual("unknown", ""),
            false,
            "127.0.0.1:7890",
        )
        .expect("unreadable state must be flagged");
        assert!(d.contains("unreadable"), "{d}");
    }

    #[test]
    fn actual_label_renders_modes() {
        assert_eq!(
            actual_label(&actual("manual", "127.0.0.1:7890")),
            "manual 127.0.0.1:7890"
        );
        assert_eq!(actual_label(&actual("manual", "")), "manual (no endpoint)");
        assert_eq!(actual_label(&actual("auto", "file:///x")), "auto (PAC)");
        assert_eq!(actual_label(&actual("none", "")), "disabled");
        assert_eq!(actual_label(&actual("bogus", "")), "unknown");
    }
}
