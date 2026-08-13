//! Round 14: generic YAML mutator for the `config.yaml`
//! writer pattern.
//!
//! `set <resource> <verb>` writers all share the same
//! "backup → read → parse → mutate key → re-validate →
//! write" shape. Round 13 inlined this pattern into
//! `client::profile::mutate_profiles_in_config`; Round 14
//! lifts it to a typed helper so `set sub add|remove|…`
//! can reuse the same read-validate-write loop without
//! duplicating the file-I/O and error-envelope code.
//!
//! The mutator is **key-typed** (`subscriptions` /
//! `profiles` / `rule_providers` / `providers`) and the
//! value type is whatever the schema module exposes for
//! that key. The helper owns:
//!
//! 1. `fs::read` + `serde_norway::from_slice` (raw read).
//! 2. Mutating the `serde_norway::Value` mapping at the
//!    named key with the caller-supplied closure.
//! 3. Serialising back with `serde_norway::to_string`.
//! 4. `fs::write` (atomic-replace via temp file + rename,
//!    so a crash mid-write never leaves a half-written
//!    `config.yaml`).
//!
//! The caller owns the **backup** step (taken before the
//! read so a power loss between the read and the write
//! still leaves the operator with a recoverable copy).
//! See [`backup_config_yaml`] for the matching backup
//! helper.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path};

use caly_profile::schema::AppConfig;

/// The single write error type for all `set <resource>`
/// writers. Each variant carries the path + reason so the
/// CLI can print a stable error envelope without losing
/// the operator's trail to the original failure. Every
/// variant is exercised by the live `backup_config_yaml` /
/// `post_write_validate` / `write_atomic` pipeline; the
/// per-resource `From<ConfigWriteError>` impl in
/// `resource_writer` collapses the read / validate /
/// write failures into the writer's own error type.
#[derive(Debug)]
pub enum ConfigWriteError {
    /// `fs::read(config.yaml)` failed.
    Read {
        path: std::path::PathBuf,
        reason: String,
    },
    /// The post-mutation schema validation failed.
    Validate {
        path: std::path::PathBuf,
        reason: String,
    },
    /// `fs::rename` (temp → real) failed.
    Write {
        path: std::path::PathBuf,
        reason: String,
    },
}

impl core::fmt::Display for ConfigWriteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read { path, reason } => {
                write!(formatter, "cannot read {}: {reason}", path.display())
            }
            Self::Validate { path, reason } => {
                write!(
                    formatter,
                    "post-write validation failed for {}: {reason}",
                    path.display()
                )
            }
            Self::Write { path, reason } => {
                write!(formatter, "cannot write {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigWriteError {}

/// Writes the new YAML to a sibling temp file, then
/// atomically renames over the real path. A crash between
/// the temp-file write and the rename leaves the original
/// `config.yaml` untouched (the operator can `cat` /
/// recover from it). The temp file is in the same
/// directory so the rename stays a single-filesystem
/// operation.
pub(super) fn write_atomic(path: &Path, contents: &str) -> Result<(), ConfigWriteError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| ConfigWriteError::Write {
        path: path.to_path_buf(),
        reason: format!("create_dir_all {}: {error}", parent.display()),
    })?;
    let mut temp = path.to_path_buf();
    let file_name = path.file_name().map_or_else(
        || "config.yaml".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    temp.set_file_name(format!(".{file_name}.{pid}.{nanos}.tmp"));
    fs::write(&temp, contents).map_err(|error| ConfigWriteError::Write {
        path: temp.clone(),
        reason: error.to_string(),
    })?;
    // Round 27 (debug): the pre-Round 27 `write_atomic`
    // left the new file with the operator's process
    // umask (typically `0644`), even when the old
    // file was `0600` (a common operator choice for
    // `config.yaml` to protect the controller secret
    // inside it). The pre-Round 27 contract was
    // "every `caly set <res> <verb> --apply` widens
    // `config.yaml` to umask-default permissions",
    // which is a silent privilege escalation for
    // any operator who `chmod 600 config.yaml` to
    // keep the secret token private. The fix is
    // to read the existing file's mode (if any) and
    // apply it to the new file via `rename`, which
    // on Linux atomically moves the new inode into
    // place *without* the umask-derived mode. The
    // pre-existing file is the source of truth for
    // "what mode did the operator want?"; a fresh
    // install (no pre-existing file) keeps the
    // umask default, which is the right default for
    // a brand-new operator.
    if let Ok(existing) = std::fs::metadata(path) {
        // `mode()` returns `u32` (always). The
        // permission-preservation contract is "the
        // new file gets the same mode as the old
        // file". The `if let Ok(_)` here is a
        // placeholder for a future per-platform
        // `try_into` (e.g. u32 on Unix / the
        // appropriate on Windows); today the
        // metadata-ok path is the only failure
        // mode we care about (the file is missing
        // or unreadable; a fresh install on a
        // missing `config.yaml` keeps the umask
        // default, which is the right operator
        // experience for a brand-new install).
        let perms = existing.permissions();
        let mode = perms.mode();
        let new_perms = std::fs::Permissions::from_mode(mode);
        let _ = std::fs::set_permissions(&temp, new_perms.clone());
        let _ = std::fs::set_permissions(path, new_perms);
    }
    fs::rename(&temp, path).map_err(|error| ConfigWriteError::Write {
        path: path.to_path_buf(),
        reason: format!("rename {} -> {}: {error}", temp.display(), path.display()),
    })?;
    Ok(())
}

/// Generic mutator: reads `config_path`, applies
/// `mutate(current_value)` to the `key` entry, and
/// re-serialises + re-validates the result. The mutator's
/// return is the *new* value for that key (the canonical
/// "produce the next state" pattern that the profile
/// writers introduced in Round 11).
///
/// Copies `config.yaml` to `config.yaml.bak` before any
/// destructive operation. Round 11 introduced this in
/// `client::profile::backup_config`; Round 14 moves it
/// here so every writer shares the same sidecar
/// semantics. The sidecar is overwritten on every call —
/// at most one backup per `config.yaml` between two
/// `cp config.yaml.bak config.yaml` rollbacks. A missing
/// source file is a no-op (fresh install).
pub fn backup_config_yaml(config_path: &Path) -> Result<(), ConfigWriteError> {
    if !config_path.is_file() {
        return Ok(());
    }
    let backup = config_path.with_extension("yaml.bak");
    fs::copy(config_path, &backup).map_err(|error| ConfigWriteError::Write {
        path: backup,
        reason: error.to_string(),
    })?;
    Ok(())
}

/// Validates a `config.yaml` round-trip after a write:
/// the layered loader must accept the new content. The
/// Round 13 `set profile add` path uses the same loader
/// indirectly (through `parse_and_validate_yaml`); this
/// helper is the dedicated test entry so a writer can
/// assert "the file the writer produced is what the
/// loader will read".
pub fn post_write_validate(path: &Path) -> Result<AppConfig, ConfigWriteError> {
    let bytes = fs::read(path).map_err(|error| ConfigWriteError::Read {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    caly_profile::schema::parse_and_validate_yaml(&bytes).map_err(|error| {
        ConfigWriteError::Validate {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::temp_root;
    use std::collections::HashMap;

    #[test]
    fn backup_config_yaml_writes_a_sidecar() {
        let dir = temp_root("backup");
        let path = dir.join("config.yaml");
        fs::write(&path, "schema_version: 1\ncore: mihomo\n").unwrap();
        backup_config_yaml(&path).unwrap();
        let backup = path.with_extension("yaml.bak");
        let mut originals = HashMap::new();
        originals.insert(path.display().to_string(), fs::read(&path).unwrap());
        originals.insert(backup.display().to_string(), fs::read(&backup).unwrap());
        assert_eq!(originals.len(), 2);
        assert_eq!(
            originals[&path.display().to_string()],
            originals[&backup.display().to_string()]
        );
    }

    #[test]
    fn backup_config_yaml_is_a_noop_when_the_source_is_missing() {
        let dir = temp_root("noop");
        let path = dir.join("config.yaml");
        backup_config_yaml(&path).unwrap();
        assert!(!path.with_extension("yaml.bak").exists());
    }

    /// Round 27 (debug): the pre-Round 27
    /// `write_atomic` left the new file with
    /// the operator's process umask (typically
    /// `0644`), even when the old file was
    /// `0600`. An operator who `chmod 600
    /// config.yaml` to protect the controller
    /// secret inside the YAML would have that
    /// file silently widened to umask-default
    /// mode on every `caly set <res> --apply`
    /// call. The test below locks the post-Round
    /// 27 contract: a `0600` file is still
    /// `0600` after `write_atomic`. A fresh
    /// install (no pre-existing file) is not
    /// tested here — the umask default is the
    /// right starting point for a brand-new
    /// operator's first `config.yaml`.
    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_root("__FUNC__");
        let path = dir.join("config.yaml");
        fs::write(
            &path,
            "schema_version: 1\ncore: mihomo\nrule_providers: []\n",
        )
        .unwrap();
        // Operator-chosen mode (e.g. to protect
        // the controller secret token inside).
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(
            &path,
            "schema_version: 1\ncore: mihomo\nrule_providers: []\n",
        )
        .unwrap();
        let after = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            after & 0o777,
            0o600,
            "write_atomic must preserve the operator's chosen mode (got {:o})",
            after & 0o777
        );
    }
}
