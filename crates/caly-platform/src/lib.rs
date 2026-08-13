//! Cross-platform infrastructure boundary.
//!
//! Concrete OS backends must live in this crate. Current modules define
//! truthful transaction and ownership contracts; unsupported operations must
//! return structured failure rather than empty success.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod command;
pub mod entropy;
mod error;
pub mod fs;
pub mod instance_lock;
pub mod node_selection;
pub mod paths;
pub mod process;
pub mod recovery;
pub mod secrets;
pub mod tun;
pub mod uds;

pub use error::PlatformFailure;

/// Builds a bounded text value, falling back to a static label on overflow and
/// aborting only when even the fallback overflows (a fixed-text invariant
/// violation). Shared across platform backends to avoid per-file duplication.
#[allow(clippy::panic)] // unreachable-by-construction: every caller's fallback literal fits MAX
pub(crate) fn bounded_text<const MAX: usize>(
    value: impl Into<String>,
    fallback: &'static str,
) -> caly_domain::BoundedText<MAX> {
    match caly_domain::BoundedText::new(value.into()) {
        Ok(value) => value,
        Err(_) => match caly_domain::BoundedText::new(fallback.to_owned()) {
            Ok(value) => value,
            // A failing fallback is a fixed-text invariant
            // violation in the CALLER; the pre-#53 shape aborted
            // the whole daemon for it, taking the control plane
            // down over a presentation bug.
            Err(error) => {
                unreachable!("bounded_text fallback `{fallback}` must fit capacity {MAX}: {error}")
            }
        },
    }
}
