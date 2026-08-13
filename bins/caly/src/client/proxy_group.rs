//! Offline proxy-group writer for the `set proxy-group …` leaves.
//!
//! The 5 CRUD leaves (`add` / `remove` / `enable` / `disable` /
//! `list`) round-trip `proxy_groups: Vec<ProxyGroupConfig>` in
//! `config.yaml` through the shared
//! [`crate::client::resource_writer`] helpers. The per-resource
//! code stays here: the resource-specific error variants
//! (`MissingUrl` / `UnexpectedUrl`) and the `PgTypeSpec` /
//! `PgMemberSpec` shims that bridge the CLI grammar to the
//! schema layer.
//!
//! # Error model
//!
//! Every writer returns `Result<_, PgWriteError>`. The CLI
//! dispatch in `commands::set::proxy_group::run_writer` maps
//! each variant to a stable `code:` in the JSON envelope:
//!
//! | variant           | code                          |
//! |-------------------|-------------------------------|
//! | `InvalidName`     | `proxy_group.invalid_name`    |
//! | `AlreadyDeclared` | `proxy_group.already_declared`|
//! | `NotDeclared`     | `proxy_group.not_declared`    |
//! | `MissingUrl`      | `proxy_group.missing_url`     |
//! | `UnexpectedUrl`   | `proxy_group.unexpected_url`  |
//! | `Write`           | `proxy_group.write_failed`    |
//! | `Parse`           | `proxy_group.parse_failed`    |
//! | `Validate`        | `proxy_group.validate_failed` |

use caly_platform::paths::AppPaths;
use caly_profile::schema::{
    ProxyGroupConfig, ProxyGroupMemberConfig, ProxyGroupTypeConfig, UrlTestConfigConfig,
};

use super::resource_writer::{
    ResourceError, ResourceWriteOutcome, is_http_url, is_path_safe_name, mutate_list,
};

/// Proxy-group-specific error variants. The shared
/// [`ResourceError`] cases flow through the `Shared(_)` arm so
/// the dispatch has one `match` instead of two parallel enums.
#[derive(Debug)]
pub enum PgWriteError {
    /// The id is empty or not path-safe ASCII.
    InvalidName(String),
    /// `add` on a name that's already in `proxy_groups:`.
    AlreadyDeclared(String),
    /// `remove` / `enable` / `disable` on a name that's not in
    /// `proxy_groups:`.
    NotDeclared(String),
    /// A `url-test` / `fallback` / `load-balance` group was
    /// added without a `--url` flag. The schema validator
    /// rejects the same case at parse time; the writer
    /// surfaces it earlier with a precise reason.
    MissingUrl(String),
    /// A `select` / `relay` group was added with a `--url`
    /// flag. The schema validator rejects the same case; the
    /// writer surfaces it earlier.
    UnexpectedUrl(String),
    /// Any other failure the shared writer surfaces (read /
    /// parse / validate / IO). Carries the human-readable
    /// reason; the dispatch prefixes the family code.
    Shared(ResourceError),
}

impl core::fmt::Display for PgWriteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidName(name) => {
                write!(
                    formatter,
                    "proxy group name `{name}` is not path-safe ASCII"
                )
            }
            Self::AlreadyDeclared(name) => {
                write!(formatter, "proxy group `{name}` is already declared")
            }
            Self::NotDeclared(name) => {
                write!(formatter, "proxy group `{name}` is not declared")
            }
            Self::MissingUrl(name) => write!(
                formatter,
                "proxy group `{name}` requires --url (url-test / fallback / load-balance)"
            ),
            Self::UnexpectedUrl(name) => write!(
                formatter,
                "proxy group `{name}` is a select/relay; --url is ignored (drop the flag)"
            ),
            Self::Shared(shared) => write!(formatter, "{shared}"),
        }
    }
}

impl std::error::Error for PgWriteError {}

// W2-β2a: no remediation hint of its own yet — the default
// `None` from the shared seam keeps the §8 envelope shape unchanged.
impl crate::output::ErrorHint for PgWriteError {}

impl From<ResourceError> for PgWriteError {
    fn from(value: ResourceError) -> Self {
        match value {
            ResourceError::AlreadyDeclared(name) => Self::AlreadyDeclared(name),
            ResourceError::NotDeclared(name) => Self::NotDeclared(name),
            other => Self::Shared(other),
        }
    }
}

/// Type-kind spec for the CLI writer. Mirrors
/// `cli::ProxyGroupTypeSpec` but stays a CLI-side
/// enum (the dispatch maps the user-typed `--type`
/// flag to one of these; the writer converts to the
/// schema enum).
///
/// Round 30: the `From<cli::ProxyGroupTypeSpec>` impl
/// is the canonical conversion (the previous
/// `to_writer_type` helper in `commands::proxy_group`
/// is removed in favour of an `.into()` call site).
/// The impl is the identity on the 5-variant enum
/// ordering; a future renamer / reordering would
/// surface as a compile error at the `.into()`
/// call site, not as a silent dispatch drift.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PgTypeSpec {
    Select,
    UrlTest,
    Fallback,
    LoadBalance,
    Relay,
}

impl PgTypeSpec {
    /// Maps to the schema enum. The mapping is a
    /// verbatim remap: both use the kebab-case
    /// `select` / `url-test` / `fallback` /
    /// `load-balance` / `relay` spelling.
    pub const fn to_schema(self) -> ProxyGroupTypeConfig {
        match self {
            Self::Select => ProxyGroupTypeConfig::Select,
            Self::UrlTest => ProxyGroupTypeConfig::UrlTest,
            Self::Fallback => ProxyGroupTypeConfig::Fallback,
            Self::LoadBalance => ProxyGroupTypeConfig::LoadBalance,
            Self::Relay => ProxyGroupTypeConfig::Relay,
        }
    }
}

impl From<crate::cli::ProxyGroupTypeSpec> for PgTypeSpec {
    /// Round 30: the canonical CLI-side →
    /// writer-side conversion. The mapping is the
    /// identity on the 5-variant enum ordering —
    /// the two enums have stayed in lock-step since
    /// Round 20, and the `From` impl makes the
    /// conversion a single `.into()` call site
    /// (the previous `to_writer_type` helper in
    /// `commands::proxy_group` collapsed into a
    /// one-line `group_type.into()` expression).
    fn from(value: crate::cli::ProxyGroupTypeSpec) -> Self {
        match value {
            crate::cli::ProxyGroupTypeSpec::Select => Self::Select,
            crate::cli::ProxyGroupTypeSpec::UrlTest => Self::UrlTest,
            crate::cli::ProxyGroupTypeSpec::Fallback => Self::Fallback,
            crate::cli::ProxyGroupTypeSpec::LoadBalance => Self::LoadBalance,
            crate::cli::ProxyGroupTypeSpec::Relay => Self::Relay,
        }
    }
}

/// Member spec for the CLI writer. Mirrors
/// `cli::ProxyGroupMemberSpec` but erases the CLI
/// grammar; the writer converts to the schema enum.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PgMemberSpec {
    Node { tag: String },
    Group { name: String },
    Direct,
    Reject,
}

impl PgMemberSpec {
    /// Maps to the schema enum. Empty strings are
    /// preserved at the writer level; the schema
    /// layer surfaces a missing `tag:` / `name:` as
    /// a parse error.
    pub fn to_schema(&self) -> ProxyGroupMemberConfig {
        match self {
            Self::Node { tag } => ProxyGroupMemberConfig::Node { tag: tag.clone() },
            Self::Group { name } => ProxyGroupMemberConfig::Group { name: name.clone() },
            Self::Direct => ProxyGroupMemberConfig::Direct,
            Self::Reject => ProxyGroupMemberConfig::Reject,
        }
    }
}

/// What an `add` / `remove` / `enable` / `disable` call did.
/// Re-exported as the dispatch's expected `Outcome` shape.
pub type PgWriteOutcome = ResourceWriteOutcome;

/// The single mutation primitive the writer actually uses.
/// Maps the shared `ResourceError` into the per-resource
/// `PgWriteError` and the per-resource `PgWriteOutcome` so the
/// dispatch in `commands::set::proxy_group::run_writer` stays
/// one match.
fn mutate(
    paths: &AppPaths,
    mutate: impl FnOnce(&[ProxyGroupConfig]) -> Result<Vec<ProxyGroupConfig>, ResourceError>,
) -> Result<PgWriteOutcome, PgWriteError> {
    let outcome = mutate_list::<ProxyGroupConfig, _>(paths, "proxy_groups", mutate)?;
    // `NoChange` (an idempotent enable / disable) bubbles up so the
    // dispatch can print the "already in the target state" summary.
    Ok(outcome.write_outcome())
}

/// `set proxy-group add <name> --type <kind> [--members …] [--url …]`.
///
/// Default: dry-run. `--apply` is the only thing that
/// actually mutates `config.yaml`.
#[allow(clippy::too_many_arguments)]
pub fn add_group(
    paths: &AppPaths,
    name: &str,
    group_type: PgTypeSpec,
    members: &[PgMemberSpec],
    url: Option<&str>,
    interval_seconds: Option<u32>,
    tolerance_ms: Option<u32>,
    apply: bool,
) -> Result<PgWriteOutcome, PgWriteError> {
    let entry = build_config(
        name,
        group_type,
        members,
        url,
        interval_seconds,
        tolerance_ms,
    )?;
    if !apply {
        return Ok(PgWriteOutcome::DryRun);
    }
    mutate(paths, |current| {
        if current.iter().any(|g| g.name == entry.name) {
            return Err(ResourceError::AlreadyDeclared(entry.name));
        }
        let mut next = current.to_vec();
        next.push(entry);
        Ok(next)
    })
}

/// `set proxy-group remove` — filters the
/// `proxy_groups:` list by name. Returns
/// `PgWriteError::NotDeclared` if the name is not
/// in the list.
pub fn remove_group(
    paths: &AppPaths,
    name: &str,
    apply: bool,
) -> Result<PgWriteOutcome, PgWriteError> {
    ensure_safe_name(name)?;
    if !group_exists(paths, name)? {
        return Err(PgWriteError::NotDeclared(name.to_owned()));
    }
    if !apply {
        return Ok(PgWriteOutcome::DryRun);
    }
    mutate(paths, |current| {
        let target = name.to_owned();
        Ok(current
            .iter()
            .filter(|g| g.name != target)
            .cloned()
            .collect())
    })
}

/// `set proxy-group enable|disable` — flips the
/// `enabled` flag. The schema default is `enabled:
/// true` (Round 20), so `enable` is a no-op for a
/// fresh entry and `disable` is the meaningful
/// state transition.
pub fn set_enabled(
    paths: &AppPaths,
    name: &str,
    enabled: bool,
    apply: bool,
) -> Result<PgWriteOutcome, PgWriteError> {
    ensure_safe_name(name)?;
    // Round 27 (debug): the dry-run path was a
    // silent no-op for unknown names. Pre-Round 27
    // `set proxy-group enable <unknown-name>`
    // returned `DryRun` (success) for `--dry-run`
    // but `NotDeclared` (error) for `--apply`.
    // The dry-run is now existence-checked the
    // same way `add_group` / `remove_group` are:
    // an unknown name surfaces `NotDeclared`
    // regardless of `apply`. Round 29: the
    // inline `config.proxy_groups.iter().any(...)`
    // check folded into the shared [`group_exists`]
    // helper so the existence predicate lives in
    // one place.
    if !group_exists(paths, name)? {
        return Err(PgWriteError::NotDeclared(name.to_owned()));
    }
    // Round 31: an idempotent `enable` on an
    // already-enabled group (or `disable` on an
    // already-disabled group) is a no-op, NOT a
    // successful `Applied`. The pre-Round 31
    // shape returned `Applied` even when the
    // on-disk `enabled` flag was already the
    // target value, so the operator saw
    // `proxy group enabled` (past tense) after
    // a redundant call. The Round 25 sub
    // writer already surfaces `NoChange` for
    // the analogous case (`set sub enable
    // <already-enabled-url>` → "subscription
    // source already enabled"); the proxy-group
    // writer now matches that contract. The
    // shape mirrors the sub writer's
    // `read_enabled` / `set_source_enabled`
    // short-circuit so the dispatch's
    // `Summaries::no_change` line ("proxy group
    // already enabled" / "proxy group already
    // disabled") is the operator-visible
    // summary, distinguishing a real state
    // change from a no-op confirmation.
    if let Some(already) = read_enabled(paths, name)?
        && already == enabled
    {
        return Ok(PgWriteOutcome::NoChange);
    }
    if !apply {
        return Ok(PgWriteOutcome::DryRun);
    }
    mutate(paths, |current| {
        let target = name.to_owned();
        let next: Vec<ProxyGroupConfig> = current
            .iter()
            .map(|g| {
                let mut g = g.clone();
                if g.name == target {
                    g.enabled = enabled;
                }
                g
            })
            .collect();
        Ok(next)
    })
}

/// Round 31: reads the `enabled` flag of one
/// declared `proxy_group`. Returns `None` if
/// the name is not declared (the same
/// `NotDeclared` condition `set_enabled`
/// surfaces) so the caller can branch on
/// "not declared" vs "declared with `enabled =
/// X`" without a second filesystem read. The
/// pre-Round 31 inline check `if current.any
/// { name }` only answered the "is it
/// declared?" question; the new helper
/// additionally returns the current `enabled`
/// state so the `NoChange` short-circuit can
/// answer "is it already in the target
/// state?" in one place.
fn read_enabled(paths: &AppPaths, name: &str) -> Result<Option<bool>, PgWriteError> {
    let config = super::resource_writer::load_app_config(paths).map_err(PgWriteError::Shared)?;
    Ok(config
        .proxy_groups
        .iter()
        .find(|g| g.name == name)
        .map(|g| g.enabled))
}

/// Round 29: extracted the "is this name in
/// `proxy_groups:`?" check from `remove_group` /
/// `set_enabled` so the two writers share one
/// existence-check call site (pre-Round-29, each
/// writer inlined its own `current.iter().any(...)`
/// / `config.proxy_groups.iter().any(...)` loop,
/// with the `set_enabled` shape going through the
/// full `load_app_config` while `remove_group` did
/// the check inside the `mutate` closure's `&[T]`
/// borrow). The helper returns `bool` so both call
/// sites read as `if !group_exists(paths, name)?` —
/// the existence check is now decoupled from the
/// write primitive.
fn group_exists(paths: &AppPaths, name: &str) -> Result<bool, PgWriteError> {
    let config = super::resource_writer::load_app_config(paths).map_err(PgWriteError::Shared)?;
    Ok(config.proxy_groups.iter().any(|g| g.name == name))
}

/// Reads the declared `proxy_groups:` list from the
/// layered `config.yaml`. Returns one row per
/// declared group; the writer's `list` leaf
/// projects this into the JSON envelope.
/// Loads the layered config once to distinguish "no groups declared"
/// from "config unreadable"; the list face reports the latter instead
/// of silently printing an empty table (2026-08-12 agent audit).
pub fn probe_layered_config(paths: &AppPaths) -> Result<(), String> {
    use caly_profile::loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits};
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    caly_profile::loader::load_layered_yaml_with(
        &layered,
        LoaderLimits::secure_default(),
        &resolver,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub fn read_declared(paths: &AppPaths) -> Vec<(String, bool, String, usize, bool)> {
    use caly_profile::loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits};
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    match caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver) {
        Ok(config) => config
            .proxy_groups
            .into_iter()
            .map(|g| {
                let kind = g.group_type.clash_label().to_owned();
                let member_count = g.members.len();
                let has_url_test = g.url_test.is_some();
                (g.name, g.enabled, kind, member_count, has_url_test)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ── private helpers ─────────────────────────────────────

fn ensure_safe_name(name: &str) -> Result<(), PgWriteError> {
    if is_path_safe_name(name) {
        Ok(())
    } else {
        Err(PgWriteError::InvalidName(name.to_owned()))
    }
}

fn build_config(
    name: &str,
    group_type: PgTypeSpec,
    members: &[PgMemberSpec],
    url: Option<&str>,
    interval_seconds: Option<u32>,
    tolerance_ms: Option<u32>,
) -> Result<ProxyGroupConfig, PgWriteError> {
    ensure_safe_name(name)?;
    let kind = group_type.to_schema();
    // `if let Some(...) = ... { … } else { … }` would
    // duplicate the `kind.needs_url()` / `MissingUrl`
    // / `UnexpectedUrl` early-return shape across the
    // two branches; the `match` keeps the
    // probe-validation logic in one place.
    #[allow(clippy::single_match_else)]
    let url_test = match url {
        Some(raw) => {
            let trimmed = is_http_url(raw)
                .map_err(|reason| PgWriteError::Shared(ResourceError::Invalid(reason)))?;
            if !kind.needs_url() {
                return Err(PgWriteError::UnexpectedUrl(name.to_owned()));
            }
            UrlTestConfigConfig {
                url: trimmed.to_owned(),
                interval_seconds: interval_seconds.unwrap_or(300),
                tolerance_ms: tolerance_ms.unwrap_or(50),
            }
        }
        None => {
            if kind.needs_url() {
                return Err(PgWriteError::MissingUrl(name.to_owned()));
            }
            return Ok(ProxyGroupConfig {
                name: name.to_owned(),
                group_type: kind,
                members: members.iter().map(PgMemberSpec::to_schema).collect(),
                url_test: None,
                enabled: true,
            });
        }
    };
    Ok(ProxyGroupConfig {
        name: name.to_owned(),
        group_type: kind,
        members: members.iter().map(PgMemberSpec::to_schema).collect(),
        url_test: Some(url_test),
        enabled: true,
    })
}

#[cfg(test)]
mod tests;
