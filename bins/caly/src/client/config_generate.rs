//! Offline configuration file actions.
//!
//! `generate` creates a documented config only when absent. `default` is an
//! explicit reset: it moves the whole existing config directory (base and
//! fragments) to a timestamped backup directory, then writes a fresh
//! owner-only default layout; a failed write rolls the backup back. Editor
//! actions open the config folder directly, which lets Vim/Neovim use their
//! native directory browser.

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::cli::Editor;
use caly_platform::paths::AppPaths;

pub fn generate_config(json: bool) -> ExitCode {
    generate_config_at(caly_platform::paths::AppPaths::from_env().config, json)
}

/// `generate` against an explicit config root (testable).
pub(crate) fn generate_config_at(root: PathBuf, json: bool) -> ExitCode {
    match generate_at(&root) {
        Ok(()) => report_success(&root.join("config.yaml"), None, json),
        Err(message) => report_error(message, json),
    }
}

/// Writes the default layout into `root`, refusing to touch an existing base
/// config. Fragments are created next to it under `config.d/`.
fn generate_at(root: &Path) -> Result<(), String> {
    let path = root.join("config.yaml");
    if path.exists() {
        return Err(format!(
            "config already exists at {}; use `caly config default` to reset it safely",
            path.display()
        ));
    }
    write_default(root, &path, false)
}

/// Resets the configuration layout, moving the entire existing config
/// directory (base file and fragments) to a timestamped backup directory so
/// nothing user-owned is ever overwritten in place.
pub fn reset_default_config(json: bool) -> ExitCode {
    reset_default_config_at(caly_platform::paths::AppPaths::from_env().config, json)
}

/// `default` against an explicit config root (testable).
pub(crate) fn reset_default_config_at(root: PathBuf, json: bool) -> ExitCode {
    match reset_at(&root) {
        Ok(backup) => report_success(&root.join("config.yaml"), backup.as_deref(), json),
        Err(message) => report_error(message, json),
    }
}

/// Reset core: backs up any existing layout, writes a fresh default, and
/// restores the backup if writing fails. Returns the backup directory if one
/// was created.
fn reset_at(root: &Path) -> Result<Option<PathBuf>, String> {
    // Directory-level backup transaction: the reset replaces both the base
    // file and every fragment, so all existing content moves aside first.
    let backup = backup_existing_root(root)?;
    let path = root.join("config.yaml");
    match write_default(root, &path, true) {
        Ok(()) => Ok(backup),
        Err(message) => {
            // Rollback: a failed reset must restore the previous layout
            // instead of leaving a half-written config directory behind.
            if let Some(backup) = &backup {
                let _ = std::fs::remove_dir_all(root);
                let _ = std::fs::rename(backup, root);
            }
            Err(message)
        }
    }
}

/// Moves a non-empty config root to a timestamped sibling backup directory,
/// then recreates an empty root. Returns the backup path, or `None` when the
/// root was absent or empty (nothing to preserve).
fn backup_existing_root(root: &Path) -> Result<Option<PathBuf>, String> {
    if !root.exists() {
        std::fs::create_dir_all(root)
            .map_err(|error| format!("cannot create config dir {}: {error}", root.display()))?;
        return Ok(None);
    }
    let has_content = std::fs::read_dir(root)
        .map_err(|error| format!("cannot read config dir {}: {error}", root.display()))?
        .next()
        .is_some();
    if !has_content {
        return Ok(None);
    }
    let backup = unique_backup_path(root);
    std::fs::rename(root, &backup).map_err(|error| {
        format!(
            "cannot back up existing config dir {} to {}: {error}",
            root.display(),
            backup.display()
        )
    })?;
    std::fs::create_dir_all(root)
        .map_err(|error| format!("cannot recreate config dir {}: {error}", root.display()))?;
    Ok(Some(backup))
}

/// Opens the configuration directory in Vim or Neovim.
pub fn edit_config(editor: Editor, json: bool) -> ExitCode {
    if json {
        return report_error(
            "--json cannot be used with an interactive editor".to_owned(),
            true,
        );
    }
    let root = caly_platform::paths::AppPaths::from_env().config;
    if let Err(error) = std::fs::create_dir_all(&root) {
        return report_error(
            format!("cannot create config dir {}: {error}", root.display()),
            false,
        );
    }
    let executable = match editor {
        Editor::Vim => "vim",
        Editor::Nvim => "nvim",
    };
    match Command::new(executable).arg(&root).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => report_error(
            format!(
                "{executable} exited with status {status}; config directory: {}",
                root.display()
            ),
            false,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => report_error(
            format!(
                "cannot open the caly configuration directory because `{executable}` is not installed or not on PATH\n\
                 config directory: {}\n\
                 hint: install `{executable}`, or open this directory with another editor",
                root.display()
            ),
            false,
        ),
        Err(error) => report_error(format!("cannot start {executable}: {error}"), false),
    }
}

/// Validates the effective layered configuration without starting a daemon.
pub fn validate_active_config(json: bool) -> ExitCode {
    let root = caly_platform::paths::AppPaths::from_env().config;
    match crate::daemon_config::load_from(root.clone()) {
        Ok(Some(_)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "root": root, "layered": true})
                );
            } else {
                println!(
                    "effective layered configuration is valid: {}",
                    root.display()
                );
            }
            ExitCode::SUCCESS
        }
        Ok(None) => report_error(
            format!(
                "no base config found at {}; run `caly config generate`",
                root.join("config.yaml").display()
            ),
            json,
        ),
        Err(error) => report_error(format!("effective configuration is invalid: {error}"), json),
    }
}

/// Prints the resolved XDG configuration root without requiring a daemon.
pub fn config_path(json: bool) -> ExitCode {
    let root = caly_platform::paths::AppPaths::from_env().config;
    if json {
        println!("{}", serde_json::json!({"ok": true, "path": root}));
    } else {
        println!("{}", root.display());
    }
    ExitCode::SUCCESS
}

/// Lists only regular YAML configuration files below the owned config root.
pub fn config_files(json: bool) -> ExitCode {
    let root = caly_platform::paths::AppPaths::from_env().config;
    let mut files = Vec::new();
    collect_config_files(&root, &root, &mut files);
    files.sort();
    if json {
        println!(
            "{}",
            serde_json::json!({"ok": true, "root": root, "files": files})
        );
    } else if files.is_empty() {
        println!("no config files under {}", root.display());
    } else {
        println!("configuration root: {}", root.display());
        for file in files {
            println!("  {file}");
        }
    }
    ExitCode::SUCCESS
}

fn collect_config_files(root: &Path, directory: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() && path != root {
            collect_config_files(root, &path, files);
        } else if metadata.is_file()
            && matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("yaml" | "yml")
            )
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.push(relative.display().to_string());
        }
    }
}

fn write_default(root: &Path, path: &Path, overwrite_fragments: bool) -> Result<(), String> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root)
        .map_err(|error| format!("cannot create config dir {}: {error}", root.display()))?;
    let fragments = caly_profile::schema::render_default_config_files();
    if !overwrite_fragments {
        for (relative, _) in &fragments {
            let destination = root.join(relative);
            if destination.exists() {
                return Err(format!(
                    "config fragment already exists at {}; refusing to overwrite",
                    destination.display()
                ));
            }
        }
    }
    // Keep the base self-validating. Feature fragments are deep-merged by the
    // existing loader only after this file has been read.
    std::fs::write(path, caly_profile::schema::render_default_base())
        .map_err(|error| format!("cannot write config {}: {error}", path.display()))?;
    for (relative, contents) in fragments {
        let destination = root.join(relative);
        let parent = destination
            .parent()
            .ok_or_else(|| "invalid generated config path".to_owned())?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create config fragment dir {}: {error}",
                parent.display()
            )
        })?;
        std::fs::write(&destination, contents).map_err(|error| {
            format!(
                "cannot write config fragment {}: {error}",
                destination.display()
            )
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                format!(
                    "cannot secure config fragment {}: {error}",
                    destination.display()
                )
            },
        )?;
    }
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure config {}: {error}", path.display()))?;
    // Self-check: the generated layout must load through the same layered
    // loader the daemon uses at boot. A failure here is a renderer regression
    // and must surface loudly instead of bricking the next daemon start.
    crate::daemon_config::load_from(root.to_path_buf()).map_err(|error| {
        format!(
            "generated configuration failed validation: {error}; \
             inspect {} before starting the daemon",
            root.display()
        )
    })?;
    Ok(())
}

fn unique_backup_path(path: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let base = path.as_os_str().to_string_lossy();
    for suffix in 0_u32..=u32::MAX {
        let candidate = if suffix == 0 {
            PathBuf::from(format!("{base}.bak.{millis}"))
        } else {
            PathBuf::from(format!("{base}.bak.{millis}.{suffix}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    // The loop is exhaustive in practice; preserve the original path only as
    // an unreachable defensive fallback rather than selecting a shared name.
    PathBuf::from(format!("{base}.bak.{millis}.{}", std::process::id()))
}

fn report_success(path: &Path, backup: Option<&Path>, json: bool) -> ExitCode {
    if json {
        // Do not hand-escape paths: valid filesystem names may contain quotes,
        // backslashes, or control characters.
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "path": path.display().to_string(),
                "backup": backup.map(|value| value.display().to_string()),
            })
        );
    } else if let Some(backup) = backup {
        println!("reset default config at {}", path.display());
        println!("previous config backed up to {}", backup.display());
    } else {
        println!("wrote base config to {}", path.display());
        println!(
            "wrote feature fragments under {}/config.d/",
            path.parent()
                .map_or_else(|| ".".to_owned(), |value| value.display().to_string())
        );
    }
    ExitCode::SUCCESS
}

fn report_error(message: String, json: bool) -> ExitCode {
    if json {
        eprintln!("{}", serde_json::json!({"ok": false, "error": message}));
    } else {
        eprintln!("{message}");
    }
    ExitCode::FAILURE
}

mod diff;

pub use diff::diff_config;
#[cfg(test)]
use diff::{DiffKind, naive_line_diff, render_structural_diff};

#[cfg(test)]
mod config_generate_tests;

#[cfg(test)]
mod diff_tests;

/// Loads the layered config, bootstrapping a default `config.yaml` when
/// the file is missing (2026-08-12 user动线: `sub add` on a fresh
/// install must not fail with a bare "No such file" — the first write
/// command initializes the layout, like `git init` for a new repo).
/// `validate`-style commands keep their strict behaviour by not using
/// this helper.
pub(crate) fn load_config_with_bootstrap(
    paths: &caly_platform::paths::AppPaths,
    json: bool,
) -> Result<caly_profile::schema::AppConfig, String> {
    let root = paths.config.clone();
    let path = root.join("config.yaml");
    if !path.exists() {
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        write_default(&root, &path, false)?;
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "bootstrapped": true,
                    "path": path.display().to_string(),
                })
            );
        } else {
            eprintln!("initialized a default config at {}", path.display());
        }
    }
    let limits = caly_profile::loader::LoaderLimits::secure_default();
    let layered = caly_profile::loader::LayeredConfigPaths::new(root, None);
    let resolver = caly_profile::loader::InMemoryProfileResolver::lenient();
    caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver)
        .map_err(|error| error.to_string())
}
