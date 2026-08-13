//! GNOME proxy support via `gsettings` (`org.gnome.system.proxy`).

use std::path::PathBuf;

use caly_platform::command::{CommandRequest, CommandResult, CommandRunner};
use caly_platform::desktop::split_endpoint_parts;

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
    super::run_ok(runner, request)
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

/// Restores a previously captured GNOME proxy state.
///
/// `manual` without a usable endpoint degrades to disabling the proxy: an
/// unknown manual endpoint must not be replaced with a guess.
///
/// Audit #114: `unknown` restores are a no-op — a capture failure must not
/// silently disable a proxy we never observed.
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

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::BoundedVec;

    /// Runner recording every command line for assertion.
    pub(super) struct RecordingRunner {
        pub(super) commands: Vec<String>,
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
    fn split_endpoint_separates_host_and_port() {
        assert_eq!(split_endpoint("127.0.0.1:7890"), ("127.0.0.1", "7890"));
        // Audit #94: gsettings `host` wants the bare IPv6 address, not the
        // bracketed form — the pre-fix splitter wrote `"[::1]"` verbatim.
        assert_eq!(split_endpoint("[::1]:7890"), ("::1", "7890"));
        assert_eq!(split_endpoint("noport"), ("noport", "0"));
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
    use super::tests::RecordingRunner;
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

    /// Restoring a captured `auto` mode re-applies the mode without
    /// touching the endpoint (the PAC URL is untouched by caly).
    #[test]
    fn restore_auto_reapplies_auto_mode() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "auto", "file:///tmp/caly/proxy.pac")?;
        assert!(runner
            .commands
            .join("\n")
            .contains("set org.gnome.system.proxy mode auto"));
        Ok(())
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
