//! Tests for `composition/backends.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::resolve_bundled_binary;
use caly_platform::paths::test_helpers::unique_path_under;

/// Creates an empty executable script at `path`. The script body is
/// the standard `#!/bin/sh` shebang; tests that exercise the binary
/// resolver never execute the contents, they only need a real file
/// node to satisfy the `.is_file()` check.
fn touch(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!("test setup: create_dir_all({parent:?}) failed: {error}")
        });
    }
    std::fs::write(path, b"#!/bin/sh\n")
        .unwrap_or_else(|error| panic!("test setup: write({path:?}) failed: {error}"));
}

#[test]
fn prefers_the_working_directory_vendor_bin() {
    let dir = unique_path_under("caly-bundle", "cwd");
    let binary = dir.join("vendor/bin/mihomo");
    touch(&binary);
    let resolved = resolve_bundled_binary("mihomo", Some(dir.as_path()), None);
    assert_eq!(resolved, binary);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn walks_executable_ancestors_for_dev_layouts() {
    let repo = unique_path_under("caly-bundle", "exe");
    let executable = repo.join("target/debug/caly");
    let binary = repo.join("vendor/bin/sing-box");
    touch(&executable);
    touch(&binary);
    let elsewhere = repo.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let resolved = resolve_bundled_binary(
        "sing-box",
        Some(elsewhere.as_path()),
        Some(executable.as_path()),
    );
    assert_eq!(resolved, binary);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn falls_back_to_a_cwd_relative_path_for_error_messages() {
    let missing = unique_path_under("caly-bundle", "none");
    std::fs::create_dir_all(&missing).unwrap();
    let resolved = resolve_bundled_binary("mihomo", Some(missing.as_path()), None);
    assert_eq!(resolved, std::path::PathBuf::from("vendor/bin/mihomo"));
    let _ = std::fs::remove_dir_all(&missing);
}
