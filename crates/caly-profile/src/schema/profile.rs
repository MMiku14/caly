//! Schema for the top-level `profiles:` config segment.
//!
//! The `ProfileConfig` mirrors the domain `Profile` (Remote / Local /
//! Merge) with serde defaults and the bounded-text wrapper conversions
//! the config layer already uses. Validation is in
//! [`crate::schema::validate`]; the conversion is in
//! [`ProfileConfig::to_domain`].

use caly_domain::{
    Profile, ProfileDescription, ProfileId, ProfileName, ProfileSource, validate_id,
};
use serde::{Deserialize, Serialize};

/// One user-declared profile (Remote / Local / Merge). Mirrors
/// [`caly_domain::Profile`]. `deny_unknown_fields` is omitted
/// because the inner source enum is tagged with `kind`; serde
/// consumes that key during flatten and `deny_unknown_fields` on
/// the outer struct would reject the already-consumed field — the
/// inner enum's `deny_unknown_fields` keeps the surface closed.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProfileConfig {
    /// Path-safe identifier (path component).
    pub id: String,
    /// Optional human label.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional free-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Where the body comes from. Tagged: `kind: remote | local | merge`.
    #[serde(flatten)]
    pub source: ProfileSourceConfig,
    /// Round 15: `enabled: false` makes the daemon skip
    /// this profile at boot (the entry stays in `profiles:`
    /// for the operator to re-enable). Mirrors the
    /// `RuleProviderConfig::enabled` shape so the same
    /// `set X enable|disable <id>` policy works for both.
    #[serde(default = "default_profile_enabled")]
    pub enabled: bool,
}

fn default_profile_enabled() -> bool {
    true
}

/// `type:` discriminator for [`ProfileConfig`]. Same shape as the
/// `RuleProviderSourceConfig` pattern: a single `kind:` token picks
/// the variant.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileSourceConfig {
    /// Body lives in a local file the operator owns.
    Local {
        /// Path component (relative to the config root) or absolute path.
        path: String,
    },
    /// Body is fetched from a public HTTP(S) URL on a refresh cadence.
    Remote {
        /// Public HTTP(S) URL.
        url: String,
        /// Polling interval in minutes. Defaults to 60.
        #[serde(default = "default_interval_minutes")]
        interval_minutes: u32,
    },
    /// Body is the deep-merge of the named profiles, applied in
    /// declaration order.
    Merge {
        /// Profile ids in merge order.
        parts: Vec<String>,
    },
}

const fn default_interval_minutes() -> u32 {
    60
}

impl ProfileConfig {
    /// Builds the domain [`Profile`]. Returns an error string when
    /// the id is empty / non-path-safe or a bounded field overflows.
    pub fn to_domain(&self) -> Result<Profile, String> {
        let id = ProfileId::new(self.id.clone())
            .map_err(|error| format!("profile id is invalid: {error}"))?;
        validate_id(&id).map_err(|_| "profile id must be path-safe ASCII".to_owned())?;
        let name = self
            .name
            .as_ref()
            .map(|value| {
                ProfileName::new(value.clone())
                    .map_err(|error| format!("profile name is invalid: {error}"))
            })
            .transpose()?;
        let description = self
            .description
            .as_ref()
            .map(|value| {
                ProfileDescription::new(value.clone())
                    .map_err(|error| format!("profile description is invalid: {error}"))
            })
            .transpose()?;
        let source = match &self.source {
            ProfileSourceConfig::Local { path } => {
                let bounded =
                    caly_domain::BoundedText::<{ caly_domain::PROFILE_ID_MAX_BYTES }>::new(
                        path.clone(),
                    )
                    .map_err(|error| format!("profile local path is invalid: {error}"))?;
                ProfileSource::Local { path: bounded }
            }
            ProfileSourceConfig::Remote {
                url,
                interval_minutes,
            } => {
                let bounded = caly_domain::BoundedText::<2_048>::new(url.clone())
                    .map_err(|error| format!("profile url is invalid: {error}"))?;
                ProfileSource::Remote {
                    url: bounded,
                    interval_minutes: *interval_minutes,
                }
            }
            ProfileSourceConfig::Merge { parts } => {
                let mut out = Vec::with_capacity(parts.len());
                for part in parts {
                    let bounded = ProfileId::new(part.clone())
                        .map_err(|error| format!("merge part `{part}` is invalid: {error}"))?;
                    validate_id(&bounded)
                        .map_err(|_| format!("merge part `{part}` must be path-safe ASCII"))?;
                    out.push(bounded);
                }
                ProfileSource::Merge { parts: out }
            }
        };
        Ok(Profile {
            id,
            name,
            description,
            source,
        })
    }
}
