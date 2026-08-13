//! Durable owner-only atomic replacement transaction.

pub mod linux;
pub use linux::LinuxAtomicFileBackend;

use std::path::{Path, PathBuf};

use caly_domain::BoundedVec;

use crate::PlatformFailure;

/// Maximum atomic file payload accepted by this generic path.
pub const MAX_ATOMIC_FILE_BYTES: usize = 16 * 1_024 * 1_024;
/// Capacity-enforced file contents.
pub type AtomicFileContents = BoundedVec<u8, MAX_ATOMIC_FILE_BYTES>;

/// Same-directory replacement plan with a unique owner token.
pub struct AtomicWritePlan {
    pub destination: PathBuf,
    pub temporary: PathBuf,
    pub contents: AtomicFileContents,
}

/// Filesystem operations supplied by the OS-specific backend.
pub trait AtomicFileBackend {
    fn create_new_owner_only(&mut self, path: &Path) -> Result<(), PlatformFailure>;
    fn write_all(&mut self, path: &Path, contents: &[u8]) -> Result<(), PlatformFailure>;
    fn sync_file(&mut self, path: &Path) -> Result<(), PlatformFailure>;
    fn replace_file(&mut self, source: &Path, destination: &Path) -> Result<(), PlatformFailure>;
    fn sync_parent(&mut self, destination: &Path) -> Result<(), PlatformFailure>;
    fn remove_file(&mut self, path: &Path) -> Result<(), PlatformFailure>;
}

/// Failure retaining both the primary transaction and cleanup outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicWriteFailure {
    pub primary: PlatformFailure,
    pub cleanup: Option<PlatformFailure>,
}

impl core::fmt::Display for AtomicWriteFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.primary)?;
        if let Some(cleanup) = &self.cleanup {
            write!(formatter, "; cleanup also failed: {cleanup}")?;
        }
        Ok(())
    }
}

/// Executes create-new → write → sync → replace → parent sync.
pub fn atomic_write(
    backend: &mut impl AtomicFileBackend,
    plan: AtomicWritePlan,
) -> Result<(), AtomicWriteFailure> {
    backend
        .create_new_owner_only(&plan.temporary)
        .map_err(|primary| AtomicWriteFailure {
            primary,
            cleanup: None,
        })?;
    if let Err(primary) = write_and_sync_temporary(backend, &plan) {
        return Err(clean_owned_temporary(backend, &plan, primary));
    }
    if let Err(primary) = backend.replace_file(&plan.temporary, &plan.destination) {
        return Err(clean_owned_temporary(backend, &plan, primary));
    }
    backend
        .sync_parent(&plan.destination)
        .map_err(|primary| AtomicWriteFailure {
            primary,
            cleanup: None,
        })
}

fn write_and_sync_temporary(
    backend: &mut impl AtomicFileBackend,
    plan: &AtomicWritePlan,
) -> Result<(), PlatformFailure> {
    backend.write_all(&plan.temporary, plan.contents.as_slice())?;
    backend.sync_file(&plan.temporary)
}

fn clean_owned_temporary(
    backend: &mut impl AtomicFileBackend,
    plan: &AtomicWritePlan,
    primary: PlatformFailure,
) -> AtomicWriteFailure {
    let cleanup = backend.remove_file(&plan.temporary).err();
    AtomicWriteFailure { primary, cleanup }
}
