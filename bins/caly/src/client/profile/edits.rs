//! Mutating commands for the `caly profile` subcommand.
//!
//! Split out of `client/profile.rs` (audit #70 file-length
//! budget): every writer here goes through the single canonical
//! read → parse → mutate → backup → atomic-write pipeline in
//! [`mutate_profiles_in_config`], which delegates the textual
//! edit to `client/yaml_surgery` so comments and unknown keys
//! elsewhere in `config.yaml` survive.

use std::{fs, path::Path};

use caly_domain::is_path_safe_component;
use caly_platform::paths::AppPaths;
use caly_profile::{
    profile_store::ProfileStoreError,
    schema::{ProfileConfig, ProfileSourceConfig},
};

use super::super::resource_writer::current_unix_ms;
use super::{
    ProfileCmdError, ProfileSourceKind, ProfileWriteOutcome, find_declared, load_declared_profiles,
};

/// Appends a new `ProfileConfig` to the operator's
/// `<config>/config.yaml`. The base config is parsed, validated,
/// and re-emitted with the new entry; the layout is preserved
/// (the existing top-level order is kept). `Local` source kinds
/// must already point at a file the operator owns; this function
/// does not materialise the body.
///
/// `dry_run` builds the new `AppConfig` and runs every check
/// (uniqueness, path safety, validation) but does **not**
/// write to disk. The caller surfaces the result so the CLI
/// can print "would have added profile `x`" without touching
/// the operator's file.
pub fn add_profile(
    paths: &AppPaths,
    id: &str,
    source: ProfileSourceKind,
    dry_run: bool,
) -> Result<ProfileWriteOutcome, ProfileCmdError> {
    if !is_path_safe_component(id) {
        return Err(ProfileCmdError::InvalidId(id.to_owned()));
    }
    let (config, _store) = load_declared_profiles(paths)?;
    if find_declared(&config, id).is_some() {
        return Err(ProfileCmdError::AlreadyDeclared(id.to_owned()));
    }
    let entry = build_profile_config(id, source);
    if dry_run {
        return Ok(ProfileWriteOutcome::DryRun);
    }
    mutate_profiles_in_config(&paths.config.join("config.yaml"), |current| {
        let mut next = current.to_vec();
        next.push(entry);
        Ok(next)
    })?;
    Ok(ProfileWriteOutcome::Applied)
}

/// Removes a profile from `config.yaml` and clears its cache.
/// When `dry_run` is set, the call validates and prints
/// "would have removed profile `x`" without touching the
/// file.
pub fn remove_profile(
    paths: &AppPaths,
    id: &str,
    dry_run: bool,
) -> Result<ProfileWriteOutcome, ProfileCmdError> {
    if !is_path_safe_component(id) {
        return Err(ProfileCmdError::InvalidId(id.to_owned()));
    }
    let (config, store) = load_declared_profiles(paths)?;
    if find_declared(&config, id).is_none() {
        return Err(ProfileCmdError::NotDeclared(id.to_owned()));
    }
    if dry_run {
        return Ok(ProfileWriteOutcome::DryRun);
    }
    store.remove(id)?;
    let target = id.to_owned();
    mutate_profiles_in_config(&paths.config.join("config.yaml"), |current| {
        Ok(current
            .iter()
            .filter(|entry| entry.id != target)
            .cloned()
            .collect())
    })?;
    Ok(ProfileWriteOutcome::Applied)
}

/// Flips the `enabled` flag on a declared profile. The
/// schema default is `enabled: true` (Round 15 added the
/// field), so `enable` is a no-op for a fresh entry and
/// `disable` is the meaningful state transition.
///
/// Round 30: the `apply: bool` parameter was added so
/// the dry-run path lives in the writer (matching the
/// pre-Round-22 contract that all 5 profile writers
/// accept a dry-run flag). Pre-Round 30 the dispatch
/// inlined the dry-run validation: it duplicated the
/// path-safety check, the load, the existence scan, and
/// the `CliError::new(…)` construction — a 60-line
/// dry-run shape that mirrored the apply path but lived
/// in a separate code path. Folding the dry-run branch
/// into the writer removes the duplication: the
/// writer's single existence scan now drives both the
/// apply and the dry-run outcome, and the dispatch's
/// `set_enabled_dispatch` collapses to a 1-line
/// `run_writer` call.
pub fn set_profile_enabled(
    paths: &AppPaths,
    id: &str,
    enabled: bool,
    apply: bool,
) -> Result<ProfileWriteOutcome, ProfileCmdError> {
    if !is_path_safe_component(id) {
        return Err(ProfileCmdError::InvalidId(id.to_owned()));
    }
    let (config, _store) = load_declared_profiles(paths)?;
    if find_declared(&config, id).is_none() {
        return Err(ProfileCmdError::NotDeclared(id.to_owned()));
    }
    if !apply {
        return Ok(ProfileWriteOutcome::DryRun);
    }
    let target = id.to_owned();
    let outcome = mutate_profiles_in_config(&paths.config.join("config.yaml"), |current| {
        Ok(current
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                if entry.id == target {
                    entry.enabled = enabled;
                }
                entry
            })
            .collect())
    })?;
    Ok(outcome.write_outcome())
}

/// Round 16: spawn `$EDITOR` (or `vi` as a fallback) with
/// the current cached body as the starting content, then
/// write the result back to the cache. Returns
/// `ProfileCmdError::NotDeclared` if the profile has no
/// cached body yet (the operator must `set profile refresh`
/// first).
///
/// `apply: false` is a no-op (the dry-run path); the editor
/// is never spawned and the cache is never touched.
pub fn edit_profile(
    paths: &AppPaths,
    id: &str,
    apply: bool,
) -> Result<ProfileWriteOutcome, ProfileCmdError> {
    if !is_path_safe_component(id) {
        return Err(ProfileCmdError::InvalidId(id.to_owned()));
    }
    let (config, store) = load_declared_profiles(paths)?;
    if find_declared(&config, id).is_none() {
        return Err(ProfileCmdError::NotDeclared(id.to_owned()));
    }
    if !apply {
        return Ok(ProfileWriteOutcome::DryRun);
    }
    let body = store.read(id)?.ok_or_else(|| {
        ProfileCmdError::Store(ProfileStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("profile `{id}` has no cached body; run `set profile refresh` first"),
        )))
    })?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
    let tmp = std::env::temp_dir().join(format!("caly-profile-{id}-{}.yaml", std::process::id()));
    std::fs::write(&tmp, &body).map_err(|e| ProfileCmdError::Store(ProfileStoreError::Io(e)))?;
    let status = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .map_err(|error| {
            ProfileCmdError::Store(ProfileStoreError::Io(std::io::Error::other(format!(
                "cannot spawn editor `{editor}`: {error}"
            ))))
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(ProfileCmdError::Store(ProfileStoreError::Io(
            std::io::Error::other(format!("editor `{editor}` exited with {status}")),
        )));
    }
    let new_body =
        std::fs::read(&tmp).map_err(|e| ProfileCmdError::Store(ProfileStoreError::Io(e)))?;
    let _ = std::fs::remove_file(&tmp);
    let url = store.read_metadata(id)?.map(|m| m.url).unwrap_or_default();
    let now_ms = current_unix_ms();
    store.write(id, &new_body, &url, now_ms)?;
    Ok(ProfileWriteOutcome::Applied)
}

/// Round 16: write the current cached body to `<out>`.
/// Returns `ProfileCmdError::NotDeclared` if the profile
/// has no cached body yet.
///
/// Round 22: returns [`ProfileWriteOutcome`] so the
/// dispatch can route `export` through the same
/// `run_writer` envelope as the other verbs. `export`
/// has no dry-run path (the operation is either the
/// write or an error), so the writer always returns
/// `Applied` on success.
pub fn export_profile(
    paths: &AppPaths,
    id: &str,
    out: &Path,
) -> Result<ProfileWriteOutcome, ProfileCmdError> {
    if !is_path_safe_component(id) {
        return Err(ProfileCmdError::InvalidId(id.to_owned()));
    }
    let (_config, store) = load_declared_profiles(paths)?;
    let body = store.read(id)?.ok_or_else(|| {
        ProfileCmdError::Store(ProfileStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("profile `{id}` has no cached body; run `set profile refresh` first"),
        )))
    })?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ProfileCmdError::Store(ProfileStoreError::Io(e)))?;
    }
    std::fs::write(out, &body).map_err(|e| ProfileCmdError::Store(ProfileStoreError::Io(e)))?;
    Ok(ProfileWriteOutcome::Applied)
}

fn build_profile_config(id: &str, source: ProfileSourceKind) -> ProfileConfig {
    let source_config = match source {
        ProfileSourceKind::Local { path } => ProfileSourceConfig::Local { path },
        ProfileSourceKind::Remote {
            url,
            interval_minutes,
        } => ProfileSourceConfig::Remote {
            url,
            interval_minutes,
        },
        ProfileSourceKind::Merge { parts } => ProfileSourceConfig::Merge { parts },
    };
    ProfileConfig {
        id: id.to_owned(),
        name: None,
        description: None,
        source: source_config,
        enabled: true,
    }
}

/// Mutates the `profiles:` list in `<config>/config.yaml` through
/// a single canonical read → parse → mutate → write pipeline.
///
/// `mutator` is called with the current `Vec<ProfileConfig>` and
/// returns the desired next list (the contract is "produce the
/// new state" rather than "mutate in place" so the function can
/// validate the result with the schema before writing).
///
/// `profiles:` is auto-created as an empty list when missing, so
/// the operator's first `add` works on a fresh install.
///
/// Audit #15/#16: the write is a targeted line edit of the
/// `profiles:` block via [`super::super::yaml_surgery::edit_yaml_list`],
/// so comments / unknown keys / formatting elsewhere in the file
/// survive; an idempotent mutation returns
/// [`super::super::resource_writer::ListEditOutcome::NoChange`] and
/// no backup / write happens at all. The backup + atomic-rename
/// now lives here (previously the `add` / `remove` callers backed
/// up unconditionally and the `set_enabled` caller not at all), so
/// every profile write is atomic and backed up exactly once, only
/// when it changes bytes.
///
/// Round 21 note: the profile writer keeps its own
/// bespoke mutator because [`ProfileCmdError::Backup`]
/// carries the source / destination paths in a
/// `ProfileCmdError` shape the shared
/// [`super::super::resource_writer::ResourceError`] doesn't
/// model.
fn mutate_profiles_in_config<F>(
    config_path: &Path,
    mutator: F,
) -> Result<super::super::resource_writer::ListEditOutcome, ProfileCmdError>
where
    F: FnOnce(&[ProfileConfig]) -> Result<Vec<ProfileConfig>, ProfileCmdError>,
{
    let text = fs::read_to_string(config_path)
        .map_err(|error| ProfileCmdError::ReadConfig(error.to_string()))?;
    let outcome = super::super::yaml_surgery::edit_yaml_list::<ProfileConfig, _, ProfileCmdError>(
        &text,
        &["profiles"],
        ProfileCmdError::ParseConfig,
        mutator,
    )?;
    let super::super::yaml_surgery::EditOutcome::Changed(new_text) = outcome else {
        return Ok(super::super::resource_writer::ListEditOutcome::NoChange);
    };
    backup_config(config_path)?;
    super::super::config_writer::write_atomic(config_path, &new_text)
        .map_err(|error| ProfileCmdError::ReadConfig(error.to_string()))?;
    Ok(super::super::resource_writer::ListEditOutcome::Changed)
}

impl From<super::super::yaml_surgery::SurgeryError> for ProfileCmdError {
    fn from(value: super::super::yaml_surgery::SurgeryError) -> Self {
        match value {
            super::super::yaml_surgery::SurgeryError::Parse(reason)
            | super::super::yaml_surgery::SurgeryError::Value(reason) => Self::ParseConfig(reason),
        }
    }
}

/// Copies `config_path` to a single `config.yaml.bak`
/// sidecar before any destructive operation. Wraps the
/// shared [`super::super::config_writer::backup_config_yaml`] with
/// the profile-specific [`ProfileCmdError::Backup`]
/// variant so the operator can read the source /
/// destination paths off the failure envelope.
///
/// The shared helper returns
/// [`super::super::config_writer::ConfigWriteError::Write`] on
/// `fs::copy` failure, which carries the same path +
/// reason payload; the profile writer surfaces the same
/// shape through its own error so a future
/// `From<ConfigWriteError> for ProfileCmdError` lift can
/// drop this wrapper.
fn backup_config(config_path: &Path) -> Result<(), ProfileCmdError> {
    if !config_path.is_file() {
        // A fresh install: no prior config to back up. The
        // caller is creating a brand-new file, so a backup
        // would be a no-op anyway.
        return Ok(());
    }
    let backup = config_path.with_extension("yaml.bak");
    fs::copy(config_path, &backup).map_err(|error| ProfileCmdError::Backup {
        from: config_path.to_path_buf(),
        to: backup,
        reason: error.to_string(),
    })?;
    Ok(())
}
