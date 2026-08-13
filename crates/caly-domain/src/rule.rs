//! Core-agnostic routing rule model plus a pure, offline host matcher.
//!
//! Rules describe how traffic is steered: a matcher (domain / domain-suffix /
//! domain-keyword / ip-cidr / geoip / catch-all) and a policy (DIRECT / REJECT
//! / a named proxy group). The model renders into Mihomo `rules:` entries and
//! sing-box `route.rules`. Host matching is pure string logic and lives here;
//! IP/CIDR matching needs address arithmetic and lives in the infrastructure
//! layer (`caly-profile::rule_match`) to keep the domain free of `std::net`.

use crate::BoundedText;

/// Maximum bytes in one rule text field (domain, keyword, CIDR, group name).
pub const RULE_TEXT_MAX_BYTES: usize = 256;
/// Maximum bytes in a GEOIP country code.
pub const GEOIP_CODE_MAX_BYTES: usize = 8;
/// Maximum bytes in a rule-set provider tag (Mihomo `RULE-SET,<name>,...`
/// and sing-box `rule_set` targets share one bounded identity).
pub const RULE_PROVIDER_NAME_MAX_BYTES: usize = 64;
/// Maximum bytes in a SagerNet geosite category name.
pub const GEOSITE_NAME_MAX_BYTES: usize = 64;
/// Maximum bytes in a process-name match pattern.
pub const PROCESS_NAME_MAX_BYTES: usize = 256;
/// Maximum rule providers in one configuration.
pub const MAX_RULE_PROVIDERS: usize = 64;
/// Maximum rules in one ordered rule set.
pub const MAX_RULES: usize = 1024;
/// Maximum bytes in one inline rule-provider payload.
pub const INLINE_RULE_PAYLOAD_MAX_BYTES: usize = 256 * 1_024;

pub type RuleText = BoundedText<RULE_TEXT_MAX_BYTES>;
pub type GeoipCode = BoundedText<GEOIP_CODE_MAX_BYTES>;
pub type RuleProviderName = BoundedText<RULE_PROVIDER_NAME_MAX_BYTES>;
pub type GeositeName = BoundedText<GEOSITE_NAME_MAX_BYTES>;
pub type ProcessNamePattern = BoundedText<PROCESS_NAME_MAX_BYTES>;

/// How a rule matches traffic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleMatch {
    /// Exact full hostname match.
    Domain(RuleText),
    /// Hostname ends with the suffix (`.example.com` semantics).
    DomainSuffix(RuleText),
    /// Hostname contains the keyword anywhere.
    DomainKeyword(RuleText),
    /// IP within a CIDR block, stored as CIDR text (`addr/prefix`). Full
    /// address validation happens in the infrastructure matcher.
    IpCidr(RuleText),
    /// GeoIP country code. Rendering-only: evaluating needs a GeoIP database,
    /// which the offline engine does not carry.
    Geoip(GeoipCode),
    /// `RULE-SET,<provider>,<policy>` reference. A named rule-provider (user
    /// or auto-emitted for GEOIP) supplies the actual matcher. The offline
    /// engine never resolves the reference; this is a render-side variant.
    RuleSet(RuleProviderName),
    /// `GEOSITE,<category>,<policy>` reference. Renders through SagerNet's
    /// `geosite-<cat>` rule-set. Offline engine never matches.
    Geosite(GeositeName),
    /// `PROCESS-NAME,<pattern>,<policy>` Mihomo-only match: traffic from a
    /// process whose name matches the pattern. Render-only.
    ProcessName(ProcessNamePattern),
    /// `SRC-IP-CIDR,<cidr>,<policy>` Mihomo-only match: the *source* IP
    /// belongs to the CIDR block (not the destination). Render-only.
    SourceIpCidr(RuleText),
    /// Catch-all; conventionally the last rule.
    Match,
}

/// Where matched traffic is steered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RulePolicy {
    /// Bypass the proxy.
    Direct,
    /// Drop the connection.
    Reject,
    /// Route through a named proxy or proxy group.
    Proxy(RuleText),
}

/// Optional Clash/Mihomo trailing rule flags (`TYPE,VALUE,POLICY[,FLAG…]`).
///
/// `no-resolve` tells the core not to resolve hostnames before evaluating an
/// IP rule (standard on `IP-CIDR`/`IP-CIDR6`/`GEOIP` entries); `src` makes the
/// rule evaluate against the *source* address. Flags are stored canonically
/// (booleans, rendered in a fixed order) so parse → render round-trips are
/// stable even if the input listed them in a different order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuleFlags {
    /// `no-resolve` — skip DNS resolution for this rule.
    pub no_resolve: bool,
    /// `src` — match the source address instead of the destination.
    pub src: bool,
}

impl RuleFlags {
    /// Clash spelling of the `no-resolve` flag.
    pub const NO_RESOLVE: &'static str = "no-resolve";
    /// Clash spelling of the `src` flag.
    pub const SRC: &'static str = "src";

    /// No flags set (the common case).
    pub const fn none() -> Self {
        Self {
            no_resolve: false,
            src: false,
        }
    }

    /// Renders the flag suffix for a Clash rule line (`",no-resolve,src"` …).
    pub fn to_clash_suffix(&self) -> String {
        let mut suffix = String::new();
        if self.no_resolve {
            suffix.push(',');
            suffix.push_str(Self::NO_RESOLVE);
        }
        if self.src {
            suffix.push(',');
            suffix.push_str(Self::SRC);
        }
        suffix
    }
}

/// One routing rule: matcher plus policy, with optional trailing flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingRule {
    pub matcher: RuleMatch,
    pub policy: RulePolicy,
    /// Clash trailing flags (`no-resolve`, `src`); empty for most rules.
    pub flags: RuleFlags,
}

/// Where one named rule-provider fetches its rule body from. Mirrors the
/// Clash `type:` field of `rule-providers:` (a `type: http` provider is
/// downloaded at boot, `type: file` reads a local file, `type: inline` keeps
/// the body in the config). Mihomo and sing-box treat these uniformly; caly
/// only renders — fetching happens at the core level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleProviderSource {
    /// `type: http` — fetched from `url` on the core's polling cadence
    /// (`interval_ms`, default 86400 = one day).
    Http { url: RuleText, interval_ms: u64 },
    /// `type: file` — read once from the local path; the core treats the
    /// file as the authoritative source and reloads on `mtime` change.
    File { path: RuleText },
    /// `type: inline` — the rule body lives in the config under `payload:`.
    /// Useful for small, hand-curated rule-sets; size-bounded so a runaway
    /// config cannot bloat the rendered kernel config. Bounded to
    /// `INLINE_RULE_PAYLOAD_MAX_BYTES` (256 KiB) — well past any hand-curated
    /// list, well below anything that would meaningfully bloat the kernel
    /// config.
    Inline {
        payload: BoundedText<INLINE_RULE_PAYLOAD_MAX_BYTES>,
    },
}

/// What traffic a rule-provider's body targets. Maps to sing-box's
/// `route.rule_set` `rule_set` types and to Clash's `behavior:` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleProviderBehavior {
    /// `domain` — full hostnames only.
    Domain,
    /// `domain_suffix` — hostnames ending with the listed suffix.
    DomainSuffix,
    /// `ipcidr` — IP/CIDR entries.
    IpCidr,
    /// `classical` — Clash format rule lines (the most flexible; sing-box
    /// receives a converted `source` rule-set).
    Classical,
}

impl RuleProviderBehavior {
    /// Returns the lowercase Clash spelling (used in the rendered
    /// `rule-providers:` block).
    pub const fn clash_label(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::DomainSuffix => "domain_suffix",
            Self::IpCidr => "ipcidr",
            Self::Classical => "classical",
        }
    }
}

/// On-disk / on-wire format of a rule-provider body. `Source` is plain
/// text (Clash YAML/rule-line syntax); `Binary` is sing-box's compiled
/// `.srs` (smaller and faster at runtime, but only the sing-box core
/// consumes it directly).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleProviderFormat {
    /// `format: source` — plain text, both kernels accept.
    Source,
    /// `format: binary` — sing-box `.srs` (file/http only).
    Binary,
}

impl RuleProviderFormat {
    pub const fn clash_label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Binary => "binary",
        }
    }
}

/// A named rule provider: a tagged source of (possibly many) matchers that
/// rules can reference via `RuleMatch::RuleSet` (Mihomo `RULE-SET,<name>,…`)
/// or via sing-box's `rule_set` lookup. Mirrors `ProviderConfig` for
/// proxies — `ProviderKind` aggregates subscription sources, `RuleProvider`
/// aggregates rule bodies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleProvider {
    /// Tag referenced from `RULE-SET,<name>,…` rules. Must be unique
    /// across the rendered document.
    pub name: RuleProviderName,
    /// Where the body comes from.
    pub source: RuleProviderSource,
    /// What kind of matchers the body contains.
    pub behavior: RuleProviderBehavior,
    /// On-disk format.
    pub format: RuleProviderFormat,
}

/// Validation/parsing failure for a routing rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleError {
    Empty,
    /// Wrong field count for the rule type.
    Malformed,
    UnknownType,
    UnknownPolicy,
    /// Trailing flag token is not `no-resolve` / `src`.
    UnknownFlag,
    TextTooLong,
    InvalidCidr,
}

impl core::fmt::Display for RuleError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("rule must not be empty"),
            Self::Malformed => formatter.write_str("rule has the wrong number of fields"),
            Self::UnknownType => formatter.write_str("unknown rule type"),
            Self::UnknownPolicy => {
                formatter.write_str("rule policy must be DIRECT, REJECT, or a group")
            }
            Self::UnknownFlag => formatter.write_str("rule flags must be no-resolve and/or src"),
            Self::TextTooLong => formatter.write_str("rule field exceeds the bounded length"),
            Self::InvalidCidr => formatter.write_str("IP-CIDR value is not a valid CIDR block"),
        }
    }
}

impl std::error::Error for RuleError {}

impl RulePolicy {
    /// Parses a Clash policy token (`DIRECT`/`REJECT`/group name).
    fn parse(token: &str) -> Result<Self, RuleError> {
        match token {
            "DIRECT" => Ok(Self::Direct),
            "REJECT" => Ok(Self::Reject),
            name if !name.is_empty() => Ok(Self::Proxy(
                RuleText::new(name.to_owned()).map_err(|_| RuleError::TextTooLong)?,
            )),
            _ => Err(RuleError::UnknownPolicy),
        }
    }

    /// Renders the policy token back to Clash form.
    pub fn to_clash(&self) -> String {
        match self {
            Self::Direct => "DIRECT".to_owned(),
            Self::Reject => "REJECT".to_owned(),
            Self::Proxy(name) => name.as_str().to_owned(),
        }
    }
}

impl RuleMatch {
    /// Renders the matcher to its Clash `TYPE[,VALUE]` prefix.
    pub fn to_clash(&self) -> String {
        match self {
            Self::Domain(value) => format!("DOMAIN,{}", value.as_str()),
            Self::DomainSuffix(value) => format!("DOMAIN-SUFFIX,{}", value.as_str()),
            Self::DomainKeyword(value) => format!("DOMAIN-KEYWORD,{}", value.as_str()),
            Self::IpCidr(value) => {
                // IPv6 CIDRs render with the Clash-canonical
                // `IP-CIDR6` keyword (a `:` in the value is a
                // definitive IPv6 marker); the single `IpCidr`
                // variant otherwise collapses the two keywords
                // on parse, so the round-trip keeps the family.
                let cidr = value.as_str();
                if cidr.contains(':') {
                    format!("IP-CIDR6,{cidr}")
                } else {
                    format!("IP-CIDR,{cidr}")
                }
            }
            Self::Geoip(code) => format!("GEOIP,{}", code.as_str()),
            Self::RuleSet(name) => format!("RULE-SET,{}", name.as_str()),
            Self::Geosite(name) => format!("GEOSITE,{}", name.as_str()),
            Self::ProcessName(value) => format!("PROCESS-NAME,{}", value.as_str()),
            Self::SourceIpCidr(value) => format!("SRC-IP-CIDR,{}", value.as_str()),
            Self::Match => "MATCH".to_owned(),
        }
    }
}

impl RoutingRule {
    /// Builds a rule from validated parts, with no trailing flags.
    pub const fn new(matcher: RuleMatch, policy: RulePolicy) -> Self {
        Self {
            matcher,
            policy,
            flags: RuleFlags::none(),
        }
    }

    /// Attaches trailing Clash flags to the rule.
    #[must_use]
    pub const fn with_flags(mut self, flags: RuleFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Parses one Clash rule line (`TYPE,VALUE,POLICY[,FLAG…]`; `MATCH,POLICY`).
    ///
    /// The policy is always the *third* field for valued rules; anything past
    /// it must be a recognised trailing flag (`no-resolve`, `src`) — Mihomo
    /// writes IP rules as `IP-CIDR,10.0.0.0/8,DIRECT,no-resolve`, so treating
    /// the *last* field as the policy mis-parses every flagged rule.
    pub fn from_clash_line(line: &str) -> Result<Self, RuleError> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Err(RuleError::Empty);
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let kind = *fields.first().ok_or(RuleError::Malformed)?;
        match kind {
            "MATCH" => {
                // `MATCH,POLICY` — no value, and no flags are defined for the
                // catch-all; extra fields are a malformed line, not noise.
                if fields.len() != 2 {
                    return Err(RuleError::Malformed);
                }
                let policy_token = fields.get(1).ok_or(RuleError::Malformed)?;
                Ok(Self::new(
                    RuleMatch::Match,
                    RulePolicy::parse(policy_token)?,
                ))
            }
            "DOMAIN" | "DOMAIN-SUFFIX" | "DOMAIN-KEYWORD" | "IP-CIDR" | "IP-CIDR6" | "GEOIP"
            | "RULE-SET" | "GEOSITE" | "PROCESS-NAME" | "SRC-IP-CIDR" => {
                if fields.len() < 3 {
                    return Err(RuleError::Malformed);
                }
                let value = fields[1];
                let policy_token = fields.get(2).ok_or(RuleError::Malformed)?;
                let policy = RulePolicy::parse(policy_token)?;
                let flags = parse_flags(&fields[3..])?;
                let matcher = match kind {
                    "DOMAIN" => RuleMatch::Domain(text(value)?),
                    "DOMAIN-SUFFIX" => RuleMatch::DomainSuffix(text(value)?),
                    "DOMAIN-KEYWORD" => RuleMatch::DomainKeyword(text(value)?),
                    "IP-CIDR" | "IP-CIDR6" => RuleMatch::IpCidr(cidr_text(value)?),
                    "GEOIP" => RuleMatch::Geoip(
                        GeoipCode::new(value.to_owned()).map_err(|_| RuleError::TextTooLong)?,
                    ),
                    "RULE-SET" => RuleMatch::RuleSet(
                        RuleProviderName::new(value.to_owned())
                            .map_err(|_| RuleError::TextTooLong)?,
                    ),
                    "GEOSITE" => RuleMatch::Geosite(
                        GeositeName::new(value.to_owned()).map_err(|_| RuleError::TextTooLong)?,
                    ),
                    "PROCESS-NAME" => RuleMatch::ProcessName(
                        ProcessNamePattern::new(value.to_owned())
                            .map_err(|_| RuleError::TextTooLong)?,
                    ),
                    "SRC-IP-CIDR" => RuleMatch::SourceIpCidr(cidr_text(value)?),
                    _ => unreachable!("RULE-SET/GEOSITE/PROCESS-NAME/SRC-IP-CIDR arms above"),
                };
                Ok(Self::new(matcher, policy).with_flags(flags))
            }
            _ => Err(RuleError::UnknownType),
        }
    }

    /// Renders the full Clash rule line (flags appended when present).
    pub fn to_clash_line(&self) -> String {
        format!(
            "{},{}{}",
            self.matcher.to_clash(),
            self.policy.to_clash(),
            self.flags.to_clash_suffix()
        )
    }

    /// Whether this rule matches a hostname. GEOIP/IP rules never match a host.
    pub fn matches_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match &self.matcher {
            RuleMatch::Domain(value) => host == value.as_str().to_ascii_lowercase(),
            RuleMatch::DomainSuffix(value) => {
                let suffix = value.as_str().to_ascii_lowercase();
                host == suffix || host.ends_with(&format!(".{suffix}"))
            }
            RuleMatch::DomainKeyword(value) => host.contains(&value.as_str().to_ascii_lowercase()),
            RuleMatch::Match => true,
            RuleMatch::IpCidr(_)
            | RuleMatch::Geoip(_)
            | RuleMatch::RuleSet(_)
            | RuleMatch::Geosite(_)
            | RuleMatch::ProcessName(_)
            | RuleMatch::SourceIpCidr(_) => false,
        }
    }
}

fn text(value: &str) -> Result<RuleText, RuleError> {
    if value.is_empty() {
        return Err(RuleError::Malformed);
    }
    RuleText::new(value.to_owned()).map_err(|_| RuleError::TextTooLong)
}

/// Parses the trailing flag fields of a Clash rule line (each must be a
/// recognised flag; unknown tokens are rejected instead of silently
/// shadowing the policy, which is how flagged rules used to be mis-parsed).
fn parse_flags(tokens: &[&str]) -> Result<RuleFlags, RuleError> {
    let mut flags = RuleFlags::none();
    for token in tokens {
        match *token {
            t if t == RuleFlags::NO_RESOLVE => flags.no_resolve = true,
            t if t == RuleFlags::SRC => flags.src = true,
            _ => return Err(RuleError::UnknownFlag),
        }
    }
    Ok(flags)
}

/// Structural CIDR check kept in the domain (no address arithmetic): a CIDR
/// must be `address/prefix` with non-empty halves. Full IP validation happens
/// in the infrastructure matcher.
fn cidr_text(value: &str) -> Result<RuleText, RuleError> {
    let (address, prefix) = value.split_once('/').ok_or(RuleError::InvalidCidr)?;
    if address.is_empty() || prefix.is_empty() {
        return Err(RuleError::InvalidCidr);
    }
    RuleText::new(value.to_owned()).map_err(|_| RuleError::TextTooLong)
}

/// Returns the first rule whose matcher hits the hostname (order-sensitive).
pub fn match_host<'a>(rules: &'a [RoutingRule], host: &str) -> Option<&'a RoutingRule> {
    rules.iter().find(|rule| rule.matches_host(host))
}

#[cfg(test)]
mod tests;
