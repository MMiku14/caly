//! Shared infrastructure for the `set <resource> …` writers.
//!
//! Every `set` writer in `client::{subscription, profile, rule_provider,
//! proxy_group}` followed the same recipe: validate the id / URL, load
//! the layered config, run a `mutate` closure, and write the file back
//! through a backup → read → parse → mutate → validate → atomic-write
//! loop. The recipe and the failure modes were duplicated four times
//! with subtle drift (the path-safety predicate was different in one
//! writer, the URL validation in another). This module is the
//! single source of truth for that recipe.
//!
//! # What's here
//!
//! - **Path-safety / URL predicates** ([`is_path_safe_name`],
//!   [`is_http_url`]) — the canonical definitions every writer uses.
//! - **Time / clock** ([`current_unix_ms`]) — bounded Unix-ms stamp for
//!   the on-disk cache metadata.
//! - **Loader entry** ([`load_app_config`]) — the "load the layered
//!   config and tolerate a fresh install" helper.
//! - **Unified write error** ([`ResourceError`]) — one error type every
//!   writer maps its domain failures into. The CLI maps each
//!   variant to a stable JSON-envelope `code:`.
//! - **Mutation primitive** ([`mutate_list`]) — the
//!   `mutate_yaml_key` analogue lifted from `config_writer.rs`,
//!   parameterized on the list-element type.
//!
//! # What's not here
//!
//! - **Per-resource semantics** (id composition, URL shape, source
//!   kind) stay in the resource module. The shared module does
//!   the boring cross-resource bookkeeping; the resource module
//!   decides *what* the change means.
//! - **CLI dispatch / human summary formatting** stays in
//!   `commands::set::*`. The writer returns a
//!   [`ResourceWriteOutcome`] and lets the caller shape the
//!   message; the dispatch in `commands::set::run_writer` is
//!   the single point that turns the outcome into a human
//!   summary + a JSON envelope.
//!
//! # See also
//!
//! - [`crate::client::config_writer`] — the lower-level
//!   `mutate_yaml_key` / `backup_config_yaml` helpers. The
//!   `mutate_list` here is a thin wrapper that re-uses them and
//!   adds the resource-error mapping.

use caly_platform::paths::AppPaths;
use caly_profile::{
    loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits},
    schema::AppConfig,
};
use serde::{Serialize, de::DeserializeOwned};

use super::config_writer::{self, ConfigWriteError, backup_config_yaml};

/// The single error type every `set <resource>` writer maps its
/// domain failures into. The CLI dispatch maps each variant to a
/// stable JSON-envelope `code:`.
///
/// # Why one enum
///
/// Before this module each writer had its own `XxxWriteError` with
/// the same four variants (`AlreadyDeclared` /
/// `NotDeclared` / `Write` / `Parse` / `Validate`) plus a
/// resource-specific variant (`InvalidUrl` for subscriptions,
/// `MissingUrl` / `UnexpectedUrl` for proxy-groups, `InvalidSource`
/// for profiles, …). The CLI dispatch in
/// `commands::set::*::run_writer` had the same
/// `match error { … } -> code` block four times. The
/// resource-specific variants are kept on this enum as the
/// `Invalid(_)` umbrella variant so the JSON envelope stays
/// stable while the message stays per-resource.
#[derive(Debug)]
pub enum ResourceError {
    /// `add` on a name that's already declared in the target
    /// list.
    AlreadyDeclared(String),
    /// `remove` / `enable` / `disable` on a name that's not
    /// declared.
    NotDeclared(String),
    /// A resource-specific semantic violation the writer
    /// catches before the loader round-trip (e.g. `proxy_group`
    /// `url-test` without `--url`, `sub` non-HTTP URL, profile
    /// empty source spec). The string is the user-facing
    /// reason; the CLI prefixes it with the resource family.
    Invalid(String),
    /// The on-disk write failed.
    Write(String),
    /// The on-disk parse failed (post-write schema validation
    /// rejects the new content; the writer never reaches disk).
    Parse(String),
    /// Post-mutation schema validation failed.
    Validate(String),
}

impl core::fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AlreadyDeclared(name) => write!(formatter, "`{name}` is already declared"),
            Self::NotDeclared(name) => write!(formatter, "`{name}` is not declared"),
            Self::Invalid(reason) => write!(formatter, "{reason}"),
            Self::Write(reason) | Self::Parse(reason) | Self::Validate(reason) => {
                write!(formatter, "{reason}")
            }
        }
    }
}

impl std::error::Error for ResourceError {}

// W2-β2a: no remediation hint of its own yet — the default
// `None` from the shared seam keeps the §8 envelope shape unchanged.
impl crate::output::ErrorHint for ResourceError {}

impl From<ConfigWriteError> for ResourceError {
    fn from(value: ConfigWriteError) -> Self {
        match value {
            ConfigWriteError::Read { reason, .. } | ConfigWriteError::Write { reason, .. } => {
                Self::Write(reason)
            }
            ConfigWriteError::Validate { reason, .. } => Self::Validate(reason),
        }
    }
}

/// What a writer call did. The dispatch surfaces this in a
/// single human-summary point so every `set` leaf returns the
/// same JSON-envelope shape (`dry_run: bool`, `name: &str`).
///
/// Round 25: a third variant `NoChange` was added to make
/// idempotent operations (e.g. `enable` on an already-enabled
/// source) explicit. Before this round the subscription
/// writer short-circuited with a fabricated `Applied` when
/// the on-disk state was already the target state, so an
/// operator running `caly set sub enable <url> --apply`
/// against an already-enabled URL saw `ok: subscription
/// source enabled` with no actual disk mutation. The new
/// `NoChange` variant lets the dispatch emit a distinct
/// `ok: subscription source already enabled` summary so the
/// operator can tell apart "I wrote a no-op" from "I changed
/// the state". The dispatch's `classify` closure returns
/// [`OutcomeKind::NoChange`](crate::commands::set::common::OutcomeKind)
/// (or `Applied` / `DryRun`) so the summary selection is
/// uniform across writers.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResourceWriteOutcome {
    /// The writer actually mutated `config.yaml` (or would
    /// have in dry-run, where the on-disk state differed from
    /// the target state).
    Applied,
    /// The writer validated the change but did not write
    /// (default dry-run path: the user did not pass
    /// `--apply`).
    DryRun,
    /// The writer observed that the on-disk state was
    /// already the target state, so neither the apply path
    /// nor the dry-run path did any I/O. The dispatch emits
    /// a distinct "already in the target state" summary so
    /// the operator can tell apart the idempotent success
    /// from a genuine write.
    NoChange,
}

/// Canonical path-safety check for any resource name. ASCII
/// alphanumeric, dash, underscore, or dot; non-empty; 1..=64
/// bytes (the same cap as `ProfileId` / `ProxyGroupName`).
///
/// Note: the bounded-length check is the writer's job; the
/// caller is expected to have run `BoundedText::new` on the
/// user-supplied id before reaching the writer. This function
/// enforces the **character class** invariant only.
pub fn is_path_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Rejects empty / non-`http(s)` URLs. Returns the trimmed URL
/// on success so the writer can store the canonical form.
pub fn is_http_url(url: &str) -> Result<&str, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("url is empty".to_owned());
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(format!(
            "url must start with `http://` or `https://` (got `{trimmed}`)"
        ));
    }
    Ok(trimmed)
}

/// Wall-clock milliseconds since the Unix epoch, clamped to
/// `u64::MAX`. Best-effort and degrades to 0 (rather than
/// panicking) on a non-monotonic clock.
///
/// Round 28: dropped the `#[allow(dead_code)]` (the
/// `inline_proxy` writer now uses this instead of its
/// own near-identical 8-line copy) and the old "reserved
/// for the profile / rule-provider writers" comment
/// (those writers don't need a Unix-ms stamp — the
/// typed `AppConfig` round-trip carries its own
/// `added_at_ms` field on the schema, not on the
/// writer). The single source of truth for
/// Unix-ms stamps is here.
pub fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis().min(u128::from(u64::MAX))).unwrap_or(u64::MAX)
        })
}

/// Loads the merged `AppConfig` through the layered loader with
/// a **lenient** profile resolver. The lenient path is what
/// the operator-facing CLI uses: a `Remote` profile whose cache
/// is missing is treated as an empty body, so `list` / `show`
/// work on a fresh install. Production boot uses the strict
/// profile store.
///
/// Used by `proxy_group::*` and `rule_provider::*` writers for
/// the dry-run-existence check in `set_enabled` (Round 27:
/// before Round 27, the dry-run path returned `DryRun` for
/// unknown ids but the apply path returned `NotDeclared` — an
/// inconsistent UX that lied to the operator about the apply
/// outcome; the unified shape now reports `NotDeclared` for
/// both paths).
pub fn load_app_config(paths: &AppPaths) -> Result<AppConfig, ResourceError> {
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    config_writer::post_write_validate(&paths.config.join("config.yaml")).or_else(|_| {
        // Fall back to the lenient loader when the
        // post-write validator fails (e.g. a missing
        // `config.yaml` on a fresh install — the
        // strict path returns `Read`, the lenient
        // path returns an empty `AppConfig`).
        caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver)
            .map_err(|error| ResourceError::Write(format!("load layered config: {error}")))
    })
}

/// Whether a [`mutate_list`] / subscription-mutator call changed the
/// on-disk file. `NoChange` means the typed before/after were
/// identical: no backup, no validation, no write happened (audit #16).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ListEditOutcome {
    /// The file was backed up, validated and atomically rewritten.
    Changed,
    /// Idempotent no-op: the on-disk state already matched.
    NoChange,
}

impl ListEditOutcome {
    /// Map to the user-facing write outcome: a physical change is
    /// `Applied`; an idempotent no-op surfaces as `NoChange` so the
    /// dispatch can tell "I wrote a no-op" apart from "I changed the
    /// state" (the Round 25 contract).
    pub fn write_outcome(self) -> ResourceWriteOutcome {
        match self {
            Self::Changed => ResourceWriteOutcome::Applied,
            Self::NoChange => ResourceWriteOutcome::NoChange,
        }
    }
}

impl From<super::yaml_surgery::SurgeryError> for ResourceError {
    fn from(value: super::yaml_surgery::SurgeryError) -> Self {
        match value {
            super::yaml_surgery::SurgeryError::Parse(reason)
            | super::yaml_surgery::SurgeryError::Value(reason) => Self::Parse(reason),
        }
    }
}

/// The canonical mutation primitive. Reads `<config>/config.yaml`,
/// pulls the `key` list out as `Vec<T>`, runs `mutate` on the
/// current list, re-validates the new content against the
/// schema, and atomic-writes the file back. The mutator's
/// contract is "produce the new state": the writer can
/// `Err(_)` from inside the closure to short-circuit the write
/// (e.g. `AlreadyDeclared`).
///
/// The closure returns [`ResourceError`] directly so the
/// per-resource variants (`AlreadyDeclared` / `NotDeclared` /
/// `Invalid(_)`) round-trip from the closure into the call
/// site's error envelope without collapsing into the generic
/// `Write` variant.
///
/// Audit #15/#16: the write itself is a *targeted line edit* of the
/// `{key}:` block (via [`super::yaml_surgery`]) — the rest of the
/// file's comments, key order and formatting are preserved — and a
/// no-op mutation short-circuits before the backup/write pipeline,
/// so an idempotent call produces no disk write at all.
pub fn mutate_list<T, F>(
    paths: &AppPaths,
    key: &str,
    mutate: F,
) -> Result<ListEditOutcome, ResourceError>
where
    T: DeserializeOwned + Serialize + Clone + PartialEq,
    F: FnOnce(&[T]) -> Result<Vec<T>, ResourceError>,
{
    let path = paths.config.join("config.yaml");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        ResourceError::Write(format!("cannot read {}: {error}", path.display()))
    })?;
    let outcome = super::yaml_surgery::edit_yaml_list::<T, _, ResourceError>(
        &text,
        &[key],
        ResourceError::Parse,
        mutate,
    )?;
    let super::yaml_surgery::EditOutcome::Changed(new_text) = outcome else {
        return Ok(ListEditOutcome::NoChange);
    };
    // Validate the new file *before* writing it: a failed
    // validation never reaches disk. The loader's own
    // `parse_and_validate_yaml` re-runs every schema
    // invariant on the new content.
    if let Err(error) = caly_profile::schema::parse_and_validate_yaml(new_text.as_bytes()) {
        return Err(ResourceError::Validate(error.to_string()));
    }
    backup_config_yaml(&path)?;
    config_writer::write_atomic(&path, &new_text)
        .map_err(|error| ResourceError::Write(error.to_string()))?;
    Ok(ListEditOutcome::Changed)
}

/// Path to the operator's `config.yaml`. One canonical location
/// so a writer doesn't drift between `paths.config.join("config.yaml")`
/// and `paths.config.join("config.yaml.bak")` etc. Used by
/// `subscription::config_yaml_path` (and any future
/// structured-write writers).
pub fn config_yaml_path(paths: &AppPaths) -> std::path::PathBuf {
    paths.config.join("config.yaml")
}

#[cfg(test)]
mod tests;
