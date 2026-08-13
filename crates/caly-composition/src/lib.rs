//! Daemon composition root: build-time wiring of `caly-application` owners
//! against concrete `caly-backends` adapters. Its duty ends before any
//! transport listener binds; `bins/caly` is the sole host (§6 决议,server
//! 经 `ApplicationServicePort` 泛型消费,从不经本 crate)。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny

use std::path::PathBuf;

use caly_domain::DaemonInstanceId;

use caly_application::{
    actor_result::ActorResultClient,
    command_bus::{CommandReceiver, command_bus},
    operations::{AdmissionController, OperationStore},
    projection::ProjectionRuntime,
    runtime::{FatalFault, RuntimeGuard, TokioTaskGroup},
    service::{CommandSupportPolicy, RuntimeService},
};

/// Optional executable overrides for managed proxy cores.
///
/// Resolution precedence is: explicit daemon CLI flag, configuration file,
/// environment variable, then the packaged `vendor/bin` fallback.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CoreBinaryPaths {
    pub mihomo: Option<PathBuf>,
    pub sing_box: Option<PathBuf>,
}

mod tuning;

pub use tuning::RuntimeTuning;

// WallClock 定义随 `ApplicationServicePort for Arc<Mutex<RuntimeService<..>>>`
// facade 留在 caly-application(orphan 规则:该 impl 只能落在 trait 属主
// crate);此处 re-export 保持组装根对外形状与抽离前一致(R3)。
pub use caly_application::service::WallClock;

/// Shared, mutex-protected platform owners. The Platform actor remains the
/// normal mutation owner; shutdown borrows the same owner only after ingress is
/// closed so graceful recovery cannot be bypassed or duplicated.
#[derive(Clone)]
pub(crate) struct SharedPlatformBackend(
    std::sync::Arc<std::sync::Mutex<caly_backends::platform::PlatformBackend>>,
);

impl SharedPlatformBackend {
    pub(crate) fn new(value: caly_backends::platform::PlatformBackend) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(value)))
    }

    /// Fidelity restore for graceful shutdown: returns the desktop to the
    /// state captured before engagement and clears the durable record only on
    /// success. No record means this daemon never engaged the proxy (no-op).
    pub(crate) fn restore_original_and_clear(
        &self,
    ) -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        let mut backend = self
            .0
            .lock()
            .map_err(|_| platform_lock_failure("platform backend lock poisoned"))?;
        backend.restore_original_and_clear()
    }
}

fn platform_lock_failure(message: &'static str) -> caly_ports::ActorFailure {
    // `message` is always a `&'static str` literal, but the infallible
    // `clamped` constructor keeps the call site abort-free: the previous
    // `unwrap_or_else(|_| std::process::abort())` form would have killed
    // the daemon on the (theoretically unreachable) too-long path.
    caly_ports::ActorFailure::clamped(
        caly_ports::ActorFailureKind::Infrastructure,
        message,
        "restart caly",
    )
}

impl caly_ports::PlatformCommandBackend for SharedPlatformBackend {
    fn set_system_proxy(
        &mut self,
        enabled: bool,
    ) -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        let mut backend = self
            .0
            .lock()
            .map_err(|_| platform_lock_failure("platform backend lock poisoned"))?;
        caly_ports::PlatformCommandBackend::set_system_proxy(&mut *backend, enabled)
    }

    fn set_system_proxy_pac(
        &mut self,
        url: &str,
    ) -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        let mut backend = self
            .0
            .lock()
            .map_err(|_| platform_lock_failure("platform backend lock poisoned"))?;
        caly_ports::PlatformCommandBackend::set_system_proxy_pac(&mut *backend, url)
    }
}

#[derive(Clone)]
pub(crate) struct SharedTunBackend(
    std::sync::Arc<
        std::sync::Mutex<
            caly_backends::platform::DurableTunBackend<caly_backends::LinuxTunCommandBackend>,
        >,
    >,
);

impl SharedTunBackend {
    pub(crate) fn new(
        value: caly_backends::platform::DurableTunBackend<caly_backends::LinuxTunCommandBackend>,
    ) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(value)))
    }

    /// Fidelity restore for graceful shutdown: releases the TUN device and
    /// clears the durable record only on success. No record means this daemon
    /// never engaged TUN (no-op).
    pub(crate) fn restore_and_clear(
        &self,
    ) -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        let mut backend = self
            .0
            .lock()
            .map_err(|_| platform_lock_failure("TUN backend lock poisoned"))?;
        backend.restore_and_clear()
    }
}

impl caly_ports::TunCommandBackend for SharedTunBackend {
    fn set_tun(
        &mut self,
        enabled: bool,
    ) -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        let mut backend = self
            .0
            .lock()
            .map_err(|_| platform_lock_failure("TUN backend lock poisoned"))?;
        caly_ports::TunCommandBackend::set_tun(&mut *backend, enabled)
    }
}

/// Runtime capacities kept in one composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCapacities {
    pub command_queue: usize,
    pub operation_total: usize,
    pub operation_terminal: usize,
    pub projection_channel: usize,
    pub replay: usize,
    pub actor_mailbox: usize,
}

impl Default for RuntimeCapacities {
    fn default() -> Self {
        Self {
            command_queue: 64,
            operation_total: 256,
            operation_terminal: 128,
            projection_channel: 64,
            replay: 256,
            actor_mailbox: 32,
        }
    }
}

/// Composition failure before daemon transport binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionError {
    InvalidCapacity,
    InvalidSnapshot,
    TaskCapacity,
    TaskFailure,
    MailboxCapacity,
    BackendUnavailable,
    UnsupportedCore,
    SecretUnavailable,
    Topology,
}

impl std::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            CompositionError::InvalidCapacity => "invalid capacity configuration",
            CompositionError::InvalidSnapshot => "invalid projection snapshot",
            CompositionError::TaskCapacity => "task capacity exceeded",
            CompositionError::TaskFailure => "task failure",
            CompositionError::MailboxCapacity => "actor mailbox capacity exceeded",
            CompositionError::BackendUnavailable => "backend unavailable",
            CompositionError::UnsupportedCore => "unsupported core",
            CompositionError::SecretUnavailable => "session secret unavailable",
            CompositionError::Topology => "topology error",
        };
        f.write_str(message)
    }
}

/// Application owners assembled before Transport is bound.
pub struct ApplicationComposition {
    pub service: std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    pub command_receiver: CommandReceiver,
    pub daemon_instance: DaemonInstanceId,
    pub configured_core: caly_domain::CoreKind,
    tun: Option<caly_domain::TunConfig>,
    controllers: caly_domain::Controllers,
    binaries: CoreBinaryPaths,
    subscription_urls: Vec<String>,
    telemetry_interval: std::time::Duration,
    tuning: RuntimeTuning,
    capacities: RuntimeCapacities,
}

/// Running bounded application tasks and their awaited shutdown owner.
pub struct RunningApplication {
    tasks: TokioTaskGroup,
    runtime_guard: std::sync::Arc<std::sync::Mutex<RuntimeGuard>>,
    template: caly_backends::TemplateRenderBackend,
    service: std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    lifecycle: caly_backends::dual::DualCoreLifecycle,
    platform: SharedPlatformBackend,
    tun: SharedTunBackend,
    _result_client: ActorResultClient,
    telemetry_ingress:
        caly_application::runtime::ActorIngress<caly_application::actors::TelemetryActorCommand>,
}

impl ApplicationComposition {
    /// Builds bounded command admission and projection owners.
    ///
    /// `core_override` is the core selected by layered configuration; it is
    /// used only when `CALY_CORE` is not set, so an explicit per-process env
    /// always wins over a checked-in config file.
    pub fn new(
        daemon_instance: DaemonInstanceId,
        capacities: RuntimeCapacities,
        core_override: Option<caly_domain::CoreKind>,
        tun: Option<caly_domain::TunConfig>,
        controllers: caly_domain::Controllers,
        telemetry_interval_ms: u64,
    ) -> Result<Self, CompositionError> {
        Self::new_with_binaries(
            daemon_instance,
            capacities,
            core_override,
            tun,
            controllers,
            CoreBinaryPaths::default(),
            Vec::new(),
            telemetry_interval_ms,
            RuntimeTuning::default(),
        )
    }

    /// Builds the application with explicit managed-core executable overrides
    /// and config-driven runtime tuning.
    pub fn new_with_binaries(
        daemon_instance: DaemonInstanceId,
        capacities: RuntimeCapacities,
        core_override: Option<caly_domain::CoreKind>,
        tun: Option<caly_domain::TunConfig>,
        controllers: caly_domain::Controllers,
        binaries: CoreBinaryPaths,
        subscription_urls: Vec<String>,
        telemetry_interval_ms: u64,
        tuning: RuntimeTuning,
    ) -> Result<Self, CompositionError> {
        if capacities.command_queue == 0
            || capacities.operation_total == 0
            || capacities.operation_terminal == 0
            || capacities.projection_channel == 0
            || capacities.replay == 0
            || capacities.actor_mailbox == 0
        {
            return Err(CompositionError::InvalidCapacity);
        }
        // Boot invariant: the synchronous wait-dependency DAG must stay acyclic.
        // Validating here turns a future actor-wiring regression into a startup
        // failure instead of a runtime deadlock.
        let topology = caly_application::supervision::default_topology()
            .map_err(|_| CompositionError::Topology)?;
        caly_application::supervision::validate_topology(&topology)
            .map_err(|_| CompositionError::Topology)?;
        let configured_core = configured_core(core_override)?;
        let snapshot =
            initial_snapshot(daemon_instance).map_err(|()| CompositionError::InvalidSnapshot)?;
        let projection =
            ProjectionRuntime::new(snapshot, capacities.projection_channel, capacities.replay)
                .map_err(|_| CompositionError::InvalidSnapshot)?;
        let (ingress, command_receiver) =
            command_bus(capacities.command_queue).map_err(|_| CompositionError::InvalidCapacity)?;
        let store = OperationStore::new(capacities.operation_total, capacities.operation_terminal)
            .map_err(|_| CompositionError::InvalidCapacity)?;
        let admission = AdmissionController::new(store, ingress, WallClock);
        Ok(Self {
            service: std::sync::Arc::new(std::sync::Mutex::new(RuntimeService::new_with_policy(
                admission,
                projection,
                CommandSupportPolicy::lifecycle_and_platform(configured_core),
            ))),
            command_receiver,
            daemon_instance,
            configured_core,
            tun,
            controllers,
            binaries,
            subscription_urls,
            telemetry_interval: std::time::Duration::from_millis(telemetry_interval_ms),
            tuning,
            capacities,
        })
    }
}

mod backends;
mod bootstrap;
mod handlers;
mod recovery;
mod rule_provider_materialize;
mod selection_reconciler;
mod shutdown;
mod support;

use support::{
    bounded_task_reason, configured_core, initial_snapshot, publish_boot_capabilities,
    publish_controller_secret, task_name, task_name_or_abort,
};

#[cfg(test)]
use support::parse_configured_core;

impl RunningApplication {
    pub fn service_handle(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>> {
        self.service.clone()
    }

    /// Subscribes the daemon transport to the first fatal runtime fault.
    pub fn fatal_receiver(
        &self,
    ) -> Result<tokio::sync::watch::Receiver<Option<FatalFault>>, CompositionError> {
        self.runtime_guard
            .lock()
            .map(|guard| guard.fatal_receiver())
            .map_err(|_| CompositionError::TaskFailure)
    }

    /// Returns the retained first-cause category for shutdown diagnostics.
    pub fn first_fault(&self) -> Option<FatalFault> {
        self.runtime_guard
            .lock()
            .map_or(Some(FatalFault::TaskJoin), |guard| guard.first_fault())
    }

    /// Borrows the worker-backed template render backend.
    pub fn template_backend(&self) -> &caly_backends::TemplateRenderBackend {
        &self.template
    }

    pub fn stop_core(&self) -> Result<(), CompositionError> {
        let mut lifecycle = self.lifecycle.clone();
        caly_application::actors::CoreLifecycleCommandBackend::stop(&mut lifecycle)
            .map(|_| ())
            .map_err(|error| {
                tracing::error!(?error, "core lifecycle stop failed");
                CompositionError::BackendUnavailable
            })
    }

    /// Runs the ordered shutdown sequence, then awaits every owned runtime task
    /// and preserves task-body/join failures instead of a false clean shutdown.
    pub async fn shutdown(self) -> Result<(), CompositionError> {
        let actions = shutdown::DaemonShutdownActions {
            telemetry: self.telemetry_ingress,
            lifecycle: self.lifecycle.clone(),
            platform: self.platform.clone(),
            tun: self.tun.clone(),
        };
        // Run the ordered phase sequence; failures are retained, not swallowed.
        let phase_failures = actions
            .run()
            .await
            .map_err(|_| CompositionError::TaskFailure)?;
        let task_failures = self
            .tasks
            .shutdown()
            .await
            .map_err(|_| CompositionError::TaskCapacity)?;
        if phase_failures.is_empty() && task_failures.is_empty() {
            Ok(())
        } else {
            Err(CompositionError::TaskFailure)
        }
    }
}
#[cfg(test)]
mod composition_tests;
