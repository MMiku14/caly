//! PlatformActor system-side-effect command port.

use caly_domain::PlatformEffectView;

use super::error::ActorFailure;

/// Nonblocking PlatformActor backend owning recovery transactions.
pub trait PlatformCommandBackend {
    fn set_system_proxy(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure>;
    /// `sysproxy pac <url>`: switch the desktop proxy to its auto mode
    /// pointing at the PAC URL. Backends without PAC support (KDE /
    /// niri) return an `Unsupported`-class failure with an actionable
    /// message instead of silently ignoring the request.
    fn set_system_proxy_pac(&mut self, url: &str) -> Result<PlatformEffectView, ActorFailure>;
}
