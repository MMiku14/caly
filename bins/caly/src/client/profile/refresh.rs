//! Cache refresh logic for the `caly profile` subcommand.
//!
//! Split out of `client/profile.rs` (audit #70 file-length
//! budget): everything here walks the declared profile list and
//! materialises `Remote` bodies through the SSRF-safe
//! [`caly_profile::profile_fetch::fetch_profile_body`] pipeline.
//! `Merge` profiles are walked depth-first so a merge's
//! `Remote` dependencies refresh before the merge itself is
//! reported done.

use caly_domain::Profile;
use caly_platform::paths::AppPaths;
use caly_profile::{
    profile_store::ProfileStore,
    schema::{AppConfig, ProfileSourceConfig},
};

use super::super::resource_writer::current_unix_ms;
use super::{ProfileCmdError, find_declared, load_declared_profiles};

/// Fetches one `Remote` profile body through the SSRF-safe
/// pipeline and materialises it in the on-disk cache. `Local`
/// and `Merge` profiles are not network-touching; the
/// caller is expected to skip them.
pub(crate) fn refresh_one(store: &ProfileStore, profile: &Profile) -> Result<(), ProfileCmdError> {
    use caly_profile::profile_fetch::{
        ProfileFetchError, fetch_profile_body, is_transient_profile_error,
    };
    use caly_subscription::{FetchPolicy, FetchValidators};
    let url = match &profile.source {
        caly_domain::ProfileSource::Remote { url, .. } => url.as_str(),
        caly_domain::ProfileSource::Local { .. } | caly_domain::ProfileSource::Merge { .. } => {
            // Non-network profiles are no-ops at the fetcher level.
            return Ok(());
        }
    };
    let policy = FetchPolicy::direct_default();
    // Conditional request metadata from the on-disk metadata sidecar —
    // only when a cached body actually exists, so a `304` can never be
    // interpreted against an empty cache.
    let validators = match store.read_metadata(profile.id.as_str())? {
        Some(metadata) if store.read(profile.id.as_str())?.is_some() => FetchValidators {
            etag: metadata
                .etag
                .and_then(|value| caly_domain::BoundedText::new(value).ok()),
            last_modified: metadata
                .last_modified
                .and_then(|value| caly_domain::BoundedText::new(value).ok()),
        },
        _ => FetchValidators::default(),
    };
    // The CLI runs synchronously, so `Handle::try_current` has nothing to
    // return here and the fetch used to fail closed with a misleading
    // "ClientBuild". Create a private current-thread runtime on demand
    // instead (cheap: one fetch per process invocation).
    let owned_runtime;
    let handle = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle
    } else {
        owned_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ProfileCmdError::Fetch(ProfileFetchError::ClientBuild))?;
        owned_runtime.handle().clone()
    };
    // Bounded retry loop: 2 attempts on transient failures,
    // 250 ms backoff. Mirrors the subscription backend's
    // pattern so the operator-facing diagnostics stay
    // consistent across fetches.
    let mut last_transient: Option<ProfileFetchError> = None;
    for attempt in 0..2 {
        let outcome = handle.block_on(fetch_profile_body(
            url,
            &caly_subscription::PublicAddressClassifier,
            policy,
            &validators,
        ));
        match outcome {
            Ok(caly_profile::profile_fetch::ProfileFetchOutcome::Updated { body, validators }) => {
                store.write_full(
                    profile.id.as_str(),
                    &body,
                    url,
                    current_unix_ms(),
                    validators.etag.map(|value| value.as_str().to_owned()),
                    validators
                        .last_modified
                        .map(|value| value.as_str().to_owned()),
                )?;
                return Ok(());
            }
            Ok(caly_profile::profile_fetch::ProfileFetchOutcome::NotModified) => {
                // ETag matched; the body on disk is still the truth. Re-write
                // the metadata so the operator sees a fresh `fetched_at_ms`
                // without touching the body — but never *fabricate* a cache
                // body: a 304 against an empty cache is a no-op, not a reason
                // to materialise an empty file.
                if let Some(cached) = store.read(profile.id.as_str())? {
                    store.write(profile.id.as_str(), &cached, url, current_unix_ms())?;
                }
                return Ok(());
            }
            Err(error) if is_transient_profile_error(&error) && attempt + 1 < 2 => {
                last_transient = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(error) => return Err(ProfileCmdError::Fetch(error)),
        }
    }
    Err(ProfileCmdError::Fetch(
        last_transient.unwrap_or(ProfileFetchError::ClientBuild),
    ))
}

/// Refreshes the cache for every `kind: remote` profile.
/// `Merge` profiles are walked depth-first: each `Remote`
/// `parts` entry is refreshed before the merge itself, and
/// a single failing dependency surfaces as
/// [`ProfileCmdError::FetchChain`] with the offending id.
/// `Local` profiles are no-ops.
pub fn refresh_all(paths: &AppPaths, id: Option<&str>) -> Result<usize, ProfileCmdError> {
    let (config, store) = load_declared_profiles(paths)?;
    if let Some(target) = id {
        if !config.profiles.iter().any(|profile| profile.id == target) {
            return Err(ProfileCmdError::NotDeclared(target.to_owned()));
        }
        // Single-target refresh path: walk the (possible)
        // merge tree so a `Merge` `target` triggers a full
        // chain refresh. The outermost id is `target`
        // itself; a nested failure surfaces with
        // `FetchChain { id: target, source: ... }`.
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let count = refresh_recursive(&config, &store, target, target, &mut visited)?;
        return Ok(count);
    }
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut count = 0;
    let now_ms = current_unix_ms();
    for profile in &config.profiles {
        // Local bodies are always fresh. Remote *and* Merge profiles are
        // walked — the merge walk refreshes every Remote dependency first
        // (a bare `caly set profile refresh` used to skip top-level Merge
        // entries entirely, leaving their bodies stale).
        if matches!(profile.source, ProfileSourceConfig::Local { .. }) {
            continue;
        }
        // #60: a top-level Remote profile honours its own
        // `interval_minutes`: a cache entry younger than the
        // interval is reused instead of re-downloaded (the
        // bulk refresh is the schedule-driving path — explicit
        // single-target refresh above always refetches). A
        // Merge is always walked: its Remote dependencies each
        // apply their own interval inside the recursion.
        if let ProfileSourceConfig::Remote {
            interval_minutes, ..
        } = &profile.source
            && cache_entry_fresh(
                store.read_metadata(&profile.id).ok().flatten().as_ref(),
                *interval_minutes,
                now_ms,
            )
        {
            continue;
        }
        count += refresh_recursive(&config, &store, &profile.id, &profile.id, &mut visited)?;
    }
    Ok(count)
}

/// #60: is the cache metadata younger than the profile's
/// declared `interval_minutes`? `None` metadata (nothing
/// fetched yet) is never fresh; an interval of `0`
/// conservatively counts as "always refresh" so a misdeclared
/// cadence can never freeze a profile; a future-dated
/// `fetched_at_ms` (clock moved backwards) counts as stale so a
/// skewed cache cannot pin a profile forever.
pub(super) fn cache_entry_fresh(
    metadata: Option<&caly_profile::profile_store::ProfileCacheEntry>,
    interval_minutes: u32,
    now_ms: u64,
) -> bool {
    let Some(metadata) = metadata else {
        return false;
    };
    let interval_ms = u64::from(interval_minutes).saturating_mul(60_000);
    if interval_ms == 0 || metadata.fetched_at_ms > now_ms {
        // Zero means "no cadence declared" (always refetch); a
        // future-dated stamp means the clock moved — refetch too,
        // so a skewed cache can never pin a profile forever.
        return false;
    }
    now_ms - metadata.fetched_at_ms < interval_ms
}

/// Recursive depth-first refresh. Walks `Merge` parts
/// recursively, fetches `Remote` bodies through the
/// SSRF-safe pipeline, and skips `Local` profiles (they
/// are always-fresh). Returns the number of `Remote`
/// profiles fetched (a `Merge` itself is a no-op for
/// the fetcher). `visited` breaks cycles for malformed
/// merge trees (the schema validator already rejects
/// them, but the runtime guard is independent).
///
/// A single failing dependency surfaces as
/// [`ProfileCmdError::FetchChain`] with the **outermost
/// merge** id (the user's declared entry point into
/// the merge tree). The internal recursion passes the
/// outermost id through every nested frame, so a
/// three-level merge `outer → inner → leaf-bad` labels
/// the error with `outer`, not `inner`. The operator
/// identifies which declared `Merge` reached into the
/// broken dependency at a glance, without walking the
/// parts tree.
fn refresh_recursive(
    config: &AppConfig,
    store: &ProfileStore,
    id: &str,
    outer_id: &str,
    visited: &mut std::collections::HashSet<String>,
) -> Result<usize, ProfileCmdError> {
    if !visited.insert(id.to_owned()) {
        return Ok(0);
    }
    let Some(profile) = find_declared(config, id) else {
        return Err(ProfileCmdError::NotDeclared(id.to_owned()));
    };
    let source = profile.source.clone();
    match &source {
        ProfileSourceConfig::Local { .. } => Ok(0),
        ProfileSourceConfig::Remote {
            interval_minutes, ..
        } => {
            // #60: inside a merge walk, a Remote part whose
            // cached body is younger than its interval is
            // reused — an explicit `refresh <id>` on a Merge
            // would otherwise hammer every dependency's origin
            // on every call.
            if cache_entry_fresh(
                store.read_metadata(id).ok().flatten().as_ref(),
                *interval_minutes,
                current_unix_ms(),
            ) {
                return Ok(0);
            }
            let domain = profile.to_domain().map_err(ProfileCmdError::ParseConfig)?;
            refresh_one(store, &domain)?;
            Ok(1)
        }
        ProfileSourceConfig::Merge { parts } => {
            // Refresh every `Remote` (and recursively, every
            // nested `Merge`) before declaring the merge done.
            // `Local` parts are always-fresh; skip them.
            //
            // The `outer_id` is threaded unchanged through the
            // recursion so any error surfaced from a nested
            // frame is attributed to the user-visible entry
            // point (the outermost declared `Merge`).
            let mut fetched = 0;
            for part in parts {
                match refresh_recursive(config, store, part, outer_id, visited) {
                    Ok(n) => fetched += n,
                    Err(error) => {
                        return Err(match error {
                            ProfileCmdError::Fetch(source) => ProfileCmdError::FetchChain {
                                id: outer_id.to_owned(),
                                source,
                            },
                            other => other,
                        });
                    }
                }
            }
            Ok(fetched)
        }
    }
}
