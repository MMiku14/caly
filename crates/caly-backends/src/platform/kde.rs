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

/// The `kreadconfig` tool matching the running KDE major version.
pub(crate) fn kreadconfig_tool() -> PathBuf {
    if std::env::var("KDE_SESSION_VERSION").as_deref() == Ok("6") {
        PathBuf::from("kreadconfig6")
    } else {
        PathBuf::from("kreadconfig5")
    }
}

/// Captures the current KDE proxy state as `(mode, endpoint)`.
///
/// KDE `ProxyType` values: 0 = no proxy, 1 = manual, 2 = automatic (PAC).
/// The endpoint is the `httpProxy` URL with its scheme stripped, and is only
/// meaningful for manual mode. Unreadable state degrades to `("none", "")`.
pub(crate) fn capture<R: CommandRunner>(runner: &mut R) -> (String, String) {
    let tool = kreadconfig_tool();
    // Audit #114: an UNREADABLE `ProxyType` (kreadconfig missing, KIO down,
    // transient failure) is `unknown`, not `none` — the pre-fix shape let a
    // probe failure masquerade as "no proxy", and the shutdown restore then
    // wrote `ProxyType=0` over the operator's genuine manual configuration.
    let Some(raw) = read_key(runner, &tool, "ProxyType") else {
        return ("unknown".to_owned(), String::new());
    };
    match raw.trim() {
        "1" => {
            // Audit #114 (endpoint arm): an unreadable `httpProxy` in a
            // confirmed manual config must not masquerade as an empty
            // endpoint — the pre-fix shape degraded to `("manual", "")` and
            // the restore then wrote `ProxyType=0`, clobbering the operator's
            // observed manual configuration. Same unknown treatment as the
            // `ProxyType` read failure.
            let Some(http_proxy) = read_key(runner, &tool, "httpProxy") else {
                return ("unknown".to_owned(), String::new());
            };
            ("manual".to_owned(), strip_http_scheme(http_proxy.trim()))
        }
        "2" => ("auto".to_owned(), String::new()),
        _ => ("none".to_owned(), String::new()),
    }
}

/// Restores a previously captured KDE proxy state and reloads KIO.
///
/// `manual` without a usable endpoint degrades to disabling the proxy: an
/// unknown manual endpoint must not be replaced with a guess.
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
    run_ok(runner, reload)
}

/// Reads one `kioslaverc` key; missing keys and failures yield `None`.
fn read_key<R: CommandRunner>(runner: &mut R, tool: &std::path::Path, key: &str) -> Option<String> {
    let request = CommandRequest {
        executable: tool.to_path_buf(),
        arguments: crate::platform::argv(&[PROXY_GROUP, &["--key", key]]).ok()?,
        timeout: std::time::Duration::from_secs(2),
    };
    let result = runner.run_bounded(request).ok()?;
    if result.exit_code != Some(0) {
        return None;
    }
    Some(
        std::str::from_utf8(result.stdout.as_slice())
            .unwrap_or_default()
            .trim()
            .to_owned(),
    )
}

/// Strips an `http://` or `https://` scheme prefix from a proxy URL.
pub(crate) fn strip_http_scheme(value: &str) -> String {
    let trimmed = value.trim();
    let stripped = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    stripped.trim_end_matches('/').to_owned()
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
    run_ok(runner, request)
}

fn run_ok<R: CommandRunner>(runner: &mut R, request: CommandRequest) -> Result<(), ActorFailure> {
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
    Ok(())
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

    /// Runner returning scripted stdout values in order (exit code zero).
    struct ScriptedRunner {
        /// Per-command scripted output; the parallel `exit_codes` queue is
        /// consumed per call so individual reads can fail independently
        /// (defaults to `Some(0)` when exhausted).
        outputs: std::collections::VecDeque<&'static str>,
        exit_codes: std::collections::VecDeque<Option<i32>>,
    }
    impl CommandRunner for ScriptedRunner {
        fn run_bounded(
            &mut self,
            _request: CommandRequest,
        ) -> Result<CommandResult, caly_platform::PlatformFailure> {
            let text = self.outputs.pop_front().unwrap_or_default();
            let exit_code = self.exit_codes.pop_front().unwrap_or(Some(0));
            let mut stdout = BoundedVec::new();
            let _ = stdout.try_extend(text.as_bytes().to_vec());
            Ok(CommandResult {
                exit_code,
                stdout,
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
        assert!(
            joined.contains(
                "kwriteconfig5 --file kioslaverc --group Proxy Settings --key ProxyType 1"
            )
        );
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
    fn capture_manual_strips_http_scheme() {
        let mut runner = ScriptedRunner {
            outputs: ["1", "http://10.0.0.1:8080"].into(),
            exit_codes: std::collections::VecDeque::new(),
        };
        assert_eq!(
            capture(&mut runner),
            ("manual".into(), "10.0.0.1:8080".into())
        );
    }

    #[test]
    fn capture_manual_with_unreadable_http_proxy_is_unknown() {
        // Audit #114 (endpoint arm): a confirmed manual config whose
        // httpProxy read *fails* must degrade to `unknown`, never
        // `("manual", "")` — the pre-fix shape let the shutdown restore
        // write `ProxyType=0` over the operator's observed manual proxy.
        let mut runner = ScriptedRunner {
            outputs: ["1", ""].into(),
            exit_codes: [Some(0), Some(1)].into(),
        };
        assert_eq!(capture(&mut runner), ("unknown".into(), String::new()));
    }

    #[test]
    fn capture_auto_and_none_modes() {
        let mut runner = ScriptedRunner {
            outputs: ["2"].into(),
            exit_codes: std::collections::VecDeque::new(),
        };
        assert_eq!(capture(&mut runner), ("auto".into(), String::new()));
        let mut runner = ScriptedRunner {
            outputs: ["0"].into(),
            exit_codes: std::collections::VecDeque::new(),
        };
        assert_eq!(capture(&mut runner), ("none".into(), String::new()));
    }

    #[test]
    fn strip_http_scheme_handles_prefixes_and_slashes() {
        assert_eq!(strip_http_scheme("http://10.0.0.1:8080/"), "10.0.0.1:8080");
        assert_eq!(strip_http_scheme("https://example:443"), "example:443");
        assert_eq!(strip_http_scheme("10.0.0.1:8080"), "10.0.0.1:8080");
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
