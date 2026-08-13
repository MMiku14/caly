//! Durable system-proxy backend: persists a recovery record on engagement and
//! restores it on startup, so a daemon crash never leaves the desktop proxy in
//! an unknown state.

use std::sync::Arc;

use caly_domain::PlatformEffectView;
use caly_platform::{
    recovery::{ProxyRecoveryAction, ProxyRecoveryRecord, ProxyRecoveryStore, RecoveryPhase},
    PlatformFailure,
};
use caly_ports::{ActorFailure, PlatformCommandBackend};

use super::{
    durable_support::{actor_to_platform, durable_failure},
    LinuxSystemProxyBackend,
};

/// Shared durable proxy recovery store handle.
pub type SharedProxyRecoveryStore = Arc<dyn ProxyRecoveryStore + Send + Sync>;

/// The desktop proxy operations needed for durable recovery.
pub trait DesktopProxyControl {
    fn set_system_proxy(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure>;
    /// PAC mode (auto + autoconfig-url). Defaults to an unsupported
    /// failure so backends opt in per desktop.
    fn set_system_proxy_pac(&mut self, _url: &str) -> Result<PlatformEffectView, ActorFailure> {
        Err(crate::unsupported_failure(
            "system proxy PAC mode is not supported on this desktop",
            "use GNOME for PAC mode, or pass a manual proxy endpoint",
        ))
    }
    fn host(&self) -> &str;
    fn port(&self) -> u16;
    /// Captures the desktop's current proxy state as `(mode, endpoint)` so a
    /// graceful shutdown can restore exactly what was found before engagement.
    fn capture_original_state(&mut self) -> (String, String);
    /// Restores a previously captured state; the durable wrapper degrades
    /// `manual` with an empty endpoint to a plain disable.
    fn restore_original(
        &mut self,
        mode: &str,
        endpoint: &str,
    ) -> Result<PlatformEffectView, ActorFailure>;
}

impl DesktopProxyControl for LinuxSystemProxyBackend {
    fn set_system_proxy(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        PlatformCommandBackend::set_system_proxy(self, enabled)
    }
    // Without this delegation the trait *default* (unsupported) would
    // win and GNOME PAC support would be unreachable through the
    // durable daemon path (2026-08-12 agent audit — a niri-hosted
    // functional test masked it because both paths error).
    fn set_system_proxy_pac(&mut self, url: &str) -> Result<PlatformEffectView, ActorFailure> {
        PlatformCommandBackend::set_system_proxy_pac(self, url)
    }
    fn host(&self) -> &str {
        self.host()
    }
    fn port(&self) -> u16 {
        self.port()
    }
    fn capture_original_state(&mut self) -> (String, String) {
        self.capture_original_state()
    }
    fn restore_original(
        &mut self,
        mode: &str,
        endpoint: &str,
    ) -> Result<PlatformEffectView, ActorFailure> {
        self.restore_original(mode, endpoint)
    }
}

/// Wraps a desktop proxy control with an owner-only durable recovery store.
pub struct DurableSystemProxyBackend<C> {
    inner: C,
    store: SharedProxyRecoveryStore,
    owner_token: [u8; 16],
}

impl<C: DesktopProxyControl> DurableSystemProxyBackend<C> {
    /// Wraps `inner` with `store` and a fresh owner token.
    pub fn new(inner: C, store: SharedProxyRecoveryStore) -> Self {
        let owner_token = caly_platform::entropy::random_bytes::<16>();
        Self {
            inner,
            store,
            owner_token,
        }
    }

    /// Re-applies any pending proxy recovery record (restore-first startup gate).
    pub fn restore_first(
        &mut self,
    ) -> Result<
        caly_platform::recovery::ProxyRecoveryOutcome,
        caly_platform::recovery::ProxyRecoveryFailure,
    > {
        let store = Arc::clone(&self.store);
        caly_platform::recovery::proxy_restore_first(store.as_ref(), self)
    }

    /// Persists a recovery record for an enabled proxy side effect, capturing
    /// the desktop state found before engagement. Capture is best-effort: an
    /// unreadable state degrades to `("none", "")` (restore = disable) and
    /// never blocks engagement. `pac_url` is non-empty when the side effect
    /// is PAC mode; crash recovery then replays PAC instead of the manual
    /// endpoint (2026-08-12).
    fn persist_enabled_record(&mut self, pac_url: &str) -> Result<(), ActorFailure> {
        let (original_mode, original_endpoint) = self.inner.capture_original_state();
        let record = ProxyRecoveryRecord {
            owner_token: self.owner_token,
            phase: RecoveryPhase::Applied,
            host: self.inner.host().to_owned(),
            port: self.inner.port(),
            enabled: true,
            original_mode,
            original_endpoint,
            pac_url: pac_url.to_owned(),
        };
        self.store
            .persist(&record)
            .map_err(|e| durable_failure(e, "proxy"))?;
        Ok(())
    }

    /// Clears the recovery record when the side effect is no longer pending.
    fn clear_record(&mut self) {
        // A failed clear leaves the record behind; restore-first on the next
        // boot then re-applies the proxy. That is safe (idempotent) but
        // undesired after a successful disable — surface it instead of
        // swallowing it silently.
        if let Err(error) = self.store.clear_if_owner(self.owner_token) {
            tracing::warn!(
                error = ?error,
                "could not clear the durable system-proxy record; restore-first \
                 will re-apply it on the next daemon boot"
            );
        }
    }
}

impl<C: DesktopProxyControl> PlatformCommandBackend for DurableSystemProxyBackend<C> {
    fn set_system_proxy_pac(&mut self, url: &str) -> Result<PlatformEffectView, ActorFailure> {
        // PAC mode engages the system proxy like the manual mode: the
        // durable record is persisted the same way so a crash between
        // apply and projection stays recoverable.
        let existing = self
            .store
            .load()
            .map_err(|e| durable_failure(e, "proxy"))?
            .is_some();
        if !existing {
            // The PAC URL must reach the record: crash recovery replays
            // PAC mode via it, and an empty value would silently fall
            // back to the manual endpoint (2026-08-12 agent audit —
            // the parameter was dropped by an earlier mechanical edit).
            self.persist_enabled_record(url)?;
        }
        match self.inner.set_system_proxy_pac(url) {
            Ok(view) => Ok(view),
            Err(error) => {
                if !existing {
                    self.clear_record();
                }
                Err(error)
            }
        }
    }

    fn set_system_proxy(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        if enabled {
            // Persist the intent durably before applying, so a crash between
            // apply and the projection still leaves a recoverable record. A
            // re-engagement keeps the existing record: re-capturing would
            // overwrite the user's true original state with caly's own proxy.
            let existing = self
                .store
                .load()
                .map_err(|e| durable_failure(e, "proxy"))?
                .is_some();
            if !existing {
                self.persist_enabled_record("")?;
            }
            match self.inner.set_system_proxy(true) {
                Ok(view) => Ok(view),
                Err(error) => {
                    if !existing {
                        self.clear_record();
                    }
                    Err(error)
                }
            }
        } else {
            // `sysproxy off` restores the desktop state captured before
            // engagement (the documented disable contract), then clears the
            // record. A failed restore retains the record so restore-first on
            // a later daemon boot can retry; a missing record is a no-op
            // because this daemon never engaged the proxy — touching the
            // desktop could clobber a proxy the user configured.
            self.restore_recorded_state()
        }
    }
}

impl<C: DesktopProxyControl> DurableSystemProxyBackend<C> {
    /// Graceful-shutdown restore: returns the desktop to the exact state
    /// captured before engagement, then clears the record. A failed restore
    /// retains the record so restore-first on the next boot can retry; a
    /// missing record is a no-op success.
    pub fn restore_original_and_clear(&mut self) -> Result<PlatformEffectView, ActorFailure> {
        self.restore_recorded_state()
    }

    /// Restores the desktop to the captured pre-engagement state and clears
    /// the record. Shared by `sysproxy off` and the graceful-shutdown restore
    /// so both "undo" paths behave identically: a failed restore retains the
    /// record (restore-first on a later daemon boot retries), a missing
    /// record is a no-op success.
    fn restore_recorded_state(&mut self) -> Result<PlatformEffectView, ActorFailure> {
        let Some(record) = self.store.load().map_err(|e| durable_failure(e, "proxy"))? else {
            // No record means this daemon never engaged the proxy: touching
            // the desktop here could clobber a proxy the user configured.
            return Ok(PlatformEffectView::none());
        };
        match self
            .inner
            .restore_original(&record.original_mode, &record.original_endpoint)
        {
            Ok(view) => {
                self.clear_record();
                Ok(view)
            }
            Err(error) => Err(error),
        }
    }
}

impl<C: DesktopProxyControl> ProxyRecoveryAction for DurableSystemProxyBackend<C> {
    fn restore_proxy(&mut self, record: &ProxyRecoveryRecord) -> Result<(), PlatformFailure> {
        // A PAC-mode record replays PAC (auto + autoconfig-url); the
        // manual-endpoint fallback only handles non-PAC records. Before
        // the `pac_url` field existed every record was manual, so legacy
        // records keep the old behaviour.
        let outcome = if record.pac_url.is_empty() {
            self.inner.set_system_proxy(record.enabled)
        } else {
            self.inner.set_system_proxy_pac(&record.pac_url)
        };
        outcome
            .map(|_| ())
            .map_err(|e| actor_to_platform(e, "restore-proxy", "system-proxy"))
    }

    fn current_owner_token(&self) -> [u8; 16] {
        self.owner_token
    }
}

#[cfg(test)]
mod durable_tests;
