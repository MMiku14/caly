use super::*;

#[test]
fn minimal_sing_box_json_is_valid() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer.render_tuned(&SingBoxBaseTuning::default())?;
    renderer.validate_bytes(&bytes)
}

#[test]
fn tun_inbound_renders_stack_route_and_interface() -> Result<(), RenderFailure> {
    use caly_domain::{TunConfig, TunStack};
    let tun = TunConfig::new(TunStack::System, true, true, 1_400)
        .map_err(|_| crate::config_failure("tun invalid", "inspect"))?;
    let json = SingBoxConfigRenderer.render_tuned(&SingBoxBaseTuning {
        tun: Some(tun),
        tun_interface: "caly0".to_owned(),
        ..SingBoxBaseTuning::default()
    })?;
    let text = String::from_utf8_lossy(&json);
    assert!(text.contains("\"type\":\"tun\""));
    assert!(text.contains("\"interface_name\":\"caly0\""));
    assert!(text.contains("\"address\":[\"172.18.0.1/30\",\"fdfe:dcba:9876::1/126\"]"));
    assert!(text.contains("\"stack\":\"system\""));
    assert!(text.contains("\"auto_route\":true"));
    assert!(text.contains("\"strict_route\":true"));
    assert!(text.contains("\"mtu\":1400"));
    Ok(())
}

#[test]
fn secret_is_embedded_in_clash_api_block() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer.render_tuned(&SingBoxBaseTuning {
        secret: Some("abc123".to_owned()),
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| crate::config_failure("not utf8", "regenerate"))?;
    assert!(text.contains("\"secret\":\"abc123\""));
    renderer.validate_bytes(&bytes)
}

#[test]
fn no_secret_omits_the_field() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer.render_tuned(&SingBoxBaseTuning::default())?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| crate::config_failure("not utf8", "regenerate"))?;
    assert!(!text.contains("\"secret\""));
    Ok(())
}

#[test]
fn empty_secret_is_omitted_not_rejected() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer.render_tuned(&SingBoxBaseTuning {
        secret: Some(String::new()),
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| crate::config_failure("not utf8", "regenerate"))?;
    assert!(!text.contains("\"secret\""));
    renderer.validate_bytes(&bytes)
}

#[test]
fn renders_transparent_redirect_and_tproxy_inbounds() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let redirect = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        transparent_port: 7892,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(redirect.as_slice()).unwrap();
    assert!(text.contains("\"type\":\"redirect\""));
    assert!(text.contains("\"listen_port\":7892"));
    renderer.validate_bytes(&redirect)?;

    let tproxy = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        transparent_port: 7893,
        transparent_tproxy: true,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(tproxy.as_slice()).unwrap();
    assert!(text.contains("\"type\":\"tproxy\""));
    assert!(text.contains("\"listen_port\":7893"));
    renderer.validate_bytes(&tproxy)
}

#[test]
fn renders_sniff_flags_on_inbounds_when_enabled() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let enabled = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        transparent_port: 7892,
        sniff: SniffOptions {
            enabled: true,
            override_destination: true,
        },
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(enabled.as_slice()).unwrap();
    assert!(text.contains("\"sniff\":true"));
    assert!(text.contains("\"sniff_override_destination\":true"));
    renderer.validate_bytes(&enabled)?;

    let disabled = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        transparent_port: 7892,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(disabled.as_slice()).unwrap();
    assert!(!text.contains("\"sniff\":true"));
    renderer.validate_bytes(&disabled)
}

#[test]
fn renders_route_rules_final_and_block_outbound() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let bytes = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        route_rules: vec![crate::rules::RouteRule {
            domain: None,
            domain_keyword: None,
            domain_suffix: Some(vec!["example.com".to_owned()]),
            ip_cidr: None,
            outbound: "block".to_owned(),
            rule_set: None,
            ip_is_private: None,
        }],
        route_final: "block".to_owned(),
        block_outbound: true,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(bytes.as_slice()).unwrap();
    assert!(text.contains("\"rules\":[{\"domain_suffix\""));
    assert!(text.contains("\"outbound\":\"block\""));
    assert!(text.contains("\"final\":\"block\""));
    // P3b J1: key order inside one object carries no meaning; the block
    // outbound is asserted as its two key/value pairs, not their adjacency.
    assert!(text.contains("\"type\":\"block\""));
    assert!(text.contains("\"tag\":\"block\""));
    renderer.validate_bytes(&bytes)?;

    let without = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(without.as_slice()).unwrap();
    assert!(!text.contains("\"rules\""));
    assert!(!text.contains("\"type\":\"block\""));
    assert!(text.contains("\"final\":\"direct\""));
    renderer.validate_bytes(&without)
}

#[test]
fn mixed_inbound_binds_per_allow_lan_matrix() -> Result<(), RenderFailure> {
    let renderer = SingBoxConfigRenderer;
    let loopback = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(loopback.as_slice()).unwrap();
    assert!(text.contains("\"listen\":\"127.0.0.1\""));

    let all = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        allow_lan: true,
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(all.as_slice()).unwrap();
    assert!(text.contains("\"listen\":\"0.0.0.0\""));

    let one_interface = renderer.render_tuned(&SingBoxBaseTuning {
        mixed_port: 7890,
        allow_lan: true,
        bind_address: "192.168.1.100".to_owned(),
        ..SingBoxBaseTuning::default()
    })?;
    let text = std::str::from_utf8(one_interface.as_slice()).unwrap();
    assert!(text.contains("\"listen\":\"192.168.1.100\""));
    renderer.validate_bytes(&one_interface)
}

#[test]
fn tun_without_dns_injects_fallback_dns_block() -> Result<(), RenderFailure> {
    // W3a 兜底: a TUN inbound with no operator DNS section must still
    // get a DNS block, or every hijacked query would blackhole.
    use caly_domain::{TunConfig, TunStack};
    let tun = TunConfig::new(TunStack::Mixed, true, false, 1_500)
        .map_err(|_| crate::config_failure("tun invalid", "inspect"))?;
    let json = SingBoxConfigRenderer.render_tuned(&SingBoxBaseTuning {
        tun: Some(tun),
        tun_interface: "caly0".to_owned(),
        ..SingBoxBaseTuning::default()
    })?;
    let text = String::from_utf8_lossy(&json);
    assert!(
        text.contains("\"dns\":{"),
        "fallback DNS block missing: {text}"
    );
    assert!(
        text.contains("\"type\":\"fakeip\""),
        "fakeip server missing"
    );
    assert!(text.contains("28.0.0.1/8"), "fake-ip range missing");
    assert!(text.contains("223.5.5.5"), "upstream missing");
    assert!(text.contains("8.8.8.8"), "second upstream missing");
    Ok(())
}

#[test]
fn tun_with_operator_dns_keeps_operator_block() -> Result<(), RenderFailure> {
    // An explicit DNS section wins over the fallback.
    use caly_dns::DnsSettingsBuilder;
    use caly_domain::{TunConfig, TunStack};
    let tun = TunConfig::new(TunStack::Mixed, true, false, 1_500)
        .map_err(|_| crate::config_failure("tun invalid", "inspect"))?;
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .push_nameserver("119.29.29.29")
        .unwrap()
        .build()
        .unwrap();
    let json = SingBoxConfigRenderer.render_tuned(&SingBoxBaseTuning {
        tun: Some(tun),
        tun_interface: "caly0".to_owned(),
        dns,
        ..SingBoxBaseTuning::default()
    })?;
    let text = String::from_utf8_lossy(&json);
    assert!(text.contains("119.29.29.29"), "operator upstream missing");
    assert!(
        !text.contains("223.5.5.5"),
        "fallback leaked over operator DNS"
    );
    Ok(())
}

#[test]
fn no_tun_without_dns_renders_no_dns_block() -> Result<(), RenderFailure> {
    // Proxy-only mode: the system resolver handles DNS; no block, no
    // fallback injection.
    let json = SingBoxConfigRenderer.render_tuned(&SingBoxBaseTuning::default())?;
    let text = String::from_utf8_lossy(&json);
    assert!(!text.contains("\"dns\":{"), "unexpected DNS block: {text}");
    Ok(())
}
