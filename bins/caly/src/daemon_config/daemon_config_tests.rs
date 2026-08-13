//! Unit tests for layered daemon configuration resolution.

use super::*;
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| 0, |duration| duration.as_nanos());
    let dir = std::env::temp_dir().join(format!("caly-daemon-config-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

#[test]
fn absent_config_yields_no_override() -> Result<(), String> {
    let root = temp_root("absent");
    assert_eq!(
        core_override_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        None
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_core_selects_mihomo() -> Result<(), String> {
    let root = temp_root("mihomo");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\n",
    )
    .map_err(|e| e.to_string())?;
    assert_eq!(
        core_override_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        Some(CoreKind::Mihomo)
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_core_selects_sing_box() -> Result<(), String> {
    let root = temp_root("sing");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: sing-box\n",
    )
    .map_err(|e| e.to_string())?;
    assert_eq!(
        core_override_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        Some(CoreKind::SingBox)
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_xray_is_rejected_as_unsupported() {
    let root = temp_root("xray");
    fs::write(root.join("config.yaml"), "schema_version: 1\ncore: xray\n").unwrap_or_default();
    assert!(matches!(
        core_override_from(root.clone()),
        Err(DaemonConfigError::UnsupportedCore)
    ));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn invalid_config_fails_boot() {
    let root = temp_root("bad");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: [not-a-scalar\n",
    )
    .unwrap_or_default();
    assert!(matches!(
        core_override_from(root.clone()),
        Err(DaemonConfigError::Layered(_))
    ));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn absent_config_yields_no_tcp_listen() -> Result<(), String> {
    let root = temp_root("listen-absent");
    assert_eq!(
        listen_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        None
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn non_loopback_listen_fails_closed() {
    let root = temp_root("listen-remote");
    // Non-loopback is only served when TLS AND the admission
    // token are BOTH configured; anything less must fail closed
    // at boot (#17). TLS + auth but NO certificate/key material
    // still refuses.
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndaemon:\n  listen: 192.168.1.10:17890\n  tls_enabled: true\n  auth_token: secret\n",
    )
    .unwrap_or_default();
    // Fail closed: the strict layered load itself rejects the
    // TLS-without-material config (`TlsMaterialMissing`), so the
    // address never even reaches the listen gate. Either a
    // schema-layer or a listen-layer error is correct here —
    // the contract is "boot refuses", which both honour.
    assert!(
        matches!(
            listen_from(root.clone()),
            Err(DaemonConfigError::RemoteListenUnavailable | DaemonConfigError::Layered(_))
        ),
        "remote listen with incomplete TLS material must fail closed"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn non_loopback_listen_with_tls_and_auth_is_served() {
    let root = temp_root("listen-remote-ok");
    // The full contract satisfied: TLS enabled with cert/key
    // paths plus an admission token. The boot path must honour
    // the address (the schema validates the same combination).
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndaemon:\n  listen: 192.168.1.10:17890\n  tls_enabled: true\n  tls_cert_path: /tmp/daemon.pem\n  tls_key_path: /tmp/daemon-key.pem\n  auth_token: secret\n",
    )
    .unwrap_or_default();
    assert_eq!(
        listen_from(root.clone()).map_err(|error| format!("{error:?}")),
        Ok(Some(
            "192.168.1.10:17890"
                .parse()
                .expect("static socket addr parses")
        ))
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn config_listen_parses_socket_address() -> Result<(), String> {
    let root = temp_root("listen");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndaemon:\n  listen: 127.0.0.1:18080\n",
    )
    .map_err(|e| e.to_string())?;
    assert_eq!(
        listen_from(root.clone()).map_err(|e| format!("{e:?}"))?,
        Some(
            "127.0.0.1:18080"
                .parse::<std::net::SocketAddr>()
                .map_err(|e| e.to_string())?
        )
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn config_tun_mtu_is_parsed() -> Result<(), String> {
    let root = temp_root("tun");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ntun:\n  enabled: true\n  mtu: 1400\n",
    )
    .map_err(|e| e.to_string())?;
    let config = load_from(root.clone())
        .map_err(|e| format!("{e:?}"))?
        .ok_or("no config")?;
    assert_eq!(config.tun.mtu, 1400);
    assert!(config.tun.enabled);
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn controllers_from_config_falls_back_to_defaults() -> Result<(), String> {
    let root = temp_root("controllers");
    // Absent config -> built-in defaults.
    let absent = controllers_from(root.clone()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(absent.mihomo, "127.0.0.1:9090");
    assert_eq!(absent.sing_box, "127.0.0.1:9091");
    // Present config -> configured addresses.
    fs::write(
            root.join("config.yaml"),
            "schema_version: 1\ncore: mihomo\ncontrollers:\n  mihomo: 127.0.0.1:9200\n  sing_box: 127.0.0.1:9201\n",
        )
        .map_err(|e| e.to_string())?;
    let configured = controllers_from(root.clone()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(configured.mihomo, "127.0.0.1:9200");
    assert_eq!(configured.sing_box, "127.0.0.1:9201");
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn tun_config_maps_stack_and_routes() -> Result<(), String> {
    let root = temp_root("tunstack");
    fs::write(
            root.join("config.yaml"),
            "schema_version: 1\ncore: mihomo\ntun:\n  enabled: true\n  mtu: 1400\n  stack: mixed\n  auto_route: false\n  strict_route: true\n",
        )
        .map_err(|e| e.to_string())?;
    let config = crate::daemon_config::tun_config_from_config(
        crate::daemon_config::load_from(root.clone())
            .map_err(|e| format!("{e:?}"))?
            .as_ref(),
    )
    .ok_or("no tun config")?;
    assert_eq!(config.stack(), caly_domain::TunStack::Mixed);
    assert!(!config.auto_route());
    assert!(config.strict_route());
    assert_eq!(config.mtu(), 1400);
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn kernel_defaults_apply_without_config() {
    let root = temp_root("kernel-absent");
    let kernel = crate::daemon_config::kernel_from(root.clone());
    assert_eq!(kernel.mixed_port, 7890);
    assert!(!kernel.allow_lan);
    assert_eq!(kernel.log_level, "error");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn kernel_values_load_from_config() {
    let root = temp_root("kernel-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nkernel:\n  mixed_port: 8899\n  allow_lan: true\n  log_level: warn\n",
    )
    .unwrap_or_default();
    let kernel = crate::daemon_config::kernel_from(root.clone());
    assert_eq!(kernel.mixed_port, 8899);
    assert!(kernel.allow_lan);
    assert_eq!(kernel.log_level, "warn");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn fetch_policy_defaults_and_overrides() {
    let root = temp_root("fetch-absent");
    let policy = crate::daemon_config::fetch_policy_from(root.clone());
    assert_eq!(policy.connect_timeout.as_millis(), 5000);
    assert_eq!(policy.max_body_bytes, 32 * 1024 * 1024);
    assert!(!policy.redirects_allowed);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("fetch-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nsubscriptions:\n  connect_timeout_ms: 900\n  request_timeout_ms: 2000\n  max_body_mb: 4\n  follow_redirects: true\n",
    )
    .unwrap_or_default();
    let policy = crate::daemon_config::fetch_policy_from(root.clone());
    assert_eq!(policy.connect_timeout.as_millis(), 900);
    assert_eq!(policy.request_timeout.as_millis(), 2000);
    assert_eq!(policy.max_body_bytes, 4 * 1024 * 1024);
    assert!(policy.redirects_allowed);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn system_proxy_endpoint_defaults_to_mixed_port() {
    let root = temp_root("proxy-endpoint-default");
    let (host, port) = crate::daemon_config::system_proxy_endpoint_from(root.clone());
    assert_eq!(host, "127.0.0.1");
    assert_eq!(port, 7890);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("proxy-endpoint-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nkernel:\n  mixed_port: 8899\nsystem_proxy:\n  enabled: false\n  host: 10.0.0.2\n",
    )
    .unwrap_or_default();
    let (host, port) = crate::daemon_config::system_proxy_endpoint_from(root.clone());
    assert_eq!(host, "10.0.0.2");
    assert_eq!(port, 8899);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("proxy-endpoint-port");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nsystem_proxy:\n  enabled: false\n  host: 127.0.0.1\n  port: 1080\n",
    )
    .unwrap_or_default();
    let (host, port) = crate::daemon_config::system_proxy_endpoint_from(root.clone());
    assert_eq!(host, "127.0.0.1");
    assert_eq!(port, 1080);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn dns_settings_resolve_from_config() {
    let root = temp_root("dns-config");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndns:\n  enabled: true\n  mode: standard\n  nameservers:\n    - 8.8.8.8\n",
    )
    .unwrap_or_default();
    let settings = crate::daemon_config::dns_settings_from(root.clone());
    let settings =
        settings.unwrap_or_else(|| panic!("dns settings must resolve from an enabled config"));
    assert!(settings.enabled());
    assert_eq!(settings.nameservers().len(), 1);
    fs::remove_dir_all(&root).ok();

    let root = temp_root("dns-disabled");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndns:\n  enabled: false\n",
    )
    .unwrap_or_default();
    assert!(crate::daemon_config::dns_settings_from(root.clone()).is_none());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn dns_nameservers_lists_configured_resolvers() {
    let root = temp_root("dns-nameservers");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\ndns:\n  enabled: false\n  nameservers:\n    - 9.9.9.9\n",
    )
    .unwrap_or_default();
    // dns_nameservers reads the raw config section even when disabled.
    let config = crate::daemon_config::load_from(root.clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("config must load"));
    assert_eq!(config.dns.nameservers, vec!["9.9.9.9"]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn tun_escalation_maps_config_values() {
    use caly_platform::tun::TunEscalation;
    let root = temp_root("escalation-absent");
    assert_eq!(
        crate::daemon_config::tun_escalation_from(root.clone()),
        TunEscalation::Auto
    );
    fs::remove_dir_all(&root).ok();

    for (value, expected) in [
        ("sudo", TunEscalation::Sudo),
        ("pkexec", TunEscalation::Pkexec),
        ("none", TunEscalation::None),
        ("auto", TunEscalation::Auto),
    ] {
        let root = temp_root(&format!("escalation-{value}"));
        fs::write(
            root.join("config.yaml"),
            format!(
                "schema_version: 1\ncore: mihomo\ntun:\n  enabled: false\n  escalation: {value}\n"
            ),
        )
        .unwrap_or_default();
        assert_eq!(
            crate::daemon_config::tun_escalation_from(root.clone()),
            expected,
            "escalation `{value}` must map correctly"
        );
        fs::remove_dir_all(&root).ok();
    }
}

#[test]
fn kernel_restart_bounds_load_from_config() {
    let root = temp_root("restart-bounds");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nkernel:\n  restart:\n    initial_backoff_ms: 250\n    max_backoff_ms: 4000\n",
    )
    .unwrap_or_default();
    let kernel = crate::daemon_config::kernel_from(root.clone());
    assert_eq!(kernel.restart.initial_backoff_ms, 250);
    assert_eq!(kernel.restart.max_backoff_ms, 4000);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn resolve_daemon_matches_per_field_getters() -> Result<(), String> {
    // A single layered load must derive every boot setting consistently with
    // the per-field getters (the getters themselves re-load, so any drift
    // between the two paths is a real bug).
    let root = temp_root("resolve-single-load");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\n\
         daemon:\n  listen: 127.0.0.1:17890\n\
         controllers:\n  mihomo: 127.0.0.1:9991\n  sing_box: 127.0.0.1:9992\n\
         kernel:\n  mixed_port: 8899\n  restart:\n    initial_backoff_ms: 300\n    max_backoff_ms: 5000\n\
         system_proxy:\n  enabled: false\n  host: 127.0.0.1\n\
         subscriptions:\n  url: https://example.invalid/sub\n",
    )
    .map_err(|e| e.to_string())?;

    let resolved = resolve_daemon(root.clone()).map_err(|e| format!("{e:?}"))?;
    let reloaded = load_from(root.clone()).map_err(|e| format!("{e:?}"))?;

    assert_eq!(
        resolved.core_override,
        core_override_from_config(reloaded.as_ref()).map_err(|e| format!("{e:?}"))?
    );
    assert_eq!(
        resolved.listen,
        listen_from_config(reloaded.as_ref()).map_err(|e| format!("{e:?}"))?
    );
    assert_eq!(
        resolved.controllers,
        controllers_from_config(reloaded.as_ref())
    );
    assert_eq!(
        resolved.binaries,
        core_binaries_from_config(reloaded.as_ref())
    );
    assert_eq!(
        resolved.subscription_urls,
        subscription_urls_from_config(reloaded.as_ref())
    );
    assert_eq!(
        resolved.auto_start_core,
        auto_start_core_from_config(reloaded.as_ref())
    );
    assert_eq!(resolved.tun, tun_config_from_config(reloaded.as_ref()));
    assert_eq!(
        resolved.tuning.dns,
        dns_settings_from_config(reloaded.as_ref())
    );
    assert_eq!(
        resolved.tuning.fetch_policy,
        fetch_policy_from_config(reloaded.as_ref())
    );
    assert_eq!(resolved.tuning.mixed_port, 8899);
    assert_eq!(resolved.tuning.restart_initial_backoff_ms, 300);
    assert_eq!(resolved.tuning.restart_max_backoff_ms, 5000);

    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn resolve_daemon_absent_config_falls_back_to_defaults() -> Result<(), String> {
    let root = temp_root("resolve-absent");
    let resolved = resolve_daemon(root.clone()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(resolved.core_override, None);
    assert_eq!(resolved.listen, None);
    assert_eq!(resolved.tun, None);
    assert_eq!(resolved.subscription_urls, Vec::<String>::new());
    assert!(resolved.auto_start_core);
    assert_eq!(
        resolved.tuning.fetch_policy,
        caly_subscription::FetchPolicy::direct_default()
    );
    fs::remove_dir_all(&root).ok();
    Ok(())
}

#[test]
fn rule_providers_resolve_into_runtime_tuning() -> Result<(), String> {
    use caly_domain::{RuleProviderBehavior, RuleProviderFormat, RuleProviderSource};
    let root = temp_root("rule-providers-tuning");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\n\
         core: mihomo\n\
         rule_providers:\n\
         \x20 - name: my-google\n\
         \x20\x20\x20 type: http\n\
         \x20\x20\x20 behavior: domain\n\
         \x20\x20\x20 format: source\n\
         \x20\x20\x20 url: https://example.com/g.yaml\n\
         \x20\x20\x20 interval_ms: 86400000\n\
         \x20 - name: local\n\
         \x20\x20\x20 type: file\n\
         \x20\x20\x20 behavior: domain_suffix\n\
         \x20\x20\x20 format: source\n\
         \x20\x20\x20 path: /etc/caly/local.yaml\n\
         \x20 - name: inline-ads\n\
         \x20\x20\x20 type: inline\n\
         \x20\x20\x20 behavior: ip_cidr\n\
         \x20\x20\x20 format: binary\n\
         \x20\x20\x20 payload: 10.0.0.0/8\n\
         rules:\n\
         \x20 - RULE-SET,my-google,DIRECT\n\
         \x20 - GEOSITE,private,DIRECT\n\
         \x20 - GEOIP,CN,DIRECT\n\
         \x20 - MATCH,PROXY\n",
    )
    .map_err(|error| error.to_string())?;
    let settings = resolve_daemon(root.clone()).map_err(|error| error.to_string())?;
    fs::remove_dir_all(&root).ok();

    // Three user-declared rule providers reach the runtime tuning bundle
    // in declaration order.
    let tuning = settings.tuning;
    assert_eq!(tuning.rule_providers.len(), 3);
    assert_eq!(tuning.rule_providers[0].name.as_str(), "my-google");
    assert!(matches!(
        tuning.rule_providers[0].source,
        RuleProviderSource::Http { .. }
    ));
    assert!(matches!(
        tuning.rule_providers[0].behavior,
        RuleProviderBehavior::Domain
    ));
    assert!(matches!(
        tuning.rule_providers[0].format,
        RuleProviderFormat::Source
    ));
    assert!(matches!(
        tuning.rule_providers[1].source,
        RuleProviderSource::File { .. }
    ));
    assert!(matches!(
        tuning.rule_providers[1].behavior,
        RuleProviderBehavior::DomainSuffix
    ));
    // The inline provider's payload survives the round trip into the
    // domain `BoundedText` (the test never reaches the OS because the
    // bounded constructor has room for 256 KiB).
    if let RuleProviderSource::Inline { payload } = &tuning.rule_providers[2].source {
        assert_eq!(payload.as_str(), "10.0.0.0/8");
    } else {
        return Err("third provider must be inline".to_owned());
    }
    if let RuleProviderBehavior::IpCidr = tuning.rule_providers[2].behavior {
    } else {
        return Err("third provider must be ipcidr".to_owned());
    }

    // The four declared rules parse into the domain model in order; the
    // new RULE-SET and GEOSITE matchers reach the runtime rule list
    // alongside the legacy GEOIP and MATCH matchers.
    assert_eq!(tuning.rules.len(), 4);
    assert!(matches!(
        tuning.rules[0].matcher,
        caly_domain::RuleMatch::RuleSet(_)
    ));
    assert!(matches!(
        tuning.rules[1].matcher,
        caly_domain::RuleMatch::Geosite(_)
    ));
    assert!(matches!(
        tuning.rules[2].matcher,
        caly_domain::RuleMatch::Geoip(_)
    ));
    assert!(matches!(
        tuning.rules[3].matcher,
        caly_domain::RuleMatch::Match
    ));
    Ok(())
}

#[test]
fn rule_providers_unknown_reference_fails_validation() -> Result<(), String> {
    let root = temp_root("rule-providers-unknown");
    fs::write(
        root.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\n\
         rules:\n\
         \x20\x20- RULE-SET,not-declared,DIRECT\n\
         \x20\x20- MATCH,PROXY\n",
    )
    .map_err(|error| error.to_string())?;
    let error = resolve_daemon(root.clone()).err();
    fs::remove_dir_all(&root).ok();
    let message = error.ok_or("expected validation failure")?;
    assert!(
        format!("{message:?}").contains("UnknownRuleProvider"),
        "expected UnknownRuleProvider, got {message:?}"
    );
    Ok(())
}
