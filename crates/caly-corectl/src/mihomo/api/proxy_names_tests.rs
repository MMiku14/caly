//! Tests for `mihomo/api.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

fn parse(body: &str) -> Vec<String> {
    // The real production parser, fed a canned controller document.
    let json: serde_json::Value = serde_json::from_str(body).unwrap();
    super::proxy_names_from_json(&json)
}

#[test]
fn mihomo_shape_lists_top_level_nodes_and_deduplicates_selector_members() {
    let body = r#"{"proxies":{
        "PROXY":{"type":"Selector","all":["n1","n2","direct"]},
        "GLOBAL":{"type":"Selector","all":["n1","n2"]},
        "n1":{"type":"Shadowsocks"},
        "n2":{"type":"Vless"},
        "direct":{"type":"Direct"},
        "REJECT":{"type":"Reject"}
    }}"#;
    let names = parse(body);
    assert_eq!(names, vec!["n1", "n2"], "deduplicated node names only");
}

#[test]
fn sing_box_shape_collects_nodes_from_selector_members() {
    let body = r#"{"proxies":{
        "PROXY":{"type":"Selector","all":["proxy-a","proxy-b","direct"]},
        "GLOBAL":{"type":"Selector","all":["proxy-a","proxy-b"]},
        "direct":{"type":"Direct"}
    }}"#;
    let names = parse(body);
    assert_eq!(names, vec!["proxy-a", "proxy-b"]);
}
