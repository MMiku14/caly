//! niri (Wayland compositor) proxy support via user `environment.d`.

use std::{fmt::Write as FmtWrite, path::PathBuf};

use caly_ports::ActorFailure;

/// Renders the `environment.d` proxy file contents for an enabled/disabled state.
pub(crate) fn proxy_environment_contents(host: &str, port: u16, enabled: bool) -> String {
    let endpoint = if enabled {
        format!("http://{host}:{port}")
    } else {
        String::new()
    };
    let mut lines = String::new();
    for var in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
        if endpoint.is_empty() {
            let _ = writeln!(lines, "unset {var}");
        } else {
            let _ = writeln!(lines, "export {var}=\"{endpoint}\"");
        }
    }
    lines
}

/// Writes the user-level `environment.d` proxy file atomically.
pub(crate) fn write_environment_d(contents: &str) -> Result<(), ActorFailure> {
    let path = niri_env_path()?;
    std::fs::create_dir_all(
        path.parent().ok_or_else(|| {
            crate::failure("no home directory", "configure HOME to write proxy env")
        })?,
    )
    .map_err(|_| {
        crate::failure(
            "cannot create environment.d directory",
            "inspect HOME permissions",
        )
    })?;
    // Staging + rename so a concurrent login shell never reads a
    // half-written file (same discipline as the PAC and geoip writes).
    let staging = path.with_extension("tmp");
    std::fs::write(&staging, contents).map_err(|_| {
        crate::failure(
            "cannot write niri proxy environment file",
            "inspect HOME permissions",
        )
    })?;
    std::fs::rename(&staging, &path).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        crate::failure(
            &format!("cannot publish niri proxy environment file: {error}"),
            "inspect HOME permissions",
        )
    })?;
    Ok(())
}

fn niri_env_path() -> Result<PathBuf, ActorFailure> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        crate::failure(
            "no HOME to locate environment.d",
            "set HOME in the daemon environment",
        )
    })?;
    Ok(PathBuf::from(home).join(".config/environment.d/caly-proxy.conf"))
}

/// Captures the current niri proxy state as `(mode, endpoint)`.
///
/// The session file is caly-owned; its presence with a parseable export means
/// a manual proxy endpoint was active. Unreadable state degrades to `none`.


pub(crate) fn restore(endpoint: &str, manual: bool) -> Result<(), ActorFailure> {
    let contents = if manual && !endpoint.is_empty() {
        manual_environment_contents(endpoint)
    } else {
        proxy_environment_contents("127.0.0.1", 0, false)
    };
    write_environment_d(&contents)
}

/// Renders an enabled environment file from a bare `host:port` endpoint.
pub(crate) fn manual_environment_contents(endpoint: &str) -> String {
    let url = format!("http://{endpoint}");
    let mut lines = String::new();
    for var in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
        let _ = writeln!(lines, "export {var}=\"{url}\"");
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_environment_exports_proxy_vars() {
        let contents = proxy_environment_contents("127.0.0.1", 7890, true);
        assert!(contents.contains("export http_proxy=\"http://127.0.0.1:7890\""));
        assert!(contents.contains("export https_proxy=\"http://127.0.0.1:7890\""));
        assert!(contents.contains("export HTTP_PROXY=\"http://127.0.0.1:7890\""));
        assert!(contents.contains("export HTTPS_PROXY=\"http://127.0.0.1:7890\""));
    }

    #[test]
    fn disabled_environment_unexports_proxy_vars() {
        let contents = proxy_environment_contents("127.0.0.1", 7890, false);
        assert!(contents.contains("unset http_proxy"));
        assert!(contents.contains("unset https_proxy"));
        assert!(!contents.contains("export http_proxy"));
    }

    #[test]


    #[test]


    #[test]
    fn manual_environment_uses_captured_endpoint() {
        let contents = manual_environment_contents("10.0.0.1:8080");
        assert!(contents.contains("export http_proxy=\"http://10.0.0.1:8080\""));
    }
}
