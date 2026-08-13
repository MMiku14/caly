//! Pure views for the five distinct state meanings.

mod applied;
mod delta;
mod desired;
mod observed;
mod platform;
mod presentation;

pub use applied::{AppliedState, AppliedStateError, CoreKind, CoreRunState};
pub use delta::PresentationDelta;
pub use desired::{DesiredState, ProxyMode};
pub use observed::ObservedState;
pub use platform::PlatformEffectView;
pub use presentation::{PresentationSnapshot, ProxyGroupView, SnapshotNodes, SnapshotRevision};
