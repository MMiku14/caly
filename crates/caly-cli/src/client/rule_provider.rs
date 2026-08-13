//! Round 15: offline rule-provider writer for the
//! `set rule-provider …` leaves.
//!
//! The 6 CRUD leaves (`add-http` / `add-file` / `add-inline` /
//! `remove` / `enable` / `disable`) round-trip
//! `rule_providers: Vec<RuleProviderConfig>` in
//! `config.yaml`. `refresh` is the daemon-RPC
//! path (separate wire command) and stays outside this
//! module.
//!
//! The shared [`crate::client::resource_writer`] helpers
//! own the cross-resource bookkeeping (path safety, URL
//! validation, the typed `mutate_list` primitive, the
//! unified `ResourceError` envelope). The per-resource
//! code here is the **type** discriminator
//! (`RuleProviderSourceConfig` has three source kinds)
//! and the `RpBehavior` shim that bridges the CLI
//! grammar to the schema enum.

use std::path::PathBuf;

use caly_platform::paths::AppPaths;
use caly_profile::schema::{
    RuleProviderBehaviorConfig, RuleProviderConfig, RuleProviderFormatConfig,
    RuleProviderSourceConfig,
};

use super::resource_writer::{
    ResourceError, ResourceWriteOutcome, is_http_url, is_path_safe_name, mutate_list,
};

/// Rule-provider-specific error variants. The shared
/// [`ResourceError`] cases flow through the `Shared(_)` arm
/// so the dispatch has one `match` instead of two parallel
/// enums.
#[allow(dead_code)]
// `InvalidBehavior` is reserved for a
// future `set rule-provider add --behavior`
// flag (Round 16+). Round 15 defaults
// to `Domain` for all new entries.
#[derive(Debug)]
pub enum RpWriteError {
    /// The id is empty or not path-safe ASCII.
    InvalidName(String),
    /// `add` on a name that's already declared.
    AlreadyDeclared(String),
    /// `remove` / `enable` / `disable` on a name that's not
    /// declared.
    NotDeclared(String),
    /// The `behavior:` is not a known matcher kind
    /// (rejected by the schema enum, surfaced for
    /// diagnostics).
    InvalidBehavior(String),
    /// The HTTP URL is empty or not `http(s)://`.
    InvalidUrl(String),
    /// Any other failure the shared writer surfaces.
    Shared(ResourceError),
}

impl core::fmt::Display for RpWriteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidName(name) => write!(
                formatter,
                "rule provider name `{name}` is not path-safe ASCII"
            ),
            Self::AlreadyDeclared(name) => {
                write!(formatter, "rule provider `{name}` is already declared")
            }
            Self::NotDeclared(name) => write!(formatter, "rule provider `{name}` is not declared"),
            Self::InvalidBehavior(reason) => {
                write!(formatter, "invalid rule provider behavior: {reason}")
            }
            Self::InvalidUrl(reason) => write!(formatter, "invalid rule provider URL: {reason}"),
            Self::Shared(shared) => write!(formatter, "{shared}"),
        }
    }
}

impl std::error::Error for RpWriteError {}

// W2-β2a: no remediation hint of its own yet — the default
// `None` from the shared seam keeps the §8 envelope shape unchanged.
impl crate::output::ErrorHint for RpWriteError {}

impl From<ResourceError> for RpWriteError {
    fn from(value: ResourceError) -> Self {
        match value {
            ResourceError::AlreadyDeclared(name) => Self::AlreadyDeclared(name),
            ResourceError::NotDeclared(name) => Self::NotDeclared(name),
            other => Self::Shared(other),
        }
    }
}

/// Source-kind spec for the CLI writer. Mirrors
/// `cli::RuleProviderSourceSpec` but erases the bounded
/// text / the file path. The CLI dispatch converts
/// between the two shapes.
#[derive(Debug, Clone)]
pub enum RpSourceSpec {
    Http { url: String, interval_ms: u64 },
    File { path: PathBuf },
    Inline { payload: String },
}

/// The behavior the rule body contains. Mirrors
/// `RuleProviderBehaviorConfig` but stays a CLI-side enum
/// (the dispatch maps the user-typed `--behavior` flag to
/// one of these; the writer converts to the schema enum).
#[allow(dead_code)]
// `DomainSuffix` / `IpCidr` / `Classical`
// / `parse` are reserved for a future
// `set rule-provider add-* --behavior` flag
// (Round 16+). Round 15 defaults to `Domain`
// for all new entries.
#[derive(Debug, Clone, Copy)]
pub enum RpBehavior {
    Domain,
    DomainSuffix,
    IpCidr,
    Classical,
}

impl RpBehavior {
    /// The default behavior when the operator does not pass
    /// `--behavior` explicitly. Round 15 default: `Domain`
    /// (the most common pattern in mihomo rule-providers).
    pub const DEFAULT: Self = Self::Domain;

    pub const fn to_schema(self) -> RuleProviderBehaviorConfig {
        match self {
            Self::Domain => RuleProviderBehaviorConfig::Domain,
            Self::DomainSuffix => RuleProviderBehaviorConfig::DomainSuffix,
            Self::IpCidr => RuleProviderBehaviorConfig::IpCidr,
            Self::Classical => RuleProviderBehaviorConfig::Classical,
        }
    }
}

/// Re-export of the unified `Outcome` shape. The dispatch
/// sees this as the writer's `Ok(_)` type.
pub type RpWriteOutcome = ResourceWriteOutcome;

fn build_config(
    name: &str,
    source: &RpSourceSpec,
    behavior: RpBehavior,
) -> Result<RuleProviderConfig, RpWriteError> {
    if !is_path_safe_name(name) {
        return Err(RpWriteError::InvalidName(name.to_owned()));
    }
    let kind = match source {
        RpSourceSpec::Http { url, interval_ms } => {
            let url = is_http_url(url).map_err(RpWriteError::InvalidUrl)?;
            RuleProviderSourceConfig::Http {
                url: url.to_owned(),
                interval_ms: *interval_ms,
            }
        }
        RpSourceSpec::File { path } => RuleProviderSourceConfig::File {
            path: path.display().to_string(),
        },
        RpSourceSpec::Inline { payload } => RuleProviderSourceConfig::Inline {
            payload: payload.clone(),
        },
    };
    Ok(RuleProviderConfig {
        name: name.to_owned(),
        kind,
        behavior: behavior.to_schema(),
        format: RuleProviderFormatConfig::Source,
        enabled: true,
    })
}

/// `set rule-provider add-http|add-file|add-inline`.
///
/// Default: dry-run. `--apply` is the only thing that
/// actually mutates `config.yaml`.
pub fn add_provider(
    paths: &AppPaths,
    name: &str,
    source: &RpSourceSpec,
    behavior: RpBehavior,
    apply: bool,
) -> Result<RpWriteOutcome, RpWriteError> {
    let entry = build_config(name, source, behavior)?;
    if !apply {
        return Ok(RpWriteOutcome::DryRun);
    }
    let outcome = mutate_list::<RuleProviderConfig, _>(paths, "rule_providers", |current| {
        if current.iter().any(|p| p.name == entry.name) {
            return Err(ResourceError::AlreadyDeclared(entry.name));
        }
        let mut next = current.to_vec();
        next.push(entry);
        Ok(next)
    })?;
    Ok(outcome.write_outcome())
}

/// `set rule-provider remove` — filters the
/// `rule_providers:` list by name. Returns
/// `RpWriteError::NotDeclared` if the name is not in the
/// list.
pub fn remove_provider(
    paths: &AppPaths,
    name: &str,
    apply: bool,
) -> Result<RpWriteOutcome, RpWriteError> {
    if !is_path_safe_name(name) {
        return Err(RpWriteError::InvalidName(name.to_owned()));
    }
    if !provider_exists(paths, name)? {
        return Err(RpWriteError::NotDeclared(name.to_owned()));
    }
    if !apply {
        return Ok(RpWriteOutcome::DryRun);
    }
    let outcome = mutate_list::<RuleProviderConfig, _>(paths, "rule_providers", |current| {
        let target = name.to_owned();
        Ok(current
            .iter()
            .filter(|p| p.name != target)
            .cloned()
            .collect())
    })?;
    Ok(outcome.write_outcome())
}

/// `set rule-provider enable|disable` — flips the
/// `enabled` flag. The schema default is `enabled: true`
/// (Round 15 added the field), so `enable` is a no-op
/// for a fresh entry and `disable` is the meaningful
/// state transition.
pub fn set_enabled(
    paths: &AppPaths,
    name: &str,
    enabled: bool,
    apply: bool,
) -> Result<RpWriteOutcome, RpWriteError> {
    if !is_path_safe_name(name) {
        return Err(RpWriteError::InvalidName(name.to_owned()));
    }
    // Round 27 (debug): the dry-run path was a
    // silent no-op for unknown names. Pre-Round 27
    // `set rule-provider enable <unknown-name>`
    // returned `DryRun` (success) for `--dry-run`
    // but `NotDeclared` (error) for `--apply`.
    // The dry-run is now existence-checked the
    // same way `add_provider` / `remove_provider`
    // are: an unknown name surfaces `NotDeclared`
    // regardless of `apply`. Round 29: the
    // existence check folded into the shared
    // [`provider_exists`] helper so the predicate
    // lives in one place (the pre-Round-29 shape
    // inlined the same `iter().any(...)` in three
    // different branches).
    if !provider_exists(paths, name)? {
        return Err(RpWriteError::NotDeclared(name.to_owned()));
    }
    if !apply {
        return Ok(RpWriteOutcome::DryRun);
    }
    let outcome = mutate_list::<RuleProviderConfig, _>(paths, "rule_providers", |current| {
        let target = name.to_owned();
        let next: Vec<RuleProviderConfig> = current
            .iter()
            .map(|p| {
                let mut p = p.clone();
                if p.name == target {
                    p.enabled = enabled;
                }
                p
            })
            .collect();
        Ok(next)
    })?;
    Ok(outcome.write_outcome())
}

/// Round 29: extracted the "is this name in
/// `rule_providers:`?" check from `remove_provider`
/// / `set_enabled` so the two writers share one
/// existence-check call site (pre-Round-29, each
/// writer inlined its own `current.iter().any(...)`
/// / `config.rule_providers.iter().any(...)` loop,
/// with `set_enabled` going through the full
/// `load_app_config` while `remove_provider` did
/// the check inside the `mutate` closure's `&[T]`
/// borrow). The helper returns `bool` so both call
/// sites read as `if !provider_exists(paths, name)?` —
/// the existence check is now decoupled from the
/// write primitive.
fn provider_exists(paths: &AppPaths, name: &str) -> Result<bool, RpWriteError> {
    let config = super::resource_writer::load_app_config(paths).map_err(RpWriteError::Shared)?;
    Ok(config.rule_providers.iter().any(|p| p.name == name))
}

#[cfg(test)]
mod tests;
