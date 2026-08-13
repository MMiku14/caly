//! DNS configuration sourced from environment variables.
//!
//! Lets the daemon opt into a rendered DNS block without a full config-input
//! pipeline. Set `CALY_DNS_ENABLE=true` and, optionally, the comma-separated
//! server lists and fake-ip range. Any invalid value disables DNS rather than
//! failing the boot.

use caly_dns::{DnsError, DnsMode, DnsSettings, DnsSettingsBuilder};

/// Reads DNS settings from the real process environment.
pub fn dns_from_env() -> Option<DnsSettings> {
    dns_from_provider(|name| std::env::var(name).ok())
}

/// Builds DNS settings from an injectable environment lookup. Pure and
/// testable without mutating the real process environment.
pub fn dns_from_provider(get: impl Fn(&str) -> Option<String>) -> Option<DnsSettings> {
    if !flag(get("CALY_DNS_ENABLE")) {
        return None;
    }
    let mode = match get("CALY_DNS_MODE").as_deref() {
        Some("fake-ip") | None => DnsMode::FakeIp,
        Some("standard") => DnsMode::Standard,
        Some("redir-host") => DnsMode::RedirHost,
        Some(_) => return None,
    };
    let mut builder = DnsSettingsBuilder::new().enabled(true).mode(mode);
    for value in split_list(get("CALY_DNS_NAMESERVERS")) {
        builder = push_logged(
            builder,
            "CALY_DNS_NAMESERVERS",
            &value,
            DnsSettingsBuilder::push_nameserver,
        )?;
    }
    for value in split_list(get("CALY_DNS_FALLBACK")) {
        builder = push_logged(
            builder,
            "CALY_DNS_FALLBACK",
            &value,
            DnsSettingsBuilder::push_fallback,
        )?;
    }
    for value in split_list(get("CALY_DNS_DIRECT")) {
        builder = push_logged(
            builder,
            "CALY_DNS_DIRECT",
            &value,
            DnsSettingsBuilder::push_direct,
        )?;
    }
    for value in split_list(get("CALY_DNS_DEFAULT")) {
        builder = push_logged(
            builder,
            "CALY_DNS_DEFAULT",
            &value,
            DnsSettingsBuilder::push_default,
        )?;
    }
    if let Some(range) = get("CALY_DNS_FAKEIP_RANGE") {
        builder = builder.fake_ip_range(&range).ok()?;
    }
    builder.build().ok().flatten()
}

/// Applies one group push, logging the rejecting entry before DNS is
/// disabled. The builder now reports capacity overflow as an error
/// (`DnsError::TooManyNameservers`) instead of silently truncating, so an
/// over-long `CALY_DNS_*` list disables DNS *visibly* rather than dropping
/// resolvers the operator listed.
fn push_logged(
    builder: DnsSettingsBuilder,
    group: &str,
    value: &str,
    push: fn(DnsSettingsBuilder, &str) -> Result<DnsSettingsBuilder, DnsError>,
) -> Option<DnsSettingsBuilder> {
    match push(builder, value) {
        Ok(next) => Some(next),
        Err(error) => {
            tracing::warn!(%group, %value, %error, "invalid CALY_DNS_* entry; kernel DNS disabled");
            None
        }
    }
}

fn flag(value: Option<String>) -> bool {
    matches!(value.as_deref(), Some("true" | "1"))
}

fn split_list(value: Option<String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn provider<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<&str, String> = values
            .iter()
            .map(|(key, value)| (*key, (*value).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn disabled_when_flag_unset() {
        assert!(dns_from_provider(provider(&[])).is_none());
        assert!(dns_from_provider(provider(&[("CALY_DNS_ENABLE", "false")])).is_none());
    }

    #[test]
    fn invalid_mode_fails_closed() {
        assert!(
            dns_from_provider(provider(&[
                ("CALY_DNS_ENABLE", "true"),
                ("CALY_DNS_MODE", "bogus"),
            ]))
            .is_none()
        );
    }

    #[test]
    fn enabled_builds_settings() -> Result<(), &'static str> {
        let dns = dns_from_provider(provider(&[
            ("CALY_DNS_ENABLE", "true"),
            ("CALY_DNS_MODE", "fake-ip"),
            ("CALY_DNS_NAMESERVERS", "8.8.8.8, 1.1.1.1"),
            ("CALY_DNS_FALLBACK", "tls://dns.google"),
            ("CALY_DNS_FAKEIP_RANGE", "198.18.0.1/16"),
        ]))
        .ok_or("dns settings unexpectedly absent")?;
        assert!(dns.enabled());
        assert_eq!(dns.nameservers().len(), 2);
        assert_eq!(dns.fallback().len(), 1);
        assert_eq!(dns.mode(), DnsMode::FakeIp);
        Ok(())
    }

    #[test]
    fn missing_nameserver_fails_closed() {
        assert!(dns_from_provider(provider(&[("CALY_DNS_ENABLE", "true")])).is_none());
    }

    #[test]
    fn overlong_nameserver_list_fails_closed_instead_of_truncating() {
        let servers = (0..=caly_dns::MAX_DNS_SERVERS)
            .map(|index| format!("10.0.{index}.1"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            dns_from_provider(provider(&[
                ("CALY_DNS_ENABLE", "true"),
                ("CALY_DNS_NAMESERVERS", servers.as_str()),
            ]))
            .is_none(),
            "17 env nameservers must disable DNS (with a warn), not silently drop one"
        );
    }
}
