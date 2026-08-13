//! Real-kernel TUN E2E test (requires CAP_NET_ADMIN in the current netns).
//!
//! Run with:
//!   unshare -Urn cargo test -p caly-platform --test tun_e2e --locked
//!
//! Without CAP_NET_ADMIN the test probes a real TUN create first and skips,
//! so an unprivileged `cargo test --all` never fails on this file.

use std::process::Command;

use caly_domain::BoundedText;
use caly_platform::tun::{LinuxTunBackend, TunBackend, TunRequest};

const PROBE_IFACE: &str = "caly-probe-e2e";
const IFACE_ENGAGE: &str = "caly-e2e-0";
const IFACE_ROUTE: &str = "caly-e2e-1";

fn iface(name: &str) -> Result<BoundedText<64>, &'static str> {
    BoundedText::new(name.to_owned()).map_err(|_| "interface name exceeds the bound")
}

/// Returns true only if a real TUN create succeeded, i.e. CAP_NET_ADMIN is present.
fn have_cap_net_admin() -> bool {
    let probe = Command::new("ip")
        .args(["tuntap", "add", "dev", PROBE_IFACE, "mode", "tun"])
        .output();
    match probe {
        Ok(out) if out.status.success() => {
            let _ = Command::new("ip")
                .args(["tuntap", "del", "dev", PROBE_IFACE, "mode", "tun"])
                .status();
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!("TUN probe denied (no CAP_NET_ADMIN?): {stderr}");
            false
        }
        Err(err) => {
            eprintln!("ip command unavailable: {err}");
            false
        }
    }
}

type E2eResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn tun_engage_mtu_up_and_restore() -> E2eResult {
    if !have_cap_net_admin() {
        eprintln!("skipping: CAP_NET_ADMIN not available");
        return Ok(());
    }

    let mut backend = LinuxTunBackend::default();
    let owned = backend
        .engage(TunRequest {
            interface: iface(IFACE_ENGAGE)?,
            mtu: 1500,
        })
        .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;

    // Interface must exist, be up, and carry the requested MTU.
    let out = Command::new("ip")
        .args(["link", "show", "dev", IFACE_ENGAGE])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("mtu 1500"), "wrong mtu: {text}");
    assert!(text.contains("UP"), "interface not up: {text}");

    // The owned handle exposes the created interface name.
    assert_eq!(owned.interface().as_str(), IFACE_ENGAGE);

    // Restore must delete the interface.
    owned
        .restore()
        .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
    let out = Command::new("ip")
        .args(["link", "show", "dev", IFACE_ENGAGE])
        .output()?;
    assert!(
        !out.status.success(),
        "interface still present after restore"
    );
    Ok(())
}

#[test]
fn tun_address_assignment_and_routing() -> E2eResult {
    if !have_cap_net_admin() {
        eprintln!("skipping: CAP_NET_ADMIN not available");
        return Ok(());
    }

    let mut backend = LinuxTunBackend::default();
    let owned = backend
        .engage(TunRequest {
            interface: iface(IFACE_ROUTE)?,
            mtu: 1500,
        })
        .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;

    // Assign an IPv4 address and a non-connected route; verify the kernel reflects both.
    assign_and_verify_addr_route()?;

    // Restore must tear down the interface (removing address + routes).
    owned
        .restore()
        .map_err(|error| Box::<dyn std::error::Error>::from(format!("{error:?}")))?;
    let out = Command::new("ip")
        .args(["link", "show", "dev", IFACE_ROUTE])
        .output()?;
    assert!(
        !out.status.success(),
        "interface still present after restore"
    );
    Ok(())
}

/// Assigns an IPv4 address and a non-connected route to the TUN interface and
/// verifies both are reflected by the real kernel.
fn assign_and_verify_addr_route() -> E2eResult {
    let addr = "10.77.0.1/24";
    let assigned = Command::new("ip")
        .args(["addr", "add", addr, "dev", IFACE_ROUTE])
        .output()?;
    assert!(assigned.status.success(), "ip addr add failed");

    let out = Command::new("ip")
        .args(["addr", "show", IFACE_ROUTE])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("inet 10.77.0.1/24"),
        "address not assigned: {text}"
    );

    let route = "10.99.0.0/24";
    let added = Command::new("ip")
        .args(["route", "add", route, "dev", IFACE_ROUTE])
        .output()?;
    assert!(added.status.success(), "ip route add failed");
    let rout = Command::new("ip").args(["route", "show"]).output()?;
    let rout_text = String::from_utf8_lossy(&rout.stdout);
    assert!(rout_text.contains(route), "route not present: {rout_text}");
    Ok(())
}
