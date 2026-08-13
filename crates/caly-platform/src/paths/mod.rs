//! Validated application path ownership.

use std::path::{Component, Path, PathBuf};

/// Shared filesystem helpers for tests across the workspace. The module is
/// unconditionally compiled (gated only on the host's `std::time` and
/// `std::fs`) so every integration test can reach it through
/// `caly_platform::paths::test_helpers` without an extra `dev-dependencies`
/// round trip. Production code must not use this module.
pub mod test_helpers;

/// Rejects traversal and multi-component user-controlled names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathNameError {
    Empty,
    TooLong,
    InvalidCharacter,
    MultipleComponents,
}

/// Safe single path component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeName(String);

impl SafeName {
    /// Accepts 1..=128 ASCII alphanumeric, dash or underscore characters.
    pub fn new(value: impl Into<String>) -> Result<Self, PathNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PathNameError::Empty);
        }
        if value.len() > 128 {
            return Err(PathNameError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(PathNameError::InvalidCharacter);
        }
        let mut components = Path::new(&value).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(PathNameError::MultipleComponents);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Resolved application-owned roots following the XDG base-directory spec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
}

impl AppPaths {
    /// Resolves the four XDG roots and joins an application-owned subdirectory.
    ///
    /// `xdg_runtime` is optional because `$XDG_RUNTIME_DIR` is not always set;
    /// when absent the shared temporary directory is used as the runtime base.
    /// Empty relative values are treated as unset per the XDG spec.
    pub fn resolve(
        home: &Path,
        xdg_config: Option<&Path>,
        xdg_data: Option<&Path>,
        xdg_state: Option<&Path>,
        xdg_runtime: Option<&Path>,
    ) -> Self {
        let config_base = xdg_config
            .filter(|path| Self::is_usable(path))
            .map_or_else(|| home.join(".config"), Path::to_path_buf);
        let data_base = xdg_data
            .filter(|path| Self::is_usable(path))
            .map_or_else(|| home.join(".local/share"), Path::to_path_buf);
        let state_base = xdg_state
            .filter(|path| Self::is_usable(path))
            .map_or_else(|| home.join(".local/state"), Path::to_path_buf);
        let runtime_root = xdg_runtime
            .filter(|path| Self::is_usable(path))
            .map_or_else(
                || std::env::temp_dir().join(format!("caly-{}", Self::runtime_label(home))),
                Path::to_path_buf,
            );
        Self {
            config: config_base.join("caly"),
            data: data_base.join("caly"),
            state: state_base.join("caly"),
            runtime: runtime_root.join("caly"),
        }
    }

    /// Returns `$XDG_RUNTIME_DIR`-derived paths under `runtime`.
    pub fn socket_path(&self) -> PathBuf {
        self.runtime.join("daemon.sock")
    }

    /// Returns the instance-lock path under `runtime`.
    pub fn lock_path(&self) -> PathBuf {
        self.runtime.join("daemon.lock")
    }

    /// Returns the owner-only controller-secret file under `runtime`. The daemon
    /// writes its generated controller secret here so offline CLI queries
    /// (proxy-groups/connections/traffic) can authenticate against the running
    /// core's controller.
    pub fn controller_secret_path(&self) -> PathBuf {
        self.runtime.join("controller.secret")
    }

    /// Returns the core working-directory under `runtime`.
    pub fn core_work_dir(&self) -> PathBuf {
        self.runtime.join("cores")
    }

    /// Returns the durable recovery-record path under `state`.
    pub fn recovery_record_path(&self) -> PathBuf {
        self.state.join("recovery.json")
    }

    /// Returns the durable TUN recovery-record path under `state` — the
    /// single source of truth for the filename, shared by the composition
    /// layer (store open) and `tun status` (offline projection).
    pub fn tun_recovery_record_path(&self) -> PathBuf {
        self.state.join("recovery-tun.json")
    }

    /// Returns the durable node-selection record path under `state`.
    pub fn node_selection_path(&self) -> PathBuf {
        self.state.join("node-selection.json")
    }

    /// An absolute, non-empty path is usable as an XDG base directory.
    fn is_usable(path: &Path) -> bool {
        !path.as_os_str().is_empty() && path.is_absolute()
    }

    /// Human label for the `/tmp` runtime fallback: without `$XDG_RUNTIME_DIR`
    /// the runtime root must be per-user (`/tmp/caly-<user>`) instead of a
    /// shared fixed `/tmp/caly`, so a hostile or crashed peer cannot interfere
    /// with the owner-only socket/lock under a shared name.
    fn runtime_label(home: &Path) -> String {
        let label = home
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("user");
        // Keep only safe characters for the directory name.
        let sanitized: String = label
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if sanitized.is_empty() {
            "user".to_owned()
        } else {
            sanitized
        }
    }

    /// Resolves the application roots from the process environment (XDG + HOME).
    pub fn from_env() -> Self {
        Self::from_env_vars(&|key| std::env::var_os(key))
    }

    /// Resolves the application roots from a supplied environment, keeping the
    /// path resolution hermetic and testable.
    pub fn from_env_vars(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        let home = env("HOME").map_or_else(std::env::temp_dir, PathBuf::from);
        Self::resolve(
            &home,
            env_path_via(env, "XDG_CONFIG_HOME").as_deref(),
            env_path_via(env, "XDG_DATA_HOME").as_deref(),
            env_path_via(env, "XDG_STATE_HOME").as_deref(),
            env_path_via(env, "XDG_RUNTIME_DIR").as_deref(),
        )
    }
}

/// Reads a non-empty environment value through a provider, if any.
fn env_path_via(env: &dyn Fn(&str) -> Option<std::ffi::OsString>, key: &str) -> Option<PathBuf> {
    env(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_rejects_traversal_and_separators() {
        assert_eq!(
            SafeName::new("../profile"),
            Err(PathNameError::InvalidCharacter)
        );
        assert_eq!(
            SafeName::new("profiles/main"),
            Err(PathNameError::InvalidCharacter)
        );
        assert_eq!(SafeName::new(""), Err(PathNameError::Empty));
    }

    #[test]
    fn safe_name_accepts_portable_component() -> Result<(), PathNameError> {
        let name = SafeName::new("profile_US-01")?;
        assert_eq!(name.as_str(), "profile_US-01");
        Ok(())
    }

    #[test]
    fn resolve_prefers_xdg_over_home_defaults() {
        let home = Path::new("/home/alice");
        let config = Path::new("/xdg/config");
        let data = Path::new("/xdg/data");
        let state = Path::new("/xdg/state");
        let runtime = Path::new("/run/user/1000");
        let paths = AppPaths::resolve(home, Some(config), Some(data), Some(state), Some(runtime));
        assert_eq!(paths.config, PathBuf::from("/xdg/config/caly"));
        assert_eq!(paths.data, PathBuf::from("/xdg/data/caly"));
        assert_eq!(paths.state, PathBuf::from("/xdg/state/caly"));
        assert_eq!(paths.runtime, PathBuf::from("/run/user/1000/caly"));
    }

    #[test]
    fn resolve_falls_back_to_home_when_xdg_absent() {
        let home = Path::new("/home/bob");
        let paths = AppPaths::resolve(home, None, None, None, None);
        assert_eq!(paths.config, PathBuf::from("/home/bob/.config/caly"));
        assert_eq!(paths.data, PathBuf::from("/home/bob/.local/share/caly"));
        assert_eq!(paths.state, PathBuf::from("/home/bob/.local/state/caly"));
        assert_eq!(
            paths.runtime,
            std::env::temp_dir().join("caly-bob").join("caly")
        );
    }

    #[test]
    fn relative_xdg_value_is_treated_as_unset() {
        let home = Path::new("/home/carol");
        let relative = Path::new("relative/dir");
        let paths = AppPaths::resolve(
            home,
            Some(relative),
            Some(relative),
            Some(relative),
            Some(relative),
        );
        assert_eq!(paths.config, PathBuf::from("/home/carol/.config/caly"));
        assert_eq!(paths.data, PathBuf::from("/home/carol/.local/share/caly"));
    }

    #[test]
    fn derived_paths_live_under_owned_roots() {
        let home = Path::new("/home/dan");
        let paths = AppPaths::resolve(home, None, None, None, None);
        assert_eq!(paths.socket_path(), paths.runtime.join("daemon.sock"));
        assert_eq!(paths.lock_path(), paths.runtime.join("daemon.lock"));
        assert_eq!(paths.core_work_dir(), paths.runtime.join("cores"));
        assert_eq!(
            paths.recovery_record_path(),
            paths.state.join("recovery.json")
        );
    }
}
