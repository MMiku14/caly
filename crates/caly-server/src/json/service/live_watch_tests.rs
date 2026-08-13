use super::*;
use caly_application::{
    command_bus::CommandEnvelope,
    events::{ApplicationEvent, SequencedEvent},
    service::{ApplicationServiceError, ApplicationWatch, CancellationResult},
};
use caly_domain::{
    AppliedState, BoundedVec, CapabilitySet, DaemonInstanceId, DesiredState, EventCursor,
    EventSequence, ObservedState, OperationId, OperationStatus, PlatformEffectView,
    PresentationSnapshot, ProxyMode, SnapshotRevision,
};
use caly_protocol::protocol::v2::{DecodeLimits, FeatureList, WatchResponse};
use std::task::Waker;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio_stream::{Stream, wrappers::BroadcastStream};

/// Minimal ApplicationServicePort stub: only snapshot/watch/subscribe are real.
struct MockApp {
    tx: tokio::sync::broadcast::Sender<SequencedEvent>,
}

fn mock_snapshot() -> PresentationSnapshot {
    let daemon = DaemonInstanceId::from_bytes([4; 16]);
    let cursor = EventCursor::new(daemon, EventSequence::ZERO);
    let capabilities = CapabilitySet::new(BoundedVec::new()).unwrap();
    PresentationSnapshot::new(
        daemon,
        SnapshotRevision::new(0),
        cursor,
        DesiredState::new(ProxyMode::Rule, None, None, false, false),
        AppliedState::stopped(),
        ObservedState::default(),
        PlatformEffectView::none(),
        capabilities,
        BoundedVec::new(),
    )
}

impl ApplicationServicePort for MockApp {
    fn submit(
        &mut self,
        _envelope: CommandEnvelope,
    ) -> Result<OperationStatus, ApplicationServiceError> {
        Err(ApplicationServiceError::InternalInvariant)
    }
    fn cancel(
        &mut self,
        _operation_id: OperationId,
    ) -> Result<CancellationResult, ApplicationServiceError> {
        Err(ApplicationServiceError::InternalInvariant)
    }
    fn operation_status(
        &self,
        _operation_id: OperationId,
    ) -> Result<OperationStatus, ApplicationServiceError> {
        Err(ApplicationServiceError::InternalInvariant)
    }
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError> {
        Ok(mock_snapshot())
    }
    fn watch_after(
        &self,
        _cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError> {
        Ok(ApplicationWatch::FullSnapshot(Box::new(mock_snapshot())))
    }
    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        self.tx.subscribe()
    }
}

fn poll_stream<A: ApplicationServicePort + Send>(
    stream: &mut Pin<Box<WatchStream<A>>>,
) -> Poll<Option<Result<WatchResponse, ServiceError>>> {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    Pin::new(&mut **stream).poll_next(&mut cx)
}

fn make_stream(
    tx: tokio::sync::broadcast::Sender<SequencedEvent>,
) -> Pin<Box<WatchStream<MockApp>>> {
    let adapter = ServiceAdapter::new(
        MockApp { tx: tx.clone() },
        [0; 16],
        [0; 16],
        crate::admission::TokenAdmission::Open,
        FeatureList::new(),
        DecodeLimits::v2_default(),
    );
    let state = Arc::new(Mutex::new(ServiceState {
        adapter,
        // Round 17: tests don't trigger a stop.
        stop_notifier: None,
        registry: SessionRegistry::new(4).unwrap(),
    }));
    let receiver = tx.subscribe();
    Box::pin(WatchStream {
        initial: Vec::new().into_iter(),
        live: BroadcastStream::new(receiver),
        state,
    })
}

#[test]
fn live_watch_stream_yields_published_events() -> Result<(), String> {
    let (tx, _) = tokio::sync::broadcast::channel(16);
    let mut stream = make_stream(tx.clone());
    let daemon = DaemonInstanceId::from_bytes([4; 16]);
    let event = SequencedEvent {
        cursor: EventCursor::new(daemon, EventSequence::new(1)),
        event: ApplicationEvent::ObservedReplaced(ObservedState::new(5, 6, 1, 0)),
    };
    tx.send(event).map_err(|_| "send failed")?;

    match poll_stream(&mut stream) {
        Poll::Ready(Some(Ok(WatchResponse::Event(_)))) => Ok(()),
        other => Err(format!("unexpected poll result: {other:?}")),
    }
}

#[test]
fn live_watch_stream_stays_pending_without_events() {
    let (tx, _) = tokio::sync::broadcast::channel(16);
    let mut stream = make_stream(tx);
    // No live event published: the stream must report Pending (never None) so a
    // continuous watch stays open.
    let result = poll_stream(&mut stream);
    assert!(
        matches!(result, Poll::Pending),
        "expected Pending on an empty live stream, got {result:?}"
    );
}
