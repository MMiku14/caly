//! Tests for `subscription/http.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

fn transient() -> ActorFailure {
    crate::failure("subscription fetch failed: RequestFailed", "retry")
}

#[test]
fn transient_failure_is_retried_then_succeeds() {
    let mut calls = 0;
    let result = retry_fetch(3, 0, || {
        calls += 1;
        if calls < 2 {
            Err((true, transient()))
        } else {
            Ok(true)
        }
    });
    assert!(result.is_ok());
    assert_eq!(calls, 2, "second attempt should succeed");
}

#[test]
fn persistent_transient_failure_surfaces_after_exhausting_attempts() {
    let mut calls = 0;
    let result = retry_fetch(3, 0, || {
        calls += 1;
        Err((true, transient()))
    });
    assert!(result.is_err());
    assert_eq!(calls, 3, "all attempts exhausted");
}

#[test]
fn nontransient_failure_returns_immediately() {
    let mut calls = 0;
    let result = retry_fetch(3, 0, || {
        calls += 1;
        Err((false, transient()))
    });
    assert!(result.is_err());
    assert_eq!(calls, 1, "non-transient failure must not retry");
}

#[test]
fn transient_classification_matches_fetch_errors() {
    use caly_subscription::FetchError;
    assert!(is_transient_fetch(&FetchError::RequestFailed(
        "connect error".to_owned()
    )));
    assert!(is_transient_fetch(&FetchError::ClientBuild));
    assert!(is_transient_fetch(&FetchError::UnexpectedStatus(503)));
    assert!(!is_transient_fetch(&FetchError::UnexpectedStatus(404)));
    assert!(!is_transient_fetch(&FetchError::UnsupportedScheme));
    assert!(!is_transient_fetch(&FetchError::BodyTooLarge));
}

#[test]
fn subscription_id_for_url_is_stable_and_distinct() {
    // Same URL always maps to the same id (stable cache key across restarts),
    // and different URLs map to different ids (per-source merge/dedup).
    let a1 = subscription_id_for_url("https://provider.example/sub");
    let a2 = subscription_id_for_url("https://provider.example/sub");
    let b = subscription_id_for_url("https://second.example/sub?token=xyz");
    assert_eq!(a1, a2, "same URL must yield the same SubscriptionId");
    assert_ne!(a1, b, "different URLs must yield distinct SubscriptionIds");
    // Id is a full 16-byte value, not a zero-padded truncation.
    assert_ne!(a1.into_bytes(), [0_u8; 16]);
    // #113: the id is derived from the CANONICAL form (WHATWG parse +
    // re-serialize), so cosmetic differences — trailing whitespace, scheme
    // case, a default port — fold into one id instead of splitting one
    // source into duplicate cache entries.
    let c1 = subscription_id_for_url("https://p.example/x");
    let c2 = subscription_id_for_url("https://p.example/x ");
    assert_eq!(c1, c2, "canonical-equal URLs must share one SubscriptionId");
    let c3 = subscription_id_for_url("HTTPS://p.example:443/x");
    assert_eq!(c1, c3, "scheme case / default port must normalize away");
    // Genuinely different URLs still differ.
    let c4 = subscription_id_for_url("https://p.example/y");
    assert_ne!(c1, c4, "distinct paths must yield distinct SubscriptionIds");
}

// ---------------------------------------------------------------- W2-β2b --
// Scheduled-refresh due rulings (Q5 cadence consumption).

#[test]
fn scheduled_due_rules_follow_the_q5_cadence() {
    let now = std::time::Instant::now();
    // Static pin: never due.
    assert!(!due_for_scheduled_refresh(Some(0), None, now));
    assert!(!due_for_scheduled_refresh(Some(0), Some(&now), now));
    // No per-source cadence: inherits the batch tick — always due.
    assert!(due_for_scheduled_refresh(None, Some(&now), now));
    // Explicit cadence: due when never fetched, or after the period.
    assert!(due_for_scheduled_refresh(Some(60), None, now));
    let recent = now
        .checked_sub(std::time::Duration::from_secs(30 * 60))
        .unwrap();
    assert!(!due_for_scheduled_refresh(Some(60), Some(&recent), now));
    let stale = now
        .checked_sub(std::time::Duration::from_secs(61 * 60))
        .unwrap();
    assert!(due_for_scheduled_refresh(Some(60), Some(&stale), now));
}
