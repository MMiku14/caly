//! Shared kernel descriptors and process-aware readiness polling.

use std::time::Duration;

use caly_domain::{
    BoundedText, BoundedVec, Capability, CapabilitySet, CapabilityStatus, ConfiguredSupport,
    CoreKind, RuntimeAvailability,
};
use caly_platform::process::{OwnedProcessTree, ProcessSpawner};

use crate::contract::{KernelControl, KernelFailure, KernelFailureKind};

/// Static binary identity separated from probed runtime health.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelDescriptor {
    pub kind: CoreKind,
    pub binary_name: &'static str,
    pub runtime_api: RuntimeAvailability,
}

/// Returns static binary identity without claiming runtime API health.
pub const fn descriptor(kind: CoreKind) -> KernelDescriptor {
    let binary_name = match kind {
        CoreKind::Mihomo => "mihomo",
        CoreKind::SingBox => "sing-box",
        CoreKind::Xray => "xray",
    };
    KernelDescriptor {
        kind,
        binary_name,
        runtime_api: RuntimeAvailability::Unknown,
    }
}

/// Polls for controller readiness, failing fast if the child process exits
/// (e.g. a missing/unsupported binary) instead of waiting out the full timeout.
pub fn wait_ready_detecting_exit<S, C>(
    tree: &mut S::Tree,
    control: &mut C,
    timeout: Duration,
    core: &str,
) -> Result<(), KernelFailure>
where
    S: ProcessSpawner,
    S::Tree: caly_platform::process::OwnedProcessTree,
    C: KernelControl,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match tree.poll_exit() {
            Ok(Some(_)) => {
                let stderr = tree.stderr_tail().trim().to_owned();
                // A stuck child sometimes writes the real cause to stderr before
                // dying (e.g. "set routes: add route … file exists" for a TUN
                // route conflict). Surface it so the hint points at the truth.
                let (message, action) = if stderr.is_empty() {
                    (
                        format!("{core} process exited before becoming ready"),
                        format!("check the {core} binary and configuration"),
                    )
                } else {
                    (
                        format!(
                            "{core} process exited before becoming ready (child stderr: {stderr})"
                        ),
                        format!("inspect the {core} stderr and rerun the check"),
                    )
                };
                return Err(failure(&message, &action));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(KernelFailure {
                    kind: KernelFailureKind::ProcessLifecycle,
                    message: bounded(&format!("{core} exit poll failed")),
                    suggested_action: bounded(&format!("inspect the {core} process owner")),
                    platform: Some(error),
                });
            }
        }
        if control.health_check(Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(failure(
                &format!("{core} controller did not become ready"),
                &format!("inspect {core} readiness and controller binding"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Shared process-lifecycle failure helper, used by the per-kernel runtimes and
/// by the readiness probe so all lifecycle errors flow through one constructor.
pub(crate) fn failure(message: &str, action: &str) -> KernelFailure {
    KernelFailure::new(KernelFailureKind::ProcessLifecycle, message, action)
}

/// Constructs a kernel failure with an explicit kind: the single shared
/// constructor all kernel modules collapse onto.
pub(crate) fn failure_with_kind(
    kind: KernelFailureKind,
    message: &str,
    action: &str,
) -> KernelFailure {
    KernelFailure::new(kind, message, action)
}

/// Configuration/rendering failures (kind fixed to `InvalidConfig`).
pub(crate) fn config_failure(message: &str, action: &str) -> KernelFailure {
    KernelFailure::new(KernelFailureKind::InvalidConfig, message, action)
}

/// Rejects a missing or non-executable core binary with a clear diagnostic,
/// checked at core start so the runtime backends can still be built (and the
/// daemon can boot into a stopped-core state) when the binary is unavailable.
pub(crate) fn ensure_executable(
    binary: &std::path::Path,
    label: &str,
) -> Result<(), KernelFailure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = std::fs::metadata(binary)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        if !executable {
            let location = if binary.is_relative() {
                format!(
                    "{} (relative to the current working directory)",
                    binary.display()
                )
            } else {
                binary.display().to_string()
            };
            return Err(failure(
                &format!("{label} binary is missing or not executable: {location}"),
                "install the core binary, chmod +x it, set the CALY_*_BIN override, run scripts/fetch-test-kernels.sh, or launch caly from the repo root",
            ));
        }
    }
    #[cfg(not(unix))]
    {
        if !binary.is_file() {
            return Err(failure(
                &format!("{label} binary is missing: {}", binary.display()),
                "install the core binary or set the CALY_*_BIN override",
            ));
        }
    }
    Ok(())
}

/// Builds one capability status for a kernel whose per-capability support is
/// known at build time (no runtime negotiation happens for these).
pub(crate) fn capability_status(
    capability: Capability,
    support: ConfiguredSupport,
) -> CapabilityStatus {
    CapabilityStatus::new(capability, support, RuntimeAvailability::NotRequired, None)
}

/// Collects statically-known capability statuses into a set. The hardcoded
/// literal always fits the bounded capacity and contains no duplicate
/// `Capability` values; the previous `unwrap_or_else(|_| process::abort)`
/// form was a process-kill fallback for a path that was never reachable,
/// replaced by the infallible truncate/dedup constructors.
pub(crate) fn capability_set(statuses: Vec<CapabilityStatus>) -> CapabilitySet {
    CapabilitySet::from_bounded_dedup(BoundedVec::from_vec_truncated(statuses))
}

fn bounded<const MAX: usize>(value: &str) -> BoundedText<MAX> {
    // Over-long runtime messages are truncated at a char boundary rather than
    // aborting the daemon; the empty fallback keeps the bound valid.
    BoundedText::from_nonempty_clamped(value.to_owned(), "error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_do_not_claim_unprobed_runtime_health() {
        for kind in [CoreKind::Mihomo, CoreKind::SingBox, CoreKind::Xray] {
            let value = descriptor(kind);
            assert_eq!(value.kind, kind);
            assert_eq!(value.runtime_api, RuntimeAvailability::Unknown);
            assert!(!value.binary_name.is_empty());
        }
    }
}

#[cfg(test)]
mod ready_tests {
    use super::*;
    use caly_domain::CapabilitySet;
    use caly_platform::process::{
        OwnedProcessTree, ProcessContainment, ProcessExit, ProcessSpawner, SpawnSpec,
    };

    /// A tree whose `poll_exit` reports an already-exited child.
    struct ExitedTree;
    impl OwnedProcessTree for ExitedTree {
        fn containment(&self) -> ProcessContainment {
            ProcessContainment::UnixProcessGroup
        }
        fn generation(&self) -> u64 {
            1
        }
        fn stop_gracefully(
            &mut self,
            _t: Duration,
        ) -> Result<Option<ProcessExit>, caly_platform::PlatformFailure> {
            Ok(None)
        }
        fn force_kill_tree(&mut self) -> Result<(), caly_platform::PlatformFailure> {
            Ok(())
        }
        fn poll_exit(&mut self) -> Result<Option<ProcessExit>, caly_platform::PlatformFailure> {
            Ok(Some(ProcessExit {
                code: Some(1),
                signalled: false,
            }))
        }
        fn wait_reaped(&mut self) -> Result<ProcessExit, caly_platform::PlatformFailure> {
            Ok(ProcessExit {
                code: Some(1),
                signalled: false,
            })
        }
    }

    struct ExitedSpawner;
    impl ProcessSpawner for ExitedSpawner {
        type Tree = ExitedTree;
        fn spawn_owned(
            &mut self,
            _spec: SpawnSpec,
            _generation: u64,
        ) -> Result<Self::Tree, caly_platform::PlatformFailure> {
            Ok(ExitedTree)
        }
    }

    /// A control that never becomes ready (always health-check failure).
    struct NeverReady;
    impl KernelControl for NeverReady {
        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::new(caly_domain::BoundedVec::new()).unwrap()
        }
        fn wait_ready(&mut self, _t: Duration) -> Result<(), KernelFailure> {
            Err(KernelFailure {
                kind: KernelFailureKind::ProcessLifecycle,
                message: bounded("not ready"),
                suggested_action: bounded("n/a"),
                platform: None,
            })
        }
        fn select_proxy(
            &mut self,
            _node: caly_domain::NodeId,
            _t: Duration,
        ) -> Result<(), KernelFailure> {
            Ok(())
        }
        fn health_check(&mut self, _t: Duration) -> Result<(), KernelFailure> {
            Err(KernelFailure {
                kind: KernelFailureKind::ProcessLifecycle,
                message: bounded("not ready"),
                suggested_action: bounded("n/a"),
                platform: None,
            })
        }
    }

    #[test]
    fn readiness_fails_fast_when_child_exits() {
        let mut tree = ExitedTree;
        let mut control = NeverReady;
        let started = std::time::Instant::now();
        let result = wait_ready_detecting_exit::<ExitedSpawner, NeverReady>(
            &mut tree,
            &mut control,
            Duration::from_secs(60),
            "test",
        );
        assert!(result.is_err());
        // Must return quickly (well under the 60s timeout) once the child exits.
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
