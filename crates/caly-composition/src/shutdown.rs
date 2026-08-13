//! Daemon shutdown action driver wired to `run_shutdown`.
//!
//! Phases for subsystems that are not yet active in the current composition
//! complete as no-ops; the phases with real resources (telemetry, core process,
//! durable platform record) perform their ordered shutdown work.

use caly_domain::BoundedText;

use caly_application::{
    actors::{CoreLifecycleCommandBackend, TelemetryActorCommand},
    runtime::{ShutdownActions, ShutdownRunError, run_shutdown},
};
use caly_backends::dual::DualCoreLifecycle;

/// Bounded shutdown failure text.
fn bounded_shutdown(value: impl Into<String>) -> BoundedText<512> {
    BoundedText::from_nonempty_clamped(value.into(), "shutdown failed")
}

/// Collects the two platform restores into a single bounded failure.
///
/// Both results are evaluated independently so a proxy failure can never
/// skip the TUN release; each failure keeps its record for restore-first on
/// the next boot (see the durable backends). Pure for testability.
fn collect_restore_failures(
    proxy: Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure>,
    tun: Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure>,
) -> Result<(), BoundedText<512>> {
    let mut failures: Vec<String> = Vec::new();
    if let Err(error) = proxy {
        failures.push(format!(
            "system proxy restore failed: {} ({})",
            error.message.as_str(),
            error.suggested_action.as_str()
        ));
    }
    if let Err(error) = tun {
        failures.push(format!(
            "TUN restore failed: {} ({})",
            error.message.as_str(),
            error.suggested_action.as_str()
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(bounded_shutdown(failures.join("; ")))
    }
}

/// Concrete shutdown actions for the current daemon composition.
pub(crate) struct DaemonShutdownActions {
    pub telemetry: caly_application::runtime::ActorIngress<TelemetryActorCommand>,
    pub lifecycle: DualCoreLifecycle,
    pub platform: super::SharedPlatformBackend,
    pub tun: super::SharedTunBackend,
}

impl DaemonShutdownActions {
    /// Runs the ordered shutdown, returning the recorded phase failures.
    pub async fn run(self) -> Result<Vec<BoundedText<512>>, ShutdownRunError> {
        let mut actions = self;
        let failures = run_shutdown(&mut actions).await?;
        Ok(failures.iter().map(|f| f.message.clone()).collect())
    }
}

#[async_trait::async_trait]
impl ShutdownActions for DaemonShutdownActions {
    async fn stop_telemetry(&mut self) -> Result<(), BoundedText<512>> {
        send_shutdown(&self.telemetry, TelemetryActorCommand::Shutdown);
        Ok(())
    }

    async fn stop_and_reap_core(&mut self) -> Result<(), BoundedText<512>> {
        let mut lifecycle = self.lifecycle.clone();
        CoreLifecycleCommandBackend::stop(&mut lifecycle)
            .map(|_| ())
            .map_err(|error| {
                bounded_shutdown(format!(
                    "core stop failed during shutdown: {}",
                    error.message.as_str()
                ))
            })
    }

    async fn restore_platform(&mut self) -> Result<(), BoundedText<512>> {
        // Fidelity restore: return the desktop to the exact state captured
        // before engagement. The durable backend clears its record only after
        // a successful revert; a missing record means nothing was engaged.
        // Proxy and TUN restores are collected independently: a proxy failure
        // must never skip TUN release (the pre-fix `?` short-circuit left the
        // TUN device + record behind, and next-boot restore-first would
        // re-engage TUN against the operator's intent).
        collect_restore_failures(
            self.platform.restore_original_and_clear(),
            self.tun.restore_and_clear(),
        )
    }
}

/// Sends a terminal `Shutdown` message to an actor ingress; a full or closed
/// mailbox is tolerated because the task group will still be awaited.
fn send_shutdown<M: Send>(ingress: &caly_application::runtime::ActorIngress<M>, message: M) {
    let _ = ingress.try_send(message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_application::runtime::{ShutdownDriver, ShutdownPhase};

    struct RecordingActions {
        order: std::sync::Mutex<Vec<&'static str>>,
    }

    impl RecordingActions {
        fn record(&self, name: &'static str) {
            self.order.lock().unwrap().push(name);
        }
    }

    #[async_trait::async_trait]
    impl ShutdownActions for RecordingActions {
        async fn reject_transport_mutations(&mut self) -> Result<(), BoundedText<512>> {
            self.record("reject_transport");
            Ok(())
        }
        async fn close_command_ingress(&mut self) -> Result<(), BoundedText<512>> {
            self.record("close_ingress");
            Ok(())
        }
        async fn stop_telemetry(&mut self) -> Result<(), BoundedText<512>> {
            self.record("stop_telemetry");
            Ok(())
        }
        async fn stop_and_reap_core(&mut self) -> Result<(), BoundedText<512>> {
            self.record("stop_core");
            Ok(())
        }
        async fn restore_platform(&mut self) -> Result<(), BoundedText<512>> {
            self.record("restore_platform");
            Ok(())
        }
        async fn stop_subscriptions(&mut self) -> Result<(), BoundedText<512>> {
            self.record("stop_subscriptions");
            Ok(())
        }
        async fn finish_config_transactions(&mut self) -> Result<(), BoundedText<512>> {
            self.record("finish_config");
            Ok(())
        }
        async fn flush_event_sequencer(&mut self) -> Result<(), BoundedText<512>> {
            self.record("flush_sequencer");
            Ok(())
        }
        async fn stop_projector(&mut self) -> Result<(), BoundedText<512>> {
            self.record("stop_projector");
            Ok(())
        }
        async fn release_transport_and_lock(&mut self) -> Result<(), BoundedText<512>> {
            self.record("release_transport");
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_shutdown_drives_all_phases_in_order() -> Result<(), ShutdownRunError> {
        let actions = RecordingActions {
            order: std::sync::Mutex::new(Vec::new()),
        };
        let mut actions = actions;
        let failures = run_shutdown(&mut actions).await?;
        assert!(failures.is_empty());
        let order = actions.order.lock().unwrap().clone();
        assert_eq!(
            order,
            vec![
                "reject_transport",
                "close_ingress",
                "stop_telemetry",
                "stop_core",
                "restore_platform",
                "stop_subscriptions",
                "finish_config",
                "flush_sequencer",
                "stop_projector",
                "release_transport",
            ]
        );
        Ok(())
    }

    #[test]
    fn shutdown_driver_advances_to_completed() {
        let mut driver = ShutdownDriver::new();
        let mut steps = 0;
        while driver.current() != ShutdownPhase::Completed {
            assert!(driver.complete_current().is_ok());
            steps += 1;
        }
        // 10 phases + Completed sentinel.
        assert_eq!(steps, 10);
    }

    fn ok_view() -> caly_domain::PlatformEffectView {
        caly_domain::PlatformEffectView::none()
    }

    fn proxy_failure() -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        Err(caly_ports::ActorFailure::infrastructure(
            "gsettings unavailable",
            "check GNOME",
        ))
    }

    fn tun_failure() -> Result<caly_domain::PlatformEffectView, caly_ports::ActorFailure> {
        Err(caly_ports::ActorFailure::infrastructure(
            "ip link failed",
            "check TUN permissions",
        ))
    }

    #[test]
    fn collect_restore_failures_both_ok_is_silent() {
        assert!(collect_restore_failures(Ok(ok_view()), Ok(ok_view())).is_ok());
    }

    #[test]
    fn collect_restore_failures_proxy_failure_still_releases_tun() {
        // The pre-fix `?` short-circuit skipped the TUN release entirely; the
        // contract is: proxy failure must not swallow the TUN restore.
        let error = collect_restore_failures(proxy_failure(), Ok(ok_view()))
            .expect_err("proxy failure must surface");
        let text = error.as_str();
        assert!(
            text.contains("system proxy restore failed: gsettings unavailable"),
            "{text}"
        );
        assert!(!text.contains("TUN restore failed"), "{text}");
    }

    #[test]
    fn collect_restore_failures_tun_failure_surfaces_alone() {
        let error = collect_restore_failures(Ok(ok_view()), tun_failure())
            .expect_err("TUN failure must surface");
        let text = error.as_str();
        assert!(
            text.contains("TUN restore failed: ip link failed"),
            "{text}"
        );
        assert!(!text.contains("system proxy restore failed"), "{text}");
    }

    #[test]
    fn collect_restore_failures_both_failures_are_merged() {
        let error = collect_restore_failures(proxy_failure(), tun_failure())
            .expect_err("both failures must surface");
        let text = error.as_str();
        assert!(
            text.contains("system proxy restore failed: gsettings unavailable"),
            "{text}"
        );
        assert!(
            text.contains("TUN restore failed: ip link failed"),
            "{text}"
        );
    }
}
