//! Privilege escalation pipeline for the TUN backend.
//!
//! Round 31: the TUN backend's `run_ip` /
//! `run_ip_sequence` / `escalation_prefixes` /
//! `attempt` / `attempt_batch` helpers used to
//! live in `linux.rs` (an 871-line monolith that
//! exceeded the 400-line hard cap — S4.1
//! advisory). The escalation policy and the
//! per-attempt / per-batch runners extract into
//! this file so the engage / restore pipelines
//! in `device.rs` can drive the right policy
//! without dragging the 200-line per-attempt
//! mechanics with them.
//!
//! The policy: try the `ip` command directly
//! first (a daemon with `CAP_NET_ADMIN` never
//! prompts), then escalate through the configured
//! chain (`pkexec` for desktop polkit, then
//! `sudo -n` for headless / CI). Batching the
//! remaining commands as one `sh -c` script keeps
//! the operator's `pkexec` prompts to one per
//! engage rather than one per `ip` subcommand.

use std::path::PathBuf;

use caly_domain::BoundedText;

use crate::PlatformFailure;
use crate::command::{CommandArguments, CommandRequest, CommandRunner};

use super::capability::failure;
use super::script::{build_arguments, build_arguments_strs, build_batch_script, first_line};

/// Direct timeout for one `ip` command; escalation
/// prompts may wait for the user, so escalated
/// attempts get a longer budget.
pub(super) const DIRECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
pub(super) const ESCALATED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Round 31: the privilege-escalation policy the
/// TUN backend threads through every `ip`
/// command. The pre-Round 31 enum lived at the
/// top of `linux.rs`; the extraction into this
/// file keeps the policy declaration next to the
/// runners that consume it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TunEscalation {
    /// Direct first, then `pkexec` (desktop polkit prompt), then `sudo -n`.
    #[default]
    Auto,
    /// Escalate only through `pkexec`.
    Pkexec,
    /// Escalate only through passwordless `sudo -n`.
    Sudo,
    /// Never escalate; report remediation instead.
    None,
}

impl TunEscalation {
    /// Round 31: stable diagnostic label. The
    /// pre-Round 31 inline `label()` method
    /// lived on the same enum in `linux.rs`;
    /// the extraction keeps the test
    /// `escalation_labels_are_stable` green.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pkexec => "pkexec",
            Self::Sudo => "sudo",
            Self::None => "none",
        }
    }
}

/// Round 31: runs one `ip` command, escalating
/// through the policy when the direct attempt
/// fails (typically a missing `CAP_NET_ADMIN`).
pub(super) fn run_ip(
    runner: &mut impl CommandRunner,
    escalation: TunEscalation,
    values: Vec<&str>,
) -> Result<(), PlatformFailure> {
    let arguments = build_arguments(&values)?;
    // Audit #93: chain failures so the returned error is the LAST attempt's
    // real cause (a cancelled pkexec prompt, a missing sudo binary), not the
    // stale "operation not permitted" from the direct try that shadowed the
    // actual escalation failure behind a misleading remediation.
    let mut last = match attempt(runner, "ip", &[], &arguments, DIRECT_TIMEOUT) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    for (executable, prefix) in escalation_prefixes(escalation) {
        match attempt(runner, executable, prefix, &arguments, ESCALATED_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Round 31: runs an ordered sequence of `ip`
/// commands, trying every command directly first
/// and, on the first direct failure, escalating
/// the REMAINING commands as one `sh -c` batch.
/// Batching matters: without it, an escalated
/// engage prompts for the root password once per
/// `ip` command (up to four prompts); with it
/// the user authenticates at most once.
pub(super) fn run_ip_sequence(
    runner: &mut impl CommandRunner,
    escalation: TunEscalation,
    commands: &[Vec<String>],
) -> Result<(), PlatformFailure> {
    // Phase 1: direct attempts in order; commands before the first failure
    // were applied without privileges and must not run again escalated.
    let mut pending_from = 0_usize;
    for (index, command) in commands.iter().enumerate() {
        let arguments = build_arguments_strs(command)?;
        if attempt(runner, "ip", &[], &arguments, DIRECT_TIMEOUT).is_err() {
            pending_from = index;
            break;
        }
        pending_from = index + 1;
    }
    if pending_from == commands.len() {
        return Ok(());
    }
    let script = build_batch_script(&commands[pending_from..])?;
    let mut script_argument = CommandArguments::new();
    let script_text =
        BoundedText::new(script).map_err(|_| failure("TUN escalation script is too long"))?;
    script_argument
        .try_push(script_text)
        .map_err(|_| failure("TUN escalation argument list is full"))?;
    for (executable, prefix) in escalation_prefixes(escalation) {
        if attempt_batch(
            runner,
            executable,
            prefix,
            &script_argument,
            ESCALATED_TIMEOUT,
        )
        .is_ok()
        {
            return Ok(());
        }
    }
    Err(failure(
        "Linux ip TUN commands failed and every escalation attempt failed",
    ))
}

/// Round 31: ordered escalation candidates for a
/// policy. Spawn failures (missing binary) are
/// handled by the caller skipping a failed
/// attempt, so availability needs no separate
/// probing.
pub(super) fn escalation_prefixes(
    policy: TunEscalation,
) -> Vec<(&'static str, &'static [&'static str])> {
    match policy {
        TunEscalation::Auto => vec![("pkexec", &[] as &[_]), ("sudo", &["-n"])],
        TunEscalation::Pkexec => vec![("pkexec", &[] as &[_])],
        TunEscalation::Sudo => vec![("sudo", &["-n"])],
        TunEscalation::None => Vec::new(),
    }
}

/// Round 31: executes one attempt —
/// `executable [prefix...] ip [arguments...]`.
pub(super) fn attempt(
    runner: &mut impl CommandRunner,
    executable: &str,
    prefix: &[&str],
    arguments: &CommandArguments,
    timeout: std::time::Duration,
) -> Result<(), PlatformFailure> {
    let mut full = CommandArguments::new();
    for value in prefix {
        let argument = BoundedText::new((*value).to_owned())
            .map_err(|_| failure("TUN escalation argument is too long"))?;
        full.try_push(argument)
            .map_err(|_| failure("TUN escalation argument list is full"))?;
    }
    // The `ip` program name is the first argument once a helper (pkexec/sudo)
    // fronts the command; it is the executable for the direct attempt.
    let direct = executable == "ip" && prefix.is_empty();
    if !direct {
        let ip_argument = BoundedText::new("ip".to_owned())
            .map_err(|_| failure("TUN escalation argument is too long"))?;
        full.try_push(ip_argument)
            .map_err(|_| failure("TUN escalation argument list is full"))?;
    }
    for argument in arguments {
        full.try_push(argument.clone())
            .map_err(|_| failure("TUN command argument list is full"))?;
    }
    let program = if direct { "ip" } else { executable };
    let result = runner.run_bounded(CommandRequest {
        executable: PathBuf::from(program),
        arguments: full,
        timeout,
    })?;
    if result.exit_code == Some(0) {
        Ok(())
    } else {
        let detail = first_line(&result.stderr);
        Err(failure(&format!("Linux ip TUN command failed{detail}")))
    }
}

/// Round 31: executes one escalated `sh -c <script>`
/// batch — the single password prompt that
/// replaces per-command escalation during a TUN
/// engage.
pub(super) fn attempt_batch(
    runner: &mut impl CommandRunner,
    executable: &str,
    prefix: &[&str],
    script: &CommandArguments,
    timeout: std::time::Duration,
) -> Result<(), PlatformFailure> {
    let mut full = CommandArguments::new();
    for value in prefix {
        let argument = BoundedText::new((*value).to_owned())
            .map_err(|_| failure("TUN escalation argument is too long"))?;
        full.try_push(argument)
            .map_err(|_| failure("TUN escalation argument list is full"))?;
    }
    for value in ["sh", "-c"] {
        let argument = BoundedText::new(value.to_owned())
            .map_err(|_| failure("TUN escalation argument is too long"))?;
        full.try_push(argument)
            .map_err(|_| failure("TUN escalation argument list is full"))?;
    }
    for argument in script {
        full.try_push(argument.clone())
            .map_err(|_| failure("TUN command argument list is full"))?;
    }
    let result = runner.run_bounded(CommandRequest {
        executable: PathBuf::from(executable),
        arguments: full,
        timeout,
    })?;
    if result.exit_code == Some(0) {
        Ok(())
    } else {
        let detail = first_line(&result.stderr);
        Err(failure(&format!(
            "Linux ip TUN batch command failed{detail}"
        )))
    }
}

#[cfg(test)]
mod escalate_tests {
    //! Round 31: the escalation policy + the
    //! per-attempt runners are the single source
    //! of truth for the TUN backend's
    //! "one-password-prompt" guarantee. The tests
    //! pin the policy labels (the JSON error
    //! envelope surfaces `tun.escalation` to
    //! `doctor` / `set`); the batching behaviour
    //! is exercised transitively by the
    //! `engage_without_privileges_escalates_one_batch_not_per_command`
    //! test in `device.rs`.

    use super::TunEscalation;

    #[test]
    fn escalation_labels_are_stable() {
        assert_eq!(TunEscalation::Auto.label(), "auto");
        assert_eq!(TunEscalation::Pkexec.label(), "pkexec");
        assert_eq!(TunEscalation::Sudo.label(), "sudo");
        assert_eq!(TunEscalation::None.label(), "none");
    }

    /// The escalation chain shape is the
    /// operator-visible contract: `Auto`
    /// tries `pkexec` before `sudo -n`,
    /// `Pkexec` / `Sudo` are 1-element
    /// chains, `None` is empty. A future
    /// change to the order (e.g. "try
    /// passwordless sudo before polkit")
    /// would land in `escalation_prefixes`
    /// and the test would surface the
    /// change.
    #[test]
    fn escalation_prefixes_match_documented_order() {
        use super::escalation_prefixes;
        let auto = escalation_prefixes(TunEscalation::Auto);
        assert_eq!(auto.len(), 2);
        assert_eq!(auto[0].0, "pkexec");
        assert_eq!(auto[1].0, "sudo");

        let pkexec = escalation_prefixes(TunEscalation::Pkexec);
        assert_eq!(pkexec.len(), 1);
        assert_eq!(pkexec[0].0, "pkexec");

        let sudo = escalation_prefixes(TunEscalation::Sudo);
        assert_eq!(sudo.len(), 1);
        assert_eq!(sudo[0].0, "sudo");
        assert_eq!(sudo[0].1, &["-n"]);

        let none = escalation_prefixes(TunEscalation::None);
        assert!(none.is_empty());
    }
}
