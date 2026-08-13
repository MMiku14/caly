//! Offline profile resource management for the `caly profile` subcommand.
//!
//! The five subcommands (`add`, `list`, `show`, `refresh`, `remove`)
//! operate on the operator's `<config>/config.yaml` and the
//! `<state>/profiles/` cache. `refresh` is the only one that
//! reaches out to the network: it routes `kind: remote` profiles
//! through the SSRF-safe [`caly_profile::profile_fetch::fetch_profile_body`]
//! pipeline and materialises the body through the on-disk
//! `ProfileStore`. The `Merge` recursive walk is implemented in
//! [`refresh::refresh_recursive`] so a merge's `Remote` dependencies are
//! refreshed before the merge itself is reported as done.
//!
//! Layout (audit #70 file-length budget): this file keeps the
//! command-facing types and the read-only queries; the network
//! refresh walk lives in [`refresh`] and the config-mutating
//! writers live in [`edits`].

use caly_platform::paths::AppPaths;
use caly_profile::{
    loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits},
    profile_store::{ProfileStore, ProfileStoreError},
    schema::{AppConfig, ProfileConfig, ProfileSourceConfig},
};

use super::resource_writer::ResourceWriteOutcome;

mod edits;
mod refresh;

pub use edits::{add_profile, edit_profile, export_profile, remove_profile, set_profile_enabled};
pub use refresh::refresh_all;

/// One declared profile as it appears in `config.yaml`'s
/// `profiles:` list, with the rendered source field expanded
/// into a machine-readable shape.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredProfile {
    /// User-supplied id; path-safe ASCII.
    pub id: String,
    /// Optional human label.
    pub name: Option<String>,
    /// Optional free-form description.
    pub description: Option<String>,
    /// Source kind expanded to a known variant.
    pub source: ProfileSourceKind,
}

/// Source kind for the CLI presentation. Mirrors
/// [`caly_domain::ProfileSource`] but erases the bounded text
/// (the CLI has no concept of "size exceeded" diagnostics; the
/// domain already enforces that at parse time).
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileSourceKind {
    Local { path: String },
    Remote { url: String, interval_minutes: u32 },
    Merge { parts: Vec<String> },
}

impl DeclaredProfile {
    // `display_label` was removed in Round 32: the JSON
    // envelope already carries `id` and `name` for
    // `show profile list` callers, and the operator-facing
    // text path uses `id` directly. The domain
    // `Profile::display_label` stays for the typed
    // rendering path (where the `Profile` struct is
    // available; the CLI never has a `Profile` in hand
    // — it only has a `DeclaredProfile`).
}

/// Failure mode for the offline profile commands.
#[derive(Debug)]
pub enum ProfileCmdError {
    /// The operator-supplied id is not path-safe ASCII.
    InvalidId(String),
    /// The supplied `source` spec is not parseable.
    InvalidSource(String),
    /// The base config could not be read.
    ReadConfig(String),
    /// The base config could not be parsed or validated.
    ParseConfig(String),
    /// The cache write failed.
    Store(ProfileStoreError),
    /// The operator asked to refresh / show / remove an id that is
    /// not declared in the base config.
    NotDeclared(String),
    /// The operator asked to add an id that already exists.
    AlreadyDeclared(String),
    /// The `Remote` body could not be fetched (network or SSRF).
    Fetch(caly_profile::profile_fetch::ProfileFetchError),
    /// `Merge.parts` referenced a `Remote` whose fetch failed; the
    /// outer error wraps the inner failure so the operator sees
    /// both the offender and the cause.
    FetchChain {
        id: String,
        source: caly_profile::profile_fetch::ProfileFetchError,
    },
    /// The pre-write backup of `<config>/config.yaml` failed.
    /// Carries the source and destination paths so the
    /// operator can investigate without re-running with a
    /// debug log.
    Backup {
        from: std::path::PathBuf,
        to: std::path::PathBuf,
        reason: String,
    },
}

impl core::fmt::Display for ProfileCmdError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidId(id) => write!(formatter, "profile id `{id}` is not path-safe ASCII"),
            Self::InvalidSource(spec) => write!(formatter, "source spec `{spec}` is invalid"),
            Self::ReadConfig(reason) => write!(formatter, "read config: {reason}"),
            Self::ParseConfig(reason) => write!(formatter, "parse config: {reason}"),
            Self::Store(error) => write!(formatter, "profile store: {error}"),
            Self::NotDeclared(id) => write!(formatter, "profile `{id}` is not declared"),
            Self::AlreadyDeclared(id) => write!(formatter, "profile `{id}` is already declared"),
            Self::Fetch(error) => write!(formatter, "profile fetch: {error}"),
            Self::FetchChain { id, source } => {
                write!(formatter, "profile `{id}` chain fetch: {source}")
            }
            Self::Backup { from, to, reason } => write!(
                formatter,
                "config backup from {} to {} failed: {reason}",
                from.display(),
                to.display()
            ),
        }
    }
}

impl std::error::Error for ProfileCmdError {}

// W2-β2a: no remediation hint of its own yet — the default
// `None` from the shared seam keeps the §8 envelope shape unchanged.
impl crate::output::ErrorHint for ProfileCmdError {}

impl From<ProfileStoreError> for ProfileCmdError {
    fn from(value: ProfileStoreError) -> Self {
        Self::Store(value)
    }
}

/// Round 22: every `set profile <verb>` writer
/// returns the same [`ProfileWriteOutcome`] so the
/// dispatch in `commands::set::profile` routes every
/// CRUD leaf through the shared
/// `commands::set::common::run_writer` envelope
/// (the same shape `set sub` / `set rule-provider` /
/// `set proxy-group` use). Pre-Round 22 the writers
/// returned two different enums (`AddOutcome` /
/// `RemoveOutcome`) and the dispatch hand-rolled 8
/// near-identical envelope / error matches.
pub type ProfileWriteOutcome = ResourceWriteOutcome;

/// Resolves the operator's XDG config and state roots. Tests
/// inject a custom root by overriding `HOME` / `XDG_*`; the
/// default uses `AppPaths::from_env()`.
pub fn resolve_paths() -> AppPaths {
    AppPaths::from_env()
}

/// Loads the declared profiles from `<config>/config.yaml` and the
/// `config.d/` fragments. The base must be readable; missing
/// `config.d/` is not an error (the empty-fragment path is
/// already exercised by the loader). The resolver is **lenient**:
/// a `Remote` profile whose cache is missing is treated as an
/// empty body, so the operator-facing CLI can list and show
/// declared profiles before the first `refresh`. Production
/// boot uses the strict [`ProfileStore`], which fails loudly
/// when the cache is missing. The returned `AppConfig` reflects
/// the merged state.
pub fn load_declared_profiles(
    paths: &AppPaths,
) -> Result<(AppConfig, ProfileStore), ProfileCmdError> {
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    let config = caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver)
        .map_err(|error| ProfileCmdError::ReadConfig(error.to_string()))?;
    let store = ProfileStore::new(paths.config.clone(), paths.state.clone());
    Ok((config, store))
}

/// Parses a `source` spec argument of the form
/// `remote:<url>`, `local:<path>` or `merge:<id1,id2,…>`.
pub fn parse_source_spec(spec: &str) -> Result<ProfileSourceKind, ProfileCmdError> {
    let (kind, body) = spec.split_once(':').ok_or_else(|| {
        ProfileCmdError::InvalidSource(format!(
            "{spec}: expected `remote:<url>`, `local:<path>` or `merge:<id1,id2,…>`"
        ))
    })?;
    match kind.trim() {
        "remote" => {
            let url = body.trim();
            if url.is_empty() {
                return Err(ProfileCmdError::InvalidSource(spec.to_owned()));
            }
            Ok(ProfileSourceKind::Remote {
                url: url.to_owned(),
                interval_minutes: 60,
            })
        }
        "local" => {
            let path = body.trim();
            if path.is_empty() {
                return Err(ProfileCmdError::InvalidSource(spec.to_owned()));
            }
            Ok(ProfileSourceKind::Local {
                path: path.to_owned(),
            })
        }
        "merge" => {
            let parts: Vec<String> = body
                .split(',')
                .map(|part| part.trim().to_owned())
                .filter(|part| !part.is_empty())
                .collect();
            if parts.is_empty() {
                return Err(ProfileCmdError::InvalidSource(spec.to_owned()));
            }
            Ok(ProfileSourceKind::Merge { parts })
        }
        other => Err(ProfileCmdError::InvalidSource(format!(
            "unknown source kind `{other}`: expected `remote`, `local` or `merge`"
        ))),
    }
}

/// Lists the declared profiles in declaration order.
pub fn list_declared(config: &AppConfig) -> Vec<DeclaredProfile> {
    config.profiles.iter().map(expand_declared).collect()
}

fn expand_declared(profile: &ProfileConfig) -> DeclaredProfile {
    let source = match &profile.source {
        ProfileSourceConfig::Local { path } => ProfileSourceKind::Local { path: path.clone() },
        ProfileSourceConfig::Remote {
            url,
            interval_minutes,
        } => ProfileSourceKind::Remote {
            url: url.clone(),
            interval_minutes: *interval_minutes,
        },
        ProfileSourceConfig::Merge { parts } => ProfileSourceKind::Merge {
            parts: parts.clone(),
        },
    };
    DeclaredProfile {
        id: profile.id.clone(),
        name: profile.name.clone(),
        description: profile.description.clone(),
        source,
    }
}

/// Looks up a single declared profile by id.
pub fn find_declared<'a>(config: &'a AppConfig, id: &str) -> Option<&'a ProfileConfig> {
    config.profiles.iter().find(|profile| profile.id == id)
}

#[cfg(test)]
mod interval_tests;
