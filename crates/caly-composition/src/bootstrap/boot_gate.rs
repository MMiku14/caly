//! Daemon boot gate: restore-first, config-driven platform effects and the
//! core auto-start. Kept in a submodule so the phase orchestration stays
//! within the file budget.

use caly_ports::CoreLifecycleCommandBackend;

use super::{CompositionError, RuntimeService, WallClock};
use caly_application::projection::ProjectionRuntime;
use caly_backends::dual::DualCoreLifecycle;

/// Runs the daemon boot gate: restore-first, config-driven system proxy, then
/// core auto-start.
pub(super) fn boot_sequence(
    platform: &mut caly_backends::platform::PlatformBackend,
    tun: &mut caly_backends::platform::DurableTunBackend<caly_backends::LinuxTunCommandBackend>,
    lifecycle: &DualCoreLifecycle,
    service: &std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    system_proxy_enabled: bool,
    auto_start: bool,
    configured_core: caly_domain::CoreKind,
) -> Result<(), CompositionError> {
    // Restore-first: re-apply any pending durable platform (proxy then TUN)
    // side effect before the core auto-starts or any new mutation begins.
    // Recovery runs regardless of auto-start — a crashed daemon must restore
    // its durable effects even when the core stays stopped — and a failed
    // restore degrades, not dies: the record survives for the next boot, so
    // the operator can still reach `caly status`.
    let proxy_outcome = restore_or_degrade(platform.restore_first(), "system-proxy");
    let tun_outcome = restore_or_degrade(tun.restore_first(), "TUN");
    if matches!(
        tun_outcome,
        caly_platform::recovery::ProxyRecoveryOutcome::Restored
    ) {
        // The device and routes were re-engaged from the durable record;
        // project the restored view so `status`/`watch` reflect it instead of
        // stale boot defaults (mirrors the proxy restore publish below).
        publish_boot_effects(
            service,
            caly_domain::PlatformEffectView::tun(true),
            false,
            true,
        );
    }
    boot_system_proxy(platform, service, system_proxy_enabled, proxy_outcome);
    if auto_start {
        boot_auto_start(lifecycle, service, configured_core)
    } else {
        Ok(())
    }
}

/// Restore-first with degradation: a failed durable restore logs a warning
/// and leaves the record for the next boot instead of dying.
fn restore_or_degrade(
    outcome: Result<caly_platform::recovery::ProxyRecoveryOutcome, impl core::fmt::Debug>,
    what: &str,
) -> caly_platform::recovery::ProxyRecoveryOutcome {
    match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::warn!(
                ?error,
                "{what} restore-first failed; degrading and retrying on the next boot"
            );
            caly_platform::recovery::ProxyRecoveryOutcome::NothingPending
        }
    }
}

/// Engages the desktop system proxy at boot when the configuration requests it.
///
/// Degrades, does not die: an unsupported desktop (or failed engagement) logs a
/// warning and leaves the daemon serving, matching the `sys proxy on` contract.
fn boot_system_proxy(
    platform: &mut caly_backends::platform::PlatformBackend,
    service: &std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    enabled: bool,
    outcome: caly_platform::recovery::ProxyRecoveryOutcome,
) {
    // Restore-first re-applied this daemon's proxy effect after a crash
    // regardless of the current config intent (restore-first runs before
    // boot dispatch, see `boot_sequence`). Project the already-engaged view
    // and the matching desired flag so `status` does not report
    // `system-proxy: false` while the desktop actually carries caly's
    // side effect.
    if matches!(
        outcome,
        caly_platform::recovery::ProxyRecoveryOutcome::Restored
    ) {
        publish_boot_effects(
            service,
            caly_domain::PlatformEffectView::proxy(true),
            true,
            false,
        );
        return;
    }
    if !enabled {
        return;
    }
    // Engaging again after a crash would re-capture caly's own proxy as the
    // desktop "original state", corrupting the graceful-shutdown fidelity
    // restore — the `Restored` branch above already short-circuits that.
    match caly_ports::PlatformCommandBackend::set_system_proxy(platform, true) {
        Ok(view) => publish_boot_effects(service, view, true, false),
        Err(error) => {
            tracing::warn!(
                message = %error.message,
                action = %error.suggested_action,
                "system proxy requested by config could not be engaged at boot; \
                 run `caly sysproxy on` on a supported desktop to retry"
            );
        }
    }
}

/// Publishes a boot-time platform effect with the matching desired-state
/// update (best-effort).
///
/// Boot effects are config-driven, so the projection starts from a default
/// `DesiredState` (both flags false); without a `DesiredReplaced` here,
/// `status` would report `system-proxy: false` / `tun: false` while the
/// desktop actually carries caly's engaged side effects. The desired slice is
/// derived from the current snapshot so unrelated fields are preserved.
fn publish_boot_effects(
    service: &std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    view: caly_domain::PlatformEffectView,
    system_proxy_engaged: bool,
    tun_engaged: bool,
) {
    if let Ok(mut service) = service.lock() {
        if let Ok(snapshot) = service.projection_mut().current_snapshot() {
            let desired = snapshot
                .desired()
                .clone()
                .with_system_proxy(system_proxy_engaged)
                .with_tun_requested(tun_engaged);
            let _ = service
                .projection_mut()
                .publish(caly_domain::PresentationDelta::DesiredReplaced(desired));
        }
        let _ = service
            .projection_mut()
            .publish(caly_domain::PresentationDelta::PlatformReplaced(view));
    }
}

/// Engages the configured core during bootstrap and projects its applied state.
///
/// Daemon-first contract: a failed auto-start must never abort the daemon. The
/// daemon keeps serving with a stopped core and `caly core start` retries on
/// demand, so a missing/invalid kernel binary degrades gracefully instead of
/// taking down the control plane.
fn boot_auto_start(
    lifecycle_handle: &DualCoreLifecycle,
    service: &std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    configured_core: caly_domain::CoreKind,
) -> Result<(), CompositionError> {
    let mut lifecycle = lifecycle_handle.clone();
    match CoreLifecycleCommandBackend::start(&mut lifecycle) {
        Ok(state) => {
            // Boot start has no client operation; project the resulting state so
            // `snapshot`/`watch` reflect the running core immediately.
            let Ok(mut service) = service.lock() else {
                // Reverse the start so a failed boot never leaks a running core.
                let _ = CoreLifecycleCommandBackend::stop(&mut lifecycle);
                return Err(CompositionError::BackendUnavailable);
            };
            if service
                .projection_mut()
                .publish(caly_domain::PresentationDelta::AppliedReplaced(state))
                .is_err()
            {
                // Reverse the start before propagating the projection failure.
                let _ = CoreLifecycleCommandBackend::stop(&mut lifecycle);
                return Err(CompositionError::BackendUnavailable);
            }
            Ok(())
        }
        Err(error) => {
            // Degrade, do not die: project a stopped core and keep the daemon
            // serving so the user can fix the binary and retry `core start`.
            tracing::warn!(
                message = %error.message,
                action = %error.suggested_action,
                configured_core = core_label(configured_core),
                "core auto-start failed at boot; daemon continues with a stopped \
                 core (fix the binary/config and run `caly core start`; the active \
                 core is chosen by `core:` in config.yaml or CALY_CORE, so \
                 CALY_CORE=sing-box selects sing-box)"
            );
            let Ok(mut service) = service.lock() else {
                return Err(CompositionError::BackendUnavailable);
            };
            if service
                .projection_mut()
                .publish(caly_domain::PresentationDelta::AppliedReplaced(
                    caly_domain::AppliedState::stopped(),
                ))
                .is_err()
            {
                return Err(CompositionError::BackendUnavailable);
            }
            Ok(())
        }
    }
}

/// Display label for the configured core kind in boot diagnostics.
fn core_label(core: caly_domain::CoreKind) -> &'static str {
    match core {
        caly_domain::CoreKind::Mihomo => "mihomo",
        caly_domain::CoreKind::SingBox => "sing-box",
        caly_domain::CoreKind::Xray => "xray",
    }
}
