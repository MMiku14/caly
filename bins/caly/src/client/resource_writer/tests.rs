//! Tests for `client/resource_writer.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

#[test]
fn is_path_safe_name_accepts_canonical_ids() {
    assert!(is_path_safe_name("team-shared"));
    assert!(is_path_safe_name("Profile_01"));
    assert!(is_path_safe_name("a"));
    assert!(is_path_safe_name("x.y.z"));
}

#[test]
fn is_path_safe_name_rejects_unsafe_characters() {
    assert!(!is_path_safe_name(""));
    assert!(!is_path_safe_name("../escape"));
    assert!(!is_path_safe_name("a/b"));
    assert!(!is_path_safe_name("a b"));
    assert!(!is_path_safe_name("naïve"));
}

#[test]
fn is_http_url_accepts_and_trims() {
    assert_eq!(is_http_url("https://x/y").unwrap(), "https://x/y");
    assert_eq!(is_http_url("  http://x  ").unwrap(), "http://x");
}

#[test]
fn is_http_url_rejects_empty_and_non_http_schemes() {
    assert!(is_http_url("").is_err());
    assert!(is_http_url("   ").is_err());
    assert!(is_http_url("file:///etc/passwd").is_err());
    assert!(is_http_url("data:text/plain,hello").is_err());
    assert!(is_http_url("not-a-url").is_err());
}

#[test]
fn current_unix_ms_is_nonzero_after_2020() {
    let ms = current_unix_ms();
    // 2020-01-01 epoch ms; any sane wall clock is well past.
    assert!(ms > 1_577_836_800_000, "must be > 2020 epoch ms");
}

#[test]
fn resource_error_from_config_write_error_maps_variants() {
    let read = ConfigWriteError::Read {
        path: std::path::PathBuf::from("/x"),
        reason: "y".to_owned(),
    };
    let validated = ConfigWriteError::Validate {
        path: std::path::PathBuf::from("/x"),
        reason: "y".to_owned(),
    };
    let written = ConfigWriteError::Write {
        path: std::path::PathBuf::from("/x"),
        reason: "y".to_owned(),
    };
    assert!(matches!(ResourceError::from(read), ResourceError::Write(_)));
    assert!(matches!(
        ResourceError::from(validated),
        ResourceError::Validate(_)
    ));
    assert!(matches!(
        ResourceError::from(written),
        ResourceError::Write(_)
    ));
}
