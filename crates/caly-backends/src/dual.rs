//! Dual-core switching owners: lifecycle, command and telemetry adapters that
//! dispatch to one active core while holding both managed kernels.

use std::{sync::Arc, time::Duration};

use caly_corectl::contract::{
    ConnectionDetail, ConnectionSummary, KernelControl, KernelFailure, ProxyGroup,
};
use caly_domain::{AppliedState, CapabilitySet, CoreKind, ObservedState, ProxyMode};
use caly_ports::{ActorFailure, CoreCommandBackend, CoreLifecycleCommandBackend};

use super::{
    CoreLifecycleBackend, MihomoCoreBackend, SharedCoreLifecycleBackend, SingBoxCoreBackend,
    failure,
};

// P7:cell 定义上移 caly-ports(actors 与组装根跨 crate 共享);此 re-export
// 保持 `caly_backends::dual::SharedActiveCore` 既存签名不动(R3 纯移动)。
pub use caly_ports::SharedActiveCore;

/// Lifecycle owner that holds both managed kernels and dispatches start/stop/
/// restart to the active one. `switch_to` stops the running kernel, flips the
/// active cell, then starts the target.
#[derive(Clone)]
pub struct DualCoreLifecycle {
    active: SharedActiveCore,
    mihomo: SharedCoreLifecycleBackend,
    sing_box: SharedCoreLifecycleBackend,
}

impl DualCoreLifecycle {
    pub fn new(
        active: SharedActiveCore,
        mihomo: SharedCoreLifecycleBackend,
        sing_box: SharedCoreLifecycleBackend,
    ) -> Self {
        Self {
            active,
            mihomo,
            sing_box,
        }
    }

    /// Returns the shared active-cell handle for command/telemetry adapters.
    pub fn active_cell(&self) -> SharedActiveCore {
        Arc::clone(&self.active)
    }

    fn backend_for(&self, kind: CoreKind) -> SharedCoreLifecycleBackend {
        match kind {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.clone(),
            CoreKind::SingBox => self.sing_box.clone(),
        }
    }

    /// Stops the active kernel (if running), flips the active cell to `target`
    /// and starts it. Idempotent when the target is already active.
    pub fn switch_to(&self, target: CoreKind) -> Result<AppliedState, ActorFailure> {
        let mut active = self.active.lock().map_err(|poisoned| {
            failure(
                &format!("active-core lock poisoned ({poisoned})"),
                "restart daemon",
            )
        })?;
        if *active == target {
            return self.backend_for(target).restart();
        }
        // Gracefully stop the previously active kernel before switching so two
        // kernels never race for the same mixed/controller ports.
        let previous = *active;
        self.backend_for(previous).stop()?;
        *active = target;
        drop(active);
        match self.backend_for(target).start() {
            Ok(state) => Ok(state),
            Err(failure) => {
                // Audit #103: the pre-fix path left BOTH kernels stopped with
                // the active cell pointing at the one that would not start.
                // Best-effort: flip the cell back and try to bring the
                // previous core back up so the operator keeps service.
                if let Ok(mut cell) = self.active.lock() {
                    *cell = previous;
                }
                if let Err(restore_error) = self.backend_for(previous).start() {
                    tracing::error!(
                        ?restore_error,
                        "also failed to restore the previous core after a failed switch"
                    );
                }
                Err(failure)
            }
        }
    }
}

impl DualCoreLifecycle {
    /// Polls the active kernel for an abnormal exit (crash monitor).
    pub fn poll_abnormal_exit(&self) -> Result<Option<AppliedState>, String> {
        // Audit #115: a poisoned active-core lock used to fall back to
        // polling the *wrong* kernel (Mihomo) instead of erroring.
        let kind = self
            .active
            .lock()
            .map_err(|poisoned| format!("active-core lock poisoned ({poisoned})"))
            .map(|active| *active)?;
        let mut backend = match kind {
            CoreKind::Mihomo | CoreKind::Xray => self
                .mihomo
                .lock()
                .map_err(|poisoned| format!("Mihomo lifecycle lock poisoned ({poisoned})"))?,
            CoreKind::SingBox => self
                .sing_box
                .lock()
                .map_err(|poisoned| format!("sing-box lifecycle lock poisoned ({poisoned})"))?,
        };
        CoreLifecycleBackend::poll_abnormal_exit(&mut backend).map_err(|error| error.clone())
    }
}

impl CoreLifecycleCommandBackend for DualCoreLifecycle {
    fn switch_to(&mut self, target: CoreKind) -> Result<AppliedState, ActorFailure> {
        DualCoreLifecycle::switch_to(self, target)
    }
    fn start(&mut self) -> Result<AppliedState, ActorFailure> {
        // Audit #115: fail loudly on a poisoned lock instead of dispatching
        // lifecycle calls to the wrong kernel.
        let kind = *self.active.lock().map_err(|poisoned| {
            failure(
                &format!("active-core lock poisoned ({poisoned})"),
                "restart daemon",
            )
        })?;
        self.backend_for(kind).start()
    }
    fn stop(&mut self) -> Result<AppliedState, ActorFailure> {
        let kind = *self.active.lock().map_err(|poisoned| {
            failure(
                &format!("active-core lock poisoned ({poisoned})"),
                "restart daemon",
            )
        })?;
        self.backend_for(kind).stop()
    }
    fn restart(&mut self) -> Result<AppliedState, ActorFailure> {
        let kind = *self.active.lock().map_err(|poisoned| {
            failure(
                &format!("active-core lock poisoned ({poisoned})"),
                "restart daemon",
            )
        })?;
        self.backend_for(kind).restart()
    }

    fn hot_reload(&self, config: &[u8], timeout: Duration) -> Result<(), String> {
        let kind = *self
            .active
            .lock()
            .map_err(|poisoned| format!("active-core lock poisoned ({poisoned})"))?;
        self.backend_for(kind)
            .hot_reload(config, timeout)
            .map_err(|error| error.clone())
    }
}

/// Command backend dispatching node selection/mode/connection control to the
/// active core while sharing one subscription node registry across both.
pub struct SwitchableCoreBackend {
    active: SharedActiveCore,
    mihomo: MihomoCoreBackend,
    sing_box: SingBoxCoreBackend,
}

impl SwitchableCoreBackend {
    pub fn new(
        mihomo: MihomoCoreBackend,
        sing_box: SingBoxCoreBackend,
        active_cell: SharedActiveCore,
    ) -> Self {
        Self {
            active: active_cell,
            mihomo,
            sing_box,
        }
    }

    /// The registry shared by both core adapters (subscription indexing target).
    pub fn registry(&self) -> super::CoreNodeRegistry {
        self.mihomo.registry()
    }

    fn active_kind(&self) -> CoreKind {
        self.active
            .lock()
            .map_or(CoreKind::Mihomo, |active| *active)
    }
}

impl CoreCommandBackend for SwitchableCoreBackend {
    fn select_proxy(&mut self, node: caly_domain::NodeId) -> Result<AppliedState, ActorFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.select_proxy(node),
            CoreKind::SingBox => self.sing_box.select_proxy(node),
        }
    }

    fn select_proxy_group(
        &mut self,
        group: &str,
        member: &str,
    ) -> Result<AppliedState, ActorFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.select_proxy_group(group, member),
            CoreKind::SingBox => self.sing_box.select_proxy_group(group, member),
        }
    }

    fn close_all_connections(&mut self) -> Result<ObservedState, ActorFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.close_all_connections(),
            CoreKind::SingBox => self.sing_box.close_all_connections(),
        }
    }

    fn set_mode(&mut self, mode: ProxyMode) -> Result<AppliedState, ActorFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.set_mode(mode),
            CoreKind::SingBox => self.sing_box.set_mode(mode),
        }
    }
}

/// Kernel control dispatching telemetry sampling to the active core's
/// controller. A switch mid-session keeps sampling truthful without rebuilding
/// the telemetry task.
pub struct SwitchableKernelControl {
    active: SharedActiveCore,
    mihomo: Box<dyn KernelControl + Send>,
    sing_box: Box<dyn KernelControl + Send>,
}

impl SwitchableKernelControl {
    pub fn new(
        active: SharedActiveCore,
        mihomo: Box<dyn KernelControl + Send>,
        sing_box: Box<dyn KernelControl + Send>,
    ) -> Self {
        Self {
            active,
            mihomo,
            sing_box,
        }
    }

    fn active_kind(&self) -> CoreKind {
        self.active
            .lock()
            .map_or(CoreKind::Mihomo, |active| *active)
    }
}

impl KernelControl for SwitchableKernelControl {
    fn capabilities(&self) -> CapabilitySet {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.capabilities(),
            CoreKind::SingBox => self.sing_box.capabilities(),
        }
    }
    fn wait_ready(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.wait_ready(timeout),
            CoreKind::SingBox => self.sing_box.wait_ready(timeout),
        }
    }
    fn select_proxy(
        &mut self,
        node: caly_domain::NodeId,
        timeout: Duration,
    ) -> Result<(), KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.select_proxy(node, timeout),
            CoreKind::SingBox => self.sing_box.select_proxy(node, timeout),
        }
    }
    fn health_check(&mut self, timeout: Duration) -> Result<(), KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.health_check(timeout),
            CoreKind::SingBox => self.sing_box.health_check(timeout),
        }
    }
    fn proxy_groups(&mut self, timeout: Duration) -> Result<Vec<ProxyGroup>, KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.proxy_groups(timeout),
            CoreKind::SingBox => self.sing_box.proxy_groups(timeout),
        }
    }
    fn connections(&mut self, timeout: Duration) -> Result<ConnectionSummary, KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.connections(timeout),
            CoreKind::SingBox => self.sing_box.connections(timeout),
        }
    }
    fn connection_details(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<ConnectionDetail>, KernelFailure> {
        // Must forward explicitly: the trait default errors, and a silent
        // empty list here would make every daemon-side flow query
        // indistinguishable from "no active connections"
        // (2026-08-12 flow agent audit).
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.connection_details(timeout),
            CoreKind::SingBox => self.sing_box.connection_details(timeout),
        }
    }
    fn traffic(&mut self, timeout: Duration) -> Result<(u64, u64), KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.traffic(timeout),
            CoreKind::SingBox => self.sing_box.traffic(timeout),
        }
    }
    fn test_delay(&mut self, name: &str, timeout: Duration) -> Result<Option<u32>, KernelFailure> {
        match self.active_kind() {
            CoreKind::Mihomo | CoreKind::Xray => self.mihomo.test_delay(name, timeout),
            CoreKind::SingBox => self.sing_box.test_delay(name, timeout),
        }
    }
}
