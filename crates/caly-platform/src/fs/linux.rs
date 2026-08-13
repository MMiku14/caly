//! Linux atomic owner-only file backend.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use super::AtomicFileBackend;
use crate::PlatformFailure;
use crate::bounded_text as bounded;

/// Linux filesystem backend for generation/config publication.
#[derive(Default)]
pub struct LinuxAtomicFileBackend;

impl AtomicFileBackend for LinuxAtomicFileBackend {
    fn create_new_owner_only(&mut self, path: &Path) -> Result<(), PlatformFailure> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .map(|_| ())
            .map_err(|error| failure("create-temp", path, error.to_string()))
    }

    fn write_all(&mut self, path: &Path, contents: &[u8]) -> Result<(), PlatformFailure> {
        let mut file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| failure("open-temp", path, error.to_string()))?;
        file.write_all(contents)
            .map_err(|error| failure("write-temp", path, error.to_string()))
    }

    fn sync_file(&mut self, path: &Path) -> Result<(), PlatformFailure> {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| failure("sync-file", path, error.to_string()))
    }

    fn replace_file(&mut self, source: &Path, destination: &Path) -> Result<(), PlatformFailure> {
        fs::rename(source, destination)
            .map_err(|error| failure("replace-file", destination, error.to_string()))
    }

    fn sync_parent(&mut self, destination: &Path) -> Result<(), PlatformFailure> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|error| failure("sync-parent", parent, error.to_string()))
    }

    fn remove_file(&mut self, path: &Path) -> Result<(), PlatformFailure> {
        fs::remove_file(path).map_err(|error| failure("remove-temp", path, error.to_string()))
    }
}

fn failure(operation: &'static str, path: &Path, message: String) -> PlatformFailure {
    PlatformFailure {
        operation: bounded(operation, "filesystem-operation"),
        resource: bounded(path.display().to_string(), "filesystem-path"),
        message: bounded(message, "Linux filesystem operation failed"),
        suggested_action: bounded(
            "inspect ownership, permissions, and disk space",
            "inspect filesystem",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::BoundedVec;

    #[test]
    fn atomic_backend_replaces_owner_file() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("caly-fs-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let destination = root.join("config.yaml");
        let temporary = root.join("config.yaml.tmp");
        let contents = BoundedVec::try_from_vec(b"mode: rule\n".to_vec())?;
        let mut backend = LinuxAtomicFileBackend;
        super::super::atomic_write(
            &mut backend,
            super::super::AtomicWritePlan {
                destination: destination.clone(),
                temporary,
                contents,
            },
        )
        .map_err(|error| format!("{error:?}"))?;
        assert_eq!(fs::read(&destination)?, b"mode: rule\n");
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
