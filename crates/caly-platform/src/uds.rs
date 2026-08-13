//! Owner-only Unix-domain-socket path validation.
//!
//! Before a daemon binds a UDS it must refuse to remove an arbitrary existing
//! file, refuse a symlink (which could point the bind at a privileged file),
//! and refuse a socket owned by another local user. Validation therefore makes
//! a "stale socket we own" the only existing-path case that may be replaced.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

/// Why an existing UDS path must not be replaced by a fresh bind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdsPathError {
    /// The path exists but is not a socket, so removing it could delete data.
    ExistingNonSocket,
    /// The path is a symlink, which could redirect the bind elsewhere.
    ExistingSymlink,
    /// The path is a socket owned by a different local user.
    OwnedByOther,
}

/// Returns `Ok(())` when `path` may be used for a fresh bind, including when it
/// does not exist. Returns an error when the existing path is unsafe to remove.
pub fn validate_uds_path(path: &Path) -> Result<(), UdsPathError> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Err(UdsPathError::ExistingSymlink);
    }
    if !file_type.is_socket() {
        return Err(UdsPathError::ExistingNonSocket);
    }
    match effective_uid() {
        // Fail closed when the owner cannot be verified rather than replacing
        // a possibly foreign socket.
        None => Err(UdsPathError::OwnedByOther),
        Some(uid) if meta.uid() != uid => Err(UdsPathError::OwnedByOther),
        Some(_) => Ok(()),
    }
}

/// Real user id of the running process, read without unsafe from `/proc/self`.
fn effective_uid() -> Option<u32> {
    std::fs::metadata("/proc/self").ok().map(|meta| meta.uid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_helpers::unique_path_under;
    use std::os::unix::net::UnixListener;

    #[test]
    fn missing_path_is_safe_to_bind() -> Result<(), String> {
        let path = unique_path_under("caly-uds", "missing");
        if validate_uds_path(&path).is_err() {
            return Err("missing path should be safe to bind".to_owned());
        }
        Ok(())
    }

    #[test]
    fn owned_socket_is_safe_to_replace() -> Result<(), String> {
        let path = unique_path_under("caly-uds", "own");
        let _listener = UnixListener::bind(&path).map_err(|e| format!("bind failed: {e}"))?;
        let result = validate_uds_path(&path);
        let _ = std::fs::remove_file(&path);
        if result.is_err() {
            return Err("owned socket should be replaceable".to_owned());
        }
        Ok(())
    }

    #[test]
    fn regular_file_is_rejected() -> Result<(), String> {
        let path = unique_path_under("caly-uds", "file");
        std::fs::write(&path, b"data").map_err(|e| format!("write failed: {e}"))?;
        let result = validate_uds_path(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result, Err(UdsPathError::ExistingNonSocket));
        Ok(())
    }

    #[test]
    fn symlink_is_rejected() -> Result<(), String> {
        let target = unique_path_under("caly-uds", "target");
        let link = unique_path_under("caly-uds", "link");
        std::fs::write(&target, b"x").map_err(|e| format!("write failed: {e}"))?;
        std::os::unix::fs::symlink(&target, &link).map_err(|e| format!("symlink failed: {e}"))?;
        let result = validate_uds_path(&link);
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
        assert_eq!(result, Err(UdsPathError::ExistingSymlink));
        Ok(())
    }
}
