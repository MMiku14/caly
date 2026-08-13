//! Layered YAML loading: base, active profile, then lexical fragments.
//!
//! The loader keeps three files in scope, in this order:
//! 1. `config.yaml` (the base, always loaded when present),
//! 2. `profiles/<active>.yaml` (one profile selected via
//!    `CALY_PROFILE`; absent or missing is not an error),
//! 3. every `*.yaml` / `*.yml` under `config.d/`, sorted
//!    lexically by filename.
//!
//! Round 7 adds a fourth layer: the **declared profile set** under the
//! top-level `profiles:` key. Declared profiles are user-named
//! resources (`id` + `name` + `source`); the loader resolves them in
//! declaration order on top of the active profile and the `config.d/`
//! fragments, after the body is materialised through the
//! [`ProfileStore`]. The merge is the same deep-merge the existing
//! layers use; the rules' references are validated by the schema so a
//! profile never needs to know about its peers.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use caly_domain::BoundedText;
use caly_platform::paths::SafeName;
use serde_norway::Value;

use crate::{
    loader::{LoaderBudget, LoaderBudgetError, LoaderLimits},
    schema::{validate, AppConfig, ConfigError},
};

use super::file::{read_bounded_regular_file, ConfigFileError};

/// Resolved layered configuration locations.
pub struct LayeredConfigPaths {
    root: PathBuf,
    active_profile: Option<SafeName>,
}

impl LayeredConfigPaths {
    pub fn new(root: PathBuf, active_profile: Option<SafeName>) -> Self {
        Self {
            root,
            active_profile,
        }
    }

    /// Configuration root (`config.yaml` / `profiles/` / `config.d/` parent).
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn base(&self) -> PathBuf {
        self.root.join("config.yaml")
    }
    fn profile(&self) -> Option<PathBuf> {
        self.active_profile.as_ref().map(|name| {
            self.root
                .join("profiles")
                .join(format!("{}.yaml", name.as_str()))
        })
    }
    fn fragments(&self) -> PathBuf {
        self.root.join("config.d")
    }
}

/// Layered load failure retaining the failing boundary.
#[derive(Debug)]
pub enum LayeredConfigError {
    File(ConfigFileError),
    FragmentDirectory(io::Error),
    FragmentEntry(io::Error),
    TooManyFragments {
        limit: usize,
    },
    Budget(LoaderBudgetError),
    Yaml(BoundedText<512>),
    YamlDetailsUnavailable,
    MergeTypeConflict {
        depth: usize,
    },
    /// A declared `Profile` could not resolve its body (Remote cache
    /// missing, Local path escape, …). The body carries a bounded
    /// reason so the error stays within the existing budget.
    ProfileBody {
        id: String,
        reason: BoundedText<512>,
    },
    /// The merged body of a profile failed to parse as YAML. The
    /// reason mirrors `Yaml` but stays attached to the profile id.
    ProfileYaml {
        id: String,
        reason: BoundedText<512>,
    },
    Config(ConfigError),
}

impl core::fmt::Display for LayeredConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::File(error) => write!(formatter, "config file: {error}"),
            Self::FragmentDirectory(error) => {
                write!(formatter, "fragment directory: {error}")
            }
            Self::FragmentEntry(error) => {
                write!(formatter, "fragment entry: {error}")
            }
            Self::TooManyFragments { limit } => {
                write!(formatter, "too many fragments (limit {limit})")
            }
            Self::Budget(error) => write!(formatter, "loader budget: {error}"),
            Self::Yaml(reason) => write!(formatter, "yaml: {reason}"),
            Self::YamlDetailsUnavailable => {
                write!(formatter, "yaml: <details unavailable>")
            }
            Self::MergeTypeConflict { depth } => {
                write!(formatter, "merge type conflict at depth {depth}")
            }
            Self::ProfileBody { id, reason } => {
                write!(formatter, "profile `{id}` body: {reason}")
            }
            Self::ProfileYaml { id, reason } => {
                write!(formatter, "profile `{id}` yaml: {reason}")
            }
            Self::Config(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for LayeredConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::File(error) => Some(error),
            Self::FragmentDirectory(error) | Self::FragmentEntry(error) => Some(error),
            Self::Budget(error) => Some(error),
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

/// Resolves the body of a declared profile by id. The layered
/// loader is decoupled from the actual fetch / cache logic so the
/// same `load_layered_yaml` can be reused in offline unit tests
/// (returning an in-memory map) and in production (returning the
/// on-disk cache for `Remote` profiles or the operator's local
/// file for `Local` profiles).
pub trait ProfileBodyResolver {
    /// Returns the rendered YAML body for `id`. The id has already
    /// been declared in the configuration; the resolver is the
    /// only owner of where the body lives. The error string is
    /// the user-facing reason (already bounded; the loader
    /// re-wraps it for the size budget).
    fn resolve(&self, id: &str) -> Result<Vec<u8>, String>;
}

/// In-memory resolver used by tests and the offline CLI. Production
/// code uses the `ProfileStore` backed by the on-disk cache.
#[derive(Clone, Debug, Default)]
pub struct InMemoryProfileResolver {
    bodies: std::collections::HashMap<String, Vec<u8>>,
    /// When `true`, the resolver returns an **empty** body for
    /// any id that is not in `bodies` instead of failing. The
    /// empty body is a no-op for the deep-merge, so the loader
    /// can still produce a merged `AppConfig` from the base +
    /// fragments when the operator has declared a profile that
    /// has not been refreshed yet. Production wiring that wants
    /// strict semantics should use [`ProfileStore`], which fails
    /// when the cache is missing.
    lenient: bool,
}

impl InMemoryProfileResolver {
    /// Creates a new empty strict resolver.
    pub fn new() -> Self {
        Self::default()
    }
    /// Creates a new lenient resolver: missing bodies are
    /// returned as empty bytes instead of an error.
    pub fn lenient() -> Self {
        Self {
            bodies: std::collections::HashMap::new(),
            lenient: true,
        }
    }
    /// Inserts (or overwrites) the body for `id`. The body must be
    /// UTF-8 YAML; the loader parses it.
    pub fn insert(&mut self, id: impl Into<String>, body: impl Into<Vec<u8>>) {
        self.bodies.insert(id.into(), body.into());
    }
}

impl ProfileBodyResolver for InMemoryProfileResolver {
    fn resolve(&self, id: &str) -> Result<Vec<u8>, String> {
        if let Some(body) = self.bodies.get(id) {
            return Ok(body.clone());
        }
        if self.lenient {
            // An empty body parses to an empty YAML value, which
            // the deep-merge turns into a no-op. This keeps the
            // loader's contract honest for the operator-facing
            // CLI while preserving the strict path for
            // production boot.
            return Ok(Vec::new());
        }
        Err(format!("profile `{id}` body is not in the resolver"))
    }
}

/// Loads and merges all configured layers without following symlinks.
///
/// `profile_resolver` is consulted for every `Profile` declared in
/// the configuration's `profiles:` segment. The resolver is invoked
/// in declaration order; the result is deep-merged on top of the
/// base, the active profile, and the `config.d/` fragments.
pub fn load_layered_yaml(
    paths: &LayeredConfigPaths,
    limits: LoaderLimits,
) -> Result<AppConfig, LayeredConfigError> {
    load_layered_yaml_with(paths, limits, &InMemoryProfileResolver::new())
}

/// Production layered load: declared `profiles:` bodies are resolved through
/// the on-disk [`crate::profile_store::ProfileStore`] (cache at
/// `<state>/profiles/<id>.yaml`, `<config>/profiles/<id>.yaml` fallback), so
/// a configuration declaring profiles boots the same way the CLI previewed
/// it — instead of erroring on the empty in-memory resolver.
///
/// Missing profile bodies are a *loud* boot failure here (run
/// `caly set profile refresh` to fill the cache); the lenient empty-body
/// behaviour is intentionally limited to the CLI preview path.
pub fn load_layered_yaml_strict(
    paths: &LayeredConfigPaths,
    limits: LoaderLimits,
    state_root: &std::path::Path,
) -> Result<AppConfig, LayeredConfigError> {
    let store = crate::profile_store::ProfileStore::new(
        paths.root().to_path_buf(),
        state_root.to_path_buf(),
    );
    load_layered_yaml_with(paths, limits, &store)
}

/// Same as [`load_layered_yaml`] but routes declared-profile bodies
/// through the supplied resolver. The base / active-profile /
/// fragments load is unchanged; only the `profiles:` segment uses
/// the resolver.
pub fn load_layered_yaml_with(
    paths: &LayeredConfigPaths,
    limits: LoaderLimits,
    profile_resolver: &dyn ProfileBodyResolver,
) -> Result<AppConfig, LayeredConfigError> {
    let mut budget = LoaderBudget::new(limits);
    let mut merged = load_value(&paths.base(), &mut budget, limits)?;
    if let Some(profile) = paths.profile() {
        let value = load_value(&profile, &mut budget, limits)?;
        deep_merge(&mut merged, value, 0, limits.max_merge_depth)?;
    }
    for fragment in fragment_paths(&paths.fragments(), limits.max_fragment_files)? {
        let value = load_value(&fragment, &mut budget, limits)?;
        deep_merge(&mut merged, value, 0, limits.max_merge_depth)?;
    }
    // Apply the declared `profiles:` set. We do this *before* the
    // final `AppConfig` decode so each body contributes a `Profile`
    // entry to the merged YAML, and the existing schema validation
    // catches duplicate ids, cycle references and broken URLs.
    let profile_section = merged
        .get("profiles")
        .cloned()
        .unwrap_or(Value::Sequence(Vec::new()));
    let profile_ids: Vec<String> = match &profile_section {
        Value::Sequence(entries) => entries
            .iter()
            .filter_map(|entry| entry.get("id").and_then(|v| v.as_str()).map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    for id in &profile_ids {
        let body = profile_resolver
            .resolve(id)
            .map_err(|reason| profile_body_error(id, reason))?;
        if body.is_empty() {
            // The lenient resolver returns an empty body for
            // cache-missed profiles; the empty value is a no-op
            // for the deep-merge, so we skip the merge step.
            continue;
        }
        let value =
            parse_profile_body(id, &body).map_err(|reason| LayeredConfigError::ProfileYaml {
                id: id.clone(),
                reason,
            })?;
        deep_merge(&mut merged, value, 0, limits.max_merge_depth)?;
    }
    let config: AppConfig =
        serde_norway::from_value(merged).map_err(|error| yaml_error(error.to_string()))?;
    validate(&config).map_err(LayeredConfigError::Config)?;
    Ok(config)
}

fn parse_profile_body(id: &str, body: &[u8]) -> Result<Value, BoundedText<512>> {
    let source = core::str::from_utf8(body).map_err(|_| {
        BoundedText::<512>::from_nonempty_clamped(
            format!("profile `{id}` body is not UTF-8"),
            "profile body is not UTF-8",
        )
    })?;
    serde_norway::from_str(source).map_err(|error| {
        BoundedText::<512>::from_nonempty_clamped(
            format!("profile `{id}` body parse error: {error}"),
            "profile body parse error",
        )
    })
}

fn profile_body_error(id: &str, reason: String) -> LayeredConfigError {
    LayeredConfigError::ProfileBody {
        id: id.to_owned(),
        reason: BoundedText::<512>::from_nonempty_clamped(reason, "profile body unavailable"),
    }
}

fn load_value(
    path: &Path,
    budget: &mut LoaderBudget,
    limits: LoaderLimits,
) -> Result<Value, LayeredConfigError> {
    let bytes = read_bounded_regular_file(path, limits.max_total_bytes)
        .map_err(LayeredConfigError::File)?;
    budget
        .charge_file(bytes.len())
        .map_err(LayeredConfigError::Budget)?;
    let source = core::str::from_utf8(&bytes)
        .map_err(|_| yaml_error("configuration is not UTF-8".to_owned()))?;
    serde_norway::from_str(source).map_err(|error| yaml_error(error.to_string()))
}

fn fragment_paths(directory: &Path, limit: usize) -> Result<Vec<PathBuf>, LayeredConfigError> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let entries = fs::read_dir(directory).map_err(LayeredConfigError::FragmentDirectory)?;
    for entry in entries {
        let entry = entry.map_err(LayeredConfigError::FragmentEntry)?;
        let path = entry.path();
        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yaml" | "yml")
        ) {
            paths.push(path);
            if paths.len() > limit {
                return Err(LayeredConfigError::TooManyFragments { limit });
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Deep-merges `overlay` onto `base`.
///
/// **Semantics contract** (read before writing layered configs): mappings
/// merge key-by-key, but **sequences are replaced wholesale, never
/// concatenated**. A profile that declares `rules:` therefore *replaces* the
/// base's `rules:` list instead of appending to it; the same applies to
/// `subscriptions.sources`, `profiles`, `proxy_groups`, … This is deliberate
/// (append semantics would give no way to *remove* base entries), but it is
/// a sharp edge: to extend a list, repeat the entries you want to keep.
fn deep_merge(
    base: &mut Value,
    overlay: Value,
    depth: usize,
    max_depth: usize,
) -> Result<(), LayeredConfigError> {
    if depth > max_depth {
        return Err(LayeredConfigError::Budget(LoaderBudgetError::MergeTooDeep));
    }
    match (base, overlay) {
        (Value::Mapping(base), Value::Mapping(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    deep_merge(existing, value, depth + 1, max_depth)?;
                } else {
                    base.insert(key, value);
                }
            }
            Ok(())
        }
        (Value::Mapping(_), _) | (_, Value::Mapping(_)) => {
            Err(LayeredConfigError::MergeTypeConflict { depth })
        }
        (base, overlay) => {
            *base = overlay;
            Ok(())
        }
    }
}

fn yaml_error(message: String) -> LayeredConfigError {
    match BoundedText::new(message) {
        Ok(message) => LayeredConfigError::Yaml(message),
        Err(_) => LayeredConfigError::YamlDetailsUnavailable,
    }
}

#[cfg(test)]
mod tests;
