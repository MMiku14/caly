//! Core-agnostic TUN device configuration model.
//!
//! Describes the TUN protocol stack (`gvisor`/`mixed`/`system`), automatic
//! routing and strict routing without knowing whether the backend is Mihomo or
//! sing-box. Renderers in the infrastructure layer translate these values into
//! core-specific `tun:` blocks.

/// Minimum accepted TUN interface MTU.
pub const MIN_TUN_MTU: u16 = 576;
/// Maximum accepted TUN interface MTU.
pub const MAX_TUN_MTU: u16 = 9_000;

/// The user-space packet-stack implementation used for TUN traffic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TunStack {
    /// gVisor user-space network stack (default; best compatibility).
    #[default]
    Gvisor,
    /// Mixed stack: system for TCP, gVisor for UDP.
    Mixed,
    /// System kernel network stack (fastest, requires privileges).
    System,
}

impl TunStack {
    /// Returns the stable renderer-agnostic label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gvisor => "gvisor",
            Self::Mixed => "mixed",
            Self::System => "system",
        }
    }
}

/// Complete TUN device configuration validated before any renderer sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunConfig {
    stack: TunStack,
    auto_route: bool,
    strict_route: bool,
    mtu: u16,
}

impl TunConfig {
    /// Constructs a TUN configuration with the given stack and routing intent.
    pub fn new(
        stack: TunStack,
        auto_route: bool,
        strict_route: bool,
        mtu: u16,
    ) -> Result<Self, TunError> {
        if !(MIN_TUN_MTU..=MAX_TUN_MTU).contains(&mtu) {
            return Err(TunError::InvalidMtu);
        }
        Ok(Self {
            stack,
            auto_route,
            strict_route,
            mtu,
        })
    }

    /// Returns the packet-stack implementation.
    pub const fn stack(&self) -> TunStack {
        self.stack
    }
    /// Returns whether the daemon should install routes automatically.
    pub const fn auto_route(&self) -> bool {
        self.auto_route
    }
    /// Returns whether routing should be strict (force all traffic through TUN).
    pub const fn strict_route(&self) -> bool {
        self.strict_route
    }
    /// Returns the TUN interface MTU.
    pub const fn mtu(&self) -> u16 {
        self.mtu
    }
}

/// TUN configuration validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TunError {
    /// MTU outside the accepted range.
    InvalidMtu,
}

impl core::fmt::Display for TunError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidMtu => write!(
                formatter,
                "TUN MTU must be between {MIN_TUN_MTU} and {MAX_TUN_MTU}"
            ),
        }
    }
}

impl std::error::Error for TunError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvisor_default_and_valid_mtu() -> Result<(), TunError> {
        let tun = TunConfig::new(TunStack::Gvisor, true, true, 1_500)?;
        assert_eq!(tun.stack(), TunStack::Gvisor);
        assert!(tun.auto_route());
        assert!(tun.strict_route());
        assert_eq!(tun.mtu(), 1_500);
        Ok(())
    }

    #[test]
    fn mixed_and_system_stacks_render_labels() {
        assert_eq!(TunStack::Mixed.label(), "mixed");
        assert_eq!(TunStack::System.label(), "system");
        assert_eq!(TunStack::Gvisor.label(), "gvisor");
    }

    #[test]
    fn out_of_range_mtu_is_rejected() {
        assert_eq!(
            TunConfig::new(TunStack::System, false, false, 100),
            Err(TunError::InvalidMtu)
        );
        assert_eq!(
            TunConfig::new(TunStack::Gvisor, true, true, 9_001),
            Err(TunError::InvalidMtu)
        );
    }
}
