//! Core configuration models and renderers (role crate, layer 2).
//!
//! Everything here is pure: domain values in, rendered config bytes out, zero
//! I/O. Spawn, control, validation and telemetry of the proxy cores live in
//! `caly-corectl`; intake of remote subscription bodies lives in
//! `caly-subscription`(P6 拆出;当前在 caly-profile 内). See
//! `docs/crate-replan.md` v4 §5.1/§5.2 for the evidence-based boundary.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod mihomo;
pub mod rules;
pub mod sing_box;
mod labels;

/// Render-side failure: a message plus the operator-facing suggested action.
///
/// Mirrors the display shape the pre-split callers surfaced through
/// `caly_corectl`'s `KernelFailure` ("<message> (<hint>)") so downstream
/// diagnostics stay byte-identical after the P3a move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderFailure {
    pub message: String,
    pub hint: String,
}

impl core::fmt::Display for RenderFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{} ({})", self.message, self.hint)
    }
}

impl std::error::Error for RenderFailure {}

/// Builds a [`RenderFailure`]; the pre-split twin of
/// `caly_corectl::common::config_failure`.
pub(crate) fn config_failure(message: &str, hint: &str) -> RenderFailure {
    RenderFailure {
        message: message.to_owned(),
        hint: hint.to_owned(),
    }
}
