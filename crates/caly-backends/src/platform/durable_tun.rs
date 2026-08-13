//! Durable TUN backend: persists a recovery record on engagement and restores
//! it on startup, so a daemon crash never leaves the TUN side effect in an
//! unknown state. Mirrors `DurableSystemProxyBackend` for the TUN side effect.

use std::sync::Arc;

use caly_domain::PlatformEffectView;
use caly_platform::{
    recovery::{
        tun_restore_first, RecoveryPhase, TunRecoveryAction, TunRecoveryRecord, TunRecoveryStore,
    },
    PlatformFailure,
};
use caly_ports::{ActorFailure, TunCommandBackend};

use super::durable_support::{actor_to_platform, durable_failure};

/// Shared durable TUN recovery store handle.
pub type SharedTunRecoveryStore = Arc<dyn TunRecoveryStore + Send + Sync>;

/// The TUN operations needed for durable recovery (interface/MTU readback).
pub trait TunControl {
    fn set_tun(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure>;
    fn interface(&self) -> &str;
    fn mtu(&self) -> u16;
}

impl TunControl for super::LinuxTunCommandBackend {
    fn set_tun(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        TunCommandBackend::set_tun(self, enabled)
    }
    fn interface(&self) -> &str {
        self.interface()
    }
    fn mtu(&self) -> u16 {
        self.mtu()
    }
}

/// Wraps a TUN control with an owner-only durable recovery store.
pub struct DurableTunBackend<C> {
    inner: C,
    store: SharedTunRecoveryStore,
    owner_token: [u8; 16],
}

impl<C: TunControl> DurableTunBackend<C> {
    /// Wraps `inner` with `store` and a fresh owner token.
    pub fn new(inner: C, store: SharedTunRecoveryStore) -> Self {
        let owner_token = caly_platform::entropy::random_bytes::<16>();
        Self {
            inner,
            store,
            owner_token,
        }
    }

    /// Re-applies any pending TUN recovery record (restore-first startup gate).
    pub fn restore_first(
        &mut self,
    ) -> Result<
        caly_platform::recovery::ProxyRecoveryOutcome,
        caly_platform::recovery::TunRecoveryFailure,
    > {
        let store = Arc::clone(&self.store);
        tun_restore_first(store.as_ref(), self)
    }

    /// Persists a recovery record for an enabled TUN side effect.
    fn persist_enabled_record(&mut self) -> Result<(), ActorFailure> {
        let record = TunRecoveryRecord {
            owner_token: self.owner_token,
            phase: RecoveryPhase::Applied,
            interface: self.inner.interface().to_owned(),
            mtu: self.inner.mtu(),
            enabled: true,
        };
        self.store
            .persist(&record)
            .map_err(|e| durable_failure(e, "tun"))?;
        Ok(())
    }

    /// Clears the recovery record when the side effect is no longer pending.
    fn clear_record(&mut self) {
        // A failed clear leaves the record behind; restore-first on the next
        // boot then re-engages the TUN device. That is safe (idempotent) but
        // undesired after a successful release — surface it instead of
        // swallowing it silently.
        if let Err(error) = self.store.clear_if_owner(self.owner_token) {
            tracing::warn!(
                error = ?error,
                "could not clear the durable TUN record; restore-first will \
                 re-engage the device on the next daemon boot"
            );
        }
    }
}

impl<C: TunControl> TunCommandBackend for DurableTunBackend<C> {
    fn set_tun(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        if enabled {
            // Persist the intent durably before applying, so a crash between
            // apply and the projection still leaves a recoverable record. A
            // re-engagement keeps the existing record, so a failed retry never
            // destroys the crash-recovery evidence of the first engagement.
            let existing = self
                .store
                .load()
                .map_err(|e| durable_failure(e, "tun"))?
                .is_some();
            if !existing {
                self.persist_enabled_record()?;
            }
            match self.inner.set_tun(true) {
                Ok(view) => Ok(view),
                Err(error) => {
                    if !existing {
                        self.clear_record();
                    }
                    Err(error)
                }
            }
        } else {
            // `sys tun off` and the graceful-shutdown release share one
            // implementation (`release_recorded_tun`): release the device and
            // clear the record only on success. A failed release retains the
            // record so restore-first on the next boot can retry; a missing
            // record is a no-op because this daemon never engaged TUN.
            self.release_recorded_tun()
        }
    }
}

impl<C: TunControl> DurableTunBackend<C> {
    /// Graceful-shutdown restore: releases the TUN device, then clears the
    /// record. A failed release retains the record so restore-first on the
    /// next boot can retry; a missing record is a no-op success (this daemon
    /// never engaged TUN, so nothing may be touched).
    pub fn restore_and_clear(&mut self) -> Result<PlatformEffectView, ActorFailure> {
        self.release_recorded_tun()
    }

    /// Releases the TUN device and clears the record. Shared by `sys tun off`
    /// and the graceful-shutdown restore so both "undo" paths behave
    /// identically: a failed release retains the record (restore-first on a
    /// later daemon boot retries), a missing record is a no-op success.
    fn release_recorded_tun(&mut self) -> Result<PlatformEffectView, ActorFailure> {
        let Some(_record) = self.store.load().map_err(|e| durable_failure(e, "tun"))? else {
            return Ok(PlatformEffectView::none());
        };
        match self.inner.set_tun(false) {
            Ok(view) => {
                self.clear_record();
                Ok(view)
            }
            Err(error) => Err(error),
        }
    }
}

impl<C: TunControl> TunRecoveryAction for DurableTunBackend<C> {
    fn restore_tun(&mut self, record: &TunRecoveryRecord) -> Result<(), PlatformFailure> {
        self.inner
            .set_tun(record.enabled)
            .map(|_| ())
            .map_err(|e| actor_to_platform(e, "restore-tun", "tun"))
    }

    fn current_owner_token(&self) -> [u8; 16] {
        self.owner_token
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_platform::recovery::FileRecoveryStore;

    struct FakeTun {
        fail: bool,
        enabled: bool,
        interface: String,
        mtu: u16,
    }
    impl TunControl for FakeTun {
        fn set_tun(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
            if self.fail {
                return Err(crate::failure("apply failed", "retry"));
            }
            self.enabled = enabled;
            Ok(PlatformEffectView::tun(enabled))
        }
        fn interface(&self) -> &str {
            &self.interface
        }
        fn mtu(&self) -> u16 {
            self.mtu
        }
    }

    fn fake_tun(fail: bool) -> FakeTun {
        FakeTun {
            fail,
            enabled: false,
            interface: "caly0".to_owned(),
            mtu: 1_500,
        }
    }

    fn shared_store(label: &str) -> Result<SharedTunRecoveryStore, String> {
        // The recovery store refuses a parent directory it cannot tighten
        // to 0700 (audit #104): root every test record in its own fresh
        // subdirectory instead of the shared temp dir itself.
        let path = caly_platform::paths::test_helpers::unique_path_under("caly-tun", label)
            .join("record.json");
        FileRecoveryStore::<TunRecoveryRecord>::new(path)
            .map(|store| Arc::new(store) as SharedTunRecoveryStore)
            .map_err(|e| format!("{e:?}"))
    }

    fn record(seed: u8) -> TunRecoveryRecord {
        TunRecoveryRecord {
            owner_token: [seed; 16],
            phase: RecoveryPhase::Applied,
            interface: "caly0".to_owned(),
            mtu: 1_500,
            enabled: true,
        }
    }

    #[test]
    fn engage_persists_record_and_disable_clears_it() -> Result<(), String> {
        let store = shared_store("engage")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store.clone());
        backend.set_tun(true).map_err(|e| format!("{e:?}"))?;
        assert!(store.load().map_err(|e| format!("{e:?}"))?.is_some());
        backend.set_tun(false).map_err(|e| format!("{e:?}"))?;
        assert!(store.load().map_err(|e| format!("{e:?}"))?.is_none());
        Ok(())
    }

    #[test]
    fn failed_apply_clears_record() -> Result<(), String> {
        let store = shared_store("fail")?;
        let mut backend = DurableTunBackend::new(fake_tun(true), store.clone());
        assert!(backend.set_tun(true).is_err());
        assert!(store.load().map_err(|e| format!("{e:?}"))?.is_none());
        Ok(())
    }

    #[test]
    fn failed_disable_retains_recovery_record() -> Result<(), String> {
        let store = shared_store("faildisable")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store.clone());
        backend.set_tun(true).map_err(|e| format!("{e:?}"))?;
        backend.inner.fail = true;
        assert!(backend.set_tun(false).is_err());
        assert!(store.load().map_err(|e| format!("{e:?}"))?.is_some());
        Ok(())
    }

    #[test]
    fn disable_without_record_never_touches_the_device() -> Result<(), String> {
        let store = shared_store("disnoop")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store);
        let view = backend.set_tun(false).map_err(|e| format!("{e:?}"))?;
        // No record means this daemon never engaged TUN: disabling must not
        // tear down a device caly does not own.
        assert_eq!(view, PlatformEffectView::none());
        assert!(!backend.inner.enabled);
        Ok(())
    }

    #[test]
    fn restore_first_reapplies_and_keeps_record() -> Result<(), String> {
        let store = shared_store("first")?;
        store.persist(&record(7)).map_err(|e| format!("{e:?}"))?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store.clone());
        backend.restore_first().map_err(|e| format!("{e:?}"))?;
        // The record must survive with phase `Applied`: the re-engaged
        // device stays releasable via `sys tun off` / graceful shutdown
        // (a cleared record would leak the TUN device after shutdown).
        let kept = store
            .load()
            .map_err(|e| format!("{e:?}"))?
            .expect("record must be kept after restore-first");
        assert_eq!(kept.phase, caly_platform::recovery::RecoveryPhase::Applied);
        assert!(backend.inner.enabled);
        Ok(())
    }

    #[test]
    fn nothing_pending_is_noop() -> Result<(), String> {
        let store = shared_store("noop")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store);
        assert_eq!(
            backend.restore_first().map_err(|e| format!("{e:?}"))?,
            caly_platform::recovery::ProxyRecoveryOutcome::NothingPending
        );
        Ok(())
    }

    #[test]
    fn failed_reengage_retains_recovery_record() -> Result<(), String> {
        let store = shared_store("refail")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store.clone());
        backend.set_tun(true).map_err(|e| format!("{e:?}"))?;
        backend.inner.fail = true;
        assert!(backend.set_tun(true).is_err());
        assert!(
            store.load().map_err(|e| format!("{e:?}"))?.is_some(),
            "a failed re-engagement must not destroy crash-recovery evidence"
        );
        Ok(())
    }

    #[test]
    fn restore_and_clear_releases_and_clears_record() -> Result<(), String> {
        let store = shared_store("relclear")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store.clone());
        backend.set_tun(true).map_err(|e| format!("{e:?}"))?;
        backend.restore_and_clear().map_err(|e| format!("{e:?}"))?;
        assert!(!backend.inner.enabled);
        assert!(store.load().map_err(|e| format!("{e:?}"))?.is_none());
        Ok(())
    }

    #[test]
    fn restore_and_clear_without_record_is_noop() -> Result<(), String> {
        let store = shared_store("relnoop")?;
        let mut backend = DurableTunBackend::new(fake_tun(false), store.clone());
        backend.restore_and_clear().map_err(|e| format!("{e:?}"))?;
        assert!(!backend.inner.enabled);
        Ok(())
    }
}
