//! SubscriptionActor refresh port.

use caly_domain::{SnapshotNodes, SubscriptionId};

use super::error::ActorFailure;

/// W2-β2b (CLI v3 §4.3): how one `refresh` command should run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RefreshMode {
    /// `sub refresh --force`: ignore the cached validators
    /// (ETag / Last-Modified) and fetch the whole body again.
    pub force: bool,
    /// Daemon-side periodic timer (#59): only sources whose
    /// per-source `refresh_every_minutes` cadence is due run (a
    /// source with `Some(0)` — e.g. a local file — never does).
    /// Manual CLI refreshes always run, cadence notwithstanding.
    pub scheduled: bool,
}

/// Outcome of one `refresh` run: the refreshed node snapshot plus a
/// `changed` flag telling the caller whether the node set actually moved
/// (any target source reported updated content). `changed == false` means
/// the daemon can skip re-rendering the kernel config (which restarts the
/// core) without losing anything (2026-08-12 auto-apply design).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefreshOutcome {
    /// The refreshed node set, already merged into the registry.
    pub nodes: SnapshotNodes,
    /// Whether at least one target source delivered updated content.
    pub changed: bool,
}

/// Nonblocking backend owning fetch/parse/cache generation work.
///
/// `subscription` is the all-zero id for "every enabled source"
/// (the pre-β2b behaviour), or a concrete id for a single source
/// (`sub refresh <name-or-url>`).
pub trait SubscriptionCommandBackend {
    fn refresh(
        &mut self,
        subscription: SubscriptionId,
        mode: RefreshMode,
    ) -> Result<RefreshOutcome, ActorFailure>;
}
