//! Tests for `commands/refresh.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;
use crate::test_helpers::temp_root;

#[test]
fn refreshable_trait_shape_is_locked() {
    // The 2 live impls satisfy the trait.
    // Compile-time check. (Round 28: the third
    // impl, `SubscriptionRefresh`, was removed
    // because `set sub refresh` still drives
    // the daemon RPC directly through
    // `run_client`; the trait would lose the
    // operator's `--json` / `--core` flags if
    // it migrated today. See the module-level
    // comment for the migration plan.)
    fn assert_refreshable<T: Refreshable>() {}
    assert_refreshable::<ProfileRefresh>();
    assert_refreshable::<RuleProviderRefresh>();
}

#[test]
fn refresh_target_as_str_round_trip() {
    // Round 28: the `family()` method was deleted
    // (pure dead code — the `RefreshError` variants
    // each carry their own `family: &'static str`
    // field, so the target never needed to derive
    // one). `as_str()` is the only string accessor
    // the dispatch uses, so it stays and is locked
    // here.
    assert_eq!(RefreshTarget::ProfileId("x".to_owned()).as_str(), "x");
    assert_eq!(
        RefreshTarget::RuleProviderName("x".to_owned()).as_str(),
        "x"
    );
}

#[test]
fn profile_refresh_unknown_id_does_not_panic() {
    // Round 28: lock the `ProfileRefresh` path —
    // an unknown profile id (or a missing
    // `config.yaml` on the operator's XDG root)
    // must surface a typed `RefreshError`, not
    // panic. The test exercises the live trait
    // method (not the path-injected
    // `refresh_with_paths` helper) so the
    // dispatch-level call site is in the
    // regression net. The trait impl maps every
    // `ProfileCmdError` arm into one of the
    // 5 `RefreshError` variants, so a missing
    // file (`Other { reason: "read config: …" }`)
    // is the expected shape — the test just
    // locks that the call returns a typed
    // envelope, not a `panic!` / unwind.
    let target = RefreshTarget::ProfileId("missing-profile".to_owned());
    let result = <ProfileRefresh as Refreshable>::refresh(&target);
    // We don't pin the specific variant
    // (a missing `config.yaml` returns `Other`,
    // a present empty `config.yaml` returns
    // `Ok(fetched(0))`, a present config
    // without the id returns `NotDeclared`).
    // The test just locks the *envelope
    // contract*: the trait method returns a
    // typed `Result<RefreshOutcome, RefreshError>`,
    // not a panic.
    match result {
        Ok(_)
        | Err(RefreshError::NotDeclared { .. })
        | Err(RefreshError::Other { .. })
        | Err(RefreshError::Fetch { .. })
        | Err(RefreshError::Store { .. })
        | Err(RefreshError::FetchChain { .. }) => {}
    }
}

#[test]
fn refresh_error_codes_are_stable() {
    let err = RefreshError::NotDeclared {
        family: "profile",
        id: "team".to_owned(),
    };
    assert_eq!(err.code(), "profile.not_declared");
    assert!(err.hint().is_some());

    let err = RefreshError::Fetch {
        family: "sub",
        id: "https://x".to_owned(),
        reason: "timeout".to_owned(),
    };
    assert_eq!(err.code(), "sub.fetch_failed");
    assert!(err.hint().is_some());
}

#[test]
#[allow(clippy::panic, clippy::match_wildcard_for_single_variants)] // test assertion: a non-matching
// outcome is a contract violation we want
// to surface loud, not silently absorb.
fn subscription_refresh_no_daemon_surfaces_typed_error() {
    // Round 28: the `SubscriptionRefresh` struct was
    // removed (it was dead — the live `set sub refresh`
    // dispatch drives the daemon RPC directly through
    // `crate::client::run_client(RefreshSubscription,
    // options)`, not through the `Refreshable` trait).
    // The contract this test used to lock is now
    // exercised end-to-end by the daemon_real_e2e
    // suite. The test here is a thin type-level
    // witness: a no-daemon `run_client_collect` against
    // `RefreshSubscription` must surface one of the
    // two typed errors (`TransportUnavailable` /
    // `DecodeRejected`) so the future
    // `SubscriptionRefresh` impl can fold them into
    // `RefreshError::Fetch` without a re-design.
    use crate::client::{ClientCommand, run_client_collect};
    let result = run_client_collect(
        &ClientCommand::RefreshSubscription {
            subscription_id: None,
            force: false,
            asynchronous: false,
        },
        &crate::cli::CliOptions::default(),
    );
    match result {
        Err(
            caly_protocol::client::ClientError::TransportUnavailable
            | caly_protocol::client::ClientError::DecodeRejected { .. },
        ) => {}
        other => panic!(
            "no-daemon refresh must surface TransportUnavailable or DecodeRejected, got {other:?}"
        ),
    }
}

// ── Round 22: RuleProviderRefresh drives an offline
//    re-validation, not a daemon RPC. The tests below
//    exercise the `refresh_with_paths` helper with
//    hermetic `AppPaths` (no process env mutation) and
//    confirm the contract: `(all)` returns the count,
//    a known name returns the count, an unknown name
//    surfaces `NotDeclared`.

use crate::test_helpers::hermetic_paths;
use caly_platform::paths::AppPaths;
use std::fs;

fn seed_with_provider(paths: &AppPaths, name: &str) {
    fs::create_dir_all(&paths.config).unwrap();
    let body = format!(
        "schema_version: 1\ncore: mihomo\nrule_providers:\n  - name: {name}\n    type: http\n    behavior: domain\n    format: source\n    url: https://example.com/{name}\n    interval_ms: 86400000\n    enabled: true\n"
    );
    fs::write(paths.config.join("config.yaml"), body).unwrap();
}

#[test]
fn rule_provider_refresh_all_counts_declared_entries() {
    let dir = temp_root("all");
    let paths = hermetic_paths(&dir);
    // Seed two declared providers in one config.yaml.
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(
        paths.config.join("config.yaml"),
        "schema_version: 1\ncore: mihomo\nrule_providers:\n  - name: geosite-cn\n    type: http\n    behavior: domain\n    format: source\n    url: https://example.com/geosite-cn\n    interval_ms: 86400000\n    enabled: true\n  - name: geoip-cn\n    type: http\n    behavior: domain\n    format: source\n    url: https://example.com/geoip-cn\n    interval_ms: 86400000\n    enabled: true\n",
    )
    .unwrap();
    let target = RefreshTarget::RuleProviderName("(all)".to_owned());
    let result =
        refresh_with_paths(&paths, &target).unwrap_or_else(|e| panic!("refresh failed: {e:?}"));
    assert_eq!(result.fetched, 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rule_provider_refresh_known_name_succeeds() {
    let dir = temp_root("known");
    let paths = hermetic_paths(&dir);
    seed_with_provider(&paths, "geosite-cn");
    let target = RefreshTarget::RuleProviderName("geosite-cn".to_owned());
    let result =
        refresh_with_paths(&paths, &target).unwrap_or_else(|e| panic!("refresh failed: {e:?}"));
    assert_eq!(result.fetched, 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rule_provider_refresh_unknown_name_surfaces_not_declared() {
    let dir = temp_root("unknown");
    let paths = hermetic_paths(&dir);
    seed_with_provider(&paths, "geosite-cn");
    let target = RefreshTarget::RuleProviderName("nope".to_owned());
    let result = refresh_with_paths(&paths, &target);
    match &result {
        Err(RefreshError::NotDeclared { family, id }) => {
            assert_eq!(*family, "rule_provider");
            assert_eq!(id, "nope");
        }
        other => {
            panic!("expected NotDeclared, got {other:?}")
        }
    }
    let _ = fs::remove_dir_all(&dir);
}
