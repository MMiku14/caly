//! Node-selection persistence shared by the command and lifecycle backends.
//!
//! `remember` runs on every successful `select_proxy`; `restore` runs after a
//! core start/restart and re-applies the saved choice. Restoring is
//! best-effort: a missing/corrupt record is a no-op, and a failed re-apply
//! (e.g. the node vanished after a subscription refresh) clears the record
//! instead of retrying forever.

use std::path::Path;
use std::time::Duration;

use caly_domain::NodeId;
use caly_platform::node_selection::{
    NodeSelectionRecord, clear_node_selection, load_node_selection, save_node_selection,
};
use caly_platform::paths::AppPaths;
use tracing::{debug, info, warn};

/// Persists the user's last successful `select_proxy`.
///
/// The stored `group`/`name` pair is exactly what the core controller select
/// API consumes, so a later restore needs no registry lookup. `core` names
/// the kernel the record was written for: Mihomo stores display names while
/// sing-box stores `proxy-<hex>` tags, so a record must only be re-applied
/// against the same core (`core switch` skips the other kernel's record
/// instead of failing the re-apply and dropping the selection).
pub fn remember(node_id: NodeId, group: &str, name: &str, core: caly_domain::CoreKind) {
    let path = AppPaths::from_env().node_selection_path();
    let record = NodeSelectionRecord {
        node_id: node_id.to_string(),
        group: group.to_owned(),
        name: name.to_owned(),
        core: core_kind_label(core),
    };
    if let Err(error) = save_node_selection(&path, &record) {
        warn!(
            message = %error,
            operation = ?error.operation,
            "could not persist node selection"
        );
    }
}

/// Stable label for the selection slot (mirrors the daemon's core label).
fn core_kind_label(core: caly_domain::CoreKind) -> String {
    match core {
        caly_domain::CoreKind::Mihomo => "mihomo".to_owned(),
        caly_domain::CoreKind::SingBox => "sing-box".to_owned(),
        caly_domain::CoreKind::Xray => "xray".to_owned(),
    }
}

/// Drops the persisted selection: a group-member pick that is not a
/// registered node (a builtin like DIRECT/REJECT, or a nested group)
/// must not leave the previous node selection behind — restoring it
/// after a restart would silently undo the operator's last choice
/// (2026-08-12 agent audit).
pub fn clear() {
    let path = AppPaths::from_env().node_selection_path();
    clear_node_selection(&path);
}

/// Re-applies the persisted selection after a core (re)start.
///
/// The `select` closure drives the actual controller call; the timeout is
/// budgeted by the caller so a slow controller cannot stall the lifecycle
/// window beyond its own budget. A record written for a *different* core
/// (legacy records included, which carry an empty slot) is left untouched:
/// its spelling belongs to the other kernel and re-applying it would 400.
pub fn restore(
    core: caly_domain::CoreKind,
    timeout: Duration,
    select: impl FnOnce(&str, &str) -> Result<(), String>,
) {
    let path = AppPaths::from_env().node_selection_path();
    restore_at(&path, core, timeout, select);
}

/// `restore` with an explicit record path (test seam; production callers use
/// `restore`).
fn restore_at(
    path: &Path,
    core: caly_domain::CoreKind,
    timeout: Duration,
    select: impl FnOnce(&str, &str) -> Result<(), String>,
) {
    let Some(record) = load_node_selection(path) else {
        return;
    };
    let label = core_kind_label(core);
    // A legacy record (written before the `core` slot existed) stores
    // Mihomo spelling — every pre-slot writer was the Mihomo path — so
    // only the Mihomo slot may consume it; replaying it against
    // sing-box would 400 and drop the selection (2026-08-12 audit).
    let foreign = !record.core.is_empty() && record.core != label
        || (record.core.is_empty() && label != "mihomo");
    if foreign {
        // The record belongs to the other kernel's spelling; keep it for
        // when that core comes back.
        info!(
            record_core = %record.core,
            active_core = %label,
            "node selection belongs to another core; keeping it for that core"
        );
        return;
    }
    match select(&record.group, &record.name) {
        Ok(()) => {
            info!(group = %record.group, node = %record.name, "restored node selection after core start");
        }
        Err(error) => {
            // 刀 3 (2026-08-12 pipeline design): a failed re-apply is most
            // often a subscription update that renamed the node — dropping
            // the record then would silently discard the operator's choice.
            // Keep the record; the selection reconciler follows the stable
            // node id after the next refresh and updates the name, so the
            // choice survives. Only a node that disappeared from every
            // refresh clears it.
            warn!(
                group = %record.group,
                node = %record.name,
                node_id = %record.node_id,
                error,
                "node selection re-apply failed; keeping the record (a subscription refresh reconciles it by id; pick again with `caly node pick` to choose a different node)"
            );
        }
    }
    let _ = timeout;
}

/// Reconciles the persisted selection against the current node registry
/// after a subscription refresh (刀 3, 2026-08-12 pipeline design): the
/// selection intent is the stable `node_id`, the display name follows it.
/// When the node still exists under a new name the record is updated, so
/// the next core restart restores the choice; when the node is gone the
/// record is cleared (there is nothing left to restore).
/// Reconciles the persisted selection against the current node registry
/// after a subscription refresh (刀 3, 2026-08-12 pipeline design): the
/// selection intent is the stable `node_id`, the display name follows it.
/// When the node still exists under a new name the record is updated, so
/// the next core restart restores the choice; when the node is gone the
/// record is cleared (there is nothing left to restore). Foreign-core
/// records are skipped untouched, mirroring [`restore_at`] — a sing-box
/// record stores `proxy-<hex>` tags, and rewriting it with the mihomo
/// display name would break the next sing-box restore permanently
/// (2026-08-12 read-only agent audit).
pub fn reconcile(
    active: caly_domain::CoreKind,
    resolve: impl FnOnce(&NodeId) -> Result<Option<String>, ()>,
) {
    let path = AppPaths::from_env().node_selection_path();
    reconcile_at(&path, active, resolve);
}

/// `reconcile` with an explicit record path (test seam).
fn reconcile_at(
    path: &Path,
    active: caly_domain::CoreKind,
    resolve: impl FnOnce(&NodeId) -> Result<Option<String>, ()>,
) {
    let Some(record) = load_node_selection(path) else {
        return;
    };
    // Mirror the restore gate: only reconcile the record that belongs to
    // the currently active core (legacy empty-core records are mihomo).
    let label = core_kind_label(active);
    let foreign = !record.core.is_empty() && record.core != label
        || (record.core.is_empty() && label != "mihomo");
    if foreign {
        debug!(
            record_core = %record.core,
            active_core = %label,
            "selection reconcile skipped for a foreign-core record"
        );
        return;
    }
    let Ok(node_id) = record.node_id.parse::<NodeId>() else {
        // Legacy/corrupt record without a parseable id: keep it — the
        // name-based restore path still applies.
        return;
    };
    // Err means the registry was unreachable (lock poisoned): neither
    // update nor clear — wiping the user's selection because of a lock
    // fault would be worse than a stale name (2026-08-12 boundary audit).
    let Ok(resolved) = resolve(&node_id) else {
        warn!(
            node_id = %record.node_id,
            "selection reconcile skipped: registry unreachable"
        );
        return;
    };
    match resolved {
        Some(current_name) if current_name != record.name => {
            info!(
                node_id = %record.node_id,
                from = %record.name,
                to = %current_name,
                "selection name reconciled after subscription refresh"
            );
            let updated = NodeSelectionRecord {
                name: current_name,
                ..record
            };
            if let Err(error) = save_node_selection(path, &updated) {
                warn!(
                    message = %error,
                    operation = ?error.operation,
                    "could not persist reconciled node selection"
                );
            }
        }
        Some(_) => {
            // Name unchanged: nothing to do.
        }
        None => {
            info!(
                node_id = %record.node_id,
                "selected node no longer exists; clearing selection"
            );
            clear_node_selection(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "caly-backends-selection-test-{}-{:?}",
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
    fn restore_drives_the_select_closure_with_the_record() {
        let dir = scratch();
        let path = dir.join("node-selection.json");
        save_node_selection(
            &path,
            &NodeSelectionRecord {
                node_id: "node-07".to_owned(),
                group: "PROXY".to_owned(),
                name: "Tokyo-02".to_owned(),
                core: "mihomo".to_owned(),
            },
        )
        .ok();
        let mut seen = None;
        restore_at(
            &path,
            caly_domain::CoreKind::Mihomo,
            Duration::from_secs(2),
            |group, name| {
                seen = Some((group.to_owned(), name.to_owned()));
                Ok(())
            },
        );
        assert_eq!(seen, Some(("PROXY".to_owned(), "Tokyo-02".to_owned())));
        // A successful restore keeps the record for the next restart.
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_skips_a_record_written_for_the_other_core() {
        // A sing-box record stores `proxy-<hex>` tags; re-applying it
        // against Mihomo would 400 and drop the selection. The record
        // is kept for when sing-box comes back.
        let dir = scratch();
        let path = dir.join("node-selection.json");
        save_node_selection(
            &path,
            &NodeSelectionRecord {
                node_id: "node-07".to_owned(),
                group: "PROXY".to_owned(),
                name: "proxy-0123".to_owned(),
                core: "sing-box".to_owned(),
            },
        )
        .ok();
        let mut called = false;
        restore_at(
            &path,
            caly_domain::CoreKind::Mihomo,
            Duration::from_secs(2),
            |_, _| {
                called = true;
                Ok(())
            },
        );
        assert!(!called, "other-core record must not be re-applied");
        assert!(path.exists(), "other-core record must be kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_restore_keeps_the_record() {
        let dir = scratch();
        let path = dir.join("node-selection.json");
        save_node_selection(
            &path,
            &NodeSelectionRecord {
                node_id: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
                group: "PROXY".to_owned(),
                name: "Gone-Node".to_owned(),
                core: "mihomo".to_owned(),
            },
        )
        .ok();
        restore_at(
            &path,
            caly_domain::CoreKind::Mihomo,
            Duration::from_secs(2),
            |_, _| Err("proxy not found".to_owned()), // 404 after a rename
        );
        // 刀 3: a failed re-apply (e.g. subscription renamed the node) must
        // keep the record — the reconciler follows the stable node id after
        // the next refresh, so the operator's choice survives.
        assert!(path.exists(), "record must survive a failed restore");
        let record = load_node_selection(&path).expect("record still loadable");
        assert_eq!(record.name, "Gone-Node");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_follows_renamed_node_by_id() {
        let dir = scratch();
        let path = dir.join("node-selection.json");
        save_node_selection(
            &path,
            &NodeSelectionRecord {
                node_id: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
                group: "PROXY".to_owned(),
                name: "FR - zhuhai.uk 02".to_owned(),
                core: "mihomo".to_owned(),
            },
        )
        .ok();
        reconcile_at(&path, caly_domain::CoreKind::Mihomo, |_| {
            Ok(Some("FR - zhuhai.uk 03".to_owned()))
        });
        let record = load_node_selection(&path).expect("record loadable");
        assert_eq!(record.name, "FR - zhuhai.uk 03");
        assert_eq!(
            record.node_id, "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
            "stable id must not change"
        );
        assert_eq!(record.group, "PROXY");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_clears_selection_when_node_is_gone() {
        let dir = scratch();
        let path = dir.join("node-selection.json");
        save_node_selection(
            &path,
            &NodeSelectionRecord {
                node_id: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
                group: "PROXY".to_owned(),
                name: "Vanished".to_owned(),
                core: "mihomo".to_owned(),
            },
        )
        .ok();
        reconcile_at(&path, caly_domain::CoreKind::Mihomo, |_| Ok(None));
        assert!(!path.exists(), "gone node must clear the selection");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_skips_foreign_core_records() {
        // A sing-box record stores proxy-<hex> tags; a mihomo refresh must
        // never rewrite it with the mihomo display name (2026-08-12 agent
        // audit — this used to corrupt the record and break restore
        // forever).
        let dir = scratch();
        let path = dir.join("node-selection.json");
        save_node_selection(
            &path,
            &NodeSelectionRecord {
                node_id: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
                group: "PROXY".to_owned(),
                name: "proxy-0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
                core: "sing-box".to_owned(),
            },
        )
        .ok();
        reconcile_at(&path, caly_domain::CoreKind::Mihomo, |_| {
            Ok(Some("renamed display".to_owned()))
        });
        let record = load_node_selection(&path).expect("record loadable");
        assert_eq!(record.name, "proxy-0f1e2d3c4b5a69788796a5b4c3d2e1f0");
        assert_eq!(record.core, "sing-box");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_without_record_is_a_noop() {
        let dir = scratch();
        let path = dir.join("missing.json");
        let mut called = false;
        reconcile_at(&path, caly_domain::CoreKind::Mihomo, |_| {
            called = true;
            Ok(None)
        });
        assert!(!called, "no record, no resolution");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_without_record_is_a_noop() {
        let dir = scratch();
        let path = dir.join("missing.json");
        let mut called = false;
        restore_at(
            &path,
            caly_domain::CoreKind::Mihomo,
            Duration::from_secs(2),
            |_, _| {
                called = true;
                Ok(())
            },
        );
        assert!(!called);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
