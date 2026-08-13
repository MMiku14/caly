//! E2E: render the real subscription fixture to a sing-box config and validate
//! it with the real sing-box binary. This exercises the strict sing-box
//! outbound renderer (`node_to_json`) against the mixed protocols found in a
//! real subscription (vless/vmess/trojan/ss), which the minimal-subscription
//! test does not cover.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // #53: integration-test helpers use unwrap/expect freely
use std::path::PathBuf;
use std::process::Command;

use caly_backends::subscription::render_compose::uri_body_to_sing_box_json;
use caly_domain::SubscriptionId;
use caly_subscription::decode_document;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn singbox_binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_SINGBOX_BIN") {
        PathBuf::from(value)
    } else {
        manifest().join("../../vendor/bin/sing-box")
    }
}

#[test]
fn real_fixture_renders_a_sing_box_config_the_binary_accepts()
-> Result<(), Box<dyn std::error::Error>> {
    let body = std::fs::read(manifest().join("../../fixtures/subscription-20260803.txt"))
        .map_err(|_| "fixture not found")?;
    let document = decode_document(body.clone()).map_err(|e| format!("decode: {e:?}"))?;
    if !matches!(
        document,
        caly_subscription::SubscriptionDocument::UriLines { .. }
    ) {
        return Err("fixture did not decode as URI lines".into());
    }
    let id = SubscriptionId::from_bytes([11; 16]);
    let json = uri_body_to_sing_box_json(body, id)
        .map_err(|e| format!("sing-box render failed: {e:?}"))?;
    let text = String::from_utf8_lossy(&json);
    assert!(text.contains("\"PROXY\""), "must contain PROXY selector");

    let binary = singbox_binary();
    if !binary.is_file() {
        eprintln!("skipping: no sing-box binary at {}", binary.display());
        return Ok(());
    }
    let dir = caly_platform::paths::test_helpers::unique_path_under("caly-fix-sb", "fixture");
    std::fs::create_dir_all(&dir)?;
    let config = dir.join("config.json");
    std::fs::write(&config, &json)?;
    let check = Command::new(&binary)
        .args(["check", "-c"])
        .arg(&config)
        .output()?;
    assert!(
        check.status.success(),
        "sing-box check rejected the fixture-rendered config:\n{}\n---config---\n{}",
        String::from_utf8_lossy(&check.stderr),
        text
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn config_rules_render_into_route_rules_accepted_by_real_binary()
-> Result<(), Box<dyn std::error::Error>> {
    use caly_backends::subscription::render_compose::uri_body_to_sing_box_json_with;
    use caly_coreconf::rules::render_sing_box_rules;
    use caly_coreconf::sing_box::SingBoxRenderTuning;
    use caly_domain::RoutingRule;

    let body = std::fs::read(manifest().join("../../fixtures/subscription-20260803.txt"))
        .map_err(|_| "fixture not found")?;
    let rules: Vec<RoutingRule> = [
        "DOMAIN-SUFFIX,google.com,PROXY",
        "DOMAIN-KEYWORD,ad,REJECT",
        "IP-CIDR,192.168.0.0/16,DIRECT",
        "GEOIP,private,DIRECT",
        "MATCH,PROXY",
    ]
    .iter()
    .map(|line| RoutingRule::from_clash_line(line).unwrap())
    .collect();
    let rendered = render_sing_box_rules(&rules, &[], true, &std::collections::BTreeSet::new());
    assert_eq!(rendered.skipped, 0, "every rule must be representable");
    assert!(
        serde_json::to_string(&rendered.rules)
            .unwrap_or_default()
            .contains("ip_is_private"),
        "GEOIP,private renders natively"
    );
    assert!(rendered.block_outbound, "REJECT needs the block outbound");

    let mut tuning = SingBoxRenderTuning::standard();
    tuning.route_rules = rendered.rules;
    tuning.rule_sets = rendered.rule_sets;
    tuning.route_final = rendered.final_outbound;
    tuning.block_outbound = rendered.block_outbound;
    let json = uri_body_to_sing_box_json_with(body, SubscriptionId::from_bytes([12; 16]), &tuning)
        .map_err(|e| format!("sing-box render failed: {e:?}"))?;
    let text = String::from_utf8_lossy(&json);
    assert!(text.contains("\"domain_suffix\":[\"google.com\"]"));
    // GEOIP,private renders natively (the plain `geoip` key was removed in
    // sing-box 1.12 and the SagerNet .srs source is decommissioned).
    assert!(text.contains("\"ip_is_private\":true"), "text: {text}");
    assert!(
        !text.contains("sing-geoip/rule-set"),
        "no dead SagerNet source: {text}"
    );
    assert!(text.contains("\"final\":\"PROXY\""));

    let binary = singbox_binary();
    if !binary.is_file() {
        eprintln!("skipping: no sing-box binary at {}", binary.display());
        return Ok(());
    }
    let dir = caly_platform::paths::test_helpers::unique_path_under("caly-rules-sb", "fixture");
    std::fs::create_dir_all(&dir)?;
    let config = dir.join("config.json");
    std::fs::write(&config, &json)?;
    let check = Command::new(&binary)
        .args(["check", "-c"])
        .arg(&config)
        .output()?;
    assert!(
        check.status.success(),
        "sing-box check rejected the rules-rendered config:\n{}\n---config---\n{}",
        String::from_utf8_lossy(&check.stderr),
        text
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
