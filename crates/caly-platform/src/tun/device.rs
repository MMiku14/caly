//! Engage / restore pipeline for the Linux TUN backend.
//!
//! Round 31: the engage + restore orchestration
//! for the Linux TUN backend used to live in
//! `linux.rs` (an 871-line monolith that exceeded
//! the 400-line hard cap — S4.1 advisory). The
//! orchestration (and its `LinuxOwnedTun` handle)
//! extract into this file so the per-attempt /
//! per-batch runners (`escalate.rs`), the
//! shell-quoting helpers (`script.rs`), and the
//! failure shape (`capability.rs`) can all be
//! reused without dragging the 200-line
//! orchestration with them.

use std::path::Path;

use caly_domain::BoundedText;

use crate::PlatformFailure;
use crate::command::LinuxCommandRunner;

use super::escalate::{TunEscalation, run_ip, run_ip_sequence};
use super::script::tun_command;
use super::{OwnedTun, TunBackend, TunRequest};

type InterfaceName = BoundedText<64>;

/// Round 31: the `OwnedTun` handle for a Linux
/// TUN device. The pre-Round 31 inline struct
/// lived in `linux.rs` next to the engage
/// pipeline; the extraction keeps the handle
/// co-located with the engage / restore code
/// that owns it.
struct LinuxOwnedTun {
    interface: InterfaceName,
    runner: LinuxCommandRunner,
    escalation: TunEscalation,
}

/// Round 31: the Linux TUN backend entry point.
/// The pre-Round 31 impl block lived in
/// `linux.rs`; the extraction moves only the
/// engage pipeline (the platform-faithful
/// "adopt the held device" / "drop the stale
/// one" / "create fresh" state machine) to
/// this file.
pub struct LinuxTunBackend {
    runner: LinuxCommandRunner,
    escalation: TunEscalation,
}

impl Default for LinuxTunBackend {
    fn default() -> Self {
        Self {
            runner: LinuxCommandRunner,
            escalation: TunEscalation::default(),
        }
    }
}

impl LinuxTunBackend {
    /// Round 31: overrides the privilege-escalation
    /// policy for privileged `ip` commands. The
    /// pre-Round 31 inline `with_escalation`
    /// moved to this module unchanged.
    #[must_use]
    pub const fn with_escalation(mut self, escalation: TunEscalation) -> Self {
        self.escalation = escalation;
        self
    }
}

impl TunBackend for LinuxTunBackend {
    fn engage(&mut self, request: TunRequest) -> Result<Box<dyn OwnedTun>, PlatformFailure> {
        engage_with(&mut self.runner, self.escalation, request)
    }
}

impl OwnedTun for LinuxOwnedTun {
    fn interface(&self) -> &InterfaceName {
        &self.interface
    }

    fn restore(mut self: Box<Self>) -> Result<(), PlatformFailure> {
        restore_tun(&mut self.runner, self.escalation, self.interface.as_str())
    }
}

/// Round 31: the engage pipeline. The contract is
/// *ensure*, not *own*: the config apply that
/// precedes the platform engage already restarted
/// the core with a `tun` inbound, and the core
/// itself creates and configures the device. So an
/// interface that already exists is ADOPTED
/// (MTU/UP confirmed idempotently) rather than
/// deleted and recreated — deleting a device a live
/// process holds fails with EBUSY, and treating that
/// ready signal as an engage failure used to leave
/// the core hijacking traffic into a device the
/// platform layer did not track.
///
/// A *stale* interface (left behind by a crashed
/// daemon or an interrupted earlier engage) is still
/// dropped first: the delete succeeds when no
/// process holds the device, and the interface is
/// then created fresh.
///
/// Every `ip` command is tried directly first; when
/// the daemon lacks `CAP_NET_ADMIN` the remaining
/// sequence is retried as ONE escalated `sh -c`
/// batch (`pkexec`, then `sudo -n`), so the user is
/// prompted for the root password at most once per
/// engage instead of once per `ip` command.
pub(crate) fn engage_with(
    runner: &mut impl crate::command::CommandRunner,
    escalation: TunEscalation,
    request: TunRequest,
) -> Result<Box<dyn OwnedTun>, PlatformFailure> {
    let interface = request.interface.clone();
    let name = interface.as_str();
    // The name is interpolated into `/sys/class/net/<name>` and into `ip`
    // argv: enforce the kernel's own rules (IFNAMSIZ) up front so a crafted
    // config cannot fail deep inside the pipeline — or worse.
    if !is_valid_interface_name(name) {
        return Err(super::capability::failure(
            "TUN interface name is invalid (kernel limit is 15 bytes; ASCII letters, digits, `-`, `_`, `.` only)",
        ));
    }
    let mtu = request.mtu;
    let ensure = || {
        vec![
            tun_command(vec!["link", "set", "dev", name, "mtu", &mtu.to_string()]),
            tun_command(vec!["link", "set", "dev", name, "up"]),
        ]
    };
    if interface_exists(name) {
        // Distinguish the two owners of an existing interface by probing the
        // delete DIRECTLY first (no escalation): EBUSY means a live process
        // (the core's tun inbound) holds the device — a ready signal that
        // must short-circuit before any pkexec/sudo attempt, so a held
        // device never triggers a pointless password prompt. Any other
        // direct failure is retried through the escalation chain; only when
        // the delete succeeds is the interface recreated fresh.
        let delete = vec!["tuntap", "del", "dev", name, "mode", "tun"];
        let arguments = super::script::build_arguments(&delete)?;
        let direct = super::escalate::attempt(
            runner,
            "ip",
            &[],
            &arguments,
            super::escalate::DIRECT_TIMEOUT,
        );
        if direct.is_err() && !matches!(direct, Err(ref error) if is_busy_failure(error)) {
            // Stale leftover that needs privileges (or a real failure): let
            // the escalation chain decide, keeping its clear error.
            run_ip(runner, escalation, delete)?;
        } else if direct.is_err() {
            // Held by the running core: adopt the device as-is. The idempotent
            // MTU/UP confirmation doubles as the readiness check — a held but
            // half-configured device fails here with a real error.
            run_ip_sequence(runner, escalation, &ensure())?;
            return Ok(Box::new(LinuxOwnedTun {
                interface,
                runner: LinuxCommandRunner,
                escalation,
            }));
        }
        // direct delete succeeded: stale interface removed, create fresh below.
    }
    let mut commands = vec![tun_command(vec![
        "tuntap", "add", "dev", name, "mode", "tun",
    ])];
    commands.extend(ensure());
    if let Err(error) = run_ip_sequence(runner, escalation, &commands) {
        // Best-effort cleanup of a partially applied engage. The cleanup MUST
        // use `TunEscalation::None` regardless of the original policy: a
        // failed engage that already prompted for the root password (or one
        // that failed to) must never prompt the user a second time for a
        // best-effort rollback. The previous implementation passed through the
        // caller's `escalation` argument, which contradicted the inline
        // "never another password prompt" comment and would have surfaced a
        // second polkit / sudo prompt on every failed engage.
        let _ = run_ip(
            runner,
            TunEscalation::None,
            vec!["tuntap", "del", "dev", name, "mode", "tun"],
        );
        return Err(error);
    }
    Ok(Box::new(LinuxOwnedTun {
        interface,
        runner: LinuxCommandRunner,
        escalation,
    }))
}

/// Round 31: deletes the TUN device, tolerating a
/// busy interface: when the core still holds the
/// device (restore raced the kernel restart) the
/// delete fails with EBUSY and the core itself
/// releases the interface on shutdown — that is a
/// successful restore, not an error. Every other
/// failure (permission, absent device) keeps its
/// original error.
pub(crate) fn restore_tun(
    runner: &mut impl crate::command::CommandRunner,
    escalation: TunEscalation,
    name: &str,
) -> Result<(), PlatformFailure> {
    match run_ip(
        runner,
        escalation,
        vec!["tuntap", "del", "dev", name, "mode", "tun"],
    ) {
        Err(error) if is_busy_failure(&error) => Ok(()),
        other => other,
    }
}

/// Round 31: whether a failed `ip` command signals
/// that the interface is held by a live process
/// ("Device or resource busy") rather than a
/// permission or environmental failure. A held
/// device is a ready signal during engage (the
/// core's tun inbound owns it) and a no-op during
/// restore (the core releases it on restart).
pub(crate) fn is_busy_failure(error: &PlatformFailure) -> bool {
    // The platform command runner pins `LC_ALL=C` for every spawned tool, so
    // the busy signal arrives as the English "Device or resource busy"
    // regardless of the daemon's locale (this match was a locale time-bomb
    // before the runner pinned the locale).
    error.message.as_str().to_ascii_lowercase().contains("busy")
}

/// Round 31: whether a network interface with the
/// requested name already exists. `/sys/class/net`
/// reflects every interface the kernel knows about
/// (persist TUN devices included), so a leftover
/// `caly0` from a previous run is detected here
/// and removed before a fresh one is created.
pub(crate) fn interface_exists(name: &str) -> bool {
    // Defense in depth: never feed a name that could escape `/sys/class/net`
    // into a filesystem join (callers validate first; a violation here simply
    // reports "does not exist").
    if !is_valid_interface_name(name) {
        return false;
    }
    Path::new("/sys/class/net").join(name).is_dir()
}

/// Whether `name` is a legal kernel interface name: 1..=15 bytes
/// (IFNAMSIZ-1), no `/`, no whitespace/control characters, and not a `.` /
/// `..` path component. This is the precise contract both sysfs joins and
/// `ip` argv need; the old code accepted any 64-byte text.
pub(crate) fn is_valid_interface_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 15 {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    name.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'='))
}

#[cfg(test)]
mod device_tests;
