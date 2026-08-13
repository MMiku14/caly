//! Durable, owner-only, atomic recovery-record storage for platform effects.
//!
//! The recovery record stores only the non-secret side-effect parameters that
//! the Linux backends need to re-apply after a crash (proxy host/port/mode, or
//! TUN interface/MTU). It deliberately does not persist credentials, so the
//! record can be serialized as JSON and written owner-only (`0600`).
//!
//! The store is generic over the record type so the proxy and TUN side effects
//! share one atomic, owner-only, bounded file-store implementation instead of
//! duplicating it.

use std::{marker::PhantomData, path::PathBuf};

use serde::{Serialize, de::DeserializeOwned};

use super::RecoveryPhase;

/// Serializable proxy recovery record (no credentials stored).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
pub struct ProxyRecoveryRecord {
    pub owner_token: [u8; 16],
    pub phase: RecoveryPhase,
    /// Proxy host to re-apply after crash recovery.
    pub host: String,
    /// Proxy port to re-apply after crash recovery.
    pub port: u16,
    /// Whether the system proxy was engaged when the record was written.
    pub enabled: bool,
    /// Original desktop proxy mode captured before engagement ("none" etc.).
    pub original_mode: String,
    /// Original `host:port` endpoint captured before engagement (empty when the
    /// original state carried no endpoint). Absent in legacy records.
    #[serde(default)]
    pub original_endpoint: String,
    /// PAC URL when the record was written by `sysproxy pac` (empty for
    /// the manual-endpoint mode). Crash recovery replays PAC mode via
    /// this URL instead of the manual endpoint (2026-08-12).
    #[serde(default)]
    pub pac_url: String,
}

/// Serializable TUN recovery record (no credentials stored).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
pub struct TunRecoveryRecord {
    pub owner_token: [u8; 16],
    pub phase: RecoveryPhase,
    /// TUN interface name to re-engage after crash recovery.
    pub interface: String,
    /// MTU to apply when re-engaging the TUN interface.
    pub mtu: u16,
    /// Whether the TUN side effect was engaged when the record was written.
    pub enabled: bool,
}

/// Store failure retaining the failed durability boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryStoreError {
    Read(String),
    Write(String),
    Parse(String),
    Remove(String),
    Permission(String),
}

/// Owner-only durable recovery storage for a concrete record type.
pub trait RecoveryStore<R> {
    fn load(&self) -> Result<Option<R>, RecoveryStoreError>;
    fn persist(&self, record: &R) -> Result<(), RecoveryStoreError>;
    fn clear_if_owner(&self, owner_token: [u8; 16]) -> Result<(), RecoveryStoreError>;
}

/// Owner-only durable proxy recovery storage (concrete record type).
pub trait ProxyRecoveryStore: RecoveryStore<ProxyRecoveryRecord> {}

/// Owner-only durable TUN recovery storage (concrete record type).
pub trait TunRecoveryStore: RecoveryStore<TunRecoveryRecord> {}

/// File-backed, owner-only, atomic JSON recovery store generic over the record.
#[derive(Clone, Debug)]
pub struct FileRecoveryStore<R> {
    path: PathBuf,
    _record: PhantomData<R>,
}

impl<R> FileRecoveryStore<R> {
    /// Creates a store rooted at `path`; the parent is created owner-only.
    pub fn new(path: PathBuf) -> Result<Self, RecoveryStoreError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            if parent.exists() {
                // Audit #104: a PRE-EXISTING parent must be owner-only too —
                // the pre-fix code only chmod'ed the directory it created
                // itself, so a world-readable leftovers directory kept
                // serving group/world access to the recovery records. Verify
                // first and tighten only when loose: an unconditional chmod
                // fails on shared roots (e.g. the test-suite temp dir),
                // and those are refused instead of silently accepted.
                ensure_owner_only(parent)?;
            } else {
                std::fs::create_dir_all(parent).map_err(|error| {
                    RecoveryStoreError::Permission(format!(
                        "cannot create recovery dir {}: {error}",
                        parent.display()
                    ))
                })?;
                set_owner_only(parent)?;
            }
        }
        Ok(Self {
            path,
            _record: PhantomData,
        })
    }
}

impl<R> RecoveryStore<R> for FileRecoveryStore<R>
where
    R: Serialize + DeserializeOwned + RecordOwnerToken,
{
    fn load(&self) -> Result<Option<R>, RecoveryStoreError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&self.path).map_err(|error| {
            RecoveryStoreError::Read(format!("cannot read {}: {error}", self.path.display()))
        })?;
        if bytes.is_empty() {
            return Ok(None);
        }
        match serde_json::from_slice(&bytes) {
            Ok(record) => Ok(Some(record)),
            Err(error) => {
                // A corrupt record must never brick the daemon (the pre-fix
                // `Parse` error propagated into boot's `BackendUnavailable`
                // and the whole control plane refused to start). Quarantine
                // the evidence and treat this boot as nothing-pending; the
                // record is regenerated on the next engage.
                let quarantine = self.path.with_extension("json.corrupt");
                if let Err(rename_error) = std::fs::rename(&self.path, &quarantine) {
                    tracing::warn!(
                        error = %rename_error,
                        "could not quarantine the corrupt recovery record"
                    );
                }
                tracing::warn!(
                    error = %error,
                    quarantine = %quarantine.display(),
                    "invalid recovery record quarantined; continuing without it"
                );
                Ok(None)
            }
        }
    }

    fn persist(&self, record: &R) -> Result<(), RecoveryStoreError> {
        let json = serde_json::to_vec(record)
            .map_err(|error| RecoveryStoreError::Write(format!("serialize failed: {error}")))?;
        atomic_owner_write(&self.path, &json)
    }

    fn clear_if_owner(&self, owner_token: [u8; 16]) -> Result<(), RecoveryStoreError> {
        match self.load()? {
            Some(record) if record.owner_token() == owner_token => std::fs::remove_file(&self.path)
                .map_err(|error| {
                    RecoveryStoreError::Remove(format!(
                        "cannot remove {}: {error}",
                        self.path.display()
                    ))
                }),
            Some(_) => Err(RecoveryStoreError::Permission(
                "recovery record owned by a different token".to_owned(),
            )),
            None => Ok(()),
        }
    }
}

impl ProxyRecoveryStore for FileRecoveryStore<ProxyRecoveryRecord> {}
impl TunRecoveryStore for FileRecoveryStore<TunRecoveryRecord> {}

/// Owner-token accessor so `clear_if_owner` can compare regardless of record
/// shape without exposing the token field publicly in the generic helper.
trait RecordOwnerToken {
    fn owner_token(&self) -> [u8; 16];
}

impl RecordOwnerToken for ProxyRecoveryRecord {
    fn owner_token(&self) -> [u8; 16] {
        self.owner_token
    }
}
impl RecordOwnerToken for TunRecoveryRecord {
    fn owner_token(&self) -> [u8; 16] {
        self.owner_token
    }
}

/// Writes `bytes` atomically and owner-only (`0600`), replacing any existing file.
fn atomic_owner_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), RecoveryStoreError> {
    use crate::fs::{AtomicFileContents, AtomicWritePlan, LinuxAtomicFileBackend, atomic_write};
    let contents = AtomicFileContents::try_from_vec(bytes.to_vec())
        .map_err(|_| RecoveryStoreError::Write("record exceeds size bound".to_owned()))?;
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    atomic_write(
        &mut LinuxAtomicFileBackend,
        AtomicWritePlan {
            destination: path.to_path_buf(),
            temporary: PathBuf::from(temporary),
            contents,
        },
    )
    .map_err(|error| RecoveryStoreError::Write(format!("atomic write failed: {error:?}")))
}

fn set_owner_only(path: &std::path::Path) -> Result<(), RecoveryStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
            RecoveryStoreError::Permission(format!(
                "cannot set owner-only mode on {}: {error}",
                path.display()
            ))
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Verifies that a PRE-EXISTING recovery parent is owner-only, tightening a
/// loose mode first. A directory the process cannot tighten (a shared root
/// such as the test suite's temp dir) is REFUSED — writing records into a
/// world-readable directory is never the right fallback (audit #104).
#[cfg(unix)]
fn ensure_owner_only(path: &std::path::Path) -> Result<(), RecoveryStoreError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).map_err(|error| {
        RecoveryStoreError::Permission(format!(
            "cannot inspect recovery dir {}: {error}",
            path.display()
        ))
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode == 0o700 {
        return Ok(());
    }
    set_owner_only(path).map_err(|error: RecoveryStoreError| {
        RecoveryStoreError::Permission(format!(
            "recovery dir {} is not owner-only (mode {mode:o}) and cannot be tightened: {error:?}",
            path.display(),
        ))
    })
}

#[cfg(not(unix))]
fn ensure_owner_only(path: &std::path::Path) -> Result<(), RecoveryStoreError> {
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_helpers::unique_path_under;

    /// The recovery store refuses a parent directory it cannot tighten to
    /// 0700 (audit #104), so tests root every record in its own fresh
    /// subdirectory instead of the shared temp dir.
    fn test_store_path(label: &str) -> std::path::PathBuf {
        unique_path_under("caly-recovery", label).join("record.json")
    }

    fn proxy_record(seed: u8, phase: RecoveryPhase) -> ProxyRecoveryRecord {
        ProxyRecoveryRecord {
            owner_token: [seed; 16],
            phase,
            host: "127.0.0.1".to_owned(),
            port: 7890,
            enabled: true,
            original_mode: "none".to_owned(),
            original_endpoint: String::new(),
            pac_url: String::new(),
        }
    }

    #[test]
    fn corrupt_record_is_quarantined_and_treated_as_absent() -> Result<(), String> {
        let path = test_store_path("corrupt");
        let store = FileRecoveryStore::<ProxyRecoveryRecord>::new(path.clone())
            .map_err(|e| format!("{e:?}"))?;
        std::fs::write(&path, b"{ this is not json").map_err(|e| format!("{e:?}"))?;
        // A corrupt record must not brick the daemon: load quarantines the
        // evidence and reports nothing-pending.
        assert_eq!(store.load().map_err(|e| format!("{e:?}"))?, None);
        assert!(
            path.with_extension("json.corrupt").is_file(),
            "corrupt record must be quarantined, not deleted"
        );
        Ok(())
    }

    #[test]
    fn legacy_proxy_record_without_endpoint_deserializes() -> Result<(), String> {
        let json = r#"{
            "owner_token": [1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],
            "phase": "Applied",
            "host": "127.0.0.1",
            "port": 7890,
            "enabled": true,
            "original_mode": "manual"
        }"#;
        let record: ProxyRecoveryRecord =
            serde_json::from_str(json).map_err(|error| error.to_string())?;
        assert_eq!(record.original_endpoint, "");
        assert_eq!(record.original_mode, "manual");
        Ok(())
    }

    fn tun_record(seed: u8, phase: RecoveryPhase) -> TunRecoveryRecord {
        TunRecoveryRecord {
            owner_token: [seed; 16],
            phase,
            interface: "caly0".to_owned(),
            mtu: 1_500,
            enabled: true,
        }
    }

    #[test]
    fn proxy_load_missing_returns_none() -> Result<(), RecoveryStoreError> {
        let store = FileRecoveryStore::<ProxyRecoveryRecord>::new(test_store_path("pmissing"))?;
        assert_eq!(store.load()?, None);
        let _ = std::fs::remove_file(&store.path);
        Ok(())
    }

    /// Parametric round-trip: every concrete record type must persist then
    /// load back the same value. Previously this lived as two
    /// byte-identical tests (`proxy_persist_then_load_round_trips` and
    /// `tun_persist_then_load_round_trips`); the trait bound on
    /// `RecoveryStore<R>` is the only thing each test exercised, so the
    /// test was consolidated into one parametric call.
    #[test]
    fn persist_then_load_round_trips() -> Result<(), RecoveryStoreError> {
        let proxy_store =
            FileRecoveryStore::<ProxyRecoveryRecord>::new(test_store_path("proxy-roundtrip"))?;
        let proxy_expected = proxy_record(1, RecoveryPhase::Applied);
        proxy_store.persist(&proxy_expected)?;
        assert_eq!(proxy_store.load()?, Some(proxy_expected));
        let _ = std::fs::remove_file(&proxy_store.path);

        let tun_store =
            FileRecoveryStore::<TunRecoveryRecord>::new(test_store_path("tun-roundtrip"))?;
        let tun_expected = tun_record(1, RecoveryPhase::Applied);
        tun_store.persist(&tun_expected)?;
        assert_eq!(tun_store.load()?, Some(tun_expected));
        let _ = std::fs::remove_file(&tun_store.path);
        Ok(())
    }

    #[test]
    fn clear_if_owner_removes_only_matching_token() -> Result<(), RecoveryStoreError> {
        let store = FileRecoveryStore::<ProxyRecoveryRecord>::new(test_store_path("pclear"))?;
        store.persist(&proxy_record(5, RecoveryPhase::Applied))?;
        assert!(matches!(
            store.clear_if_owner([9; 16]),
            Err(RecoveryStoreError::Permission(_))
        ));
        assert!(store.load()?.is_some());
        store.clear_if_owner([5; 16])?;
        assert_eq!(store.load()?, None);
        let _ = std::fs::remove_file(&store.path);
        Ok(())
    }

    #[test]
    fn persisted_proxy_record_is_owner_only() -> Result<(), RecoveryStoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let store = FileRecoveryStore::<ProxyRecoveryRecord>::new(test_store_path("pperm"))?;
            store.persist(&proxy_record(2, RecoveryPhase::Prepared))?;
            let mode = std::fs::metadata(&store.path)
                .map_err(|error| RecoveryStoreError::Read(error.to_string()))?
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let _ = std::fs::remove_file(&store.path);
        }
        Ok(())
    }
}
