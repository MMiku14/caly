//! Bootstrap assembly: channels, auto-start, dispatcher and runtime startup.

use super::{
    ApplicationComposition, CommandReceiver, CompositionError, RunningApplication,
    RuntimeCapacities, RuntimeService, WallClock, task_name,
};
use caly_application::{
    ActorCommandFanout,
    actor_result::{ActorResultClient, ActorResultReceiver, actor_result_mailbox},
    projection::ProjectionRuntime,
    runtime::{CancellationToken, TokioTaskFailure, TokioTaskGroup, actor_mailbox},
};
use caly_backends::dual::DualCoreLifecycle;

/// Actor channels opened once during runtime bootstrap.
struct ActorChannels {
    fanout: ActorCommandFanout,
    result_client: ActorResultClient,
    result_receiver: ActorResultReceiver,
    core_lifecycle_receiver:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    config_receiver:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    core_receiver:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    subscription_receiver:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    platform_receiver:
        caly_application::runtime::ActorReceiver<caly_application::routing::RoutedCommand>,
    telemetry_receiver:
        caly_application::runtime::ActorReceiver<caly_application::actors::TelemetryActorCommand>,
    telemetry_ingress:
        caly_application::runtime::ActorIngress<caly_application::actors::TelemetryActorCommand>,
}

fn open_channels(capacities: RuntimeCapacities) -> Result<ActorChannels, CompositionError> {
    let (core_lifecycle, core_lifecycle_receiver) =
        actor_mailbox(capacities.actor_mailbox).map_err(|_| CompositionError::MailboxCapacity)?;
    let (config, config_receiver) =
        actor_mailbox(capacities.actor_mailbox).map_err(|_| CompositionError::MailboxCapacity)?;
    let (core, core_receiver) =
        actor_mailbox(capacities.actor_mailbox).map_err(|_| CompositionError::MailboxCapacity)?;
    let (subscription, subscription_receiver) =
        actor_mailbox(capacities.actor_mailbox).map_err(|_| CompositionError::MailboxCapacity)?;
    let (platform, platform_receiver) =
        actor_mailbox(capacities.actor_mailbox).map_err(|_| CompositionError::MailboxCapacity)?;
    let (result_ingress, result_receiver) = actor_result_mailbox(capacities.actor_mailbox)
        .map_err(|_| CompositionError::MailboxCapacity)?;
    let (telemetry_ingress, telemetry_receiver) =
        actor_mailbox(capacities.actor_mailbox).map_err(|_| CompositionError::MailboxCapacity)?;
    Ok(ActorChannels {
        fanout: ActorCommandFanout {
            core_lifecycle,
            config,
            core,
            subscription,
            platform,
        },
        result_client: ActorResultClient::new(result_ingress),
        result_receiver,
        core_lifecycle_receiver,
        config_receiver,
        core_receiver,
        subscription_receiver,
        platform_receiver,
        telemetry_receiver,
        telemetry_ingress,
    })
}

mod boot_gate;

use boot_gate::boot_sequence;

/// Spawns the command dispatcher task and registers it on the task group.
fn spawn_dispatcher(
    tasks: &mut TokioTaskGroup,
    service: std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    command_receiver: CommandReceiver,
    fanout: ActorCommandFanout,
    result_receiver: ActorResultReceiver,
    cancellation: CancellationToken,
    runtime_guard: std::sync::Arc<std::sync::Mutex<caly_application::runtime::RuntimeGuard>>,
) -> Result<(), CompositionError> {
    // Resolve the task name once. The previous implementation called
    // `task_name("command-dispatcher")` twice: once for the outer
    // `spawn_owned_with_fault` registration, then a second time inside the
    // inner `Join` error path with an `unwrap_or_else(|_| abort)` fallback.
    // The second call could not fail in practice (the input is a static
    // literal), but the abort fallback made the call site inconsistent with
    // the call to `task_name(...)` two lines earlier and hid the size
    // invariant behind a process-kill. Resolve once and clone the result.
    let dispatcher_name = task_name("command-dispatcher")?;
    let join_name = dispatcher_name.clone();
    let dispatcher_cancellation = cancellation;
    let dispatcher = tokio::spawn(async move {
        super::handlers::dispatch_loop(
            service,
            command_receiver,
            fanout,
            result_receiver,
            dispatcher_cancellation,
            runtime_guard,
        )
        .await
    });
    tasks
        .spawn_owned_with_fault(
            dispatcher_name,
            caly_application::runtime::FatalFault::CommandDispatch,
            async move {
                dispatcher
                    .await
                    .map_err(|_| TokioTaskFailure::Join { name: join_name })??;
                Ok(())
            },
        )
        .map_err(|_| CompositionError::TaskCapacity)
}

impl ApplicationComposition {
    /// Starts the application runtime.
    ///
    /// `auto_start` engages the configured core (Mihomo/sing-box) during
    /// bootstrap. Hermetic composition tests pass `false`; the daemon passes
    /// `true`.
    pub fn start_runtime(self, auto_start: bool) -> Result<RunningApplication, CompositionError> {
        let channels = open_channels(self.capacities)?;
        let mut runtime_backends = super::backends::build_runtime_backends(
            self.configured_core,
            self.tun,
            &self.controllers,
            &self.binaries,
            self.subscription_urls.as_slice(),
            &self.tuning,
        )?;
        let core_backend = runtime_backends.core;
        let registry = runtime_backends.registry;
        let mut subscription_backend = runtime_backends.subscription;
        // Restore cached subscriptions into the node registry before any
        // command can be served, so list-nodes/delay/select work immediately
        // after a daemon restart without a re-fetch. The restored projection
        // slices also seed the presentation snapshot so `list-nodes` shows
        // the same nodes right away.
        match subscription_backend.restore_cached() {
            Ok(restored) if !restored.is_empty() => {
                let mut merged = Vec::new();
                for nodes in restored {
                    merged.extend(nodes.as_slice().iter().cloned());
                }
                match caly_domain::SnapshotNodes::try_from_vec(merged) {
                    Ok(nodes) => {
                        if let Ok(mut service) = self.service.lock()
                            && let Err(error) = service
                                .projection_mut()
                                .publish(caly_domain::PresentationDelta::NodesReplaced(nodes))
                        {
                            tracing::warn!(error = ?error, "cached nodes did not seed projection");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = ?error, "cached node snapshot exceeded bounds");
                    }
                }
                tracing::info!("restored cached subscriptions");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(error = %error, "cached subscription restore failed; nodes start empty");
            }
        }
        let config_backend = runtime_backends.config;
        let lifecycle_handle = runtime_backends.lifecycle.clone();
        // Restore-first + config effects + auto-start run before any task is
        // spawned; only the core auto-start itself is optional.
        boot_sequence(
            &mut runtime_backends.platform,
            &mut runtime_backends.tun,
            &lifecycle_handle,
            &self.service,
            self.tuning.system_proxy_enabled,
            auto_start,
            self.configured_core,
        )?;
        let platform_backend = super::SharedPlatformBackend::new(runtime_backends.platform);
        let tun_backend = super::SharedTunBackend::new(runtime_backends.tun);
        let controller_secret = runtime_backends.controller_secret.clone();
        // Persist the controller secret so offline CLI queries can authenticate
        // against the running core's controller.
        if let Some(secret) = &controller_secret {
            super::publish_controller_secret(secret);
        }
        let lifecycle_backend = lifecycle_handle.clone();
        let template_backend = runtime_backends.template;
        super::publish_boot_capabilities(
            &self.service,
            self.configured_core,
            self.tuning.dns.is_some(),
        )?;
        let runtime_guard = std::sync::Arc::new(std::sync::Mutex::new(
            caly_application::runtime::RuntimeGuard::new(),
        ));
        let mut tasks = TokioTaskGroup::with_runtime_guard(runtime_guard.clone());
        let cancellation = tasks.cancellation();
        let handles = spawn_runtime_tasks(
            &mut tasks,
            self.service.clone(),
            self.command_receiver,
            channels,
            &lifecycle_backend,
            core_backend,
            subscription_backend,
            platform_backend.clone(),
            tun_backend.clone(),
            config_backend,
            registry,
            &lifecycle_handle,
            self.configured_core,
            controller_secret,
            &self.controllers,
            self.telemetry_interval,
            cancellation,
            runtime_guard.clone(),
            self.tuning.restart_backoffs(),
        )?;
        Ok(RunningApplication {
            tasks,
            runtime_guard,
            template: template_backend,
            service: self.service,
            lifecycle: lifecycle_handle,
            platform: platform_backend,
            tun: tun_backend,
            _result_client: handles.result_client,
            telemetry_ingress: handles.telemetry_ingress,
        })
    }
}

/// Ingress handles kept alive for the runtime lifetime.
struct RuntimeHandles {
    result_client: ActorResultClient,
    telemetry_ingress:
        caly_application::runtime::ActorIngress<caly_application::actors::TelemetryActorCommand>,
}

/// Spawns dispatcher, exit monitor, owner handlers and supervision actors.
fn spawn_runtime_tasks(
    tasks: &mut TokioTaskGroup,
    service: std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    command_receiver: CommandReceiver,
    channels: ActorChannels,
    lifecycle_backend: &DualCoreLifecycle,
    core_backend: caly_backends::dual::SwitchableCoreBackend,
    subscription_backend: caly_backends::HttpSubscriptionBackend,
    platform_backend: super::SharedPlatformBackend,
    tun_backend: super::SharedTunBackend,
    config_backend: caly_backends::config::ActiveConfigBackend,
    registry: caly_backends::CoreNodeRegistry,
    lifecycle_handle: &DualCoreLifecycle,
    configured_core: caly_domain::CoreKind,
    controller_secret: Option<String>,
    controllers: &caly_domain::Controllers,
    telemetry_interval: std::time::Duration,
    cancellation: caly_application::runtime::CancellationToken,
    runtime_guard: std::sync::Arc<std::sync::Mutex<caly_application::runtime::RuntimeGuard>>,
    restart_backoffs: (u64, u64),
) -> Result<RuntimeHandles, CompositionError> {
    spawn_command_tasks(
        tasks,
        service,
        command_receiver,
        channels.fanout,
        channels.result_receiver,
        channels.result_client.clone(),
        super::handlers::HandlerReceivers {
            core_lifecycle: channels.core_lifecycle_receiver,
            config: channels.config_receiver,
            core: channels.core_receiver,
            subscription: channels.subscription_receiver,
            platform: channels.platform_receiver,
        },
        lifecycle_backend,
        core_backend,
        subscription_backend,
        platform_backend,
        tun_backend,
        config_backend,
        registry,
        cancellation.clone(),
        runtime_guard,
        restart_backoffs,
    )?;
    super::handlers::spawn_supervision(
        tasks,
        channels.telemetry_receiver,
        channels.telemetry_ingress.clone(),
        lifecycle_handle,
        channels.result_client.clone(),
        configured_core,
        controller_secret,
        controllers,
        telemetry_interval,
        cancellation,
    )?;
    Ok(RuntimeHandles {
        result_client: channels.result_client,
        telemetry_ingress: channels.telemetry_ingress,
    })
}

/// Spawns the command dispatcher, exit monitor and five owner handlers.
fn spawn_command_tasks(
    tasks: &mut TokioTaskGroup,
    service: std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    command_receiver: CommandReceiver,
    fanout: ActorCommandFanout,
    result_receiver: ActorResultReceiver,
    result_client: ActorResultClient,
    receivers: super::handlers::HandlerReceivers,
    lifecycle_backend: &DualCoreLifecycle,
    core_backend: caly_backends::dual::SwitchableCoreBackend,
    subscription_backend: caly_backends::HttpSubscriptionBackend,
    platform_backend: super::SharedPlatformBackend,
    tun_backend: super::SharedTunBackend,
    config_backend: caly_backends::config::ActiveConfigBackend,
    registry: caly_backends::CoreNodeRegistry,
    cancellation: caly_application::runtime::CancellationToken,
    runtime_guard: std::sync::Arc<std::sync::Mutex<caly_application::runtime::RuntimeGuard>>,
    restart_backoffs: (u64, u64),
) -> Result<(), CompositionError> {
    // 刀 2 (2026-08-12 pipeline design): the shared event bus carries
    // pipeline facts; the config reconciler subscribes and reacts to a
    // changed subscription refresh by re-rendering the kernel config. The
    // config mailbox is cloned before the dispatcher takes ownership of the
    // fanout below.
    let config_mailbox = fanout.config.clone();
    let event_bus = caly_application::events::EventBus::new();
    spawn_dispatcher(
        tasks,
        service,
        command_receiver,
        fanout,
        result_receiver,
        cancellation.clone(),
        runtime_guard,
    )?;
    super::handlers::spawn_exit_monitor(
        tasks,
        lifecycle_backend.clone(),
        result_client.clone(),
        cancellation.clone(),
        restart_backoffs.0,
        restart_backoffs.1,
    )?;
    // Explicit publish→subscribe contract replacing the ad-hoc hook:
    // `SubscriptionRefreshed { changed: true }` → `ReloadConfig`, and the
    // selection reconciler follows node renames by stable id (刀 3).
    caly_application::reconciler::ConfigReconciler::spawn(event_bus.clone(), config_mailbox);
    super::selection_reconciler::spawn(
        event_bus.clone(),
        registry,
        lifecycle_backend.active_cell(),
    );
    super::handlers::spawn_handlers(
        tasks,
        receivers,
        cancellation,
        result_client,
        core_backend,
        subscription_backend,
        platform_backend,
        tun_backend,
        lifecycle_backend.clone(),
        config_backend,
        lifecycle_backend.active_cell(),
        event_bus,
    )
}
