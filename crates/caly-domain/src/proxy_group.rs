//! Proxy-group domain model.
//!
//! A **proxy group** is a named user-facing selector in the proxy
//! configuration. Mihomo/Clash render it as one entry under
//! `proxy-groups:`; sing-box projects it as a tag-referenced
//! `selector` / `urltest` / `fallback` / `relay` outbound. The
//! model mirrors the `RuleProvider` shape: a `name` (the
//! unique tag), a `type` (the behaviour), a list of `members`
//! (other groups, nodes, `DIRECT`, `REJECT`) and — for
//! probe-driven groups — a `url-test` config block.
//!
//! The model is render-only: it captures the declarative shape
//! and turns it into a tagged string for the kernel config
//! writer. The live selection of a node inside a group is a
//! kernel concern; it never enters the domain.
//!
//! Validation is also render-side: the model rejects duplicate
//! names within a configuration, identifies unknown members
//! against a `[proxy/group/regex]` set supplied by the schema
//! validator, and detects cycles in `relay` chains. It does
//! **not** validate URLs (the schema validator already rejects
//! non-`http(s)` URLs at parse time).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::BoundedText;

/// Maximum bytes in one proxy-group name. Mirrors
/// [`crate::RULE_PROVIDER_NAME_MAX_BYTES`] so a tag can be
/// referenced from `rules:` (e.g. `MATCH,MyGroup`) without
/// the schema layer raising a length cap.
pub const PROXY_GROUP_NAME_MAX_BYTES: usize = 64;

/// Maximum bytes in the `url-test:` `url:` field. The
/// kernel itself caps the reachable size; this is a
/// defensive ceiling so a runaway config cannot bloat the
/// rendered document.
pub const PROXY_GROUP_URL_MAX_BYTES: usize = 2_048;

/// Maximum number of members per group. The Mihomo/Clash
/// cap is implementation-defined; this budget matches
/// [`crate::MAX_RULE_PROVIDERS`] (one of the larger
/// bounded resources on the same config) and is well past
/// the practical size of any real selector.
pub const MAX_PROXY_GROUP_MEMBERS: usize = 256;

/// Maximum number of proxy groups per configuration.
pub const MAX_PROXY_GROUPS: usize = 64;

/// Bounded proxy-group name. Mirrors the path-safe ASCII
/// expectations of rule policies and proxy group
/// references: the loader uses the same `is_path_safe_component`
/// check as for `ProfileId`.
pub type ProxyGroupName = BoundedText<PROXY_GROUP_NAME_MAX_BYTES>;

/// Bounded URL for `url-test` / `fallback` groups.
pub type ProxyGroupUrl = BoundedText<PROXY_GROUP_URL_MAX_BYTES>;

/// Bounded node-tag reference inside a [`ProxyGroupMember`].
/// Reuses the same byte cap as the rule-text fields so the
/// rendered Mihomo block stays within the existing budget.
pub type ProxyGroupNodeTag = BoundedText<256>;

/// What a single member of a proxy group can refer to.
///
/// Members are intentionally a closed sum: Mihomo/Clash
/// accept either a node tag, a nested group tag, or the
/// two special tokens `DIRECT` and `REJECT`. sing-box
/// resolves the same set to its own outbound tag space.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyGroupMember {
    /// A reference to a node by its Mihomo `name:` tag (or,
    /// for inline proxies, the `id` rendered as a tag).
    Node {
        /// Bounded node tag.
        tag: ProxyGroupNodeTag,
    },
    /// A reference to another proxy group in the same
    /// configuration. The schema validator ensures the
    /// referenced group exists and that the reference does
    /// not form a cycle.
    Group {
        /// Bounded group name; the validator looks it up
        /// in the declared `proxy_groups:` set.
        name: ProxyGroupName,
    },
    /// `DIRECT` — bypass the proxy. Equivalent to the
    /// Mihomo `DIRECT` policy token.
    Direct,
    /// `REJECT` — drop the connection. Equivalent to the
    /// Mihomo `REJECT` policy token.
    Reject,
}

impl ProxyGroupMember {
    /// Renders the member back to its Mihomo/Clash form.
    pub fn to_clash(&self) -> String {
        match self {
            Self::Node { tag } => tag.as_str().to_owned(),
            Self::Group { name } => name.as_str().to_owned(),
            Self::Direct => "DIRECT".to_owned(),
            Self::Reject => "REJECT".to_owned(),
        }
    }
}

impl fmt::Display for ProxyGroupMember {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_clash())
    }
}

/// What a proxy group *does*.
///
/// The five variants cover every type the Mihomo kernel
/// understands and map 1-for-1 to the sing-box `selector`
/// / `urltest` / `fallback` / `loadbalance` / `relay`
/// outbound kinds. `deny_unknown_fields` on the schema
/// config enum mirrors the same set so a typo'd `type:`
/// fails at parse time, not at render time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyGroupType {
    /// `select` — operator picks one member by hand. The
    /// default group kind; survives every other group
    /// failure.
    Select,
    /// `url-test` — the kernel measures every member
    /// against `url` on `interval` seconds and selects the
    /// fastest. `tolerance` (ms) suppresses churn when two
    /// members are within that margin.
    UrlTest,
    /// `fallback` — try the members in declaration order;
    /// fall through to the next on probe failure. Uses the
    /// same `url` probe as `url-test`.
    Fallback,
    /// `load-balance` — round-robin over healthy
    /// members. Uses the same `url` probe as `url-test`
    /// (sing-box falls back to a built-in probe when
    /// `url` is missing).
    LoadBalance,
    /// `relay` — chain the members in declaration order;
    /// each member hands off to the next. The sing-box
    /// outbound kind is the same; Mihomo uses
    /// `type: relay` and accepts the same member set.
    Relay,
}

impl ProxyGroupType {
    /// Returns the lowercase Clash spelling (used in the
    /// rendered `proxy-groups:` block and inside
    /// `--json` envelopes).
    pub const fn clash_label(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::UrlTest => "url-test",
            Self::Fallback => "fallback",
            Self::LoadBalance => "load-balance",
            Self::Relay => "relay",
        }
    }

    /// Whether this group kind needs a probe `url:` in
    /// the rendered config. `Select` and `Relay` do not
    /// probe; the others do.
    pub const fn needs_url(self) -> bool {
        matches!(self, Self::UrlTest | Self::Fallback | Self::LoadBalance)
    }
}

/// Probe configuration for `url-test` / `fallback` /
/// `load-balance` groups.
///
/// `interval` and `tolerance` are the same knobs the
/// Mihomo kernel exposes; `url` is the probe endpoint.
/// Defaults match Mihomo's: 300s interval, 50ms
/// tolerance, `http://www.gstatic.com/generate_204` as
/// the probe URL.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct UrlTestConfig {
    /// Probe URL; must be `http(s)://`. The schema
    /// validator enforces the scheme.
    pub url: ProxyGroupUrl,
    /// Probe cadence in seconds. Defaults to 300.
    #[serde(default = "default_url_test_interval")]
    pub interval_seconds: u32,
    /// Tolerance in milliseconds: a new "winner" within
    /// this margin of the current selection is ignored
    /// (suppresses flap). Defaults to 50.
    #[serde(default = "default_url_test_tolerance")]
    pub tolerance_ms: u32,
}

const fn default_url_test_interval() -> u32 {
    300
}

const fn default_url_test_tolerance() -> u32 {
    50
}

impl UrlTestConfig {
    /// Builds a default config pinned at gstatic + 300s +
    /// 50ms tolerance. Returns `Err(_)` only if the
    /// literal default URL ever grows past the
    /// [`PROXY_GROUP_URL_MAX_BYTES`] cap (the literal
    /// is a 32-byte ASCII string; the cap cannot be
    /// reached today).
    ///
    /// The default is a `Result` because the project
    /// denies `clippy::panic` / `clippy::expect_used`
    /// and the bounded-text constructor is fallible.
    /// Callers that need a "definitely-default" path
    /// can match the literal and `panic!` at the
    /// call site (their own choice, not this crate's).
    pub fn default_probe() -> Result<Self, crate::TextError> {
        const DEFAULT_URL: &str = "http://www.gstatic.com/generate_204";
        let url = ProxyGroupUrl::new(DEFAULT_URL.to_owned())?;
        Ok(Self {
            url,
            interval_seconds: default_url_test_interval(),
            tolerance_ms: default_url_test_tolerance(),
        })
    }
}

/// One declared proxy group: a tagged selector of nodes,
/// groups and the two policy tokens.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProxyGroup {
    /// Group tag; referenced from `rules:` (e.g.
    /// `MATCH,Auto`) and from other groups' `members:`
    /// lists. Must be unique within one configuration.
    pub name: ProxyGroupName,
    /// Behaviour of the group.
    #[serde(rename = "type")]
    pub kind: ProxyGroupType,
    /// Members in declaration order. Mihomo treats the
    /// first member as the default for `select`; the
    /// probe-driven groups iterate the list to find a
    /// healthy member.
    pub members: Vec<ProxyGroupMember>,
    /// Probe configuration; required for `url-test` /
    /// `fallback` / `load-balance`, ignored for `select`
    /// / `relay`. The schema validator rejects groups
    /// whose `kind` requires a probe but does not
    /// provide one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_test: Option<UrlTestConfig>,
}

/// Validation failure for a declared proxy group set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyGroupError {
    /// Two groups share the same `name`. The loader
    /// dedupes by `name` because both rules and other
    /// groups reference groups by it.
    DuplicateName { name: String },
    /// `url-test` / `fallback` / `load-balance` group
    /// without a `url_test:` block. The renderer
    /// would have to fabricate a URL, which the
    /// configuration model explicitly refuses.
    MissingUrlTest { name: String },
    /// `select` / `relay` group with a `url_test:`
    /// block. The probe config is silently ignored
    /// today; the schema validator surfaces this
    /// case so the operator knows the field is dead.
    UnexpectedUrlTest { name: String },
    /// A `url-test` / `fallback` / `load-balance`
    /// interval is zero. A zero cadence would hammer
    /// the probe URL.
    InvalidInterval { name: String },
    /// A `ProxyGroupMember::Group` reference points at
    /// a name that is not declared. The loader sees
    /// the full set of group names; a missing
    /// reference is a hard failure at boot.
    UnknownMemberGroup { group: String, member: String },
    /// A `relay` chain is cyclic (A → B → A). Mihomo
    /// and sing-box would loop forever.
    RelayCycle { group: String },
    /// The group declares zero `members:`. Both kernels
    /// refuse a proxy group with an empty member list at
    /// boot, so the schema validator rejects it earlier
    /// with a precise diagnostic.
    EmptyMembers { name: String },
    /// A probe-driven group's `url_test.url` is not a
    /// public http(s) URL. The kernel only fails when the
    /// first probe runs; the schema validator rejects the
    /// malformed probe target at load time.
    InvalidProbeUrl { name: String },
    /// The group has more members than the budget.
    TooManyMembers { name: String },
    /// The total number of groups exceeds the budget.
    TooManyGroups,
}

impl fmt::Display for ProxyGroupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => {
                write!(formatter, "proxy group `{name}` is declared more than once")
            }
            Self::MissingUrlTest { name } => write!(
                formatter,
                "proxy group `{name}` requires a `url_test:` block (url-test / fallback / load-balance)"
            ),
            Self::UnexpectedUrlTest { name } => write!(
                formatter,
                "proxy group `{name}` is a select/relay; `url_test:` is ignored"
            ),
            Self::InvalidInterval { name } => write!(
                formatter,
                "proxy group `{name}` has a non-positive `interval_seconds`"
            ),
            Self::UnknownMemberGroup { group, member } => write!(
                formatter,
                "proxy group `{group}` references unknown group `{member}`"
            ),
            Self::RelayCycle { group } => {
                write!(formatter, "proxy group `{group}` has a relay cycle")
            }
            Self::EmptyMembers { name } => write!(
                formatter,
                "proxy group `{name}` declares no members; the core refuses empty groups"
            ),
            Self::InvalidProbeUrl { name } => write!(
                formatter,
                "proxy group `{name}` url_test.url must be a public http(s) URL"
            ),
            Self::TooManyMembers { name } => write!(
                formatter,
                "proxy group `{name}` has more than {MAX_PROXY_GROUP_MEMBERS} members"
            ),
            Self::TooManyGroups => write!(
                formatter,
                "more than {MAX_PROXY_GROUPS} proxy groups are declared"
            ),
        }
    }
}

impl std::error::Error for ProxyGroupError {}

#[cfg(test)]
mod tests;
