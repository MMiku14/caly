//! Profile validation (`profiles:` entries, merge cycles, source shapes).
//!
//! Split out of `validate.rs` (audit #70 file-length budget); every check
//! here is reached from `super::validate` and turns a
//! parseable-but-unusable configuration into an explicit boot failure.

use super::{AppConfig, ConfigError, MergeWalkState};

/// Validates every `profiles:` entry. Ids must be unique and
/// path-safe; `Remote` URLs must be public http(s); merge parts
/// must reference declared profiles and contain no cycles.
pub(super) fn validate_profiles(config: &AppConfig) -> Result<(), ConfigError> {
    use caly_domain::MAX_PROFILES;
    if config.profiles.len() > MAX_PROFILES {
        return Err(ConfigError::InvalidRule {
            index: config.profiles.len(),
            reason: format!("more than {MAX_PROFILES} profiles"),
        });
    }
    let declared: std::collections::HashSet<String> = validate_profile_entries(config)?;
    validate_profile_merge_cycles(config, &declared)?;
    Ok(())
}

/// Per-entry structural checks (unique ids, path-safety, valid
/// `Local` / `Remote` / `Merge` shapes). Returns the set of
/// declared ids so [`validate_profile_merge_cycles`] can re-use
/// it without re-walking the list.
pub(super) fn validate_profile_entries(
    config: &AppConfig,
) -> Result<std::collections::HashSet<String>, ConfigError> {
    use caly_domain::is_path_safe_component;
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for profile in &config.profiles {
        if !is_path_safe_component(&profile.id) {
            return Err(ConfigError::Profile(caly_domain::ProfileError::UnknownId {
                id: profile.id.clone(),
            }));
        }
        if !seen_ids.insert(profile.id.clone()) {
            return Err(ConfigError::Profile(
                caly_domain::ProfileError::DuplicateId {
                    id: profile.id.clone(),
                },
            ));
        }
        declared.insert(profile.id.clone());
        validate_profile_source(&profile.id, &profile.source)?;
    }
    Ok(declared)
}

/// Validates one `Profile.source` against its kind-specific
/// invariants. `Local` rejects empty paths, `..` segments, and
/// absolute paths; `Remote` rejects non-http(s) URLs and a
/// zero `interval_minutes`; `Merge` rejects non-path-safe parts.
pub(super) fn validate_profile_source(
    id: &str,
    source: &super::super::ProfileSourceConfig,
) -> Result<(), ConfigError> {
    use super::super::ProfileSourceConfig;
    use caly_domain::{is_path_safe_component, ProfileError};
    match source {
        ProfileSourceConfig::Local { path } => {
            if path.trim().is_empty() {
                return Err(ConfigError::InvalidRule {
                    index: 0,
                    reason: format!("profile `{id}` local path must not be empty"),
                });
            }
            // The local path is resolved relative to `<config>/profiles/`
            // at load time, so reject anything that could escape that
            // root. Component-level inspection is the precise check:
            // `..` *segments* and absolute roots are traversal, while a
            // filename that merely contains dots (`a..b.yaml`) is legal
            // content the old substring check used to kill incorrectly.
            let escapes = std::path::Path::new(path).components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
            if escapes {
                return Err(ConfigError::Profile(ProfileError::LocalPathEscape {
                    id: id.to_owned(),
                    path: path.clone(),
                }));
            }
            Ok(())
        }
        ProfileSourceConfig::Remote {
            url,
            interval_minutes,
        } => {
            let parsed = url::Url::parse(url).map_err(|_| ConfigError::InvalidRule {
                index: 0,
                reason: format!("profile `{id}` url `{url}` is not parseable"),
            })?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(ConfigError::InvalidRule {
                    index: 0,
                    reason: format!("profile `{id}` url `{url}` must be a public http(s) URL"),
                });
            }
            if *interval_minutes == 0 {
                return Err(ConfigError::Profile(ProfileError::InvalidInterval {
                    id: id.to_owned(),
                }));
            }
            Ok(())
        }
        ProfileSourceConfig::Merge { parts } => {
            for part in parts {
                if !is_path_safe_component(part) {
                    return Err(ConfigError::Profile(ProfileError::UnknownId {
                        id: part.clone(),
                    }));
                }
            }
            Ok(())
        }
    }
}

/// Depth-first cycle detection across declared `Merge`
/// profiles. The first cycle encountered is reported with the
/// offending id; the rest of the declared set is left
/// untouched (the schema validator never aborts on the first
/// error when subsequent checks can still report a distinct
/// failure).
pub(super) fn validate_profile_merge_cycles(
    config: &AppConfig,
    declared: &std::collections::HashSet<String>,
) -> Result<(), ConfigError> {
    use super::super::ProfileSourceConfig;
    let parts_for = |id: &str| -> Option<Vec<String>> {
        config
            .profiles
            .iter()
            .find(|candidate| candidate.id == id)
            .and_then(|candidate| match &candidate.source {
                ProfileSourceConfig::Merge { parts } => Some(parts.clone()),
                _ => None,
            })
    };
    let mut state = MergeWalkState::default();
    for profile in &config.profiles {
        if matches!(profile.source, ProfileSourceConfig::Merge { .. }) {
            walk_merge(&profile.id, &parts_for, declared, &mut state)?;
        }
    }
    Ok(())
}

/// Two-colour DFS bookkeeping shared across merge walks: `in_stack`
/// holds the ids on the current recursion path (a revisit *here* is a
/// genuine cycle), `done` holds fully explored ids (safe to skip, and
/// — crucially — revisiting one through a diamond-shaped merge is *not*
/// a cycle). Defined once in the parent module; the proxy-group walk
/// reuses the same state shape.

fn walk_merge(
    id: &str,
    parts_for: &dyn Fn(&str) -> Option<Vec<String>>,
    declared: &std::collections::HashSet<String>,
    state: &mut MergeWalkState,
) -> Result<(), caly_domain::ProfileError> {
    if state.done.contains(id) {
        return Ok(());
    }
    if !state.in_stack.insert(id.to_owned()) {
        return Err(caly_domain::ProfileError::Cycle { id: id.to_owned() });
    }
    if let Some(parts) = parts_for(id) {
        for part in parts {
            if !declared.contains(&part) {
                return Err(caly_domain::ProfileError::UnknownId { id: part });
            }
            walk_merge(&part, parts_for, declared, state)?;
        }
    }
    // Pop the node from the recursion path before marking it done: a
    // plain "visited once anywhere" set would flag the perfectly legal
    // diamond `merged = merge:[base, extra]`, `base/extra -> shared`.
    state.in_stack.remove(id);
    state.done.insert(id.to_owned());
    Ok(())
}
