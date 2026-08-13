//! CoreActor command port: proxy selection and connection close.

use caly_domain::{AppliedState, NodeId, ObservedState};

use super::error::ActorFailure;

/// Nonblocking CoreActor backend; process/API I/O belongs to its owned worker.
pub trait CoreCommandBackend {
    fn select_proxy(&mut self, node: NodeId) -> Result<AppliedState, ActorFailure>;
    /// W4 (`node pick`): select a named member inside a named proxy
    /// group. Implementations resolve kernel-side member tags (Mihomo
    /// display names vs sing-box `proxy-<hex>` tags); a kernel without
    /// group selection returns an `Unsupported` failure.
    fn select_proxy_group(
        &mut self,
        group: &str,
        member: &str,
    ) -> Result<AppliedState, ActorFailure>;
    fn close_all_connections(&mut self) -> Result<ObservedState, ActorFailure>;
    /// Applies a routing mode to the running core and returns the new applied state.
    fn set_mode(&mut self, mode: caly_domain::ProxyMode) -> Result<AppliedState, ActorFailure>;
    /// W3b: the kernel's proxy groups — kind, kernel-side membership
    /// (`all`), and the current selection (`now`). The default returns an
    /// empty slice; kernels with group listing (mihomo `/proxies`) override
    /// so the CLI can render group rows and the GROUP column on the online
    /// node list. A listing failure degrades to an empty slice — the
    /// declared group structure stays authoritative.
    fn list_proxy_groups(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Vec<caly_domain::ProxyGroupView>, ActorFailure> {
        let _ = timeout;
        Ok(Vec::new())
    }
}
