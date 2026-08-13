//! Pure projection of externally visible platform side effects.

use crate::BoundedText;

/// Maximum degraded-reason length in UTF-8 bytes.
pub const DEGRADED_REASON_MAX_BYTES: usize = 512;

/// Redacted view of platform side effects.
///
/// Concrete proxy snapshots, recovery records, process handles and TUN handles
/// remain in `caly-platform` and their owner actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformEffectView {
    proxy_engaged: bool,
    tun_engaged: bool,
    recovery_pending: bool,
    degraded_reason: Option<BoundedText<DEGRADED_REASON_MAX_BYTES>>,
}

impl PlatformEffectView {
    /// Constructs a platform-effect projection.
    pub const fn new(
        proxy_engaged: bool,
        tun_engaged: bool,
        recovery_pending: bool,
        degraded_reason: Option<BoundedText<DEGRADED_REASON_MAX_BYTES>>,
    ) -> Self {
        Self {
            proxy_engaged,
            tun_engaged,
            recovery_pending,
            degraded_reason,
        }
    }

    /// The canonical view carrying no owned platform effect.
    pub const fn none() -> Self {
        Self {
            proxy_engaged: false,
            tun_engaged: false,
            recovery_pending: false,
            degraded_reason: None,
        }
    }

    /// View after engaging or restoring the desktop system proxy.
    pub const fn proxy(engaged: bool) -> Self {
        Self {
            proxy_engaged: engaged,
            tun_engaged: false,
            recovery_pending: false,
            degraded_reason: None,
        }
    }

    /// View after engaging or restoring the TUN device.
    pub const fn tun(engaged: bool) -> Self {
        Self {
            proxy_engaged: false,
            tun_engaged: engaged,
            recovery_pending: false,
            degraded_reason: None,
        }
    }

    /// Returns whether caly currently owns an applied system-proxy change.
    pub const fn proxy_engaged(&self) -> bool {
        self.proxy_engaged
    }
    /// Returns whether caly currently owns an applied TUN side effect.
    pub const fn tun_engaged(&self) -> bool {
        self.tun_engaged
    }
    /// Returns whether durable crash recovery remains outstanding.
    pub const fn recovery_pending(&self) -> bool {
        self.recovery_pending
    }
    /// Returns a safe explanation of degraded platform behavior.
    pub const fn degraded_reason(&self) -> Option<&BoundedText<DEGRADED_REASON_MAX_BYTES>> {
        self.degraded_reason.as_ref()
    }
}
