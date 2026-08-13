//! Cross-module test helpers.
//!
//! Round 26: collapses the per-module `temp_root` and
//! `hermetic_paths` helpers that were duplicated across 11
//! client / dispatch test modules. Before this round each
//! module had its own private 10-line `temp_root` with a
//! different `caly-{tag}-{pid}-{n}-{nanos}` prefix; a future
//! rename of the tag scheme would have required 11
//! parallel edits. The two helpers here are the single
//! point of truth for "create a hermetic XDG root" and
//! "build a unique temp dir for a test fixture".
//!
//! # Why `cfg(test)` at the symbol level
//!
//! The helpers are only meaningful for unit tests, and
//! shipping them to the production binary would pull
//! `std::time::SystemTime` plumbing into the
//! `AppPaths::from_env_vars` builder for no real reason.
//! The module itself is `pub(crate)` (so internal test
//! modules can `use crate::test_helpers::...`), but each
//! public symbol carries `#[cfg(test)]` so a release
//! build does not see them — `cargo build --release`
//! stays clean (no `dead_code` warnings) and a
//! non-test caller cannot accidentally reach for the
//! helpers.
//!
//! # Layout
//!
//! - `temp_root`: a unique subdirectory of
//!   `std::env::temp_dir()` for hermetic test fixtures.
//!   The `tag` is the human-readable label the test
//!   uses to identify the fixture in `eprintln!`
//!   diagnostics (e.g. `"sub-dry"`, `"pg-dispatch"`).
//! - `hermetic_paths`: a `caly_platform::paths::AppPaths`
//!   rooted at a per-test temp dir. The two XDG paths
//!   point at the same root (the production operator
//!   layout does the same — `XDG_STATE_HOME` and
//!   `XDG_CONFIG_HOME` are usually the same parent).
//!   `HOME` / `XDG_RUNTIME_DIR` are filled with
//!   hermetic dummy values so the loader does not
//!   accidentally reach the operator's actual XDG
//!   state if the writer forgets to honour the
//!   injected `AppPaths` (defense-in-depth — a
//!   pre-Round 26 regression test caught exactly
//!   this in `set_proxy_dry_run_emits_envelope`).
//!
//! Note: the function names are written in backticks
//! (not `[..]` intra-doc links) because the symbols
//! carry `#[cfg(test)]` — a release-build `cargo doc`
//! does not see them and the link checker would
//! fail. The names are real at the `cargo test --doc`
//! build (test build), where the doc comment is also
//! checked.

/// Creates a unique temp directory under
/// `std::env::temp_dir()` for a single test fixture.
/// The directory's name is
/// `caly-{tag}-{pid}-{counter}-{nanos}` where
/// `counter` is a process-global atomic
/// (test-instances within the same `nanos` bucket
/// get distinct counters) and `nanos` is a
/// `SystemTime` clock reading. The `pid` is the
/// process id (multi-process parallel test runs do
/// not collide because `pid` differs across
/// processes).
///
/// The directory is `create_dir_all`'d before the
/// function returns. A `create_dir_all` failure
/// aborts the test process — a hermetic temp
/// directory is a precondition for every test
/// fixture, and a `?`-propagated `Err` would just
/// add a third path the test had to cover.
#[cfg(test)]
pub fn temp_root(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!("caly-{tag}-{pid}-{n}-{nanos}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Builds a hermetic [`caly_platform::paths::AppPaths`]
/// rooted at `dir`. `XDG_STATE_HOME` and
/// `XDG_CONFIG_HOME` both point at `dir` (the
/// production layout does the same; state and
/// config are siblings under `$HOME` or
/// `$XDG_DATA_HOME`'s parent). `HOME` and
/// `XDG_RUNTIME_DIR` are filled with dummy values
/// so a writer that forgets to honour the injected
/// `paths` (and instead reaches for
/// `AppPaths::from_env()`) does not touch the
/// operator's actual XDG state.
#[cfg(test)]
pub fn hermetic_paths(dir: &std::path::Path) -> caly_platform::paths::AppPaths {
    let env = |key: &str| -> Option<std::ffi::OsString> {
        match key {
            "HOME" => Some(std::ffi::OsString::from("/home/test")),
            "XDG_RUNTIME_DIR" => Some(std::ffi::OsString::from("/run/user/1000")),
            "XDG_STATE_HOME" => Some(std::ffi::OsString::from(dir.as_os_str())),
            "XDG_CONFIG_HOME" => Some(std::ffi::OsString::from(dir.as_os_str())),
            _ => None,
        }
    };
    caly_platform::paths::AppPaths::from_env_vars(&env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `temp_root` must produce a unique, existing
    /// subdirectory of `std::env::temp_dir()`.
    /// Subsequent calls in the same test process
    /// must not collide (the `counter` part of
    /// the path disambiguates them).
    #[test]
    fn temp_root_creates_a_unique_directory() {
        let a = temp_root("round26-helper");
        let b = temp_root("round26-helper");
        assert_ne!(a, b, "consecutive temp_root calls must be distinct");
        assert!(a.is_dir());
        assert!(b.is_dir());
    }

    /// `temp_root` must embed the tag in the path
    /// so a failed test can `eprintln!` its fixture
    /// path and the operator can find the directory
    /// under `/tmp` (or `$TMPDIR`). The tag is
    /// literally interpolated into the path
    /// component; assert it round-trips.
    #[test]
    fn temp_root_embeds_the_tag() {
        let path = temp_root("hello");
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert!(name.contains("hello"), "tag must appear in path: {name}");
    }

    /// `hermetic_paths` must produce an `AppPaths`
    /// whose `config` and `state` subdirectories
    /// live under the test dir, so the writer's
    /// `paths.config.join("config.yaml")` lands in
    /// the test fixture and not the operator's
    /// XDG state.
    #[test]
    fn hermetic_paths_root_state_and_config_under_dir() {
        let dir = temp_root("round26-hermetic");
        let paths = hermetic_paths(&dir);
        assert!(paths.config.starts_with(&dir));
        assert!(paths.state.starts_with(&dir));
    }
}
