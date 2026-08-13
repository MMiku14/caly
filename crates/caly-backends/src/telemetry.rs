//! TelemetryActor backend.

use std::sync::{Arc, Mutex};

use caly_domain::ObservedState;

use caly_ports::{ActorFailure, TelemetryCommandBackend};

/// Shares the current observed runtime state so telemetry sampling and the
/// projection read the same owner. Sampling advances generation accounting;
/// real per-second byte/connection deltas arrive from the kernel API.
#[derive(Clone, Default)]
pub struct SharedObservedState(pub Arc<Mutex<ObservedState>>);

impl SharedObservedState {
    /// Returns a snapshot of the current observed state. Audit #105: a
    /// poisoned cell used to report a zeroed default (observed counters blink
    /// to 0 after any writer panic); recovering the guard keeps the last
    /// coherent values instead — poison remains surfaced by the runtime as a
    /// fault.
    pub fn value(&self) -> ObservedState {
        self.0
            .lock()
            .map_or_else(|poison| *poison.into_inner(), |guard| *guard)
    }
}

/// Telemetry backend that owns the observed-state cell used by sampling.
///
/// The attached kernel control is a boxed `KernelControl` (Mihomo or sing-box),
/// selected by the active core; sampling reads live Clash-compatible API data.
pub struct TelemetryBackend {
    observed: SharedObservedState,
    generation: u64,
    control: Option<Box<dyn caly_corectl::contract::KernelControl + Send>>,
}

impl TelemetryBackend {
    pub fn new(observed: SharedObservedState) -> Self {
        Self {
            observed,
            generation: 0,
            control: None,
        }
    }

    /// Attaches a kernel control (Mihomo or sing-box) so sampling reads live
    /// Clash API data.
    #[must_use]
    pub fn with_controller(
        mut self,
        control: Box<dyn caly_corectl::contract::KernelControl + Send>,
    ) -> Self {
        self.control = Some(control);
        self
    }

    pub const fn observed(&self) -> &SharedObservedState {
        &self.observed
    }
}

impl TelemetryCommandBackend for TelemetryBackend {
    fn sample(&mut self) -> Result<(), ActorFailure> {
        // When a core backend is attached, sample live Clash API data into the
        // observed cell; otherwise keep the cell authoritative as-is. The
        // controller round-trips (up to 2s each) run BEFORE the mutex is
        // taken so a slow kernel never stalls every `show status` reader
        // behind the observed-state lock.
        let Some(control) = &mut self.control else {
            return Ok(());
        };
        let timeout = std::time::Duration::from_secs(2);
        let connections = control.connections(timeout).unwrap_or_default();
        // `traffic` is a per-second rate (the Clash /traffic up/down
        // snapshot), which is exactly what the ObservedState per-second
        // fields store — no delta computation is required or wanted here.
        let (download_per_second, upload_per_second) = control.traffic(timeout).unwrap_or((0, 0));
        let mut observed = self.observed.0.lock().map_err(|_| {
            crate::failure("telemetry observed state poisoned", "restart the runtime")
        })?;
        *observed = ObservedState::with_self_heal(
            upload_per_second,
            download_per_second,
            connections.active,
            observed.telemetry_dropped(),
            observed.core_restart_count(),
            observed.core_restart_backoff_ms(),
        );
        Ok(())
    }

    fn record_dropped(&mut self, count: u64) -> Result<(), ActorFailure> {
        let mut observed = self.observed.0.lock().map_err(|_| {
            crate::failure("telemetry observed state poisoned", "restart the runtime")
        })?;
        let dropped = observed.telemetry_dropped().saturating_add(count);
        // Preserve the self-heal counters: rebuilding via `ObservedState::new`
        // used to reset core_restart_count/backoff to zero on every drop,
        // letting dropped-sample bursts fake a healthy core.
        *observed = ObservedState::with_self_heal(
            observed.upload_bytes_per_second(),
            observed.download_bytes_per_second(),
            observed.active_connections(),
            dropped,
            observed.core_restart_count(),
            observed.core_restart_backoff_ms(),
        );
        Ok(())
    }

    fn reset_generation(&mut self, generation: u64) -> Result<(), ActorFailure> {
        self.generation = generation;
        Ok(())
    }
}
