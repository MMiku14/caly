//! Capability-list presentation helpers for `caly status`.

use caly_protocol::protocol::v2::WirePresentationSnapshot;

pub(super) fn print_capabilities(snapshot: &WirePresentationSnapshot) {
    if snapshot.capabilities.is_empty() {
        println!("capabilities: none");
        return;
    }
    println!("capabilities:");
    for capability in &snapshot.capabilities {
        println!(
            "  {:<16} configured={:<10} runtime={:<10} {}",
            capability_label(capability.capability),
            configured_label(capability.configured),
            runtime_label(capability.runtime),
            capability.caveat.as_deref().unwrap_or("")
        );
    }
}

fn capability_label(value: i32) -> &'static str {
    match value {
        1 => "tun-config",
        2 => "dns-config",
        3 => "mode-switch",
        4 => "proxy-groups",
        5 => "proxy-selection",
        6 => "url-test",
        7 => "connections",
        8 => "connection-close",
        9 => "traffic",
        10 => "logs",
        11 => "rules",
        _ => "unknown",
    }
}

fn configured_label(value: i32) -> &'static str {
    match value {
        1 => "unsupported",
        2 => "supported",
        3 => "partial",
        _ => "unknown",
    }
}

fn runtime_label(value: i32) -> &'static str {
    match value {
        1 => "not-required",
        2 => "available",
        3 => "unavailable",
        4 => "unknown",
        _ => "unknown",
    }
}
