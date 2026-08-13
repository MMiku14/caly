//! Admission-time command support policy.
//!
//! A wire command must be rejected before an Operation is reserved when the
//! current daemon composition has no complete owner/terminal-result path for
//! it. This prevents contract-only commands from killing an actor and leaving
//! a permanently Running operation.

use caly_domain::CoreKind;

use crate::command_bus::Command;

use super::ApplicationServiceError;

/// Command surface exposed by one concrete application composition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandSupportPolicy {
    /// Used by isolated service tests and embedders that supply every owner.
    #[default]
    All,
    /// Current daemon MVP: only lifecycle operations for the selected core.
    LifecycleOnly { core: CoreKind },
    /// Lifecycle for the selected core plus the fully-wired platform and
    /// connection commands (`SetSystemProxy`, `CloseAllConnections`). Commands
    /// whose owners or data flows are not complete remain fail-fast.
    LifecycleAndPlatform { core: CoreKind },
}

impl CommandSupportPolicy {
    /// Creates the safe public policy for the current daemon MVP.
    pub const fn lifecycle_only(core: CoreKind) -> Self {
        Self::LifecycleOnly { core }
    }

    /// Creates the daemon policy that also exposes wired platform commands.
    pub const fn lifecycle_and_platform(core: CoreKind) -> Self {
        Self::LifecycleAndPlatform { core }
    }

    /// Rejects unsupported work before operation reservation and queueing.
    pub fn validate(self, command: Command) -> Result<(), ApplicationServiceError> {
        match self {
            Self::All => Ok(()),
            Self::LifecycleOnly { core: _ } => match command {
                // Both managed kernels are pre-built, so switching targets is
                // always admissible; `core` remains the startup preference.
                Command::SwitchCore {
                    target: CoreKind::Mihomo | CoreKind::SingBox,
                    ..
                } => Ok(()),
                Command::SwitchCore { .. } => Err(invalid_command(
                    "requested core backend is not managed by this daemon",
                )),
                // Round 17: `StopDaemon` and `ReloadConfig` are
                // always admissible (they're daemon-lifecycle
                // operations, not kernel-specific).
                Command::StopDaemon | Command::ReloadConfig => Ok(()),
                Command::ApplyConfig { .. }
                | Command::SelectProxy { .. }
                | Command::SelectProxyGroup { .. }
                | Command::SetMode { .. }
                | Command::SetTun { .. }
                | Command::SetSystemProxy { .. }
                | Command::SetSystemProxyPac { .. }
                | Command::RefreshSubscription { .. }
                | Command::CloseAllConnections => Err(invalid_command(
                    "command is not wired into the current daemon runtime",
                )),
            },
            Self::LifecycleAndPlatform { core: _ } => match command {
                // Both managed kernels are pre-built; switching targets is
                // always admissible. `core` remains the startup preference.
                Command::SwitchCore {
                    target: CoreKind::Mihomo | CoreKind::SingBox,
                    ..
                } => Ok(()),
                Command::SwitchCore { .. } => Err(invalid_command(
                    "requested core backend is not managed by this daemon",
                )),
                Command::SetSystemProxy { .. }
                | Command::SetSystemProxyPac { .. }
                | Command::CloseAllConnections
                | Command::SetTun { .. } => Ok(()),
                // Round 17: `StopDaemon` and `ReloadConfig`
                // are always admissible (daemon-lifecycle
                // operations).
                Command::StopDaemon | Command::ReloadConfig => Ok(()),
                Command::SelectProxy { .. }
                | Command::SelectProxyGroup { .. }
                | Command::RefreshSubscription { .. }
                | Command::SetMode { .. }
                | Command::ApplyConfig { .. } => Ok(()),
            },
        }
    }
}

fn invalid_command(reason: &'static str) -> ApplicationServiceError {
    // The input is a `&'static str` literal well under the 512-byte bound,
    // so the bounded constructor cannot fail. The previous
    // `unwrap_or_else(|_| abort)` form was a process-kill fallback for an
    // unreachable path; the infallible `from_nonempty_clamped` keeps the
    // same behaviour for the well-formed call sites and surfaces a stable
    // `"_"` fallback for any future refactor that accidentally widens the
    // input.
    let reason =
        caly_domain::BoundedText::from_nonempty_clamped(reason.to_owned(), "invalid command");
    ApplicationServiceError::InvalidCommand(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_bus::CoreAction;

    #[test]
    fn lifecycle_policy_accepts_both_managed_cores() {
        let policy = CommandSupportPolicy::lifecycle_only(CoreKind::Mihomo);
        for target in [CoreKind::Mihomo, CoreKind::SingBox] {
            assert!(
                policy
                    .validate(Command::SwitchCore {
                        target,
                        action: CoreAction::Restart,
                    })
                    .is_ok()
            );
        }
        assert!(matches!(
            policy.validate(Command::SwitchCore {
                target: CoreKind::Xray,
                action: CoreAction::Restart,
            }),
            Err(ApplicationServiceError::InvalidCommand(_))
        ));
    }

    #[test]
    fn lifecycle_policy_rejects_contract_only_commands() {
        let policy = CommandSupportPolicy::lifecycle_only(CoreKind::Mihomo);
        assert!(matches!(
            policy.validate(Command::SetTun { enabled: true }),
            Err(ApplicationServiceError::InvalidCommand(_))
        ));
    }

    #[test]
    fn platform_policy_admits_wired_commands() {
        let policy = CommandSupportPolicy::lifecycle_and_platform(CoreKind::Mihomo);
        assert!(
            policy
                .validate(Command::SwitchCore {
                    target: CoreKind::Mihomo,
                    action: CoreAction::Restart,
                })
                .is_ok()
        );
        assert!(
            policy
                .validate(Command::SetSystemProxy { enabled: true })
                .is_ok()
        );
        assert!(policy.validate(Command::CloseAllConnections).is_ok());
    }

    #[test]
    fn platform_policy_admits_proxy_selection_and_subscription_refresh() {
        let policy = CommandSupportPolicy::lifecycle_and_platform(CoreKind::Mihomo);
        assert!(
            policy
                .validate(Command::SelectProxy {
                    node_id: caly_domain::NodeId::from_bytes([1; 16]),
                })
                .is_ok()
        );
        assert!(
            policy
                .validate(Command::RefreshSubscription {
                    subscription_id: caly_domain::SubscriptionId::from_bytes([2; 16]),
                    force: false,
                    scheduled: false,
                })
                .is_ok()
        );
        assert!(
            policy
                .validate(Command::SetMode {
                    mode: caly_domain::ProxyMode::Global
                })
                .is_ok()
        );
        assert!(
            policy
                .validate(Command::ApplyConfig {
                    candidate_id: [3; 16]
                })
                .is_ok()
        );
    }

    #[test]
    fn platform_policy_admits_set_tun_and_dual_core_switch() {
        let policy = CommandSupportPolicy::lifecycle_and_platform(CoreKind::Mihomo);
        assert!(policy.validate(Command::SetTun { enabled: true }).is_ok());
        assert!(
            policy
                .validate(Command::SwitchCore {
                    target: CoreKind::SingBox,
                    action: CoreAction::Start,
                })
                .is_ok()
        );
        assert!(matches!(
            policy.validate(Command::SwitchCore {
                target: CoreKind::Xray,
                action: CoreAction::Start,
            }),
            Err(ApplicationServiceError::InvalidCommand(_))
        ));
    }
}
