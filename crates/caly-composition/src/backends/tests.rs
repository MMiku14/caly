//! Tests for `composition/backends.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use std::{
    os::unix::fs::PermissionsExt,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn sing_box_config_publication_is_owner_only() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("caly-singbox-mode-{nonce}"));
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let destination = directory.join("config.json");
    super::lifecycle_support::publish_owner_only_config(
        &destination,
        br#"{"secret":"credential"}"#.to_vec(),
        1,
    )
    .map_err(|error| format!("{error:?}"))?;
    let mode = std::fs::metadata(&destination)
        .map_err(|error| error.to_string())?
        .permissions()
        .mode()
        & 0o777;
    let cleanup = std::fs::remove_dir_all(directory);
    assert_eq!(mode, 0o600);
    cleanup.map_err(|error| error.to_string())
}
