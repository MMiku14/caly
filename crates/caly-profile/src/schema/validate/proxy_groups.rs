//! Proxy-group validation (budget, path-safety, probe shape, member
//! references, relay cycles).
//!
//! Split out of `validate.rs` (audit #70 file-length budget); every check
//! here is reached from `super::validate` and turns a
//! parseable-but-unusable configuration into an explicit boot failure.

use super::super::proxy_group::ProxyGroupTypeConfig;
use super::{AppConfig, ConfigError};

/// Validates every `proxy_groups:` entry before render.
///
/// The validator enforces five invariants the schema cannot
/// express through `serde` alone:
///
/// 1. **Budget.** Total groups are bounded by
///    [`caly_domain::MAX_PROXY_GROUPS`]; each group carries at
///    most [`caly_domain::MAX_PROXY_GROUP_MEMBERS`] members.
/// 2. **Path-safety.** Each `name` is a path-safe ASCII
///    identifier (the same rule as `ProfileId`); this is the
///    only way a group can be referenced from `RULE-SET,`
///    policies and from other groups' `members:` lists without
///    further normalisation.
/// 3. **Probe shape.** A probe-driven group (`url-test` /
///    `fallback` / `load-balance`) must have a `url_test:`
///    block; a non-probe group must not (the renderer would
///    silently drop the field, and the operator deserves to
///    know). A zero `interval_seconds` is rejected.
/// 4. **Member references.** A `Group` member that points at
///    an undeclared name is a hard failure — the live core
///    would refuse to start and the user would get a less
///    precise error.
/// 5. **Relay cycles.** `relay` groups form a graph; a
///    depth-first walk catches every cycle and reports the
///    first id that re-enters the visited set.
///
/// Domain errors are wrapped through [`From`] so the
/// `?` operator at the call site stays one line.
/// Relay-cycle walk state: the ids currently on the DFS stack
/// (`in_stack`) and the ids fully explored (`done`). Reuses the
/// shared [`super::MergeWalkState`] defined for the profile-merge
/// walker; the two graphs are independent, so only the state
/// shape is shared.

pub(super) fn validate_proxy_groups(config: &AppConfig) -> Result<(), ConfigError> {
    use super::super::proxy_group::ProxyGroupMemberConfig;
    use caly_domain::{
        MAX_PROXY_GROUP_MEMBERS, MAX_PROXY_GROUPS, ProxyGroupError, is_path_safe_component,
    };
    if config.proxy_groups.len() > MAX_PROXY_GROUPS {
        return Err(ProxyGroupError::TooManyGroups.into());
    }
    // Step 1: budget + path-safety + probe shape.
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for group in &config.proxy_groups {
        if !is_path_safe_component(&group.name) {
            return Err(ProxyGroupError::UnknownMemberGroup {
                group: group.name.clone(),
                member: group.name.clone(),
            }
            .into());
        }
        if !declared.insert(group.name.clone()) {
            return Err(ProxyGroupError::DuplicateName {
                name: group.name.clone(),
            }
            .into());
        }
        // Both kernels refuse to start a proxy group with an empty
        // `proxies:` list — reject it here with a named diagnostic
        // instead of letting the core report a generic render failure.
        if group.members.is_empty() {
            return Err(ProxyGroupError::EmptyMembers {
                name: group.name.clone(),
            }
            .into());
        }
        if group.members.len() > MAX_PROXY_GROUP_MEMBERS {
            return Err(ProxyGroupError::TooManyMembers {
                name: group.name.clone(),
            }
            .into());
        }
        let needs_url = group.group_type.needs_url();
        if needs_url && group.url_test.is_none() {
            return Err(ProxyGroupError::MissingUrlTest {
                name: group.name.clone(),
            }
            .into());
        }
        if !needs_url && group.url_test.is_some() {
            return Err(ProxyGroupError::UnexpectedUrlTest {
                name: group.name.clone(),
            }
            .into());
        }
        if let Some(url_test) = &group.url_test {
            if url_test.interval_seconds == 0 {
                return Err(ProxyGroupError::InvalidInterval {
                    name: group.name.clone(),
                }
                .into());
            }
            // The probe URL only fails at runtime today; validate the
            // scheme/host at load so a typo does not reach the core (and
            // non-http(s) targets never become a probe request at all).
            let parsed =
                url::Url::parse(&url_test.url).map_err(|_| ProxyGroupError::InvalidProbeUrl {
                    name: group.name.clone(),
                })?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(ProxyGroupError::InvalidProbeUrl {
                    name: group.name.clone(),
                }
                .into());
            }
        }
    }
    // Step 2: member-reference resolution. A `Group` member
    // that points at an undeclared name is a hard failure
    // (the live core would refuse to start with a less
    // precise error). The schema validator runs **before**
    // the renderer, so a missing reference is the operator's
    // signal to add a `proxy_groups:` entry.
    for group in &config.proxy_groups {
        for member in &group.members {
            if let ProxyGroupMemberConfig::Group { name } = member
                && !declared.contains(name)
            {
                return Err(ProxyGroupError::UnknownMemberGroup {
                    group: group.name.clone(),
                    member: name.clone(),
                }
                .into());
            }
        }
    }
    // Step 3: relay cycles. A `relay` group forms a
    // directed graph where the outgoing edges are the
    // `Group` members. The walk is depth-first over the
    // set of declared `relay` groups; the first cycle
    // encountered is reported with the offending id so
    // the operator can break the loop in one place. A
    // diamond (two relay chains reusing one downstream
    // relay) revisits a *finished* node and must not be
    // reported as a cycle — same two-colour discipline
    // as the profile-merge walk.
    let mut state = super::MergeWalkState::default();
    for group in &config.proxy_groups {
        if group.group_type != ProxyGroupTypeConfig::Relay {
            continue;
        }
        walk_relay(&group.name, &declared, &config.proxy_groups, &mut state)?;
    }
    Ok(())
}

/// DFS over the `relay` member graph. The `declared` set
/// is the full set of group names; the walk is scoped to
/// the `relay` members only. A revisit of the same id
/// while it is still on the recursion stack is a cycle.
fn walk_relay(
    id: &str,
    declared: &std::collections::HashSet<String>,
    groups: &[super::super::ProxyGroupConfig],
    state: &mut super::MergeWalkState,
) -> Result<(), ConfigError> {
    if state.done.contains(id) {
        return Ok(());
    }
    if !state.in_stack.insert(id.to_owned()) {
        return Err(caly_domain::ProxyGroupError::RelayCycle {
            group: id.to_owned(),
        }
        .into());
    }
    if let Some(group) = groups.iter().find(|candidate| candidate.name == id) {
        for member in &group.members {
            if let super::super::proxy_group::ProxyGroupMemberConfig::Group { name } = member
                && declared.contains(name.as_str())
                && let Some(next) = groups.iter().find(|candidate| candidate.name == *name)
                && next.group_type == ProxyGroupTypeConfig::Relay
            {
                walk_relay(name.as_str(), declared, groups, state)?;
            }
        }
    }
    state.in_stack.remove(id);
    state.done.insert(id.to_owned());
    Ok(())
}
