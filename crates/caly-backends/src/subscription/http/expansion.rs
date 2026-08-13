//! URL-list subscription expansion: a plain-text body whose lines are
//! subscription URLs is fetched child-by-child (same SSRF-safe path) and
//! merged before node parsing.

use caly_domain::SubscriptionId;
use caly_ports::ActorFailure;
use caly_subscription::{decode_document, FetchResult, FetchValidators, SubscriptionDocument};

use super::{resolve_addresses, HttpSubscriptionBackend, FETCH_ATTEMPTS, FETCH_RETRY_MILLIS};

/// Maximum child subscriptions expanded from one URL-list document.
const MAX_URL_LIST_CHILDREN: usize = 32;

impl HttpSubscriptionBackend {
    /// Expands a plain-text URL-list subscription: fetches each child
    /// subscription through the same SSRF-safe path and merges the bodies so
    /// downstream parsing sees one URI-line/base64/Clash document. Returns
    /// whether any child delivered updated content (a fresh child means the
    /// node set may have moved even when the parent answered 304).
    pub(super) fn expand_url_list(
        &mut self,
        id: SubscriptionId,
        touched: &mut std::collections::BTreeSet<SubscriptionId>,
    ) -> Result<bool, ActorFailure> {
        let Some(body) = self.cache.raw_source(id) else {
            return Ok(false);
        };
        // Not a URL list (or undecodable): leave the body untouched so the
        // regular parse path reports it.
        let Ok(SubscriptionDocument::UrlList(lines)) = decode_document(body) else {
            return Ok(false);
        };
        if lines.len() > MAX_URL_LIST_CHILDREN {
            return Err(crate::failure(
                "URL-list subscription has too many entries",
                "reduce the list to at most 32 URLs",
            ));
        }
        let mut merged: Vec<u8> = Vec::new();
        let mut changed = false;
        for line in &lines {
            let (child, child_changed) = self.fetch_child(line.as_str(), touched)?;
            changed |= child_changed;
            // Audit #112: 32 children × 32 MiB each used to build a ~1 GiB
            // merged body in memory, then be parsed down to a
            // `BodyTooLarge`-shaped failure anyway. Enforce the document
            // ceiling incrementally, while the merge is cheap to stop.
            let projected = merged.len().saturating_add(child.len()).saturating_add(1);
            if projected > caly_subscription::MAX_SUBSCRIPTION_BODY_BYTES {
                return Err(crate::failure(
                    "merged URL-list subscription exceeds the subscription body ceiling",
                    "reduce the number or size of the listed subscriptions",
                ));
            }
            merged.extend_from_slice(&child);
            merged.push(b'\n');
        }
        if let Ok(SubscriptionDocument::UrlList(_)) = decode_document(clone_prefix(&merged)) {
            return Err(crate::failure(
                "nested URL-list subscriptions are not supported",
                "point the subscription at a single URL list level",
            ));
        }
        self.cache.put_source(id, merged);
        Ok(changed)
    }

    /// Fetches one child subscription body with the shared retry policy.
    ///
    /// Children get the same conditional-request treatment as top-level
    /// sources, keyed by the derived `SubscriptionId` of their URL. Their
    /// bodies are cached **memory-only** (never persisted), so a daemon
    /// restart cannot mistake a merged child for a top-level subscription.
    /// A `304 NotModified` therefore reuses the previously cached child body
    /// instead of merging an empty document. The `bool` is the
    /// "content updated" flag (false for 304).
    fn fetch_child(
        &mut self,
        source: &str,
        touched: &mut std::collections::BTreeSet<SubscriptionId>,
    ) -> Result<(Vec<u8>, bool), ActorFailure> {
        let child_id = super::subscription_id_for_url(source);
        touched.insert(child_id);
        let addresses = resolve_addresses(source)?;
        let mut last = crate::failure("child subscription fetch failed", "retry the refresh");
        for index in 0..FETCH_ATTEMPTS {
            let validators = self.cache.validators(child_id);
            match self.request_body(source, &addresses, &validators) {
                Ok(FetchResult::Updated {
                    body,
                    etag,
                    last_modified,
                    ..
                }) => {
                    let body = body.into_vec();
                    self.cache.put_source_memory(child_id, body.clone());
                    self.cache.put_validators(
                        child_id,
                        FetchValidators {
                            etag,
                            last_modified,
                        },
                    );
                    return Ok((body, true));
                }
                Ok(FetchResult::NotModified) => {
                    // Reuse the previously cached child body; sending
                    // validators without a cached body cannot happen (the
                    // validators only exist after an Updated response), but
                    // a missing entry degrades to an empty merge input rather
                    // than fabricating content.
                    return Ok((self.cache.raw_source(child_id).unwrap_or_default(), false));
                }
                Err((transient, error)) => {
                    last = error;
                    if !transient || index + 1 >= FETCH_ATTEMPTS {
                        return Err(last);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(FETCH_RETRY_MILLIS));
                }
            }
        }
        Err(last)
    }
}

/// Audit #112: the nested-list *probe* only needs a bounded slice — the
/// pre-fix code cloned the whole merged body (up to the document ceiling)
/// for a yes/no detection.
fn clone_prefix(merged: &[u8]) -> Vec<u8> {
    merged[..merged.len().min(64 * 1_024)].to_vec()
}
