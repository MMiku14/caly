//! Subscription-declared proxy groups rendered as sing-box outbounds.
//!
//! mihomo owns five group kinds; sing-box's composite outbounds carry only
//! `selector` and `urltest`, so the mapping is deliberately lossy-in-one-
//! direction and documented per arm (2026-08-09 规划: subscription groups
//! own the routing when declared):
//!
//! | Clash kind     | sing-box outbound | Notes                                        |
//! |----------------|-------------------|----------------------------------------------|
//! | `select`       | `selector`        | Exact.                                        |
//! | `url-test`     | `urltest`         | Exact, incl. probe url / interval / tolerance.|
//! | `fallback`     | `urltest`         | sing-box has no ordered fallback; the urltest |
//! |                |                   | health check is the closest semantic.         |
//! | `load-balance` | `urltest`         | Round-robin distribution is not expressible;  |
//! |                |                   | degrades to health-checked selection.         |
//! | `relay`        | `selector`        | Chaining needs per-hop dialers, not group     |
//! |                |                   | outbounds; renders as a manual selector.      |
//!
//! Members resolve through the caller's tag resolver: node names map to the
//! canonical sing-box node outbound tag, group names keep their own tag
//! (nested selectors are legal), `DIRECT`/`REJECT` map to the built-in
//! `direct`/`block` outbounds. Members that resolve to nothing are dropped;
//! a group left with zero members is not rendered at all.

use caly_domain::{ProxyGroup, ProxyGroupMember, ProxyGroupType};
use serde::Serialize;
use serde_json::Value;

/// Renders one declared group, or `None` when no member resolves.
pub fn proxy_group_to_sing_box_outbound(
    group: &ProxyGroup,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Option<Value> {
    let members: Vec<String> = group
        .members
        .iter()
        .filter_map(|member| match member {
            ProxyGroupMember::Node { tag } => resolve(tag.as_str()),
            ProxyGroupMember::Group { name } => Some(name.as_str().to_owned()),
            ProxyGroupMember::Direct => Some("direct".to_owned()),
            ProxyGroupMember::Reject => Some("block".to_owned()),
        })
        .collect();
    if members.is_empty() {
        return None;
    }
    let tag = group.name.as_str().to_owned();
    let outbound = if matches!(group.kind, ProxyGroupType::Select | ProxyGroupType::Relay) {
        GroupOutbound::Selector(SelectorOutbound {
            outbounds: members,
            tag,
            kind: "selector",
        })
    } else {
        let probe = group.url_test.as_ref();
        let url = probe.map_or("https://www.gstatic.com/generate_204", |probe| {
            probe.url.as_str()
        });
        let interval = probe.map_or("300s".to_owned(), |probe| {
            format!("{}s", probe.interval_seconds)
        });
        let tolerance = probe.map_or(50, |probe| probe.tolerance_ms);
        GroupOutbound::Urltest(UrltestOutbound {
            interval,
            outbounds: members,
            tag,
            tolerance,
            kind: "urltest",
            url: url.to_owned(),
        })
    };
    // Derived serialization over plain String/Vec/integer fields cannot
    // fail; `.ok()` exists only because `to_value` is Result-typed.
    serde_json::to_value(&outbound).ok()
}

/// One subscription-declared group rendered as a sing-box outbound
/// (`selector` or `urltest`). P3b typed form: fields are declared
/// alphabetically, mirroring the pre-typed `serde_json::Value` (BTreeMap)
/// byte order exactly.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum GroupOutbound {
    Selector(SelectorOutbound),
    Urltest(UrltestOutbound),
}

/// `selector` outbound (`select`, and `relay` degraded to manual selection).
/// Also the shape of the document-level PROXY/GLOBAL catch-all selectors, so
/// `document.rs` shares this struct instead of declaring a second one.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SelectorOutbound {
    pub(crate) outbounds: Vec<String>,
    pub(crate) tag: String,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
}

/// `urltest` outbound (`url-test` exact; `fallback`/`load-balance` degrade to
/// the health-checked nearest semantic).
#[derive(Clone, Debug, Serialize)]
struct UrltestOutbound {
    interval: String,
    outbounds: Vec<String>,
    tag: String,
    tolerance: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::{
        ProxyGroupMember, ProxyGroupName, ProxyGroupNodeTag, ProxyGroupType, ProxyGroupUrl,
        UrlTestConfig,
    };

    fn mk_group(
        kind: ProxyGroupType,
        members: Vec<ProxyGroupMember>,
        probe: Option<UrlTestConfig>,
    ) -> ProxyGroup {
        let name = match ProxyGroupName::new("测试组".to_owned()) {
            Ok(name) => name,
            Err(error) => panic_or_abort(error),
        };
        ProxyGroup {
            name,
            kind,
            members,
            url_test: probe,
        }
    }

    /// Test-only infallible bounded constructors, kept out of `unwrap()` for
    /// the workspace lint gate.
    fn bounded<T, E>(outcome: Result<T, E>) -> T {
        match outcome {
            Ok(value) => value,
            Err(_) => panic_or_abort("bounded constructor failed"),
        }
    }

    #[cold]
    fn panic_or_abort<E: std::fmt::Debug>(error: E) -> ! {
        std::panic::panic_any(format!("{error:?}"));
    }

    fn node(tag: &str) -> ProxyGroupMember {
        ProxyGroupMember::Node {
            tag: bounded(ProxyGroupNodeTag::new(tag.to_owned())),
        }
    }

    fn resolve(tag: &str) -> Option<String> {
        (tag == "node-a").then(|| "proxy-1".to_owned())
    }

    #[test]
    fn select_renders_selector_with_resolved_members() {
        let group = mk_group(
            ProxyGroupType::Select,
            vec![
                node("node-a"),
                ProxyGroupMember::Direct,
                ProxyGroupMember::Reject,
                ProxyGroupMember::Group {
                    name: bounded(ProxyGroupName::new("其他组".to_owned())),
                },
            ],
            None,
        );
        let outbound = proxy_group_to_sing_box_outbound(&group, &resolve).unwrap_or_default();
        assert_eq!(outbound["type"].as_str(), Some("selector"));
        assert_eq!(outbound["tag"].as_str(), Some("测试组"));
        assert_eq!(
            outbound["outbounds"].as_array().map(Vec::len),
            Some(4),
            "node tag + direct + block + nested group: {outbound}"
        );
    }

    #[test]
    fn url_test_probe_parameters_pass_through() {
        let probe = UrlTestConfig {
            url: bounded(ProxyGroupUrl::new("https://cp.cloudflare.com/".to_owned())),
            interval_seconds: 600,
            tolerance_ms: 200,
        };
        let group = mk_group(ProxyGroupType::UrlTest, vec![node("node-a")], Some(probe));
        let outbound = proxy_group_to_sing_box_outbound(&group, &resolve).unwrap_or_default();
        assert_eq!(outbound["type"].as_str(), Some("urltest"));
        assert_eq!(outbound["url"].as_str(), Some("https://cp.cloudflare.com/"));
        assert_eq!(outbound["interval"].as_str(), Some("600s"));
        assert_eq!(outbound["tolerance"].as_u64(), Some(200));
    }

    #[test]
    fn fallback_and_load_balance_degrade_to_urltest_relay_to_selector() {
        for kind in [ProxyGroupType::Fallback, ProxyGroupType::LoadBalance] {
            let group = mk_group(kind, vec![ProxyGroupMember::Direct], None);
            let outbound = proxy_group_to_sing_box_outbound(&group, &resolve).unwrap_or_default();
            assert_eq!(outbound["type"].as_str(), Some("urltest"), "{kind:?}");
        }
        let relay = mk_group(ProxyGroupType::Relay, vec![ProxyGroupMember::Direct], None);
        let outbound = proxy_group_to_sing_box_outbound(&relay, &resolve).unwrap_or_default();
        assert_eq!(outbound["type"].as_str(), Some("selector"));
    }

    #[test]
    fn unresolvable_members_drop_and_empty_group_vanishes() {
        let group = mk_group(
            ProxyGroupType::Select,
            vec![node("不存在"), node("也不存在"), ProxyGroupMember::Direct],
            None,
        );
        let outbound = proxy_group_to_sing_box_outbound(&group, &resolve).unwrap_or_default();
        assert_eq!(outbound["outbounds"].as_array().map(Vec::len), Some(1));
        let empty = mk_group(ProxyGroupType::Select, vec![node("不存在")], None);
        assert!(proxy_group_to_sing_box_outbound(&empty, &resolve).is_none());
    }
}
