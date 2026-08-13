//! Shared lifecycle backend adapters for Mihomo and sing-box.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use caly_corectl::{
    contract::KernelControl,
    contract::RenderedConfigRef,
    mihomo::{
        MihomoHttpControl, MihomoRuntime, MihomoRuntimeError, MihomoRuntimeEvent,
        MihomoSpawnSpecFactory,
    },
};
use caly_domain::BoundedText;
use caly_platform::process::LinuxProcessSpawner;

use super::failure;
use super::sing_box::SingBoxLifecycleBackend;
use caly_ports::{ActorFailure, CoreLifecycleCommandBackend};

/// Concrete Mihomo process/readiness/health owner.
pub struct MihomoLifecycleBackend {
    runtime: MihomoRuntime<LinuxProcessSpawner, MihomoHttpControl>,
    config: RenderedConfigRef,
    generation: u64,
    /// Whether the rendered config enables a TUN inbound; a start failure then
    /// likely needs `CAP_NET_ADMIN` in the core process, surfaced as a hint.
    tun_enabled: bool,
    /// Controller endpoint/secret kept for the post-start node-selection
    /// restore (the runtime owns its own control instance).
    controller: String,
    secret: Option<BoundedText<4_096>>,
}

impl MihomoLifecycleBackend {
    pub fn new(
        binary: PathBuf,
        working_directory: PathBuf,
        controller: String,
        config: PathBuf,
        generation: u64,
        secret: Option<String>,
        tun_enabled: bool,
    ) -> Result<Self, String> {
        let factory = MihomoSpawnSpecFactory::new(binary, working_directory).map_err(|error| {
            format!(
                "Mihomo factory failed: {} ({})",
                error.message, error.suggested_action
            )
        })?;
        let secret = secret.map(|value| {
            BoundedText::new(value).map_err(|_| "Mihomo controller secret is too long".to_owned())
        });
        let secret = match secret {
            Some(Ok(value)) => Some(value),
            Some(Err(error)) => return Err(error),
            None => None,
        };
        let control =
            MihomoHttpControl::new(controller.clone(), secret.clone()).map_err(|error| {
                format!(
                    "Mihomo control failed: {} ({})",
                    error.message, error.suggested_action
                )
            })?;
        Ok(Self {
            runtime: MihomoRuntime::new(factory, LinuxProcessSpawner, control),
            config: RenderedConfigRef {
                generation,
                path: config,
            },
            generation,
            tun_enabled,
            controller,
            secret,
        })
    }

    pub fn start(&mut self, timeout: Duration) -> Result<(), MihomoRuntimeError> {
        self.runtime.start(&self.config, self.generation, timeout)?;
        self.restore_selection();
        Ok(())
    }

    /// Re-applies the persisted node selection once the core is ready.
    /// Best-effort: controller failures only log. The controller secret is
    /// optional — `MihomoHttpControl` works fine without one, so a secretless
    /// deployment must not silently skip the restore (selected nodes would
    /// reset to the group default on every core restart).
    fn restore_selection(&self) {
        let Ok(control) = MihomoHttpControl::new(self.controller.clone(), self.secret.clone())
        else {
            return;
        };
        crate::selection::restore(
            caly_domain::CoreKind::Mihomo,
            CONTROL_RESTORE_TIMEOUT,
            |group, name| {
                control
                    .select_proxy(group, name, CONTROL_RESTORE_TIMEOUT)
                    .map_err(|error| error.message.to_string())
            },
        );
    }
    /// Hot-reloads the kernel config without restarting the process
    /// (刀 5, 2026-08-12 pipeline design): pushes `config` through the
    /// Clash-compatible `PUT /configs?force=true` endpoint so existing
    /// connections stay up. Returns an error when the kernel has no reload
    /// surface (sing-box) or the reload failed — the caller falls back to a
    /// restart.
    pub fn hot_reload(&self, config: &[u8], timeout: Duration) -> Result<(), String> {
        let Ok(mut control) = MihomoHttpControl::new(self.controller.clone(), self.secret.clone())
        else {
            return Err("cannot build the kernel controller for a hot reload".to_owned());
        };
        control
            .reload_config(config, timeout)
            .map_err(|error| error.message.to_string())
    }
    pub fn health(&mut self, timeout: Duration) -> Result<(), caly_domain::BoundedText<1_024>> {
        self.runtime
            .health_check(timeout)
            .map_err(|error| error.message)
    }
    pub fn stop(&mut self, timeout: Duration) -> Result<(), MihomoRuntimeError> {
        self.runtime.stop(timeout).map(|_| ())
    }
    pub fn restart(&mut self, timeout: Duration) -> Result<(), MihomoRuntimeError> {
        self.runtime
            .restart(&self.config, self.generation, timeout)?;
        self.restore_selection();
        Ok(())
    }
    pub fn poll_exit(&mut self) -> Result<Option<MihomoRuntimeEvent>, MihomoRuntimeError> {
        self.runtime.poll_exit()
    }
    pub const fn is_running(&self) -> bool {
        self.runtime.is_running()
    }
    pub fn poll_abnormal_exit(
        &mut self,
    ) -> Result<Option<caly_domain::AppliedState>, MihomoRuntimeError> {
        let Some(_event) = self.poll_exit()? else {
            return Ok(None);
        };
        // A crash is a platform observation and is reported regardless of any
        // pending client operation, so the projection reflects the truth.
        let state = caly_domain::AppliedState::new(
            Some(caly_domain::CoreKind::Mihomo),
            caly_domain::CoreRunState::Crashed,
            None,
            Some(self.generation),
        )
        .map_err(|_| {
            MihomoRuntimeError::Kernel(caly_corectl::contract::KernelFailure {
                kind: caly_corectl::contract::KernelFailureKind::ProcessLifecycle,
                message: text("Mihomo crash state projection failed"),
                suggested_action: action_text("restart Mihomo"),
                platform: None,
            })
        })?;
        Ok(Some(state))
    }
}

/// Core lifecycle backend selected by `CALY_CORE`.
pub enum CoreLifecycleBackend {
    Mihomo(Arc<Mutex<MihomoLifecycleBackend>>),
    SingBox(Arc<Mutex<SingBoxLifecycleBackend>>),
}

/// Shared, cloneable core lifecycle handle with config-driven readiness
/// budgets. A local newtype (not an `Arc` alias) so the port trait can be
/// implemented for it without violating orphan rules.
#[derive(Clone)]
pub struct SharedCoreLifecycleBackend {
    inner: Arc<Mutex<CoreLifecycleBackend>>,
    start_timeout: std::time::Duration,
    stop_timeout: std::time::Duration,
}

impl SharedCoreLifecycleBackend {
    /// Wraps an owned core lifecycle handle with the historical budgets.
    pub fn new(inner: Arc<Mutex<CoreLifecycleBackend>>) -> Self {
        Self::with_timeouts(
            inner,
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(5),
        )
    }

    /// Wraps a handle with explicit start/stop budgets from configuration.
    pub const fn with_timeouts(
        inner: Arc<Mutex<CoreLifecycleBackend>>,
        start_timeout: std::time::Duration,
        stop_timeout: std::time::Duration,
    ) -> Self {
        Self {
            inner,
            start_timeout,
            stop_timeout,
        }
    }

    /// Locks the shared core lifecycle backend.
    pub fn lock(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'_, CoreLifecycleBackend>,
        std::sync::PoisonError<std::sync::MutexGuard<'_, CoreLifecycleBackend>>,
    > {
        self.inner.lock()
    }

    /// Hot-reloads the active core's config without restarting the process
    /// (刀 5); errors fall back to a restart.
    pub fn hot_reload(&self, config: &[u8], timeout: Duration) -> Result<(), String> {
        self.lock()
            .map_err(|_| "core lifecycle lock poisoned".to_owned())?
            .hot_reload(config, timeout)
    }
}

impl CoreLifecycleBackend {
    /// Hot-reloads the active core's config (刀 5); sing-box has no reload
    /// surface, so its arm reports the fallback error explicitly instead of
    /// building a controller that would fail anyway.
    pub fn hot_reload(&mut self, config: &[u8], timeout: Duration) -> Result<(), String> {
        match self {
            Self::Mihomo(value) => value
                .lock()
                .map_err(|_| "Mihomo lifecycle lock poisoned".to_owned())?
                .hot_reload(config, timeout),
            Self::SingBox(value) => value
                .lock()
                .map_err(|_| "sing-box lifecycle lock poisoned".to_owned())?
                .hot_reload(config, timeout),
        }
    }
    pub fn poll_abnormal_exit(&mut self) -> Result<Option<caly_domain::AppliedState>, String> {
        match self {
            Self::Mihomo(value) => value
                .lock()
                .map_err(|_| "Mihomo lifecycle lock poisoned".to_owned())?
                .poll_abnormal_exit()
                .map_err(|error| error.to_string()),
            Self::SingBox(value) => value
                .lock()
                .map_err(|_| "sing-box lifecycle lock poisoned".to_owned())?
                .poll_abnormal_exit()
                .map_err(|error| error.to_string()),
        }
    }
}

impl CoreLifecycleCommandBackend for SharedCoreLifecycleBackend {
    fn start(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.lock()
            .map_err(|_| failure("core lifecycle lock poisoned", "restart daemon"))?
            .start_timed(self.start_timeout)
    }
    fn stop(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.lock()
            .map_err(|_| failure("core lifecycle lock poisoned", "restart daemon"))?
            .stop_timed(self.stop_timeout)
    }
    fn restart(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.lock()
            .map_err(|_| failure("core lifecycle lock poisoned", "restart daemon"))?
            .restart_timed(self.start_timeout)
    }
}

/// Appends targeted remediation hints when TUN is enabled: a route conflict
/// (another route/process owns the TUN subnet, e.g. a second proxy) surfaces a
/// specific ```file exists``` hint; a fresh start surfaces a `CAP_NET_ADMIN`
/// hint. The core child inherits the daemon's (unprivileged) process context
/// even when the daemon's own `ip` commands escalate, so opening the TUN
/// device still needs a capability-granted binary.
fn tun_cap_net_admin_hint(tun_enabled: bool, core: &str, message: &str) -> String {
    if !tun_enabled {
        return String::new();
    }
    // Audit #26: match case-insensitively on a lowercased copy. The messages
    // originate inside the mihomo / sing-box cores (Go programs, whose error
    // strings are not localized), so this is not the LANG-sensitive matching
    // the platform-side `is_busy_failure` had; lowercasing only hardens
    // against capitalization drift between core versions.
    let lowered = message.to_ascii_lowercase();
    let route_conflict = [
        "file exists",
        "already exists",
        "address already in use",
        "route",
        "eexist",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
        && (lowered.contains("add route") || lowered.contains("tun"));
    if route_conflict {
        format!(
            " (TUN is enabled: another process owns the {core} TUN subnet and routes — stop the conflicting proxy/driver or change {core}'s TUN address to a free subnet)"
        )
    } else {
        format!(
            " (TUN is enabled: the {core} process needs CAP_NET_ADMIN to open the TUN device — \
             grant it once with `caly doctor --fix` (one sudo password for ip and the core \
             binaries) or run the daemon with CAP_NET_ADMIN)"
        )
    }
}

/// Default budgets for callers that do not configure them.
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(10);

/// Budget for re-applying the persisted node selection after a core start.
/// Strictly below the start budget so the restore cannot stall the lifecycle
/// window; a failure here is logged, never fatal.
pub(crate) const CONTROL_RESTORE_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);

impl CoreLifecycleBackend {
    /// Starts the core with a config-driven readiness budget.
    pub fn start_timed(
        &mut self,
        start_timeout: Duration,
    ) -> Result<caly_domain::AppliedState, ActorFailure> {
        match self {
            Self::Mihomo(value) => {
                let mut backend = value
                    .lock()
                    .map_err(|_| failure("Mihomo lifecycle lock poisoned", "restart daemon"))?;
                if backend.is_running() {
                    // Idempotent: an already-running core reports success.
                    return state(
                        caly_domain::CoreKind::Mihomo,
                        caly_domain::CoreRunState::Running,
                        backend.generation,
                    );
                }
                let generation = backend.generation;
                backend
                    .start(start_timeout)
                    .map_err(|error| {
                        failure(
                            &format!(
                                "Mihomo start failed: {error}{}",
                                tun_cap_net_admin_hint(
                                    backend.tun_enabled,
                                    "mihomo",
                                    &error.to_string()
                                )
                            ),
                            "inspect Mihomo readiness",
                        )
                    })
                    .and_then(|()| {
                        state(
                            caly_domain::CoreKind::Mihomo,
                            caly_domain::CoreRunState::Running,
                            generation,
                        )
                    })
            }
            Self::SingBox(value) => {
                let mut backend = value
                    .lock()
                    .map_err(|_| failure("sing-box lifecycle lock poisoned", "restart daemon"))?;
                if backend.is_running() {
                    return state(
                        caly_domain::CoreKind::SingBox,
                        caly_domain::CoreRunState::Running,
                        backend.generation(),
                    );
                }
                backend.start_timed(start_timeout).map_err(|error| {
                    // The inner `SingBoxLifecycleBackend` already prefixes with
                    // "sing-box start failed: …" — wrapping its message again would
                    // duplicate the prefix. Reuse it verbatim and append the hint.
                    failure(
                        &format!(
                            "{}{}",
                            error.message,
                            tun_cap_net_admin_hint(
                                backend.tun_enabled,
                                "sing-box",
                                error.message.as_str()
                            )
                        ),
                        "inspect sing-box readiness",
                    )
                })
            }
        }
    }
    /// Stops the core with a config-driven graceful budget.
    pub fn stop_timed(
        &mut self,
        stop_timeout: Duration,
    ) -> Result<caly_domain::AppliedState, ActorFailure> {
        match self {
            Self::Mihomo(value) => {
                let mut backend = value
                    .lock()
                    .map_err(|_| failure("Mihomo lifecycle lock poisoned", "restart daemon"))?;
                if !backend.is_running() {
                    // Idempotent: stopping an already-stopped core succeeds so a
                    // shutdown sequence never fails on a stopped core.
                    return stopped_state();
                }
                backend
                    .stop(stop_timeout)
                    .map_err(|error| {
                        failure(
                            &format!("Mihomo stop failed: {error}"),
                            "inspect process group",
                        )
                    })
                    .and_then(|()| stopped_state())
            }
            Self::SingBox(value) => {
                let mut backend = value
                    .lock()
                    .map_err(|_| failure("sing-box lifecycle lock poisoned", "restart daemon"))?;
                if !backend.is_running() {
                    return stopped_state();
                }
                backend.stop_timed(stop_timeout).map_err(|error| {
                    failure(
                        &format!(
                            "sing-box stop failed: {} ({})",
                            error.message, error.suggested_action
                        ),
                        "inspect process group",
                    )
                })
            }
        }
    }
    /// Restarts the core with a config-driven readiness budget.
    pub fn restart_timed(
        &mut self,
        start_timeout: Duration,
    ) -> Result<caly_domain::AppliedState, ActorFailure> {
        match self {
            Self::Mihomo(value) => {
                let mut backend = value
                    .lock()
                    .map_err(|_| failure("Mihomo lifecycle lock poisoned", "restart daemon"))?;
                backend
                    .restart(start_timeout)
                    .map_err(|error| {
                        failure(
                            &format!(
                                "Mihomo restart failed: {error}{}",
                                tun_cap_net_admin_hint(
                                    backend.tun_enabled,
                                    "mihomo",
                                    &error.to_string()
                                )
                            ),
                            "inspect lifecycle",
                        )
                    })
                    .and_then(|()| {
                        state(
                            caly_domain::CoreKind::Mihomo,
                            caly_domain::CoreRunState::Running,
                            backend.generation,
                        )
                    })
            }
            Self::SingBox(value) => {
                let mut backend = value
                    .lock()
                    .map_err(|_| failure("sing-box lifecycle lock poisoned", "restart daemon"))?;
                backend.restart_timed(start_timeout).map_err(|error| {
                    // The inner `SingBoxLifecycleBackend` already prefixes the full
                    // `sing-box restart failed: …` message; append the hint without
                    // re-prefixing so the user sees the cause once.
                    failure(
                        &format!(
                            "{}{}",
                            error.message,
                            tun_cap_net_admin_hint(
                                backend.tun_enabled,
                                "sing-box",
                                error.message.as_str()
                            )
                        ),
                        "inspect lifecycle",
                    )
                })
            }
        }
    }
}

impl CoreLifecycleCommandBackend for CoreLifecycleBackend {
    fn start(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.start_timed(DEFAULT_START_TIMEOUT)
    }
    fn stop(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.stop_timed(DEFAULT_STOP_TIMEOUT)
    }
    fn restart(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.restart_timed(DEFAULT_START_TIMEOUT)
    }
}

fn stopped_state() -> Result<caly_domain::AppliedState, ActorFailure> {
    // Stopped must not retain runtime details (StoppedWithRuntimeDetails).
    caly_domain::AppliedState::new(None, caly_domain::CoreRunState::Stopped, None, None).map_err(
        |_| {
            failure(
                "core applied state is invalid",
                "inspect lifecycle projection",
            )
        },
    )
}

pub(crate) fn state(
    kind: caly_domain::CoreKind,
    run_state: caly_domain::CoreRunState,
    generation: u64,
) -> Result<caly_domain::AppliedState, ActorFailure> {
    caly_domain::AppliedState::new(Some(kind), run_state, None, Some(generation)).map_err(|_| {
        failure(
            "core applied state is invalid",
            "inspect lifecycle projection",
        )
    })
}
/// Wraps a small literal in the bounded type without panicking. Every call
/// site passes a string literal that is well under the capacity bound, so
/// the `BoundedText::new` failure path is unreachable. We use
/// `from_nonempty_clamped` with a single-character fallback rather than
/// `unwrap_or_else` to a `process::abort` so a future contributor who
/// accidentally grows an input past the bound sees a clamped value, not a
/// daemon crash.
fn text(value: &str) -> caly_domain::BoundedText<1_024> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "_")
}

fn action_text(value: &str) -> caly_domain::BoundedText<512> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "_")
}

#[cfg(test)]
mod hint_tests;
