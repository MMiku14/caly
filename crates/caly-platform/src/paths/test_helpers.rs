//! Shared filesystem helpers for integration/unit tests.
//!
//! Tests across the workspace need a unique temp path (nanos + PID), an
//! owner-only temp file, or a writable temp directory. Before this module
//! existed, nine different test modules each rolled their own `unique_path`
//! helper, with subtle differences in nanosecond formatting and a copy of
//! `expect(...)` on every filesystem failure (panics are fine in
//! test-only helpers; a `#![allow]` at module top declares that
//! intentionally instead of hiding it behind `abort()` calls).
//! Centralising the
//! helpers here:
//! - collapses ~60 lines of copy-paste into one tested module,
//! - gives every test a consistent naming scheme (`<prefix>-<label>-<nanos>-<pid>`),
//! - keeps the `unwrap_or_else(|_| process::abort)` perms failures scoped to
//!   test setup (never the production path being tested).
//!
//! The module is `pub` so test crates in the workspace can reach it
//! through `caly_platform::paths::test_helpers::*`.

#![allow(clippy::expect_used, clippy::panic)] // test-only helper: expect/panic are the intended failure mode

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Common prefix for every caly test path. The leading `caly-` keeps the
/// cleanup sweep simple (a single `rm -rf /tmp/caly-*` clears stale state
/// from a crashed test run on any developer's machine).
const TEST_PATH_PREFIX: &str = "caly";

/// Returns a unique temp path under the system temp dir, scoped to this
/// process so concurrent test binaries cannot collide. The path is **not**
/// created: callers can choose whether to create a file, a directory, or
/// use the path for `validate_uds_path`-style "must-not-exist" checks.
///
/// Naming scheme: `caly-<label>-<nanos>-<pid>`. The label is a free-form
/// short tag for grep-ability in the temp dir.
pub fn unique_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let safe_label = sanitize_label(label);
    std::env::temp_dir().join(format!(
        "{TEST_PATH_PREFIX}-{safe_label}-{nanos}-{}",
        std::process::id()
    ))
}

/// Returns a unique temp path under a caller-supplied namespace, used by
/// tests that need to assert the prefix (e.g. `crates/caly-backends/tests/*`
/// distinguishing `apply-e2e` from `singbox-apply-e2e`).
pub fn unique_path_under(prefix: &str, label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let safe_label = sanitize_label(label);
    let safe_prefix = sanitize_label(prefix);
    std::env::temp_dir().join(format!(
        "{safe_prefix}-{safe_label}-{nanos}-{}",
        std::process::id()
    ))
}

/// Creates `path` (and any missing parent directories) and writes
/// `contents` to it with mode `0o600`. Used by tests that need a real
/// owner-only file (e.g. controller secret, recovery record) on disk.
///
/// # Panics
///
/// Filesystem failures (no temp dir, permission denied, full disk) panic
/// the test: they are setup failures, never production behavior, and the
/// panic makes the test fail loudly instead of silently passing on a
/// broken CI host.
pub fn write_owner_only(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| {
            eprintln!(
                "test setup: create_dir_all({}) failed: {error}",
                parent.display()
            );
            panic!("test helper FS operation failed");
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, contents))
            .unwrap_or_else(|error| {
                eprintln!("test setup: write({}) failed: {error}", path.display());
                panic!("test helper FS operation failed");
            });
    }
    #[cfg(not(unix))]
    {
        let _ = contents;
        std::fs::write(path, contents).unwrap_or_else(|error| {
            eprintln!("test setup: write({}) failed: {error}", path.display());
            panic!("test helper FS operation failed");
        });
    }
}

/// Replaces any character in `label` that is not ASCII alphanumeric, dash,
/// or underscore with `_`. Keeps the unique path safe to embed in error
/// messages and prevents accidental path traversal from a typo in a test
/// label.
fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_lives_under_temp_and_includes_label() {
        let path = unique_path("demo");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("caly-demo-"), "got {path_str}");
        assert!(path_str.contains(&std::process::id().to_string()));
        // Temp dir on every platform starts with the platform temp root.
        assert!(path.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn unique_path_under_respects_caller_prefix() {
        let path = unique_path_under("caly-apply-e2e", "config");
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("caly-apply-e2e-config-"),
            "got {path_str}"
        );
    }

    #[test]
    fn sanitize_label_rejects_path_separators() {
        let path = unique_path_under("prefix", "evil/path");
        let path_str = path.to_string_lossy();
        // The label is sanitised: the slash is replaced with `_`, so the
        // path stays single-component and never traverses out of temp_dir.
        assert!(path_str.contains("evil_path"));
        let stripped = path
            .strip_prefix(std::env::temp_dir())
            .unwrap_or_else(|error| {
                eprintln!("path {path:?} not under temp dir: {error}");
                panic!("test helper FS operation failed");
            });
        let components: Vec<_> = stripped.components().collect();
        // Exactly one path component beyond the temp root: `<prefix>-<label>-<nanos>-<pid>`.
        assert_eq!(components.len(), 1, "got {path:?}");
    }

    #[test]
    fn write_owner_only_creates_file_with_owner_mode() {
        let path = unique_path("owner");
        write_owner_only(&path, b"hello\n");
        let contents = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            eprintln!("read failed: {error}");
            panic!("test helper FS operation failed");
        });
        assert_eq!(contents, "hello\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .unwrap_or_else(|error| {
                    eprintln!("metadata failed: {error}");
                    panic!("test helper FS operation failed");
                })
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "expected 0o600, got {mode:o}");
        }
        let _ = std::fs::remove_file(&path);
    }
}
