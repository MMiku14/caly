//! Offline subscription writer for the `set sub …` leaves.
//!
//! Round 14: also owns the offline subscription
//! **inspection** surface (`list` / `check` / `import`)
//! that `show sub …` and the legacy `set sub import`
//! preview path go through. The previous home
//! (`commands::bridge`) is being retired; the helpers
//! below are the source of truth.
//!
//! The 5 CRUD writers round-trip
//! `subscriptions.sources: Vec<SubscriptionSource>` and
//! the `providers:` list when an `import` writes inline
//! nodes. The per-source writers live in [`sources`]
//! (audit #70 file-length budget); the inspection
//! surface stays here.

use caly_platform::paths::AppPaths;

use super::resource_writer::{ResourceError, ResourceWriteOutcome};

#[cfg(test)]
use super::resource_writer::is_http_url;

mod sources;

pub use sources::{
    add_source, disable_source, enable_source, remove_source, resolve_source_ref, set_source,
};

/// Subscription-specific error variants. The shared
/// [`ResourceError`] cases flow through the `Shared(_)` arm.
#[derive(Debug)]
pub enum SubCmdError {
    /// The URL is empty, missing a scheme, or points at a
    /// non-HTTP(S) scheme (the SSRF guard refuses
    /// `file://` and `data://`).
    InvalidUrl(String),
    /// The optional `--name` display name is empty after
    /// trimming, over-long, or contains control characters.
    InvalidName(String),
    /// `add` on a URL that's already in
    /// `subscriptions.sources`.
    AlreadyDeclared(String),
    /// W2-β2a (§8-4): `add --name` collides with the display name
    /// of an existing source.
    NameTaken(String),
    /// W2-β2a (§8-7): the three-segment `add` token resolved to a
    /// local file that does not exist. `given` is the token as
    /// typed, `resolved` its absolute form.
    SourceFileNotFound { given: String, resolved: String },
    /// W2-β2b: a name-shaped addressing token matched more than one
    /// source (hand-edited configs can duplicate names; `add` keeps
    /// them unique). Address by URL instead.
    AmbiguousName(String),
    /// W2-β2a: `--every` (non-zero) was passed for a file source,
    /// which is always static. Classed as usage → exit 2.
    EveryOnFile,
    /// `remove` / `enable` / `disable` on a URL that's
    /// not in `subscriptions.sources`.
    NotDeclared(String),
    /// Any other failure the shared writer surfaces
    /// (read / parse / validate / IO). Carries the
    /// human-readable reason.
    Shared(ResourceError),
}

impl core::fmt::Display for SubCmdError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidUrl(reason) => write!(formatter, "invalid subscription URL: {reason}"),
            Self::InvalidName(reason) => write!(formatter, "invalid source name: {reason}"),
            Self::AlreadyDeclared(url) => {
                write!(formatter, "subscription source `{url}` is already declared")
            }
            // §8-4 contract, verbatim wording ("subscription \"N\"
            // already exists.").
            Self::NameTaken(name) => write!(formatter, "subscription \"{name}\" already exists."),
            // §8-7 contract: the error block itself is three lines —
            // what / resolved-absolute / how to disambiguate.
            Self::SourceFileNotFound { given, resolved } => write!(
                formatter,
                "file not found: {given}\nResolved absolute: {resolved}\nIf you meant a URL, include the scheme: https://..."
            ),
            Self::EveryOnFile => write!(
                formatter,
                "`--every` has no effect on file sources: a local file never changes upstream, so the source stays static (every: 0)."
            ),
            Self::AmbiguousName(name) => write!(
                formatter,
                "multiple subscription sources are named \"{name}\"; address the one you mean by its URL."
            ),
            Self::NotDeclared(url) => {
                write!(formatter, "subscription source `{url}` is not declared")
            }
            Self::Shared(shared) => write!(formatter, "{shared}"),
        }
    }
}

impl std::error::Error for SubCmdError {}

impl crate::output::ErrorHint for SubCmdError {
    fn hint(&self) -> Option<String> {
        match self {
            // §8-4: point at `sub set` (β2b, same window) or a fresh
            // name.
            Self::NameTaken(name) => Some(format!(
                "Use `caly sub set {name} --url <new-url>` to update, or `caly sub add <url> --name <other>` to rename."
            )),
            Self::AlreadyDeclared(url) => Some(format!(
                "Use `caly sub set {url} --every <hours>` to change its cadence, or `caly sub remove {url}` to delete it first."
            )),
            Self::EveryOnFile => Some("Drop `--every` to accept the static pin.".to_owned()),
            Self::AmbiguousName(_) => Some(
                "Re-run with the source URL instead of the name; `caly sub list` shows both."
                    .to_owned(),
            ),
            _ => None,
        }
    }
}

impl From<ResourceError> for SubCmdError {
    fn from(value: ResourceError) -> Self {
        match value {
            ResourceError::AlreadyDeclared(name) => Self::AlreadyDeclared(name),
            ResourceError::NotDeclared(name) => Self::NotDeclared(name),
            other => Self::Shared(other),
        }
    }
}

impl From<super::config_writer::ConfigWriteError> for SubCmdError {
    fn from(value: super::config_writer::ConfigWriteError) -> Self {
        Self::Shared(ResourceError::from(value))
    }
}

/// Resolves the operator's XDG roots. Mirrors
/// `client::profile::resolve_paths` so the test
/// fixtures can inject a custom `HOME` / `XDG_*` and the
/// behaviour is identical across writers.
pub fn resolve_paths() -> AppPaths {
    AppPaths::from_env()
}

/// Loads the active `AppConfig` from `paths` (so the
/// tests can inject a hermetic root via
/// `AppPaths::from_env_vars(&test_env())`).
pub fn load_declared_with(
    paths: &AppPaths,
) -> Result<caly_profile::schema::AppConfig, SubCmdError> {
    use caly_profile::loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits};
    // A missing config.yaml is a fresh install, not an error: bootstrap
    // the default layout so the first `sub add` works out of the box
    // (2026-08-12 用户动线 audit).
    if !paths.config.join("config.yaml").exists() {
        crate::client::config_generate::load_config_with_bootstrap(paths, false)
            .map_err(SubCmdError::InvalidUrl)?;
    }
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    caly_profile::loader::load_layered_yaml_with(
        &layered,
        limits,
        &InMemoryProfileResolver::lenient(),
    )
    .map_err(|error| {
        SubCmdError::Shared(ResourceError::Write(format!(
            "load layered config: {error}"
        )))
    })
}

/// What an `add` / `remove` / `enable` / `disable` /
/// `import` call did. `DryRun` is what `--dry-run` (the
/// default) returns when the call would have written.
pub type SubWriteOutcome = ResourceWriteOutcome;

/// Maximum length (in chars) of a `--name` display name. Mirrors
/// the TUI filter buffer bound so an over-long name never breaks
/// the presentation layer.
pub const MAX_SOURCE_NAME_CHARS: usize = 64;

// ── Round 16: offline subscription inspection ───────────────

/// `caly sub list` (v1 `show sub providers`) — list the
/// configured subscription providers and their sources.
/// W2 (§5.1/§5.4): adaptive table/TSV human face.
pub fn list_providers(
    json: bool,
    format: Option<crate::cli::OutputFormat>,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    let root = caly_platform::paths::AppPaths::from_env().config;
    let (declared, providers) = match crate::daemon_config::load_from(root) {
        Ok(None) => (
            caly_profile::schema::SubscriptionConfig::default(),
            Vec::new(),
        ),
        Ok(Some(config)) => (config.subscriptions.clone(), config.resolved_providers()),
        Err(error) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({ "ok": false, "error": error.to_string() })
                );
            } else {
                eprintln!("subscription list failed: {error}");
            }
            return ExitCode::FAILURE;
        }
    };
    let sources = source_rows(&declared);
    if providers.is_empty() {
        return render_empty_providers(json, format, sources);
    }
    if json {
        let items: Vec<serde_json::Value> = providers
            .iter()
            .map(|p| provider_json_item(p, &sources))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "providers": items,
                "sources": sources,
                "count": providers.len(),
            })
        );
    } else {
        // W2 (cli-v3-design.md §5.1/§5.4, Q6): one adaptive
        // table — subscription sources and inline-provider
        // nodes share the NAME/TYPE/SOURCE columns; W3a adds the
        // offline-cache-derived NODES/GROUPS/LAST REFRESH/
        // NEXT REFRESH columns (never a live parse) and the ⚠
        // stale badge (enabled source with no cached body). The
        // v1 provider wrapper lines and the trailing `N provider(s)`
        // count are retired (JSON keeps `providers` and `count`).
        let mode = crate::output::table_mode(format);
        let headers: &[&str] = match mode {
            crate::output::TableMode::Tsv => &[
                "id",
                "name",
                "type",
                "source",
                "nodes",
                "groups",
                "last_refresh",
                "next_refresh",
                "status",
            ],
            crate::output::TableMode::Table => &[
                "ID",
                "NAME",
                "TYPE",
                "SOURCE",
                "NODES",
                "GROUPS",
                "LAST REFRESH",
                "NEXT REFRESH",
                "STATUS",
            ],
        };
        let rows = provider_rows(mode, &sources, &providers);
        crate::output::print_table(mode, headers, &rows);
    }
    ExitCode::SUCCESS
}

/// Builds the W2/W3a unified listing rows: one row per subscription
/// source (`#<1-based index>` when unnamed), then one row per
/// inline-provider node. STATUS is the ●○⚠ badge on a TTY and the
/// `ok`/`disabled`/`stale` word in TSV; inline nodes carry no enable
/// state, hence `-`.
fn provider_rows(
    mode: crate::output::TableMode,
    sources: &[serde_json::Value],
    providers: &[caly_profile::schema::ProviderConfig],
) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    for row in sources {
        let enabled = row["enabled"].as_bool() == Some(true);
        // ID column: the 1-based add-order id shared with
        // `resolve_source_ref` (`sub remove 2` / `sub refresh 3` …);
        // unnamed sources keep the `#N` name fallback for back-compat.
        let id = row["index"].as_u64().map_or(0, |i| i + 1).to_string();
        let name = row["name"]
            .as_str()
            .map_or_else(|| format!("#{id}"), str::to_owned);
        let cached = row["cached"].as_bool() == Some(true);
        let nodes = row["nodes"].as_u64().unwrap_or(0);
        let groups = row["groups"]
            .as_str()
            .map_or_else(String::new, str::to_owned);
        let last_refresh = row["last_refresh"].as_u64();
        let next_refresh = row["next_refresh"].as_u64();
        let status = match mode {
            crate::output::TableMode::Tsv => {
                if !enabled {
                    "disabled".to_owned()
                } else if cached {
                    "ok".to_owned()
                } else {
                    "stale".to_owned()
                }
            }
            crate::output::TableMode::Table => {
                // ANSI only on a real terminal (same guard as the
                // online node table / offline group table); an explicit
                // `--format=table` into a pipe stays clean bytes.
                if !crate::client::output::ansi_enabled() {
                    if !enabled {
                        "○".to_owned()
                    } else if cached {
                        "●".to_owned()
                    } else {
                        "⚠".to_owned()
                    }
                } else if !enabled {
                    crate::output::paint("90", "○")
                } else if cached {
                    crate::output::paint("32", "●")
                } else {
                    crate::output::paint("33", "⚠")
                }
            }
        };
        let (last_cell, next_cell) = match mode {
            crate::output::TableMode::Tsv => (
                last_refresh.map_or_else(String::new, |ts| ts.to_string()),
                next_refresh.map_or_else(String::new, |ts| ts.to_string()),
            ),
            crate::output::TableMode::Table => (
                relative_time(now, last_refresh),
                relative_time(now, next_refresh),
            ),
        };
        rows.push(vec![
            id,
            name,
            "subscription".to_owned(),
            row["url"].as_str().unwrap_or("").to_owned(),
            nodes.to_string(),
            groups.clone(),
            last_cell,
            next_cell,
            status,
        ]);
    }
    for p in providers {
        if let caly_profile::schema::ProviderKind::InlineNodes(nodes) = &p.kind {
            for node in nodes {
                rows.push(vec![
                    "-".to_owned(),
                    p.name.clone(),
                    "inline-node".to_owned(),
                    node.clone(),
                    "-".to_owned(),
                    "-".to_owned(),
                    "-".to_owned(),
                    "-".to_owned(),
                    "-".to_owned(),
                ]);
            }
        }
    }
    rows
}

/// Relative human time (`2h ago` / `10h later` / `just now` / `—`),
/// mirroring the §5.4 example; TSV carries the raw unix seconds.
fn relative_time(now: u64, then: Option<u64>) -> String {
    let Some(then) = then else {
        return "—".to_owned();
    };
    let delta = i64::try_from(now).unwrap_or(i64::MAX) - i64::try_from(then).unwrap_or(i64::MAX);
    let (magnitude, unit) = if delta.abs() < 60 {
        // A future timestamp inside the next minute must not read as
        // "just now" — NEXT REFRESH would lie (2026-08-12 CLI audit).
        let seconds = delta.abs().max(1);
        return if delta >= 0 {
            "just now".to_owned()
        } else {
            format!("in {seconds}s")
        };
    } else if delta.abs() < 3600 {
        (delta.abs() / 60, "m")
    } else if delta.abs() < 86400 {
        (delta.abs() / 3600, "h")
    } else {
        (delta.abs() / 86400, "d")
    };
    if delta >= 0 {
        format!("{magnitude}{unit} ago")
    } else {
        format!("{magnitude}{unit} later")
    }
}

/// The no-provider face (extracted for the 100-line budget): JSON keeps
/// the empty envelope, sources-only configs still list their rows on the
/// human face (2026-08-12 CLI audit — "no provider" was misleading when
/// sources exist), a bare TSV prints the header contract, and an
/// absolutely empty config gets the actionable hint.
fn render_empty_providers(
    json: bool,
    format: Option<crate::cli::OutputFormat>,
    sources: Vec<serde_json::Value>,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    let mode = crate::output::table_mode(format);
    let headers: &[&str] = match mode {
        crate::output::TableMode::Tsv => &[
            "id",
            "name",
            "type",
            "source",
            "nodes",
            "groups",
            "last_refresh",
            "next_refresh",
            "status",
        ],
        crate::output::TableMode::Table => &[
            "ID",
            "NAME",
            "TYPE",
            "SOURCE",
            "NODES",
            "GROUPS",
            "LAST REFRESH",
            "NEXT REFRESH",
            "STATUS",
        ],
    };
    if json {
        println!(
            "{}",
            serde_json::json!({ "providers": [], "sources": sources, "count": 0 })
        );
    } else if !sources.is_empty() {
        let rows = provider_rows(mode, &sources, &[]);
        crate::output::print_table(mode, headers, &rows);
    } else if mode == crate::output::TableMode::Tsv {
        crate::output::print_table(mode, headers, &[]);
    } else {
        println!("no subscription provider configured");
        println!(
            "hint: set `subscriptions.url` or `subscriptions.sources` in config.yaml to create the default provider"
        );
    }
    ExitCode::SUCCESS
}

/// Renders one provider as its JSON row. Kind-specific
/// payload lives under its own key so consumers never
/// see a subscription provider's source list mirrored
/// onto an inline provider.
fn provider_json_item(
    p: &caly_profile::schema::ProviderConfig,
    sources: &[serde_json::Value],
) -> serde_json::Value {
    use caly_profile::schema::ProviderKind;
    match &p.kind {
        ProviderKind::SubscriptionSources => serde_json::json!({
            "name": p.name,
            "kind": "subscription-sources",
            "source_urls": sources
                .iter()
                .filter(|row| row["enabled"].as_bool() == Some(true))
                .filter_map(|row| row["url"].as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
        }),
        ProviderKind::InlineNodes(nodes) => serde_json::json!({
            "name": p.name,
            "kind": "inline-nodes",
            "nodes": nodes,
        }),
    }
}

/// Projects the declared subscription sources into the JSON row
/// model both `list_sources` and `list_providers` print: one row
/// per entry with the real `enabled` flag plus the optional
/// `--name` label (previously every row was hardcoded
/// `enabled: true`). W3a (§5.4): each row gains the offline-cache
/// derived `nodes`/`groups` counts, `last_refresh`/`next_refresh`
/// unix timestamps and the `cached` flag (⚠ = enabled without a
/// cached body — refresh never succeeded). The legacy scalar
/// `subscriptions.url` counts as index 0 when set (it is always
/// enabled).
fn source_rows(declared: &caly_profile::schema::SubscriptionConfig) -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    if let Some(url) = &declared.url {
        rows.push(serde_json::json!({
            "index": rows.len(), "url": url, "enabled": true,
        }));
    }
    for source in &declared.sources {
        let mut row = serde_json::json!({
            "index": rows.len(), "url": source.url, "enabled": source.enabled,
        });
        if let Some(name) = &source.name {
            row["name"] = serde_json::Value::String(name.clone());
        }
        rows.push(row);
    }
    for row in &mut rows {
        let url = row["url"].as_str().unwrap_or("");
        let (nodes, groups, last_refresh, cached) = offline_cache_stats(url);
        let next_refresh = next_refresh_ts(
            last_refresh,
            source_cadence(
                declared,
                usize::try_from(row["index"].as_u64().unwrap_or(0)).unwrap_or(usize::MAX),
            ),
        );
        row["nodes"] = serde_json::json!(nodes);
        row["groups"] = serde_json::json!(groups.join(", "));
        row["cached"] = serde_json::json!(cached);
        if let Some(ts) = last_refresh {
            row["last_refresh"] = serde_json::json!(ts);
        }
        if let Some(ts) = next_refresh {
            row["next_refresh"] = serde_json::json!(ts);
        }
    }
    rows
}

/// The effective refresh cadence in minutes for one declared source:
/// per-source `refresh_every_minutes`, else the batch cadence; `0`
/// (or a static file pin) means never.
fn source_cadence(declared: &caly_profile::schema::SubscriptionConfig, index: usize) -> u64 {
    // `index` counts the legacy scalar `subscriptions.url` as row 0, so
    // the `sources` list starts at index 1 — every row was off by one
    // (2026-08-12 agent audit): with a legacy url present, row N showed
    // the cadence of source N+1.
    let source_index = if declared.url.is_some() {
        index.saturating_sub(1)
    } else {
        index
    };
    if let Some(source) = declared.sources.get(source_index)
        && let Some(every) = source.refresh_every_minutes
    {
        return every;
    }
    declared.refresh_interval_minutes
}

/// `last_refresh + cadence`; `None` when there is no cache or the
/// cadence is zero (static / timer disabled).
fn next_refresh_ts(last_refresh: Option<u64>, cadence_minutes: u64) -> Option<u64> {
    match (last_refresh, cadence_minutes) {
        (Some(last), minutes) if minutes > 0 => last.checked_add(minutes.saturating_mul(60)),
        _ => None,
    }
}

/// Offline cache projection (W3a, §5.4): reads the daemon's cached
/// body for one source URL and derives the node/group counts without
/// any live parse trigger. The cache file is
/// `<state>/subscriptions/<hex(subscription_id_for_url(url))>`; its
/// mtime is the last successful refresh. `cached` = a body exists
/// (an enabled source without one has never refreshed — the ⚠ badge).
fn offline_cache_stats(url: &str) -> (usize, Vec<String>, Option<u64>, bool) {
    let root = caly_platform::paths::AppPaths::from_env()
        .state
        .join("subscriptions");
    let id = caly_backends::subscription::subscription_id_for_url(url);
    let path = root.join(caly_domain::to_hex(id.into_bytes()));
    let Ok(metadata) = std::fs::metadata(&path) else {
        return (0, Vec::new(), None, false);
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    let Ok(body) = std::fs::read(&path) else {
        // A body that exists but cannot be read is NOT cached — the
        // ⚠ stale badge must not hide an unreadable store (2026-08-12
        // CLI audit).
        return (0, Vec::new(), mtime, false);
    };
    let subscription = caly_domain::SubscriptionId::from_bytes([0; 16]);
    let (nodes, groups) = match caly_subscription::decode_document(body.clone()) {
        Ok(caly_subscription::SubscriptionDocument::ClashYaml(yaml)) => {
            std::str::from_utf8(yaml.as_slice())
                .ok()
                .map_or((0, Vec::new()), |source| {
                    caly_subscription::parse_clash_config(source, subscription).map_or(
                        (0, Vec::new()),
                        |import| {
                            (
                                import.proxies.len(),
                                import
                                    .proxy_groups
                                    .iter()
                                    .map(|group| group.name.to_string())
                                    .collect::<Vec<_>>(),
                            )
                        },
                    )
                })
        }
        Ok(caly_subscription::SubscriptionDocument::UriLines { .. }) => {
            caly_subscription::parse_uri_body_to_display_lossy(body, subscription)
                .map_or((0, Vec::new()), |projection| {
                    (projection.nodes.len(), Vec::new())
                })
        }
        _ => (0, Vec::new()),
    };
    (nodes, groups, mtime, true)
}

#[cfg(test)]
mod list_render_tests;

/// `show sub parse <path>` — offline subscription tree (W3a,
/// cli-v3-design.md G1/§5.3/§6.1). Human default is the three-zone
/// entry tree; `--json` is the §6.1 contract. url-list and ssr-only
/// documents keep the legacy summary shape.
pub fn check_subscription(
    path: &std::path::Path,
    userinfo: Option<&str>,
    json: bool,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    match crate::subscription::parse_subscription_tree(path, userinfo) {
        Ok(outcome) => {
            if json {
                println!("{}", crate::subscription::render_tree_json(&outcome));
            } else {
                print!("{}", crate::subscription::render_tree_human(&outcome));
            }
            if outcome.is_usable() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({ "ok": false, "error": error.clone() })
                );
            } else {
                eprintln!("subscription check failed: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

/// `show sub import <path>` / `--clipboard` — parse a
/// URI list and print a preview.
pub fn import_preview(
    path: Option<&std::path::Path>,
    clipboard: bool,
    json: bool,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    let summary = match (clipboard, path) {
        (true, Some(_)) => {
            eprintln!("error: `sub import --clipboard` takes no path argument");
            return ExitCode::from(2);
        }
        (true, None) | (false, None) => crate::subscription::read_clipboard(),
        (false, Some(path)) => crate::subscription::inspect_subscription_with_userinfo(path, None),
    };
    match summary {
        Ok(summary) => {
            if json {
                println!("{}", crate::subscription::render_json(&summary));
            } else {
                print!("{}", crate::subscription::render_human(&summary));
                if summary.format == "url-list" {
                    println!(
                        "This is a subscription *source* (fetchable URL). It is not persisted by `import`; add its URL to `subscriptions.sources` in config.yaml and it will appear in `caly sub list` as the default provider."
                    );
                } else {
                    println!(
                        "Preview only — nothing was saved. These are proxy node URIs (no upstream subscription URL), so they cannot form a subscription provider and won't appear in `sub list`. Paste a source subscription URL for a refreshable provider."
                    );
                }
            }
            if crate::subscription::is_usable(&summary) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({ "ok": false, "error": error.clone() })
                );
            } else {
                eprintln!("subscription import failed: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
