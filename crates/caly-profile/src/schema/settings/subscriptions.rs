//! Subscription sources and proxy-content provider settings.
//!
//! Split out of `settings.rs` (audit #70 file-length budget).

use serde::{Deserialize, Serialize};

/// Subscription refresh source and fetch-policy inputs wired to the daemon.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionConfig {
    /// Public HTTP(S) subscription URL. Omitted means the legacy single-source
    /// path is unavailable; prefer `sources` for multi-source refresh.
    pub url: Option<String>,
    /// Multiple explicit subscription sources; each is refreshed when
    /// `caly sub refresh` runs. `enabled` toggles an individual source.
    #[serde(default)]
    pub sources: Vec<SubscriptionSource>,
    /// TCP connect timeout for one fetch attempt in milliseconds (>= 100).
    pub connect_timeout_ms: u64,
    /// Overall request timeout for one fetch attempt in milliseconds (>= 1000).
    pub request_timeout_ms: u64,
    /// Maximum accepted response body in mebibytes (1..=256).
    pub max_body_mb: u64,
    /// Follow HTTP redirects (disabled by default for SSRF containment).
    /// Even when enabled, redirects are pinned to the original host: a
    /// cross-host Location hop stops the fetch rather than bypassing the
    /// DNS pinning (audit #111).
    pub follow_redirects: bool,
    /// Route the fetch through the environment proxy variables.
    pub use_environment_proxy: bool,
    /// Daemon-side periodic refresh cadence in minutes. `0`
    /// (default) disables the background timer entirely — the
    /// operator opts in so an unattended daemon never surprises
    /// them with outbound fetches (#59). Anything from `1`
    /// upwards spawns the timer.
    pub refresh_interval_minutes: u64,
}

/// One explicit subscription source in `subscriptions.sources`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionSource {
    /// Public HTTP(S) subscription URL.
    pub url: String,
    /// Refresh this source on `caly sub refresh` (default true).
    pub enabled: bool,
    /// Optional human-friendly display name (`set sub add --name`).
    /// Absent for sources declared before the flag landed; never
    /// serialised when `None` so existing configs round-trip
    /// byte-for-byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// W2-β2 (CLI v3 §8, decision Q5): per-source refresh period in minutes.
    /// `None` inherits the batch cadence (`refresh_interval_minutes`);
    /// `Some(0)` pins the source as static (never auto-refreshed — used for
    /// local file sources). Never serialised when `None` so existing configs
    /// round-trip byte-for-byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_every_minutes: Option<u64>,
}

impl Default for SubscriptionSource {
    fn default() -> Self {
        Self {
            url: String::new(),
            enabled: true,
            name: None,
            refresh_every_minutes: None,
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::SubscriptionSource;

    #[test]
    fn refresh_every_minutes_defaults_to_none_and_stays_unwritten() {
        let source: SubscriptionSource =
            serde_json::from_str(r#"{"url":"https://example.com/feed","enabled":true}"#)
                .expect("legacy json without refresh_every_minutes parses");
        assert_eq!(source.refresh_every_minutes, None);
        let written = serde_json::to_string(&source).expect("serialise");
        assert!(
            !written.contains("refresh_every_minutes"),
            "None must not be serialised: {written}"
        );
    }

    #[test]
    fn refresh_every_minutes_round_trips_when_set() {
        let source: SubscriptionSource = serde_json::from_str(
            r#"{"url":"file:///tmp/feed.yaml","enabled":true,"refresh_every_minutes":0}"#,
        )
        .expect("json with refresh_every_minutes parses");
        assert_eq!(source.refresh_every_minutes, Some(0));
        let written = serde_json::to_string(&source).expect("serialise");
        assert!(written.contains("\"refresh_every_minutes\":0"));
    }
}

/// A named provider grouping subscription sources or inline node URIs.
/// Rendered as one `proxy-providers:` entry in the Mihomo config; an unset
/// `providers:` list derives a `default` enumeration provider containing
/// every enabled subscription source. Declared via the CLI's legacy
/// `set provider` verbs, which the `sub` domain supersedes for sources —
/// inline nodes keep this container as their declaration home.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
}

/// What a provider aggregates.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// Every enabled subscription source URL, re-fetched with the source.
    #[default]
    SubscriptionSources,
    /// A fixed set of node URIs declared inline (no remote fetch).
    InlineNodes(Vec<String>),
}

impl SubscriptionConfig {
    /// The auto-created enumeration default provider: contains every enabled
    /// subscription source URL — exactly the set `caly sub list` reads.
    /// Returns `None` when no source is configured, so a node-only config
    /// resolves to an empty provider set.
    pub fn default_provider(&self) -> Option<ProviderConfig> {
        if self.enabled_source_urls().is_empty() {
            return None;
        }
        Some(ProviderConfig {
            name: "default".to_owned(),
            kind: ProviderKind::SubscriptionSources,
        })
    }

    /// Returns every enabled source URL: the legacy `url` (always enabled when
    /// set) followed by the enabled entries of `sources`. This is the full set
    /// `caly sub refresh` fetches and merges.
    pub fn enabled_source_urls(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(url) = &self.url {
            out.push(url.clone());
        }
        out.extend(
            self.sources
                .iter()
                .filter(|source| source.enabled)
                .map(|source| source.url.clone()),
        );
        out
    }
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            url: None,
            sources: Vec::new(),
            connect_timeout_ms: 5_000,
            request_timeout_ms: 30_000,
            max_body_mb: 32,
            follow_redirects: false,
            use_environment_proxy: false,
            refresh_interval_minutes: 0,
        }
    }
}
