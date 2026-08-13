//! Hermetic tests for the offline config generation and reset flows.

use super::{generate_at, reset_at};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "caly-config-gen-{tag}-{nanos}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

#[test]
fn generate_writes_a_valid_layout_into_an_empty_root() {
    let root = temp_root("generate");
    let generated = generate_at(&root);
    assert!(generated.is_ok(), "generate failed: {generated:?}");
    assert!(root.join("config.yaml").is_file());
    assert!(root.join("config.d/10-core.yaml").is_file());
    // The written layout must load through the daemon's layered loader.
    let loaded = crate::daemon_config::load_from(root.clone());
    assert!(
        matches!(loaded, Ok(Some(_))),
        "generated layout must validate: {loaded:?}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn generate_refuses_to_overwrite_an_existing_base_config() {
    let root = temp_root("generate-exists");
    assert!(generate_at(&root).is_ok());
    let second = generate_at(&root);
    let Err(message) = second else {
        panic!("second generate must refuse")
    };
    assert!(message.contains("already exists"));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn reset_preserves_modified_content_in_a_backup_directory() {
    let root = temp_root("reset");
    assert!(generate_at(&root).is_ok());
    // Modify a fragment so the reset has user content to preserve.
    let fragment = root.join("config.d/30-subscriptions.yaml");
    fs::write(
        &fragment,
        "subscriptions:\n  url: https://user.example/sub\n",
    )
    .unwrap_or_default();

    let backup = reset_at(&root);
    let backup = backup.unwrap_or_else(|error| panic!("reset failed: {error}"));
    let backup_dir =
        backup.unwrap_or_else(|| panic!("reset must produce a backup for a non-empty root"));
    // The modified fragment must survive in the backup, untouched.
    let preserved =
        fs::read_to_string(backup_dir.join("config.d/30-subscriptions.yaml")).unwrap_or_default();
    assert!(preserved.contains("user.example"));
    // The live layout is a fresh, valid default again.
    let refreshed = fs::read_to_string(&fragment).unwrap_or_default();
    assert!(refreshed.contains("subscriptions: {}"));
    let loaded = crate::daemon_config::load_from(root.clone());
    assert!(matches!(loaded, Ok(Some(_))), "reset layout: {loaded:?}");
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&backup_dir).ok();
}

#[test]
fn reset_on_an_absent_root_writes_a_fresh_layout_without_backup() {
    let root = temp_root("reset-empty");
    fs::remove_dir_all(&root).ok();
    let backup = reset_at(&root);
    let backup = backup.unwrap_or_else(|error| panic!("reset on absent root failed: {error}"));
    assert!(backup.is_none(), "nothing to back up for an absent root");
    assert!(root.join("config.yaml").is_file());
    fs::remove_dir_all(&root).ok();
}
