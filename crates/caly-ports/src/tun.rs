//! TunActor side-effect command port.

use caly_domain::PlatformEffectView;

use super::error::ActorFailure;

/// Nonblocking TunActor backend owning TUN engage/restore lifecycle.
///
/// The concrete MTU and interface are fixed at construction from configuration,
/// so the toggle only carries the desired engagement flag.
pub trait TunCommandBackend {
    /// Engages (true) or restores (false) the owned TUN interface.
    fn set_tun(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure>;
}
