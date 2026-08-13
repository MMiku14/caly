use super::*;
use caly_domain::SubscriptionId;
use caly_ports::{ActorFailure, ConfigActorPort, ConfigCandidate};
use std::fs;

#[test]
fn supplied_fixture_refreshes_and_advances_generation() -> Result<(), ActorFailure> {
    let id = SubscriptionId::from_bytes([4; 16]);
    let body = fs::read("../../fixtures/subscription-20260803.txt").map_err(|error| {
        ActorFailure::new(
            caly_ports::ActorFailureKind::Infrastructure,
            "fixture read failed",
            "check fixture path exists relative to crates/caly-backends",
        )
        .unwrap_or_else(|failure| {
            unreachable!("ActorFailure ctor is infallible: {failure:?} (io: {error})")
        })
    })?;
    let mut backend = CachedSubscriptionBackend::new();
    backend.put_source(id, body);
    let projection = backend.refresh_projection(id)?;
    assert!(!projection.nodes.is_empty());
    // The fixture parses completely since VMess HTTP/H2 transports are
    // representable; any rejection would be a parser regression.
    assert_eq!(projection.rejected_lines, 0);
    assert_eq!(backend.generation(id), 1);
    Ok(())
}

#[test]
fn config_backend_rolls_back_to_previous_generation() -> Result<(), ActorFailure> {
    let destination =
        caly_platform::paths::test_helpers::unique_path_under("caly-rollback", "rollback");
    let mut backend = MihomoConfigBackend::new(destination.clone());
    let prepared_first = backend.parse_and_render(ConfigCandidate { id: [1; 16] })?;
    let first = backend.commit_candidate(prepared_first)?;
    // 刀 4: an identical render is now a no-op, so make the second render
    // differ by touching the published file — the rollback path must still
    // exercise the real write.
    let existing = std::fs::read_to_string(&destination)
        .map_err(|error| crate::failure("rollback test read failed", &format!("{error}")))?;
    std::fs::write(&destination, format!("{existing}# external edit\n"))
        .map_err(|error| crate::failure("rollback test write failed", &format!("{error}")))?;
    let prepared_second = backend.parse_and_render(ConfigCandidate { id: [2; 16] })?;
    let second = backend.commit_candidate(prepared_second)?;
    assert!(!second.unchanged);
    assert!(
        std::fs::read_to_string(&destination)
            .map_err(|_| ActorFailure::new(
                caly_ports::ActorFailureKind::Infrastructure,
                "rollback test read failed",
                "retry test"
            )
            .unwrap_or_else(|failure| unreachable!(
                "ActorFailure ctor is infallible: {failure:?}"
            )))?
            .contains("caly-generation: 2")
    );
    backend.rollback_commit(second)?;
    assert!(
        std::fs::read_to_string(&destination)
            .map_err(|_| ActorFailure::new(
                caly_ports::ActorFailureKind::Infrastructure,
                "rollback test read failed",
                "retry test"
            )
            .unwrap_or_else(|failure| unreachable!(
                "ActorFailure ctor is infallible: {failure:?}"
            )))?
            .contains("caly-generation: 1")
    );
    let _ = backend.rollback_commit(first);
    let _ = fs::remove_file(destination);
    Ok(())
}

#[test]
fn config_backend_commits_a_generation() -> Result<(), ActorFailure> {
    let destination =
        caly_platform::paths::test_helpers::unique_path_under("caly-config", "commit");
    let mut backend = MihomoConfigBackend::new(destination.clone());
    let prepared = backend.parse_and_render(ConfigCandidate { id: [5; 16] })?;
    let committed = backend.commit_candidate(prepared)?;
    assert_eq!(committed.generation, 1);
    assert!(destination.exists());
    let _ = fs::remove_file(destination);
    Ok(())
}

#[test]
fn unchanged_commit_skips_publish() -> Result<(), ActorFailure> {
    let destination =
        caly_platform::paths::test_helpers::unique_path_under("caly-noop", "unchanged");
    let mut backend = MihomoConfigBackend::new(destination.clone());
    let first_prepared = backend.parse_and_render(ConfigCandidate { id: [1; 16] })?;
    let first = backend.commit_candidate(first_prepared)?;
    assert!(!first.unchanged, "first publish must write");
    let second_prepared = backend.parse_and_render(ConfigCandidate { id: [2; 16] })?;
    let second = backend.commit_candidate(second_prepared)?;
    assert!(second.unchanged, "identical render must skip the publish");
    assert_eq!(
        second.generation, 1,
        "generation must not advance on a no-op apply"
    );
    let _ = fs::remove_file(destination);
    Ok(())
}

#[test]
fn unchanged_survives_generation_reset_after_daemon_restart() -> Result<(), ActorFailure> {
    // 刀 4 + agent audit: the in-memory generation resets to 0 on daemon
    // restart while the published file still carries `# caly-generation: 7`.
    // The no-op comparison must strip ANY trailing marker, not just the one
    // matching the current in-memory generation.
    let destination = caly_platform::paths::test_helpers::unique_path_under("caly-noop", "restart");
    let mut backend = MihomoConfigBackend::new(destination.clone());
    let prepared = backend.parse_and_render(ConfigCandidate { id: [1; 16] })?;
    let first = backend.commit_candidate(prepared)?;
    assert!(!first.unchanged);
    // Simulate the daemon restarting: fresh backend, generation back to 0,
    // same render body.
    let mut backend = MihomoConfigBackend::new(destination.clone());
    let prepared = backend.parse_and_render(ConfigCandidate { id: [2; 16] })?;
    let second = backend.commit_candidate(prepared)?;
    assert!(
        second.unchanged,
        "same render must be a no-op even after a generation reset"
    );
    let _ = fs::remove_file(destination);
    Ok(())
}

#[test]
fn failure_clamps_long_runtime_strings_without_panicking() {
    let long = "x".repeat(2000);
    let error = crate::failure(&long, &long);
    assert_eq!(error.message.len_bytes(), 512);
    assert_eq!(error.suggested_action.len_bytes(), 512);
}

#[test]
fn failure_empty_message_uses_fallback() {
    let error = crate::failure("", "do something");
    assert_eq!(error.message.as_str(), "operation failed");
    assert_eq!(error.suggested_action.as_str(), "do something");
}
