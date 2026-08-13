//! Linux process identity helpers for PID-reuse-safe ownership.

use std::io;

/// Reads Linux `/proc/<pid>/stat` start time in clock ticks.
pub fn process_start_id(pid: u32) -> io::Result<u64> {
    let value = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after_command = value
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat"))?;
    after_command
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process start id"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid process start id"))
}

/// Reads the current process start identity.
pub fn current_process_start_id() -> io::Result<u64> {
    process_start_id(std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_has_a_stable_start_identity() -> io::Result<()> {
        let first = current_process_start_id()?;
        let second = current_process_start_id()?;
        assert_eq!(first, second);
        Ok(())
    }
}
