//! Shared coordination cells: the mutex-protected owners crossed between
//! application actors and composition wiring. Defined at port level (P7) so
//! `caly-application` handlers and the `caly-composition` root can both hold
//! them without the use-case crate naming concrete backend adapters.

use std::sync::{Arc, Mutex};

use caly_domain::{CoreKind, DesiredState};

/// Shares the current desired (user intent) state. The projection is the
/// authoritative store; this cell lets the command owner read the latest
/// desired values (e.g. preserve selected node) before building a delta.
#[derive(Clone)]
pub struct SharedDesiredState(pub Arc<Mutex<DesiredState>>);

impl Default for SharedDesiredState {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(DesiredState::new(
            caly_domain::ProxyMode::Rule,
            None,
            None,
            false,
            false,
        ))))
    }
}

impl SharedDesiredState {
    /// Returns a snapshot of the current desired state, or the default if the
    /// cell is poisoned.
    pub fn value(&self) -> DesiredState {
        match self.0.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => DesiredState::new(caly_domain::ProxyMode::Rule, None, None, false, false),
        }
    }

    /// Replaces the desired state and returns the new value.
    pub fn replace(&self, state: DesiredState) -> DesiredState {
        if let Ok(mut guard) = self.0.lock() {
            *guard = state.clone();
        }
        state
    }
}

/// Shared, cloneable active-core cell consulted by the lifecycle, command and
/// telemetry adapters so a switch is observed atomically everywhere.
pub type SharedActiveCore = Arc<Mutex<CoreKind>>;
