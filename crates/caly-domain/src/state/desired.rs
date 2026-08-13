//! Persisted user intent.

use crate::{NodeId, SubscriptionId};

/// User-selected proxy routing mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyMode {
    /// Apply configured routing rules.
    Rule,
    /// Route all supported traffic through the selected proxy.
    Global,
    /// Bypass proxy routing.
    Direct,
}

/// Pure projection of persisted user intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredState {
    mode: ProxyMode,
    selected_node: Option<NodeId>,
    active_subscription: Option<SubscriptionId>,
    tun_requested: bool,
    system_proxy_requested: bool,
}

impl DesiredState {
    /// Constructs a validated desired-state projection.
    pub const fn new(
        mode: ProxyMode,
        selected_node: Option<NodeId>,
        active_subscription: Option<SubscriptionId>,
        tun_requested: bool,
        system_proxy_requested: bool,
    ) -> Self {
        Self {
            mode,
            selected_node,
            active_subscription,
            tun_requested,
            system_proxy_requested,
        }
    }

    /// Returns the requested routing mode.
    pub const fn mode(&self) -> ProxyMode {
        self.mode
    }
    /// Returns a new desired state with only the routing mode changed.
    #[must_use]
    pub const fn with_mode(mut self, mode: ProxyMode) -> Self {
        self.mode = mode;
        self
    }

    /// Returns a copy with the system-proxy request flag replaced.
    #[must_use]
    pub const fn with_system_proxy(mut self, requested: bool) -> Self {
        self.system_proxy_requested = requested;
        self
    }

    /// Returns a copy with the TUN request flag replaced.
    #[must_use]
    pub const fn with_tun_requested(mut self, requested: bool) -> Self {
        self.tun_requested = requested;
        self
    }
    /// Returns the stable node selection requested by the user.
    pub const fn selected_node(&self) -> Option<NodeId> {
        self.selected_node
    }
    /// Returns the active subscription requested by the user.
    pub const fn active_subscription(&self) -> Option<SubscriptionId> {
        self.active_subscription
    }
    /// Returns whether TUN was requested, not whether it is active.
    pub const fn tun_requested(&self) -> bool {
        self.tun_requested
    }
    /// Returns whether system proxy engagement was requested.
    pub const fn system_proxy_requested(&self) -> bool {
        self.system_proxy_requested
    }
}
