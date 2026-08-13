//! SIP008 JSON subscription parsing.
//!
//! SIP008 distributes Shadowsocks servers as JSON: either a bare array of
//! server objects or a `{"version":1,"servers":[...]}` envelope. Each entry
//! maps onto a Shadowsocks node; entries with unsupported ciphers or invalid
//! fields are skipped and counted rather than failing the whole document.

use core::num::NonZeroU16;

use caly_domain::{
    Credential, DialableNode, Endpoint, EndpointHost, NodeBuilder, NodeDisplayName, NodeSource,
    Protocol, SubscriptionId, sanitized_display_name,
};
use serde_norway::{Mapping, Value};

use super::special_uri::shadowsocks_cipher;

/// Parses a SIP008 JSON document, skipping unusable entries. Returns the
/// parsed nodes and the number of skipped entries.
pub fn parse_sip008(source: &str, subscription: SubscriptionId) -> (Vec<DialableNode>, usize) {
    let Ok(value) = serde_norway::from_str::<Value>(source) else {
        return (Vec::new(), 1);
    };
    let servers: &[Value] = match &value {
        Value::Sequence(items) => items,
        Value::Mapping(mapping) => match mapping.get("servers") {
            Some(Value::Sequence(items)) => items,
            _ => return (Vec::new(), 1),
        },
        _ => return (Vec::new(), 1),
    };
    let mut nodes = Vec::new();
    let mut skipped = 0_usize;
    for entry in servers {
        match parse_entry(entry, subscription) {
            Ok(node) => nodes.push(node),
            Err(()) => skipped = skipped.saturating_add(1),
        }
    }
    (nodes, skipped)
}

/// Maps one SIP008 server object onto a Shadowsocks node.
fn parse_entry(entry: &Value, subscription: SubscriptionId) -> Result<DialableNode, ()> {
    let Value::Mapping(fields) = entry else {
        return Err(());
    };
    let host = text(fields, "server").ok_or(())?;
    let port = port_value(fields)?;
    let method = shadowsocks_cipher(&text(fields, "method").ok_or(())?).map_err(|_| ())?;
    let password = Credential::new(text(fields, "password").ok_or(())?).map_err(|_| ())?;
    let endpoint = endpoint(host, port)?;
    let name = node_name(fields, &endpoint)?;
    NodeBuilder::new(
        name,
        endpoint,
        Protocol::Shadowsocks {
            method,
            password,
            plugin: None,
        },
        NodeSource::Subscription(subscription),
    )
    .build()
    .map_err(|_| ())
}

fn text(fields: &Mapping, key: &str) -> Option<String> {
    match fields.get(key)? {
        Value::String(value) => Some(value.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn port_value(fields: &Mapping) -> Result<u16, ()> {
    match fields.get("server_port").ok_or(())? {
        Value::Number(number) => {
            let value = number.as_u64().ok_or(())?;
            u16::try_from(value).map_err(|_| ())
        }
        Value::String(value) => value.parse::<u16>().map_err(|_| ()),
        _ => Err(()),
    }
}

fn endpoint(host: String, port: u16) -> Result<Endpoint, ()> {
    let host = EndpointHost::new(host).map_err(|_| ())?;
    let port = NonZeroU16::new(port).ok_or(())?;
    Ok(Endpoint::new(host, port))
}

/// Entry display name: `remarks`/`remark` when present, else `host:port`.
fn node_name(fields: &Mapping, endpoint: &Endpoint) -> Result<NodeDisplayName, ()> {
    let name = text(fields, "remarks")
        .or_else(|| text(fields, "remark"))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{}:{}", endpoint.host().as_str(), endpoint.port().get()));
    sanitized_display_name(name).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub() -> SubscriptionId {
        SubscriptionId::from_bytes([0; 16])
    }

    #[test]
    fn parses_bare_array_form() {
        let source = r#"[
            {"server":"192.0.2.1","server_port":8388,"method":"aes-256-gcm","password":"pw1","remarks":"node-a"},
            {"server":"192.0.2.2","server_port":8389,"method":"aes-128-gcm","password":"pw2"}
        ]"#;
        let (nodes, skipped) = parse_sip008(source, sub());
        assert_eq!(nodes.len(), 2);
        assert_eq!(skipped, 0);
        assert_eq!(nodes[0].endpoint().host().as_str(), "192.0.2.1");
        assert_eq!(nodes[0].endpoint().port().get(), 8388);
        assert_eq!(nodes[1].endpoint().host().as_str(), "192.0.2.2");
        assert_eq!(nodes[1].endpoint().port().get(), 8389);
    }

    #[test]
    fn parses_versioned_envelope_form() {
        let source = r#"{"version":1,"servers":[
            {"server":"example.com","server_port":443,"method":"chacha20-ietf-poly1305","password":"pw"}
        ]}"#;
        let (nodes, skipped) = parse_sip008(source, sub());
        assert_eq!(nodes.len(), 1);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn skips_unsupported_cipher_and_bad_entries() {
        let source = r#"[
            {"server":"192.0.2.1","server_port":8388,"method":"rc4-md5","password":"pw"},
            {"server":"192.0.2.2","server_port":8389,"method":"aes-256-gcm","password":"pw"},
            "not-an-object"
        ]"#;
        let (nodes, skipped) = parse_sip008(source, sub());
        assert_eq!(nodes.len(), 1);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn rejects_non_sip008_json() {
        let (nodes, skipped) = parse_sip008(r#"{"unrelated":true}"#, sub());
        assert!(nodes.is_empty());
        assert_eq!(skipped, 1);
    }
}
