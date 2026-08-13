//! Generation-directory cache transaction with manifest-last publication.

mod quota;

pub use quota::{EvictionPlan, GenerationUsage, QuotaError, plan_evictions};

use std::path::{Path, PathBuf};

use caly_domain::BoundedVec;
use caly_platform::PlatformFailure;

/// Cache payload ceilings.
pub const MAX_CACHE_RAW_BYTES: usize = 32 * 1_024 * 1_024;
pub const MAX_CACHE_META_BYTES: usize = 1_024 * 1_024;
pub type CacheRaw = BoundedVec<u8, MAX_CACHE_RAW_BYTES>;
pub type CacheMetadata = BoundedVec<u8, MAX_CACHE_META_BYTES>;

/// Immutable cache generation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheGenerationId(pub [u8; 16]);

/// Complete unpublished generation.
pub struct CacheGeneration {
    pub id: CacheGenerationId,
    pub raw: CacheRaw,
    pub metadata: CacheMetadata,
}

/// Storage backend enforcing owner-only files and durability.
pub trait CacheBackend {
    fn create_generation(&mut self, id: CacheGenerationId) -> Result<PathBuf, PlatformFailure>;
    fn write_owner_only(&mut self, path: &Path, data: &[u8]) -> Result<(), PlatformFailure>;
    fn sync_file(&mut self, path: &Path) -> Result<(), PlatformFailure>;
    fn sync_directory(&mut self, path: &Path) -> Result<(), PlatformFailure>;
    fn replace_current_manifest(&mut self, id: CacheGenerationId) -> Result<(), PlatformFailure>;
    fn sync_cache_root(&mut self) -> Result<(), PlatformFailure>;
    fn remove_generation(&mut self, id: CacheGenerationId) -> Result<(), PlatformFailure>;
}

/// Publish failure retaining cleanup failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheCommitFailure {
    pub primary: PlatformFailure,
    pub cleanup: Option<PlatformFailure>,
    pub current_may_reference_generation: bool,
}

/// Writes immutable generation files and publishes CURRENT last.
pub fn commit_generation(
    backend: &mut impl CacheBackend,
    generation: CacheGeneration,
) -> Result<(), CacheCommitFailure> {
    let id = generation.id;
    let directory = backend
        .create_generation(id)
        .map_err(|primary| CacheCommitFailure {
            primary,
            cleanup: None,
            current_may_reference_generation: false,
        })?;
    if let Err(primary) = write_generation(backend, &directory, generation) {
        let cleanup = backend.remove_generation(id).err();
        return Err(CacheCommitFailure {
            primary,
            cleanup,
            current_may_reference_generation: false,
        });
    }
    if let Err(primary) = backend.replace_current_manifest(id) {
        return Err(CacheCommitFailure {
            primary,
            cleanup: None,
            current_may_reference_generation: true,
        });
    }
    backend
        .sync_cache_root()
        .map_err(|primary| CacheCommitFailure {
            primary,
            cleanup: None,
            current_may_reference_generation: true,
        })
}

fn write_generation(
    backend: &mut impl CacheBackend,
    directory: &Path,
    generation: CacheGeneration,
) -> Result<(), PlatformFailure> {
    let raw_path = directory.join("raw");
    let metadata_path = directory.join("meta");
    backend.write_owner_only(&raw_path, generation.raw.as_slice())?;
    backend.sync_file(&raw_path)?;
    backend.write_owner_only(&metadata_path, generation.metadata.as_slice())?;
    backend.sync_file(&metadata_path)?;
    backend.sync_directory(directory)
}
