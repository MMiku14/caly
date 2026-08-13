//! Additional runtime settings sections of the application configuration.
//!
//! Kept in a sibling file so `schema/mod.rs` stays within the project's
//! file-size limit. These sections let the config file drive controller
//! endpoints, log verbosity and telemetry cadence.
//!
//! This module is also the single source of truth for generated default
//! configs: `config generate` writes `render_default_base()` plus
//! `render_default_config_files()`, and `render_default_config()` composes the
//! same pieces into one documented file, so the layouts cannot drift apart.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
mod defaults;
mod rule_providers;
mod subscriptions;

pub use defaults::{render_default_base, render_default_config, render_default_config_files};
pub use rule_providers::{
    RuleProviderBehaviorConfig, RuleProviderConfig, RuleProviderFormatConfig,
    RuleProviderSourceConfig,
};
pub use subscriptions::{ProviderConfig, ProviderKind, SubscriptionConfig, SubscriptionSource};

/// Per-core Clash-compatible controller endpoints.
///
/// Used by the daemon's command/telemetry control clients and the subscription
/// refresh path. The environment variables `CALY_MIHOMO_CONTROLLER` /
/// `CALY_SINGBOX_CONTROLLER` still override the config values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControllersConfig {
    /// Mihomo `external-controller` address (host:port).
    pub mihomo: String,
    /// sing-box `external_controller` address (host:port).
    pub sing_box: String,
}

impl Default for ControllersConfig {
    fn default() -> Self {
        Self {
            mihomo: "127.0.0.1:9090".to_owned(),
            sing_box: "127.0.0.1:9091".to_owned(),
        }
    }
}

/// Optional executable locations for managed proxy cores.
///
/// Paths are intentionally not required to exist while the configuration is
/// parsed: packages may install a config before the binary. The lifecycle
/// owner validates existence and executability immediately before start.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CoreBinariesConfig {
    /// Absolute or relative path to the Mihomo executable.
    pub mihomo: Option<PathBuf>,
    /// Absolute or relative path to the sing-box executable.
    pub sing_box: Option<PathBuf>,
}

/// Daemon logging verbosity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// One of `error`, `warn`, `info`, `debug`, `trace`.
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
        }
    }
}

/// Telemetry sampling cadence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Sampling interval in milliseconds (>= 100).
    pub interval_ms: u64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { interval_ms: 1_000 }
    }
}

#[cfg(test)]
mod tests;
