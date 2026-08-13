use super::*;
use caly_dns::DnsSettingsBuilder;

#[test]
fn minimal_yaml_contains_required_mihomo_fields() -> Result<(), MihomoConfigError> {
    let renderer = MihomoConfigRenderer;
    let bytes = renderer.render(&MihomoConfigSettings::default())?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(text.contains("mixed-port: 7890"));
    assert!(text.contains("external-controller: 127.0.0.1:9090"));
    assert!(!text.contains("dns:"));
    Ok(())
}

#[test]
fn secret_is_embedded_and_empty_secret_rejected() -> Result<(), MihomoConfigError> {
    let settings = MihomoConfigSettings {
        secret: Some("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6".to_owned()),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&settings)?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(text.contains("secret: a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6"));
    let empty = MihomoConfigSettings {
        secret: Some(String::new()),
        ..MihomoConfigSettings::default()
    };
    assert_eq!(
        MihomoConfigRenderer.render(&empty),
        Err(MihomoConfigError::InvalidSecret)
    );
    Ok(())
}

#[test]
fn zero_port_is_rejected() {
    let settings = MihomoConfigSettings {
        mixed_port: 0,
        ..MihomoConfigSettings::default()
    };
    assert_eq!(
        MihomoConfigRenderer.render(&settings),
        Err(MihomoConfigError::InvalidPort)
    );
}

#[test]
fn renders_a_tun_block_when_configured() -> Result<(), MihomoConfigError> {
    use caly_domain::{TunConfig, TunStack};
    let tun = TunConfig::new(TunStack::System, true, true, 1_400)
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    let settings = MihomoConfigSettings {
        tun: Some(tun),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&settings)?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(text.contains("tun:"));
    assert!(text.contains("device: caly0"));
    assert!(text.contains("stack: system"));
    assert!(text.contains("auto-route: true"));
    assert!(text.contains("strict-route: true"));
    assert!(text.contains("auto-detect-interface: true"));
    assert!(text.contains("dns-hijack:"));
    assert!(text.contains("mtu: 1400"));
    Ok(())
}

#[test]
fn appends_a_subscription_proxy_section_when_present() -> Result<(), MihomoConfigError> {
    let section = BoundedText::new(
        "proxies:\n  - name: \"node\"\n    type: ss\n    server: 1.2.3.4\n    port: 443\n\
         proxy-groups:\n  - name: \"AUTO\"\n    type: url-test\n    proxies:\n      - \"node\"\n\
         rules:\n  - MATCH,AUTO\n"
            .to_owned(),
    )
    .map_err(|_| MihomoConfigError::OutputTooLarge)?;
    let settings = MihomoConfigSettings {
        proxies: Some(section),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&settings)?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(text.contains("proxies:"));
    assert!(text.contains("type: ss"));
    assert!(text.contains("rules:"));
    Ok(())
}

#[test]
fn renders_a_dns_block() -> Result<(), Box<dyn std::error::Error>> {
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")?
        .push_fallback("tls://dns.google")?
        .push_default("223.5.5.5")?
        .fake_ip_range("198.18.0.1/16")?
        .build()?
        .ok_or("dns settings disabled")?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&settings)
        .map_err(|_| "render failed")?;
    let text = std::str::from_utf8(bytes.as_slice()).map_err(|_| "not utf8")?;
    assert!(text.contains("dns:"));
    assert!(text.contains("enhanced-mode: fake-ip"));
    assert!(text.contains("    - 8.8.8.8"));
    assert!(text.contains("fake-ip-range: 198.18.0.1/16"));
    Ok(())
}

#[test]
fn b4_direct_nameserver_group_is_rendered() -> Result<(), Box<dyn std::error::Error>> {
    // B4: the direct group was collected by the model but never rendered —
    // a dead configuration. It now lands on mihomo `direct-nameserver`.
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .push_nameserver("8.8.8.8")?
        .push_direct("223.5.5.5")?
        .push_default("119.29.29.29")?
        .build()?
        .ok_or("dns settings disabled")?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&settings)
        .map_err(|_| "render failed")?;
    let text = std::str::from_utf8(bytes.as_slice()).map_err(|_| "not utf8")?;
    assert!(text.contains("  direct-nameserver:\n    - 223.5.5.5"));
    assert!(text.contains("  default-nameserver:\n    - 119.29.29.29"));
    Ok(())
}

#[test]
fn renders_redir_host_dns_mode() -> Result<(), Box<dyn std::error::Error>> {
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::RedirHost)
        .push_nameserver("8.8.8.8")?
        .build()?
        .ok_or("dns settings disabled")?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&settings)
        .map_err(|_| "render failed")?;
    let text = std::str::from_utf8(bytes.as_slice()).map_err(|_| "not utf8")?;
    assert!(text.contains("enhanced-mode: redir-host"));
    // redir-host must not emit a fake-ip range.
    assert!(!text.contains("fake-ip-range"));
    Ok(())
}

#[test]
fn renders_transparent_redir_and_tproxy_ports() -> Result<(), Box<dyn std::error::Error>> {
    let redir = MihomoConfigSettings {
        transparent_port: 7892,
        transparent_tproxy: false,
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&redir)
        .map_err(|_| "render failed")?;
    let text = std::str::from_utf8(bytes.as_slice()).map_err(|_| "not utf8")?;
    assert!(text.contains("redir-port: 7892"));
    assert!(!text.contains("tproxy-port"));

    let tproxy = MihomoConfigSettings {
        transparent_port: 7893,
        transparent_tproxy: true,
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&tproxy)
        .map_err(|_| "render failed")?;
    let text = std::str::from_utf8(bytes.as_slice()).map_err(|_| "not utf8")?;
    assert!(text.contains("tproxy-port: 7893"));
    assert!(!text.contains("redir-port"));
    Ok(())
}

#[test]
fn renders_a_sniffer_block_when_configured() -> Result<(), MihomoConfigError> {
    let settings = MihomoConfigSettings {
        sniffer: Some(MihomoSniffer {
            override_destination: true,
            parse_pure_ip: true,
            force_dns_mapping: false,
            http_ports: vec!["80".to_owned(), "8080-8880".to_owned()],
            tls_ports: vec!["443".to_owned()],
            quic_ports: Vec::new(),
        }),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&settings)?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(text.contains("sniffer:\n  enable: true"));
    assert!(text.contains("override-destination: true"));
    assert!(text.contains("force-dns-mapping: false"));
    assert!(text.contains("HTTP:\n      ports: [80, 8080-8880]"));
    assert!(text.contains("TLS:\n      ports: [443]"));
    assert!(!text.contains("QUIC:"));
    Ok(())
}

#[test]
fn omits_the_sniffer_block_by_default() -> Result<(), MihomoConfigError> {
    let bytes = MihomoConfigRenderer.render(&MihomoConfigSettings::default())?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(!text.contains("sniffer:"));
    Ok(())
}

#[test]
fn renders_dns_filters_listen_and_ipv6() -> Result<(), Box<dyn std::error::Error>> {
    use caly_dns::{DnsMode, DnsSettingsBuilder, FallbackFilter};
    let filter = FallbackFilter::new(
        true,
        Some("CN".to_owned()),
        vec!["240.0.0.0/4".to_owned()],
        vec!["+.google.com".to_owned()],
    )
    .unwrap();
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")?
        .push_fallback("1.1.1.1")?
        .fake_ip_range("198.18.0.1/16")?
        .fake_ip_filter(&["+.local".to_owned(), "+.stun.*".to_owned()])
        .unwrap()
        .fallback_filter(filter)
        .ipv6(true)
        .listen("127.0.0.1:1053")
        .unwrap()
        .build()?
        .ok_or("dns disabled")?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&settings)
        .map_err(|e| format!("{e}"))?;
    let text = std::str::from_utf8(bytes.as_slice())?;
    assert!(text.contains("listen: 127.0.0.1:1053"));
    assert!(text.contains("ipv6: true"));
    assert!(text.contains("fake-ip-filter:"));
    assert!(text.contains("- '+.local'"));
    assert!(text.contains("fallback-filter:"));
    assert!(text.contains("geoip: true"));
    assert!(text.contains("geoip-code: CN"));
    assert!(text.contains("- 240.0.0.0/4"));
    assert!(text.contains("- '+.google.com'"));
    Ok(())
}

#[test]
fn renders_bind_address_only_with_allow_lan() -> Result<(), MihomoConfigError> {
    let lan = MihomoConfigSettings {
        allow_lan: true,
        bind_address: "192.168.1.100".to_owned(),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&lan)?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(text.contains("bind-address: 192.168.1.100"));
    let loopback = MihomoConfigSettings::default();
    let bytes = MihomoConfigRenderer.render(&loopback)?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(!text.contains("bind-address"));
    Ok(())
}

#[test]
fn fake_ip_filter_auto_merges_node_domains_and_skips_ips() -> Result<(), Box<dyn std::error::Error>>
{
    use caly_dns::{DnsMode, DnsSettingsBuilder};
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")?
        .fake_ip_filter(&["+.local".to_owned()])
        .map_err(|_| "filter")?
        .fake_ip_range("198.18.0.1/16")?
        .build()?
        .ok_or("dns settings disabled")?;
    let proxies = BoundedText::new(
        "proxies:\n    - name: a\n      server: www.true.th\n      port: 443\n    - name: b\n      server: \"cf4.danfeng.eu.org\"\n      port: 80\n    - name: c\n      server: 8.8.8.8\n      port: 80\n".to_owned(),
    ).map_err(|_| "bounded")?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        proxies: Some(proxies),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&settings)
        .map_err(|_| "render")?;
    let text = std::str::from_utf8(bytes.as_slice())?;
    assert!(text.contains("fake-ip-filter:"));
    assert!(text.contains("    - '+.local'"));
    assert!(text.contains("    - '+.www.true.th'"), "node domain merged");
    assert!(
        text.contains("    - '+.cf4.danfeng.eu.org'"),
        "quoted domain merged"
    );
    assert!(!text.contains("'+.8.8.8.8'"), "bare IPs never filtered");
    Ok(())
}

#[test]
fn server_domains_dedupes_and_drops_user_filter_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    use caly_dns::{DnsMode, DnsSettingsBuilder};
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")?
        .fake_ip_range("198.18.0.1/16")?
        // user already covers a node domain (wildcard form)
        .fake_ip_filter(&["*.cdn.example.org".to_owned()])
        .map_err(|_| "filter")?
        .build()?
        .ok_or("dns settings disabled")?;
    let proxies = BoundedText::new(
        "proxies:\n    - name: x\n      server: cdn.example.org\n      port: 443\n".to_owned(),
    )
    .map_err(|_| "bounded")?;
    let settings = MihomoConfigSettings {
        dns: Some(dns),
        proxies: Some(proxies),
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer
        .render(&settings)
        .map_err(|_| "render")?;
    let text = std::str::from_utf8(bytes.as_slice())?;
    assert!(
        !text.contains("'+.cdn.example.org'"),
        "user-filtered domain must not be re-added"
    );
    Ok(())
}

#[test]
fn tun_without_dns_injects_fallback_dns_block() -> Result<(), MihomoConfigError> {
    // W3a 兜底: `dns-hijack: [any:53]` with no `dns:` section would
    // drop every hijacked query; the renderer injects the default.
    use caly_domain::{TunConfig, TunStack};
    let tun = Some(
        TunConfig::new(TunStack::Mixed, true, false, 1_500)
            .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?,
    );
    let settings = MihomoConfigSettings {
        tun,
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&settings)?;
    let yaml = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(yaml.contains("dns:"), "fallback DNS block missing: {yaml}");
    assert!(yaml.contains("enhanced-mode: fake-ip"), "fake-ip missing");
    assert!(yaml.contains("28.0.0.1/8"), "fake-ip range missing");
    assert!(yaml.contains("223.5.5.5"), "upstream missing");
    Ok(())
}

#[test]
fn tun_with_operator_dns_keeps_operator_block() -> Result<(), MihomoConfigError> {
    use caly_dns::DnsSettingsBuilder;
    use caly_domain::{TunConfig, TunStack};
    let tun = Some(
        TunConfig::new(TunStack::Mixed, true, false, 1_500)
            .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?,
    );
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .push_nameserver("119.29.29.29")
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?
        .build()
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    let settings = MihomoConfigSettings {
        dns,
        tun,
        ..MihomoConfigSettings::default()
    };
    let bytes = MihomoConfigRenderer.render(&settings)?;
    let yaml = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(yaml.contains("119.29.29.29"), "operator upstream missing");
    assert!(
        !yaml.contains("223.5.5.5"),
        "fallback leaked over operator DNS"
    );
    Ok(())
}

#[test]
fn no_tun_without_dns_renders_no_dns_block() -> Result<(), MihomoConfigError> {
    let bytes = MihomoConfigRenderer.render(&MihomoConfigSettings::default())?;
    let yaml = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| MihomoConfigError::InvalidGeneratedConfig)?;
    assert!(
        !yaml
            .lines()
            .any(|line| line.trim_start().starts_with("dns:")),
        "unexpected DNS block: {yaml}"
    );
    Ok(())
}
