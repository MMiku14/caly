//! Recoverable TUN side-effect contract.
//!
//! Round 31: the Linux TUN backend used to live in
//! a single 871-line `linux.rs` file (S4.1
//! advisory — the 400-line hard cap). The
//! extraction splits the backend into four
//! focused modules without changing the public
//! surface:
//!
//! - `device` — engage / restore pipeline +
//!   `LinuxTunBackend` / `LinuxOwnedTun` +
//!   integration tests (the orchestration
//!   state machine).
//! - `escalate` — `TunEscalation` policy +
//!   per-attempt runners (the privilege-
//!   escalation mechanics).
//! - `script` — `tun_command` / `build_arguments` /
//!   `build_batch_script` (the shell-quoting
//!   helpers).
//! - `capability` — the structured failure
//!   shape (`failure` / `failure_with_detail` /
//!   `failure_parts` + the `doctor --fix` hint
//!   text).
//!
//! The `linux` module is gone; `LinuxTunBackend`
//! re-exports the device-module entry point so
//! the public API stays unchanged (`use
//! crate::tun::LinuxTunBackend;` keeps working
//! at every call site).

mod capability;
mod device;
mod escalate;
mod script;

pub use device::LinuxTunBackend;
pub use escalate::TunEscalation;

use caly_domain::BoundedText;

use crate::PlatformFailure;

/// Single validated interface component.
pub type InterfaceName = BoundedText<64>;

/// TUN ownership settings.
pub struct TunRequest {
    pub interface: InterfaceName,
    pub mtu: u16,
}

/// Sole owned TUN side effect.
pub trait OwnedTun: Send {
    fn interface(&self) -> &InterfaceName;
    fn restore(self: Box<Self>) -> Result<(), PlatformFailure>;
}

/// Platform backend that verifies conflicts before engagement.
pub trait TunBackend {
    fn engage(&mut self, request: TunRequest) -> Result<Box<dyn OwnedTun>, PlatformFailure>;
}
