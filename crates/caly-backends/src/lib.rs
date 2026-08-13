//! Concrete application backends.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod capabilities;
pub mod config;
pub mod core;
pub mod dns_env;
pub mod dual;
pub mod lifecycle;
pub mod platform;
pub mod selection;
pub mod sing_box;
pub mod subscription;
pub mod telemetry;
pub mod template;

pub use capabilities::core_capability_set;
pub use config::MihomoConfigBackend;
pub use core::{
    CoreBackend, CoreNodeRegistry, CoreRoutingRegistry, MihomoCoreBackend, MihomoNodeRegistry,
    SingBoxCoreBackend, SubscriptionRouting, shared_routing_registry,
};
// P7:cell 定义上移 caly-ports;re-export 保持本 crate 对外根路径不动。
pub use caly_ports::SharedDesiredState;
pub use lifecycle::{CoreLifecycleBackend, MihomoLifecycleBackend, SharedCoreLifecycleBackend};
pub use platform::{LinuxSystemProxyBackend, LinuxTunCommandBackend};
pub use sing_box::SingBoxLifecycleBackend;
pub use subscription::{CachedSubscriptionBackend, HttpSubscriptionBackend};
pub use telemetry::{SharedObservedState, TelemetryBackend};
pub use template::{TemplateRenderBackend, TemplateRenderInput, default_worker_executable};

/// Builds an infrastructure `ActorFailure`, delegating to the shared clamped
/// constructor in `caly-ports` so a long runtime string can never abort.
pub(crate) fn failure(message: &str, action: &str) -> caly_ports::ActorFailure {
    caly_ports::ActorFailure::infrastructure(message, action)
}

/// Builds an `Unsupported` `ActorFailure` for capability gaps (desktop
/// without a system-proxy backend, PAC mode on a backend that does not
/// implement it, …). Distinct from `failure` so callers can distinguish
/// "this environment cannot do it" from "the operation failed".
pub(crate) fn unsupported_failure(message: &str, action: &str) -> caly_ports::ActorFailure {
    caly_ports::ActorFailure {
        kind: caly_ports::ActorFailureKind::Unsupported,
        message: caly_domain::BoundedText::from_nonempty_clamped(message.to_owned(), "unsupported"),
        suggested_action: caly_domain::BoundedText::from_nonempty_clamped(action.to_owned(), ""),
    }
}

#[cfg(test)]
mod tests;
