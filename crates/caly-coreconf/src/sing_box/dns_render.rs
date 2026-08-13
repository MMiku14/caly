//! sing-box DNS block rendering (1.12+ server format), typed-model form.
//!
//! Every DNS server carries a `type`; fake-ip is expressed as a `fakeip`
//! server type referenced by a rule. When a server references a domain-hosted
//! resolver, the caller must also wire `route.default_domain_resolver` to the
//! first IP-literal server, which `RenderedDns::domain_resolver` reports.
//!
//! Typed-model note (P3b): all structs declare fields in alphabetical order,
//! matching the byte order the pre-typed `serde_json::Value` (BTreeMap)
//! assembly produced, so re-serialization stays byte-comparable.

use caly_dns::{DnsMode, DnsSettings, Nameserver, NameserverKind};
use serde::Serialize;

/// A rendered `dns` object plus the resolver the caller must wire into `route`
/// when the configuration references a domain-hosted DNS server.
pub(crate) struct RenderedDns {
    pub(crate) block: DnsBlock,
    pub(crate) domain_resolver: Option<String>,
}

/// sing-box `dns` block: the server list, optional fakeip shortcut rules and
/// an optional `final` tag.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DnsBlock {
    #[serde(rename = "final", skip_serializing_if = "Option::is_none")]
    pub(crate) final_tag: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) rules: Vec<DnsRule>,
    pub(crate) servers: Vec<DnsServer>,
}

/// One typed sing-box DNS server entry. `server` is absent for `local`;
/// `inet4_range` only exists on the synthesized `fakeip` server;
/// `domain_resolver` wires non-IP servers at the first IP-literal entry;
/// `path` carries the DoH endpoint path (B1).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DnsServer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) domain_resolver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) inet4_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) server: Option<String>,
    pub(crate) tag: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
}

/// One `dns.rules` entry (the fakeip A/AAAA shortcut).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DnsRule {
    pub(crate) query_type: Vec<String>,
    pub(crate) server: String,
}

/// Renders a bounded DNS configuration as a sing-box `dns` JSON object.
pub(crate) fn render_dns_object(dns: &DnsSettings) -> RenderedDns {
    let entries = collect_entries(dns);
    let resolver_tag = find_resolver(&entries);
    let mut servers = Vec::new();
    let mut rules = Vec::new();
    let mut final_tag = None;
    for (group, index, value) in entries {
        let server = render_server(&group, index, value, resolver_tag.as_deref());
        if group == "nameserver" && final_tag.is_none() {
            final_tag = Some(format!("{group}-{index}"));
        }
        servers.push(server);
    }
    if dns.mode() == DnsMode::FakeIp
        && let Some(range) = dns.fake_ip_range()
    {
        servers.push(DnsServer {
            domain_resolver: None,
            inet4_range: Some(range.as_str().to_owned()),
            path: None,
            server: None,
            tag: "fakeip".to_owned(),
            kind: "fakeip".to_owned(),
        });
        rules.push(DnsRule {
            query_type: vec!["A".to_owned(), "AAAA".to_owned()],
            server: "fakeip".to_owned(),
        });
    }
    RenderedDns {
        block: DnsBlock {
            final_tag,
            rules,
            servers,
        },
        domain_resolver: resolver_tag,
    }
}

/// Collects (group, index, nameserver) entries in a stable render order.
fn collect_entries(dns: &DnsSettings) -> Vec<(String, usize, &Nameserver)> {
    let mut entries = Vec::new();
    for (index, server) in dns.nameservers().iter().enumerate() {
        entries.push(("nameserver".to_owned(), index, server));
    }
    for (index, server) in dns.fallback().iter().enumerate() {
        entries.push(("fallback".to_owned(), index, server));
    }
    for (index, server) in dns.direct().iter().enumerate() {
        entries.push(("direct".to_owned(), index, server));
    }
    for (index, server) in dns.default().iter().enumerate() {
        entries.push(("default".to_owned(), index, server));
    }
    entries
}

/// Returns the first IP-literal server tag usable as a domain resolver.
/// B4: the direct group is preferred — `default_domain_resolver` bootstraps
/// domain-hosted servers, and that lookup must not depend on the proxy path
/// those very servers serve (pre-fix the nameserver group won by order,
/// which deadlocks when it only holds domain-hosted entries).
fn find_resolver(entries: &[(String, usize, &Nameserver)]) -> Option<String> {
    let usable =
        |server: &Nameserver| server.kind() != NameserverKind::Local && server.is_ip_literal();
    entries
        .iter()
        .find(|(group, _, server)| group == "direct" && usable(server))
        .or_else(|| entries.iter().find(|(_, _, server)| usable(server)))
        .map(|(group, index, _)| format!("{group}-{index}"))
}

/// Builds one typed sing-box DNS server object from the structured
/// nameserver (the kind/address parse happened in `Nameserver::new`,
/// replicating the historical cut rules). B1: the DoH `path` is rendered
/// from the parsed shape — pre-fix the renderer stripped it, so
/// `https://dns.google/dns-query` dialed `/` and the resolver rejected
/// the queries.
fn render_server(
    group: &str,
    index: usize,
    server: &Nameserver,
    resolver_tag: Option<&str>,
) -> DnsServer {
    let local = server.kind() == NameserverKind::Local;
    DnsServer {
        domain_resolver: if local || server.is_ip_literal() {
            None
        } else {
            resolver_tag.map(str::to_owned)
        },
        inet4_range: None,
        // `path` is a DoH-only field in the sing-box schema; TLS/QUIC
        // servers carry no path, so it stays omitted for other kinds.
        path: (server.kind() == NameserverKind::Https)
            .then(|| server.path().map(str::to_owned))
            .flatten(),
        server: (!local).then(|| server.address().to_owned()),
        tag: format!("{group}-{index}"),
        kind: server.kind().label().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_dns::DnsSettingsBuilder;

    #[test]
    fn renders_typed_servers_and_fakeip_rule() -> Result<(), Box<dyn std::error::Error>> {
        let dns = DnsSettingsBuilder::new()
            .enabled(true)
            .mode(DnsMode::FakeIp)
            .push_nameserver("8.8.8.8")?
            .push_nameserver("1.1.1.1")?
            .push_fallback("tls://dns.google")?
            .push_direct("223.5.5.5")?
            .fake_ip_range("198.18.0.1/16")?
            .build()?
            .ok_or("dns settings disabled")?;
        let rendered = render_dns_object(&dns);
        let json = serde_json::to_string(&rendered.block)?;
        // P3b J1: key order inside one object carries no meaning; assert the
        // key/value pairs individually instead of pinning their adjacency.
        assert!(json.contains("\"type\":\"udp\""));
        assert!(json.contains("\"tag\":\"nameserver-0\""));
        assert!(json.contains("\"type\":\"fakeip\""));
        assert!(json.contains("\"inet4_range\":\"198.18.0.1/16\""));
        assert!(json.contains("\"final\":\"nameserver-0\""));
        assert!(json.contains("{\"query_type\":[\"A\",\"AAAA\"],\"server\":\"fakeip\"}"));
        // B4: the direct group wins the resolver election (pre-fix
        // `nameserver-0`; both are IP literals, but bootstrap lookups must
        // go through the direct path).
        assert_eq!(rendered.domain_resolver.as_deref(), Some("direct-0"));
        Ok(())
    }

    #[test]
    fn b2_host_port_server_is_elected_domain_resolver() -> Result<(), Box<dyn std::error::Error>> {
        // B2: `8.8.8.8:53` is an IP literal in spite of the port, so it can
        // bootstrap domain-hosted resolvers; pre-fix it was misjudged as a
        // domain and no resolver was elected.
        let dns = DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("8.8.8.8:53")?
            .push_fallback("tls://dns.google")?
            .build()?
            .ok_or("dns settings disabled")?;
        let rendered = render_dns_object(&dns);
        assert_eq!(rendered.domain_resolver.as_deref(), Some("nameserver-0"));
        let json = serde_json::to_string(&rendered.block)?;
        // The resolver address keeps its port in the `server` field while the
        // domain-hosted fallback wires `domain_resolver` onto it.
        assert!(json.contains("\"server\":\"8.8.8.8:53\""));
        assert!(json.contains("\"domain_resolver\":\"nameserver-0\""));
        Ok(())
    }

    #[test]
    fn b1_doh_path_is_rendered() -> Result<(), Box<dyn std::error::Error>> {
        // B1: pre-fix the renderer dropped the DoH path and dialed `/`.
        let dns = DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("https://dns.google/dns-query")?
            .push_default("223.5.5.5")?
            .build()?
            .ok_or("dns settings disabled")?;
        let rendered = render_dns_object(&dns);
        let json = serde_json::to_string(&rendered.block)?;
        assert!(json.contains("\"type\":\"https\""));
        assert!(json.contains("\"path\":\"/dns-query\""));
        assert!(json.contains("\"server\":\"dns.google\""));
        // A bare IP nameserver must not grow a path key.
        let plain = DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("8.8.8.8")?
            .build()?
            .ok_or("dns settings disabled")?;
        let json = serde_json::to_string(&render_dns_object(&plain).block)?;
        assert!(!json.contains("\"path\""));
        Ok(())
    }

    #[test]
    fn b4_direct_group_is_the_preferred_resolver() -> Result<(), Box<dyn std::error::Error>> {
        // B4: default_domain_resolver bootstraps domain-hosted servers; the
        // direct group must win over the nameserver group, which can itself
        // be domain-hosted (electing it would deadlock the bootstrap).
        let dns = DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("tls://dns.google")?
            .push_direct("223.5.5.5")?
            .build()?
            .ok_or("dns settings disabled")?;
        let rendered = render_dns_object(&dns);
        assert_eq!(rendered.domain_resolver.as_deref(), Some("direct-0"));
        let json = serde_json::to_string(&rendered.block)?;
        assert!(json.contains("\"domain_resolver\":\"direct-0\""));
        Ok(())
    }
}
