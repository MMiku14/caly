//! Free helpers for composition: bounded task naming, the boot snapshot,
//! configured-core resolution and capability publication.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use caly_domain::{
    AppliedState, BoundedText, BoundedVec, CapabilitySet, DaemonInstanceId, DesiredState,
    EventCursor, EventSequence, ObservedState, PlatformEffectView, PresentationSnapshot, ProxyMode,
    SnapshotRevision,
};

use super::{CompositionError, WallClock};
use caly_application::{
    projection::ProjectionRuntime, runtime::TokioTaskFailure, service::RuntimeService,
};

pub(super) fn task_name(value: &'static str) -> Result<BoundedText<64>, CompositionError> {
    BoundedText::new(value.to_owned()).map_err(|_| CompositionError::TaskCapacity)
}

/// Infallible task name for the production code paths that take a `&'static
/// str` literal and therefore cannot fail the bounded constructor.
pub(super) fn task_name_or_abort(value: &'static str) -> BoundedText<64> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "_")
}

/// Infallible task-reason wrapper for `&'static str` literals. The
/// `from_nonempty_clamped` form is the only path that ever runs in practice;
/// the bounded constructor's `Empty` failure is impossible because every
/// call site passes a static reason string.
pub(super) fn bounded_task_reason(value: &'static str) -> BoundedText<512> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "task failed")
}

/// Bounded task-failure for a named owned task, shared by the dispatcher and
/// the exit-monitor recovery paths.
pub(super) fn task_failure(name: &'static str, reason: &'static str) -> TokioTaskFailure {
    TokioTaskFailure::Task {
        name: task_name_or_abort(name),
        reason: bounded_task_reason(reason),
    }
}

pub(super) fn initial_snapshot(daemon: DaemonInstanceId) -> Result<PresentationSnapshot, ()> {
    let cursor = EventCursor::new(daemon, EventSequence::ZERO);
    let capabilities = CapabilitySet::new(BoundedVec::new()).map_err(|_| ())?;
    Ok(PresentationSnapshot::new(
        daemon,
        SnapshotRevision::new(0),
        cursor,
        DesiredState::new(ProxyMode::Rule, None, None, false, false),
        AppliedState::stopped(),
        ObservedState::default(),
        PlatformEffectView::none(),
        capabilities,
        BoundedVec::new(),
    ))
}

/// Projects the boot-time capability set for the effective core into the
/// projection, so `snapshot`/`watch` report a truthful DNS assessment instead
/// of an empty capability set.
pub(super) fn configured_core(
    override_core: Option<caly_domain::CoreKind>,
) -> Result<caly_domain::CoreKind, CompositionError> {
    match std::env::var("CALY_CORE") {
        Ok(value) => parse_configured_core(Some(&value)),
        Err(std::env::VarError::NotPresent) => match override_core {
            Some(core) => Ok(core),
            None => parse_configured_core(None),
        },
        Err(std::env::VarError::NotUnicode(_)) => Err(CompositionError::UnsupportedCore),
    }
}

pub(super) fn parse_configured_core(
    value: Option<&str>,
) -> Result<caly_domain::CoreKind, CompositionError> {
    match value {
        None | Some("mihomo") => Ok(caly_domain::CoreKind::Mihomo),
        Some("sing-box") => Ok(caly_domain::CoreKind::SingBox),
        Some(_) => Err(CompositionError::UnsupportedCore),
    }
}

pub(super) fn publish_boot_capabilities(
    service: &std::sync::Arc<std::sync::Mutex<RuntimeService<WallClock, ProjectionRuntime>>>,
    core: caly_domain::CoreKind,
    dns_enabled: bool,
) -> Result<(), CompositionError> {
    let capabilities = caly_backends::core_capability_set(core, dns_enabled).map_err(|error| {
        tracing::error!(?error, "cannot build the boot capability set");
        CompositionError::BackendUnavailable
    })?;
    let mut service = service.lock().map_err(|error| {
        tracing::error!(
            ?error,
            "runtime service mutex poisoned during boot capability publish"
        );
        CompositionError::BackendUnavailable
    })?;
    if service
        .projection_mut()
        .publish(caly_domain::PresentationDelta::CapabilitiesReplaced(
            capabilities,
        ))
        .is_err()
    {
        return Err(CompositionError::BackendUnavailable);
    }
    Ok(())
}

/// Writes the generated controller secret to an owner-only file under the XDG
/// runtime root so offline CLI queries can authenticate against the running
/// core's controller. Best-effort: a failure to persist never aborts boot.
pub(super) fn publish_controller_secret(secret: &str) {
    let path = caly_platform::paths::AppPaths::from_env().controller_secret_path();
    if let Err(error) = write_controller_secret(&path, secret) {
        tracing::warn!(
            error = %error,
            "controller secret could not be persisted to {}; \
             offline CLI authentication will require re-running `caly daemon`",
            path.display()
        );
    }
}

/// Persists the controller secret to `path` with `mode(0o600)` on the freshly
/// created inode so no umask-derived default (typically 0o644) is ever
/// observable (the previous `write` + `set_permissions` sequence left a
/// world-readable window and silently kept it if the chmod failed).
///
/// Public-to-crate so tests can exercise the mode invariant on a tempdir
/// instead of contaminating the real XDG state root.
pub(super) fn write_controller_secret(path: &std::path::Path, secret: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, secret.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::write_controller_secret;
    use caly_platform::paths::test_helpers::unique_path_under;
    use std::os::unix::fs::PermissionsExt;

    /// Regression: a freshly written controller secret must be owner-only at
    /// the moment of creation, not after a follow-up `set_permissions` call.
    /// The previous `std::fs::write` + `chmod` sequence left a 0o644 window
    /// during which any local user could read the secret.
    #[test]
    fn persisted_secret_is_owner_only_from_creation() -> Result<(), String> {
        let path = unique_path_under("caly-ctrl-secret", "create");
        write_controller_secret(&path, "credential-content")
            .map_err(|error| format!("write failed: {error}"))?;
        let mode = std::fs::metadata(&path)
            .map_err(|error| format!("stat failed: {error}"))?
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600, "expected 0o600, got {mode:o}");
        Ok(())
    }

    /// Re-running with the same path must truncate the previous content
    /// (the controller secret rotates on every daemon restart) and keep
    /// the owner-only mode.
    #[test]
    fn second_write_truncates_and_preserves_mode() -> Result<(), String> {
        let path = unique_path_under("caly-ctrl-secret", "truncate");
        write_controller_secret(&path, "first")
            .map_err(|error| format!("first write failed: {error}"))?;
        write_controller_secret(&path, "second")
            .map_err(|error| format!("second write failed: {error}"))?;
        let content =
            std::fs::read_to_string(&path).map_err(|error| format!("read failed: {error}"))?;
        let mode = std::fs::metadata(&path)
            .map_err(|error| format!("stat failed: {error}"))?
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(content, "second", "expected second write to win");
        assert_eq!(mode, 0o600, "expected 0o600, got {mode:o}");
        Ok(())
    }
}
