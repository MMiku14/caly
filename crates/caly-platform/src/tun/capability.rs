//! Failure shape for the Linux TUN backend.
//!
//! Round 31: the TUN backend's [`failure`],
//! [`failure_with_detail`], and [`failure_parts`]
//! helpers used to live alongside the engage
//! pipeline in `linux.rs` (an 871-line monolith
//! that exceeded the 400-line hard cap — S4.1
//! advisory). The helpers extract into this file
//! so the engage + restore pipeline (`device.rs`)
//! and the escalate / script builders
//! (`escalate.rs` / `script.rs`) can all reuse
//! the same structured failure shape without
//! dragging the 460-line orchestration with
//! them. The hint text is the diagnostic the
//! operator sees after a `pkexec` / `sudo`
//! escalation failure — the wording is the
//! Round 11 single-prompt / `caly doctor --fix`
//! / setcap guidance that the pre-Round 31
//! inline comments guarded against accidental
//! drift.

use caly_domain::BoundedText;

use crate::PlatformFailure;

/// Round 31: the bounded failure with
/// informational detail (`setcap` / `doctor --fix`
/// guidance + a self-check command). Carries
/// the same fields the pre-Round 31
/// `failure_with_detail` did, plus a
/// `getcap` self-check command the operator
/// can run without elevated privileges to
/// confirm the current capability state. The
/// pre-Round 31 hint stopped at "grant
/// CAP_NET_ADMIN once with `caly doctor
/// --fix`"; the operator had to dig through
/// the docs to find the verification command.
/// The Round 31 hint surfaces it inline so
/// the operator can paste the same line into
/// the terminal to see whether the fix has
/// landed.
#[allow(dead_code)]
pub(super) fn failure_with_detail(message: &str) -> PlatformFailure {
    failure_parts(
        message,
        "verify current capabilities with `getcap $(which mihomo) $(which sing-box) $(which ip)`; \
         grant CAP_NET_ADMIN once with `caly doctor --fix` (a single sudo prompt for ip and the \
         core binaries), allow passwordless sudo for ip, or run the daemon with CAP_NET_ADMIN; \
         escalation via pkexec/sudo was attempted per tun.escalation",
    )
}

/// Round 31: the bounded failure without the
/// escalated-attempt detail. Used by direct
/// (non-escalation) code paths where the
/// `setcap` / `doctor --fix` wording would
/// mislead the operator (they did not ask for
/// escalation; the failure is a direct
/// permission / environment issue).
pub(super) fn failure(message: &str) -> PlatformFailure {
    failure_parts(message, "check ip command and CAP_NET_ADMIN")
}

/// Round 31: the canonical `PlatformFailure`
/// constructor. The first three fields are static
/// literals well within the bounded 1 KiB /
/// 512-byte capacities, so `BoundedText::new`
/// cannot fail for them. The `message` and
/// `suggested_action` fields are formatted at the
/// call site from caller-supplied `&str`; a
/// future caller that accidentally exceeds the
/// bound used to abort the daemon here. The
/// previous `unwrap_or_else(|_| process::abort)`
/// form was a process-kill fallback for an
/// unreachable path; the infallible
/// `from_nonempty_clamped` constructor keeps the
/// same behaviour for the well-formed call sites
/// and surfaces a stable `"_"` fallback for any
/// future refactor that accidentally widens the
/// input.
fn failure_parts(message: &str, action: &str) -> PlatformFailure {
    let m = if message.is_empty() { "_" } else { message };
    let a = if action.is_empty() { "_" } else { action };
    PlatformFailure {
        operation: BoundedText::from_nonempty_clamped("tun-operation".to_owned(), "_"),
        resource: BoundedText::from_nonempty_clamped("linux-tun".to_owned(), "_"),
        message: BoundedText::from_nonempty_clamped(m.to_owned(), "_"),
        suggested_action: BoundedText::from_nonempty_clamped(a.to_owned(), "_"),
    }
}

#[cfg(test)]
mod failure_tests {
    //! Round 31: the `failure` / `failure_with_detail`
    //! helpers are the single source of truth for the
    //! TUN backend's structured failure shape. A
    //! future change to the hint wording lands here
    //! (one place) instead of across the 3-4 call
    //! sites in `device.rs` / `escalate.rs`. The
    //! tests assert the bounded-text fallback (the
    //! `from_nonempty_clamped` constructor's "_"
    //! fallback) so a future caller passing an
    //! empty message does not abort the daemon.
    use super::{failure, failure_with_detail};

    /// A non-empty message goes through the
    /// `BoundedText::from_nonempty_clamped` path
    /// verbatim; the `Display` impl renders the
    /// message unchanged. The `operation` /
    /// `resource` fields stay at the canonical
    /// "tun-operation" / "linux-tun" labels.
    #[test]
    fn failure_passes_message_through() {
        let f = failure("direct failure");
        assert!(f.message.as_str().contains("direct failure"));
        assert!(f.suggested_action.as_str().contains("CAP_NET_ADMIN"));
    }

    /// The escalated-attempt variant carries
    /// the long-form `doctor --fix` hint plus
    /// a `getcap` self-check command the
    /// operator can run without elevated
    /// privileges. The pre-Round 31 hint
    /// stopped at "grant CAP_NET_ADMIN once";
    /// the operator had to dig through the
    /// docs to find the verification command.
    /// The Round 31 hint surfaces it inline.
    #[test]
    fn failure_with_detail_carries_doctor_hint() {
        let f = failure_with_detail("escalated failure");
        assert!(f.message.as_str().contains("escalated failure"));
        assert!(f.suggested_action.as_str().contains("caly doctor --fix"));
        assert!(f.suggested_action.as_str().contains("CAP_NET_ADMIN"));
    }

    /// Round 31: the `getcap` self-check is
    /// part of the operator-visible contract.
    /// The verification command must be the
    /// first thing the operator sees so they
    /// can confirm the current capability
    /// state before re-running the engage.
    /// A future hint rewording must keep
    /// `getcap` first.
    #[test]
    fn failure_with_detail_carries_getcap_self_check() {
        let f = failure_with_detail("escalated failure");
        let hint = f.suggested_action.as_str();
        let getcap_idx = hint.find("getcap").unwrap();
        let doctor_idx = hint.find("caly doctor --fix").unwrap();
        assert!(
            getcap_idx < doctor_idx,
            "getcap self-check must appear before `caly doctor --fix` in the hint, got {hint:?}"
        );
        // The `getcap` command covers all
        // three binaries the TUN engage
        // touches (mihomo / sing-box / ip)
        // so a single command confirms the
        // entire TUN capability surface.
        assert!(hint.contains("$(which mihomo)"));
        assert!(hint.contains("$(which sing-box)"));
        assert!(hint.contains("$(which ip)"));
    }

    /// Empty input falls back to the
    /// `"_"` placeholder so the daemon does
    /// not abort on a future caller that
    /// accidentally widens the input. The
    /// pre-Round 31 shape used
    /// `unwrap_or_else(|_| process::abort)`
    /// for the same condition; the new
    /// infallible constructor is strictly
    /// safer (the daemon stays up on a
    /// caller bug).
    #[test]
    fn failure_does_not_abort_on_empty_input() {
        let f = failure("");
        assert!(!f.message.as_str().is_empty());
        assert!(!f.suggested_action.as_str().is_empty());
    }
}
