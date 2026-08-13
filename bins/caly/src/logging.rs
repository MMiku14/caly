//! Structured logging bootstrap for the daemon and CLI.
//!
//! Installs a `tracing` subscriber that writes timestamped, level-filtered,
//! human-readable log lines to stderr. The filter is taken from `CALY_LOG` when
//! present, then `RUST_LOG`, then a conservative default (`info`) so a daemon
//! does not flood its log in the common case. Structured fields (daemon id,
//! operation id, core) attach to the surrounding events for correlation.
//!
//! The subscriber is installed at most once; callers that spawn a client (a
//! separate process, or before the daemon initializes) are safe to call this
//! again because it is idempotent and never panics on a duplicate install.

use tracing_subscriber::EnvFilter;

/// Installs the process-wide tracing subscriber, honoring `CALY_LOG` then
/// `RUST_LOG`. Returns whether a subscriber is now active for this process.
pub fn init() -> bool {
    let filter = std::env::var("CALY_LOG")
        .ok()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .or_else(|| Some(crate::daemon_config::log_level()))
        .unwrap_or_else(|| "info".to_owned());
    // Audit #97: `EnvFilter::new` *panics* on an unparseable directive, and
    // the directive comes from the process environment — one bad `CALY_LOG`
    // value used to abort every caly process at startup. Fall back to the
    // default level instead.
    let filter = EnvFilter::try_new(with_third_party_caps(filter)).unwrap_or_else(|error| {
        eprintln!("caly: ignoring invalid log filter ({error}); using `info`");
        EnvFilter::new("info")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init()
        .is_ok()
}

/// Caps verbose third-party transport crates at `info` so a `debug` filter
/// surfaces caly business events instead of per-frame h2/hyper noise. A target
/// the user already configured explicitly is left untouched.
fn with_third_party_caps(filter: String) -> String {
    let mut out = filter;
    for target in ["h2", "hyper", "tonic", "tower"] {
        // Audit #107: also treat `target::sub=level` (a more specific module
        // under the cap) as "already configured" — capping `h2` when the
        // operator set `h2::codec=debug` was untidy even though EnvFilter's
        // specificity rules made the outcome accidentally correct.
        let already_configured = out.split(',').any(|directive| {
            let trimmed = directive.trim();
            trimmed == target
                || trimmed
                    .strip_prefix(target)
                    .is_some_and(|rest| rest.starts_with('=') || rest.starts_with("::"))
        });
        if !already_configured {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!(",{target}=info"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplicate install must not panic (idempotent) and only the first wins.
    #[test]
    fn init_is_idempotent_and_does_not_panic() {
        // Whichever call wins, neither may panic; a process may call init more
        // than once (e.g. client then daemon paths).
        let _first = init();
        let _second = init();
        // A structured event through the subscriber must also not panic.
        tracing::info!("structured logging initialized for test");
    }

    #[test]
    fn third_party_caps_skip_sub_module_directives() {
        // Audit #107: a `h2::codec=debug` user directive counts as
        // configured for the `h2` cap.
        let capped = with_third_party_caps("debug,h2::codec=debug".to_owned());
        assert!(!capped.contains("h2=info"));
        assert!(capped.contains("h2::codec=debug"));
        assert!(capped.contains("hyper=info"));
    }

    #[test]
    fn third_party_caps_append_without_overriding_user_directives() {
        let capped = with_third_party_caps("debug".to_owned());
        assert!(capped.contains("h2=info"));
        assert!(capped.contains("hyper=info"));
        assert!(capped.starts_with("debug"));
        // Explicit user configuration of a noisy target is preserved.
        let preserved = with_third_party_caps("debug,h2=debug".to_owned());
        assert!(!preserved.contains("h2=info"));
        assert!(preserved.contains("h2=debug"));
        assert!(preserved.contains("hyper=info"));
    }
}
