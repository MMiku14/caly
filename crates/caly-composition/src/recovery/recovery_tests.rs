use super::*;

#[test]
fn first_crash_uses_start_backoff() {
    let mut policy = CrashLoopPolicy::new();
    assert_eq!(policy.on_crash(), CRASH_RESTART_BACKOFF_START_MS);
}

#[test]
fn consecutive_crashes_double_backoff() {
    let mut policy = CrashLoopPolicy::new();
    policy.on_crash();
    policy.on_crash();
    assert_eq!(policy.backoff_ms(), CRASH_RESTART_BACKOFF_START_MS * 2);
}

#[test]
fn backoff_is_capped_at_maximum() {
    let mut policy = CrashLoopPolicy::new();
    for _ in 0..20 {
        policy.on_crash();
    }
    assert_eq!(policy.backoff_ms(), CRASH_RESTART_BACKOFF_MAX_MS);
}

#[test]
fn failed_restart_keeps_growing_backoff() {
    let mut policy = CrashLoopPolicy::new();
    policy.on_crash();
    let first = policy.backoff_ms();
    policy.on_restart_failed();
    assert_eq!(
        policy.backoff_ms(),
        (first * 2).min(CRASH_RESTART_BACKOFF_MAX_MS)
    );
}

#[test]
fn successful_restart_alone_does_not_reset_backoff() {
    let mut policy = CrashLoopPolicy::new();
    policy.on_crash();
    policy.on_crash();
    let grown = policy.backoff_ms();
    // No reset occurs until the stabilization window elapses.
    assert!(!policy.on_stable_tick(CRASH_RESTART_BACKOFF_START_MS));
    assert_eq!(policy.backoff_ms(), grown);
}

#[test]
fn stabilization_window_resets_backoff() {
    let mut policy = CrashLoopPolicy::new();
    policy.on_crash();
    policy.on_crash();
    // Elapse the full window in one tick.
    assert!(policy.on_stable_tick(STABILIZE_WINDOW_MS));
    assert_eq!(policy.backoff_ms(), CRASH_RESTART_BACKOFF_START_MS);
}

#[test]
fn crash_after_stabilization_uses_start_backoff() {
    let mut policy = CrashLoopPolicy::new();
    policy.on_crash();
    assert!(policy.on_stable_tick(STABILIZE_WINDOW_MS));
    assert_eq!(policy.on_crash(), CRASH_RESTART_BACKOFF_START_MS);
}

#[test]
fn configurable_bounds_drive_backoff_and_ceiling() {
    let mut policy = CrashLoopPolicy::with_bounds(200, 800);
    assert_eq!(policy.backoff_ms(), 200);
    policy.on_crash();
    assert_eq!(policy.on_crash(), 400);
    assert_eq!(policy.on_crash(), 800);
    // Ceiling holds the configured max, not the built-in 30s.
    assert_eq!(policy.on_crash(), 800);
    // Stabilization resets to the configured initial, not the built-in 1s.
    assert!(policy.on_stable_tick(STABILIZE_WINDOW_MS));
    assert_eq!(policy.backoff_ms(), 200);
}
