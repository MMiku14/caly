//! W1 (cli-v3-design.md §10, D10): the profile context record.
//!
//! `caly profile use <id>` persists the operator's current working
//! profile to `<state>/context.json` (`~/.local/state/caly/` via
//! `AppPaths::state`). The record is pure client-side state: the
//! daemon never reads it, and a missing or corrupt file silently
//! falls back to "no context" (never a panic). Writes are
//! tmp+rename atomic (R-W6). W2 adds the consumers (the
//! bare-command defaults for `node` / `sub refresh`).

use std::path::PathBuf;
use std::process::ExitCode;

use caly_platform::paths::{AppPaths, SafeName};

/// The on-disk location of the context record.
pub fn context_path(paths: &AppPaths) -> PathBuf {
    paths.state.join("context.json")
}

/// Reads the current context; a missing or corrupt record falls
/// back to `None` (a warning on stderr for the corrupt case, so
/// an operator can tell their state file rotted).
pub fn current(paths: &AppPaths) -> Option<String> {
    let body = std::fs::read_to_string(context_path(paths)).ok()?;
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(value) => value.get("profile")?.as_str().map(str::to_owned),
        Err(error) => {
            eprintln!(
                "warning: ignoring corrupt context record ({}): {error}; \
                 falling back to no context",
                context_path(paths).display(),
            );
            None
        }
    }
}

/// D10 consumer (W2, cli-v3-design.md §10): the profile context as
/// the loader's fallback selector. Precedence: `CALY_PROFILE`
/// (evaluated by the caller) > context.json > no profile layer.
/// A recorded id that no longer resolves to a declared profile
/// (removed since the switch) warns on stderr and falls back to
/// no profile layer instead of failing daemon boot (R6 family:
/// corrupt client state degrades, never hard-fails).
pub fn active_profile() -> Option<SafeName> {
    active_profile_under(&AppPaths::from_env())
}

/// The injectable half of [`active_profile`] for hermetic tests.
pub(crate) fn active_profile_under(paths: &AppPaths) -> Option<SafeName> {
    let id = current(paths)?;
    let name = match SafeName::new(id.clone()) {
        Ok(name) => name,
        Err(_error) => {
            eprintln!(
                "warning: profile context `{id}` is not a safe profile name; \
                 ignoring (pick a valid one with `caly profile use <id>`)"
            );
            return None;
        }
    };
    match crate::client::profile::load_declared_profiles(paths) {
        Ok((config, _store)) => {
            if crate::client::profile::find_declared(&config, &id).is_some() {
                Some(name)
            } else {
                eprintln!(
                    "warning: profile context `{id}` no longer matches a declared \
                     profile; ignoring (run `caly profile list` then `caly profile use <id>`)"
                );
                None
            }
        }
        Err(error) => {
            eprintln!(
                "warning: cannot verify the profile context `{id}` ({error}); \
                 ignoring it for this boot"
            );
            None
        }
    }
}

/// `caly profile use <id>`: verify the profile is declared, then
/// persist the switch atomically. Always writes (like the Round-12
/// refresh exception: the record IS the state, so there is no
/// dry-run form).
pub fn use_profile(id: &str, json: bool) -> ExitCode {
    let paths = crate::client::profile::resolve_paths();
    let config = match crate::client::profile::load_declared_profiles(&paths) {
        Ok((config, _store)) => config,
        Err(error) => {
            eprintln!("error: profile use failed: {error}");
            return ExitCode::from(1);
        }
    };
    if crate::client::profile::find_declared(&config, id).is_none() {
        let available: Vec<String> = crate::client::profile::list_declared(&config)
            .iter()
            .map(|profile| profile.id.clone())
            .collect();
        eprintln!(
            "error: profile `{id}` not found.\navailable profiles: {}\n\
             run `caly profile list` to see all.",
            available.join(", "),
        );
        return ExitCode::from(1);
    }
    let previous = current(&paths);
    if let Err(error) = std::fs::create_dir_all(&paths.state) {
        eprintln!(
            "error: cannot create state dir {}: {error}",
            paths.state.display()
        );
        return ExitCode::from(1);
    }
    let record = serde_json::json!({ "profile": id });
    let target = context_path(&paths);
    let tmp = target.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&tmp, record.to_string()) {
        eprintln!("error: cannot write {}: {error}", tmp.display());
        return ExitCode::from(1);
    }
    if let Err(error) = std::fs::rename(&tmp, &target) {
        eprintln!("error: cannot activate {}: {error}", target.display());
        return ExitCode::from(1);
    }
    if json {
        let payload = serde_json::json!({
            "context": { "profile": id, "previous": previous },
        });
        println!("{payload}");
    } else {
        match previous {
            Some(previous) => println!("context: {id} (was: {previous})"),
            None => println!("context: {id}"),
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{hermetic_paths, temp_root};

    fn write_base_config(paths: &AppPaths, body: &str) {
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config.join("config.yaml"), body).unwrap();
    }

    fn write_context(paths: &AppPaths, id: &str) {
        std::fs::create_dir_all(&paths.state).unwrap();
        std::fs::write(
            context_path(paths),
            serde_json::json!({ "profile": id }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn active_profile_is_none_without_a_context() {
        let dir = temp_root("ctx-none");
        let paths = hermetic_paths(dir.as_path());
        write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
        assert!(active_profile_under(&paths).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_profile_resolves_a_declared_context() {
        let dir = temp_root("ctx-ok");
        let paths = hermetic_paths(dir.as_path());
        write_base_config(
            &paths,
            "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: team\n    kind: local\n    path: a.yaml\n",
        );
        write_context(&paths, "team");
        let selected = active_profile_under(&paths);
        assert_eq!(
            selected.map(|name| name.as_str().to_owned()),
            Some("team".to_owned())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_profile_ignores_a_stale_context() {
        // D10/R6: a context left behind by a since-removed profile
        // degrades to "no profile layer" with a stderr warning —
        // daemon boot must not fail on stale client state.
        let dir = temp_root("ctx-stale");
        let paths = hermetic_paths(dir.as_path());
        write_base_config(
            &paths,
            "schema_version: 1\ncore: mihomo\nprofiles:\n  - id: team\n    kind: local\n    path: a.yaml\n",
        );
        write_context(&paths, "ghost");
        assert!(active_profile_under(&paths).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_profile_ignores_a_corrupt_context() {
        let dir = temp_root("ctx-corrupt");
        let paths = hermetic_paths(dir.as_path());
        write_base_config(&paths, "schema_version: 1\ncore: mihomo\n");
        std::fs::create_dir_all(&paths.state).unwrap();
        std::fs::write(context_path(&paths), "not json {").unwrap();
        assert!(active_profile_under(&paths).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
