//! Ordered event cursor values.

use core::fmt;

use crate::DaemonInstanceId;

/// Monotonic sequence allocated by the sole Event Sequencer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EventSequence(u64);

impl EventSequence {
    /// Sequence before the first emitted event.
    pub const ZERO: Self = Self(0);

    /// Constructs a sequence received from a trusted persistence boundary.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the primitive sequence value.
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Advances without ever wrapping.
    pub fn checked_next(self) -> Result<Self, SequenceExhausted> {
        self.0.checked_add(1).map(Self).ok_or(SequenceExhausted)
    }
}

/// Fatal indication that a daemon epoch cannot allocate another event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceExhausted;

impl fmt::Display for SequenceExhausted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("event sequence exhausted; shut down and start a new daemon instance")
    }
}

impl std::error::Error for SequenceExhausted {}

/// Replay position scoped to one daemon lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventCursor {
    daemon_instance: DaemonInstanceId,
    sequence: EventSequence,
}

impl EventCursor {
    /// Constructs an epoch-aware cursor.
    pub const fn new(daemon_instance: DaemonInstanceId, sequence: EventSequence) -> Self {
        Self {
            daemon_instance,
            sequence,
        }
    }

    /// Returns the daemon epoch.
    pub const fn daemon_instance(self) -> DaemonInstanceId {
        self.daemon_instance
    }

    /// Returns the position within the daemon epoch.
    pub const fn sequence(self) -> EventSequence {
        self.sequence
    }
}

/// Client action required when comparing a cursor with the current stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorDisposition {
    /// The next event can be applied directly.
    Apply,
    /// The event is already represented locally.
    Stale,
    /// Events are missing or the daemon restarted; fetch a full snapshot.
    FullSnapshotRequired,
}

/// Classifies an incoming cursor without guessing missing state.
pub fn classify_cursor(current: EventCursor, incoming: EventCursor) -> CursorDisposition {
    if current.daemon_instance != incoming.daemon_instance {
        return CursorDisposition::FullSnapshotRequired;
    }
    if incoming.sequence <= current.sequence {
        return CursorDisposition::Stale;
    }
    match current.sequence.checked_next() {
        Ok(expected) if expected == incoming.sequence => CursorDisposition::Apply,
        Ok(_) | Err(_) => CursorDisposition::FullSnapshotRequired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(epoch: u8, sequence: u64) -> EventCursor {
        EventCursor::new(
            DaemonInstanceId::from_bytes([epoch; 16]),
            EventSequence::new(sequence),
        )
    }

    #[test]
    fn sequence_never_wraps() {
        assert_eq!(
            EventSequence::new(7).checked_next(),
            Ok(EventSequence::new(8))
        );
        assert_eq!(
            EventSequence::new(u64::MAX).checked_next(),
            Err(SequenceExhausted)
        );
    }

    #[test]
    fn restart_and_gap_require_snapshot() {
        assert_eq!(
            classify_cursor(cursor(1, 7), cursor(2, 1)),
            CursorDisposition::FullSnapshotRequired
        );
        assert_eq!(
            classify_cursor(cursor(1, 7), cursor(1, 9)),
            CursorDisposition::FullSnapshotRequired
        );
        assert_eq!(
            classify_cursor(cursor(1, 7), cursor(1, 8)),
            CursorDisposition::Apply
        );
    }
}
