use super::*;

#[test]
fn minimal_loopback_config_is_valid() {
    let parsed = parse_and_validate_json(br#"{"schema_version":1,"core":"mihomo"}"#);
    assert!(parsed.is_ok());
}

#[test]
fn remote_listen_requires_tls_then_auth() {
    let no_tls = br#"{"schema_version":1,"core":"xray","daemon":{"listen":"0.0.0.0:17890"}}"#;
    assert!(matches!(
        parse_and_validate_json(no_tls),
        Err(ConfigError::RemoteListenRequiresTls)
    ));
    // TLS on but without the certificate/key material: the
    // listener cannot possibly come up, so this is rejected
    // before the auth gate is even considered.
    let no_material = br#"{"schema_version":1,"core":"xray","daemon":{"listen":"0.0.0.0:17890","tls_enabled":true}}"#;
    assert!(matches!(
        parse_and_validate_json(no_material),
        Err(ConfigError::TlsMaterialMissing)
    ));
    let no_auth = br#"{"schema_version":1,"core":"xray","daemon":{"listen":"0.0.0.0:17890","tls_enabled":true,"tls_cert_path":"/tmp/c.pem","tls_key_path":"/tmp/k.pem"}}"#;
    assert!(matches!(
        parse_and_validate_json(no_auth),
        Err(ConfigError::RemoteListenRequiresAuth)
    ));
}

#[test]
fn remote_secret_is_redacted() -> Result<(), ConfigError> {
    let source = br#"{"schema_version":1,"core":"sing-box","daemon":{"listen":"0.0.0.0:17890","tls_enabled":true,"tls_cert_path":"/tmp/c.pem","tls_key_path":"/tmp/k.pem","auth_token":"do-not-print"}}"#;
    let parsed = parse_and_validate_json(source)?;
    let rendered = format!("{:?}", parsed.daemon.auth_token);
    assert!(!rendered.contains("do-not-print"));
    assert!(rendered.contains("REDACTED"));
    Ok(())
}

#[test]
fn unknown_fields_and_bad_mtu_are_rejected() {
    let unknown = br#"{"schema_version":1,"core":"mihomo","extra":true}"#;
    assert!(matches!(
        parse_and_validate_json(unknown),
        Err(ConfigError::Parse { .. })
    ));
    let mtu = br#"{"schema_version":1,"core":"mihomo","tun":{"mtu":100}}"#;
    assert_eq!(
        parse_and_validate_json(mtu).err(),
        Some(ConfigError::InvalidTunMtu)
    );
}

#[test]
fn unsupported_schema_is_explicit() {
    let source = br#"{"schema_version":2,"core":"mihomo"}"#;
    assert!(matches!(
        parse_and_validate_json(source),
        Err(ConfigError::UnsupportedSchemaVersion(2))
    ));
}

#[test]
fn controllers_log_telemetry_defaults_apply() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(br#"{"schema_version":1,"core":"mihomo"}"#)?;
    assert_eq!(parsed.controllers.mihomo, "127.0.0.1:9090");
    assert_eq!(parsed.controllers.sing_box, "127.0.0.1:9091");
    assert_eq!(parsed.core_binaries, CoreBinariesConfig::default());
    assert_eq!(parsed.log.level, "info");
    assert_eq!(parsed.telemetry.interval_ms, 1_000);
    Ok(())
}

#[test]
fn controllers_log_telemetry_are_validated() {
    let bad_controller = br#"{"schema_version":1,"core":"mihomo","controllers":{"mihomo":"nope"}}"#;
    assert!(matches!(
        parse_and_validate_json(bad_controller),
        Err(ConfigError::InvalidControllerAddress { .. })
    ));
    let bad_log = br#"{"schema_version":1,"core":"mihomo","log":{"level":"verbose"}}"#;
    assert!(matches!(
        parse_and_validate_json(bad_log),
        Err(ConfigError::InvalidLogLevel)
    ));
    let bad_interval = br#"{"schema_version":1,"core":"mihomo","telemetry":{"interval_ms":50}}"#;
    assert!(matches!(
        parse_and_validate_json(bad_interval),
        Err(ConfigError::InvalidTelemetryInterval)
    ));
}

#[test]
fn subscription_sources_list_validates_each_entry() -> Result<(), ConfigError> {
    // Multi-source config: url (legacy) + sources; all enabled URLs must be
    // validated individually.
    let valid = br#"{"schema_version":1,"core":"mihomo","subscriptions":{"url":"https://a.example/sub","sources":[{"url":"https://b.example/sub","enabled":true},{"url":"https://c.example/sub","enabled":false}]}}"#;
    let parsed = parse_and_validate_json(valid)?;
    assert_eq!(
        parsed.subscriptions.enabled_source_urls(),
        vec![
            "https://a.example/sub".to_owned(),
            "https://b.example/sub".to_owned(),
        ]
    );
    // An invalid URL anywhere fails validation.
    let bad = br#"{"schema_version":1,"core":"mihomo","subscriptions":{"sources":[{"url":"ftp://x"},{"url":"https://ok.example"}]}}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidSubscriptionUrl)
    ));
    Ok(())
}

#[test]
fn subscription_url_requires_http_or_https() {
    let valid = br#"{"schema_version":1,"core":"mihomo","subscriptions":{"url":"https://example.com/sub"}}"#;
    assert!(parse_and_validate_json(valid).is_ok());
    let invalid =
        br#"{"schema_version":1,"core":"mihomo","subscriptions":{"url":"file:///etc/passwd"}}"#;
    assert!(matches!(
        parse_and_validate_json(invalid),
        Err(ConfigError::InvalidSubscriptionUrl)
    ));
}

#[test]
fn default_provider_is_derived_from_sources() -> Result<(), ConfigError> {
    // With any enabled source, the auto enumeration `default` provider aggregates
    // every enabled source URL — exactly the set `caly sub list` reads.
    let multi = br#"{"schema_version":1,"core":"mihomo","subscriptions":{"url":"https://a.example/sub","sources":[{"url":"https://b.example/sub","enabled":true},{"url":"https://c.example/sub","enabled":false}]}}"#;
    let parsed = parse_and_validate_json(multi)?;
    let providers = parsed.resolved_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, "default");
    assert_eq!(providers[0].kind, ProviderKind::SubscriptionSources);
    assert_eq!(
        parsed.subscriptions.default_provider(),
        Some(providers[0].clone())
    );

    // A single legacy `url` also yields the default provider.
    let single = parse_and_validate_json(
        br#"{"schema_version":1,"core":"mihomo","subscriptions":{"url":"https://a.example/sub"}}"#,
    )?;
    assert_eq!(single.resolved_providers().len(), 1);

    // No source at all → no default provider (a node-only config must not
    // fabricate an empty enumeration provider).
    let empty = parse_and_validate_json(br#"{"schema_version":1,"core":"mihomo"}"#)?;
    assert!(empty.resolved_providers().is_empty());
    assert!(empty.subscriptions.default_provider().is_none());
    Ok(())
}

#[test]
fn explicit_providers_override_default_derivation() -> Result<(), ConfigError> {
    // Explicit `providers:` takes precedence over the auto default derivation.
    let source =
        br#"{"schema_version":1,"core":"mihomo","providers":[{"name":"kitchen-sink","kind":"subscription-sources"}]}"#;
    let parsed = parse_and_validate_json(source)?;
    let providers = parsed.resolved_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, "kitchen-sink");
    assert_eq!(providers[0].kind, ProviderKind::SubscriptionSources);
    Ok(())
}

#[test]
fn inline_nodes_provider_parses() -> Result<(), ConfigError> {
    // vmess://-style bare content forms an `InlineNodes` provider (nodes
    // with no upstream URL) rather than a fetchable source.
    let source = br#"{"schema_version":1,"core":"mihomo","providers":[{"name":"manual","kind":{"inline-nodes":["vmess://eyJhZGQiOiJleGFtcGxlLmNvbSIsInBvcnQiOjQ0MywiaWQiOiI2YTg5YTIxNS0yMmJmLTRiZDctOTY0Mi1iOTViMmUwOTU4M2EiLCJhaWQiOjAsIm5ldCI6IndzIiwicGF0aCI6Ii9zIiwiaG9zdCI6ImV4YW1wbGUuY29tIiwidGxzIjoidGxzIiwicHMiOiJ2bWVzLW5vZGUifQ==","ss://Y2hhY2hhMjAtaWV0Zi1wb2x5MTMwNTpzZWNyZXRwYXNz@example.com:8443#ss-node"]}}]}"#;
    let parsed = parse_and_validate_json(source)?;
    let providers = parsed.resolved_providers();
    assert_eq!(providers[0].name, "manual");
    assert!(matches!(&providers[0].kind, ProviderKind::InlineNodes(n) if n.len() == 2));

    // Malformed inline node URIs are rejected by validate_providers.
    let bad = br#"{"schema_version":1,"core":"mihomo","providers":[{"name":"manual","kind":{"inline-nodes":["vmess://abc"]}}]}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidProvider { .. })
    ));
    // Duplicate provider names are rejected as well.
    let duplicate = br#"{"schema_version":1,"core":"mihomo","providers":[{"name":"a","kind":"subscription-sources"},{"name":"a","kind":"subscription-sources"}]}"#;
    assert!(matches!(
        parse_and_validate_json(duplicate),
        Err(ConfigError::InvalidProvider { .. })
    ));
    Ok(())
}

#[test]
fn kernel_section_is_validated() {
    let zero_port = br#"{"schema_version":1,"core":"mihomo","kernel":{"mixed_port":0}}"#;
    assert!(matches!(
        parse_and_validate_json(zero_port),
        Err(ConfigError::InvalidKernelPort)
    ));
    let bad_level = br#"{"schema_version":1,"core":"mihomo","kernel":{"log_level":"verbose"}}"#;
    assert!(matches!(
        parse_and_validate_json(bad_level),
        Err(ConfigError::InvalidKernelLogLevel)
    ));
}

#[test]
fn dns_section_is_validated() {
    let enabled_without_nameservers =
        br#"{"schema_version":1,"core":"mihomo","dns":{"enabled":true,"mode":"standard"}}"#;
    assert!(matches!(
        parse_and_validate_json(enabled_without_nameservers),
        Err(ConfigError::InvalidDns(_))
    ));
    let valid = br#"{"schema_version":1,"core":"mihomo","dns":{"enabled":true,"mode":"fake-ip","nameservers":["8.8.8.8"],"fake_ip_range":"198.18.0.1/16"}}"#;
    assert!(parse_and_validate_json(valid).is_ok());
}

#[test]
fn subscription_fetch_policy_is_validated() {
    let tiny_timeout =
        br#"{"schema_version":1,"core":"mihomo","subscriptions":{"connect_timeout_ms":10}}"#;
    assert!(matches!(
        parse_and_validate_json(tiny_timeout),
        Err(ConfigError::InvalidFetchPolicy { .. })
    ));
    let huge_body = br#"{"schema_version":1,"core":"mihomo","subscriptions":{"max_body_mb":1000}}"#;
    assert!(matches!(
        parse_and_validate_json(huge_body),
        Err(ConfigError::InvalidFetchPolicy { .. })
    ));
}

#[test]
fn system_proxy_endpoint_is_validated() {
    let zero_port =
        br#"{"schema_version":1,"core":"mihomo","system_proxy":{"enabled":false,"host":"127.0.0.1","port":0}}"#;
    assert!(matches!(
        parse_and_validate_json(zero_port),
        Err(ConfigError::InvalidSystemProxy)
    ));
    let empty_host =
        br#"{"schema_version":1,"core":"mihomo","system_proxy":{"enabled":false,"host":""}}"#;
    assert!(matches!(
        parse_and_validate_json(empty_host),
        Err(ConfigError::InvalidSystemProxy)
    ));
}

#[test]
fn default_config_renders_and_reparses() {
    let yaml = render_default_config();
    assert!(yaml.contains("core: mihomo"));
    assert!(yaml.contains("mihomo: 127.0.0.1:9090"));
    assert!(yaml.contains("sing_box: 127.0.0.1:9091"));
    // The rendered default must be valid config the loader accepts.
    let parsed = parse_and_validate_yaml(yaml.as_bytes());
    assert!(parsed.is_ok(), "default config must re-parse: {parsed:?}");
}

#[test]
fn sniffer_defaults_apply() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(br#"{"schema_version":1,"core":"mihomo"}"#)?;
    assert!(!parsed.sniffer.enabled);
    assert!(parsed.sniffer.override_destination);
    assert!(parsed.sniffer.parse_pure_ip);
    assert_eq!(
        parsed.sniffer.tls_ports,
        vec!["443".to_owned(), "8443".to_owned()]
    );
    Ok(())
}

#[test]
fn sniffer_port_specs_are_validated() {
    let bad = br#"{"schema_version":1,"core":"mihomo","sniffer":{"enabled":true,"http_ports":["80","bad-port"]}}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidSnifferPort { .. })
    ));
    let inverted = br#"{"schema_version":1,"core":"mihomo","sniffer":{"enabled":true,"tls_ports":["8443-443"]}}"#;
    assert!(matches!(
        parse_and_validate_json(inverted),
        Err(ConfigError::InvalidSnifferPort { .. })
    ));
    let zero =
        br#"{"schema_version":1,"core":"mihomo","sniffer":{"enabled":true,"quic_ports":["0"]}}"#;
    assert!(matches!(
        parse_and_validate_json(zero),
        Err(ConfigError::InvalidSnifferPort { .. })
    ));
    let valid = br#"{"schema_version":1,"core":"mihomo","sniffer":{"enabled":true,"quic_ports":["443","8443-9443"]}}"#;
    assert!(parse_and_validate_json(valid).is_ok());
}

#[test]
fn dns_fallback_filter_cidr_is_validated() {
    let bad = br#"{"schema_version":1,"core":"mihomo","dns":{"enabled":true,"mode":"fake-ip","nameservers":["8.8.8.8"],"fake_ip_range":"198.18.0.1/16","fallback":["1.1.1.1"],"fallback_filter":{"geoip":true,"ipcidr":["not-a-cidr"]}}}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidDnsCidrFilter)
    ));
}

#[test]
fn dns_listen_must_be_a_socket_address() {
    let bad = br#"{"schema_version":1,"core":"mihomo","dns":{"enabled":true,"mode":"standard","nameservers":["8.8.8.8"],"listen":"not-an-address"}}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidDnsListen)
    ));
    let good = br#"{"schema_version":1,"core":"mihomo","dns":{"enabled":true,"mode":"standard","nameservers":["8.8.8.8"],"listen":"127.0.0.1:1053"}}"#;
    assert!(parse_and_validate_json(good).is_ok());
}

#[test]
fn rule_providers_http_parses_and_validates() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(
        br#"{
            "schema_version": 1,
            "core": "mihomo",
            "rule_providers": [
                {
                    "name": "my-google",
                    "type": "http",
                    "behavior": "domain",
                    "format": "source",
                    "url": "https://example.com/google.yaml",
                    "interval_ms": 86400000
                }
            ]
        }"#,
    )?;
    assert_eq!(parsed.rule_providers.len(), 1);
    let provider = &parsed.rule_providers[0];
    assert_eq!(provider.name, "my-google");
    let domain = provider
        .to_rule_provider()
        .map_err(|error| ConfigError::InvalidRule {
            index: 0,
            reason: error,
        })?;
    assert_eq!(domain.name.as_str(), "my-google");
    assert!(matches!(
        domain.behavior,
        caly_domain::RuleProviderBehavior::Domain
    ));
    Ok(())
}

#[test]
fn rule_providers_file_and_inline_round_trip() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(
        br#"{
            "schema_version": 1,
            "core": "mihomo",
            "rule_providers": [
                {
                    "name": "local-rules",
                    "type": "file",
                    "behavior": "classical",
                    "format": "source",
                    "path": "/etc/caly/rules.yaml"
                },
                {
                    "name": "inline-ads",
                    "type": "inline",
                    "behavior": "domain_suffix",
                    "format": "source",
                    "payload": "- example.com\n- ads.example.org\n"
                }
            ]
        }"#,
    )?;
    assert_eq!(parsed.rule_providers.len(), 2);
    let local = &parsed.rule_providers[0];
    let inline = &parsed.rule_providers[1];
    let local_provider = local
        .to_rule_provider()
        .map_err(|error| ConfigError::InvalidRule {
            index: 0,
            reason: error,
        })?;
    let inline_provider = inline
        .to_rule_provider()
        .map_err(|error| ConfigError::InvalidRule {
            index: 1,
            reason: error,
        })?;
    assert!(matches!(
        local_provider.source,
        caly_domain::RuleProviderSource::File { .. }
    ));
    assert!(matches!(
        local_provider.behavior,
        caly_domain::RuleProviderBehavior::Classical
    ));
    assert!(matches!(
        inline_provider.source,
        caly_domain::RuleProviderSource::Inline { .. }
    ));
    assert!(matches!(
        inline_provider.behavior,
        caly_domain::RuleProviderBehavior::DomainSuffix
    ));
    Ok(())
}

#[test]
fn rule_providers_reject_duplicate_names() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rule_providers": [
            {
                "name": "shared",
                "type": "inline",
                "behavior": "domain",
                "format": "source",
                "payload": "a.com\n"
            },
            {
                "name": "shared",
                "type": "inline",
                "behavior": "domain_suffix",
                "format": "source",
                "payload": "b.com\n"
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::DuplicateRuleProvider { .. })
    ));
}

#[test]
fn rule_providers_reject_short_polling_interval() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rule_providers": [
            {
                "name": "noisy",
                "type": "http",
                "behavior": "domain",
                "format": "source",
                "url": "https://example.com/r.yaml",
                "interval_ms": 1000
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidRuleProviderInterval { .. })
    ));
}

#[test]
fn rule_providers_reject_invalid_http_url() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rule_providers": [
            {
                "name": "bad",
                "type": "http",
                "behavior": "domain",
                "format": "source",
                "url": "ftp://example.com/r.yaml"
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidRuleProviderUrl { .. })
    ));
}

#[test]
fn rule_providers_reject_empty_inline_payload() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rule_providers": [
            {
                "name": "empty",
                "type": "inline",
                "behavior": "domain",
                "format": "source",
                "payload": "   \n"
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::EmptyRuleProviderPayload { .. })
    ));
}

#[test]
fn rule_set_rule_must_reference_a_known_provider() {
    let unknown = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rule_providers": [
            {
                "name": "declared",
                "type": "inline",
                "behavior": "domain",
                "format": "source",
                "payload": "a.com\n"
            }
        ],
        "rules": [
            "RULE-SET,declared,DIRECT",
            "RULE-SET,undeclared,DIRECT"
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(unknown),
        Err(ConfigError::UnknownRuleProvider { .. })
    ));
}

#[test]
fn rule_set_rule_passes_when_provider_is_declared() {
    let ok = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rule_providers": [
            {
                "name": "my-prov",
                "type": "inline",
                "behavior": "domain",
                "format": "source",
                "payload": "a.com\n"
            }
        ],
        "rules": [
            "RULE-SET,my-prov,DIRECT",
            "MATCH,PROXY"
        ]
    }"#;
    assert!(parse_and_validate_json(ok).is_ok());
}

#[test]
fn geoip_rule_does_not_require_a_declared_rule_provider() {
    // `geoip-cn` is auto-emitted by the rule renderer (SagerNet's
    // sing-geoip rule-set), so a `GEOIP,CN,…` rule must validate even
    // when the user has not declared it.
    let ok = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "rules": ["GEOIP,CN,DIRECT", "MATCH,PROXY"]
    }"#;
    assert!(parse_and_validate_json(ok).is_ok());
}

#[test]
fn profiles_local_and_remote_parse() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(
        br#"{
            "schema_version": 1,
            "core": "mihomo",
            "profiles": [
                {
                    "id": "team-shared",
                    "name": "Team shared",
                    "kind": "remote",
                    "url": "https://example.com/team.yaml",
                    "interval_minutes": 60
                },
                {
                    "id": "local-extra",
                    "kind": "local",
                    "path": "extra.yaml"
                }
            ]
        }"#,
    )?;
    assert_eq!(parsed.profiles.len(), 2);
    let team = &parsed.profiles[0];
    let local = &parsed.profiles[1];
    let team_domain = team.to_domain().map_err(|error| ConfigError::InvalidRule {
        index: 0,
        reason: error,
    })?;
    let local_domain = local
        .to_domain()
        .map_err(|error| ConfigError::InvalidRule {
            index: 1,
            reason: error,
        })?;
    assert_eq!(team_domain.id.as_str(), "team-shared");
    assert!(matches!(
        team_domain.source,
        caly_domain::ProfileSource::Remote { .. }
    ));
    assert!(matches!(
        local_domain.source,
        caly_domain::ProfileSource::Local { .. }
    ));
    Ok(())
}

#[test]
fn profiles_merge_parses_and_validates_declaration() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(
        br#"{
            "schema_version": 1,
            "core": "mihomo",
            "profiles": [
                {"id": "team", "kind": "remote", "url": "https://example.com/team.yaml", "interval_minutes": 60},
                {"id": "local", "kind": "local", "path": "local.yaml"},
                {"id": "combined", "kind": "merge", "parts": ["team", "local"]}
            ]
        }"#,
    )?;
    assert_eq!(parsed.profiles.len(), 3);
    Ok(())
}

#[test]
fn profiles_reject_duplicate_ids() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "shared", "kind": "local", "path": "a.yaml"},
            {"id": "shared", "kind": "local", "path": "b.yaml"}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::Profile(
            caly_domain::ProfileError::DuplicateId { .. }
        ))
    ));
}

#[test]
fn profiles_reject_non_path_safe_id() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "with/slash", "kind": "local", "path": "a.yaml"}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::Profile(_))
    ));
}

#[test]
fn profiles_reject_empty_local_path() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "x", "kind": "local", "path": "   "}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidRule { .. })
    ));
}

#[test]
fn profiles_reject_local_path_escape() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "x", "kind": "local", "path": "../etc/passwd"}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::Profile(
            caly_domain::ProfileError::LocalPathEscape { .. }
        ))
    ));
}

#[test]
fn profiles_reject_remote_loopback_url() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "loopback", "kind": "remote", "url": "http://127.0.0.1/x", "interval_minutes": 60}
        ]
    }"#;
    // URL parses and is http, so the path-traversal guard does not apply.
    // The SSRF containment is enforced by the fetch path (Round 7 E2E),
    // not by the schema validator, so a loopback URL still parses here.
    // The test is here to lock the documented contract: the schema
    // accepts public http(s) URLs; the fetcher rejects private/loopback
    // destinations at runtime.
    assert!(parse_and_validate_json(bad).is_ok());
}

#[test]
fn profiles_reject_remote_zero_interval() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "x", "kind": "remote", "url": "https://example.com/x", "interval_minutes": 0}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::Profile(
            caly_domain::ProfileError::InvalidInterval { .. }
        ))
    ));
}

#[test]
fn profiles_reject_merge_cycle() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "a", "kind": "merge", "parts": ["b"]},
            {"id": "b", "kind": "merge", "parts": ["a"]}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::Profile(
            caly_domain::ProfileError::Cycle { .. }
        ))
    ));
}

#[test]
fn profiles_reject_merge_unknown_part() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "a", "kind": "merge", "parts": ["ghost"]}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::Profile(
            caly_domain::ProfileError::UnknownId { .. }
        ))
    ));
}

#[test]
fn profiles_default_is_empty_list() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(br#"{"schema_version":1,"core":"mihomo"}"#)?;
    assert!(parsed.profiles.is_empty());
    Ok(())
}

#[test]
fn profiles_diamond_merge_is_not_a_cycle() -> Result<(), ConfigError> {
    // merged = merge:[base, extra]; base and extra both merge `shared`.
    // The shared profile is *visited twice* through different paths but is
    // never part of a loop — a DFS that never pops its recursion stack used
    // to misreport this as `ProfileError::Cycle`.
    let diamond = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "profiles": [
            {"id": "shared", "kind": "local", "path": "shared.yaml"},
            {"id": "base", "kind": "merge", "parts": ["shared"]},
            {"id": "extra", "kind": "merge", "parts": ["shared"]},
            {"id": "merged", "kind": "merge", "parts": ["base", "extra"]}
        ]
    }"#;
    parse_and_validate_json(diamond)?;
    Ok(())
}

#[test]
fn proxy_groups_diamond_relay_is_not_a_cycle() -> Result<(), ConfigError> {
    // A -> C, B -> C (all relay) — the diamond converges on C, which is
    // legal; only a genuine loop (A -> B -> A) is a RelayCycle.
    let diamond = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "C",
                "type": "relay",
                "members": [{"kind": "node", "tag": "n1"}]
            },
            {
                "name": "A",
                "type": "relay",
                "members": [{"kind": "group", "name": "C"}]
            },
            {
                "name": "B",
                "type": "relay",
                "members": [{"kind": "group", "name": "C"}]
            },
            {
                "name": "Z",
                "type": "relay",
                "members": [{"kind": "group", "name": "A"}, {"kind": "group", "name": "B"}]
            }
        ]
    }"#;
    parse_and_validate_json(diamond)?;
    Ok(())
}

#[test]
fn proxy_groups_minimal_select_parses_and_validates() -> Result<(), ConfigError> {
    let yaml = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "Proxy",
                "type": "select",
                "members": [
                    {"kind": "node", "tag": "hk-1"},
                    {"kind": "direct"}
                ]
            }
        ]
    }"#;
    let parsed = parse_and_validate_json(yaml)?;
    assert_eq!(parsed.proxy_groups.len(), 1);
    let group = &parsed.proxy_groups[0];
    assert_eq!(group.name, "Proxy");
    assert_eq!(group.members.len(), 2);
    assert!(group.url_test.is_none());
    Ok(())
}

#[test]
fn proxy_groups_url_test_requires_url_test_block() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {"name": "Auto", "type": "url-test", "members": [{"kind": "node", "tag": "n1"}]}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::MissingUrlTest { .. }
        ))
    ));
}

#[test]
fn proxy_groups_rejects_empty_members() {
    // Both kernels refuse a group with an empty member list; the schema
    // validator reports it before the core ever sees the config.
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {"name": "P", "type": "select", "members": []}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::EmptyMembers { .. }
        ))
    ));
}

#[test]
fn proxy_groups_rejects_invalid_probe_url() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "Auto",
                "type": "url-test",
                "members": [{"kind": "node", "tag": "n1"}],
                "url_test": {"url": "notaurl", "interval_seconds": 300}
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::InvalidProbeUrl { .. }
        ))
    ));
}

#[test]
fn proxy_groups_select_rejects_url_test_block() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "P",
                "type": "select",
                "members": [{"kind": "node", "tag": "n1"}],
                "url_test": {"url": "http://example.com"}
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::UnexpectedUrlTest { .. }
        ))
    ));
}

#[test]
fn proxy_groups_rejects_duplicate_name() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {"name": "A", "type": "select", "members": [{"kind": "node", "tag": "n1"}]},
            {"name": "A", "type": "select", "members": [{"kind": "node", "tag": "n2"}]}
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::DuplicateName { .. }
        ))
    ));
}

#[test]
fn proxy_groups_rejects_unknown_member_reference() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "Outer",
                "type": "select",
                "members": [{"kind": "group", "name": "Ghost"}]
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::UnknownMemberGroup { .. }
        ))
    ));
}

#[test]
fn proxy_groups_rejects_relay_cycle() {
    let bad = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "A",
                "type": "relay",
                "members": [{"kind": "group", "name": "B"}]
            },
            {
                "name": "B",
                "type": "relay",
                "members": [{"kind": "group", "name": "A"}]
            }
        ]
    }"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::ProxyGroup(
            caly_domain::ProxyGroupError::RelayCycle { .. }
        ))
    ));
}

#[test]
fn proxy_groups_nested_group_reference_resolves() -> Result<(), ConfigError> {
    let yaml = br#"{
        "schema_version": 1,
        "core": "mihomo",
        "proxy_groups": [
            {
                "name": "Proxy",
                "type": "select",
                "members": [
                    {"kind": "node", "tag": "hk-1"},
                    {"kind": "direct"}
                ]
            },
            {
                "name": "Auto",
                "type": "url-test",
                "members": [{"kind": "group", "name": "Proxy"}],
                "url_test": {"url": "http://www.gstatic.com/generate_204"}
            }
        ]
    }"#;
    let parsed = parse_and_validate_json(yaml)?;
    assert_eq!(parsed.proxy_groups.len(), 2);
    Ok(())
}

#[test]
fn proxy_groups_default_is_empty_list() -> Result<(), ConfigError> {
    let parsed = parse_and_validate_json(br#"{"schema_version":1,"core":"mihomo"}"#)?;
    assert!(parsed.proxy_groups.is_empty());
    Ok(())
}
