//! JSON request dispatch and lifecycle polling for `JsonService`.
//!
//! Split out of `json/service.rs` (audit #70 file-length budget):
//! the per-request dispatch match plus the terminal-status polling
//! loop (exponential backoff, bounded deadline) both keep the
//! shared adapter lock strictly inside one statement so the
//! `async fn`s stay `Send`.

use std::sync::Arc;

use caly_application::service::ApplicationServicePort;
use caly_protocol::{
    protocol::v2::WatchEventsRequest,
    wire_frames::{JsonRequest, JsonResponse, ServerFrame, WireError, WireErrorCode, wire_error},
};
use tokio::io::AsyncWrite;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use super::super::ServiceV2;
use super::watch::WatchStream;
use super::{lock_state, service_to_wire, write_json_frame};

impl<A> super::JsonService<A> {
    /// Dispatches one non-handshake, non-watch request to the application.
    ///
    /// `async` so the `StopDaemon` path can wait for the operation to reach a
    /// terminal state before triggering the daemon shutdown notifier (the
    /// previous synchronous version fired the notifier as soon as the
    /// `execute` call returned, but `application.submit` is a Pending
    /// reservation — the actor transition to Completed happens on a different
    /// thread, so the UDS transport tore down before the client could
    /// observe the final status).
    pub(super) async fn dispatch(&self, request: JsonRequest) -> Result<JsonResponse, WireError>
    where
        A: ApplicationServicePort,
    {
        match request {
            JsonRequest::Execute(request) => {
                let is_stop = matches!(
                    request.command,
                    caly_protocol::protocol::v2::WireCommand::StopDaemon
                );
                let operation_id = request.operation_id;
                let response = {
                    let mut state = lock_state(&self.state)?;
                    state
                        .adapter
                        .execute(request)
                        .map(JsonResponse::Execute)
                        .map_err(service_to_wire)?
                };
                // Round 17: a successful `StopDaemon` must
                // hold the transport open until the
                // application actor reports the operation
                // as Completed/Failed, so the response
                // carries the terminal status. Firing
                // the notifier earlier would tear down
                // the UDS while the client is still
                // trying to read the body, surfacing
                // `connect_failed` on the next call.
                if is_stop {
                    let terminal = self
                        .wait_for_terminal_status(operation_id)
                        .await
                        .ok()
                        .flatten();
                    if let Some(terminal) = terminal {
                        // Round 17: with the terminal status in
                        // hand, signal the daemon runtime to
                        // break its `serve()` loop. The current
                        // connection task is still mid-write, so
                        // the listener's `notified().await` will
                        // fire on the next poll and the response
                        // is fully serialized before the daemon
                        // process exits.
                        if let Ok(state) = lock_state(&self.state)
                            && let Some(notifier) = &state.stop_notifier
                        {
                            notifier.notify_waiters();
                        }
                        return Ok(JsonResponse::Execute(terminal));
                    }
                }
                Ok(response)
            }
            JsonRequest::CancelOperation(request) => {
                let mut state = lock_state(&self.state)?;
                state
                    .adapter
                    .cancel(request)
                    .map(JsonResponse::CancelOperation)
                    .map_err(service_to_wire)
            }
            JsonRequest::GetOperationStatus(request) => {
                let mut state = lock_state(&self.state)?;
                state
                    .adapter
                    .status(request)
                    .map(JsonResponse::GetOperationStatus)
                    .map_err(service_to_wire)
            }
            JsonRequest::GetSnapshot(_) => {
                let mut state = lock_state(&self.state)?;
                state
                    .adapter
                    .snapshot()
                    .map(JsonResponse::GetSnapshot)
                    .map_err(service_to_wire)
            }
            JsonRequest::Handshake(_) | JsonRequest::WatchEvents(_) => Err(wire_error(
                WireErrorCode::InvalidArgument,
                "method not dispatched here",
            )),
        }
    }

    /// Polls the in-process `application.operation_status` for `id` until the
    /// status is terminal, then re-runs the execute path's wire conversion so
    /// the caller can serialize a Completed/Failed `ExecuteResponse` to the
    /// client. The mutex is acquired and released on each poll iteration so the
    /// dispatcher thread is never blocked across the await points — a tight
    /// sync loop inside the Tokio runtime would starve other connections.
    ///
    /// `Ok(Some(_))` → terminal status reached within the bounded wait.
    /// `Ok(None)`    → the wait timed out (operation still non-terminal).
    /// `Err(_)`      → the application rejected the status query (e.g. the
    ///                  record was evicted, which should not happen for a
    ///                  just-submitted operation).
    async fn wait_for_terminal_status(
        &self,
        id: caly_protocol::protocol::v2::WireId,
    ) -> Result<Option<caly_protocol::protocol::v2::ExecuteResponse>, WireError>
    where
        A: ApplicationServicePort,
    {
        use caly_protocol::protocol::v2::{ExecuteResponse, GetOperationStatusRequest};
        use std::time::{Duration, Instant};
        const POLL_START: Duration = Duration::from_millis(10);
        const POLL_MAX: Duration = Duration::from_millis(250);
        const TIMEOUT: Duration = Duration::from_secs(5);
        let request = GetOperationStatusRequest { operation_id: id };
        let deadline = Instant::now() + TIMEOUT;
        // Exponential backoff: a fixed 10 ms spin produced 500 adapter-lock
        // acquisitions per second while the daemon was already busy with the
        // very operation being awaited.
        let mut poll = POLL_START;
        loop {
            let snapshot = {
                let mut state = lock_state(&self.state)?;
                state
                    .adapter
                    .status(request)
                    .ok()
                    .and_then(|status| status.state.is_terminal().then_some(status))
            };
            if let Some(terminal) = snapshot {
                let response = ExecuteResponse {
                    operation: terminal,
                };
                return Ok(Some(response));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(poll).await;
            poll = (poll * 2).min(POLL_MAX);
        }
    }

    /// Streams the initial replay followed by live projection events until the
    /// peer disconnects or the application broadcast closes.
    pub(super) async fn stream_watch<W>(&self, mut writer: W, request: WatchEventsRequest)
    where
        A: ApplicationServicePort + Send,
        W: AsyncWrite + Unpin,
    {
        // Build the stream inside a single async step so the shared adapter
        // lock is never held across an await (the stream must stay Send).
        let built = async {
            let mut state = lock_state(&self.state)?;
            let initial = state.adapter.watch(request).map_err(service_to_wire)?;
            let live = state.adapter.subscribe_live().map_err(service_to_wire)?;
            Ok::<WatchStream<A>, WireError>(WatchStream {
                initial,
                live: BroadcastStream::new(live),
                state: Arc::clone(&self.state),
            })
        }
        .await;
        let stream = match built {
            Ok(stream) => stream,
            Err(error) => {
                let _ = write_json_frame(&mut writer, &ServerFrame::Error(error)).await;
                return;
            }
        };
        let mut stream = Box::pin(stream);
        while let Some(item) = stream.next().await {
            let frame = match item {
                Ok(wire) => ServerFrame::Result(Box::new(JsonResponse::WatchEvent(wire))),
                Err(error) => ServerFrame::Error(service_to_wire(error)),
            };
            if write_json_frame(&mut writer, &frame).await.is_err() {
                // Peer disconnected; the daemon-side stream is done.
                break;
            }
            if matches!(frame, ServerFrame::Error(_)) {
                break;
            }
        }
    }
}
