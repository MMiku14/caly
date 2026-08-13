use std::sync::{Arc, Mutex};

use super::*;
use caly_application::{
    actor_result::{ActorReport, ResultDeltas, actor_result_mailbox},
    command_bus::command_bus,
    operations::{AdmissionController, OperationStore},
    projection::ProjectionRuntime,
    runtime::{RuntimeGuard, actor_mailbox},
    service::RuntimeService,
};
use caly_domain::{
    AppliedState, BoundedVec, CapabilitySet, DaemonInstanceId, DesiredState, EventCursor,
    EventSequence, ObservedState, OperationId, PlatformEffectView, PresentationSnapshot, ProxyMode,
    SnapshotRevision,
};

fn snapshot() -> Result<PresentationSnapshot, String> {
    let daemon = DaemonInstanceId::from_bytes([4; 16]);
    let capabilities = CapabilitySet::new(BoundedVec::new()).map_err(|error| error.to_string())?;
    Ok(PresentationSnapshot::new(
        daemon,
        SnapshotRevision::new(0),
        EventCursor::new(daemon, EventSequence::ZERO),
        DesiredState::new(ProxyMode::Rule, None, None, false, false),
        AppliedState::stopped(),
        ObservedState::default(),
        PlatformEffectView::none(),
        capabilities,
        BoundedVec::new(),
    ))
}

fn service_and_commands() -> Result<(SharedRuntimeService, CommandReceiver), String> {
    let (commands, receiver) = command_bus(2).map_err(|error| error.to_string())?;
    let store = OperationStore::new(4, 2).map_err(|error| error.to_string())?;
    let admission = AdmissionController::new(store, commands, WallClock);
    let projection = ProjectionRuntime::new(snapshot()?, 2, 2)
        .map_err(|error| format!("projection setup failed: {error:?}"))?;
    Ok((
        Arc::new(Mutex::new(RuntimeService::new(admission, projection))),
        receiver,
    ))
}

fn closed_fanout() -> Result<ActorCommandFanout, String> {
    let (core_lifecycle, _) = actor_mailbox(1).map_err(|error| error.to_string())?;
    let (config, _) = actor_mailbox(1).map_err(|error| error.to_string())?;
    let (core, _) = actor_mailbox(1).map_err(|error| error.to_string())?;
    let (subscription, _) = actor_mailbox(1).map_err(|error| error.to_string())?;
    let (platform, _) = actor_mailbox(1).map_err(|error| error.to_string())?;
    Ok(ActorCommandFanout {
        core_lifecycle,
        config,
        core,
        subscription,
        platform,
    })
}

fn invalid_result_receiver() -> Result<ActorResultReceiver, String> {
    let (results, receiver) = actor_result_mailbox(1).map_err(|error| error.to_string())?;
    results
        .try_send(ActorReport::Completed {
            operation_id: OperationId::from_bytes([9; 16]),
            deltas: ResultDeltas::new(),
        })
        .map_err(|_| "cannot inject actor result".to_owned())?;
    Ok(receiver)
}

#[tokio::test]
async fn actor_result_admission_rejection_is_skipped_not_fatal() -> Result<(), String> {
    // W3a BUG-2: the injected report targets an unknown operation id, so
    // applying it yields an Admission rejection — a business-level
    // condition. The dispatcher must skip it and keep serving; the daemon
    // must NOT escalate to a runtime fatal (the old behaviour killed the
    // daemon on any actor-result error, which is the suspected root of
    // the intermittent silent daemon death during `sub refresh`).
    let (service, command_receiver) = service_and_commands()?;
    let fanout = closed_fanout()?;
    let result_receiver = invalid_result_receiver()?;
    let guard = Arc::new(Mutex::new(RuntimeGuard::new()));
    let cancellation = guard
        .lock()
        .map_err(|_| "runtime guard poisoned".to_owned())?
        .cancellation();
    let outcome = dispatch_loop(
        service,
        command_receiver,
        fanout,
        result_receiver,
        cancellation.clone(),
        Arc::clone(&guard),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "dispatcher must survive an admission rejection"
    );
    assert!(
        !cancellation.is_cancelled(),
        "no fatal on a business-level rejection"
    );
    assert_eq!(
        guard.lock().map_or(None, |guard| guard.first_fault()),
        None,
        "no fatal fault may be recorded"
    );
    Ok(())
}
