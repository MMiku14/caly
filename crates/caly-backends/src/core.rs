//! Core command backend: proxy selection and connection close, unified by the
//! active core. Mirrors the `CoreLifecycleBackend` dispatch so every command
//! hits the kernel controller that is actually running.

use caly_corectl::{
    contract::KernelControl, mihomo::MihomoHttpControl, sing_box::SingBoxHttpControl,
};
use caly_domain::{
    AppliedState, CoreKind, CoreRunState, NodeId, ObservedState, ProxyMode, SubscriptionId,
};
use caly_ports::{ActorFailure, CoreCommandBackend};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Controller request budget for core commands.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// A subscription node indexed for both core selection and config rendering.
#[derive(Clone, Debug)]
pub struct RegisteredProxy {
    /// Clash proxy-group core-selection calls address for this node. With
    /// subscription-owned routing the node "lands" in the author's first
    /// containing `select` group (else any containing group, else the
    /// implicit fallback name); without subscription groups it is the
    /// implicit fallback group derived from the subscription body.
    pub landing_group: String,
    /// Clash proxy tag used by core selection and config rendering.
    pub name: String,
    /// Rendered `- name: ...` Mihomo YAML block (bounded upstream).
    pub yaml: String,
    /// sing-box outbound JSON object (tag `proxy-<id>`), when representable.
    /// Stored alongside the Mihomo YAML so both config backends render from
    /// the same registry without re-parsing the subscription body.
    pub singbox: Option<String>,
    /// Subscription the node came from, so a refresh replaces (not merges
    /// into) the previous generation of that subscription. Without this, a
    /// cached older body and a newer fetch with changed identities would
    /// accumulate duplicate proxy names and Mihomo would reject the render.
    pub subscription: SubscriptionId,
}

/// A source document's full routing surface (2026-08-09 规划:
/// "subscriptions own the routing"). When a subscription declares
/// `proxy-groups`, they are indexed here alongside the per-node registry and
/// rendered into the kernel verbatim; the implicit `url-test` fallback group
/// is then omitted, and the subscription's rule table follows the schema's
/// own `rules:` in render order. Indexed per subscription so a refresh
/// replaces (not merges into) the previous routing generation.
#[derive(Clone, Debug, Default)]
pub struct SubscriptionRouting {
    pub groups: Vec<caly_domain::ProxyGroup>,
    pub rules: Vec<caly_domain::RoutingRule>,
}

/// Shared subscription-author routing registry (`None`-equivalent when empty
/// for a given `SubscriptionId`).
pub type CoreRoutingRegistry = Arc<Mutex<BTreeMap<SubscriptionId, SubscriptionRouting>>>;

/// Builds the shared routing registry, mirroring the inline construction of
/// the per-node [`CoreNodeRegistry`].
#[must_use]
pub fn shared_routing_registry() -> CoreRoutingRegistry {
    Arc::new(Mutex::new(BTreeMap::new()))
}

/// Merges every subscription's declared routing into one render input.
/// Groups key by name with first-subscription-wins (a duplicate name would
/// Shared subscription-to-core proxy registry.
///
/// Ordered by canonical node identity (`NodeId: Ord`) so config rendering and
/// enumeration are deterministic across runs — a `HashMap` made the rendered
/// proxy order random per process.
pub type CoreNodeRegistry = Arc<Mutex<BTreeMap<NodeId, RegisteredProxy>>>;

/// Back-compat alias used by the subscription backend.
pub type MihomoNodeRegistry = CoreNodeRegistry;

/// Active-core command dispatch.
pub enum CoreBackend {
    /// Mihomo controller adapter.
    Mihomo(MihomoCoreBackend),
    /// sing-box controller adapter.
    SingBox(SingBoxCoreBackend),
}

impl CoreCommandBackend for CoreBackend {
    fn select_proxy(&mut self, node: NodeId) -> Result<AppliedState, ActorFailure> {
        match self {
            Self::Mihomo(backend) => backend.select_proxy(node),
            Self::SingBox(backend) => backend.select_proxy(node),
        }
    }

    fn select_proxy_group(
        &mut self,
        group: &str,
        member: &str,
    ) -> Result<AppliedState, ActorFailure> {
        match self {
            Self::Mihomo(backend) => backend.select_proxy_group(group, member),
            Self::SingBox(backend) => backend.select_proxy_group(group, member),
        }
    }

    fn close_all_connections(&mut self) -> Result<ObservedState, ActorFailure> {
        match self {
            Self::Mihomo(backend) => backend.close_all_connections(),
            Self::SingBox(backend) => backend.close_all_connections(),
        }
    }

    fn set_mode(&mut self, mode: ProxyMode) -> Result<AppliedState, ActorFailure> {
        match self {
            Self::Mihomo(backend) => backend.set_mode(mode),
            Self::SingBox(backend) => backend.set_mode(mode),
        }
    }
}

/// Mihomo core command adapter owning the shared node registry.
pub struct MihomoCoreBackend {
    control: MihomoHttpControl,
    nodes: CoreNodeRegistry,
}

impl MihomoCoreBackend {
    /// Wraps a ready controller with a fresh node registry.
    pub fn new(control: MihomoHttpControl) -> Self {
        Self {
            control,
            nodes: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Shares the registry the subscription backend indexes nodes into.
    pub fn registry(&self) -> CoreNodeRegistry {
        Arc::clone(&self.nodes)
    }
}

impl CoreCommandBackend for MihomoCoreBackend {
    fn select_proxy(&mut self, node: NodeId) -> Result<AppliedState, ActorFailure> {
        let registered = resolve_node(&self.nodes, node)?;
        self.control
            .select_proxy(&registered.landing_group, &registered.name, CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        crate::selection::remember(
            node,
            &registered.landing_group,
            &registered.name,
            CoreKind::Mihomo,
        );
        applied_running(CoreKind::Mihomo, Some(node))
    }

    // W4 (`node pick`): Mihomo group members are addressed by their
    // display name, and a declared selector group renders under its own
    // name — both travel verbatim from the offline entry tree. The kernel
    // answers 400 for a member outside the group, which surfaces as a
    // terminal failure (exit 1) rather than a silent no-op.
    fn select_proxy_group(
        &mut self,
        group: &str,
        member: &str,
    ) -> Result<AppliedState, ActorFailure> {
        self.control
            .select_proxy(group, member, CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        // Best-effort selection persistence: remember when the member is a
        // registered node; a builtin / nested-group pick clears any
        // previous node selection so a restart cannot resurrect it. The
        // applied projection carries the picked node too — `status` must
        // show the group-path selection, not a stale None (2026-08-12
        // status audit).
        let selected = if let Ok((node_id, _)) = find_node_by_name(&self.nodes, member) {
            crate::selection::remember(node_id, group, member, CoreKind::Mihomo);
            Some(node_id)
        } else {
            crate::selection::clear();
            None
        };
        applied_running(CoreKind::Mihomo, selected)
    }

    fn close_all_connections(&mut self) -> Result<ObservedState, ActorFailure> {
        self.control
            .close_all_connections(CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        Ok(ObservedState::new(0, 0, 0, 0))
    }

    fn set_mode(&mut self, mode: ProxyMode) -> Result<AppliedState, ActorFailure> {
        self.control
            .set_mode(mode_label(mode), CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        applied_running(CoreKind::Mihomo, None)
    }

    fn list_proxy_groups(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Vec<caly_domain::ProxyGroupView>, ActorFailure> {
        // W3b: mihomo `/proxies` carries kernel-side membership (`all`) and
        // the current selection (`now`); failure degrades to an empty slice
        // (the declared group structure stays authoritative).
        self.control
            .list_proxy_groups(timeout)
            .map(|groups| {
                groups
                    .into_iter()
                    .map(|g| caly_domain::ProxyGroupView {
                        name: g.name,
                        kind: g.kind,
                        selected: g.selected,
                        members: g.members,
                    })
                    .collect()
            })
            .map_err(|_| {
                crate::failure(
                    "cannot list kernel proxy groups",
                    "check the core is running",
                )
            })
    }
}

/// sing-box core command adapter owning the shared node registry.
pub struct SingBoxCoreBackend {
    control: SingBoxHttpControl,
    nodes: CoreNodeRegistry,
}

impl SingBoxCoreBackend {
    /// Wraps a ready controller with a fresh node registry.
    pub fn new(control: SingBoxHttpControl) -> Self {
        Self {
            control,
            nodes: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Wraps a ready controller sharing an existing registry (dual-core mode).
    pub fn new_with_registry(control: SingBoxHttpControl, nodes: CoreNodeRegistry) -> Self {
        Self { control, nodes }
    }

    /// Shares the registry the subscription backend indexes nodes into.
    pub fn registry(&self) -> CoreNodeRegistry {
        Arc::clone(&self.nodes)
    }
}

impl CoreCommandBackend for SingBoxCoreBackend {
    fn select_proxy(&mut self, node: NodeId) -> Result<AppliedState, ActorFailure> {
        // Verify the node is subscription-indexed before dialing the kernel.
        resolve_node(&self.nodes, node)?;
        // sing-box renders outbounds tagged `proxy-<canonical-hex>` under the
        // PROXY/GLOBAL selectors, so the Clash API select must address that
        // tag in the PROXY group — not the Mihomo display name/group ("AUTO")
        // stored on the shared registry entry.
        let tag = format!("proxy-{}", caly_domain::to_hex(node.into_bytes()));
        self.control
            .select_proxy("PROXY", &tag, CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        crate::selection::remember(node, "PROXY", &tag, CoreKind::SingBox);
        applied_running(CoreKind::SingBox, Some(node))
    }

    // W4 (`node pick`): sing-box renders node outbounds under
    // `proxy-<hex>` tags (builtins `direct`/`block`, nested groups under
    // their own name), so the member name from the offline tree must be
    // resolved to the kernel tag before the Clash-API select. A node
    // member is matched through the shared registry by display name;
    // builtins map to the lowercase tags sing-box actually renders
    // (the CLI normalizes the declared spelling to `DIRECT`/`REJECT`, so
    // a verbatim pass-through would 400); anything else (a nested group
    // reference) travels verbatim.
    fn select_proxy_group(
        &mut self,
        group: &str,
        member: &str,
    ) -> Result<AppliedState, ActorFailure> {
        let registered = find_node_by_name(&self.nodes, member);
        let tag = match &registered {
            Ok((node_id, _)) => format!("proxy-{}", caly_domain::to_hex(node_id.into_bytes())),
            Err(_) if member.eq_ignore_ascii_case("DIRECT") => "direct".to_owned(),
            Err(_) if member.eq_ignore_ascii_case("REJECT") => "block".to_owned(),
            Err(_) => {
                // A name that is neither a registered node nor a builtin
                // is either a nested-group reference (passes through
                // verbatim — correct) or a node that drifted out of the
                // registry after a subscription refresh (the kernel will
                // answer 400; the warning makes the cause visible instead
                // of a bare controller error).
                tracing::warn!(
                    member,
                    "member is not in the node registry; passing through verbatim (nested group or drifted node)"
                );
                member.to_owned()
            }
        };
        self.control
            .select_proxy(group, &tag, CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        // The applied projection carries the picked node when the member is
        // registered — `status` must show the group-path selection, not a
        // stale None (2026-08-12 status audit).
        let selected = if let Ok((node_id, _)) = registered {
            crate::selection::remember(node_id, group, &tag, CoreKind::SingBox);
            Some(node_id)
        } else {
            crate::selection::clear();
            None
        };
        applied_running(CoreKind::SingBox, selected)
    }

    fn close_all_connections(&mut self) -> Result<ObservedState, ActorFailure> {
        self.control
            .close_all_connections(CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        Ok(ObservedState::new(0, 0, 0, 0))
    }

    fn set_mode(&mut self, mode: ProxyMode) -> Result<AppliedState, ActorFailure> {
        self.control
            .set_mode(mode_label(mode), CONTROL_TIMEOUT)
            .map_err(kernel_failure)?;
        applied_running(CoreKind::SingBox, None)
    }

    fn list_proxy_groups(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Vec<caly_domain::ProxyGroupView>, ActorFailure> {
        // The sing-box control surface proxies to the mihomo-compatible
        // inner control; the same `/proxies` enrichment applies.
        self.control
            .proxy_groups(timeout)
            .map(|groups| {
                groups
                    .into_iter()
                    .map(|g| caly_domain::ProxyGroupView {
                        name: g.name,
                        kind: g.kind,
                        selected: g.selected,
                        members: g.members,
                    })
                    .collect()
            })
            .map_err(|_| {
                crate::failure(
                    "cannot list kernel proxy groups",
                    "check the core is running",
                )
            })
    }
}

/// Resolves a node identity through the subscription-indexed registry.
fn resolve_node(nodes: &CoreNodeRegistry, node: NodeId) -> Result<RegisteredProxy, ActorFailure> {
    let mapping = nodes.lock().map_err(|_| {
        ActorFailure::infrastructure(
            "core node registry is poisoned",
            "restart the application runtime",
        )
    })?;
    mapping.get(&node).cloned().ok_or_else(|| {
        ActorFailure::infrastructure(
            "node is not registered by any subscription",
            "refresh the subscription and retry",
        )
    })
}

/// W4: resolves a member by its display name (the offline tree's spelling)
/// through the shared registry, returning the node identity alongside the
/// registered entry so callers can derive kernel tags and persist
/// selections. Members outside every subscription index are a terminal
/// failure — the CLI already validates membership against the declared
/// tree, so reaching here means the online registry drifted.
fn find_node_by_name(
    nodes: &CoreNodeRegistry,
    name: &str,
) -> Result<(NodeId, RegisteredProxy), ActorFailure> {
    let mapping = nodes.lock().map_err(|_| {
        ActorFailure::infrastructure(
            "core node registry is poisoned",
            "restart the application runtime",
        )
    })?;
    mapping
        .iter()
        .find(|(_, entry)| entry.name == name)
        .map(|(node_id, entry)| (*node_id, entry.clone()))
        .ok_or_else(|| {
            ActorFailure::infrastructure(
                "member is not registered by any subscription",
                "refresh the subscription and retry",
            )
        })
}

/// Builds the applied-state projection for a running core after a command.
fn applied_running(
    core: CoreKind,
    selected_node: Option<NodeId>,
) -> Result<AppliedState, ActorFailure> {
    AppliedState::new(Some(core), CoreRunState::Running, selected_node, None).map_err(|_| {
        ActorFailure::infrastructure(
            "applied state projection is invalid",
            "restart the application runtime",
        )
    })
}

/// Maps a kernel controller failure onto an actor failure, preserving the
/// kernel's human-readable message and suggested action.
fn kernel_failure(error: caly_corectl::contract::KernelFailure) -> ActorFailure {
    ActorFailure::infrastructure(error.message.as_str(), error.suggested_action.as_str())
}

/// Clash API mode label for a routing mode.
fn mode_label(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Rule => "rule",
        ProxyMode::Global => "global",
        ProxyMode::Direct => "direct",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with(entries: &[(u8, &str)]) -> CoreNodeRegistry {
        let registry: CoreNodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
        if let Ok(mut map) = registry.lock() {
            for (seed, name) in entries {
                map.insert(
                    NodeId::from_bytes([*seed; 16]),
                    RegisteredProxy {
                        landing_group: "PROXY".to_owned(),
                        name: (*name).to_owned(),
                        yaml: format!("- name: {name}\n"),
                        singbox: None,
                        subscription: caly_domain::SubscriptionId::from_bytes([0; 16]),
                    },
                );
            }
        }
        registry
    }

    #[test]
    fn unregistered_node_is_rejected_before_any_controller_call() {
        let mut backend = SingBoxCoreBackend {
            control: SingBoxHttpControl::new("127.0.0.1:1".to_owned(), None).unwrap(),
            nodes: registry_with(&[(1, "node-a")]),
        };
        let error = backend
            .select_proxy(NodeId::from_bytes([9; 16]))
            .err()
            .unwrap();
        assert!(error.message.as_str().contains("not registered"));
    }

    #[test]
    fn registered_node_reaches_the_wire() {
        // Nothing listens on port 1: a transport failure (not a registry
        // failure) proves resolution succeeded.
        let mut backend = SingBoxCoreBackend {
            control: SingBoxHttpControl::new("127.0.0.1:1".to_owned(), None).unwrap(),
            nodes: registry_with(&[(1, "node-a")]),
        };
        let error = backend
            .select_proxy(NodeId::from_bytes([1; 16]))
            .err()
            .unwrap();
        assert!(!error.message.as_str().contains("not registered"));
    }

    #[test]
    fn registry_iterates_in_node_id_order() {
        let registry = registry_with(&[(3, "node-c"), (1, "node-a"), (2, "node-b")]);
        let mapping = registry.lock().unwrap();
        let names: Vec<String> = mapping.values().map(|entry| entry.name.clone()).collect();
        assert_eq!(names, vec!["node-a", "node-b", "node-c"]);
    }
}

/// Converts the schema's declared `proxy_groups:` into the domain routing
/// model, so the config's own groups render into the kernel alongside the
/// subscription-author groups (2026-08-12 组源统一: previously the schema
/// groups were validated by the CLI but never rendered — `node pick` on a
/// config group passed the offline check and the kernel answered 400).
/// Disabled groups are dropped (matching `node list --offline --enabled`).
///
/// # Panics
///
/// Never: every bound field degrades instead — an oversized probe URL
/// drops the probe block, unboundable names/members skip the group.
pub fn declared_groups_to_domain(
    groups: &[caly_profile::schema::ProxyGroupConfig],
) -> Vec<caly_domain::ProxyGroup> {
    groups
        .iter()
        .filter(|group| group.enabled)
        .filter_map(|group| {
            let name = caly_domain::ProxyGroupName::new(group.name.clone()).ok()?;
            let kind = match group.group_type {
                caly_profile::schema::ProxyGroupTypeConfig::Select => {
                    caly_domain::ProxyGroupType::Select
                }
                caly_profile::schema::ProxyGroupTypeConfig::UrlTest => {
                    caly_domain::ProxyGroupType::UrlTest
                }
                caly_profile::schema::ProxyGroupTypeConfig::Fallback => {
                    caly_domain::ProxyGroupType::Fallback
                }
                caly_profile::schema::ProxyGroupTypeConfig::LoadBalance => {
                    caly_domain::ProxyGroupType::LoadBalance
                }
                caly_profile::schema::ProxyGroupTypeConfig::Relay => {
                    caly_domain::ProxyGroupType::Relay
                }
            };
            let members: Vec<caly_domain::ProxyGroupMember> = group
                .members
                .iter()
                .filter_map(|member| match member {
                    caly_profile::schema::ProxyGroupMemberConfig::Node { tag } => {
                        caly_domain::ProxyGroupNodeTag::new(tag.clone())
                            .ok()
                            .map(|tag| caly_domain::ProxyGroupMember::Node { tag })
                    }
                    caly_profile::schema::ProxyGroupMemberConfig::Group { name } => {
                        caly_domain::ProxyGroupName::new(name.clone())
                            .ok()
                            .map(|name| caly_domain::ProxyGroupMember::Group { name })
                    }
                    caly_profile::schema::ProxyGroupMemberConfig::Direct => {
                        Some(caly_domain::ProxyGroupMember::Direct)
                    }
                    caly_profile::schema::ProxyGroupMemberConfig::Reject => {
                        Some(caly_domain::ProxyGroupMember::Reject)
                    }
                })
                .collect();
            let url_test = group.url_test.as_ref().and_then(|probe| {
                // The schema bounds the probe URL, but the conversion
                // must not panic on a hand-edited oversized value —
                // drop the probe block instead (the kernel falls
                // back to its default probe URL).
                Some(caly_domain::UrlTestConfig {
                    url: caly_domain::ProxyGroupUrl::new(probe.url.clone()).ok()?,
                    interval_seconds: probe.interval_seconds,
                    tolerance_ms: probe.tolerance_ms,
                })
            });
            Some(caly_domain::ProxyGroup {
                name,
                kind,
                members,
                url_test,
            })
        })
        .collect()
}

/// The unified group source for kernel rendering: the config's declared
/// groups take precedence, subscription-author groups fill the gaps
/// (first-wins per name — the config is the operator's explicit intent,
/// a subscription rename cannot silently override it). Rules still
/// concatenate in `SubscriptionId` order after the config rules, which
/// the renderers merge first (the usual Clash convention).
#[must_use]
pub fn merged_routing_with_declared(
    store: &CoreRoutingRegistry,
    declared: &[caly_domain::ProxyGroup],
) -> Option<(Vec<caly_domain::ProxyGroup>, Vec<caly_domain::RoutingRule>)> {
    let stored = store.lock().ok()?;
    let mut groups: Vec<caly_domain::ProxyGroup> = declared.to_vec();
    let mut seen: std::collections::HashSet<String> = declared
        .iter()
        .map(|group| group.name.as_str().to_owned())
        .collect();
    let mut rules = Vec::new();
    for (id, routing) in stored.iter() {
        for group in &routing.groups {
            if seen.insert(group.name.as_str().to_owned()) {
                groups.push(group.clone());
            } else {
                tracing::warn!(
                    subscription = %id,
                    group = group.name.as_str(),
                    "subscription proxy-group name already declared in config; config wins"
                );
            }
        }
        rules.extend(routing.rules.iter().cloned());
    }
    if groups.is_empty() {
        return None;
    }
    Some((groups, rules))
}

#[cfg(test)]
mod group_source_tests {
    use super::*;

    fn schema_group(
        name: &str,
        enabled: bool,
        kind: &str,
        members: Vec<&str>,
    ) -> caly_profile::schema::ProxyGroupConfig {
        use caly_profile::schema::{ProxyGroupMemberConfig, ProxyGroupTypeConfig};
        let kind = match kind {
            "select" => ProxyGroupTypeConfig::Select,
            "url-test" => ProxyGroupTypeConfig::UrlTest,
            other => panic!("bad kind {other}"),
        };
        let members = members
            .into_iter()
            .map(|member| {
                if member == "DIRECT" {
                    ProxyGroupMemberConfig::Direct
                } else {
                    ProxyGroupMemberConfig::Node {
                        tag: member.to_owned(),
                    }
                }
            })
            .collect();
        caly_profile::schema::ProxyGroupConfig {
            name: name.to_owned(),
            group_type: kind,
            members,
            url_test: None,
            enabled,
        }
    }

    /// The config's declared groups convert into the domain routing model
    /// with disabled groups dropped (2026-08-12 组源统一).
    #[test]
    fn declared_groups_convert_with_disabled_dropped() {
        let groups = vec![
            schema_group("节点选择", true, "select", vec!["hk-01", "DIRECT"]),
            schema_group("off", false, "select", vec!["hk-02"]),
        ];
        let converted = declared_groups_to_domain(&groups);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].name.as_str(), "节点选择");
        assert_eq!(converted[0].members.len(), 2);
        assert_eq!(
            converted[0].members[0].to_clash(),
            "hk-01",
            "node member keeps its tag"
        );
        assert_eq!(converted[0].members[1].to_clash(), "DIRECT");
    }

    /// The unified source renders config groups first and subscription
    /// groups only fill names the config did not declare — a subscription
    /// rename cannot silently override the operator's topology.
    #[test]
    fn merged_routing_prefers_declared_over_subscription() {
        let store: CoreRoutingRegistry = shared_routing_registry();
        let subscription_group = caly_domain::ProxyGroup {
            name: caly_domain::ProxyGroupName::new("节点选择".to_owned()).unwrap(),
            kind: caly_domain::ProxyGroupType::UrlTest,
            members: vec![caly_domain::ProxyGroupMember::Direct],
            url_test: None,
        };
        store.lock().unwrap().insert(
            caly_domain::SubscriptionId::from_bytes([9; 16]),
            SubscriptionRouting {
                groups: vec![subscription_group.clone()],
                rules: Vec::new(),
            },
        );
        let declared = vec![caly_domain::ProxyGroup {
            name: caly_domain::ProxyGroupName::new("节点选择".to_owned()).unwrap(),
            kind: caly_domain::ProxyGroupType::Select,
            members: vec![caly_domain::ProxyGroupMember::Direct],
            url_test: None,
        }];
        let (merged, _) = merged_routing_with_declared(&store, &declared).expect("merged");
        assert_eq!(merged.len(), 1, "duplicate name collapses to the config's");
        assert_eq!(merged[0].kind, caly_domain::ProxyGroupType::Select);
    }
}
