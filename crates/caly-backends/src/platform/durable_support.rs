//! Shared plumbing for the durable platform side-effect backends.
//!
//! Both `DurableSystemProxyBackend` and `DurableTunBackend` follow the same
//! "persist-before-apply, clear-on-disable" transaction shape and convert
//! errors through the same bounded-text helpers. Keeping those helpers in one
//! place avoids drift between the two concrete backends.

use caly_domain::BoundedText;
use caly_platform::{PlatformFailure, recovery::RecoveryStoreError};
use caly_ports::{ActorFailure, ActorFailureKind};

/// Builds a bounded text, clamping over-long input at a char boundary.
///
/// Durable restore must survive hostile/malformed state, so truncation is
/// preferred over aborting — the daemon keeps running even when a message
/// exceeds the bound.
pub(crate) fn bounded<const MAX: usize>(value: &str) -> BoundedText<MAX> {
    // Over-long messages are truncated at a char boundary instead of aborting
    // the daemon (durable restore must survive hostile/malformed state).
    BoundedText::from_nonempty_clamped(value.to_owned(), "error")
}

/// Maps an [`ActorFailure`] to a [`PlatformFailure`] with a fixed operation and
/// resource label for the restore path.
pub(crate) fn actor_to_platform(
    error: ActorFailure,
    operation: &str,
    resource: &str,
) -> PlatformFailure {
    PlatformFailure {
        operation: bounded(operation),
        resource: bounded(resource),
        message: bounded(error.message.as_str()),
        suggested_action: bounded(error.suggested_action.as_str()),
    }
}

/// Maps a recovery-store error to an infrastructure [`ActorFailure`].
///
/// `kind` is a short label (e.g. "proxy" or "tun") embedded in the message.
/// The recovery-store error carries a free-form `String` (typically a path
/// or OS error description) and can easily exceed the bounded failure-text
/// capacity; the previous `ActorFailure::new(...).unwrap_or_else(|_| abort)`
/// form was a process-kill fallback for the (realistically reachable)
/// over-long path. `ActorFailure::clamped` keeps the same behaviour for
/// the well-formed call sites and surfaces a stable `"_"` fallback for any
/// future error variant that overshoots the bound.
pub(crate) fn durable_failure(error: RecoveryStoreError, kind: &str) -> ActorFailure {
    ActorFailure::clamped(
        ActorFailureKind::Infrastructure,
        &format!("durable {kind} record failed: {error:?}"),
        "inspect the recovery store path and permissions",
    )
}
