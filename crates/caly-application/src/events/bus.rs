//! Daemon event bus: process-wide domain facts for the pipeline layer.
//!
//! Distinct from the projection event stream (`ApplicationEvent` +
//! `EventSequencer`, which carries ordered presentation deltas for snapshot
//! sync). This bus carries pipeline-stage facts (刀 2, 2026-08-12 pipeline
//! design): a producer publishes a fact (`SubscriptionRefreshed { changed
//! }`), zero or more consumers subscribe and react — e.g. the config
//! reconciler re-renders the kernel config only when a refresh actually
//! changed nodes. Publishing never blocks: a slow subscriber misses events
//! rather than stalling the pipeline. A bounded recent-history ring keeps
//! the last events for observability.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Domain facts published by pipeline stages.
#[derive(Clone, Debug)]
pub enum DaemonEvent {
    /// A subscription refresh completed. `changed == true` means at least
    /// one source delivered updated content, so the kernel config should
    /// be re-rendered (the config reconciler's trigger).
    SubscriptionRefreshed { changed: bool },
    /// Config rendering/apply failed and was rolled back.
    ConfigApplyFailed { reason: String },
    /// Config rendering/apply succeeded at the given generation.
    ConfigApplied { generation: u64 },
}

/// Bounded recent-event history entry for observability.
#[derive(Clone, Debug)]
pub struct RecordedEvent {
    /// Monotonic wall-clock instant when the event was published.
    pub at: Instant,
    /// The event itself.
    pub event: DaemonEvent,
}

/// Maximum number of events retained in the recent-history ring.
pub const EVENT_HISTORY_CAP: usize = 64;
/// Per-subscriber queue capacity: a consumer slower than this misses
/// events (try_send fails full) instead of blocking the publisher.
pub const EVENT_CHANNEL_CAP: usize = 256;

/// Lightweight multi-subscriber event bus.
///
/// Clone is cheap (shared state); hand a clone to every producer and
/// consumer. Subscribers use [`EventBus::subscribe`] and receive every
/// event published after subscription — there is no replay, by design:
/// reconcilers converge from current state, history is for humans.
#[derive(Clone, Default)]
pub struct EventBus {
    subscribers: Arc<Mutex<Vec<std::sync::mpsc::SyncSender<DaemonEvent>>>>,
    history: Arc<Mutex<VecDeque<RecordedEvent>>>,
}

impl EventBus {
    /// Creates an empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes a new consumer. Events published after this call are
    /// delivered; a dropped receiver is pruned on the next publish.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<DaemonEvent> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(EVENT_CHANNEL_CAP);
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender);
        }
        receiver
    }

    /// Publishes an event to every subscriber and records it in the ring.
    /// Never blocks and never fails the caller: a dead subscriber is
    /// silently dropped, a full history ring drops the oldest entry.
    pub fn publish(&self, event: DaemonEvent) {
        tracing::debug!(?event, "daemon event");
        if let Ok(mut history) = self.history.lock() {
            history.push_back(RecordedEvent {
                at: Instant::now(),
                event: event.clone(),
            });
            while history.len() > EVENT_HISTORY_CAP {
                history.pop_front();
            }
        }
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        // Full → the consumer is slow; drop the event for it but keep the
        // subscription. Dropping is a deliberate back-pressure choice, but
        // it must be visible: a reconciler that misses `changed: true` lets
        // the kernel config go stale (2026-08-12 agent audit).
        let mut dropped = 0u32;
        subscribers.retain(|sender| match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                dropped += 1;
                true
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
        });
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "daemon event dropped for a slow subscriber (queue full)"
            );
        }
    }

    /// Returns the recent-history ring (oldest first) for observability.
    pub fn recent(&self) -> Vec<RecordedEvent> {
        self.history
            .lock()
            .map(|history| history.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn publish_reaches_subscribers_in_order() {
        let bus = EventBus::new();
        let receiver = bus.subscribe();
        bus.publish(DaemonEvent::SubscriptionRefreshed { changed: true });
        bus.publish(DaemonEvent::ConfigApplied { generation: 3 });
        let first = receiver
            .recv_timeout(Duration::from_millis(10))
            .expect("first event");
        let second = receiver
            .recv_timeout(Duration::from_millis(10))
            .expect("second event");
        assert!(matches!(
            first,
            DaemonEvent::SubscriptionRefreshed { changed: true }
        ));
        assert!(matches!(
            second,
            DaemonEvent::ConfigApplied { generation: 3 }
        ));
    }

    #[test]
    fn history_keeps_recent_events_bounded() {
        let bus = EventBus::new();
        for index in 0..(EVENT_HISTORY_CAP + 10) {
            bus.publish(DaemonEvent::ConfigApplied {
                generation: index as u64,
            });
        }
        let recent = bus.recent();
        assert_eq!(recent.len(), EVENT_HISTORY_CAP);
        assert!(matches!(
            recent.first().expect("oldest recorded").event,
            DaemonEvent::ConfigApplied { generation: 10 }
        ));
        assert!(matches!(
            recent.last().expect("newest recorded").event,
            DaemonEvent::ConfigApplied { generation: 73 }
        ));
    }

    #[test]
    fn dead_subscriber_does_not_block_publish() {
        let bus = EventBus::new();
        {
            // Receiver dropped immediately: the sender must be pruned on
            // the next publish instead of erroring the caller.
            let _receiver = bus.subscribe();
        }
        bus.publish(DaemonEvent::SubscriptionRefreshed { changed: false });
        let receiver = bus.subscribe();
        bus.publish(DaemonEvent::ConfigApplied { generation: 1 });
        assert!(
            receiver.recv_timeout(Duration::from_millis(10)).is_ok(),
            "live subscriber must still receive after a dead one"
        );
    }
}
