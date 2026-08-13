//! Argument-vector builders for the TUN backend's `ip` commands.
//!
//! Round 31: the `tun_command` / `build_arguments` /
//! `build_arguments_strs` / `build_batch_script` /
//! `first_line` helpers used to live in
//! `linux.rs` (an 871-line monolith that exceeded
//! the 400-line hard cap — S4.1 advisory). They
//! extract into this file so the engage /
//! escalate / device modules can all reuse the
//! same `CommandArguments` shape and the same
//! shell-safe batch-script joiner without
//! dragging the orchestration with them. The
//! helpers are pure functions over bounded text
//! and shell-safe kernel tokens; the file has no
//! state of its own and no tests beyond the
//! contract checks for the batch-script joiner
//! (the rest of the helpers are exercised
//! transitively by the `device` and `escalate`
//! integration tests).

use caly_domain::BoundedText;

use crate::PlatformFailure;
use crate::command::CommandArguments;

use super::capability::failure;

/// Round 31: builds one `ip` command's arguments as
/// owned bounded strings (the `tun_command` shape
/// the engage / restore pipelines pass around). The
/// call sites in `device.rs` build a `Vec<String>`
/// of `ip` subcommand tokens + interface name +
/// mode + MTU; the bounded wrapping is here so the
/// `CommandArguments::try_push` call site stays
/// thin.
pub(super) fn tun_command(values: Vec<&str>) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

/// Round 31: builds bounded arguments from owned
/// strings. The inverse of [`build_arguments`]:
/// `device.rs` already owns the `Vec<String>`
/// (it built it via [`tun_command`]) and only
/// needs the bounded-text wrap.
pub(super) fn build_arguments_strs(values: &[String]) -> Result<CommandArguments, PlatformFailure> {
    let mut arguments = CommandArguments::new();
    for value in values {
        let argument = BoundedText::new(value.clone())
            .map_err(|_| failure("TUN command argument is too long"))?;
        arguments
            .try_push(argument)
            .map_err(|_| failure("TUN command argument list is full"))?;
    }
    Ok(arguments)
}

/// Round 31: builds bounded arguments from
/// borrowed `&str` slices. The single-shot
/// escalate path uses this shape (the
/// pre-Round 31 inline `build_arguments(&[&str])`
/// helper); the multi-shot sequence path uses
/// the `Vec<String>` flavour above.
pub(super) fn build_arguments(values: &[&str]) -> Result<CommandArguments, PlatformFailure> {
    let mut arguments = CommandArguments::new();
    for value in values {
        let argument = BoundedText::new((*value).to_owned())
            .map_err(|_| failure("TUN command argument is too long"))?;
        arguments
            .try_push(argument)
            .map_err(|_| failure("TUN command argument list is full"))?;
    }
    Ok(arguments)
}

/// Round 31: joins commands into one
/// `ip ... && ip ...` script for `sh -c`. Only
/// the interface name and MTU are interpolated;
/// the interface name is validated as a plain
/// kernel token so hostile input cannot escape
/// the shell. The pre-Round 31 helper lived in
/// the same file as the engage pipeline; the
/// extraction keeps the safety check (the
/// alphanumeric / `-` / `_` / `.` allow-list)
/// near the rest of the shell-quoting code.
pub(super) fn build_batch_script(commands: &[Vec<String>]) -> Result<String, PlatformFailure> {
    let mut script = String::new();
    for (index, command) in commands.iter().enumerate() {
        if index > 0 {
            script.push_str(" && ");
        }
        script.push_str("ip");
        for argument in command {
            if !argument.is_empty()
                && argument
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            {
                script.push(' ');
                script.push_str(argument);
            } else {
                return Err(failure("TUN interface name is not a safe kernel token"));
            }
        }
    }
    Ok(script)
}

/// Round 31: first stderr line, bounded and
/// prefixed for embedding in the failure. The
/// pre-Round 31 inline helper was 160-char
/// bounded to keep the failure envelope short;
/// the new module-level doc-comment records the
/// rationale.
pub(super) fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        String::new()
    } else {
        let bounded: String = line.chars().take(160).collect();
        format!(": {bounded}")
    }
}

#[cfg(test)]
mod script_tests {
    //! Round 31: the batch-script joiner is the
    //! single shell-quoting surface for the TUN
    //! backend (every escalated engage goes
    //! through `build_batch_script` → `sh -c`).
    //! A regression here would be a shell
    //! injection or a missing `&&` chain, so
    //! the tests pin the script shape and the
    //! safe-token reject.

    use super::build_batch_script;

    /// A two-command sequence joins with
    /// `&&` and prefixes each command with
    /// `ip`. The pre-Round 31 inline assertion
    /// was the same; the extraction is
    /// purely a file-organisation change.
    #[test]
    fn batch_script_joins_commands_with_and() -> Result<(), String> {
        let script = build_batch_script(&[
            vec![
                "tuntap".to_owned(),
                "add".to_owned(),
                "dev".to_owned(),
                "caly0".to_owned(),
                "mode".to_owned(),
                "tun".to_owned(),
            ],
            vec![
                "link".to_owned(),
                "set".to_owned(),
                "dev".to_owned(),
                "caly0".to_owned(),
                "up".to_owned(),
            ],
        ])
        .map_err(|e| e.to_string())?;
        assert_eq!(
            script,
            "ip tuntap add dev caly0 mode tun && ip link set dev caly0 up"
        );
        Ok(())
    }

    /// A hostile interface name must not
    /// reach the shell. The pre-Round 31
    /// inline assertion locked this; the
    /// extraction is a no-op for the contract.
    #[test]
    fn batch_script_rejects_unsafe_tokens() {
        assert!(
            build_batch_script(&[vec![
                "link".to_owned(),
                "set".to_owned(),
                "dev".to_owned(),
                "x;rm -rf /".to_owned(),
                "up".to_owned()
            ]])
            .is_err()
        );
    }

    /// Round 31: the first-line stderr
    /// projection stays bounded to 160 chars
    /// so a kernel panic line cannot bloat
    /// the JSON failure envelope. The
    /// pre-Round 31 inline helper was
    /// identically bounded; the new
    /// module-level test pins the contract.
    #[test]
    fn first_line_bounds_stderr_detail() {
        use super::first_line;
        assert_eq!(first_line(b""), "");
        assert_eq!(
            first_line(b"Operation not permitted\nmore\n"),
            ": Operation not permitted"
        );
        let mut long = b"prefix: ".to_vec();
        long.extend(std::iter::repeat_n(b'x', 2_000));
        let line = first_line(&long);
        // 160 chars + the leading `": "`
        assert!(line.len() <= 162, "first_line must be bounded: {line:?}");
    }
}
