//! Profile domain model.
//!
//! A **profile** is a named unit of configuration that the layered loader
//! can apply alongside the base `config.yaml` and the `config.d/`
//! fragments. Profiles are the user-facing primitive that turns
//! configuration into a *resource*: a user can `caly profile add
//! https://example.com/team.yaml` (Remote), copy a file in (Local), or
//! compose several profiles (Merge) without editing the base config.
//!
//! Profiles are **not** validated at construction time beyond the bounded
//! text constraints; the loader is responsible for parsing the body
//! and surfacing a `ProfileError` if the YAML is malformed. The cache
//! layout (`<state>/profiles/<id>.yaml` + `<id>.meta.toml`) is owned by
//! the `ProfileStore` in `caly-profile`, not the domain.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{BoundedText, TextError};

/// Maximum number of profiles per configuration. The same budget covers
/// declared profiles, fetched caches and merge parts combined.
pub const MAX_PROFILES: usize = 64;

/// Maximum length of a profile identifier in bytes (path-safe, ASCII
/// alphanumeric + dash + underscore — see [`ProfileId::new`]).
pub const PROFILE_ID_MAX_BYTES: usize = 64;

/// Maximum length of a profile `name:` field in bytes. The name is a
/// human-facing label; the `id` is the path component.
pub const PROFILE_NAME_MAX_BYTES: usize = 128;

/// Maximum length of a profile `description:` field in bytes.
pub const PROFILE_DESCRIPTION_MAX_BYTES: usize = 1_024;

/// Maximum size of a fetched profile body in bytes (post-decode, UTF-8
/// counted). Matches the 1 MiB budget used by the rule providers and is
/// shared with the layered loader so a profile can never bypass the
/// size cap.
pub const PROFILE_BODY_MAX_BYTES: usize = 1_024 * 1_024;

/// Bounded profile identifier used as the on-disk path component
/// (`<state>/profiles/<id>.yaml`). Path-safe: ASCII alphanumeric, dash
/// and underscore only, 1..=64 bytes.
pub type ProfileId = BoundedText<PROFILE_ID_MAX_BYTES>;

/// Bounded human label.
pub type ProfileName = BoundedText<PROFILE_NAME_MAX_BYTES>;

/// Bounded free-form description.
pub type ProfileDescription = BoundedText<PROFILE_DESCRIPTION_MAX_BYTES>;

/// Bounded profile body (the rendered YAML the loader merges on top of
/// the base config).
pub type ProfileBody = BoundedText<PROFILE_BODY_MAX_BYTES>;

/// Where a profile's body comes from.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileSource {
    /// Body lives in a local file the operator owns. The path is
    /// resolved at fetch time against `<config>/profiles/` (or
    /// absolute when the path starts with `/`).
    Local {
        /// Path component (relative to the config root) or absolute
        /// path on disk. The same path-safety rules as
        /// [`ProfileId`] apply.
        path: BoundedText<PROFILE_ID_MAX_BYTES>,
    },
    /// Body is fetched from a public HTTP(S) URL on a refresh
    /// cadence. caly applies the same SSRF guard as the
    /// subscription fetch path (no loopback/private destinations).
    Remote {
        /// Public HTTP(S) URL.
        url: BoundedText<2_048>,
        /// Polling interval in minutes. Defaults to 60 (one hour) in
        /// the schema. 0 is rejected.
        interval_minutes: u32,
    },
    /// Body is the deep-merge of the named profiles, applied in
    /// declaration order. The referenced ids must exist in the same
    /// configuration. Cycles (A includes B includes A) are rejected
    /// at validation time.
    Merge {
        /// Profile ids in merge order; each must be a declared
        /// `Profile` in the same config.
        parts: Vec<ProfileId>,
    },
}

/// One profile declaration in the layered configuration. The
/// `name` is the user-facing label; the `id` is the path component
/// used by the cache and by `Merge.parts`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Path-safe identifier used as the cache filename and as
    /// `Merge.parts` reference target.
    pub id: ProfileId,
    /// Human-readable label.
    #[serde(default)]
    pub name: Option<ProfileName>,
    /// Optional free-form description.
    #[serde(default)]
    pub description: Option<ProfileDescription>,
    /// Where the body comes from.
    pub source: ProfileSource,
}

impl Profile {
    /// Returns the profile's `name` if set, otherwise the `id`. The
    /// fallback keeps log output readable when a profile declares
    /// only an `id`.
    pub fn display_label(&self) -> &str {
        self.name
            .as_ref()
            .map_or_else(|| self.id.as_str(), BoundedText::as_str)
    }
}

/// Failures that the layered loader can attribute to a specific
/// profile. Errors carry the id and a bounded message so the
/// `LayeredConfigError::Profile` variant stays within the existing
/// bounded-text budget of the config layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileError {
    /// The profile id is referenced (in `Merge.parts`, in a
    /// `RULE-SET,<name>,…` rule, etc.) but no `Profile` with that
    /// id is declared in the configuration.
    UnknownId { id: String },
    /// Two `Profile` declarations share the same `id`. Ids must be
    /// unique within one configuration.
    DuplicateId { id: String },
    /// `Merge.parts` includes the profile itself (direct or
    /// transitive cycle). Detected by depth-first walk.
    Cycle { id: String },
    /// A `Merge.parts` entry cannot be resolved because the
    /// referenced profile is a `Remote` whose body has not been
    /// refreshed yet (cache missing). This is a soft error: a
    /// subsequent `caly profile refresh` clears it.
    BodyUnavailable { id: String },
    /// The merged body failed to parse as YAML. The
    /// `LayeredConfigError::Yaml` variant is preferred when the
    /// message is already bounded; this variant is the fallback for
    /// per-profile attribution.
    BodyInvalid {
        id: String,
        reason: BoundedText<512>,
    },
    /// `Remote.interval_minutes` is zero. The schema rejects this at
    /// parse time; this variant exists for loader-level guards
    /// (e.g. when a `Profile` is built in code).
    InvalidInterval { id: String },
    /// `Local.path` resolves outside the configured root (path
    /// traversal).
    LocalPathEscape { id: String, path: String },
    /// `Local.path` does not exist on disk.
    LocalPathMissing { id: String, path: String },
    /// The fetched body exceeds `PROFILE_BODY_MAX_BYTES`.
    BodyTooLarge {
        id: String,
        limit: usize,
        actual: usize,
    },
}

impl fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownId { id } => {
                write!(formatter, "profile `{id}` is referenced but not declared")
            }
            Self::DuplicateId { id } => {
                write!(formatter, "profile `{id}` is declared more than once")
            }
            Self::Cycle { id } => {
                write!(formatter, "profile `{id}` has a merge cycle (A → A)")
            }
            Self::BodyUnavailable { id } => {
                write!(
                    formatter,
                    "profile `{id}` body is not yet cached; run `caly profile refresh`"
                )
            }
            Self::BodyInvalid { id, reason } => {
                write!(formatter, "profile `{id}` body is invalid: {reason}")
            }
            Self::InvalidInterval { id } => {
                write!(
                    formatter,
                    "profile `{id}` has a non-positive `interval_minutes`"
                )
            }
            Self::LocalPathEscape { id, path } => {
                write!(
                    formatter,
                    "profile `{id}` local path `{path}` resolves outside the config root"
                )
            }
            Self::LocalPathMissing { id, path } => {
                write!(formatter, "profile `{id}` local path `{path}` is missing")
            }
            Self::BodyTooLarge { id, limit, actual } => {
                write!(
                    formatter,
                    "profile `{id}` body is {actual} bytes, exceeds the {limit}-byte limit"
                )
            }
        }
    }
}

impl std::error::Error for ProfileError {}

/// Validates that the `text` is a path-safe single component: 1..=64
/// ASCII alphanumeric, dash or underscore. Reused by the
/// `ProfileId` constructor and by the loader's `Local` path check.
pub fn is_path_safe_component(text: &str) -> bool {
    if text.is_empty() || text.len() > PROFILE_ID_MAX_BYTES {
        return false;
    }
    text.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Ensures a `ProfileId` is path-safe; the `BoundedText<64>::new`
/// constructor only checks length, not character class.
pub fn validate_id(id: &ProfileId) -> Result<(), TextError> {
    if is_path_safe_component(id.as_str()) {
        Ok(())
    } else {
        Err(TextError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_safe_accepts_alphanumeric_dash_underscore() {
        assert!(is_path_safe_component("team-shared"));
        assert!(is_path_safe_component("Profile_01"));
        assert!(is_path_safe_component("a"));
        assert!(!is_path_safe_component("../etc"));
        assert!(!is_path_safe_component("a/b"));
        assert!(!is_path_safe_component(""));
        assert!(!is_path_safe_component("a b"));
        assert!(!is_path_safe_component("naïve"));
    }

    #[test]
    fn profile_id_accepts_safe_components() {
        let id = ProfileId::new("team-shared").unwrap();
        assert!(validate_id(&id).is_ok());
    }

    #[test]
    fn profile_id_rejects_unsafe_characters() {
        let id = ProfileId::new("a/b").unwrap();
        assert_eq!(validate_id(&id), Err(TextError::Invalid));
    }

    #[test]
    fn source_tagged_enum_round_trips() {
        let local: ProfileSource =
            serde_norway::from_str("kind: local\npath: extra.yaml\n").unwrap();
        assert!(matches!(local, ProfileSource::Local { .. }));
        let remote: ProfileSource = serde_norway::from_str(
            "kind: remote\nurl: https://example.com/team.yaml\ninterval_minutes: 60\n",
        )
        .unwrap();
        assert!(matches!(remote, ProfileSource::Remote { .. }));
        let merge: ProfileSource = serde_norway::from_str("kind: merge\nparts: [a, b]\n").unwrap();
        assert!(matches!(merge, ProfileSource::Merge { .. }));
    }

    #[test]
    fn source_rejects_unknown_field() {
        let bad = "kind: local\npath: extra.yaml\ninterval_minutes: 60\n";
        let result: Result<ProfileSource, _> = serde_norway::from_str(bad);
        assert!(result.is_err(), "Local variant must reject extra fields");
    }

    #[test]
    fn display_label_falls_back_to_id() {
        let profile = Profile {
            id: ProfileId::new("team").unwrap(),
            name: None,
            description: None,
            source: ProfileSource::Merge { parts: Vec::new() },
        };
        assert_eq!(profile.display_label(), "team");
    }

    #[test]
    fn profile_error_display_includes_profile_id() {
        let error = ProfileError::UnknownId {
            id: "ghost".to_owned(),
        };
        let rendered = format!("{error}");
        assert!(rendered.contains("ghost"));
        let cycle = ProfileError::Cycle { id: "x".to_owned() };
        assert!(format!("{cycle}").contains('x'));
    }

    #[test]
    fn max_profiles_constant_matches_docstring() {
        // 64 mirrors the documented budget in `crates/caly-domain/src/profile.rs`.
        // Locking the value here keeps accidental refactors in one place
        // visible to the docstring review.
        assert_eq!(MAX_PROFILES, 64);
    }
}
