//! Application events: two complementary streams.
//!
//! - The projection event stream (`ApplicationEvent` = `PresentationDelta`,
//!   ordered by the sole `EventSequencer`) carries presentation deltas for
//!   snapshot sync.
//! - The daemon event bus (`DaemonEvent`, `EventBus`) carries pipeline-stage
//!   facts for reconcilers and observability (刀 2, 2026-08-12 pipeline
//!   design) — publish/subscribe, process-local, non-blocking.

mod bus;
mod sequencer;

pub use bus::{DaemonEvent, EVENT_HISTORY_CAP, EventBus, RecordedEvent};
pub use caly_domain::PresentationDelta as ApplicationEvent;
pub use sequencer::{EventSequencer, SequencedEvent, SequencerError};
