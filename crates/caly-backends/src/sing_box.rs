//! Application lifecycle wrapper for sing-box.

use std::{path::PathBuf, time::Duration};

use caly_corectl::sing_box::SingBoxRuntime;
use caly_domain::BoundedText;
use caly_ports::{ActorFailure, CoreLifecycleCommandBackend};

/// Budget for re-applying the persisted node selection after a core start.
/// Strictly below the start budget so the restore cannot stall the lifecycle
/// window; a failure here is logged, never fatal.
const CONTROL_RESTORE_TIMEOUT: Duration = Duration::from_secs(3);

/// sing-box lifecycle backend using the same operation handler contract.
pub struct SingBoxLifecycleBackend {
    runtime: SingBoxRuntime,
    generation: u64,
    /// Whether the rendered config enables a TUN inbound; a start failure
    /// then likely needs `CAP_NET_ADMIN` in the core process.
    pub(crate) tun_enabled: bool,
    /// Controller endpoint/secret kept for the post-start node-selection
    /// restore (the runtime owns its own control instance).
    controller: String,
    secret: Option<BoundedText<4_096>>,
}

impl SingBoxLifecycleBackend {
    /// sing-box has no Clash-compatible config-write endpoint, so a hot
    /// reload is not possible — the caller falls back to a restart.
    pub fn hot_reload(&self, _config: &[u8], _timeout: Duration) -> Result<(), String> {
        Err("sing-box has no hot reload surface; falling back to a restart".to_owned())
    }
    /// Returns the rendered-config generation this backend was built with
    /// (reported into lifecycle projections).
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn poll_abnormal_exit(
        &mut self,
    ) -> Result<Option<caly_domain::AppliedState>, ActorFailure> {
        let Some(_) = self.runtime.poll_exit().map_err(|error| {
            crate::failure(
                &format!("sing-box exit poll failed: {error}"),
                "inspect sing-box process ownership",
            )
        })?
        else {
            return Ok(None);
        };
        // A crash is a platform observation reported regardless of any pending
        // client operation, so the projection reflects the truth.
        let state = state(
            caly_domain::CoreKind::SingBox,
            caly_domain::CoreRunState::Crashed,
            self.generation,
        )?;
        Ok(Some(state))
    }

    pub fn new(
        binary: PathBuf,
        directory: PathBuf,
        controller: String,
        config: PathBuf,
        generation: u64,
        secret: Option<String>,
        tun_enabled: bool,
    ) -> Result<Self, ActorFailure> {
        let secret = secret.map(|value| {
            BoundedText::new(value).map_err(|_| "sing-box controller secret is too long".to_owned())
        });
        let secret = match secret {
            Some(Ok(value)) => Some(value),
            Some(Err(error)) => {
                return Err(crate::failure(&error, "shorten the controller secret"));
            }
            None => None,
        };
        let runtime = SingBoxRuntime::new(
            binary,
            directory,
            controller.clone(),
            config,
            generation,
            secret.clone(),
        )
        .map_err(|error| {
            crate::failure(
                &format!("sing-box lifecycle init failed: {error}"),
                "inspect sing-box paths and controller",
            )
        })?;
        Ok(Self {
            runtime,
            generation,
            tun_enabled,
            controller,
            secret,
        })
    }

    /// Re-applies the persisted node selection once the core is ready.
    /// Best-effort: controller failures only log. A missing secret is
    /// fine — the controller may be secretless, and skipping the restore
    /// entirely would silently drop the selection on every restart
    /// (2026-08-12 agent audit; the Mihomo lifecycle path already
    /// supports secretless).
    fn restore_selection(&self) {
        let Ok(control) = caly_corectl::sing_box::SingBoxHttpControl::new(
            self.controller.clone(),
            self.secret.clone(),
        ) else {
            return;
        };
        crate::selection::restore(
            caly_domain::CoreKind::SingBox,
            CONTROL_RESTORE_TIMEOUT,
            |group, name| {
                control
                    .select_proxy(group, name, CONTROL_RESTORE_TIMEOUT)
                    .map_err(|error| error.message.to_string())
            },
        );
    }

    /// Returns whether the sing-box process tree is currently owned.
    pub const fn is_running(&self) -> bool {
        self.runtime.is_running()
    }

    pub fn poll_exit(
        &mut self,
    ) -> Result<Option<caly_corectl::sing_box::SingBoxRuntimeEvent>, ActorFailure> {
        self.runtime.poll_exit().map_err(|error| {
            crate::failure(
                &format!("sing-box abnormal-exit poll failed: {error}"),
                "inspect sing-box process ownership",
            )
        })
    }
}

impl SingBoxLifecycleBackend {
    /// Starts the core with a config-driven readiness budget.
    pub fn start_timed(
        &mut self,
        start_timeout: Duration,
    ) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.runtime.start(start_timeout).map_err(|error| {
            crate::failure(
                &format!(
                    "sing-box start failed: {} ({})",
                    error.message, error.suggested_action
                ),
                "inspect sing-box config and readiness",
            )
        })?;
        self.restore_selection();
        state(
            caly_domain::CoreKind::SingBox,
            caly_domain::CoreRunState::Running,
            self.generation,
        )
    }

    /// Stops the core with a config-driven graceful budget.
    pub fn stop_timed(
        &mut self,
        stop_timeout: Duration,
    ) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.runtime.stop(stop_timeout).map_err(|error| {
            crate::failure(
                &format!(
                    "sing-box stop failed: {} ({})",
                    error.message, error.suggested_action
                ),
                "inspect sing-box process ownership",
            )
        })?;
        stopped_state()
    }

    /// Restarts the core with a config-driven readiness budget.
    pub fn restart_timed(
        &mut self,
        start_timeout: Duration,
    ) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.runtime.restart(start_timeout).map_err(|error| {
            crate::failure(
                &format!(
                    "sing-box restart failed: {} ({})",
                    error.message, error.suggested_action
                ),
                "inspect sing-box lifecycle",
            )
        })?;
        self.restore_selection();
        state(
            caly_domain::CoreKind::SingBox,
            caly_domain::CoreRunState::Running,
            self.generation,
        )
    }
}

impl CoreLifecycleCommandBackend for SingBoxLifecycleBackend {
    fn start(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.start_timed(Duration::from_secs(10))
    }

    fn stop(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.stop_timed(Duration::from_secs(5))
    }

    fn restart(&mut self) -> Result<caly_domain::AppliedState, ActorFailure> {
        self.restart_timed(Duration::from_secs(10))
    }
}

fn stopped_state() -> Result<caly_domain::AppliedState, ActorFailure> {
    // Stopped must not retain runtime details (StoppedWithRuntimeDetails).
    caly_domain::AppliedState::new(None, caly_domain::CoreRunState::Stopped, None, None).map_err(
        |_| {
            crate::failure(
                "sing-box applied state is invalid",
                "inspect lifecycle projection",
            )
        },
    )
}

fn state(
    kind: caly_domain::CoreKind,
    run_state: caly_domain::CoreRunState,
    generation: u64,
) -> Result<caly_domain::AppliedState, ActorFailure> {
    caly_domain::AppliedState::new(Some(kind), run_state, None, Some(generation)).map_err(|_| {
        crate::failure(
            "sing-box applied state is invalid",
            "inspect lifecycle projection",
        )
    })
}
