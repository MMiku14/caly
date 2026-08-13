//! Desktop session / system-proxy state read surface.
//!
//! P8b: moved up from `caly-backends` so the presentation layer can
//! inspect the desktop proxy state without reaching into adapters —
//! platform owns system touchpoints (M12), and the read side of the
//! GNOME/KDE/niri backends is exactly that: gsettings/kioslaverc/
//! environment.d reads with no side effects.
//!
//! The WRITE side (apply/restore/engage) stays in the `PlatformCommandBackend`
//! implementations inside `caly-backends`, which delegate their read side
//! back to [`capture_proxy_state`] so both paths always agree.

use std::path::{Path, PathBuf};

use crate::command::{
    CommandArguments, CommandRequest, CommandResult, CommandRunner, LinuxCommandRunner,
};

/// Supported Linux desktop proxy backends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopProxyMode {
    /// GNOME / GNOME-based desktops via `gsettings`.
    Gnome,
    /// KDE Plasma via `kwriteconfig5`/`kwriteconfig6` writing `kioslaverc`.
    Kde,
    /// niri (Wayland compositor) via user `environment.d` proxy variables.
    Niri,
    /// A desktop with no supported system-proxy mechanism.
    Unsupported,
}

/// Detects the current desktop proxy mode from the session environment.
pub fn detect_desktop_mode() -> DesktopProxyMode {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default()
        .to_lowercase();
    classify_desktop(&desktop)
}

/// Pure classification of a lowercased desktop/session identifier.
fn classify_desktop(desktop: &str) -> DesktopProxyMode {
    if desktop.contains("gnome") {
        DesktopProxyMode::Gnome
    } else if desktop.contains("kde") || desktop.contains("plasma") {
        DesktopProxyMode::Kde
    } else if desktop.contains("niri") {
        DesktopProxyMode::Niri
    } else {
        DesktopProxyMode::Unsupported
    }
}

/// Captures the current desktop proxy state as `(mode, endpoint)` through a
/// fresh runner, mirroring the daemon-side capture exactly (single source of
/// truth for the read side).
pub fn capture_proxy_state(mode: DesktopProxyMode) -> (String, String) {
    match mode {
        DesktopProxyMode::Gnome => capture_gnome(&mut LinuxCommandRunner),
        DesktopProxyMode::Kde => capture_kde(&mut LinuxCommandRunner),
        DesktopProxyMode::Niri => capture_niri(),
        DesktopProxyMode::Unsupported => ("none".to_owned(), String::new()),
    }
}

/// Runs one command and requires exit code zero (read-side tolerance: a
/// probe failure yields `None`, never a crash).
fn run_ok<R: CommandRunner>(runner: &mut R, executable: PathBuf, args: &[&str]) -> Option<CommandResult> {
    let arguments = arguments(args)?;
    runner
        .run_bounded(CommandRequest {
            executable,
            arguments,
            timeout: std::time::Duration::from_secs(2),
        })
        .ok()
        .filter(|result| result.exit_code == Some(0))
}

/// Bounded argument list (read-side probe arguments are short; an over-long
/// one yields `None`).
fn arguments(values: &[&str]) -> Option<CommandArguments> {
    let mut out = CommandArguments::new();
    for value in values {
        let argument = caly_domain::BoundedText::new((*value).to_owned()).ok()?;
        out.try_push(argument).ok()?;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// GNOME (gsettings)
// ---------------------------------------------------------------------------

/// Captures the current GNOME proxy state as `(mode, endpoint)`.
///
/// `auto` reports the PAC URL; `manual` reports `host:port`; unreadable mode
/// degrades to `("unknown", "")` — Audit #114: a probe failure must never
/// masquerade as "no proxy", or the shutdown restore would write `none` over
/// the operator's genuine configuration.
fn capture_gnome<R: CommandRunner>(runner: &mut R) -> (String, String) {
    let Some(mode) = run_ok(runner, gsettings(), &["get", "org.gnome.system.proxy", "mode"])
        .and_then(|result| parse_gsettings_value(stdout_text(&result)))
    else {
        return ("unknown".to_owned(), String::new());
    };
    if mode == "auto" {
        let pac_url = run_ok(
            runner,
            gsettings(),
            &["get", "org.gnome.system.proxy", "autoconfig-url"],
        )
        .and_then(|result| parse_gsettings_value(stdout_text(&result)))
        .unwrap_or_default();
        return (mode, pac_url);
    }
    if mode != "manual" {
        return (mode, String::new());
    }
    let host = run_ok(
        runner,
        gsettings(),
        &["get", "org.gnome.system.proxy.http", "host"],
    )
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
    let port = run_ok(
        runner,
        gsettings(),
        &["get", "org.gnome.system.proxy.http", "port"],
    )
    .and_then(|result| parse_gsettings_u32(stdout_text(&result)));
    let Some(port) = port else {
        return (mode, String::new());
    };
    (mode, format!("{host}:{port}"))
}

fn gsettings() -> PathBuf {
    PathBuf::from("gsettings")
}

fn stdout_text(result: &CommandResult) -> &str {
    std::str::from_utf8(result.stdout.as_slice()).unwrap_or_default()
}

/// Parses one `gsettings get` value, stripping the surrounding single quotes.
fn parse_gsettings_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let unquoted = trimmed.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(unquoted.to_owned())
}

/// Parses a `gsettings get` numeric value, tolerating the `uint32 ` prefix.
fn parse_gsettings_u32(raw: &str) -> Option<u16> {
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

// ---------------------------------------------------------------------------
// KDE Plasma (kioslaverc via kreadconfig)
// ---------------------------------------------------------------------------

const KDE_PROXY_GROUP: &str = "Proxy Settings";

/// KDE `ProxyType` values: 0 = no proxy, 1 = manual, 2 = automatic (PAC).
fn kreadconfig_tool() -> PathBuf {
    if std::env::var("KDE_SESSION_VERSION").as_deref() == Ok("6") {
        PathBuf::from("kreadconfig6")
    } else {
        PathBuf::from("kreadconfig5")
    }
}

/// Captures the current KDE proxy state as `(mode, endpoint)`.
///
/// The endpoint is the `httpProxy` URL with its scheme stripped, and is only
/// meaningful for manual mode. Unreadable state degrades to `("unknown", "")`
/// (Audit #114: a probe failure must not masquerade as "no proxy", or the
/// shutdown restore would write `ProxyType=0` over the operator's genuine
/// manual configuration).
fn capture_kde<R: CommandRunner>(runner: &mut R) -> (String, String) {
    let tool = kreadconfig_tool();
    let Some(raw) = read_key(runner, &tool, "ProxyType") else {
        return ("unknown".to_owned(), String::new());
    };
    match raw.trim() {
        "1" => {
            // Audit #114 (endpoint arm): an unreadable `httpProxy` in a
            // confirmed manual config must not masquerade as an empty
            // endpoint — same unknown treatment as the `ProxyType` read.
            let Some(http_proxy) = read_key(runner, &tool, "httpProxy") else {
                return ("unknown".to_owned(), String::new());
            };
            ("manual".to_owned(), strip_http_scheme(http_proxy.trim()))
        }
        "2" => ("auto".to_owned(), String::new()),
        _ => ("none".to_owned(), String::new()),
    }
}

fn read_key<R: CommandRunner>(runner: &mut R, tool: &Path, key: &str) -> Option<String> {
    let result = run_ok(
        runner,
        tool.to_path_buf(),
        &[KDE_PROXY_GROUP, "--key", key],
    )?;
    Some(std::str::from_utf8(result.stdout.as_slice()).unwrap_or_default().trim().to_owned())
}

/// Strips an `http://` or `https://` scheme prefix from a proxy URL.
fn strip_http_scheme(value: &str) -> String {
    let trimmed = value.trim();
    let stripped = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    stripped.trim_end_matches('/').to_owned()
}

// ---------------------------------------------------------------------------
// niri (environment.d)
// ---------------------------------------------------------------------------

/// Captures the current niri proxy state as `(mode, endpoint)`.
///
/// The session file is caly-owned; its presence with a parseable export means
/// a manual proxy endpoint was active. Unreadable state degrades to `none`.
fn capture_niri() -> (String, String) {
    let Some(path) = niri_env_path() else {
        return ("none".to_owned(), String::new());
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return ("none".to_owned(), String::new());
    };
    match parse_niri_endpoint(&contents) {
        Some(endpoint) => ("manual".to_owned(), endpoint),
        None => ("none".to_owned(), String::new()),
    }
}

fn niri_env_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/environment.d/caly-proxy.conf"))
}

fn parse_niri_endpoint(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("export http_proxy=") else {
            continue;
        };
        let unquoted = rest.trim().trim_matches('"');
        let stripped = unquoted.strip_prefix("http://").unwrap_or(unquoted);
        if !stripped.is_empty() {
            return Some(stripped.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runner returning scripted stdout values in order (exit code zero).
    struct ScriptedRunner {
        outputs: std::collections::VecDeque<&'static str>,
    }

    impl CommandRunner for ScriptedRunner {
        fn run_bounded(
            &mut self,
            _request: CommandRequest,
        ) -> Result<CommandResult, crate::PlatformFailure> {
            // A scripted command with no output left simulates a probe
            // failure (nonzero exit): the read side must degrade to
            // `unknown`, never to an empty-but-ok read (Audit #114).
            let Some(stdout) = self.outputs.pop_front() else {
                return Ok(CommandResult {
                    exit_code: Some(1),
                    stdout: caly_domain::BoundedVec::new(),
                    stderr: caly_domain::BoundedVec::new(),
                });
            };
            Ok(CommandResult {
                exit_code: Some(0),
                stdout: caly_domain::BoundedVec::try_from_vec(stdout.as_bytes().to_vec())
                    .unwrap_or_default(),
                stderr: caly_domain::BoundedVec::new(),
            })
        }
    }

    #[test]
    fn parse_gsettings_value_strips_quotes() {
        assert_eq!(parse_gsettings_value("'manual'"), Some("manual".to_owned()));
        assert_eq!(parse_gsettings_value("'auto'"), Some("auto".to_owned()));
        assert_eq!(parse_gsettings_value("none"), None);
        assert_eq!(parse_gsettings_value("''"), Some(String::new()));
    }

    #[test]
    fn parse_gsettings_u32_tolerates_type_prefix() {
        assert_eq!(parse_gsettings_u32("uint32 7890"), Some(7890));
        assert_eq!(parse_gsettings_u32("7890"), Some(7890));
        assert_eq!(parse_gsettings_u32("not-a-number"), None);
    }

    #[test]
    fn capture_reads_split_host_and_port_keys() {
        let mut runner = ScriptedRunner {
            outputs: vec!["'manual'", "'127.0.0.1'", "uint32 7890"].into(),
        };
        assert_eq!(
            capture_gnome(&mut runner),
            ("manual".to_owned(), "127.0.0.1:7890".to_owned())
        );
    }

    #[test]
    fn capture_accepts_combined_host_port_value() {
        let mut runner = ScriptedRunner {
            outputs: vec!["'manual'", "'127.0.0.1:7890'"].into(),
        };
        assert_eq!(
            capture_gnome(&mut runner),
            ("manual".to_owned(), "127.0.0.1:7890".to_owned())
        );
    }

    #[test]
    fn capture_none_mode_has_no_endpoint() {
        let mut runner = ScriptedRunner {
            outputs: vec!["'none'"].into(),
        };
        assert_eq!(capture_gnome(&mut runner), ("none".to_owned(), String::new()));
    }

    #[test]
    fn capture_unreadable_mode_is_unknown_not_none() {
        // Audit #114: a failed probe must not masquerade as "no proxy".
        let mut runner = ScriptedRunner {
            outputs: std::collections::VecDeque::new(),
        };
        assert_eq!(
            capture_gnome(&mut runner),
            ("unknown".to_owned(), String::new())
        );
    }

    #[test]
    fn capture_manual_strips_http_scheme() {
        let mut runner = ScriptedRunner {
            outputs: vec!["1", "http://127.0.0.1:7890/"].into(),
        };
        assert_eq!(
            capture_kde(&mut runner),
            ("manual".to_owned(), "127.0.0.1:7890".to_owned())
        );
    }

    #[test]
    fn capture_manual_with_unreadable_http_proxy_is_unknown() {
        // Audit #114 (endpoint arm): missing httpProxy in a manual config
        // is unknown, not an empty endpoint.
        let mut runner = ScriptedRunner {
            outputs: vec!["1"].into(),
        };
        assert_eq!(
            capture_kde(&mut runner),
            ("unknown".to_owned(), String::new())
        );
    }

    #[test]
    fn capture_auto_and_none_modes() {
        let mut runner = ScriptedRunner {
            outputs: vec!["2"].into(),
        };
        assert_eq!(capture_kde(&mut runner), ("auto".to_owned(), String::new()));
        let mut runner = ScriptedRunner {
            outputs: vec!["0"].into(),
        };
        assert_eq!(capture_kde(&mut runner), ("none".to_owned(), String::new()));
    }

    #[test]
    fn strip_http_scheme_handles_prefixes_and_slashes() {
        assert_eq!(strip_http_scheme("http://a:1/"), "a:1");
        assert_eq!(strip_http_scheme("https://b:2"), "b:2");
        assert_eq!(strip_http_scheme("plain:3"), "plain:3");
    }

    #[test]
    fn parse_niri_endpoint_reads_exported_endpoint() {
        let contents = "export http_proxy=\"http://127.0.0.1:7890\"\n";
        assert_eq!(
            parse_niri_endpoint(contents),
            Some("127.0.0.1:7890".to_owned())
        );
    }

    #[test]
    fn parse_niri_endpoint_ignores_unset_files() {
        assert_eq!(parse_niri_endpoint(""), None);
        assert_eq!(parse_niri_endpoint("# comment\nexport HTTPS_PROXY=\"http://x\"\n"), None);
    }

    #[test]
    fn split_endpoint_parts_is_bracket_aware() {
        assert_eq!(split_endpoint_parts("[::1]:7890"), ("::1", Some("7890")));
        assert_eq!(split_endpoint_parts("127.0.0.1:7890"), ("127.0.0.1", Some("7890")));
        assert_eq!(split_endpoint_parts("::1"), ("::1", None));
    }
}
