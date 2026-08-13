//! GNOME proxy support via `gsettings` (`org.gnome.system.proxy`).

use std::path::PathBuf;

use caly_platform::command::{CommandRequest, CommandResult, CommandRunner};

use caly_ports::ActorFailure;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Builds a bounded `gsettings` argument list from plain slices.
fn arguments(values: &[&str]) -> Result<caly_platform::command::CommandArguments, ActorFailure> {
    crate::platform::argv(&[values])
}

/// Runs one `gsettings` invocation and requires exit code zero.
pub(crate) fn run<R: CommandRunner>(
    runner: &mut R,
    values: &[&str],
) -> Result<CommandResult, ActorFailure> {
    let request = CommandRequest {
        executable: PathBuf::from("gsettings"),
        arguments: arguments(values)?,
        timeout: TIMEOUT,
    };
    let result = runner.run_bounded(request).map_err(|_| {
        crate::failure(
            "system proxy command failed",
            "install the desktop backend or use a supported desktop",
        )
    })?;
    if result.exit_code != Some(0) {
        return Err(crate::failure(
            "system proxy command returned failure",
            "inspect desktop proxy permissions",
        ));
    }
    Ok(result)
}

/// Enables or disables the GNOME proxy; enabling points at `host:port`.
///
/// The endpoint is written split across the `http.host`/`http.port` keys
/// (the gsettings schema shape), matching the restore path — the earlier
/// combined `host:port` in the `host` key was only ever tolerated on read.
pub(crate) fn apply<R: CommandRunner>(
    runner: &mut R,
    host: &str,
    port: u16,
    enabled: bool,
) -> Result<(), ActorFailure> {
    if enabled {
        // #94: gsettings wants the bare address in `host` — a bracketed
        // IPv6 literal must lose its brackets, exactly like restore does.
        let (host_part, _) = split_endpoint_parts(host);
        let port_text = port.to_string();
        run(
            runner,
            &["set", "org.gnome.system.proxy.http", "host", host_part],
        )?;
        run(
            runner,
            &["set", "org.gnome.system.proxy.http", "port", &port_text],
        )?;
    }
    run(
        runner,
        &[
            "set",
            "org.gnome.system.proxy",
            "mode",
            if enabled { "manual" } else { "none" },
        ],
    )?;
    Ok(())
}

/// `sysproxy pac <url>`: switch GNOME to auto mode pointing at the PAC
/// URL (`org.gnome.system.proxy.autoconfig-url` + `mode auto`).
pub(crate) fn apply_pac<R: CommandRunner>(runner: &mut R, url: &str) -> Result<(), ActorFailure> {
    run(
        runner,
        &["set", "org.gnome.system.proxy", "autoconfig-url", url],
    )?;
    run(runner, &["set", "org.gnome.system.proxy", "mode", "auto"])?;
    Ok(())
}

/// Captures the current GNOME proxy state as `(mode, endpoint)`.
///
/// The endpoint is only meaningful for `manual` mode and is rendered as
/// `host:port`; for `auto` mode the endpoint holds the PAC URL. Any
/// unreadable component degrades to an empty endpoint rather than failing
/// the capture.
///
/// Audit #114 (ported from the KDE backend): an unreadable *mode* yields
/// `("unknown", "")`, never `none` — the pre-fix shape let a transient
/// gsettings failure silently erase the recorded proxy state, and the
/// subsequent restore then actively disabled a proxy we never observed.
pub(crate) fn capture<R: CommandRunner>(runner: &mut R) -> (String, String) {
    let Some(mode) = run(runner, &["get", "org.gnome.system.proxy", "mode"])
        .ok()
        .and_then(|result| parse_gsettings_value(stdout_text(&result)))
    else {
        return ("unknown".to_owned(), String::new());
    };
    if mode == "auto" {
        let pac_url = run(runner, &["get", "org.gnome.system.proxy", "autoconfig-url"])
            .ok()
            .and_then(|result| parse_gsettings_value(stdout_text(&result)))
            .unwrap_or_default();
        return (mode, pac_url);
    }
    if mode != "manual" {
        return (mode, String::new());
    }
    let host = run(runner, &["get", "org.gnome.system.proxy.http", "host"])
        .ok()
        .and_then(|result| parse_gsettings_value(stdout_text(&result)))
        .unwrap_or_default();
    if host.is_empty() {
        return (mode, String::new());
    }
    // Some tools (including earlier caly versions) store a combined
    // `host:port` in the host key; accept it directly when well-formed.
    if endpoint_port(&host).is_some() {
        return (mode, host);
    }
    let port = run(runner, &["get", "org.gnome.system.proxy.http", "port"])
        .ok()
        .and_then(|result| parse_gsettings_u32(stdout_text(&result)));
    let Some(port) = port else {
        return (mode, String::new());
    };
    (mode, format!("{host}:{port}"))
}

/// Parses a `gsettings get` numeric value, tolerating the `uint32 ` prefix.
pub(crate) fn parse_gsettings_u32(raw: &str) -> Option<u16> {
    let trimmed = raw.trim();
    let digits = trimmed.strip_prefix("uint32 ").unwrap_or(trimmed);
    digits.parse::<u16>().ok()
}

/// The trailing port of a `host:port` endpoint, when well-formed.
///
/// Audit #94: bracket-aware — `[::1]:7890` yields `7890`; a bare IPv6
/// literal has no port separator semantics and yields `None` (the old
/// `rfind(':')` read `::1` as host `::` / port `1`).
fn endpoint_port(endpoint: &str) -> Option<u16> {
    let (_host, port) = split_endpoint_parts(endpoint);
    port?.parse::<u16>().ok()
}

/// Restores a previously captured GNOME proxy state.
///
/// `manual` without a usable endpoint degrades to disabling the proxy: an
/// unknown manual endpoint must not be replaced with a guess.
pub(crate) fn restore<R: CommandRunner>(
    runner: &mut R,
    mode: &str,
    endpoint: &str,
) -> Result<(), ActorFailure> {
    match mode {
        "manual" if !endpoint.is_empty() => {
            let (host, port) = split_endpoint(endpoint);
            run(
                runner,
                &["set", "org.gnome.system.proxy.http", "host", host],
            )?;
            run(
                runner,
                &["set", "org.gnome.system.proxy.http", "port", port],
            )?;
            run(runner, &["set", "org.gnome.system.proxy", "mode", "manual"])?;
        }
        "auto" => {
            // Re-point the PAC URL (captured alongside the auto mode)
            // before re-engaging auto: restoring mode-only would leave
            // caly's own PAC active (2026-08-12 agent audit).
            if !endpoint.is_empty() {
                run(
                    runner,
                    &["set", "org.gnome.system.proxy", "autoconfig-url", endpoint],
                )?;
            }
            run(runner, &["set", "org.gnome.system.proxy", "mode", "auto"])?;
        }
        "unknown" => {
            // Audit #114: a capture failure yields `unknown`; restoring must
            // not disable a proxy we never observed. Leave gsettings
            // untouched.
            return Ok(());
        }
        _ => {
            run(runner, &["set", "org.gnome.system.proxy", "mode", "none"])?;
        }
    }
    Ok(())
}

/// Splits a `host:port` endpoint for the gsettings host/port keys.
fn split_endpoint(endpoint: &str) -> (&str, &str) {
    let (host, port) = split_endpoint_parts(endpoint);
    (host, port.unwrap_or("0"))
}

/// Bracket-aware endpoint splitter (#94):
/// - `[v6]:port` → (`v6`, Some(`port`)) — gsettings wants the bare address;
/// - `[v6]` / bare `v6` → (`v6`, None) — a bare IPv6 literal carries no port;
/// - `host:port` / bare host → the classic split (IPv4 or hostname).
fn split_endpoint_parts(endpoint: &str) -> (&str, Option<&str>) {
    if let Some(rest) = endpoint.strip_prefix('[')
        && let Some(close) = rest.find(']')
    {
        let host = &rest[..close];
        let tail = &rest[close + 1..];
        return (host, tail.strip_prefix(':'));
    }
    // More than one colon means a bare IPv6 literal without brackets.
    if endpoint.matches(':').count() > 1 {
        return (endpoint, None);
    }
    match endpoint.rfind(':') {
        Some(index) if index > 0 => ((&endpoint[..index]), Some(&endpoint[index + 1..])),
        _ => (endpoint, None),
    }
}

/// Decodes the bounded stdout bytes of a command result as UTF-8.
fn stdout_text(result: &CommandResult) -> &str {
    std::str::from_utf8(result.stdout.as_slice()).unwrap_or_default()
}

/// Parses one `gsettings get` value, stripping the surrounding single quotes.
pub(crate) fn parse_gsettings_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let unquoted = trimmed.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(unquoted.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::BoundedVec;

    /// Runner returning scripted stdout values in order (exit code zero).
    pub(crate) struct ScriptedRunner {
        pub(crate) outputs: std::collections::VecDeque<&'static str>,
    }

    impl CommandRunner for ScriptedRunner {
        fn run_bounded(
            &mut self,
            _request: CommandRequest,
        ) -> Result<CommandResult, caly_platform::PlatformFailure> {
            let text = self.outputs.pop_front().unwrap_or_default();
            let mut stdout = BoundedVec::new();
            let _ = stdout.try_extend(text.as_bytes().to_vec());
            Ok(CommandResult {
                exit_code: Some(0),
                stdout,
                stderr: BoundedVec::new(),
            })
        }
    }

    /// Runner recording every command line for assertion.
    pub(crate) struct RecordingRunner {
        pub(crate) commands: Vec<String>,
    }

    impl CommandRunner for RecordingRunner {
        fn run_bounded(
            &mut self,
            request: CommandRequest,
        ) -> Result<CommandResult, caly_platform::PlatformFailure> {
            let args: Vec<String> = request
                .arguments
                .iter()
                .map(|argument| argument.as_str().to_owned())
                .collect();
            self.commands.push(format!(
                "{} {}",
                request.executable.display(),
                args.join(" ")
            ));
            Ok(CommandResult {
                exit_code: Some(0),
                stdout: BoundedVec::new(),
                stderr: BoundedVec::new(),
            })
        }
    }

    #[test]
    fn parse_gsettings_value_strips_quotes() {
        assert_eq!(
            parse_gsettings_value("'manual'\n").as_deref(),
            Some("manual")
        );
        assert_eq!(parse_gsettings_value("").as_deref(), None);
        assert_eq!(parse_gsettings_value("manual").as_deref(), None);
    }

    #[test]
    fn parse_gsettings_u32_tolerates_type_prefix() {
        assert_eq!(parse_gsettings_u32("uint32 3128"), Some(3128));
        assert_eq!(parse_gsettings_u32("3128\n"), Some(3128));
        assert_eq!(parse_gsettings_u32("not-a-port"), None);
    }

    #[test]
    fn split_endpoint_separates_host_and_port() {
        assert_eq!(split_endpoint("127.0.0.1:7890"), ("127.0.0.1", "7890"));
        // Audit #94: gsettings `host` wants the bare IPv6 address, not the
        // bracketed form — the pre-fix splitter wrote `"[::1]"` verbatim.
        assert_eq!(split_endpoint("[::1]:7890"), ("::1", "7890"));
        assert_eq!(split_endpoint("noport"), ("noport", "0"));
    }

    #[test]
    fn capture_reads_split_host_and_port_keys() {
        let mut runner = ScriptedRunner {
            outputs: ["'manual'", "'10.0.0.1'", "uint32 3128"].into(),
        };
        assert_eq!(
            capture(&mut runner),
            ("manual".into(), "10.0.0.1:3128".into())
        );
    }

    #[test]
    fn capture_accepts_combined_host_port_value() {
        let mut runner = ScriptedRunner {
            outputs: ["'manual'", "'10.0.0.1:3128'"].into(),
        };
        assert_eq!(
            capture(&mut runner),
            ("manual".into(), "10.0.0.1:3128".into())
        );
    }

    #[test]
    fn capture_none_mode_has_no_endpoint() {
        let mut runner = ScriptedRunner {
            outputs: ["'none'"].into(),
        };
        assert_eq!(capture(&mut runner), ("none".into(), String::new()));
    }

    #[test]
    fn apply_enabled_writes_split_host_and_port_keys() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        apply(&mut runner, "127.0.0.1", 7890, true)?;
        let joined = runner.commands.join("\n");
        assert!(
            joined.contains("set org.gnome.system.proxy.http host 127.0.0.1"),
            "{joined}"
        );
        assert!(
            joined.contains("set org.gnome.system.proxy.http port 7890"),
            "{joined}"
        );
        assert!(
            joined.contains("set org.gnome.system.proxy mode manual"),
            "{joined}"
        );
        assert!(
            !joined.contains("host 127.0.0.1:7890"),
            "combined host:port must not be written: {joined}"
        );
        Ok(())
    }

    #[test]
    fn apply_enabled_strips_ipv6_brackets_for_gsettings() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        apply(&mut runner, "[::1]", 7890, true)?;
        let joined = runner.commands.join("\n");
        assert!(
            joined.contains("set org.gnome.system.proxy.http host ::1"),
            "{joined}"
        );
        assert!(
            joined.contains("set org.gnome.system.proxy.http port 7890"),
            "{joined}"
        );
        Ok(())
    }

    #[test]
    fn apply_disabled_writes_mode_none_only() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        apply(&mut runner, "127.0.0.1", 7890, false)?;
        let joined = runner.commands.join("\n");
        assert!(
            joined.contains("set org.gnome.system.proxy mode none"),
            "{joined}"
        );
        assert!(!joined.contains("http"), "{joined}");
        Ok(())
    }

    #[test]
    fn restore_manual_writes_endpoint_then_mode() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "manual", "10.0.0.1:3128")?;
        let joined = runner.commands.join("\n");
        assert!(joined.contains("set org.gnome.system.proxy.http host 10.0.0.1"));
        assert!(joined.contains("set org.gnome.system.proxy.http port 3128"));
        assert!(joined.contains("set org.gnome.system.proxy mode manual"));
        Ok(())
    }

    #[test]
    fn restore_manual_without_endpoint_disables() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "manual", "")?;
        assert!(runner.commands.join("\n").contains("mode none"));
        Ok(())
    }

    #[test]
    fn split_endpoint_is_bracket_aware_for_ipv6() {
        // Audit #94: bare and bracketed IPv6 endpoints must not be diced at
        // the last colon.
        assert_eq!(split_endpoint("[::1]:7890"), ("::1", "7890"));
        assert_eq!(split_endpoint("::1"), ("::1", "0"));
        assert_eq!(split_endpoint("2001:db8::5"), ("2001:db8::5", "0"));
        assert_eq!(split_endpoint("127.0.0.1:7890"), ("127.0.0.1", "7890"));
        assert_eq!(split_endpoint("proxy.lan"), ("proxy.lan", "0"));
        assert_eq!(endpoint_port("[::1]:7890"), Some(7890));
        assert_eq!(endpoint_port("::1"), None);
        assert_eq!(endpoint_port("127.0.0.1:7890"), Some(7890));
    }

    #[test]
    fn restore_auto_sets_auto_mode() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "auto", "")?;
        assert!(runner.commands.join("\n").contains("mode auto"));
        Ok(())
    }
}

#[cfg(test)]
mod pac_tests {
    use super::tests::{RecordingRunner, ScriptedRunner};
    use super::*;

    #[test]
    fn apply_pac_sets_autoconfig_url_then_auto_mode() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        apply_pac(&mut runner, "file:///tmp/caly/proxy.pac")?;
        let joined = runner.commands.join("\n");
        assert!(
            joined.contains("set org.gnome.system.proxy autoconfig-url file:///tmp/caly/proxy.pac"),
            "{joined}"
        );
        assert!(
            joined.contains("set org.gnome.system.proxy mode auto"),
            "{joined}"
        );
        Ok(())
    }

    #[test]
    fn capture_auto_mode_reads_the_pac_url() {
        let mut runner = ScriptedRunner {
            outputs: ["'auto'", "'file:///tmp/caly/proxy.pac'"].into(),
        };
        assert_eq!(
            capture(&mut runner),
            ("auto".into(), "file:///tmp/caly/proxy.pac".into())
        );
    }

    #[test]
    fn capture_auto_without_url_keeps_empty_endpoint() {
        let mut runner = ScriptedRunner {
            outputs: ["'auto'", "''"].into(),
        };
        assert_eq!(capture(&mut runner), ("auto".into(), String::new()));
    }

    /// Restoring a captured `auto` mode re-applies the mode without
    /// touching the endpoint (the PAC URL is untouched by caly).
    #[test]
    fn restore_auto_reapplies_auto_mode() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "auto", "file:///tmp/caly/proxy.pac")?;
        assert!(
            runner
                .commands
                .join("\n")
                .contains("set org.gnome.system.proxy mode auto")
        );
        Ok(())
    }

    #[test]
    fn capture_unreadable_mode_degrades_to_unknown() {
        // Audit #114: a failing gsettings mode read must not masquerade as
        // `none` — the pre-fix shape silently erased the recorded state and
        // the restore then actively disabled a proxy caly never observed.
        let mut runner = ScriptedRunner {
            outputs: std::collections::VecDeque::new(),
        };
        assert_eq!(capture(&mut runner), ("unknown".into(), String::new()));
    }

    #[test]
    fn restore_unknown_never_touches_gsettings() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "unknown", "")?;
        assert!(
            runner.commands.is_empty(),
            "unknown restore must be a no-op, ran: {}",
            runner.commands.join("\n")
        );
        Ok(())
    }
}
