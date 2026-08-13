//! Tests for the durable system-proxy backend: engage/disable record
//! lifecycle, restore-first, and graceful-shutdown fidelity restore.

use std::sync::Arc;

use caly_domain::PlatformEffectView;
use caly_platform::recovery::{FileRecoveryStore, ProxyRecoveryRecord, RecoveryPhase};
use caly_ports::ActorFailure;

use super::{DesktopProxyControl, DurableSystemProxyBackend, SharedProxyRecoveryStore};
use caly_platform::recovery::ProxyRecoveryAction;
use caly_ports::PlatformCommandBackend;

struct FakeControl {
    fail: bool,
    enabled: bool,
    host: String,
    port: u16,
    capture_state: (String, String),
    restore_fail: bool,
    restored: Option<(String, String)>,
}

impl DesktopProxyControl for FakeControl {
    fn set_system_proxy(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        if self.fail {
            return Err(ActorFailure::new(
                caly_ports::ActorFailureKind::Infrastructure,
                "apply failed",
                "retry",
            )
            .unwrap());
        }
        self.enabled = enabled;
        Ok(PlatformEffectView::proxy(enabled))
    }
    fn host(&self) -> &str {
        &self.host
    }
    fn port(&self) -> u16 {
        self.port
    }
    fn capture_original_state(&mut self) -> (String, String) {
        self.capture_state.clone()
    }
    fn restore_original(
        &mut self,
        mode: &str,
        endpoint: &str,
    ) -> Result<PlatformEffectView, ActorFailure> {
        if self.restore_fail {
            return Err(ActorFailure::new(
                caly_ports::ActorFailureKind::Infrastructure,
                "restore failed",
                "retry",
            )
            .unwrap());
        }
        self.restored = Some((mode.to_owned(), endpoint.to_owned()));
        Ok(PlatformEffectView::none())
    }
}

fn fake_control(fail: bool) -> FakeControl {
    FakeControl {
        fail,
        enabled: false,
        host: "127.0.0.1".to_owned(),
        port: 7890,
        capture_state: ("none".to_owned(), String::new()),
        restore_fail: false,
        restored: None,
    }
}

fn store_path(label: &str) -> std::path::PathBuf {
    // The recovery store refuses a parent directory it cannot tighten to
    // 0700 (audit #104): root every test record in its own fresh
    // subdirectory instead of the shared temp dir itself.
    caly_platform::paths::test_helpers::unique_path_under("caly-durable-proxy", label)
        .join("record.json")
}

fn shared_store(path: &std::path::Path) -> Result<SharedProxyRecoveryStore, String> {
    FileRecoveryStore::<caly_platform::recovery::ProxyRecoveryRecord>::new(path.to_path_buf())
        .map(|store| Arc::new(store) as SharedProxyRecoveryStore)
        .map_err(|e| format!("{e:?}"))
}

fn record(seed: u8) -> ProxyRecoveryRecord {
    ProxyRecoveryRecord {
        owner_token: [seed; 16],
        phase: RecoveryPhase::Applied,
        host: "127.0.0.1".to_owned(),
        port: 7890,
        enabled: true,
        original_mode: "manual".to_owned(),
        original_endpoint: String::new(),
        pac_url: String::new(),
    }
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn engage_persists_record_and_disable_clears_it() -> Result<(), String> {
    let path = store_path("engage");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    assert!(backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .is_some());
    backend
        .set_system_proxy(false)
        .map_err(|e| format!("{e:?}"))?;
    assert!(backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .is_none());
    cleanup(&path);
    Ok(())
}

#[test]
fn engage_persists_captured_original_state() -> Result<(), String> {
    let path = store_path("capture");
    let store = shared_store(&path)?;
    let mut control = fake_control(false);
    control.capture_state = ("manual".to_owned(), "10.0.0.1:8080".to_owned());
    let mut backend = DurableSystemProxyBackend::new(control, store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    let stored = backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .ok_or("record missing")?;
    assert_eq!(stored.original_mode, "manual");
    assert_eq!(stored.original_endpoint, "10.0.0.1:8080");
    cleanup(&path);
    Ok(())
}

#[test]
fn failed_apply_clears_record() -> Result<(), String> {
    let path = store_path("failapply");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(true), store);
    assert!(backend.set_system_proxy(true).is_err());
    assert!(backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .is_none());
    cleanup(&path);
    Ok(())
}

#[test]
fn failed_disable_retains_recovery_record() -> Result<(), String> {
    let path = store_path("faildisable");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store.clone());
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    // Disable restores the captured state; a failed restore must keep the
    // record so restore-first on the next boot can retry.
    backend.inner.restore_fail = true;
    assert!(backend.set_system_proxy(false).is_err());
    assert!(store.load().map_err(|e| format!("{e:?}"))?.is_some());
    cleanup(&path);
    Ok(())
}

#[test]
fn disable_restores_captured_state_and_clears_record() -> Result<(), String> {
    let path = store_path("disrestore");
    let store = shared_store(&path)?;
    let mut control = fake_control(false);
    control.capture_state = ("manual".to_owned(), "10.0.0.1:8080".to_owned());
    let mut backend = DurableSystemProxyBackend::new(control, store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    backend
        .set_system_proxy(false)
        .map_err(|e| format!("{e:?}"))?;
    // The desktop must be restored to the pre-engagement state, not merely
    // disabled, and the record must be cleared.
    assert_eq!(
        backend.inner.restored,
        Some(("manual".to_owned(), "10.0.0.1:8080".to_owned()))
    );
    assert!(backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .is_none());
    cleanup(&path);
    Ok(())
}

#[test]
fn disable_without_record_never_touches_the_desktop() -> Result<(), String> {
    let path = store_path("disnoop");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store);
    let view = backend
        .set_system_proxy(false)
        .map_err(|e| format!("{e:?}"))?;
    // No record means this daemon never engaged the proxy: disabling must not
    // clobber a proxy the user configured outside caly.
    assert_eq!(view, PlatformEffectView::none());
    assert_eq!(backend.inner.restored, None);
    assert!(!backend.inner.enabled);
    cleanup(&path);
    Ok(())
}

#[test]
fn restore_proxy_reapplies_enabled_state() -> Result<(), String> {
    let path = store_path("restore");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store);
    backend
        .restore_proxy(&record(3))
        .map_err(|e| format!("{e:?}"))?;
    cleanup(&path);
    Ok(())
}

#[test]
fn restore_first_reapplies_and_keeps_record() -> Result<(), String> {
    let path = store_path("first");
    let store = shared_store(&path)?;
    store.persist(&record(7)).map_err(|e| format!("{e:?}"))?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store);
    backend.restore_first().map_err(|e| format!("{e:?}"))?;
    // The record must survive with phase `Applied`: the re-applied proxy
    // stays undoable via `sysproxy off` / graceful shutdown (a cleared
    // record would orphan the engaged side effect forever).
    let kept = backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .expect("record must be kept after restore-first");
    assert_eq!(kept.phase, caly_platform::recovery::RecoveryPhase::Applied);
    assert!(backend.inner.enabled);
    cleanup(&path);
    Ok(())
}

#[test]
fn restore_original_and_clear_restores_captured_state() -> Result<(), String> {
    let path = store_path("fidelity");
    let store = shared_store(&path)?;
    let mut control = fake_control(false);
    control.capture_state = ("manual".to_owned(), "10.0.0.1:8080".to_owned());
    let mut backend = DurableSystemProxyBackend::new(control, store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    backend
        .restore_original_and_clear()
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        backend.inner.restored,
        Some(("manual".to_owned(), "10.0.0.1:8080".to_owned()))
    );
    assert!(backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .is_none());
    cleanup(&path);
    Ok(())
}

#[test]
fn restore_original_and_clear_retains_record_on_failure() -> Result<(), String> {
    let path = store_path("fidfail");
    let store = shared_store(&path)?;
    let mut control = fake_control(false);
    control.capture_state = ("manual".to_owned(), "10.0.0.1:8080".to_owned());
    let mut backend = DurableSystemProxyBackend::new(control, store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    backend.inner.restore_fail = true;
    assert!(backend.restore_original_and_clear().is_err());
    assert!(backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .is_some());
    cleanup(&path);
    Ok(())
}

#[test]
fn restore_original_and_clear_without_record_is_noop() -> Result<(), String> {
    let path = store_path("fidnoop");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store);
    backend
        .restore_original_and_clear()
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(backend.inner.restored, None);
    cleanup(&path);
    Ok(())
}

#[test]
fn reengage_keeps_first_captured_original_state() -> Result<(), String> {
    let path = store_path("reengage");
    let store = shared_store(&path)?;
    let mut control = fake_control(false);
    control.capture_state = ("manual".to_owned(), "10.0.0.1:8080".to_owned());
    let mut backend = DurableSystemProxyBackend::new(control, store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    // A re-engagement must not re-capture: the desktop now carries caly's own
    // proxy, which must never overwrite the user's true original state.
    backend.inner.capture_state = ("none".to_owned(), String::new());
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    let stored = backend
        .store
        .load()
        .map_err(|e| format!("{e:?}"))?
        .ok_or("record missing")?;
    assert_eq!(stored.original_mode, "manual");
    assert_eq!(stored.original_endpoint, "10.0.0.1:8080");
    cleanup(&path);
    Ok(())
}

#[test]
fn failed_reengage_retains_recovery_record() -> Result<(), String> {
    let path = store_path("refail");
    let store = shared_store(&path)?;
    let mut backend = DurableSystemProxyBackend::new(fake_control(false), store);
    backend
        .set_system_proxy(true)
        .map_err(|e| format!("{e:?}"))?;
    backend.inner.fail = true;
    assert!(backend.set_system_proxy(true).is_err());
    assert!(
        backend
            .store
            .load()
            .map_err(|e| format!("{e:?}"))?
            .is_some(),
        "a failed re-engagement must not destroy crash-recovery evidence"
    );
    cleanup(&path);
    Ok(())
}
