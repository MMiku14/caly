//! Stale instance-lock reclaim for Linux.

use std::path::Path;

use super::{process_start_id, LockOwner};
use crate::bounded_text as bounded;
use crate::PlatformFailure;

/// Reads the process state character (field 3 of `/proc/<pid>/stat`).
fn process_state(pid: u32) -> std::io::Result<char> {
    let value = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after_command = value
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid proc stat"))?;
    after_command
        .split_whitespace()
        .next()
        .and_then(|state| state.chars().next())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing state"))
}

/// Removes a lock whose recorded owner process is dead (PID-reuse safe via the
/// recorded process start identity).
///
/// A zombie (`Z`) counts as dead: the process has exited, and if its parent
/// never reaps it the `/proc/<pid>` entry lingers. Treating "exists" as
/// live would let an un-reaped daemon child block restarts forever — exactly
/// the observed "restart right after kill" failure.
pub(crate) fn reclaim_stale(path: &Path) -> Result<bool, PlatformFailure> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Ok(false); // disappeared between probe and reclaim
    };
    let Some((pid, start_id)) = parse_owner(contents.trim_end()) else {
        // Unparseable lock file cannot be proven stale; require manual action.
        return Err(failure(
            "reclaim-stale-lock",
            path,
            "lock file has an unparseable owner token".to_owned(),
            "inspect the runtime lock file before retrying",
        ));
    };
    if pid == 0 {
        return Ok(false);
    }
    let proc_dir = format!("/proc/{pid}");
    let exists = Path::new(&proc_dir).exists();
    let same_start = process_start_id(pid).is_ok_and(|actual| actual == start_id);
    // A zombie holds its PID but is dead: reclaim. A live process with a
    // matching start identity is a genuine owner: never steal its lock.
    let zombie = exists && process_state(pid).is_ok_and(|state| state == 'Z');
    if exists && !zombie && same_start {
        return Ok(false); // live owner, not stale
    }
    // Owner is dead (process gone, zombie, or start identity no longer
    // matches). Remove the stale lock.
    std::fs::remove_file(path).map_err(|error| {
        failure(
            "reclaim-stale-lock",
            path,
            format!("cannot remove stale lock: {error}"),
            "inspect the runtime lock file before retrying",
        )
    })?;
    Ok(true)
}

/// Parses `pid:start_id:<token>` back into its owner identity fields.
pub(crate) fn parse_owner(token: &str) -> Option<(u32, u64)> {
    let mut parts = token.splitn(3, ':');
    let pid = parts.next()?.parse().ok()?;
    let start_id = parts.next()?.parse().ok()?;
    Some((pid, start_id))
}

/// Formats a lock file line from an owner.
pub(crate) fn owner_token(owner: LockOwner) -> String {
    let mut token = format!("{}:{}:", owner.pid, owner.process_start_id);
    token.push_str(&caly_domain::to_hex(owner.owner_token));
    token.push('\n');
    token
}

pub(crate) fn failure(
    operation: &'static str,
    path: &Path,
    message: String,
    action: &'static str,
) -> PlatformFailure {
    PlatformFailure {
        operation: bounded(operation.to_owned(), "lock-operation"),
        resource: bounded(path.display().to_string(), "lock-path"),
        message: bounded(message, "platform lock operation failed"),
        suggested_action: bounded(action.to_owned(), "inspect the runtime lock"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_decodes_pid_and_start_id() {
        let result = parse_owner("123:456:deadbeef");
        assert_eq!(result, Some((123, 456)));
    }

    #[test]
    fn parse_owner_rejects_malformed_token() {
        assert!(parse_owner("not-a-token").is_none());
        assert!(parse_owner("1:notanum:xx").is_none());
    }
}
