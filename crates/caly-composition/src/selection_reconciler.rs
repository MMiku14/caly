//! Selection reconciler (刀 3, 2026-08-12 pipeline design).
//!
//! Subscribes to pipeline events and reconciles the persisted node
//! selection against the registry after a changed subscription refresh.
//! The selection intent is the stable `node_id`; the display name follows
//! it — so a core restart restores the operator's choice even when the
//! subscription renamed the node (the classic `restore` 404). When the node
//! disappeared from every refresh, the record is cleared instead.

use std::time::Duration;

use caly_application::events::{DaemonEvent, EventBus};
use caly_backends::{CoreNodeRegistry, dual::SharedActiveCore};

/// Poll interval; `recv_timeout` wakes periodically and only a disconnected
/// bus (daemon shutdown) ends the thread.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Spawns the selection reconciler on the shared event bus and registry.
pub fn spawn(event_bus: EventBus, registry: CoreNodeRegistry, active_cell: SharedActiveCore) {
    let events = event_bus.subscribe();
    std::thread::spawn(move || {
        loop {
            match events.recv_timeout(POLL_INTERVAL) {
                Ok(DaemonEvent::SubscriptionRefreshed { changed: true }) => {
                    // Reconcile against the ACTIVE core's spelling: a
                    // sing-box record must never be rewritten with the
                    // mihomo display name (reconcile skips foreign-core
                    // records internally).
                    let active = active_cell
                        .lock()
                        .map_or(caly_domain::CoreKind::Mihomo, |kind| *kind);
                    caly_backends::selection::reconcile(active, |node_id| {
                        registry
                            .lock()
                            .map(|nodes| nodes.get(node_id).map(|proxy| proxy.name.clone()))
                            .map_err(|_| ())
                    });
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}
