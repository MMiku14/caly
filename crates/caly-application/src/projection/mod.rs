//! Reliable five-state presentation projection.

mod projector;
mod runtime;

pub use projector::{ProjectionError, StateProjector};
pub use runtime::{ProjectionRuntime, ProjectionRuntimeError};
