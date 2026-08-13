//! Restore-first durable recovery state machine.
//!
//! A single generic, owner-only, atomic recovery store (`FileRecoveryStore<R>`)
//! serves the two concrete platform side effects — system proxy and TUN — via
//! typed records and typed restore-first startup gates. There is deliberately
//! no separate legacy recovery abstraction: both side effects share one store
//! implementation and one restore-first pattern.

mod store;
pub use store::{
    FileRecoveryStore, ProxyRecoveryRecord, ProxyRecoveryStore, RecoveryStore, RecoveryStoreError,
    TunRecoveryRecord, TunRecoveryStore,
};

use crate::PlatformFailure;

/// Durable recovery lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RecoveryPhase {
    Prepared,
    Applied,
    Restoring,
}

/// Re-applies a durable proxy side effect to the desktop backend.
pub trait ProxyRecoveryAction {
    fn restore_proxy(&mut self, record: &ProxyRecoveryRecord) -> Result<(), PlatformFailure>;
    /// The owner token of the running daemon, written into the record after a
    /// successful restore-first so the current instance owns the side effect
    /// and its later `clear_if_owner` (disable / graceful shutdown) matches.
    fn current_owner_token(&self) -> [u8; 16];
}

/// Startup gate for the proxy side effect: re-apply a pending record before any
/// new core/platform mutation begins, then keep the record (phase `Applied`)
/// so the re-applied side effect stays undoable.
///
/// Clearing the record here used to orphan the engaged proxy: `sysproxy off`
/// and the graceful-shutdown restore both no-op on a missing record, so a
/// crash-recovered proxy could never be undone (and on shutdown the restored
/// TUN device leaked). The record still carries the true pre-engagement
/// capture; keeping it makes repeated boots idempotent (re-apply
/// `enabled=true`) and leaves every undo path working.
pub fn proxy_restore_first(
    store: &dyn ProxyRecoveryStore,
    action: &mut impl ProxyRecoveryAction,
) -> Result<ProxyRecoveryOutcome, ProxyRecoveryFailure> {
    let Some(mut record) = store.load().map_err(ProxyRecoveryFailure::Load)? else {
        return Ok(ProxyRecoveryOutcome::NothingPending);
    };
    record.phase = RecoveryPhase::Restoring;
    store
        .persist(&record)
        .map_err(ProxyRecoveryFailure::MarkRestoring)?;
    action
        .restore_proxy(&record)
        .map_err(ProxyRecoveryFailure::Restore)?;
    // Take ownership of the re-applied side effect: the record still carries
    // the true pre-engagement capture, but the owner token must be the
    // current instance's so its later clear (disable / graceful shutdown)
    // matches. Leaving the crashed instance's token behind would make every
    // clear a no-op and the record would survive forever, re-engaging the
    // proxy on every boot.
    record.owner_token = action.current_owner_token();
    record.phase = RecoveryPhase::Applied;
    store
        .persist(&record)
        .map_err(ProxyRecoveryFailure::ReapplyPersist)?;
    Ok(ProxyRecoveryOutcome::Restored)
}

/// Successful proxy recovery result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyRecoveryOutcome {
    NothingPending,
    Restored,
}

/// Proxy recovery failure retaining the exact failed durability boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyRecoveryFailure {
    Load(RecoveryStoreError),
    MarkRestoring(RecoveryStoreError),
    Restore(PlatformFailure),
    ReapplyPersist(RecoveryStoreError),
}

/// Re-applies a durable TUN side effect to the platform backend.
pub trait TunRecoveryAction {
    fn restore_tun(&mut self, record: &TunRecoveryRecord) -> Result<(), PlatformFailure>;
    /// The owner token of the running daemon, written into the record after a
    /// successful restore-first (see `ProxyRecoveryAction::current_owner_token`).
    fn current_owner_token(&self) -> [u8; 16];
}

/// Startup gate for the TUN side effect: re-engage a pending record before any
/// new mutation begins, then keep the record (phase `Applied`) so the
/// re-engaged device stays undoable (see `proxy_restore_first`).
pub fn tun_restore_first(
    store: &dyn TunRecoveryStore,
    action: &mut impl TunRecoveryAction,
) -> Result<ProxyRecoveryOutcome, TunRecoveryFailure> {
    let Some(mut record) = store.load().map_err(TunRecoveryFailure::Load)? else {
        return Ok(ProxyRecoveryOutcome::NothingPending);
    };
    record.phase = RecoveryPhase::Restoring;
    store
        .persist(&record)
        .map_err(TunRecoveryFailure::MarkRestoring)?;
    action
        .restore_tun(&record)
        .map_err(TunRecoveryFailure::Restore)?;
    // Take ownership of the re-engaged device (see `proxy_restore_first`).
    record.owner_token = action.current_owner_token();
    record.phase = RecoveryPhase::Applied;
    store
        .persist(&record)
        .map_err(TunRecoveryFailure::ReapplyPersist)?;
    Ok(ProxyRecoveryOutcome::Restored)
}

/// TUN recovery failure retaining the exact failed durability boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunRecoveryFailure {
    Load(RecoveryStoreError),
    MarkRestoring(RecoveryStoreError),
    Restore(PlatformFailure),
    ReapplyPersist(RecoveryStoreError),
}

#[cfg(test)]
mod tests {
    use super::store::RecoveryStore as _;
    use super::*;
    use std::sync::Mutex;

    struct MemStore {
        record: Mutex<Option<ProxyRecoveryRecord>>,
    }

    impl MemStore {
        fn new(record: Option<ProxyRecoveryRecord>) -> Self {
            Self {
                record: Mutex::new(record),
            }
        }
    }

    impl super::store::RecoveryStore<ProxyRecoveryRecord> for MemStore {
        fn load(&self) -> Result<Option<ProxyRecoveryRecord>, RecoveryStoreError> {
            self.record
                .lock()
                .map_err(|_| RecoveryStoreError::Read("poisoned".into()))
                .map(|guard| guard.clone())
        }
        fn persist(&self, record: &ProxyRecoveryRecord) -> Result<(), RecoveryStoreError> {
            *self
                .record
                .lock()
                .map_err(|_| RecoveryStoreError::Write("poisoned".into()))? = Some(record.clone());
            Ok(())
        }
        fn clear_if_owner(&self, owner_token: [u8; 16]) -> Result<(), RecoveryStoreError> {
            let mut guard = self
                .record
                .lock()
                .map_err(|_| RecoveryStoreError::Remove("poisoned".into()))?;
            if guard.as_ref().is_some_and(|r| r.owner_token == owner_token) {
                *guard = None;
            }
            Ok(())
        }
    }
    impl ProxyRecoveryStore for MemStore {}

    struct ActionRecorder {
        restored: Mutex<bool>,
        fail_restore: bool,
    }

    impl ProxyRecoveryAction for ActionRecorder {
        fn restore_proxy(&mut self, _record: &ProxyRecoveryRecord) -> Result<(), PlatformFailure> {
            if self.fail_restore {
                return Err(PlatformFailure {
                    operation: bounded_pl("restore"),
                    resource: bounded_pl("proxy"),
                    message: bounded_pl("failed"),
                    suggested_action: bounded_pl("retry"),
                });
            }
            *self.restored.lock().map_err(|_| PlatformFailure {
                operation: bounded_pl("lock"),
                resource: bounded_pl("proxy"),
                message: bounded_pl("poisoned"),
                suggested_action: bounded_pl("retry"),
            })? = true;
            Ok(())
        }

        fn current_owner_token(&self) -> [u8; 16] {
            [9; 16]
        }
    }

    fn bounded_pl<const MAX: usize>(value: &str) -> caly_domain::BoundedText<MAX> {
        caly_domain::BoundedText::new(value.to_owned()).unwrap()
    }

    fn pending_record(seed: u8) -> ProxyRecoveryRecord {
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

    #[test]
    fn proxy_restore_first_with_nothing_pending() -> Result<(), ProxyRecoveryFailure> {
        let store = MemStore::new(None);
        let mut action = ActionRecorder {
            restored: Mutex::new(false),
            fail_restore: false,
        };
        assert_eq!(
            proxy_restore_first(&store, &mut action)?,
            ProxyRecoveryOutcome::NothingPending
        );
        Ok(())
    }

    #[test]
    fn proxy_restore_first_reapplies_and_keeps_record() -> Result<(), ProxyRecoveryFailure> {
        let store = MemStore::new(Some(pending_record(1)));
        let mut action = ActionRecorder {
            restored: Mutex::new(false),
            fail_restore: false,
        };
        assert_eq!(
            proxy_restore_first(&store, &mut action)?,
            ProxyRecoveryOutcome::Restored
        );
        assert!(
            *action
                .restored
                .lock()
                .map_err(
                    |_| ProxyRecoveryFailure::ReapplyPersist(RecoveryStoreError::Read(
                        "poisoned".into()
                    ))
                )?
        );
        // The record must survive with phase `Applied` and the *current*
        // instance's owner token: the re-applied side effect stays undoable
        // (`sysproxy off` / graceful shutdown), and repeated boots re-apply
        // idempotently. Without the ownership takeover the crashed instance's
        // token would make every later clear a no-op forever.
        let kept = store
            .load()
            .map_err(ProxyRecoveryFailure::Load)?
            .expect("record must be kept after restore-first");
        assert_eq!(kept.phase, RecoveryPhase::Applied);
        assert_eq!(kept.owner_token, [9; 16]);
        Ok(())
    }

    #[test]
    fn proxy_restore_first_failed_restore_keeps_record() {
        let store = MemStore::new(Some(pending_record(2)));
        let mut action = ActionRecorder {
            restored: Mutex::new(false),
            fail_restore: true,
        };
        assert!(matches!(
            proxy_restore_first(&store, &mut action),
            Err(ProxyRecoveryFailure::Restore(_))
        ));
        // The pending record must remain so a later startup can retry.
        assert!(store.load().is_ok());
    }
}
