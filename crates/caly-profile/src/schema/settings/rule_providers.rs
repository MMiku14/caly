//! Rule-provider settings (`rule_providers:` entries and their enums).
//!
//! Split out of `settings.rs` (audit #70 file-length budget).

use caly_domain::{RuleProviderBehavior, RuleProviderFormat, RuleProviderName, RuleText};
use serde::{Deserialize, Serialize};

/// One user-defined rule provider. Mirrors `ProviderConfig` (a named source
/// of proxy content): `RuleProvider` is a named source of rule content,
/// tagged and referenced from `RULE-SET,<name>,…` rules. Three source
/// kinds cover the common shapes; caly never fetches HTTP bodies itself
/// (the core does, on its own polling cadence).
///
/// `deny_unknown_fields` is omitted because the inner source enum is
/// tagged with `type`; serde consumes that key during flatten and
/// `deny_unknown_fields` on the outer struct would reject the
/// already-consumed field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuleProviderConfig {
    /// Tag referenced from `RULE-SET,<name>,…` rules and from the
    /// rendered core's `rule-providers:` / `route.rule_set` block. Must
    /// be unique within one configuration.
    pub name: String,
    /// Where the rule body comes from. Internally tagged: a `type:` field
    /// selects the variant. Exactly one variant is decoded per entry.
    #[serde(flatten)]
    pub kind: RuleProviderSourceConfig,
    /// What kind of matchers the body contains. Maps to Mihomo's
    /// `behavior:` and sing-box's `route_set` `type` discriminator.
    pub behavior: RuleProviderBehaviorConfig,
    /// On-disk / on-wire format. `Binary` is sing-box's compiled `.srs`
    /// and is only valid for `http` / `file` sources.
    #[serde(default = "default_format")]
    pub format: RuleProviderFormatConfig,
    /// Round 15: `enabled: false` makes the daemon skip this
    /// provider at boot (the entry stays in `rule_providers:`
    /// for the operator to re-enable). Mirrors the
    /// `SubscriptionSource::enabled` shape so the same
    /// `set X enable|disable <name>` policy works for both.
    #[serde(default = "default_rule_provider_enabled")]
    pub enabled: bool,
}

fn default_rule_provider_enabled() -> bool {
    true
}

impl RuleProviderConfig {
    /// Builds the domain [`caly_domain::RuleProvider`] used by the
    /// rule renderer. Returns `Err(String)` when one of the required
    /// fields for the chosen source kind is empty (the loader runs
    /// before validation can enforce them).
    pub fn to_rule_provider(&self) -> Result<caly_domain::RuleProvider, String> {
        let name = RuleProviderName::new(self.name.clone())
            .map_err(|error| format!("rule provider name is too long: {error}"))?;
        let source = match &self.kind {
            RuleProviderSourceConfig::Http { url, interval_ms } => {
                let bounded = RuleText::new(url.clone())
                    .map_err(|error| format!("rule provider url is too long: {error}"))?;
                caly_domain::RuleProviderSource::Http {
                    url: bounded,
                    interval_ms: *interval_ms,
                }
            }
            RuleProviderSourceConfig::File { path } => {
                let bounded = RuleText::new(path.clone())
                    .map_err(|error| format!("rule provider path is too long: {error}"))?;
                caly_domain::RuleProviderSource::File { path: bounded }
            }
            RuleProviderSourceConfig::Inline { payload } => {
                let bounded = caly_domain::BoundedText::<
                    { caly_domain::INLINE_RULE_PAYLOAD_MAX_BYTES },
                >::new(payload.clone())
                .map_err(|error| format!("rule provider payload is too long: {error}"))?;
                caly_domain::RuleProviderSource::Inline { payload: bounded }
            }
        };
        let behavior = self.behavior.to_domain();
        let format = self.format.to_domain();
        Ok(caly_domain::RuleProvider {
            name,
            source,
            behavior,
            format,
        })
    }
}

const fn default_format() -> RuleProviderFormatConfig {
    RuleProviderFormatConfig::Source
}

/// `type:` discriminator for [`RuleProviderConfig`]. The serde `tag`
/// attribute picks the variant by the value of the `type` field, so a
/// single `type: http | file | inline` token selects the body shape.
/// Exactly one variant is decoded per entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleProviderSourceConfig {
    /// `type: http` — fetched from `url` on `interval_ms` cadence.
    Http {
        /// Public HTTP(S) URL serving the rule body.
        url: String,
        /// Polling interval in milliseconds. Defaults to 86400000 (one day).
        #[serde(default = "default_http_interval_ms")]
        interval_ms: u64,
    },
    /// `type: file` — read once from the local file.
    File {
        /// Absolute or working-directory-relative path.
        path: String,
    },
    /// `type: inline` — body lives in the config under `payload:`.
    Inline {
        /// Rule body; one rule per line, same syntax as a Mihomo
        /// `rules:` block.
        payload: String,
    },
}

const fn default_http_interval_ms() -> u64 {
    86_400_000
}

/// `behavior:` field of a rule provider (the kind of matchers the body
/// contains). Mirrors `RouteProviderBehavior` so the rendered kernel
/// config keeps the same vocabulary as the domain model.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleProviderBehaviorConfig {
    Domain,
    DomainSuffix,
    IpCidr,
    Classical,
}

impl RuleProviderBehaviorConfig {
    const fn to_domain(self) -> RuleProviderBehavior {
        match self {
            Self::Domain => RuleProviderBehavior::Domain,
            Self::DomainSuffix => RuleProviderBehavior::DomainSuffix,
            Self::IpCidr => RuleProviderBehavior::IpCidr,
            Self::Classical => RuleProviderBehavior::Classical,
        }
    }
}

/// `format:` field of a rule provider; defaults to `source`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleProviderFormatConfig {
    Source,
    Binary,
}

impl RuleProviderFormatConfig {
    const fn to_domain(self) -> RuleProviderFormat {
        match self {
            Self::Source => RuleProviderFormat::Source,
            Self::Binary => RuleProviderFormat::Binary,
        }
    }
}
