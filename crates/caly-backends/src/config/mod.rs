//! ConfigActor backends: Mihomo and sing-box config rendering, real-kernel
//! validation and atomic generation publication (see `mod.rs` for the shared
//! transaction shape and `sing_box.rs` for the sing-box twin).
use caly_platform::paths::{AppPaths, SafeName};
use caly_ports::{ActorFailure, CommittedConfig, ConfigActorPort, ConfigCandidate, PreparedConfig};
use caly_profile::loader::{LayeredConfigPaths, LoaderLimits, load_layered_yaml_strict};

use super::failure;

/// Maximum committed generations retained in memory for rollback; older
/// generations are dropped (the on-disk current generation stays authoritative).
pub const MAX_CONFIG_HISTORY: usize = 2;

/// Maps the schema TUN settings (layered config) to the domain TUN intent,
/// or `None` when TUN is disabled. Shared by the daemon boot resolution and
/// the per-apply re-read so both stay in sync.
pub fn tun_config_from_schema(
    config: &caly_profile::schema::AppConfig,
) -> Option<caly_domain::TunConfig> {
    if !config.tun.enabled {
        return None;
    }
    let stack = match config.tun.stack {
        caly_profile::schema::TunStack::Gvisor => caly_domain::TunStack::Gvisor,
        caly_profile::schema::TunStack::Mixed => caly_domain::TunStack::Mixed,
        caly_profile::schema::TunStack::System => caly_domain::TunStack::System,
    };
    caly_domain::TunConfig::new(
        stack,
        config.tun.auto_route,
        config.tun.strict_route,
        config.tun.mtu,
    )
    .ok()
}

/// Re-reads the TUN tuning from the layered config (env profile + config.d),
/// mirroring the subscription-URL re-read on refresh. A `tun.enabled` edit
/// therefore reaches the next `config apply` without a daemon restart.
pub fn resolve_tun_from_config() -> Option<caly_domain::TunConfig> {
    let paths_env = AppPaths::from_env();
    let root = paths_env.config.clone();
    if !root.join("config.yaml").is_file() {
        return None;
    }
    let profile = std::env::var("CALY_PROFILE")
        .ok()
        .and_then(|value| SafeName::new(value).ok());
    let paths = LayeredConfigPaths::new(root, profile);
    // Strict ProfileStore resolution: an apply-side re-read must resolve
    // declared profiles exactly like the daemon boot did.
    load_layered_yaml_strict(&paths, LoaderLimits::secure_default(), &paths_env.state)
        .ok()
        .as_ref()
        .and_then(tun_config_from_schema)
}

mod mihomo_backend;
mod mihomo_port;

pub use mihomo_backend::MihomoConfigBackend;

#[cfg(test)]
mod config_tests;

pub mod sing_box;

/// Active config backend dispatcher: renders/commits through the backend
/// matching the RUNTIME active core (the shared cell consulted by the lifecycle
/// switch), so `config apply` follows a `core switch` — a static configured
/// core would keep publishing the pre-switch kernel's file.
pub struct ActiveConfigBackend {
    active: crate::dual::SharedActiveCore,
    mihomo: MihomoConfigBackend,
    sing_box: sing_box::SingBoxConfigBackend,
}

impl ActiveConfigBackend {
    /// Wraps both backends behind the shared active-core cell.
    pub fn new(
        active: crate::dual::SharedActiveCore,
        mihomo: MihomoConfigBackend,
        sing_box: sing_box::SingBoxConfigBackend,
    ) -> Self {
        Self {
            active,
            mihomo,
            sing_box,
        }
    }

    /// The currently active core (poisoned cell degrades to Mihomo, matching
    /// the dual command backend's fallback).
    fn active_kind(&self) -> caly_domain::CoreKind {
        self.active
            .lock()
            .map_or(caly_domain::CoreKind::Mihomo, |active| *active)
    }
}

impl ConfigActorPort for ActiveConfigBackend {
    fn parse_and_render(
        &mut self,
        candidate: ConfigCandidate,
    ) -> Result<PreparedConfig, ActorFailure> {
        match self.active_kind() {
            caly_domain::CoreKind::Mihomo | caly_domain::CoreKind::Xray => {
                self.mihomo.parse_and_render(candidate)
            }
            caly_domain::CoreKind::SingBox => self.sing_box.parse_and_render(candidate),
        }
    }

    fn discard_prepared(&mut self, prepared: PreparedConfig) -> Result<(), ActorFailure> {
        match self.active_kind() {
            caly_domain::CoreKind::Mihomo | caly_domain::CoreKind::Xray => {
                self.mihomo.discard_prepared(prepared)
            }
            caly_domain::CoreKind::SingBox => self.sing_box.discard_prepared(prepared),
        }
    }

    fn commit_candidate(
        &mut self,
        prepared: PreparedConfig,
    ) -> Result<CommittedConfig, ActorFailure> {
        match self.active_kind() {
            caly_domain::CoreKind::Mihomo | caly_domain::CoreKind::Xray => {
                self.mihomo.commit_candidate(prepared)
            }
            caly_domain::CoreKind::SingBox => self.sing_box.commit_candidate(prepared),
        }
    }

    fn rollback_commit(&mut self, committed: CommittedConfig) -> Result<(), ActorFailure> {
        match self.active_kind() {
            caly_domain::CoreKind::Mihomo | caly_domain::CoreKind::Xray => {
                self.mihomo.rollback_commit(committed)
            }
            caly_domain::CoreKind::SingBox => self.sing_box.rollback_commit(committed),
        }
    }

    /// Returns the most recently committed config text (for a hot reload,
    /// 刀 5) or `None` when nothing was committed yet.
    fn current_contents(&self) -> Option<Vec<u8>> {
        match self.active_kind() {
            caly_domain::CoreKind::Mihomo | caly_domain::CoreKind::Xray => {
                self.mihomo.current_contents()
            }
            caly_domain::CoreKind::SingBox => self.sing_box.current_contents(),
        }
    }
}
