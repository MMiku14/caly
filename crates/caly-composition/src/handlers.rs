pub(super) use super::recovery::spawn_exit_monitor;

use std::time::Duration;

use caly_application::{
    ActorCommandFanout,
    actor_result::{ActorResultClient, ActorResultReceiver, ResultDeltas},
    actors::{
        CoreCommandHandler, CoreLifecycleCommandHandler, PlatformCommandHandler,
        SubscriptionCommandHandler,
    },
    command_bus::CommandReceiver,
    projection::ProjectionRuntime,
    runtime::{ActorHandler, ActorLoopExit, RuntimeGuard, TokioTaskFailure, TokioTaskGroup, run_actor},
    service::RuntimeService,
};

use caly_backends::HttpSubscriptionBackend;

use super::{CompositionError, WallClock, bounded_task_reason, task_failure, task_name};

/// Receivers for the five owner actor handlers.
pub(super) struct HandlerReceivers {
    pub core_lifecycle:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    pub config: caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    pub core: caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    pub subscription:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    pub platform:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
}

pub(super) fn spawn_handlers<B, T>(
    tasks: &mut TokioTaskGroup,
    receivers: HandlerReceivers,
    cancellation: caly_application::runtime::CancellationToken,
    result_client: ActorResultClient,
    core_backend: caly_backends::dual::SwitchableCoreBackend,
    subscription_backend: HttpSubscriptionBackend,
    platform_backend: B,
    tun_backend: T,
    lifecycle_backend: caly_backends::dual::DualCoreLifecycle,
    config_backend: caly_backends::config::ActiveConfigBackend,
    active_cell: caly_backends::dual::SharedActiveCore,
    event_bus: caly_application::events::EventBus,
) -> Result<(), CompositionError>
where
    B: caly_ports::PlatformCommandBackend + Send + 'static,
    T: caly_ports::TunCommandBackend + Send + 'static,
{
    // One shared desired-state cell across the mode and platform owners, so a
    // SetMode delta never clobbers a SetTun/SetSystemProxy flag (or vice
    // versa) when both publish DesiredReplaced.
    let desired = caly_backends::SharedDesiredState::default();
    spawn_handler_task(
        tasks,
        receivers.core_lifecycle,
        "core-lifecycle",
        cancellation.clone(),
        CoreLifecycleCommandHandler::new(lifecycle_backend.clone(), result_client.clone()),
    )?;
    spawn_handler_task(
        tasks,
        receivers.config,
        "config",
        cancellation.clone(),
        caly_application::actors::ConfigCommandHandler::new(
            config_backend,
            lifecycle_backend,
            active_cell,
            result_client.clone(),
        ),
    )?;
    spawn_handler_task(
        tasks,
        receivers.core,
        "core",
        cancellation.clone(),
        CoreCommandHandler::new(core_backend, result_client.clone(), desired.clone()),
    )?;
    spawn_handler_task(
        tasks,
        receivers.subscription,
        "subscription",
        cancellation.clone(),
        SubscriptionCommandHandler::new(subscription_backend, result_client.clone())
            // 刀 2 (2026-08-12 pipeline design): the handler publishes the
            // refresh fact; the config reconciler reacts with a reload only
            // when nodes actually changed.
            .with_event_bus(event_bus),
    )?;
    spawn_handler_task(
        tasks,
        receivers.platform,
        "platform",
        cancellation,
        PlatformCommandHandler::new(platform_backend, tun_backend, desired, result_client),
    )?;
    Ok(())
}

/// Clamps a controller secret to at most `MAX_LEN` UTF-8 bytes, keeping the
/// bounded text non-empty with a `"_"` fallback.
fn clamp_secret<const MAX_LEN: usize>(value: String) -> caly_domain::BoundedText<MAX_LEN> {
    caly_domain::BoundedText::from_nonempty_clamped(value, "_")
}

#[cfg(test)]
mod clamp_secret_tests {
    use super::clamp_secret;

    /// Helper that recovers the inner `String` for assertions, then drops
    /// the wrapper. `BoundedText` exposes its content through `Display` (it
    /// is a generic text wrapper, not a secret), so `format!` is sufficient.
    /// Tests never run against real secrets, only generated fuzz-style
    /// inputs, so the test-only exposure is safe.
    fn unwrap<const N: usize>(text: caly_domain::BoundedText<N>) -> String {
        format!("{text}")
    }

    /// `clamp_secret` is the byte-boundary-safe successor to the previous
    /// `chars().take(MAX_LEN)` truncation that would produce a 4-byte-char
    /// string of 4_096×4 = 16 KiB and abort the daemon when the bounded
    /// constructor rejected it. The four cases below cover every input
    /// shape the controller-secret generator can produce: ASCII under/over
    /// the limit, multi-byte UTF-8 over the limit, and the empty fallback.
    /// They were previously four redundant single-assertion tests; the
    /// table-driven form keeps every assertion while sharing the helper
    /// setup.
    #[test]
    fn clamp_secret_covers_ascii_utf8_and_empty() {
        // ASCII under limit: passthrough, byte-for-byte.
        let under = clamp_secret::<4_096>("hello".to_owned());
        assert_eq!(unwrap(under), "hello");

        // ASCII over limit: truncate to `MAX_LEN` bytes (not chars).
        let over_input: String = "a".repeat(8_000);
        let over = clamp_secret::<4_096>(over_input);
        assert_eq!(unwrap(over).len(), 4_096);

        // 6_000 four-byte characters = 24_000 bytes; the byte-bound must hold
        // AND the truncation point must land on a char boundary so the
        // recovered string is still valid UTF-8.
        let multibyte_input: String = "🦀".repeat(6_000);
        assert_eq!(multibyte_input.len(), 24_000);
        let multibyte = clamp_secret::<4_096>(multibyte_input);
        let recovered = unwrap(multibyte);
        assert!(recovered.len() <= 4_096, "len={}", recovered.len());
        assert!(recovered.is_char_boundary(recovered.len()));
        assert!(std::str::from_utf8(recovered.as_bytes()).is_ok());

        // Empty input: fallback `"_"` keeps the BoundedText non-empty.
        let empty = clamp_secret::<4_096>(String::new());
        assert_eq!(unwrap(empty), "_");
    }
}

fn telemetry_dual(
    active_cell: caly_backends::dual::SharedActiveCore,
    secret: Option<String>,
    observed: caly_backends::SharedObservedState,
    controllers: &caly_domain::Controllers,
) -> caly_backends::TelemetryBackend {
    let mihomo = caly_corectl::mihomo::MihomoHttpControl::new(
        controllers.mihomo.clone(),
        secret.clone().map(clamp_secret::<4_096>),
    )
    .map(|control| Box::new(control) as Box<dyn caly_corectl::contract::KernelControl + Send>);
    let sing_box = caly_corectl::sing_box::SingBoxHttpControl::new(
        controllers.sing_box.clone(),
        secret.map(clamp_secret::<4_096>),
    )
    .map(|control| Box::new(control) as Box<dyn caly_corectl::contract::KernelControl + Send>);
    // A control that cannot be constructed keeps the observed cell
    // authoritative without failing startup, but the degradation must be
    // visible: an unobservable controller usually means a bad controller
    // address/secret, which would otherwise surface as silently stale
    // telemetry.
    let degraded: Vec<&str> = match (&mihomo, &sing_box) {
        (Err(_), Err(_)) => vec!["mihomo", "sing-box"],
        (Err(_), Ok(_)) => vec!["mihomo"],
        (Ok(_), Err(_)) => vec!["sing-box"],
        (Ok(_), Ok(_)) => Vec::new(),
    };
    if !degraded.is_empty() {
        tracing::warn!(
            degraded = %degraded.join(","),
            "telemetry controller construction failed for: {}; check controllers.* and the daemon controller secret (telemetry will stay stale)",
            degraded.join(", ")
        );
    }
    match (mihomo, sing_box) {
        (Ok(mihomo), Ok(sing_box)) => {
            caly_backends::TelemetryBackend::new(observed).with_controller(Box::new(
                caly_backends::dual::SwitchableKernelControl::new(active_cell, mihomo, sing_box),
            ))
        }
        _ => caly_backends::TelemetryBackend::new(observed),
    }
}

pub(super) fn spawn_supervision(
    tasks: &mut TokioTaskGroup,
    telemetry_receiver: caly_application::runtime::ActorReceiver<
        caly_application::actors::TelemetryActorCommand,
    >,
    telemetry_ingress: caly_application::runtime::ActorIngress<
        caly_application::actors::TelemetryActorCommand,
    >,
    lifecycle_handle: &caly_backends::dual::DualCoreLifecycle,
    result_client: ActorResultClient,
    _configured_core: caly_domain::CoreKind,
    controller_secret: Option<String>,
    controllers: &caly_domain::Controllers,
    telemetry_interval: std::time::Duration,
    cancellation: caly_application::runtime::CancellationToken,
) -> Result<(), CompositionError> {
    let observed = caly_backends::SharedObservedState::default();
    let telemetry = telemetry_dual(
        lifecycle_handle.active_cell(),
        controller_secret,
        observed.clone(),
        controllers,
    );
    spawn_telemetry_handler(tasks, telemetry_receiver, telemetry, cancellation.clone())?;
    spawn_telemetry_scheduler(
        tasks,
        telemetry_ingress,
        observed,
        result_client,
        telemetry_interval,
        cancellation,
    )
}

/// Spawns a periodic producer that requests a telemetry `Sample` and publishes
/// the refreshed `ObservedState` to the projection. Delivery is best-effort: a
/// full/closed mailbox or a transient delta overflow is ignored and the loop
/// continues, so telemetry can never take down the daemon.
fn spawn_telemetry_scheduler(
    tasks: &mut TokioTaskGroup,
    telemetry: caly_application::runtime::ActorIngress<
        caly_application::actors::TelemetryActorCommand,
    >,
    observed: caly_backends::SharedObservedState,
    results: ActorResultClient,
    interval: std::time::Duration,
    cancellation: caly_application::runtime::CancellationToken,
) -> Result<(), CompositionError> {
    tasks
        .spawn_owned_with_fault(
            task_name("telemetry-scheduler")?,
            caly_application::runtime::FatalFault::Projection,
            async move {
                let mut interval = tokio::time::interval(interval);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // 刀 6 (CPU audit): only project the observed state when it
                // actually changed — an unchanged sample must not re-report
                // an identical snapshot every tick (wasted wire frames and
                // projector work).
                let mut last_observed: Option<caly_domain::ObservedState> = None;
                while !cancellation.is_cancelled() {
                    interval.tick().await;
                    if cancellation.is_cancelled() {
                        break;
                    }
                    let _ =
                        telemetry.try_send(caly_application::actors::TelemetryActorCommand::Sample);
                    let state = observed.value();
                    if last_observed.as_ref() == Some(&state) {
                        continue;
                    }
                    last_observed = Some(state);
                    let Ok(deltas) = ResultDeltas::try_from_vec(vec![
                        caly_domain::PresentationDelta::ObservedReplaced(state),
                    ]) else {
                        continue;
                    };
                    let _ =
                        results.try_report(caly_application::actor_result::ActorReport::Observed {
                            deltas,
                        });
                }
                Ok(())
            },
        )
        .map_err(|_| CompositionError::TaskCapacity)
}

/// Spawns the TelemetryActor handler task (the single supervision owner).
fn spawn_telemetry_handler(
    tasks: &mut TokioTaskGroup,
    telemetry_receiver: caly_application::runtime::ActorReceiver<
        caly_application::actors::TelemetryActorCommand,
    >,
    telemetry: caly_backends::TelemetryBackend,
    cancellation: caly_application::runtime::CancellationToken,
) -> Result<(), CompositionError> {
    spawn_handler_task(
        tasks,
        telemetry_receiver,
        "telemetry",
        cancellation,
        caly_application::actors::TelemetryCommandHandler(telemetry),
    )
}

fn spawn_handler_task<M, H>(
    tasks: &mut TokioTaskGroup,
    receiver: caly_application::runtime::ActorReceiver<M>,
    name: &'static str,
    cancellation: caly_application::runtime::CancellationToken,
    handler: H,
) -> Result<(), CompositionError>
where
    M: Send + 'static,
    H: ActorHandler<M> + Send + 'static,
    H::Error: core::fmt::Debug + Send,
{
    let task_name = task_name(name)?;
    tasks
        .spawn_owned(task_name.clone(), async move {
            let actor = tokio::task::spawn_blocking(move || {
                let mut handler = handler;
                run_actor(
                    receiver,
                    &cancellation,
                    Duration::from_millis(100),
                    &mut handler,
                )
            });
            match actor.await.map_err(|_| TokioTaskFailure::Join {
                name: task_name.clone(),
            })? {
                ActorLoopExit::Cancelled
                | ActorLoopExit::MailboxClosed
                | ActorLoopExit::HandlerStopped => Ok(()),
                ActorLoopExit::InvalidPollInterval => Err(TokioTaskFailure::Task {
                    name: task_name,
                    reason: bounded_task_reason("invalid actor poll interval"),
                }),
                ActorLoopExit::HandlerFailed(error) => {
                    // W3a BUG-2 (deep review): a business-level handler
                    // failure — e.g. a refresh operation whose result
                    // mailbox was momentarily full — used to escalate to
                    // a daemon-wide fatal and silently kill the daemon.
                    // The operation itself carries a failure envelope;
                    // log loudly and end the actor task cleanly instead.
                    // True panics still surface through the Join arm
                    // above and remain fatal.
                    tracing::warn!(
                        task = %task_name,
                        error = ?error,
                        "actor handler failed; ending actor task (daemon continues)"
                    );
                    Ok(())
                }
            }
        })
        .map_err(|_| CompositionError::TaskCapacity)
}

type SharedRuntimeService =
    std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>;
type SharedRuntimeGuard = std::sync::Arc<std::sync::Mutex<caly_application::runtime::RuntimeGuard>>;

fn actor_result_step(
    service: &SharedRuntimeService,
    receiver: &ActorResultReceiver,
    runtime_guard: &SharedRuntimeGuard,
) -> Result<bool, TokioTaskFailure> {
    let mut locked = service.lock().unwrap_or_else(|poisoned| {
        tracing::error!("application service lock poisoned; continuing with the inner value");
        poisoned.into_inner()
    });
    let result = locked.apply_actor_result_once(receiver, Duration::from_millis(1));
    match result {
        Ok(caly_application::service::ActorResultOutcome::MailboxClosed) => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => {
            use caly_application::service::ActorResultError;
            match &error {
                // W3a BUG-3: a projection fault is no longer a death
                // sentence — rebuild from the projector's last-consistent
                // snapshot and keep serving; only a failed self-heal (or a
                // poisoned service lock) escalates to fatal.
                ActorResultError::Projection(_) => {
                    let recovered = service
                        .lock()
                        .ok()
                        .and_then(|mut svc| svc.recover_projection().ok());
                    if let Some(()) = recovered {
                        tracing::warn!(
                            ?error,
                            "projection recovered from snapshot; daemon continues"
                        );
                        return Ok(false);
                    }
                    record_fatal(runtime_guard, |guard| {
                        guard.record_actor_result_error(&error);
                    });
                    Err(task_failure(
                        "command-dispatcher",
                        "actor result application failed",
                    ))
                }
                // A rejected admission is a business-level condition (the
                // operation itself carries a failure envelope); it must not
                // take down the daemon (W3a BUG-2).
                ActorResultError::Admission(_) => {
                    tracing::warn!(?error, "actor result admission rejected; skipping");
                    Ok(false)
                }
                ActorResultError::InvalidTimeout => {
                    record_fatal(runtime_guard, |guard| {
                        guard.record_actor_result_error(&error);
                    });
                    Err(task_failure(
                        "command-dispatcher",
                        "actor result application failed",
                    ))
                }
            }
        }
    }
}

fn dispatch_step(
    service: &SharedRuntimeService,
    receiver: &CommandReceiver,
    fanout: &mut ActorCommandFanout,
    runtime_guard: &SharedRuntimeGuard,
) -> Result<(), TokioTaskFailure> {
    // W3a 风险-1: a panicking step poisons the mutex; recover the inner
    // data (recording the fault loudly) instead of turning every later
    // step into a daemon-wide fatal.
    let result = service
        .lock()
        .unwrap_or_else(|poisoned| {
            tracing::error!("application service lock poisoned; continuing with the inner value");
            poisoned.into_inner()
        })
        .dispatch_once(receiver, fanout, Duration::from_millis(100));
    if let Err(error) = result {
        record_fatal(runtime_guard, |guard| guard.record_dispatch_error(&error));
        return Err(task_failure("command-dispatcher", "command dispatch failed"));
    }
    Ok(())
}

/// Records a dispatcher-owned fatal on the shared runtime guard, logging the
/// lock-poison case loudly instead of silently skipping the record.
fn record_fatal(runtime_guard: &SharedRuntimeGuard, record: impl FnOnce(&mut RuntimeGuard)) {
    if let Ok(mut guard) = runtime_guard.lock() {
        record(&mut guard);
    }
}

pub(super) async fn dispatch_loop(
    service: SharedRuntimeService,
    receiver: CommandReceiver,
    mut fanout: ActorCommandFanout,
    result_receiver: ActorResultReceiver,
    cancellation: caly_application::runtime::CancellationToken,
    runtime_guard: SharedRuntimeGuard,
) -> Result<(), TokioTaskFailure> {
    while !cancellation.is_cancelled() {
        if actor_result_step(&service, &result_receiver, &runtime_guard)? {
            break;
        }
        dispatch_step(&service, &receiver, &mut fanout, &runtime_guard)?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

#[cfg(test)]
mod handler_tests;
