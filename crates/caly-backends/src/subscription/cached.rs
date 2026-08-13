//! Bounded raw subscription source cache with generation tracking.

use crate::core::{MihomoNodeRegistry, RegisteredProxy};
use caly_domain::{SnapshotNodes, SubscriptionId};
use caly_platform::fs::{
    AtomicFileContents, AtomicWritePlan, LinuxAtomicFileBackend, atomic_write,
};
use caly_ports::{ActorFailure, RefreshOutcome, SubscriptionCommandBackend};
use caly_subscription::{FetchValidators, SubscriptionProjection};

use super::render_compose::mihomo_proxy_set_from_document;

use super::hex_id;

/// Bounded raw subscription source cache with generation tracking.
pub struct CachedSubscriptionBackend {
    sources: std::collections::BTreeMap<SubscriptionId, Vec<u8>>,
    generations: std::collections::BTreeMap<SubscriptionId, u64>,
    userinfo: std::collections::BTreeMap<SubscriptionId, caly_subscription::SubscriptionUserInfo>,
    /// Last seen HTTP validators (ETag / Last-Modified) per source, fed back
    /// into the next conditional request. Memory-only: validators are a
    /// bandwidth optimisation, so a daemon restart simply re-fetches.
    validators: std::collections::BTreeMap<SubscriptionId, FetchValidators>,
    /// Ids whose bodies are memory-only (URL-list children): they are not
    /// persisted into `cache_dir`, so a daemon restart never re-projects a
    /// merged child body as a standalone top-level subscription.
    memory_only: std::collections::BTreeSet<SubscriptionId>,
    cache_dir: Option<std::path::PathBuf>,
    filesystem: LinuxAtomicFileBackend,
    node_registry: Option<MihomoNodeRegistry>,
    /// Subscription-author routing registry (groups + rules) indexed
    /// alongside the nodes; `None` keeps the implicit-group legacy path.
    routing_registry: Option<crate::core::CoreRoutingRegistry>,
}

impl CachedSubscriptionBackend {
    /// Creates an empty source cache.
    /// Drops every cache entry whose subscription id is neither declared nor
    /// touched by the most recent refresh (刀 6, 2026-08-12 memory audit):
    /// a removed subscription must release its bodies, validators, projections
    /// and node/routing registrations instead of accumulating for the daemon's
    /// lifetime — this was the only unbounded growth point in the subsystem.
    pub fn retain_declared(
        &mut self,
        declared: &std::collections::BTreeSet<SubscriptionId>,
        touched: &std::collections::BTreeSet<SubscriptionId>,
    ) {
        let keep = |id: &SubscriptionId| declared.contains(id) || touched.contains(id);
        self.sources.retain(|id, _| keep(id));
        self.generations.retain(|id, _| keep(id));
        self.userinfo.retain(|id, _| keep(id));
        self.validators.retain(|id, _| keep(id));
        // URL-list children live memory-only, keyed by derived ids that are
        // never declared; keep only the ones the refresh actually touched.
        self.memory_only.retain(|id| touched.contains(id));
        // Released subscriptions must also release their node and routing
        // registrations, or the registry keeps serving stale nodes/rules.
        if let Some(registry) = &self.node_registry
            && let Ok(mut mapping) = registry.lock()
        {
            mapping.retain(|_, entry| declared.contains(&entry.subscription));
        }
        if let Some(registry) = &self.routing_registry
            && let Ok(mut mapping) = registry.lock()
        {
            // The routing registry is keyed BY subscription id.
            mapping.retain(|id, _| declared.contains(id));
        }
    }

    pub fn new() -> Self {
        Self {
            sources: std::collections::BTreeMap::new(),
            generations: std::collections::BTreeMap::new(),
            userinfo: std::collections::BTreeMap::new(),
            validators: std::collections::BTreeMap::new(),
            memory_only: std::collections::BTreeSet::new(),
            cache_dir: None,
            filesystem: LinuxAtomicFileBackend,
            node_registry: None,
            routing_registry: None,
        }
    }

    /// Enables a persistent raw-body cache directory.
    #[must_use]
    pub fn with_cache_dir(mut self, cache_dir: std::path::PathBuf) -> Self {
        self.cache_dir = Some(cache_dir);
        self
    }

    /// Returns the configured persistent raw-body cache directory.
    pub fn cache_dir(&self) -> Option<&std::path::Path> {
        self.cache_dir.as_deref()
    }

    /// Re-indexes every subscription body persisted in the cache directory,
    /// restoring the node registry after a daemon restart without re-fetching.
    /// Cached bodies are best-effort: a corrupt or unreadable entry is skipped
    /// with a warning rather than failing the daemon bootstrap. Returns the
    /// restored projection slices so the caller can seed the presentation
    /// snapshot with the same names the registry now serves.
    pub fn restore_from_cache(
        &mut self,
        declared: &std::collections::BTreeSet<caly_domain::SubscriptionId>,
    ) -> Result<Vec<SnapshotNodes>, ActorFailure> {
        let Some(directory) = self.cache_dir.clone() else {
            return Ok(Vec::new());
        };
        let mut restored = Vec::new();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Round 31: a fresh install that
                // has not yet cached anything —
                // the canonical "first boot"
                // condition. The pre-Round 31
                // shape logged the same
                // "unreadable" warning on every
                // fresh boot, which is operator
                // noise that hides the real
                // "your state directory is
                // locked" failure when one
                // happens. A missing directory
                // now returns an empty `Ok` so
                // the daemon bootstrap continues
                // normally; the directory is
                // synthesised on the first
                // `put_source` (see
                // [`Self::persist_source`]).
                return Ok(Vec::new());
            }
            Err(error) => {
                return Err(crate::failure(
                    &format!("subscription cache directory is unreadable: {error}"),
                    "inspect state-directory ownership and that the parent is not a symlink to a read-only mount",
                ));
            }
        };
        let mut skipped_ghosts = 0_usize;
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // These are exact suffix markers for files we generate ourselves
            // (commit-tmp / rollback), not user file extensions, so matching is
            // intentionally case-sensitive to the lowercase literals we write.
            #[allow(clippy::case_sensitive_file_extension_comparisons)]
            if name.ends_with(".tmp") || name.ends_with(".rollback") {
                // Audit #91: reap our own crashed-write leftovers instead of
                // skipping them forever — they are never valid restore
                // candidates and otherwise accumulate without bound.
                if let Err(error) = std::fs::remove_file(entry.path()) {
                    tracing::warn!(file = %name, %error, "cannot reap a stale subscription cache staging file");
                }
                continue;
            }
            let Ok(id) = name.parse::<SubscriptionId>() else {
                continue;
            };
            if !declared.contains(&id) {
                // Ghost cache: not declared in config.yaml — restoring it
                // would render nodes the operator cannot list or manage.
                skipped_ghosts += 1;
                continue;
            }
            match self.refresh_projection(id) {
                Ok(projection) => restored.push(projection.nodes),
                Err(error) => {
                    // Audit #91: route restore diagnostics through tracing
                    // like every other daemon path (was `eprintln!`).
                    //
                    // 2026-08-12: an un-restorable body (the source answered
                    // an error page, e.g. `error code: 520`, which got cached)
                    // is discarded so the warning does not repeat on every
                    // boot — the next refresh fetches a fresh body.
                    tracing::warn!(
                        subscription = %id,
                        reason = %error.message,
                        hint = %error.suggested_action,
                        "cached subscription body is not restorable; discarding the stale body (the next refresh fetches it fresh)"
                    );
                    if let Err(remove_error) = std::fs::remove_file(entry.path()) {
                        tracing::warn!(
                            file = %name,
                            %remove_error,
                            "cannot discard the stale subscription cache body"
                        );
                    }
                }
            }
        }
        if skipped_ghosts > 0 {
            tracing::warn!(
                skipped = skipped_ghosts,
                "cached subscription body(s) are not declared in config.yaml; \
                 skipped at restore — run `caly sub add` to re-declare, or delete \
                 the cache files under $STATE/caly/subscriptions/ to drop them"
            );
        }
        Ok(restored)
    }

    /// Shares an automatic NodeId → Mihomo group/node registry.
    #[must_use]
    pub fn with_node_registry(mut self, registry: MihomoNodeRegistry) -> Self {
        self.node_registry = Some(registry);
        self
    }

    /// Shares the subscription-author routing registry (2026-08-09 规划:
    /// subscription-declared `proxy-groups` become the rendered topology
    /// when present, replacing the implicit `url-test` fallback group).
    #[must_use]
    pub fn with_routing_registry(mut self, registry: crate::core::CoreRoutingRegistry) -> Self {
        self.routing_registry = Some(registry);
        self
    }

    /// Installs fetched raw bytes and persists them when a cache directory is configured.
    pub fn put_source(&mut self, id: SubscriptionId, body: Vec<u8>) {
        self.memory_only.remove(&id);
        self.persist_source(id, &body);
        self.sources.insert(id, body);
    }

    /// Installs fetched raw bytes in memory only (URL-list children). The
    /// body is available for `raw_source` lookups during this daemon
    /// lifetime but is never written into `cache_dir`, so a restart cannot
    /// mistake a merged child for a top-level subscription.
    pub fn put_source_memory(&mut self, id: SubscriptionId, body: Vec<u8>) {
        self.memory_only.insert(id);
        self.sources.insert(id, body);
    }

    /// Records the HTTP validators returned by the last successful fetch so
    /// the next refresh issues a conditional `If-None-Match` request.
    pub fn put_validators(&mut self, id: SubscriptionId, validators: FetchValidators) {
        self.validators.insert(id, validators);
    }

    /// W2-β2b (`sub refresh --force`): drops the stored validators so
    /// the next fetch is unconditional (no If-None-Match /
    /// If-Modified-Since), forcing a full body re-download.
    pub fn clear_validators(&mut self, id: SubscriptionId) {
        self.validators.remove(&id);
    }

    /// Returns the last recorded validators for conditional revalidation
    /// (empty on the first fetch or after a restart).
    pub fn validators(&self, id: SubscriptionId) -> FetchValidators {
        self.validators
            .get(&id)
            .cloned()
            .unwrap_or(FetchValidators {
                etag: None,
                last_modified: None,
            })
    }

    /// Records the latest subscription quota metadata (from the fetch header).
    pub fn put_userinfo(
        &mut self,
        id: SubscriptionId,
        userinfo: caly_subscription::SubscriptionUserInfo,
    ) {
        self.userinfo.insert(id, userinfo);
    }

    /// Returns the last subscription quota metadata, or all-None.
    pub fn userinfo(&self, id: SubscriptionId) -> caly_subscription::SubscriptionUserInfo {
        self.userinfo
            .get(&id)
            .copied()
            .unwrap_or(caly_subscription::SubscriptionUserInfo {
                upload_bytes: None,
                download_bytes: None,
                total_bytes: None,
                expire_unix: None,
            })
    }

    fn persist_source(&mut self, id: SubscriptionId, body: &[u8]) {
        let Some(directory) = &self.cache_dir else {
            return;
        };
        // Audit #91: persistence failures (ENOSPC, EACCES, oversized merged
        // bodies) used to vanish into `let _ = …`, so a reboot silently lost
        // the cached body with the operator none the wiser. Persisting is
        // still best-effort — but loud now.
        if let Err(error) = std::fs::create_dir_all(directory) {
            tracing::warn!(
                subscription = %id,
                %error,
                "cannot create the subscription cache directory; body not persisted"
            );
            return;
        }
        let Ok(contents) = AtomicFileContents::try_from_vec(body.to_vec()) else {
            tracing::warn!(
                subscription = %id,
                bytes = body.len(),
                "subscription body exceeds the atomic-file ceiling; body not persisted"
            );
            return;
        };
        let destination = directory.join(hex_id(id));
        let temporary = directory.join(format!("{}.tmp", hex_id(id)));
        if let Err(error) = atomic_write(
            &mut self.filesystem,
            AtomicWritePlan {
                destination,
                temporary,
                contents,
            },
        ) {
            tracing::warn!(
                subscription = %id,
                %error,
                "subscription body persist failed; the next boot needs a re-fetch"
            );
        }
    }

    /// Returns cached raw bytes for controlled outbound rendering.
    pub fn raw_source(&self, id: SubscriptionId) -> Option<Vec<u8>> {
        self.sources.get(&id).cloned().or_else(|| {
            self.cache_dir
                .as_ref()
                .and_then(|directory| std::fs::read(directory.join(hex_id(id))).ok())
        })
    }

    /// Returns the committed source generation.
    pub fn generation(&self, id: SubscriptionId) -> u64 {
        self.generations.get(&id).copied().unwrap_or(0)
    }

    /// Parses, normalizes, deduplicates, projects and commits one generation.
    pub fn refresh_projection(
        &mut self,
        id: SubscriptionId,
    ) -> Result<SubscriptionProjection, ActorFailure> {
        let body = self.cached_body(id)?;
        // Single-pass pipeline (2026-08-12 refactor): the body is decoded
        // ONCE and the document is shared across the projection, Mihomo
        // render, sing-box render and routing consumers — the pre-refactor
        // shape decoded the same body up to four times per refresh.
        let document = caly_subscription::decode_document(body).map_err(|error| {
            crate::failure(
                &format!("subscription pipeline failed: {error:?}"),
                "inspect source format and URI diagnostics",
            )
        })?;
        let projection = caly_subscription::parse_document_to_display_lossy(&document, id)
            .map_err(|error| {
                crate::failure(
                    &format!("subscription pipeline failed: {error}"),
                    "inspect source format and URI diagnostics",
                )
            })?;
        if let Some(registry) = &self.node_registry {
            index_subscription_snapshot(registry, self.routing_registry.as_ref(), &document, id);
        }
        self.generations
            .insert(id, self.generation(id).saturating_add(1));
        Ok(projection)
    }

    fn cached_body(&self, id: SubscriptionId) -> Result<Vec<u8>, ActorFailure> {
        self.sources
            .get(&id)
            .cloned()
            .or_else(|| {
                self.cache_dir
                    .as_ref()
                    .and_then(|directory| std::fs::read(directory.join(hex_id(id))).ok())
            })
            .ok_or_else(|| {
                crate::failure(
                    "subscription source is not cached",
                    "fetch the subscription before refresh",
                )
            })
    }
}

fn index_subscription_snapshot(
    registry: &MihomoNodeRegistry,
    routing_registry: Option<&crate::core::CoreRoutingRegistry>,
    document: &caly_subscription::SubscriptionDocument,
    id: SubscriptionId,
) {
    // Rendering is best-effort; a mixed or unsupported subscription leaves the
    // registry unchanged rather than failing the whole refresh.
    let Ok(set) = mihomo_proxy_set_from_document(document, id) else {
        return;
    };
    // sing-box outbound JSON for the same body, keyed by canonical NodeId, so
    // the sing-box config backend renders from the registry without re-parsing.
    // Nodes the strict renderer cannot represent are warned here with the
    // protocol reason — a silently shrinking pool would only resurface later
    // as opaque group-drop warnings at `config apply` time.
    let (singbox, skipped) =
        super::render_compose::sing_box_outbound_map_from_document(document, id)
            .unwrap_or_default();
    for skip in &skipped {
        tracing::warn!(
            subscription = %id,
            node = %skip.tag,
            protocol = skip.protocol,
            "subscription node has no sing-box outbound; skipped (protocol unsupported by the sing-box renderer)"
        );
    }
    if skipped.len() > 5 {
        tracing::warn!(
            subscription = %id,
            total = skipped.len(),
            "more subscription nodes skipped for sing-box; the pool will be smaller under the sing-box core"
        );
    }
    // 2026-08-09 规划: a Clash document that declares `proxy-groups` owns the
    // routing; its groups are indexed for verbatim rendering and each node
    // lands in the group the author placed it in (the first `select` group,
    // falling back to any containing group), so core selection keeps working
    // unchanged through `RegisteredProxy.landing_group`.
    let routing = caly_subscription::clash_routing_from_document(document, id);
    if let Ok(mut mapping) = registry.lock() {
        // A refresh replaces the previous generation of this subscription:
        // drop stale entries (an older cached body or a changed source that
        // produced different NodeIds) before indexing the new document, so
        // duplicate proxy names can never accumulate across generations.
        mapping.retain(|_, entry| entry.subscription != id);
        for entry in set.entries() {
            mapping.insert(
                entry.id,
                RegisteredProxy {
                    landing_group: selection_group(
                        routing.as_ref(),
                        entry.tag.as_str(),
                        set.group(),
                    ),
                    name: entry.tag.as_str().to_owned(),
                    yaml: entry.yaml.as_str().to_owned(),
                    singbox: singbox.get(&entry.id).cloned(),
                    subscription: id,
                },
            );
        }
    }
    if let Some(store) = routing_registry
        && let Ok(mut stored) = store.lock()
    {
        match routing {
            Some((groups, rules)) => {
                stored.insert(id, crate::core::SubscriptionRouting { groups, rules });
            }
            // No author routing (URI list, unparseable document, or a
            // group-less Clash body): any stale generation must go, or
            // a subscription that *lost* its groups would keep routing
            // traffic through the previous document's topology.
            None => {
                stored.remove(&id);
            }
        }
    }
}

/// Resolves the kernel group a node's core-selection call should address:
/// the first `select` group the subscription author placed the node in,
/// then any containing group, then the implicit fallback name.
fn selection_group(
    routing: Option<&(Vec<caly_domain::ProxyGroup>, Vec<caly_domain::RoutingRule>)>,
    tag: &str,
    fallback: &str,
) -> String {
    let Some((groups, _)) = routing else {
        return fallback.to_owned();
    };
    let contains = |group: &caly_domain::ProxyGroup| {
        group.members.iter().any(|member| {
            matches!(member, caly_domain::ProxyGroupMember::Node { tag: member_tag } if member_tag.as_str() == tag)
        })
    };
    groups
        .iter()
        .find(|group| matches!(group.kind, caly_domain::ProxyGroupType::Select) && contains(group))
        .or_else(|| groups.iter().find(|group| contains(group)))
        .map_or_else(
            || fallback.to_owned(),
            |group| group.name.as_str().to_owned(),
        )
}

impl Default for CachedSubscriptionBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionCommandBackend for CachedSubscriptionBackend {
    /// The cache half implements the port for tests/direct consumers;
    /// the network half (`HttpSubscriptionBackend`) consumes the
    /// mode itself and drives [`Self::refresh_projection`] directly,
    /// so the mode is intentionally ignored here — there is no
    /// fetch to force and no schedule to gate in a pure projection.
    /// `changed` is conservative true: the pure projection cannot see
    /// the network, so a consumer that re-renders the kernel config on
    /// `changed` stays correct (the HTTP half is the one that reports
    /// precise 304/updated signals).
    fn refresh(
        &mut self,
        subscription: SubscriptionId,
        _mode: caly_ports::RefreshMode,
    ) -> Result<RefreshOutcome, ActorFailure> {
        self.refresh_projection(subscription)
            .map(|projection| RefreshOutcome {
                nodes: projection.nodes,
                changed: true,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CoreRoutingRegistry;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};

    /// Decodes a test body once, mirroring the single-pass pipeline entry.
    fn doc(body: Vec<u8>) -> caly_subscription::SubscriptionDocument {
        caly_subscription::decode_document(body).expect("test body must decode")
    }

    /// A minimal dialable body with one vless node; the port distinguishes
    /// node identity (NodeId hashes the dialable target, not the name).
    fn one_node(name: &str, port: u16) -> Vec<u8> {
        format!(
            "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:{port}?security=tls#{name}\n"
        )
        .into_bytes()
    }

    fn names(registry: &MihomoNodeRegistry) -> Vec<String> {
        let mut names: Vec<String> = registry
            .lock()
            .map(|mapping| mapping.values().map(|entry| entry.name.clone()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn refresh_replaces_previous_generation_of_same_subscription() {
        let registry: MihomoNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let id = SubscriptionId::from_bytes([7; 16]);
        index_subscription_snapshot(&registry, None, &doc(one_node("node-a", 8443)), id);
        assert_eq!(names(&registry), vec!["node-a"]);
        // Same subscription refreshed with a changed body: the old identity
        // must disappear instead of accumulating a duplicate proxy name.
        index_subscription_snapshot(&registry, None, &doc(one_node("node-b", 8444)), id);
        assert_eq!(names(&registry), vec!["node-b"]);
    }

    #[test]
    fn distinct_subscriptions_coexist() {
        let registry: MihomoNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let first = SubscriptionId::from_bytes([8; 16]);
        let second = SubscriptionId::from_bytes([9; 16]);
        index_subscription_snapshot(&registry, None, &doc(one_node("node-a", 8443)), first);
        index_subscription_snapshot(&registry, None, &doc(one_node("node-b", 8444)), second);
        assert_eq!(names(&registry), vec!["node-a", "node-b"]);
    }

    /// A Clash document with a `节点选择` select group plus a probe group:
    /// the routing store carries both, and every node lands in the first
    /// containing `select` group so `caly core select` works unchanged.
    fn grouped_body(probe_only: bool) -> Vec<u8> {
        let node = |name: &str| {
            format!(
                "  - name: {name}\n    type: vmess\n    server: {name}.example.net\n    port: 443\n    uuid: 6d5a3f10-5a2e-4a1b-9b0e-2a76b4f25a01\n"
            )
        };
        let mut body = String::from("proxies:\n");
        for name in ["node-a", "node-b"] {
            body.push_str(&node(name));
        }
        body.push_str(
            "proxy-groups:\n\
             \x20 - {name: 节点选择, type: select, proxies: [node-a, node-b]}\n",
        );
        if !probe_only {
            body.push_str(
                "  - {name: 自动选择, type: url-test, url: 'http://www.gstatic.com/generate_204', \
                 proxies: [node-a]}\n",
            );
        }
        body.push_str("rules:\n  - 'MATCH,节点选择'\n");
        body.into_bytes()
    }

    fn groups_of(registry: &MihomoNodeRegistry) -> Vec<String> {
        let mut groups: Vec<String> = registry
            .lock()
            .map(|mapping| {
                mapping
                    .values()
                    .map(|entry| entry.landing_group.clone())
                    .collect()
            })
            .unwrap_or_default();
        groups.sort();
        groups
    }

    #[test]
    fn subscription_groups_are_indexed_and_nodes_land_in_the_select_group() {
        let registry: MihomoNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let routing = crate::core::shared_routing_registry();
        let id = SubscriptionId::from_bytes([11; 16]);
        index_subscription_snapshot(&registry, Some(&routing), &doc(grouped_body(false)), id);
        assert_eq!(groups_of(&registry), vec!["节点选择", "节点选择"]);
        let stored = routing.lock().map(|map| map.len()).unwrap_or_default();
        assert_eq!(stored, 1);
        let (groups, rules) = routing
            .lock()
            .ok()
            .and_then(|map| {
                map.get(&id)
                    .map(|routing| (routing.groups.len(), routing.rules.len()))
            })
            .unwrap_or_default();
        assert_eq!((groups, rules), (2, 1));
    }

    #[test]
    fn refresh_replaces_routing_generation_and_losing_groups_clears_it() {
        let registry: MihomoNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let routing = crate::core::shared_routing_registry();
        let id = SubscriptionId::from_bytes([12; 16]);
        index_subscription_snapshot(&registry, Some(&routing), &doc(grouped_body(false)), id);
        // Same subscription refreshed with a document declaring only the
        // select group: the stale url-test entry must not linger.
        index_subscription_snapshot(&registry, Some(&routing), &doc(grouped_body(true)), id);
        let remaining = routing
            .lock()
            .ok()
            .and_then(|map| map.get(&id).map(|routing| routing.groups.len()))
            .unwrap_or_default();
        assert_eq!(remaining, 1);
        // And a body that loses its groups entirely clears the routing row,
        // so traffic can never route through yesterday's topology.
        index_subscription_snapshot(
            &registry,
            Some(&routing),
            &doc(one_node("node-a", 8443)),
            id,
        );
        assert_eq!(routing.lock().map_or(1, |map| map.len()), 0);
        assert_eq!(groups_of(&registry), vec!["AUTO"]);
    }

    #[test]
    fn node_absent_from_every_group_keeps_the_implicit_landing() {
        let registry: MihomoNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let routing = crate::core::shared_routing_registry();
        let id = SubscriptionId::from_bytes([13; 16]);
        // The author's group lists only node-a; node-b exists but no group
        // contains it, so it keeps the implicit-group landing for selection.
        let mut body = grouped_body(true);
        let stray = String::from(
            "  - name: node-c\n    type: vmess\n    server: node-c.example.net\n    port: 444\n    uuid: 6d5a3f10-5a2e-4a1b-9b0e-2a76b4f25a99\n",
        );
        let mut text = String::from_utf8_lossy(&body).into_owned();
        text = text.replacen("proxy-groups:", &format!("{stray}proxy-groups:"), 1);
        body = text.into_bytes();
        index_subscription_snapshot(&registry, Some(&routing), &doc(body), id);
        assert_eq!(groups_of(&registry), vec!["AUTO", "节点选择", "节点选择"]);
    }

    #[test]
    fn retain_declared_releases_removed_subscriptions() {
        let node_registry: MihomoNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let routing_registry: CoreRoutingRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        let mut cache = CachedSubscriptionBackend::default()
            .with_node_registry(Arc::clone(&node_registry))
            .with_routing_registry(Arc::clone(&routing_registry));
        let kept = SubscriptionId::from_bytes([1; 16]);
        let removed = SubscriptionId::from_bytes([2; 16]);
        cache.put_source(kept, b"kept body".to_vec());
        cache.put_source(removed, b"removed body".to_vec());
        cache.put_validators(
            removed,
            FetchValidators {
                etag: Some(caly_domain::BoundedText::new("e").expect("short etag fits the bound")),
                last_modified: None,
            },
        );
        // 刀 6 (memory audit): a removed subscription must release its
        // cache entries, validators, projections and registrations instead
        // of accumulating for the daemon's lifetime.
        let declared: BTreeSet<SubscriptionId> = [kept].into_iter().collect();
        let touched: BTreeSet<SubscriptionId> = BTreeSet::new();
        cache.retain_declared(&declared, &touched);
        assert!(cache.raw_source(kept).is_some(), "declared source stays");
        assert!(
            cache.raw_source(removed).is_none(),
            "removed source released"
        );
        assert!(
            !cache.validators.contains_key(&removed),
            "validators released"
        );
        // Projection of the removed subscription is gone from the registry.
        let registry = node_registry.lock().unwrap();
        assert!(
            registry
                .iter()
                .all(|(_, entry)| entry.subscription != removed),
            "removed subscription nodes released from the registry"
        );
    }
}
