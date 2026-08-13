//! Single event ordering, replay and reliable projector delivery.

use std::{
    collections::VecDeque,
    sync::mpsc::{SyncSender, TrySendError},
};

use caly_domain::{DaemonInstanceId, EventCursor, EventSequence, SequenceExhausted};

use super::ApplicationEvent;

/// Event with a daemon-epoch cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequencedEvent {
    pub cursor: EventCursor,
    pub event: ApplicationEvent,
}

/// Publish failure that never silently drops the event.
#[derive(Debug, Eq, PartialEq)]
pub enum SequencerError {
    InvalidReplayCapacity,
    SequenceExhausted(ApplicationEvent),
    ProjectorBackpressure(ApplicationEvent),
    ProjectorClosed(ApplicationEvent),
}

/// Sole allocator of event sequence numbers.
pub struct EventSequencer {
    daemon_instance: DaemonInstanceId,
    current: EventSequence,
    replay: VecDeque<SequencedEvent>,
    replay_capacity: usize,
    projector: SyncSender<SequencedEvent>,
}

impl EventSequencer {
    /// Creates a positive-capacity sequencer at the start of a daemon epoch.
    pub fn new(
        daemon_instance: DaemonInstanceId,
        replay_capacity: usize,
        projector: SyncSender<SequencedEvent>,
    ) -> Result<Self, SequencerError> {
        Self::new_at(
            EventCursor::new(daemon_instance, EventSequence::ZERO),
            replay_capacity,
            projector,
        )
    }

    /// Resumes from an authoritative snapshot cursor with an empty replay window.
    pub fn new_at(
        cursor: EventCursor,
        replay_capacity: usize,
        projector: SyncSender<SequencedEvent>,
    ) -> Result<Self, SequencerError> {
        if replay_capacity == 0 {
            return Err(SequencerError::InvalidReplayCapacity);
        }
        Ok(Self {
            daemon_instance: cursor.daemon_instance(),
            current: cursor.sequence(),
            replay: VecDeque::with_capacity(replay_capacity),
            replay_capacity,
            projector,
        })
    }

    /// Publishes only when reliable projector delivery accepts the event.
    pub fn try_publish(&mut self, event: ApplicationEvent) -> Result<EventCursor, SequencerError> {
        let next = self
            .current
            .checked_next()
            .map_err(|SequenceExhausted| SequencerError::SequenceExhausted(event.clone()))?;
        let cursor = EventCursor::new(self.daemon_instance, next);
        let sequenced = SequencedEvent { cursor, event };
        match self.projector.try_send(sequenced.clone()) {
            Ok(()) => Ok(self.commit_published(sequenced)),
            Err(TrySendError::Full(value)) => {
                Err(SequencerError::ProjectorBackpressure(value.event))
            }
            Err(TrySendError::Disconnected(value)) => {
                Err(SequencerError::ProjectorClosed(value.event))
            }
        }
    }

    /// Returns replay strictly after a cursor, or None when snapshot recovery is required.
    pub fn replay_after(&self, cursor: EventCursor) -> Option<Vec<SequencedEvent>> {
        if cursor.daemon_instance() != self.daemon_instance {
            return None;
        }
        if cursor.sequence() > self.current {
            return None;
        }
        if cursor.sequence() == self.current {
            return Some(Vec::new());
        }
        let first = self.replay.front()?.cursor.sequence();
        let expected = cursor.sequence().checked_next().ok()?;
        if expected < first {
            return None;
        }
        let index = lower_bound(&self.replay, cursor.sequence());
        Some(self.replay.iter().skip(index).cloned().collect())
    }

    fn commit_published(&mut self, sequenced: SequencedEvent) -> EventCursor {
        self.current = sequenced.cursor.sequence();
        self.replay.push_back(sequenced);
        while self.replay.len() > self.replay_capacity {
            self.replay.pop_front();
        }
        EventCursor::new(self.daemon_instance, self.current)
    }
}

fn lower_bound(events: &VecDeque<SequencedEvent>, sequence: EventSequence) -> usize {
    let mut low = 0;
    let mut high = events.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if events[middle].cursor.sequence() <= sequence {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

impl core::fmt::Display for SequencerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "event sequencing failed: backpressure, closure, or exhausted epoch"
        )
    }
}

impl std::error::Error for SequencerError {}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn full_projector_channel_does_not_consume_sequence() -> Result<(), Box<dyn std::error::Error>>
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut sequencer = EventSequencer::new(DaemonInstanceId::from_bytes([1; 16]), 4, sender)?;
        let first = ApplicationEvent::ObservedReplaced(caly_domain::ObservedState::default());
        assert_eq!(
            sequencer.try_publish(first)?.sequence(),
            EventSequence::new(1)
        );
        let blocked = ApplicationEvent::ObservedReplaced(caly_domain::ObservedState::default());
        assert!(matches!(
            sequencer.try_publish(blocked),
            Err(SequencerError::ProjectorBackpressure(_))
        ));
        let _received = receiver.recv()?;
        let third = ApplicationEvent::ObservedReplaced(caly_domain::ObservedState::default());
        assert_eq!(
            sequencer.try_publish(third)?.sequence(),
            EventSequence::new(2)
        );
        Ok(())
    }

    #[test]
    fn future_or_wrong_epoch_cursor_requires_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let epoch = DaemonInstanceId::from_bytes([1; 16]);
        let sequencer = EventSequencer::new(epoch, 4, sender)?;
        assert!(
            sequencer
                .replay_after(EventCursor::new(epoch, EventSequence::new(9)))
                .is_none()
        );
        let other = DaemonInstanceId::from_bytes([2; 16]);
        assert!(
            sequencer
                .replay_after(EventCursor::new(other, EventSequence::ZERO))
                .is_none()
        );
        Ok(())
    }
}
