//! Shared bounded application error type used across actor ports and adapters.

use caly_domain::BoundedText;

/// Bounded safe application failure context.
pub type FailureMessage = BoundedText<512>;

/// Failure category returned across actor ownership boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorFailure {
    pub kind: ActorFailureKind,
    pub message: FailureMessage,
    pub suggested_action: FailureMessage,
}

impl core::fmt::Display for ActorFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{} ({})", self.message, self.suggested_action)
    }
}

impl ActorFailure {
    /// Constructs bounded safe failure context.
    pub fn new(
        kind: ActorFailureKind,
        message: impl Into<String>,
        suggested_action: impl Into<String>,
    ) -> Result<Self, caly_domain::TextError> {
        Ok(Self {
            kind,
            message: FailureMessage::new(message)?,
            suggested_action: FailureMessage::new(suggested_action)?,
        })
    }

    /// Constructs an `Infrastructure` failure, UTF-8-safely clamping long or
    /// empty text to the bounded field. This is the single shared constructor
    /// for backends/application `failure` helpers, so the duplicated local
    /// helpers collapse onto one responsibility and never abort the daemon on
    /// an oversized dynamic message.
    pub fn infrastructure(message: &str, suggested_action: &str) -> Self {
        Self {
            kind: ActorFailureKind::Infrastructure,
            // `from_nonempty_clamped` guarantees the bound, so construction cannot fail.
            message: FailureMessage::from_nonempty_clamped(message.to_owned(), "operation failed"),
            suggested_action: FailureMessage::from_nonempty_clamped(
                suggested_action.to_owned(),
                "inspect configuration",
            ),
        }
    }

    /// Constructs any-kind failure with infallible UTF-8-safe clamping. The
    /// `Result`-returning `new` is the right call when a mis-sized input is a
    /// bug that should fail fast during development; this constructor is the
    /// right call when the input is dynamic (a `format!` of an OS error, a
    /// daemon-assembled hint string) and a too-long or empty value must
    /// gracefully degrade to a placeholder instead of aborting the daemon.
    ///
    /// The shared fallback `"_"` keeps both fields non-empty, so the
    /// structured wire representation never carries a "no message" cell that
    /// a thin client would have to special-case.
    pub fn clamped(kind: ActorFailureKind, message: &str, suggested_action: &str) -> Self {
        Self {
            kind,
            message: FailureMessage::from_nonempty_clamped(message.to_owned(), "_"),
            suggested_action: FailureMessage::from_nonempty_clamped(
                suggested_action.to_owned(),
                "_",
            ),
        }
    }
}

/// Stable failure category; concrete I/O errors remain in Infrastructure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorFailureKind {
    InvalidCandidate,
    Unsupported,
    ResourceExhausted,
    DeadlineExceeded,
    Infrastructure,
    GenerationConflict,
    RecoveryRequired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_accepts_short_inputs() {
        let failure = ActorFailure::clamped(ActorFailureKind::Unsupported, "short", "retry");
        assert_eq!(failure.kind, ActorFailureKind::Unsupported);
        assert_eq!(format!("{}", failure.message), "short");
        assert_eq!(format!("{}", failure.suggested_action), "retry");
    }

    #[test]
    fn clamped_replaces_empty_message_with_fallback() {
        let failure = ActorFailure::clamped(ActorFailureKind::Infrastructure, "", "");
        // Empty inputs must still produce a non-empty failure so the wire
        // representation never carries a "no message" cell.
        assert_eq!(format!("{}", failure.message), "_");
        assert_eq!(format!("{}", failure.suggested_action), "_");
    }

    /// Regression: callers used to do
    /// `ActorFailure::new(...).unwrap_or_else(|_| process::abort())` which
    /// would kill the daemon if a dynamic string exceeded the bound. The
    /// infallible `clamped` constructor must clamp to the bound instead.
    #[test]
    fn clamped_truncates_oversized_message_to_bound() {
        // 1_000 four-byte characters = 4_000 bytes; message bound is 512.
        let long: String = "🦀".repeat(1_000);
        let failure = ActorFailure::clamped(
            ActorFailureKind::DeadlineExceeded,
            long.as_str(),
            "short action",
        );
        // `Display` exposes the bounded text directly.
        let message = format!("{}", failure.message);
        assert!(message.len() <= 512, "message len = {}", message.len());
        assert!(message.is_char_boundary(message.len()));
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
        // The action was short and survived unchanged.
        assert_eq!(format!("{}", failure.suggested_action), "short action");
    }
}
