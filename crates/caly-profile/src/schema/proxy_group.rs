//! `proxy_groups:` schema segment.
//!
//! Mirrors [`crate::schema::settings`] for `rule_providers:`:
//! a parseable, validated config-level shape that round-trips
//! to the domain [`caly_domain::ProxyGroup`] model. The
//! schema layer keeps the kebab-case `type:` spelling
//! (matching the Mihomo render output and the
//! `set proxy-group add --type` CLI flag), the bounded
//! `interval_seconds` / `tolerance_ms` knobs, and the
//! `kind:`-tagged member set. Domain conversion happens
//! through [`ProxyGroupConfig::to_domain`] so the schema
//! validator can stay stringly-typed.
//!
//! The schema validator (`schema/validate.rs`) enforces
//! the per-set invariants the domain cannot: unique names,
//! reference resolution, and `relay` cycle detection. The
//! per-entry structural checks (non-empty `name`, the
//! `kind` discriminator landing on a known variant) are
//! left to serde's `deny_unknown_fields` plus the
//! `to_domain` conversion.

use std::fmt;

use caly_domain::{
    ProxyGroup, ProxyGroupError, ProxyGroupMember, ProxyGroupName, ProxyGroupType, ProxyGroupUrl,
    UrlTestConfig, is_path_safe_component,
};
use serde::{Deserialize, Serialize};

/// `proxy_groups:` entry as it appears in `config.yaml`.
/// `deny_unknown_fields` keeps the surface closed so a
/// typo'd `tppe:` is rejected at parse time.
///
/// **Deprecation track** (2026-08-09 规划, "subscriptions own the routing"):
/// schema-declared groups remain manageable through the CLI for one
/// transition cycle, but whenever any subscription declares its own
/// `proxy-groups`, the subscription topology is the one rendered into the
/// kernel — schema groups then have no render effect. New configurations
/// should rely on the subscription document; this block will be removed in
/// a later release after the transition period ends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyGroupConfig {
    /// Group tag; referenced from `rules:` and from other
    /// groups' `members:` lists. The schema validator
    /// enforces the same path-safe ASCII rule the
    /// profile id uses so the tag can be used in a
    /// bounded-text context without further
    /// normalisation.
    pub name: String,
    /// Behaviour: `select` / `url-test` / `fallback` /
    /// `load-balance` / `relay`. Renamed from the Rust
    /// enum's `kind` field to the on-disk `type:` key
    /// so the rendered Mihomo config and the user's
    /// `config.yaml` share the same spelling.
    #[serde(rename = "type")]
    pub group_type: ProxyGroupTypeConfig,
    /// Members in declaration order. Each entry is a
    /// `kind:`-tagged mapping.
    #[serde(default)]
    pub members: Vec<ProxyGroupMemberConfig>,
    /// Probe config; required for the probe-driven
    /// groups. `Option` is the schema-level shape; the
    /// validator rejects probe-driven groups without
    /// one and the non-probe groups with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_test: Option<UrlTestConfigConfig>,
    /// Round 20: `enabled: false` makes the daemon skip
    /// this group at boot (the entry stays in
    /// `proxy_groups:` for the operator to re-enable).
    /// Mirrors the
    /// `RuleProviderConfig::enabled` shape and the
    /// `SubscriptionSource::enabled` shape so the same
    /// `set X enable|disable <name>` policy works for
    /// all three.
    #[serde(default = "default_proxy_group_enabled")]
    pub enabled: bool,
}

const fn default_proxy_group_enabled() -> bool {
    true
}

impl ProxyGroupConfig {
    /// Builds the domain [`caly_domain::ProxyGroup`].
    /// Returns `Err(String)` for the field-level shape
    /// failures the domain can express (empty name,
    /// tag/path too long, …); cross-field invariants
    /// (duplicates, references, cycles) live in the
    /// schema validator and are reported as
    /// [`ProxyGroupError`] variants.
    pub fn to_domain(&self) -> Result<ProxyGroup, String> {
        let name = ProxyGroupName::new(self.name.clone())
            .map_err(|error| format!("proxy group name is too long: {error}"))?;
        let members = self
            .members
            .iter()
            .map(ProxyGroupMemberConfig::to_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let url_test = match &self.url_test {
            Some(value) => Some(value.to_domain()?),
            None => None,
        };
        Ok(ProxyGroup {
            name,
            kind: self.group_type.to_domain(),
            members,
            url_test,
        })
    }

    /// True when the group is not path-safe. The
    /// schema validator uses this to reject a name
    /// that would otherwise round-trip into a
    /// `ProxyGroupName` of dubious origin.
    pub fn has_unsafe_name(&self) -> bool {
        !is_path_safe_component(&self.name)
    }
}

impl fmt::Display for ProxyGroupConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "proxy group `{}` ({}, {} member(s))",
            self.name,
            self.group_type.clash_label(),
            self.members.len()
        )
    }
}

/// Closed set of proxy-group behaviours. Mirrors
/// [`caly_domain::ProxyGroupType`] but is a separate
/// schema type so the config layer never has to know
/// about the domain's serde attributes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyGroupTypeConfig {
    Select,
    UrlTest,
    Fallback,
    LoadBalance,
    Relay,
}

impl ProxyGroupTypeConfig {
    /// Returns the lowercase Clash spelling used in
    /// the rendered `proxy-groups:` block and in the
    /// `set proxy-group` CLI `--json` envelope.
    pub const fn clash_label(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::UrlTest => "url-test",
            Self::Fallback => "fallback",
            Self::LoadBalance => "load-balance",
            Self::Relay => "relay",
        }
    }

    /// Mirrors [`caly_domain::ProxyGroupType::needs_url`].
    pub const fn needs_url(self) -> bool {
        matches!(self, Self::UrlTest | Self::Fallback | Self::LoadBalance)
    }

    /// Converts to the domain type.
    pub const fn to_domain(self) -> ProxyGroupType {
        match self {
            Self::Select => ProxyGroupType::Select,
            Self::UrlTest => ProxyGroupType::UrlTest,
            Self::Fallback => ProxyGroupType::Fallback,
            Self::LoadBalance => ProxyGroupType::LoadBalance,
            Self::Relay => ProxyGroupType::Relay,
        }
    }
}

/// `members:` entry. Tagged with `kind:` so the
/// discriminator is a single key; the rest of the
/// fields are inline at the same level.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyGroupMemberConfig {
    Node { tag: String },
    Group { name: String },
    Direct,
    Reject,
}

impl ProxyGroupMemberConfig {
    /// Converts to the domain enum. The schema layer
    /// does not enforce that the referenced group
    /// exists; the validator does.
    pub fn to_domain(&self) -> Result<ProxyGroupMember, String> {
        match self {
            Self::Node { tag } => {
                let bounded = caly_domain::ProxyGroupNodeTag::new(tag.clone())
                    .map_err(|error| format!("proxy group member tag is too long: {error}"))?;
                Ok(ProxyGroupMember::Node { tag: bounded })
            }
            Self::Group { name } => {
                let bounded = ProxyGroupName::new(name.clone())
                    .map_err(|error| format!("proxy group reference name is too long: {error}"))?;
                Ok(ProxyGroupMember::Group { name: bounded })
            }
            Self::Direct => Ok(ProxyGroupMember::Direct),
            Self::Reject => Ok(ProxyGroupMember::Reject),
        }
    }
}

/// `url_test:` block. Both the `interval_seconds` and
/// `tolerance_ms` fields default to the Mihomo
/// defaults (300s, 50ms) so a config that supplies
/// only `url:` still validates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UrlTestConfigConfig {
    pub url: String,
    #[serde(default = "default_url_test_interval")]
    pub interval_seconds: u32,
    #[serde(default = "default_url_test_tolerance")]
    pub tolerance_ms: u32,
}

const fn default_url_test_interval() -> u32 {
    300
}

const fn default_url_test_tolerance() -> u32 {
    50
}

impl UrlTestConfigConfig {
    /// Builds the domain [`UrlTestConfig`].
    pub fn to_domain(&self) -> Result<UrlTestConfig, String> {
        let url = ProxyGroupUrl::new(self.url.clone())
            .map_err(|error| format!("proxy group url is too long: {error}"))?;
        Ok(UrlTestConfig {
            url,
            interval_seconds: self.interval_seconds,
            tolerance_ms: self.tolerance_ms,
        })
    }
}

/// The `proxy_group` variant of [`crate::schema::ConfigError`]
/// surfaces the [`ProxyGroupError`] produced by the
/// schema validator. The translation lives here so the
/// `?` operator keeps the call sites compact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyGroupConfigError(pub ProxyGroupError);

impl fmt::Display for ProxyGroupConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for ProxyGroupConfigError {}

impl From<ProxyGroupError> for ProxyGroupConfigError {
    fn from(value: ProxyGroupError) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_minimal_select_group() {
        let value = json!({
            "name": "Proxy",
            "type": "select",
            "members": [
                {"kind": "node", "tag": "node-1"},
                {"kind": "direct"},
                {"kind": "reject"},
            ]
        });
        let parsed: ProxyGroupConfig =
            serde_json::from_value(value).unwrap_or_else(|error| panic!("must parse: {error}"));
        assert_eq!(parsed.name, "Proxy");
        assert_eq!(parsed.group_type, ProxyGroupTypeConfig::Select);
        assert_eq!(parsed.members.len(), 3);
        assert!(parsed.url_test.is_none());
        let domain = parsed
            .to_domain()
            .unwrap_or_else(|error| panic!("to_domain: {error}"));
        assert_eq!(domain.kind, ProxyGroupType::Select);
    }

    #[test]
    fn parses_url_test_with_defaults() {
        let value = json!({
            "name": "Auto",
            "type": "url-test",
            "members": [
                {"kind": "node", "tag": "node-1"},
                {"kind": "group", "name": "Sub"}
            ],
            "url_test": {
                "url": "http://www.gstatic.com/generate_204"
            }
        });
        let parsed: ProxyGroupConfig = serde_json::from_value(value).unwrap();
        let url_test = parsed.url_test.as_ref().unwrap();
        assert_eq!(url_test.interval_seconds, 300);
        assert_eq!(url_test.tolerance_ms, 50);
    }

    #[test]
    fn rejects_unknown_field() {
        let value = json!({
            "name": "Auto",
            "type": "select",
            "members": [],
            "oops": true
        });
        let result: Result<ProxyGroupConfig, _> = serde_json::from_value(value);
        assert!(result.is_err(), "deny_unknown_fields must reject");
    }

    #[test]
    fn rejects_unknown_type() {
        let value = json!({
            "name": "Auto",
            "type": "round-robin",
            "members": []
        });
        let result: Result<ProxyGroupConfig, _> = serde_json::from_value(value);
        assert!(result.is_err(), "unknown group type must fail");
    }

    #[test]
    fn rejects_unknown_member_kind() {
        let value = json!({
            "name": "Auto",
            "type": "select",
            "members": [{"kind": "url", "value": "foo"}]
        });
        let result: Result<ProxyGroupConfig, _> = serde_json::from_value(value);
        assert!(result.is_err(), "unknown member kind must fail");
    }

    #[test]
    fn rejects_unsafe_name() {
        let value = json!({
            "name": "../escape",
            "type": "select",
            "members": []
        });
        let parsed: ProxyGroupConfig = serde_json::from_value(value).unwrap();
        assert!(parsed.has_unsafe_name());
    }

    #[test]
    fn rejects_node_tag_too_long() {
        let value = json!({
            "name": "Auto",
            "type": "select",
            "members": [
                {"kind": "node", "tag": "x".repeat(257)}
            ]
        });
        let parsed: ProxyGroupConfig = serde_json::from_value(value).unwrap();
        assert!(parsed.to_domain().is_err());
    }
}
