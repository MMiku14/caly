//! Tests for `composition/backends.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::provision_geoip_metadb_with_sources;
use caly_platform::paths::test_helpers::unique_path_under;

/// The 1 MiB sanity bound guarding against stub or partial copies.
const SEED_MIN_BYTES: usize = 1_000_000;

/// Aborts the test on a setup failure. Every test in this module needs
/// an empty sandbox under `unique_path_under`, removed at the end of the
/// test. Filesystem errors during setup are not what the suite is
/// testing, so a process-kill surfaces them loudly instead of papering
/// over them with `unwrap()` (denied by the workspace lint).
fn must_create_dir(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|error| {
        panic!(
            "test setup: create_dir_all({}) failed: {error}",
            path.display()
        )
    });
}

fn must_write(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes)
        .unwrap_or_else(|error| panic!("test setup: write({}) failed: {error}", path.display()));
}

fn must_read(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path)
        .unwrap_or_else(|error| panic!("test setup: read({}) failed: {error}", path.display()))
}

fn must_metadata(path: &std::path::Path) -> std::fs::Metadata {
    std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("test setup: metadata({}) failed: {error}", path.display()))
}

/// Regression: a corrupt (sub-1 MiB) `geoip.metadb` at the target path
/// must not bypass the source-copy loop. The previous implementation
/// short-circuited on `target.is_file()` and left a stub from an
/// interrupted earlier run in place forever, so a real `mihomo -t`
/// validation would then use a half-written database and fail in
/// confusing ways.
#[test]
fn corrupt_target_is_replaced_when_source_is_healthy() {
    let working_dir = unique_path_under("caly-geoip", "corrupt-target");
    must_create_dir(&working_dir);
    let target = working_dir.join("geoip.metadb");
    // A stub written by a previous, interrupted run.
    must_write(&target, b"corrupt");

    // Inject a healthy (>1 MiB) source at a temp path so the test is
    // hermetic — independent of any `/etc/mihomo/geoip.metadb` that may
    // happen to exist in the test environment.
    let source = unique_path_under("caly-geoip", "source-healthy");
    must_create_dir(source.parent().unwrap());
    must_write(&source, &vec![0xA5_u8; SEED_MIN_BYTES]);

    let seeded = provision_geoip_metadb_with_sources(&working_dir, [source]);
    assert!(seeded, "a healthy source must seed successfully");
    let after = must_metadata(&target);
    assert!(
        after.len() >= SEED_MIN_BYTES as u64,
        "target must hold the healthy source, got {} bytes",
        after.len()
    );
    let _ = std::fs::remove_dir_all(&working_dir);
}

/// A complete (≥ 1 MiB) target is preserved untouched; the function must
/// not waste time re-copying a healthy database.
#[test]
fn healthy_target_is_not_overwritten() {
    let working_dir = unique_path_under("caly-geoip", "healthy-target");
    must_create_dir(&working_dir);
    let target = working_dir.join("geoip.metadb");
    let healthy_bytes = vec![0xAB_u8; SEED_MIN_BYTES];
    must_write(&target, &healthy_bytes);
    let seeded =
        provision_geoip_metadb_with_sources(&working_dir, std::iter::empty::<std::path::PathBuf>());
    assert!(seeded, "a healthy target counts as already-seeded");
    let after = must_read(&target);
    assert_eq!(after.len(), healthy_bytes.len());
    assert!(after.iter().all(|byte| *byte == 0xAB));
    let _ = std::fs::remove_dir_all(&working_dir);
}

/// A missing target with no healthy source reports failure but must not
/// panic and must not leave a half-written target behind.
#[test]
fn missing_target_with_no_source_is_a_clean_noop() {
    let working_dir = unique_path_under("caly-geoip", "missing-target");
    // The directory does not even exist; the function must create it
    // and return false without aborting.
    let seeded =
        provision_geoip_metadb_with_sources(&working_dir, std::iter::empty::<std::path::PathBuf>());
    assert!(!seeded, "no healthy source available in this test");
    assert!(working_dir.is_dir(), "function must create the working dir");
    let _ = std::fs::remove_dir_all(&working_dir);
}

/// A stub (sub-1 MiB) source is treated as corrupt and never copied —
/// mirroring the existing source-side guard.
#[test]
fn stub_source_is_skipped_without_overwriting() {
    let working_dir = unique_path_under("caly-geoip", "stub-source");
    let stub = unique_path_under("caly-geoip", "stub-source-data");
    must_create_dir(stub.parent().unwrap());
    must_write(&stub, b"too-small");
    let seeded = provision_geoip_metadb_with_sources(&working_dir, [stub.clone()]);
    assert!(!seeded, "a sub-1 MiB source must not be considered healthy");
    // No target file should be created when the only source is corrupt.
    let target = working_dir.join("geoip.metadb");
    assert!(
        !target.exists(),
        "target must not be created from a stub source"
    );
    let _ = std::fs::remove_file(&stub);
    let _ = std::fs::remove_dir_all(&working_dir);
}
