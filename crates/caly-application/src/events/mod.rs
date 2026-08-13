//! Application events: two complementary streams.
//!
//! - The projection event stream (`ApplicationEvent` = `PresentationDelta`,
//!   ordered by the sole `EventSequencer`) carries presentation deltas for
//!   snapshot sync.
//! - The daemon event bus (`DaemonEvent`, `EventBus`) carries pipeline-stage
//!   facts for reconcilers and observability (刀 2, 2026-08-12).

mod bus;
mod sequencer;

pub use bus::{run_reconciler, DaemonEvent, EventBus, RecordedEvent, EVENT_HISTORY_CAP};
pub use caly_domain::PresentationDelta as ApplicationEvent;
pub use sequencer::{EventSequencer, SequencedEvent, SequencerError};
