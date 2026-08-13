//! Tests for `client/subscription.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[test]
fn disabled_and_named_rows_survive_the_projection() {
    let declared = caly_profile::schema::SubscriptionConfig {
        url: None,
        sources: vec![
            caly_profile::schema::SubscriptionSource {
                url: "https://a.example.com/sub".to_owned(),
                enabled: true,
                name: Some("alpha".to_owned()),
                refresh_every_minutes: None,
            },
            caly_profile::schema::SubscriptionSource {
                url: "https://b.example.com/sub".to_owned(),
                enabled: false,
                name: None,
                refresh_every_minutes: None,
            },
        ],
        ..caly_profile::schema::SubscriptionConfig::default()
    };
    let projected = source_rows(&declared);
    assert_eq!(projected.len(), 2, "one row per declared source");
    assert_eq!(projected[0]["name"].as_str(), Some("alpha"));
    assert_eq!(projected[0]["enabled"].as_bool(), Some(true));
    assert_eq!(projected[1]["enabled"].as_bool(), Some(false));
    assert!(projected[1].get("name").is_none());
}

/// 2026-08-12 CLI audit: a future NEXT REFRESH inside the next minute
/// must read `in Ns`, not "just now" (which lies about the past tense).
#[test]
fn relative_time_future_seconds_are_not_just_now() {
    assert_eq!(super::relative_time(1000, Some(1000)), "just now");
    assert_eq!(super::relative_time(1000, Some(1005)), "in 5s");
    assert_eq!(super::relative_time(1000, Some(995)), "just now");
    // Larger future deltas use the existing `later` wording.
    assert_eq!(super::relative_time(1000, Some(10_000)), "2h later");
}
