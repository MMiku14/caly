//! KDE Plasma proxy support via `kwriteconfig` and KIO reload.

use std::path::PathBuf;

use caly_platform::command::{CommandRequest, CommandRunner};

use caly_ports::ActorFailure;

/// Picks kwriteconfig5 or kwriteconfig6 based on the running KDE major version.
pub(crate) fn kwriteconfig_tool() -> PathBuf {
    if std::env::var("KDE_SESSION_VERSION").as_deref() == Ok("6") {
        PathBuf::from("kwriteconfig6")
    } else {
        PathBuf::from("kwriteconfig5")
    }
}

/// KDE proxy-config keys written to `kioslaverc`.
pub(crate) const PROXY_GROUP: &[&str] = &["--file", "kioslaverc", "--group", "Proxy Settings"];

/// Runs one `kwriteconfig` write and notifies KIO to reload slave config.
pub(crate) fn apply_kde_proxy<R: CommandRunner>(
    runner: &mut R,
    tool: PathBuf,
    host: &str,
    port: u16,
    enabled: bool,
) -> Result<(), ActorFailure> {
    let group = PROXY_GROUP;
    write_key(
        runner,
        &tool,
        group,
        "ProxyType",
        if enabled { "1" } else { "0" },
    )?;
    if enabled {
        let endpoint = format!("http://{host}:{port}");
        for key in ["httpProxy", "httpsProxy", "ftpProxy", "socksProxy"] {
            write_key(runner, &tool, group, key, &endpoint)?;
        }
    }
    reload_kio(runner)
}

/// Restores a previously captured KDE proxy state.
///
/// `manual` writes `ProxyType=1` and the endpoint across the four proxy
/// keys; `auto` writes `ProxyType=2`; `unknown` is a no-op (Audit #114);
/// anything else writes `ProxyType=0`. KIO is reloaded afterwards.
pub(crate) fn restore<R: CommandRunner>(
    runner: &mut R,
    mode: &str,
    endpoint: &str,
) -> Result<(), ActorFailure> {
    let tool = kwriteconfig_tool();
    let group = PROXY_GROUP;
    match mode {
        "manual" if !endpoint.is_empty() => {
            write_key(runner, &tool, group, "ProxyType", "1")?;
            let endpoint_url = format!("http://{endpoint}");
            for key in ["httpProxy", "httpsProxy", "ftpProxy", "socksProxy"] {
                write_key(runner, &tool, group, key, &endpoint_url)?;
            }
        }
        "auto" => write_key(runner, &tool, group, "ProxyType", "2")?,
        "unknown" => {
            // Audit #114: a capture failure yields `unknown`; restoring must
            // not disable a proxy we never observed. Leave KIO untouched.
            return Ok(());
        }
        _ => write_key(runner, &tool, group, "ProxyType", "0")?,
    }
    reload_kio(runner)
}

/// Asks KIO to reload slave configuration so proxy changes apply live.
fn reload_kio<R: CommandRunner>(runner: &mut R) -> Result<(), ActorFailure> {
    let reload = CommandRequest {
        executable: PathBuf::from("dbus-send"),
        arguments: crate::platform::argv(&[&[
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
            "string:",
        ]])?,
        timeout: std::time::Duration::from_secs(2),
    };
    super::run_ok(runner, reload).map(|_| ())
}

fn write_key<R: CommandRunner>(
    runner: &mut R,
    tool: &std::path::Path,
    group: &[&str],
    key: &str,
    value: &str,
) -> Result<(), ActorFailure> {
    let request = CommandRequest {
        executable: tool.to_path_buf(),
        arguments: crate::platform::argv(&[group, &["--key", key, value]])?,
        timeout: std::time::Duration::from_secs(2),
    };
    super::run_ok(runner, request).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::BoundedVec;
    use caly_platform::command::CommandResult;

    struct RecordingRunner {
        commands: Vec<String>,
    }
    impl CommandRunner for RecordingRunner {
        fn run_bounded(
            &mut self,
            request: CommandRequest,
        ) -> Result<CommandResult, caly_platform::PlatformFailure> {
            let args: Vec<String> = request
                .arguments
                .iter()
                .map(|arg| arg.as_str().to_owned())
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
    fn kde_enable_writes_kioslaverc_and_reloads_kio() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        apply_kde_proxy(
            &mut runner,
            PathBuf::from("kwriteconfig5"),
            "127.0.0.1",
            7890,
            true,
        )?;
        let joined = runner.commands.join("\n");
        assert!(joined
            .contains("kwriteconfig5 --file kioslaverc --group Proxy Settings --key ProxyType 1"));
        assert!(joined.contains("--key httpProxy http://127.0.0.1:7890"));
        assert!(joined.contains("dbus-send --type=signal /KIO/Scheduler org.kde.KIO.Scheduler.reparseSlaveConfiguration string:"));
        Ok(())
    }

    #[test]
    fn kde_disable_writes_proxy_type_zero() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        apply_kde_proxy(
            &mut runner,
            PathBuf::from("kwriteconfig5"),
            "127.0.0.1",
            7890,
            false,
        )?;
        let joined = runner.commands.join("\n");
        assert!(joined.contains("--key ProxyType 0"));
        assert!(!joined.contains("httpProxy"));
        Ok(())
    }

    #[test]
    fn restore_manual_writes_endpoint_and_reloads() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "manual", "10.0.0.1:8080")?;
        let joined = runner.commands.join("\n");
        assert!(joined.contains("--key ProxyType 1"));
        assert!(joined.contains("--key httpProxy http://10.0.0.1:8080"));
        assert!(joined.contains("dbus-send"));
        Ok(())
    }

    #[test]
    fn restore_manual_without_endpoint_disables() -> Result<(), ActorFailure> {
        let mut runner = RecordingRunner {
            commands: Vec::new(),
        };
        restore(&mut runner, "manual", "")?;
        let joined = runner.commands.join("\n");
        assert!(joined.contains("--key ProxyType 0"));
        assert!(!joined.contains("httpProxy"));
        Ok(())
    }
}
