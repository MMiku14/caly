//! TunActor command backend owning the Linux TUN engage/restore lifecycle.

use caly_domain::PlatformEffectView;
use caly_platform::{
    PlatformFailure,
    tun::{InterfaceName, LinuxTunBackend, OwnedTun, TunBackend, TunEscalation, TunRequest},
};
use caly_ports::{ActorFailure, ActorFailureKind, TunCommandBackend};

/// Owns the configured MTU, interface name and the currently engaged TUN.
pub struct LinuxTunCommandBackend {
    backend: LinuxTunBackend,
    interface: InterfaceName,
    mtu: u16,
    owned: Option<Box<dyn OwnedTun>>,
}

impl LinuxTunCommandBackend {
    /// Creates an owner with a fixed interface name and MTU from configuration.
    pub fn new(interface: InterfaceName, mtu: u16) -> Result<Self, ActorFailure> {
        Self::new_with_escalation(interface, mtu, TunEscalation::default())
    }

    /// Creates an owner with an explicit privilege-escalation policy for the
    /// privileged `ip` commands (pkexec/sudo when the daemon lacks
    /// `CAP_NET_ADMIN`).
    pub fn new_with_escalation(
        interface: InterfaceName,
        mtu: u16,
        escalation: TunEscalation,
    ) -> Result<Self, ActorFailure> {
        if !(576..=9_000).contains(&mtu) {
            return Err(crate::failure(
                "TUN MTU must be between 576 and 9000",
                "configure a valid TUN MTU",
            ));
        }
        Ok(Self {
            backend: LinuxTunBackend::default().with_escalation(escalation),
            interface,
            mtu,
            owned: None,
        })
    }

    /// Returns whether caly currently owns an engaged TUN interface.
    pub fn engaged(&self) -> bool {
        self.owned.is_some()
    }

    /// Returns the configured interface name (used by durable recovery).
    pub fn interface(&self) -> &str {
        self.interface.as_str()
    }

    /// Returns the configured MTU (used by durable recovery).
    pub const fn mtu(&self) -> u16 {
        self.mtu
    }
}

impl TunCommandBackend for LinuxTunCommandBackend {
    fn set_tun(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        if enabled {
            if self.owned.is_some() {
                return Ok(PlatformEffectView::tun(true));
            }
            let request = TunRequest {
                interface: self.interface.clone(),
                mtu: self.mtu,
            };
            let owned = self.backend.engage(request).map_err(tun_failure)?;
            self.owned = Some(owned);
            Ok(PlatformEffectView::tun(true))
        } else {
            if let Some(owned) = self.owned.take() {
                owned.restore().map_err(tun_failure)?;
            }
            Ok(PlatformEffectView::none())
        }
    }
}

fn tun_failure(error: PlatformFailure) -> ActorFailure {
    // `PlatformFailure::message` is `BoundedText<1_024>` but
    // `ActorFailure::message` is the tighter `BoundedText<512>`. The previous
    // `ActorFailure::new(...).unwrap_or_else(|_| abort)` form killed the
    // daemon when an OS error description (often a 600–1024 byte stderr
    // chain from `ip`/polkit) was wrapped into a TUN failure message.
    // `ActorFailure::clamped` UTF-8-safely clamps the message to the
    // 512-byte bound with a stable fallback, so the daemon survives a
    // chatty error without losing the actionable context.
    ActorFailure::clamped(
        ActorFailureKind::Infrastructure,
        error.message.as_str(),
        error.suggested_action.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::BoundedText;

    fn interface(name: &str) -> InterfaceName {
        InterfaceName::new(name.to_owned()).unwrap()
    }

    #[test]
    fn out_of_range_mtu_is_rejected() {
        assert!(LinuxTunCommandBackend::new(interface("caly0"), 100).is_err());
        assert!(LinuxTunCommandBackend::new(interface("caly0"), 9_001).is_err());
    }

    #[test]
    fn starts_disengaged() -> Result<(), String> {
        let backend =
            LinuxTunCommandBackend::new(interface("caly0"), 1_500).map_err(|e| format!("{e:?}"))?;
        assert!(!backend.engaged());
        Ok(())
    }

    /// Regression: `tun_failure` previously did
    /// `ActorFailure::new(...).unwrap_or_else(|_| process::abort)`. That
    /// killed the daemon whenever `PlatformFailure::message` (bounded to
    /// 1_024 bytes) exceeded the tighter 512-byte `ActorFailure::message`
    /// bound. A real `ip` or polkit stderr chain routinely runs 600–1024
    /// bytes. The infallible `clamped` constructor must clamp to the
    /// bound instead of aborting.
    #[test]
    fn tun_failure_clamps_oversized_message_without_aborting() {
        let long_message: String = "e".repeat(900);
        let failure = PlatformFailure {
            operation: BoundedText::from_nonempty_clamped("op".to_owned(), "_"),
            resource: BoundedText::from_nonempty_clamped("res".to_owned(), "_"),
            message: BoundedText::from_nonempty_clamped(long_message, "_"),
            suggested_action: BoundedText::from_nonempty_clamped("retry".to_owned(), "_"),
        };
        let actor = tun_failure(failure);
        // Message is bounded to 512 bytes (the ActorFailure bound) and
        // survives the conversion without aborting.
        assert!(
            actor.message.as_str().len() <= 512,
            "message must be clamped: got {} bytes",
            actor.message.as_str().len()
        );
        assert!(std::str::from_utf8(actor.message.as_str().as_bytes()).is_ok());
    }

    /// Even an empty `PlatformFailure::message` (a real possibility when
    /// the kernel emits no stderr) must produce a non-empty actor failure
    /// so the wire representation never carries an empty cell.
    #[test]
    fn tun_failure_uses_stable_fallback_for_empty_message() {
        let failure = PlatformFailure {
            operation: BoundedText::from_nonempty_clamped("op".to_owned(), "_"),
            resource: BoundedText::from_nonempty_clamped("res".to_owned(), "_"),
            message: BoundedText::from_nonempty_clamped(String::new(), "_"),
            suggested_action: BoundedText::from_nonempty_clamped(String::new(), "_"),
        };
        let actor = tun_failure(failure);
        assert_eq!(actor.message.as_str(), "_");
        assert_eq!(actor.suggested_action.as_str(), "_");
    }
}
