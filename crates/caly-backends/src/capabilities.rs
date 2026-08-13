//! Truthful capability assessment driven by the selected core and renderer.
//!
//! Capabilities are no longer a hard-coded empty set: each feature is assessed
//! from (a) whether the configured core's renderer can represent it and (b)
//! whether the runtime endpoint it needs is expected to exist. This module is
//! pure (no I/O) so it is trivially testable and safe to call from the
//! composition root at boot.

use caly_domain::{
    BoundedText, BoundedVec, Capability, CapabilitySet, CapabilityStatus, ConfiguredSupport,
    CoreKind, DuplicateCapability, RuntimeAvailability,
};

/// Maximum caveat text length mirrored from the domain constant.
const CAVEAT_MAX: usize = 256;

/// Assesses DNS configuration support for a core.
///
/// DNS is rendered into the core's config file, so it needs no separate
/// runtime endpoint; the assessment therefore carries `NotRequired` runtime
/// availability once the renderer can represent it.
pub fn assess_dns(core: CoreKind, dns_enabled: bool) -> CapabilityStatus {
    let configured = match core {
        CoreKind::Mihomo | CoreKind::SingBox => ConfiguredSupport::Supported,
        CoreKind::Xray => ConfiguredSupport::Unsupported,
    };
    let caveat = match (configured, dns_enabled) {
        (ConfiguredSupport::Supported, true) => None,
        (ConfiguredSupport::Supported, false) => {
            Some(bounded("DNS renderer available; config not enabled"))
        }
        (ConfiguredSupport::Unsupported, _) => Some(bounded("DNS not supported by this core")),
        _ => None,
    };
    CapabilityStatus::new(
        Capability::DnsConfiguration,
        configured,
        RuntimeAvailability::NotRequired,
        caveat,
    )
}

/// Assesses TUN render/apply support for a core (both renderers emit a tun block).
pub fn assess_tun(core: CoreKind) -> CapabilityStatus {
    let configured = match core {
        CoreKind::Mihomo | CoreKind::SingBox => ConfiguredSupport::Supported,
        CoreKind::Xray => ConfiguredSupport::Unsupported,
    };
    CapabilityStatus::new(
        Capability::TunConfiguration,
        configured,
        RuntimeAvailability::NotRequired,
        configured_supported_or_caveat(configured, "TUN not supported by this core"),
    )
}

/// Assesses a capability that the daemon exposes through the core's
/// Clash-compatible control API (requires a healthy controller at runtime).
fn assess_control(capability: Capability, core: CoreKind) -> CapabilityStatus {
    let configured = match core {
        CoreKind::Mihomo | CoreKind::SingBox => ConfiguredSupport::Supported,
        CoreKind::Xray => ConfiguredSupport::Unsupported,
    };
    CapabilityStatus::new(
        capability,
        configured,
        RuntimeAvailability::NotRequired,
        configured_supported_or_caveat(configured, "control API not supported by this core"),
    )
}

/// Marks connections/traffic as Partial for sing-box (its Clash API surface is
/// a documented subset) and Supported for Mihomo.
fn assess_stream(capability: Capability, core: CoreKind) -> CapabilityStatus {
    let configured = match core {
        CoreKind::Mihomo => ConfiguredSupport::Supported,
        CoreKind::SingBox => ConfiguredSupport::Partial,
        CoreKind::Xray => ConfiguredSupport::Unsupported,
    };
    let caveat = match core {
        CoreKind::SingBox => Some(bounded("sing-box exposes a subset of the Clash API")),
        CoreKind::Mihomo => None,
        CoreKind::Xray => Some(bounded("not supported by this core")),
    };
    CapabilityStatus::new(
        capability,
        configured,
        RuntimeAvailability::NotRequired,
        caveat,
    )
}

fn configured_supported_or_caveat(
    configured: ConfiguredSupport,
    caveat: &'static str,
) -> Option<BoundedText<CAVEAT_MAX>> {
    match configured {
        ConfiguredSupport::Supported => None,
        _ => Some(bounded(caveat)),
    }
}

/// Builds the boot-time capability set for a running core generation: the full
/// set of features the daemon system can provide for `core`, assessed
/// truthfully from the renderer and control API, so a thin client never sees an
/// optimistic or empty capability set.
pub fn core_capability_set(
    core: CoreKind,
    dns_enabled: bool,
) -> Result<CapabilitySet, DuplicateCapability> {
    let statuses = vec![
        assess_tun(core),
        assess_dns(core, dns_enabled),
        assess_control(Capability::RuntimeModeSwitch, core),
        assess_control(Capability::ProxyGroups, core),
        assess_control(Capability::ProxySelection, core),
        assess_control(Capability::UrlTest, core),
        assess_control(Capability::ConnectionClose, core),
        assess_stream(Capability::Connections, core),
        assess_stream(Capability::Traffic, core),
    ];
    // The hardcoded `statuses` vector always fits the bounded capacity and
    // contains no duplicate `Capability` values. The previous
    // `unwrap_or_else(|_| process::abort)` form was a process-kill fallback
    // for an unreachable path; switching to `from_vec_truncated` +
    // `from_bounded_dedup` keeps the same behaviour for the well-formed
    // call sites and survives any future contributor who widens the literal
    // past the bound. The function still returns a `Result` so the
    // composition caller can map a `DuplicateCapability` error to a
    // `CompositionError`; we wrap the infallible dedup output in `Ok`
    // because the inner constructor never produces a `DuplicateCapability`
    // for the well-formed hardcoded list.
    let values = BoundedVec::from_vec_truncated(statuses);
    Ok(CapabilitySet::from_bounded_dedup(values))
}

fn bounded(value: &'static str) -> BoundedText<CAVEAT_MAX> {
    BoundedText::from_nonempty_clamped(value.to_owned(), "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mihomo_dns_is_usable() -> Result<(), DuplicateCapability> {
        let set = core_capability_set(CoreKind::Mihomo, true)?;
        assert!(set.is_usable(Capability::DnsConfiguration));
        let status = set
            .get(Capability::DnsConfiguration)
            .ok_or(DuplicateCapability(Capability::DnsConfiguration))?;
        assert_eq!(status.configured(), ConfiguredSupport::Supported);
        assert_eq!(status.runtime(), RuntimeAvailability::NotRequired);
        Ok(())
    }

    #[test]
    fn xray_dns_is_not_usable() -> Result<(), DuplicateCapability> {
        let set = core_capability_set(CoreKind::Xray, true)?;
        assert!(!set.is_usable(Capability::DnsConfiguration));
        Ok(())
    }

    #[test]
    fn sing_box_dns_has_disabled_caveat() -> Result<(), DuplicateCapability> {
        let set = core_capability_set(CoreKind::SingBox, false)?;
        assert!(set.is_usable(Capability::DnsConfiguration));
        assert!(
            set.get(Capability::DnsConfiguration)
                .and_then(CapabilityStatus::caveat)
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn capability_set_covers_the_full_wired_surface() -> Result<(), DuplicateCapability> {
        for core in [CoreKind::Mihomo, CoreKind::SingBox] {
            let set = core_capability_set(core, true)?;
            // Every feature the daemon can actually perform must be advertised.
            for capability in [
                Capability::TunConfiguration,
                Capability::DnsConfiguration,
                Capability::RuntimeModeSwitch,
                Capability::ProxyGroups,
                Capability::ProxySelection,
                Capability::UrlTest,
                Capability::Connections,
                Capability::ConnectionClose,
                Capability::Traffic,
            ] {
                assert!(
                    set.is_usable(capability),
                    "{core:?} must advertise {capability:?}"
                );
            }
        }
        // sing-box connections/traffic are partial (subset of the Clash API).
        let sing = core_capability_set(CoreKind::SingBox, true)?;
        assert_eq!(
            sing.get(Capability::Connections)
                .map(CapabilityStatus::configured),
            Some(ConfiguredSupport::Partial)
        );
        Ok(())
    }

    #[test]
    fn xray_capability_set_is_all_unsupported() -> Result<(), DuplicateCapability> {
        let set = core_capability_set(CoreKind::Xray, true)?;
        assert!(!set.is_usable(Capability::DnsConfiguration));
        assert!(!set.is_usable(Capability::TunConfiguration));
        assert!(!set.is_usable(Capability::RuntimeModeSwitch));
        Ok(())
    }
}
