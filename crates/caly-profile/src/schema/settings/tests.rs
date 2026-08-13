//! Tests for `schema/settings.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::schema::parse_and_validate_yaml;

#[test]
fn generated_base_parses_and_validates() {
    let parsed = parse_and_validate_yaml(render_default_base().as_bytes());
    assert!(parsed.is_ok(), "base config must validate: {parsed:?}");
}

#[test]
fn generated_fragments_validate_individually() {
    for (path, contents) in render_default_config_files() {
        // Fragments are merged onto the base by the layered loader; each one
        // must at least parse as YAML on its own.
        let value: serde_norway::Value = serde_norway::from_str(&contents)
            .unwrap_or_else(|error| panic!("{} must parse: {error}", path.display()));
        assert!(
            value.as_mapping().is_some(),
            "{} must be a mapping",
            path.display()
        );
    }
}

#[test]
fn enabled_option_blocks_validate_when_substituted() {
    // The documented replacement blocks must produce valid configurations.
    let core_binaries = "schema_version: 1\ncore: mihomo\n\
                         core_binaries:\n  mihomo: /opt/mihomo/mihomo\n  \
                         sing_box: /opt/sing-box/sing-box\n";
    assert!(parse_and_validate_yaml(core_binaries.as_bytes()).is_ok());
    let subscription = "schema_version: 1\ncore: mihomo\n\
                        subscriptions:\n  url: https://provider.example/subscription\n";
    assert!(parse_and_validate_yaml(subscription.as_bytes()).is_ok());
}

#[test]
fn fragment_paths_are_sorted_deterministically() {
    let paths: Vec<PathBuf> = render_default_config_files()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "fragment merge order must be stable");
}

#[test]
fn generated_layout_round_trips_through_the_layered_loader() {
    use crate::loader::{LayeredConfigPaths, LoaderLimits, load_layered_yaml};
    // Write exactly what `caly config generate` writes, then load it back
    // through the real layered loader: the generated default must boot.
    let root = std::env::temp_dir().join(format!(
        "caly-settings-roundtrip-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |value| value.as_nanos())
    ));
    std::fs::create_dir_all(&root).unwrap_or_default();
    std::fs::write(root.join("config.yaml"), render_default_base()).unwrap_or_default();
    for (relative, contents) in render_default_config_files() {
        let destination = root.join(relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).unwrap_or_default();
        }
        std::fs::write(&destination, contents).unwrap_or_default();
    }
    let loaded = load_layered_yaml(
        &LayeredConfigPaths::new(root.clone(), None),
        LoaderLimits::secure_default(),
    );
    std::fs::remove_dir_all(&root).ok();
    let config = loaded.unwrap_or_else(|error| panic!("generated layout must load: {error:?}"));
    assert_eq!(config.controllers.mihomo, "127.0.0.1:9090");
    assert_eq!(config.telemetry.interval_ms, 1_000);
    assert!(!config.tun.enabled);
    assert!(config.subscriptions.url.is_none());
}

#[test]
fn generated_routing_fragment_documents_new_matchers() {
    let (_, contents) = crate::schema::fragments_routing::routing_fragment();
    for token in [
        "DOMAIN,",
        "DOMAIN-SUFFIX,",
        "DOMAIN-KEYWORD,",
        "GEOIP,",
        "IP-CIDR,",
        "SRC-IP-CIDR,",
        "PROCESS-NAME,",
        "RULE-SET,",
        "GEOSITE,",
        "MATCH,",
    ] {
        assert!(
            contents.contains(token),
            "routing fragment must document `{token}` matcher"
        );
    }
    // The default config is `rules: []` — every snippet is commented,
    // so a fresh install has zero user rules.
    let parsed: serde_norway::Value = serde_norway::from_str(&contents)
        .unwrap_or_else(|error| panic!("routing fragment must parse: {error}"));
    assert!(parsed.get("rules").is_some(), "rules: key must exist");
    assert_eq!(
        parsed.get("rules").and_then(|v| v.as_sequence()),
        Some(&serde_norway::Sequence::new()),
        "rules default must be an empty list"
    );
}

#[test]
fn generated_rule_providers_fragment_documents_three_source_kinds() {
    let (_, contents) = crate::schema::fragments_routing::rule_providers_fragment();
    for token in [
        "type: http",
        "type: file",
        "type: inline",
        "behavior: domain_suffix",
        "behavior: ip_cidr",
        "format: source",
        "rule_providers:",
        "Loyalsoldier/v2ray-rules-dat",
    ] {
        assert!(
            contents.contains(token),
            "rule-providers fragment must document `{token}`"
        );
    }
    // The default config is `rule_providers: []` — every snippet is
    // commented, so a fresh install has zero user rule providers.
    let parsed: serde_norway::Value = serde_norway::from_str(&contents)
        .unwrap_or_else(|error| panic!("rule-providers fragment must parse: {error}"));
    assert_eq!(
        parsed.get("rule_providers").and_then(|v| v.as_sequence()),
        Some(&serde_norway::Sequence::new()),
        "rule_providers default must be an empty list"
    );
}

#[test]
fn generated_profiles_fragment_documents_three_source_kinds() {
    let (_, contents) = crate::schema::fragments_profiles::profiles_fragment();
    for token in [
        "kind: remote",
        "kind: local",
        "kind: merge",
        "interval_minutes:",
        "parts: [team-shared",
        "profiles:",
    ] {
        assert!(
            contents.contains(token),
            "profiles fragment must document `{token}`"
        );
    }
    let parsed: serde_norway::Value = serde_norway::from_str(&contents)
        .unwrap_or_else(|error| panic!("profiles fragment must parse: {error}"));
    assert_eq!(
        parsed.get("profiles").and_then(|v| v.as_sequence()),
        Some(&serde_norway::Sequence::new()),
        "profiles default must be an empty list"
    );
}

#[test]
fn generated_proxy_groups_fragment_documents_five_kinds() {
    let (_, contents) = crate::schema::fragments_proxy_groups::proxy_groups_fragment();
    for token in [
        "type: select",
        "type: url-test",
        "type: fallback",
        "type: load-balance",
        "type: relay",
        "kind: node",
        "kind: group",
        "kind: direct",
        "kind: reject",
        "proxy_groups:",
    ] {
        assert!(
            contents.contains(token),
            "proxy-groups fragment must document `{token}`"
        );
    }
    // The default config is `proxy_groups: []` — every
    // snippet is commented, so a fresh install has zero
    // user proxy groups.
    let parsed: serde_norway::Value = serde_norway::from_str(&contents)
        .unwrap_or_else(|error| panic!("proxy-groups fragment must parse: {error}"));
    assert!(
        parsed.get("proxy_groups").is_some(),
        "proxy_groups: key must exist"
    );
    assert_eq!(
        parsed.get("proxy_groups").and_then(|v| v.as_sequence()),
        Some(&serde_norway::Sequence::new()),
        "proxy_groups default must be an empty list"
    );
}
