//! Linux-compatible owner-token instance lock backend.
//!
//! The lock is acquired with an atomic `create_new` operation. The file remains
//! owned by the process and contains a token, so release can never remove a
//! replacement lock created by another owner.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use super::{
    reclaim::failure, reclaim::owner_token, reclaim::reclaim_stale, InstanceLock,
    InstanceLockBackend, LockOwner,
};
use crate::PlatformFailure;

/// File-backed instance-lock backend for Linux.
#[derive(Default)]
pub struct LinuxInstanceLockBackend;

/// Grace period (per attempt) given to an owner that is exiting gracefully.
/// `kill` / Ctrl-C deliver SIGTERM; the daemon tears down its core and
/// releases the lock, which can take a few hundred ms. Without this window,
/// "restart right after kill" fails even though the old owner is leaving.
const RECLAIM_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(80);

/// Upper bound on reclaim/retry attempts before declaring the lock live-held.
/// 40 x 80ms = a ~3.2s grace window: a SIGTERM teardown (stop core, release
/// lock, restore platform effects) can legitimately take a couple of seconds
/// when the kernel is slow to reap; the 1.2s window (15 attempts) made
/// "restart right after kill" fail spuriously. A genuinely live lock still
/// fails fast after ~3.2s with the same clear message as before.
const RECLAIM_RETRY_LIMIT: usize = 40;

/// An acquired Linux instance lock.
pub struct LinuxInstanceLock {
    file: File,
    path: PathBuf,
    owner: LockOwner,
}

impl InstanceLockBackend for LinuxInstanceLockBackend {
    fn acquire(
        &mut self,
        path: PathBuf,
        owner: LockOwner,
    ) -> Result<Box<dyn InstanceLock>, PlatformFailure> {
        match Self::acquire_create_new(&path, owner) {
            Ok(lock) => Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::acquire_with_reclaim_window(&path, owner)
            }
            Err(error) => Err(failure(
                "acquire-lock",
                &path,
                format!("cannot create owner-only lock: {error}"),
                "stop the existing daemon or choose another runtime path",
            )),
        }
    }
}

impl LinuxInstanceLockBackend {
    /// Reclaims a stale lock or waits out a graceful-shutdown race.
    ///
    /// The lock file survives `kill -9`; `reclaim_stale` removes it once the
    /// recorded owner PID+start identity is gone. But a SIGTERM shutdown also
    /// races: the owner is still alive (token still "live") while it tears
    /// down, so a fresh daemon started immediately after would otherwise fail
    /// with "held by a live process". This loop retries a bounded number of
    /// times, re-probing both the re-acquire and the reclaim on each pass,
    /// then fails with the same actionable message as before.
    fn acquire_with_reclaim_window(
        path: &PathBuf,
        owner: LockOwner,
    ) -> Result<Box<dyn InstanceLock>, PlatformFailure> {
        for attempt in 0..=RECLAIM_RETRY_LIMIT {
            if attempt > 0 {
                std::thread::sleep(RECLAIM_RETRY_DELAY);
            }
            // Owner may have released (graceful) or died (kill -9) since the
            // previous attempt; reclaim removes the stale file when the
            // recorded identity is gone.
            if reclaim_stale(path)? {
                continue;
            }
            match Self::acquire_create_new(path, owner) {
                Ok(lock) => return Ok(lock),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(other) => {
                    return Err(failure(
                        "acquire-lock",
                        path,
                        format!("cannot create owner-only lock after reclaim: {other}"),
                        "stop the existing daemon or choose another runtime path",
                    ));
                }
            }
        }
        Err(failure(
            "acquire-lock",
            path,
            "instance lock is held by a live process".to_owned(),
            "stop the existing daemon or choose another runtime path",
        ))
    }
}

impl LinuxInstanceLockBackend {
    fn acquire_create_new(
        path: &PathBuf,
        owner: LockOwner,
    ) -> std::io::Result<Box<dyn InstanceLock>> {
        ensure_parent_dir(path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        let token = owner_token(owner);
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        Ok(Box::new(LinuxInstanceLock {
            file,
            path: path.clone(),
            owner,
        }))
    }
}

impl InstanceLock for LinuxInstanceLock {
    fn owner(&self) -> LockOwner {
        self.owner
    }

    fn release(mut self: Box<Self>) -> Result<(), PlatformFailure> {
        let mut contents = String::new();
        self.file.seek(SeekFrom::Start(0)).map_err(|error| {
            failure(
                "read-lock",
                &self.path,
                format!("cannot seek lock owner token: {error}"),
                "inspect the runtime lock file before retrying",
            )
        })?;
        self.file.read_to_string(&mut contents).map_err(|error| {
            failure(
                "read-lock",
                &self.path,
                format!("cannot read lock owner token: {error}"),
                "inspect the runtime lock file before retrying",
            )
        })?;
        if contents != owner_token(self.owner) {
            return Err(failure(
                "release-lock",
                &self.path,
                "lock owner token changed while held".to_owned(),
                "do not remove the lock manually; investigate concurrent ownership",
            ));
        }
        drop(self.file);
        fs::remove_file(&self.path).map_err(|error| {
            failure(
                "release-lock",
                &self.path,
                format!("cannot remove owner lock: {error}"),
                "remove the lock only after confirming the daemon is stopped",
            )
        })
    }
}

/// Creates the lock file's parent directory owner-only when it does not exist,
/// so a fresh XDG runtime root can host the lock without manual setup.
///
/// Permissions are tightened only for directories this function creates: a
/// pre-existing parent (e.g. the system `/tmp` or an XDG runtime root the user
/// already provisioned) is never chmod'ed, and a symlinked parent is rejected
/// outright so no path we own ever follows one.
fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    if parent.exists() {
        #[cfg(unix)]
        {
            let metadata = std::fs::symlink_metadata(parent)?;
            if metadata.file_type().is_symlink() {
                return Err(std::io::Error::other(
                    "refusing to use a symlinked runtime parent directory",
                ));
            }
        }
        return Ok(());
    }
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::current_process_start_id;
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn owner(seed: u8) -> LockOwner {
        LockOwner {
            pid: std::process::id(),
            process_start_id: current_process_start_id().unwrap_or(u64::from(seed)),
            owner_token: [seed; 16],
        }
    }

    fn path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("caly-lock-{nanos}"))
    }

    #[test]
    fn acquire_is_atomic_and_release_removes_only_owned_lock() -> Result<(), PlatformFailure> {
        let path = path();
        let mut backend = LinuxInstanceLockBackend;
        let lock = backend.acquire(path.clone(), owner(1))?;
        assert_eq!(lock.owner(), owner(1));
        assert!(backend.acquire(path.clone(), owner(2)).is_err());
        lock.release()?;
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn release_rejects_changed_owner_token() -> Result<(), PlatformFailure> {
        let path = path();
        let mut backend = LinuxInstanceLockBackend;
        let lock = backend.acquire(path.clone(), owner(3))?;
        assert!(fs::write(&path, owner_token(owner(4))).is_ok());
        assert!(lock.release().is_err());
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn stale_lock_from_dead_owner_is_reclaimed() -> Result<(), PlatformFailure> {
        let path = path();
        // A lock owned by a dead PID (999999 + a start id) is stale and must be
        // reclaimed so the new daemon can start.
        fs::write(&path, format!("999999:1:{}\n", "aa".repeat(16))).map_err(|_| {
            failure(
                "setup",
                &path,
                "cannot write stale lock".to_owned(),
                "inspect runtime dir",
            )
        })?;
        let mut backend = LinuxInstanceLockBackend;
        let lock = backend.acquire(path.clone(), owner(5))?;
        assert_eq!(lock.owner(), owner(5));
        lock.release()?;
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn acquire_creates_missing_parent_directory() -> Result<(), PlatformFailure> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let dir = std::env::temp_dir().join(format!("caly-lock-dir-{nanos}"));
        let path = dir.join("nested/daemon.lock");
        let mut backend = LinuxInstanceLockBackend;
        let lock = backend.acquire(path.clone(), owner(9))?;
        assert!(path.exists());
        lock.release()?;
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn live_lock_is_not_reclaimed() -> Result<(), PlatformFailure> {
        let path = path();
        // Our own (live) PID with its real start identity must not be reclaimed.
        let live_start = current_process_start_id().map_err(|_| {
            failure(
                "setup",
                &path,
                "cannot read process start id".to_owned(),
                "inspect /proc",
            )
        })?;
        let token = format!("{}:{live_start}:{}\n", std::process::id(), "bb".repeat(16));
        fs::write(&path, token).map_err(|_| {
            failure(
                "setup",
                &path,
                "cannot write lock".to_owned(),
                "inspect runtime dir",
            )
        })?;
        let mut backend = LinuxInstanceLockBackend;
        let result = backend.acquire(path.clone(), owner(6));
        assert!(result.is_err(), "live lock must not be reclaimed");
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn acquire_retries_through_graceful_shutdown_window() -> Result<(), PlatformFailure> {
        // Simulates the "restart immediately after kill" race: a helper (sh)
        // claims the lock with its own live PID/start identity, holds it for
        // a few hundred ms as if tearing down, then exits without releasing.
        // The retry window must reclaim it instead of reporting a live lock.
        let path = path();
        let lock_path = path.display();
        let script = format!(
            "p=$$; s=$(awk '{{print $22}}' /proc/$p/stat); printf '%s:%s:%s\n' $p $s aaaaaaaaaaaaaaaa > {lock_path}; sleep 0.3"
        );
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .spawn()
            .map_err(|error| {
                failure(
                    "setup",
                    &path,
                    format!("cannot spawn helper: {error}"),
                    "inspect test env",
                )
            })?;
        std::thread::sleep(RECLAIM_RETRY_DELAY);
        let mut backend = LinuxInstanceLockBackend;
        // The child is alive for ~0.25s more; the window must wait it out
        // rather than fail with "held by a live process".
        let lock = backend.acquire(path.clone(), owner(7))?;
        assert!(path.exists());
        assert_eq!(lock.owner(), owner(7));
        lock.release()?;
        assert!(!path.exists());
        child.wait().ok();
        Ok(())
    }
}
