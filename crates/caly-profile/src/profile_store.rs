//! On-disk cache for `Profile` bodies.
//!
//! Remote profiles are fetched from a public URL on a refresh
//! cadence; the fetched body is materialised at
//! `<state>/profiles/<id>.yaml` together with a `<id>.meta.toml`
//! sidecar that records the source URL, fetch timestamp, and
//! SHA-256. The loader reads from the cache; the `refresh` path
//! rewrites the cache atomically.
//!
//! Local profiles point at a file the operator owns; the
//! `ProfileStore` does **not** materialise them — the loader reads
//! the file directly through the [`ProfileBodyResolver`] trait.
//! This keeps the cache layout minimal: the `state/profiles/`
//! directory only contains Remote-profile bodies and metadata.
//!
//! All public functions in this module are fallible; failure
//! modes are explicit and bounded (`BoundedText` for user-facing
//! reasons, `io::Error` for filesystem errors). The store never
//! panics on user input.

use std::{fs, io, path::PathBuf};

use caly_domain::ProfileId;
use serde::{Deserialize, Serialize};

use crate::loader::ProfileBodyResolver;

/// One cached Remote profile. The metadata sidecar carries the
/// `Profile` body filename, source URL, last refresh, content
/// length, and SHA-256. The `Local` variant is not stored here —
/// local files are read through the operator's own path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProfileCacheEntry {
    /// Public URL the body was fetched from.
    pub url: String,
    /// Unix epoch milliseconds when the cache was last refreshed.
    pub fetched_at_ms: u64,
    /// UTF-8 byte length of the cached body. Mirrors the on-disk
    /// file size so the loader can refuse oversize bodies without
    /// re-reading the file.
    pub body_bytes: usize,
    /// Lowercase hex SHA-256 of the cached body. Optional for
    /// back-compat with caches written by older builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_hex: Option<String>,
    /// HTTP `ETag` returned by the last fetch (fed back as
    /// `If-None-Match` on the next refresh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// HTTP `Last-Modified` returned by the last fetch (fed back
    /// as `If-Modified-Since` on the next refresh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// Failure mode for store operations.
#[derive(Debug)]
pub enum ProfileStoreError {
    /// The path component was not path-safe ASCII, or the cache
    /// directory is missing required safety invariants.
    InvalidId,
    /// The cache directory could not be created or read.
    Io(io::Error),
    /// The body exceeds [`caly_domain::PROFILE_BODY_MAX_BYTES`].
    BodyTooLarge { limit: usize, actual: usize },
    /// The on-disk metadata could not be parsed.
    MetadataMalformed,
    /// The on-disk metadata carries a different `body_bytes`
    /// than the file actually has (corruption / partial write).
    MetadataMismatch,
}

impl core::fmt::Display for ProfileStoreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("profile id is not path-safe"),
            Self::Io(error) => write!(formatter, "profile store I/O error: {error}"),
            Self::BodyTooLarge { limit, actual } => {
                write!(
                    formatter,
                    "profile body is {actual} bytes; exceeds the {limit}-byte limit"
                )
            }
            Self::MetadataMalformed => formatter.write_str("profile cache metadata is malformed"),
            Self::MetadataMismatch => {
                formatter.write_str("profile cache metadata does not match on-disk file")
            }
        }
    }
}

impl std::error::Error for ProfileStoreError {}

/// Resolver backed by the on-disk cache. `Local` profiles are read
/// from `<config>/profiles/<path>` (relative to the operator's
/// config root). `Remote` profiles are served from
/// `<state>/profiles/<id>.yaml` after a successful `refresh`.
///
/// The store does not own the fetch path; production wiring in
/// `caly-application` provides the actual `Remote` body and
/// calls `write` to materialise it. This keeps the cache layer
/// hermetic and unit-testable.
pub struct ProfileStore {
    config_root: PathBuf,
    state_root: PathBuf,
}

impl ProfileStore {
    /// Builds a store rooted at `config_root` (where `Local`
    /// profile paths are resolved) and `state_root` (where the
    /// Remote cache lives). Both directories are created on
    /// first write.
    pub fn new(config_root: PathBuf, state_root: PathBuf) -> Self {
        Self {
            config_root,
            state_root,
        }
    }

    /// Returns the absolute path of the cache file for `id`.
    /// Callers (and tests) use this to read the body bytes
    /// directly.
    pub fn cache_path(&self, id: &str) -> Result<PathBuf, ProfileStoreError> {
        if !is_path_safe(id) {
            return Err(ProfileStoreError::InvalidId);
        }
        Ok(self.state_root.join("profiles").join(format!("{id}.yaml")))
    }

    /// Returns the absolute path of the metadata sidecar.
    pub fn metadata_path(&self, id: &str) -> Result<PathBuf, ProfileStoreError> {
        if !is_path_safe(id) {
            return Err(ProfileStoreError::InvalidId);
        }
        Ok(self
            .state_root
            .join("profiles")
            .join(format!("{id}.meta.toml")))
    }

    /// Reads the cached body. Returns `Ok(None)` when the cache
    /// file is missing (the operator has not run `refresh`
    /// yet); a corrupt metadata sidecar returns
    /// `Err(MetadataMismatch)`.
    pub fn read(&self, id: &str) -> Result<Option<Vec<u8>>, ProfileStoreError> {
        let body_path = self.cache_path(id)?;
        let bytes = match fs::read(&body_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ProfileStoreError::Io(error)),
        };
        if bytes.len() > caly_domain::PROFILE_BODY_MAX_BYTES {
            return Err(ProfileStoreError::BodyTooLarge {
                limit: caly_domain::PROFILE_BODY_MAX_BYTES,
                actual: bytes.len(),
            });
        }
        if let Some(meta) = self.read_metadata(id)?
            && meta.body_bytes != bytes.len()
        {
            return Err(ProfileStoreError::MetadataMismatch);
        }
        Ok(Some(bytes))
    }

    /// Atomically writes a new body. The metadata sidecar is
    /// rewritten with the new timestamp and size. The body
    /// file is written under a temporary name in the same
    /// directory and renamed so a concurrent reader can never
    /// see a partial body.
    pub fn write(
        &self,
        id: &str,
        body: &[u8],
        url: &str,
        now_ms: u64,
    ) -> Result<(), ProfileStoreError> {
        self.write_full(id, body, url, now_ms, None, None)
    }

    /// Same as [`Self::write`] but records the HTTP validators from the
    /// fetch response so the next refresh can issue a conditional request.
    /// Passing `None` validators *preserves* the ones already on disk (a
    /// `304 NotModified` rewrite updates `fetched_at_ms` only).
    #[allow(clippy::too_many_arguments)]
    pub fn write_full(
        &self,
        id: &str,
        body: &[u8],
        url: &str,
        now_ms: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Result<(), ProfileStoreError> {
        if !is_path_safe(id) {
            return Err(ProfileStoreError::InvalidId);
        }
        if body.len() > caly_domain::PROFILE_BODY_MAX_BYTES {
            return Err(ProfileStoreError::BodyTooLarge {
                limit: caly_domain::PROFILE_BODY_MAX_BYTES,
                actual: body.len(),
            });
        }
        let dir = self.state_root.join("profiles");
        fs::create_dir_all(&dir).map_err(ProfileStoreError::Io)?;
        let body_path = self.cache_path(id)?;
        let meta_path = self.metadata_path(id)?;
        // Atomic write: temp file + rename.
        let tmp = body_path.with_extension("yaml.tmp");
        fs::write(&tmp, body).map_err(ProfileStoreError::Io)?;
        fs::rename(&tmp, &body_path).map_err(ProfileStoreError::Io)?;
        // No new validators → keep the previously stored ones (a 304
        // metadata touch-up must not erase the very ETag that produced it).
        let (etag, last_modified) = match (etag, last_modified) {
            (None, None) => match self.read_metadata(id)? {
                Some(previous) => (previous.etag, previous.last_modified),
                None => (None, None),
            },
            (etag, last_modified) => (etag, last_modified),
        };
        let entry = ProfileCacheEntry {
            url: url.to_owned(),
            fetched_at_ms: now_ms,
            body_bytes: body.len(),
            sha256_hex: None,
            etag,
            last_modified,
        };
        let serialized = serde_norway::to_string(&entry).unwrap_or_default();
        let meta_tmp = meta_path.with_extension("meta.toml.tmp");
        fs::write(&meta_tmp, serialized).map_err(ProfileStoreError::Io)?;
        fs::rename(&meta_tmp, &meta_path).map_err(ProfileStoreError::Io)?;
        Ok(())
    }

    /// Reads the metadata sidecar, if present. Missing sidecar
    /// returns `Ok(None)`.
    pub fn read_metadata(&self, id: &str) -> Result<Option<ProfileCacheEntry>, ProfileStoreError> {
        let meta_path = self.metadata_path(id)?;
        let bytes = match fs::read(&meta_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ProfileStoreError::Io(error)),
        };
        let entry: ProfileCacheEntry =
            serde_norway::from_slice(&bytes).map_err(|_| ProfileStoreError::MetadataMalformed)?;
        Ok(Some(entry))
    }

    /// Lists the cached profile ids. The returned set only
    /// includes ids whose body file is present; a leftover
    /// metadata sidecar without a body is treated as missing
    /// and skipped.
    pub fn list_cached(&self) -> Result<Vec<ProfileId>, ProfileStoreError> {
        let dir = self.state_root.join("profiles");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(ProfileStoreError::Io)? {
            let entry = entry.map_err(ProfileStoreError::Io)?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("yaml") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Ok(id) = ProfileId::new(stem.to_owned())
            {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Removes a cached profile (body + metadata). Missing
    /// files are not an error.
    pub fn remove(&self, id: &str) -> Result<(), ProfileStoreError> {
        if !is_path_safe(id) {
            return Err(ProfileStoreError::InvalidId);
        }
        let body_path = self.cache_path(id)?;
        match fs::remove_file(&body_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ProfileStoreError::Io(error)),
        }
        let meta_path = self.metadata_path(id)?;
        match fs::remove_file(&meta_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ProfileStoreError::Io(error)),
        }
        Ok(())
    }
}

fn is_path_safe(id: &str) -> bool {
    caly_domain::is_path_safe_component(id)
}

impl ProfileBodyResolver for ProfileStore {
    fn resolve(&self, id: &str) -> Result<Vec<u8>, String> {
        // Try the cache first (Remote profiles); on miss, fall back
        // to a Local file at `<config>/profiles/<id>.yaml` so an
        // operator can also drop a hand-edited body in place.
        match self.read(id) {
            Ok(Some(body)) => Ok(body),
            Ok(None) => {
                let local = self.config_root.join("profiles").join(format!("{id}.yaml"));
                match fs::read(&local) {
                    Ok(bytes) => Ok(bytes),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Err(format!(
                        "profile `{id}` body is not yet cached and no local file is present"
                    )),
                    Err(error) => Err(format!("profile `{id}` local file read failed: {error}")),
                }
            }
            Err(error) => Err(format!("profile `{id}` cache read failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests;
