//! Direct HTTP subscription backend using the repository's SSRF-safe fetch policy.

use crate::core::MihomoNodeRegistry;
use caly_domain::SubscriptionId;
use caly_platform::paths::{AppPaths, SafeName};
use caly_ports::{ActorFailure, RefreshOutcome, SubscriptionCommandBackend};
use caly_profile::loader::{LayeredConfigPaths, LoaderLimits, load_layered_yaml_strict};
use caly_subscription::net::{prefer_public_addresses, resolve_host_with_retry};
/// P8b: intake-domain function moved up to `caly_subscription::id`;
/// re-exported here so adapter internals keep their call sites.
use caly_subscription::subscription_id_for_url;
use caly_subscription::{
    FetchPolicy, FetchResult, FetchValidators, ResolvedAddresses, fetch_pinned,
};

use super::PublicAddressClassifier;
use super::render_compose::uri_body_to_sing_box_json_with;

mod expansion;

/// HTTP fetch attempts before surfacing a failure (absorbs transient blips).
const FETCH_ATTEMPTS: usize = 2;
/// Inter-attempt delay in milliseconds for a transient fetch failure.
const FETCH_RETRY_MILLIS: u64 = 250;

/// A fetch attempt result carrying an explicit transient flag, so retry
/// decisions are structural rather than string-matched, plus the
/// "content updated" flag (true = the server sent fresh bytes, false =
/// 304 NotModified) that gates the post-refresh kernel re-render.
type Attempt = Result<bool, (bool, ActorFailure)>;

/// Runs `attempt` with up to `max_attempts` tries, retrying only transient
/// failures with `retry_millis` backoff. Non-transient failures surface
/// immediately. The `bool` payload is the last attempt's "content updated"
/// flag. Pure and testable without network I/O.
fn retry_fetch<F>(
    max_attempts: usize,
    retry_millis: u64,
    mut attempt: F,
) -> Result<bool, ActorFailure>
where
    F: FnMut() -> Attempt,
{
    let mut last = crate::failure("fetch failed", "retry");
    for index in 0..max_attempts {
        match attempt() {
            Ok(changed) => return Ok(changed),
            Err((transient, error)) if index + 1 < max_attempts && transient => {
                last = error;
                std::thread::sleep(std::time::Duration::from_millis(retry_millis));
            }
            Err((_, error)) => return Err(error),
        }
    }
    Err(last)
}

/// Direct HTTP subscription backend using the repository's SSRF-safe fetch policy.
pub struct HttpSubscriptionBackend {
    sources: std::collections::BTreeMap<SubscriptionId, String>,
    /// W2-β2b: per-source last-successful-fetch ledger, consulted
    /// only by scheduled (`#59` timer) refreshes. Memory-only: a
    /// daemon restart treats every source as due once, which matches
    /// "the source may have changed while we were down".
    last_refresh: std::collections::BTreeMap<SubscriptionId, std::time::Instant>,
    cache: super::CachedSubscriptionBackend,
    /// Declared subscription ids (config.yaml `subscriptions`) the cache
    /// restore is allowed to revive; anything else is a ghost cache
    /// (2026-08-12 user-flow audit).
    declared: std::collections::BTreeSet<caly_domain::SubscriptionId>,
    handle: tokio::runtime::Handle,
    policy: FetchPolicy,
}

impl HttpSubscriptionBackend {
    /// Creates an HTTP backend using the current application runtime handle.
    pub fn new() -> Result<Self, ActorFailure> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            crate::failure(
                "subscription backend requires a Tokio runtime",
                "construct it inside daemon runtime composition",
            )
        })?;
        Ok(Self {
            sources: std::collections::BTreeMap::new(),
            last_refresh: std::collections::BTreeMap::new(),
            cache: super::CachedSubscriptionBackend::new(),
            declared: std::collections::BTreeSet::new(),
            handle,
            policy: FetchPolicy::direct_default(),
        })
    }

    /// Overrides the SSRF-safe fetch policy bounds (timeouts, body cap,
    /// redirects, environment proxy) from configuration.
    #[must_use]
    pub fn with_policy(mut self, policy: FetchPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Shares an automatic NodeId → Mihomo group/node registry.
    #[must_use]
    pub fn with_node_registry(mut self, registry: MihomoNodeRegistry) -> Self {
        self.cache = self.cache.with_node_registry(registry);
        self
    }

    /// Shares the subscription-author routing registry (groups + rules) so a
    /// refresh indexes author-declared topology alongside the nodes.
    #[must_use]
    pub fn with_routing_registry(mut self, registry: crate::core::CoreRoutingRegistry) -> Self {
        self.cache = self.cache.with_routing_registry(registry);
        self
    }

    /// Enables a persistent raw-body cache directory so the node registry can
    /// be rebuilt after a daemon restart without re-fetching.
    #[must_use]
    pub fn with_cache_dir(mut self, cache_dir: std::path::PathBuf) -> Self {
        self.cache = self.cache.with_cache_dir(cache_dir);
        self
    }

    /// Restores every persisted subscription body into the node registry.
    /// Best-effort: unreadable entries are skipped with a warning. Returns
    /// the restored projection slices for seeding the presentation snapshot.
    pub fn restore_cached(&mut self) -> Result<Vec<caly_domain::SnapshotNodes>, ActorFailure> {
        let declared = self.declared.clone();
        self.cache.restore_from_cache(&declared)
    }

    /// Declares a subscription id as live (config.yaml), so cache
    /// restore revives it; used by `register_subscription_url`.
    pub fn declare_id(&mut self, id: caly_domain::SubscriptionId) {
        self.declared.insert(id);
    }

    /// Registers a source URL for a subscription id.
    pub fn put_url(&mut self, id: SubscriptionId, url: String) {
        self.sources.insert(id, url);
    }

    /// Returns the last committed generation.
    pub fn generation(&self, id: SubscriptionId) -> u64 {
        self.cache.generation(id)
    }

    /// Fetches the source and generates a sing-box JSON config from dialable
    /// nodes, rendering the document header from `tuning`.
    pub fn refresh_sing_box_config(
        &mut self,
        id: SubscriptionId,
        tuning: &caly_coreconf::sing_box::SingBoxRenderTuning,
    ) -> Result<Vec<u8>, ActorFailure> {
        let mut touched = std::collections::BTreeSet::new();
        self.fetch(id, &mut touched)?;
        let body = self.cache.raw_source(id).ok_or_else(|| {
            crate::failure(
                "sing-box raw subscription cache is missing",
                "retry the source fetch",
            )
        })?;
        uri_body_to_sing_box_json_with(body, id, tuning).map_err(|error| {
            crate::failure(
                &format!("sing-box outbound rendering failed: {error}"),
                "inspect subscription protocol support",
            )
        })
    }

    fn fetch(
        &mut self,
        id: SubscriptionId,
        touched: &mut std::collections::BTreeSet<SubscriptionId>,
    ) -> Result<bool, ActorFailure> {
        let source = self.sources.get(&id).cloned().ok_or_else(|| {
            crate::failure(
                "subscription URL is not configured",
                "configure a source URL",
            )
        })?;
        let addresses = resolve_addresses(&source)?;
        touched.insert(id);
        let changed = retry_fetch(FETCH_ATTEMPTS, FETCH_RETRY_MILLIS, || {
            self.fetch_once(&source, &addresses, id)
        })?;
        // URL-list children are conditionally fetched too: a fresh child
        // counts as content change even when the parent answered 304.
        Ok(changed || self.expand_url_list(id, touched)?)
    }

    fn fetch_once(
        &mut self,
        source: &str,
        addresses: &ResolvedAddresses,
        id: SubscriptionId,
    ) -> Attempt {
        let validators = self.cache.validators(id);
        match self.request_body(source, addresses, &validators)? {
            FetchResult::Updated {
                body,
                etag,
                last_modified,
                userinfo,
            } => {
                self.cache.put_source(id, body.into_vec());
                self.cache.put_userinfo(id, userinfo);
                // Feed the validators back into the next conditional request:
                // without this the server never sees If-None-Match again and
                // every refresh pulls the full body (and the NotModified arm
                // stays dead code).
                self.cache.put_validators(
                    id,
                    FetchValidators {
                        etag,
                        last_modified,
                    },
                );
                Ok(true)
            }
            FetchResult::NotModified => Ok(false),
        }
    }

    /// One SSRF-safe pinned HTTP request shared by the primary fetch and the
    /// URL-list child fetches. `validators` carries the conditional-request
    /// metadata from the previous successful fetch of the same URL.
    pub(super) fn request_body(
        &mut self,
        source: &str,
        addresses: &ResolvedAddresses,
        validators: &FetchValidators,
    ) -> Result<FetchResult, (bool, ActorFailure)> {
        // The backend runs inside a `spawn_blocking` actor, so a direct
        // `block_on` is safe and avoids the multi-thread-runtime dependency of
        // `block_in_place` (which would panic on a current-thread runtime).
        self.handle
            .block_on(fetch_pinned(
                source,
                addresses.clone(),
                &PublicAddressClassifier,
                self.policy,
                validators,
            ))
            .map_err(|error| {
                let transient = is_transient_fetch(&error);
                let failure = crate::failure(
                    &format!("subscription fetch failed: {error}"),
                    "inspect HTTP, DNS, and source policy",
                );
                (transient, failure)
            })
    }
}

/// Whether a fetch error is transient and worth retrying: connect/request
/// timeouts and temporary (5xx) server statuses. Policy rejections (bad URL,
/// unsupported scheme, non-public address, body too large) are not retried.
fn is_transient_fetch(error: &caly_subscription::FetchError) -> bool {
    use caly_subscription::FetchError;
    match error {
        FetchError::RequestFailed(_) | FetchError::ClientBuild => true,
        FetchError::UnexpectedStatus(status) => (500..600).contains(status),
        _ => false,
    }
}

fn resolve_addresses(source: &str) -> Result<ResolvedAddresses, ActorFailure> {
    let parsed = url::Url::parse(source)
        .map_err(|_| crate::failure("subscription URL is invalid", "use an HTTP(S) URL"))?;
    // W2-β2a (Q5): `file://` sources skip DNS entirely — the fetch
    // half reads from disk before it would touch the resolved set.
    if parsed.scheme() == "file" {
        return ResolvedAddresses::try_from_vec(Vec::new())
            .map_err(|_| crate::failure("empty DNS-answer set is out of bounds", "report a bug"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| crate::failure("subscription URL has no host", "configure a valid host"))?;
    let port = parsed.port_or_known_default().ok_or_else(|| {
        crate::failure(
            "subscription URL has no port",
            "configure a valid HTTP(S) URL",
        )
    })?;
    // Shared resolver (single implementation lives in caly_subscription::net):
    // retries transient/empty lookups, then filters to public addresses with
    // IPv4 preferred.
    let addresses = resolve_host_with_retry(host, port).map_err(|rejection| {
        let detail = match rejection {
            caly_subscription::net::ResolveRejection::Empty => {
                "subscription DNS returned no answers"
            }
            caly_subscription::net::ResolveRejection::Lookup => {
                "subscription DNS resolution failed"
            }
        };
        crate::failure(detail, "inspect DNS and source availability")
    })?;
    let addresses = prefer_public_addresses(addresses, &PublicAddressClassifier);
    if addresses.is_empty() {
        return Err(crate::failure(
            "subscription resolved only to non-public addresses",
            "use a public subscription host",
        ));
    }
    ResolvedAddresses::try_from_vec(addresses).map_err(|_| {
        crate::failure(
            "subscription has too many DNS answers",
            "use a bounded source host",
        )
    })
}

impl SubscriptionCommandBackend for HttpSubscriptionBackend {
    #[allow(clippy::too_many_lines)] // pipeline stages: resolve→cleanup→fetch→merge
    fn refresh(
        &mut self,
        subscription: SubscriptionId,
        mode: caly_ports::RefreshMode,
    ) -> Result<RefreshOutcome, ActorFailure> {
        // Re-resolve the sources each refresh: env wins, then layered config,
        // so a config edit takes effect without a daemon restart. A source
        // registered by bootstrap (e.g. sing-box document rendering) is never
        // overwritten. A broken config.yaml is surfaced as its own error
        // instead of masquerading as "no subscription source is configured".
        let sources = resolve_sources()?;
        for resolved in &sources {
            self.sources.insert(resolved.id, resolved.url.clone());
        }
        // 刀 6 (memory audit): ids touched by this refresh (top-level and
        // URL-list children); removed subscriptions are released below.
        // The declared set is resolved up front; the retain runs AFTER the
        // fetch loop so `touched` carries the real child ids — running it
        // here (before fetching) would wipe every URL-list child cache and
        // defeat their conditional requests (2026-08-12 boundary audit).
        let mut touched: std::collections::BTreeSet<SubscriptionId> =
            std::collections::BTreeSet::new();
        let declared: std::collections::BTreeSet<SubscriptionId> =
            sources.iter().map(|source| source.id).collect();
        // W2-β2b target selection: the all-zero id means "every enabled
        // source" (batch), narrowed to the due subset when the periodic
        // timer drives the run; a concrete id selects exactly one source
        // and an unknown id is a hard error instead of a silent batch.
        let all_zero = subscription.into_bytes() == [0_u8; 16];
        let targets: Vec<&ResolvedSource> = if all_zero {
            if mode.scheduled {
                let now = std::time::Instant::now();
                sources
                    .iter()
                    .filter(|source| {
                        due_for_scheduled_refresh(
                            source.every_minutes,
                            self.last_refresh.get(&source.id),
                            now,
                        )
                    })
                    .collect()
            } else {
                sources.iter().collect()
            }
        } else {
            let Some(one) = sources.iter().find(|s| s.id == subscription) else {
                return Err(crate::failure(
                    "subscription source is not configured",
                    "check `caly sub list` for the enabled sources",
                ));
            };
            vec![one]
        };
        // Scheduled ticks with nothing due are a quiet success — the
        // operator opted into a cadence, not into a fetch.
        if targets.is_empty() && mode.scheduled {
            return caly_domain::SnapshotNodes::try_from_vec(Vec::new())
                .map(|nodes| RefreshOutcome {
                    nodes,
                    changed: false,
                })
                .map_err(|_| crate::failure("empty snapshot is out of bounds", "report a bug"));
        }
        // Fetch every selected source and merge their projections into one
        // snapshot. A single failing source does not abort the rest; its
        // failure is reported and the remaining sources still land.
        let mut merged = Vec::new();
        let mut any_changed = false;
        let mut first_error: Option<ActorFailure> = None;
        for source in &targets {
            let id = source.id;
            // Audit #89: a partially-failed multi-source refresh used to drop
            // the failing source's error silently (only returned when ALL
            // sources failed). Warn per failure so the stale nodes that keep
            // serving are at least visible to the operator.
            if mode.force {
                self.cache.clear_validators(id);
            }
            match self.fetch(id, &mut touched) {
                Ok(changed) => {
                    any_changed |= changed;
                }
                Err(error) => {
                    tracing::warn!(
                        subscription = %id,
                        source = %source.url,
                        error = %error.message,
                        "subscription source fetch failed; keeping any cached nodes"
                    );
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
            }
            self.last_refresh.insert(id, std::time::Instant::now());
            match self.cache.refresh_projection(id) {
                Ok(projection) => merged.extend(projection.nodes.into_vec()),
                Err(error) => {
                    tracing::warn!(
                        subscription = %id,
                        source = %source.url,
                        error = %error.message,
                        "subscription source re-projection failed; keeping prior nodes"
                    );
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        // 刀 6 (memory audit): release cache entries, projections and
        // node/routing registrations of no-longer-declared subscriptions.
        // Runs BEFORE the empty-merge early return: a refresh that yields
        // no nodes (e.g. every source disabled) must still converge the
        // registry, or disabled subscriptions keep their stale nodes
        // (2026-08-13 user report). `touched` carries real child ids from
        // the fetch loop, so URL-list children keep their conditional state.
        self.cache.retain_declared(&declared, &touched);
        if merged.is_empty() {
            // Distinguish "no sources at all" from "sources exist but every
            // one came back empty": the former is a configuration error, the
            // latter is a legitimate (if unusual) refresh outcome
            // (2026-08-12 boundary audit).
            if let Some(error) = first_error {
                return Err(error);
            }
            return caly_domain::SnapshotNodes::try_from_vec(Vec::new())
                .map(|nodes| RefreshOutcome {
                    nodes,
                    changed: true,
                })
                .map_err(|_| crate::failure("empty snapshot is out of bounds", "report a bug"));
        }
        let nodes = caly_domain::SnapshotNodes::try_from_vec(merged).map_err(|_| {
            crate::failure(
                "merged subscription nodes exceeded the snapshot bound",
                "reduce the number of subscription sources or their node counts",
            )
        })?;
        Ok(RefreshOutcome {
            nodes,
            changed: any_changed,
        })
    }
}

/// W2-β2b (Q5 cadence consumption): whether the `#59` periodic tick
/// should fetch this source now. `Some(0)` pins the source static
/// (never scheduled); `None` inherits the batch tick (always due);
/// `Some(m)` is due only after `m` minutes since the last successful
/// fetch — a source never fetched (daemon restart) is due.
fn due_for_scheduled_refresh(
    every_minutes: Option<u64>,
    last: Option<&std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    match every_minutes {
        Some(0) => false,
        Some(minutes) => {
            let interval = std::time::Duration::from_secs(minutes.saturating_mul(60));
            last.is_none_or(|since| now.duration_since(*since) >= interval)
        }
        None => true,
    }
}

/// Resolves every enabled subscription source: `CALY_SUBSCRIPTION_URL` first
/// (single-source override), then the layered config (`subscriptions.url` plus
/// each enabled `subscriptions.sources` entry). Each URL maps to a stable
/// `SubscriptionId` derived from its SHA-256 digest, so the same URL always
/// targets the same cache entry across restarts; re-evaluated per refresh.
/// A present-but-broken `config.yaml` is a hard error: swallowing it used to
/// bury the parse failure behind a misleading "no subscription source" hint.
fn resolve_sources() -> Result<Vec<ResolvedSource>, ActorFailure> {
    let mut urls: Vec<(String, Option<u64>)> = Vec::new();
    if let Ok(value) = std::env::var("CALY_SUBSCRIPTION_URL") {
        urls.push((value, None));
    } else {
        let app_paths = AppPaths::from_env();
        let root = app_paths.config.clone();
        if root.join("config.yaml").is_file() {
            let profile = std::env::var("CALY_PROFILE")
                .ok()
                .and_then(|v| SafeName::new(v).ok());
            let paths = LayeredConfigPaths::new(root, profile);
            let config =
                load_layered_yaml_strict(&paths, LoaderLimits::secure_default(), &app_paths.state)
                    .map_err(|error| {
                        crate::failure(
                            &format!("config load failed while resolving subscriptions: {error}"),
                            "run `caly config validate` and fix the configuration",
                        )
                    })?;
            // W2-β2b: keep the per-source cadence alongside the URL so
            // the scheduled-refresh filter can gate on it; the legacy
            // singular `subscriptions.url` has no per-source fields.
            urls.extend(
                config
                    .subscriptions
                    .sources
                    .iter()
                    .filter(|source| source.enabled)
                    .map(|source| (source.url.clone(), source.refresh_every_minutes)),
            );
            urls.extend(
                config
                    .subscriptions
                    .enabled_source_urls()
                    .into_iter()
                    .filter(|url| {
                        !config
                            .subscriptions
                            .sources
                            .iter()
                            .any(|source| source.enabled && &source.url == url)
                    })
                    .map(|url| (url, None)),
            );
        }
    }
    let mut out: Vec<ResolvedSource> = Vec::new();
    let mut seen: std::collections::BTreeSet<SubscriptionId> = std::collections::BTreeSet::new();
    for (url, every) in urls {
        let id = subscription_id_for_url(&url);
        if seen.insert(id) {
            out.push(ResolvedSource {
                id,
                url,
                every_minutes: every,
            });
        }
    }
    Ok(out)
}

/// W2-β2b: one enabled subscription source plus its per-source
/// refresh cadence (`refresh_every_minutes`; `None` inherits the
/// batch timer cadence, `Some(0)` pins the source static).
struct ResolvedSource {
    id: SubscriptionId,
    url: String,
    every_minutes: Option<u64>,
}

#[cfg(test)]
mod tests;
