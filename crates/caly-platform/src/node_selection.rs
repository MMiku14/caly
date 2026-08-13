//! Durable, owner-only memory of the user's node selection.
//!
//! A proxy core (re)start rebuilds every selector group to its rendered
//! default, which during the daemon's usual lifecycle is typically the dead
//! node the user selected away from. The selection record lets the daemon
//! re-apply the user's last choice once the core is ready, so `core restart`
//! and daemon reboots keep the working node instead of silently regressing.
//!
//! The record stores only the non-secret `group`/`name` pair understood by
//! the core's controller API, written owner-only (`0600`) via the atomic
//! replacement backend so a crash mid-write cannot corrupt the previous
//! state.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fs::{AtomicWritePlan, LinuxAtomicFileBackend, atomic_write};
use crate::{PlatformFailure, bounded_text as bounded};

/// Maximum bytes accepted for the selection record payload.
pub const MAX_SELECTION_BYTES: usize = 4 * 1_024;

/// The reference to re-apply: which node inside which group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeSelectionRecord {
    /// Operator-visible node reference (diagnostics only).
    pub node_id: String,
    /// Group name as rendered into the core config (e.g. `PROXY`).
    pub group: String,
    /// Proxy/outbound node name as rendered into the core config.
    pub name: String,
    /// Which core the record was written for (`mihomo` / `sing-box`).
    /// A record written under one core stores kernel-specific spelling
    /// (Mihomo display names vs sing-box `proxy-<hex>` tags), so a
    /// `core switch` must not re-apply it against the other kernel —
    /// it would 400 and drop the selection. `#[serde(default)]` keeps
    /// records written before this field existed readable.
    #[serde(default)]
    pub core: String,
}

/// Persists the node selection atomically, owner-only.
pub fn save_node_selection(
    path: &Path,
    record: &NodeSelectionRecord,
) -> Result<(), PlatformFailure> {
    let parent = if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| failure(path, format!("cannot create state dir: {error}")))?;
        parent.to_path_buf()
    } else {
        return Err(failure(
            path,
            "selection path has no parent directory".to_owned(),
        ));
    };
    let contents = serde_json::to_vec(record)
        .map_err(|error| failure(path, format!("cannot encode selection: {error}")))?;
    if contents.len() > MAX_SELECTION_BYTES {
        return Err(failure(
            path,
            "selection record exceeds the size bound".to_owned(),
        ));
    }
    let file_stem = path.file_name().map_or_else(
        || "selection".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = parent.join(format!("{file_stem}.tmp.{}", std::process::id()));
    let mut backend = LinuxAtomicFileBackend;
    let contents = crate::fs::AtomicFileContents::try_from_vec(contents).map_err(|_| {
        failure(
            path,
            "selection record exceeds the bounded file size".to_owned(),
        )
    })?;
    atomic_write(
        &mut backend,
        AtomicWritePlan {
            destination: path.to_path_buf(),
            temporary,
            contents,
        },
    )
    .map_err(|atomic| failure(path, atomic.primary.message.to_string()))
}

/// Loads the persisted node selection, if any.
///
/// Missing, corrupt or oversized records yield `None`: restoring a selection
/// is best-effort and must never fail the core lifecycle.
pub fn load_node_selection(path: &Path) -> Option<NodeSelectionRecord> {
    use std::io::Read;
    let mut contents = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_SELECTION_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .ok()?;
    if contents.len() > MAX_SELECTION_BYTES {
        return None;
    }
    serde_json::from_slice::<NodeSelectionRecord>(&contents).ok()
}

/// Removes the persisted node selection (e.g. after the re-apply failed).
pub fn clear_node_selection(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn failure(path: &Path, message: String) -> PlatformFailure {
    PlatformFailure {
        operation: bounded("save-node-selection".to_owned(), "selection-operation"),
        resource: bounded(path.display().to_string(), "selection-path"),
        message: bounded(message, "node selection persistence failed"),
        suggested_action: bounded(
            "inspect the XDG state directory permissions".to_owned(),
            "state permissions",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "caly-selection-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn selection_round_trip() {
        let dir = scratch();
        let path = dir.join("node-selection.json");
        let record = NodeSelectionRecord {
            node_id: "node-07".to_owned(),
            group: "PROXY".to_owned(),
            name: "Tokyo-02 东京".to_owned(),
            core: "mihomo".to_owned(),
        };
        assert!(save_node_selection(&path, &record).is_ok());
        assert_eq!(load_node_selection(&path), Some(record.clone()));
        // A newer selection atomically replaces the old one.
        let next = NodeSelectionRecord {
            node_id: "node-09".to_owned(),
            group: "PROXY".to_owned(),
            name: "Singapore-01".to_owned(),
            core: "sing-box".to_owned(),
        };
        assert!(save_node_selection(&path, &next).is_ok());
        assert_eq!(load_node_selection(&path), Some(next));
        clear_node_selection(&path);
        assert_eq!(load_node_selection(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_record_without_core_field_still_loads() {
        // Records written before the `core` slot existed must stay
        // readable (serde default); a legacy record is treated as
        // belonging to no core in particular.
        let dir = scratch();
        let path = dir.join("node-selection.json");
        std::fs::write(
            &path,
            r#"{"node_id":"node-07","group":"PROXY","name":"Tokyo-02"}"#,
        )
        .ok();
        let loaded = load_node_selection(&path).expect("legacy record must load");
        assert_eq!(loaded.core, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_corrupt_selection_is_none() {
        let dir = scratch();
        let path = dir.join("missing.json");
        assert_eq!(load_node_selection(&path), None);
        std::fs::write(&path, b"not json at all {{{").ok();
        assert_eq!(load_node_selection(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
