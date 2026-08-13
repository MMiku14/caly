//! Continuous watch stream for `JsonService` connections.
//!
//! Split out of `json/service.rs` (audit #70 file-length budget):
//! yields the initial replay/snapshot, then follows live projection
//! events. A slow subscriber that lags the broadcast ring is sent a
//! fresh full snapshot and re-subscribes, so a slow client only drops
//! best-effort live events and never blocks the authoritative
//! projector.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use caly_application::{events::SequencedEvent, service::ApplicationServicePort};
use caly_protocol::protocol::v2::WatchResponse;
use tokio_stream::{Stream, wrappers::BroadcastStream};

use super::{ServiceError, ServiceState, lock_state};
use super::{ServiceV2, event_to_wire};

/// Continuous watch stream: yields the initial replay/snapshot, then follows
/// live projection events. A slow subscriber that lags the broadcast ring is
/// sent a fresh full snapshot and re-subscribes, so a slow client only drops
/// best-effort live events and never blocks the authoritative projector.
pub struct WatchStream<A> {
    pub(super) initial: std::vec::IntoIter<Result<WatchResponse, ServiceError>>,
    pub(super) live: BroadcastStream<SequencedEvent>,
    pub(super) state: Arc<Mutex<ServiceState<A>>>,
}

impl<A> WatchStream<A> {
    /// Fetches the current authoritative snapshot as a wire watch response.
    fn fetch_snapshot(&self) -> Option<Result<WatchResponse, ServiceError>>
    where
        A: ApplicationServicePort,
    {
        let mut state = lock_state(&self.state).ok()?;
        let wire = ServiceV2::snapshot(&mut state.adapter).ok()?;
        Some(Ok(WatchResponse::FullSnapshot(Box::new(wire))))
    }

    /// Re-subscribes to the live broadcast after a lag recovery.
    fn resubscribe(&self) -> Option<BroadcastStream<SequencedEvent>>
    where
        A: ApplicationServicePort,
    {
        lock_state(&self.state)
            .ok()?
            .adapter
            .subscribe_live()
            .ok()
            .map(BroadcastStream::new)
    }
}

impl<A> Stream for WatchStream<A>
where
    A: ApplicationServicePort + Send,
{
    type Item = Result<WatchResponse, ServiceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(item) = this.initial.next() {
            return Poll::Ready(Some(item));
        }
        loop {
            match Pin::new(&mut this.live).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(_))) => {
                    let snapshot = this.fetch_snapshot();
                    this.live = match this.resubscribe() {
                        Some(stream) => stream,
                        None => return Poll::Ready(None),
                    };
                    if let Some(snapshot) = snapshot {
                        return Poll::Ready(Some(snapshot));
                    }
                }
                Poll::Ready(Some(Ok(event))) => {
                    let Ok(wire) = event_to_wire(&event) else {
                        return Poll::Ready(Some(Err(ServiceError::ApplicationUnavailable)));
                    };
                    return Poll::Ready(Some(Ok(WatchResponse::Event(wire))));
                }
            }
        }
    }
}
