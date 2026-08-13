//! Fusion tests for `render_compose` (subscription intake × coreconf
//! renderers), relocated from `caly-profile` during the P3a split: pre-split
//! they lived next to the renderers; post-split they must cross intake and
//! rendering, which only this crate may do.

use super::*;

/// Subscription-intake leg reused by the consistency test below.
use caly_subscription::parse_uri_body_to_display_lossy;

#[test]
fn projection_tags_repeated_names_like_the_kernel_renderer() -> Result<(), String> {
    // The list projection and the Mihomo renderer must number repeated
    // names identically, otherwise `delay`/`select` names never match.
    let body = [
        "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQ@203.0.113.1:8388#dup",
        "vless://00000000-0000-0000-0000-000000000001@203.0.113.2:443#dup",
    ]
    .join("\n")
    .into_bytes();
    let id = SubscriptionId::from_bytes([7; 16]);
    let projection =
        parse_uri_body_to_display_lossy(body.clone(), id).map_err(|e| format!("{e:?}"))?;
    let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("{e:?}"))?;
    let projected = projection
        .nodes
        .iter()
        .map(|node| node.name().as_str().to_owned())
        .collect::<Vec<_>>();
    let rendered = set
        .entries()
        .iter()
        .map(|entry| entry.tag.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(projected, rendered, "projection and kernel tags must match");
    Ok(())
}

mod sing_box_fusion {
    //! Former `caly-profile::subscription::sing_box` tests (document header
    //! and whole-document renders through intake).

    use super::*;
    use serde_json::Value;

    fn ss_uri(name: &str) -> String {
        use base64::{Engine as _, engine::general_purpose};
        let credentials = general_purpose::STANDARD.encode("aes-256-gcm:pw");
        format!("ss://{credentials}@example.com:8388#{name}")
    }

    #[test]
    fn document_renders_tun_inbound_and_dns_block() {
        use caly_domain::{TunConfig, TunStack};
        // P3b: tuning hands the bounded DNS settings and typed route rules to
        // the renderer directly (no more pre-rendered JSON fragments).
        let dns = caly_dns::DnsSettingsBuilder::new()
            .enabled(true)
            .push_nameserver("8.8.8.8")
            .unwrap()
            .build()
            .unwrap()
            .unwrap();
        let tun = TunConfig::new(TunStack::Gvisor, true, true, 1400).unwrap();
        let tuning = SingBoxRenderTuning {
            controller: "127.0.0.1:9291".to_owned(),
            secret: "topsecret".to_owned(),
            log_level: "warn".to_owned(),
            mixed_port: 8899,
            allow_lan: false,
            bind_address: "*".to_owned(),
            tun: Some(tun),
            tun_interface: "caly0".to_owned(),
            dns: Some(dns),
            sniff: true,
            sniff_override_destination: true,
            route_rules: vec![caly_coreconf::rules::RouteRule {
                domain: None,
                domain_keyword: None,
                domain_suffix: Some(vec!["example.com".to_owned()]),
                ip_cidr: None,
                outbound: "block".to_owned(),
                rule_set: None,
                ip_is_private: None,
            }],
            rule_sets: Vec::new(),
            route_final: "block".to_owned(),
            block_outbound: true,
        };
        let body = ss_uri("node-a").into_bytes();
        let json =
            uri_body_to_sing_box_json_with(body, SubscriptionId::from_bytes([1; 16]), &tuning)
                .unwrap_or_else(|error| panic!("render failed: {error:?}"));
        let value: Value = serde_json::from_slice(&json).unwrap();
        let inbounds = value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2, "mixed + tun inbounds expected");
        assert_eq!(inbounds[0]["listen_port"], 8899);
        assert_eq!(inbounds[0]["sniff"], true);
        assert_eq!(inbounds[0]["sniff_override_destination"], true);
        assert_eq!(inbounds[1]["type"], "tun");
        assert_eq!(inbounds[1]["interface_name"], "caly0");
        assert_eq!(inbounds[1]["mtu"], 1400);
        assert!(value["dns"]["servers"].is_array());
        assert_eq!(value["route"]["default_domain_resolver"], "nameserver-0");
        assert_eq!(value["experimental"]["clash_api"]["secret"], "topsecret");
        assert_eq!(value["log"]["level"], "warn");
        // Route rules: clash_mode entries first, then config-driven rules.
        let rules = value["route"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[2]["domain_suffix"][0], "example.com");
        assert_eq!(rules[2]["outbound"], "block");
        assert_eq!(value["route"]["final"], "block");
        let outbound_tags: Vec<&str> = value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|outbound| outbound["tag"].as_str())
            .collect();
        assert!(outbound_tags.contains(&"block"));
    }

    #[test]
    fn trojan_and_vmess_render_tls_alter_id_and_transport() -> Result<(), String> {
        let body = "trojan://secret-pass@example.com:443?security=tls&sni=example.com&type=ws&path=%2Fstream#trojan-ws\n\
                    vmess://eyJhZGQiOiJleGFtcGxlLmNvbSIsInBvcnQiOjQ0MywiaWQiOiI2YTg5YTIxNS0yMmJmLTRiZDctOTY0Mi1iOTViMmUwOTU4M2EiLCJhaWQiOjAsIm5ldCI6IndzIiwicGF0aCI6Ii9zIiwiaG9zdCI6ImV4YW1wbGUuY29tIiwidGxzIjoidGxzIiwicHMiOiJ2bWVzLW5vZGUifQ==\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([8; 16]);
        let json = uri_body_to_sing_box_json(body, id).map_err(|e| format!("{e:?}"))?;
        let text = String::from_utf8_lossy(&json);
        assert!(text.contains("\"tls\""), "trojan/vmess TLS must render");
        assert!(text.contains("\"alter_id\""), "vmess alter_id must render");
        assert!(text.contains("\"security\""), "vmess security must render");
        assert!(text.contains("\"transport\""), "ws transport must render");
        Ok(())
    }

    #[test]
    fn hysteria2_and_tuic_render() -> Result<(), String> {
        let body = "hysteria2://secret@example.com:443?security=tls&sni=example.com&obfs=salamander&obfs-password=obfspass&upmbps=50&downmbps=100#hy2\n\
                    tuic://2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365:hello@example.com:443?congestion_control=bbr#tuic\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([9; 16]);
        let json = uri_body_to_sing_box_json(body, id).map_err(|e| format!("{e:?}"))?;
        let text = String::from_utf8_lossy(&json);
        assert!(text.contains("\"type\":\"hysteria2\""));
        assert!(text.contains("\"password\":\"secret\""));
        assert!(text.contains("\"up_mbps\":50"));
        assert!(text.contains("\"obfs\""));
        assert!(text.contains("\"type\":\"tuic\""));
        assert!(text.contains("\"congestion_control\":\"bbr\""));
        Ok(())
    }

    #[test]
    fn emits_global_selector_and_clash_mode_rules() -> Result<(), String> {
        let body = "vless://2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365@example.com:443?security=tls#n1\n\
                    vless://2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365@example.org:443?security=tls#n2\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([10; 16]);
        let json = uri_body_to_sing_box_json(body, id).map_err(|e| format!("{e:?}"))?;
        let text = String::from_utf8_lossy(&json);
        // A GLOBAL selector over all nodes enables the Global routing mode.
        assert!(
            text.contains("\"tag\":\"GLOBAL\""),
            "must emit a GLOBAL selector"
        );
        // The clash_mode rules make rule/global/direct switchable at runtime.
        assert!(text.contains("\"clash_mode\":\"Global\",\"outbound\":\"GLOBAL\""));
        assert!(text.contains("\"clash_mode\":\"Direct\",\"outbound\":\"direct\""));
        Ok(())
    }

    #[test]
    fn outbound_map_indexes_deduped_nodes() -> Result<(), String> {
        let body = "ss://YWVzLTEyOC1nY206cGFzc3dvcmQ=@example.com:8388#first\n\
                    ss://YWVzLTEyOC1nY206cGFzc3dvcmQ=@example.com:8388#duplicate\n\
                    ss://YWVzLTEyOC1nY206cGFzc3dvcmQ=@example.net:443#second\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([9; 16]);
        let (map, _skipped) =
            uri_body_to_sing_box_outbound_map(&body, id).map_err(|e| format!("{e:?}"))?;
        // Two distinct canonical nodes (duplicate lines collapse to one).
        assert_eq!(map.len(), 2, "deduplicated nodes expected");
        for outbound in map.values() {
            let value: serde_json::Value =
                serde_json::from_str(outbound).map_err(|e| e.to_string())?;
            let tag = value["tag"].as_str().ok_or("missing tag")?;
            assert!(tag.starts_with("proxy-"), "tag must be proxy-<id>");
            assert_eq!(value["type"], "shadowsocks");
        }
        Ok(())
    }
}

mod mihomo_fusion {
    //! Former `caly-profile::subscription::mihomo` tests that cross intake
    //! and rendering (pure-renderer tests moved with the renderer to
    //! `caly-coreconf`).

    use super::*;
    use caly_coreconf::mihomo::proxy_sections::render_proxy_sections;

    /// A mixed, minimal, deterministic subscription body.
    fn mini_body() -> Vec<u8> {
        let ss = "ss://Y2hhY2hhMjAtaWV0Zi1wb2x5MTMwNTpzZWNyZXRwYXNz@example.com:8443#ss-node";
        format!(
            "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=tls#vless-node\n\
             trojan://secret-pass@example.com:443?security=tls#trojan-node\n{ss}\n"
        )
        .into_bytes()
    }

    #[test]
    fn clash_yaml_renders_a_proxy_set() -> Result<(), String> {
        let body = br#"proxies:
  - name: "hk-ss"
    type: ss
    server: example.com
    port: 8388
    cipher: aes-128-gcm
    password: secret
  - name: "jp-trojan"
    type: trojan
    server: example.org
    port: 443
    password: pass
"#
        .to_vec();
        let id = SubscriptionId::from_bytes([6; 16]);
        let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(set.len(), 2, "clash yaml must render both proxies");
        let section = render_proxy_sections(&set);
        assert!(section.contains("type: ss"));
        assert!(section.contains("type: trojan"));
        Ok(())
    }

    #[test]
    fn vless_ws_reality_renders_flow_tls_reality_and_transport() -> Result<(), String> {
        let body = "vless://a5ea9247-79f3-4655-aece-3fb51e1e669e@example.com:443?security=reality&flow=xtls-rprx-vision&sni=example.com&pbk=zT7a-PnmIWP4c-G1EDUT3KZ7URi1kc8EppAWPr3h5lk&sid=afeed89ae23b36ed&type=ws&path=%2Fstream&host=example.com#reality-ws\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([7; 16]);
        let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(set.len(), 1);
        let section = render_proxy_sections(&set);
        // XTLS flow, Reality public key, and WS transport must all survive into
        // the rendered config, otherwise the outbound is incomplete.
        assert!(
            section.contains("flow: xtls-rprx-vision"),
            "flow must render"
        );
        assert!(section.contains("tls: true"), "TLS must render");
        assert!(section.contains("reality-opts:"), "reality must render");
        assert!(
            section.contains("public-key:"),
            "reality public key must render"
        );
        assert!(section.contains("network: ws"), "ws transport must render");
        assert!(section.contains("path:"), "ws path must render");
        Ok(())
    }

    #[test]
    fn renders_representable_proxies_and_group() -> Result<(), String> {
        let id = SubscriptionId::from_bytes([3; 16]);
        let set = uri_body_to_mihomo_proxy_set(mini_body(), id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(set.len(), 3, "vless, trojan and ss must all render");
        assert_eq!(set.group(), "AUTO");
        let section = render_proxy_sections(&set);
        assert!(section.contains("type: vless"));
        assert!(section.contains("type: trojan"));
        assert!(section.contains("type: ss"));
        assert!(section.contains("proxy-groups:"));
        assert!(section.contains("rules:"));
        assert!(section.contains("MATCH,AUTO"));
        Ok(())
    }

    #[test]
    fn unsupported_document_is_rejected() {
        let id = SubscriptionId::from_bytes([4; 16]);
        let body = b"this is not a subscription format".to_vec();
        let result = uri_body_to_mihomo_proxy_set(body, id);
        assert!(matches!(result, Err(MihomoProxyError::InvalidFormat)));
    }

    #[test]
    fn http_proxy_uri_now_renders() {
        let id = SubscriptionId::from_bytes([5; 16]);
        // An HTTP proxy URI maps to `Protocol::Http`, which Mihomo can dial.
        let body = b"http://user:pass@example.com:8080#http-node\n".to_vec();
        let result = uri_body_to_mihomo_proxy_set(body, id);
        assert!(result.is_ok(), "http proxy nodes must render: {result:?}");
    }

    #[test]
    fn no_representable_nodes_reports_no_usable() {
        let id = SubscriptionId::from_bytes([6; 16]);
        // A WireGuard URI has no Mihomo outbound representation here.
        let body = b"wireguard://abcdef0123456789abcdef0123456789@example.com:51820?public-key=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=#wg\n".to_vec();
        let result = uri_body_to_mihomo_proxy_set(body, id);
        assert!(matches!(result, Err(MihomoProxyError::NoUsableNodes)));
    }

    #[test]
    fn trojan_and_vmess_render_tls_and_transport() -> Result<(), String> {
        // Trojan over TLS with a ws transport, and a vmess with alterId/cipher/tls.
        let body = "trojan://secret-pass@example.com:443?security=tls&sni=example.com&type=ws&path=%2Fstream#trojan-ws\n\
                    vmess://eyJhZGQiOiJleGFtcGxlLmNvbSIsInBvcnQiOjQ0MywiaWQiOiI2YTg5YTIxNS0yMmJmLTRiZDctOTY0Mi1iOTViMmUwOTU4M2EiLCJhaWQiOjAsIm5ldCI6IndzIiwicGF0aCI6Ii9zIiwiaG9zdCI6ImV4YW1wbGUuY29tIiwidGxzIjoidGxzIiwicHMiOiJ2bWVzLW5vZGUifQ==\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([8; 16]);
        let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(set.len(), 2);
        let section = render_proxy_sections(&set);
        assert!(section.contains("type: trojan"));
        assert!(section.contains("type: vmess"));
        assert!(
            section.contains("tls: true"),
            "trojan/vmess TLS must render"
        );
        assert!(section.contains("network: ws"), "ws transport must render");
        assert!(section.contains("alterId: 0"), "vmess alterId must render");
        assert!(section.contains("cipher: auto"), "vmess cipher must render");
        Ok(())
    }

    #[test]
    fn hysteria2_and_tuic_render() -> Result<(), String> {
        let body = "hysteria2://secret@example.com:443?security=tls&sni=example.com&obfs=salamander&obfs-password=obfspass&upmbps=50&downmbps=100#hy2\n\
                    tuic://2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365:hello@example.com:443?congestion_control=bbr#tuic\n"
            .as_bytes()
            .to_vec();
        let id = SubscriptionId::from_bytes([9; 16]);
        let set = uri_body_to_mihomo_proxy_set(body, id).map_err(|e| format!("{e:?}"))?;
        assert_eq!(set.len(), 2, "hysteria2 and tuic must render");
        let section = render_proxy_sections(&set);
        assert!(section.contains("type: hysteria2"));
        // Passwords render through yaml_quote like every other credential.
        assert!(section.contains("password: \"secret\""));
        assert!(section.contains("obfs-password: \"obfspass\""));
        assert!(section.contains("obfs: salamander"));
        assert!(section.contains("type: tuic"));
        assert!(section.contains("password: \"hello\""));
        assert!(section.contains("congestion-controller: bbr"));
        Ok(())
    }
}

/// 2026-08-12: http/socks5 nodes render into the sing-box map (they
/// are first-class sing-box outbounds) instead of being skipped.
#[test]
fn http_and_socks5_nodes_render_in_sing_box_map() -> Result<(), String> {
    let id = SubscriptionId::from_bytes([7; 16]);
    let body = b"http://user:pass@a.example.com:8080#HTTP - a\nsocks5://u:p@b.example.com:1080#SOCKS - b\n"
            .to_vec();
    let (map, skipped) =
        uri_body_to_sing_box_outbound_map(&body, id).map_err(|e| format!("{e:?}"))?;
    assert_eq!(map.len(), 2, "both proxy nodes must render");
    assert!(skipped.is_empty(), "no skips expected: {skipped:?}");
    let json: Vec<String> = map.values().cloned().collect();
    let json = json.join(",");
    assert!(json.contains("\"type\":\"http\""), "{json}");
    assert!(json.contains("\"type\":\"socks\""), "{json}");
    Ok(())
}

/// 2026-08-12: a shadowsocks node with a v2ray-plugin parameter is
/// labelled `shadowsocks+plugin` in the skip list — the warning must
/// say which variant lost, not a bare protocol name.
#[test]
fn ss_plugin_nodes_are_skipped_with_variant_label() -> Result<(), String> {
    let id = SubscriptionId::from_bytes([8; 16]);
    let body =
        b"ss://bm9uZTpwYXNz@1.2.3.4:443?plugin=v2ray-plugin%3Bmode%3Dwebsocket#ss-plug\n".to_vec();
    let (map, skipped) =
        uri_body_to_sing_box_outbound_map(&body, id).map_err(|e| format!("{e:?}"))?;
    assert!(map.is_empty(), "plugin ss must not render: {map:?}");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].protocol, "shadowsocks+plugin");
    Ok(())
}

/// 2026-08-12: a legacy-CFB ss node is skipped with the exact cipher
/// in the label (`shadowsocks+aes-128-cfb`) — Mihomo dials CFB,
/// sing-box does not, so the diagnostic must name the cipher, not
/// the generic protocol.
#[test]
fn cfb_ss_nodes_are_skipped_with_cipher_label() -> Result<(), String> {
    let id = SubscriptionId::from_bytes([10; 16]);
    // aes-128-cfb: YWVzLTEyOC1jZmI6cGFzcw== = "aes-128-cfb:pass"
    let body = b"ss://YWVzLTEyOC1jZmI6cGFzcw@1.2.3.4:443#cfb-node\n".to_vec();
    let (map, skipped) =
        uri_body_to_sing_box_outbound_map(&body, id).map_err(|e| format!("{e:?}"))?;
    assert!(map.is_empty(), "cfb ss must not render");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].protocol, "shadowsocks+aes-128-cfb");
    Ok(())
}
