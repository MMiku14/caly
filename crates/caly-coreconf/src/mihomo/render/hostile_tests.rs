//! Tests for `subscription/mihomo/render.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

/// Any remaining raw control byte would make the emitted YAML invalid
/// (yaml.v3 rejects control characters in double-quoted scalars).
fn assert_no_raw_controls(value: &str) {
    assert!(
        !value.chars().any(char::is_control),
        "emitted YAML contains a raw control character: {value:?}"
    );
}

#[test]
fn hostile_names_never_leak_raw_controls() {
    for name in [
        "\u{0000}injected",
        "line\u{0001}feed",
        "snowman\u{001F}unit",
        "del\u{007F}char",
        "c1\u{0085}control",
        "tab\there",
        "line\nbreak",
    ] {
        let quoted = yaml_quote(name);
        assert_no_raw_controls(&quoted);
        assert!(quoted.starts_with('"') && quoted.ends_with('"'));
    }
}

#[test]
fn hostile_names_cannot_break_out_of_the_scalar() {
    // A hostile name must not be able to terminate the value and start a
    // new YAML mapping (name + colon + space) or a comment.
    let quoted = yaml_quote("good: name\n  evil: true #comment");
    assert_no_raw_controls(&quoted);
    assert!(
        !quoted.contains("\n  evil"),
        "newline must be escaped, not emitted literally"
    );
}

#[test]
fn ordinary_names_stay_readable() {
    assert_eq!(yaml_quote("hk-01"), "\"hk-01\"");
    assert_eq!(yaml_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
}

#[test]
fn control_escapes_match_yaml_canonical_forms() {
    assert_eq!(yaml_quote("\u{0000}"), "\"\\0\"");
    assert_eq!(yaml_quote("\u{0008}"), "\"\\b\"");
    assert_eq!(yaml_quote("\u{000C}"), "\"\\f\"");
    assert_eq!(yaml_quote("\u{0001}"), "\"\\x01\"");
    assert_eq!(yaml_quote("\u{007F}"), "\"\\x7f\"");
    assert_eq!(yaml_quote("\u{0085}"), "\"\\x85\"");
}

/// A shadowsocks node with a v2ray-plugin parameter must render the
/// plugin into the Mihomo YAML (mihomo dials it via `plugin:` +
/// `plugin-opts:`); dropping it would emit a node that can never
/// connect (2026-08-12: the parser now carries the plugin, so the
/// renderer must not lose it).
#[test]
fn ss_plugin_node_renders_plugin_lines() -> Result<(), String> {
    let subscription = caly_domain::SubscriptionId::from_bytes([9; 16]);
    let node = caly_subscription::parse_any_proxy_uri(
        "ss://bm9uZTpwYXNz@1.2.3.4:443?plugin=v2ray-plugin%3Bmode%3Dwebsocket#ss-plug",
        subscription,
    )
    .map_err(|e| format!("{e:?}"))?;
    let entry = proxy_to_entry(
        &node,
        super::super::proxy_sections::MihomoProxyTag::new("ss-plug".to_owned()).unwrap(),
    )
    .map_err(|e| format!("{e:?}"))?;
    let yaml = entry.yaml.as_str();
    assert!(yaml.contains("plugin: \"v2ray-plugin\""), "{yaml}");
    assert!(yaml.contains("plugin-opts: \"mode=websocket\""), "{yaml}");
    Ok(())
}
