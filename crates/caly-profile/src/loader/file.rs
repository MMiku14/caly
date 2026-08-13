//! Bounded daemon-owned configuration file loading.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

use crate::schema::{
    AppConfig, ConfigError, MAX_CONFIG_BYTES, parse_and_validate_json, parse_and_validate_yaml,
};

/// File-boundary failure without exposing configuration contents.
#[derive(Debug)]
pub enum ConfigFileError {
    Metadata(io::Error),
    SymlinkRejected,
    NotRegularFile,
    TooLarge { limit: usize, actual: u64 },
    Open(io::Error),
    Read(io::Error),
    Config(ConfigError),
}

impl core::fmt::Display for ConfigFileError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Metadata(error) => write!(formatter, "cannot inspect configuration: {error}"),
            Self::SymlinkRejected => formatter
                .write_str("configuration symlinks are rejected; use an owned regular file"),
            Self::NotRegularFile => {
                formatter.write_str("configuration path must be a regular file")
            }
            Self::TooLarge { limit, actual } => write!(
                formatter,
                "configuration is {actual} bytes; reduce it to at most {limit} bytes"
            ),
            Self::Open(error) => write!(formatter, "cannot open configuration: {error}"),
            Self::Read(error) => write!(formatter, "cannot read configuration: {error}"),
            Self::Config(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ConfigFileError {}

/// Reads one regular non-symlink JSON file under a strict byte ceiling.
pub fn load_json_file(path: &Path) -> Result<AppConfig, ConfigFileError> {
    let bytes = read_bounded_regular_file(path, MAX_CONFIG_BYTES)?;
    parse_and_validate_json(&bytes).map_err(ConfigFileError::Config)
}

/// Reads and validates a daemon configuration file, auto-detecting YAML or
/// JSON by content. This is the single entry for `config check` so it accepts
/// the same YAML configuration the daemon loads (not only JSON).
pub fn load_config_file(path: &Path) -> Result<AppConfig, ConfigFileError> {
    let bytes = read_bounded_regular_file(path, MAX_CONFIG_BYTES)?;
    // JSON is a strict subset of YAML, so try JSON first; fall back to YAML.
    parse_and_validate_json(&bytes)
        .or_else(|_| parse_and_validate_yaml(&bytes))
        .map_err(ConfigFileError::Config)
}

pub(crate) fn read_bounded_regular_file(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, ConfigFileError> {
    let metadata = fs::symlink_metadata(path).map_err(ConfigFileError::Metadata)?;
    if metadata.file_type().is_symlink() {
        return Err(ConfigFileError::SymlinkRejected);
    }
    if !metadata.is_file() {
        return Err(ConfigFileError::NotRegularFile);
    }
    if metadata.len() > limit as u64 {
        return Err(ConfigFileError::TooLarge {
            limit,
            actual: metadata.len(),
        });
    }
    let file = File::open(path).map_err(ConfigFileError::Open)?;
    let capacity = usize::try_from(metadata.len()).map_err(|_| ConfigFileError::TooLarge {
        limit,
        actual: metadata.len(),
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take((limit as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(ConfigFileError::Read)?;
    if bytes.len() > limit {
        return Err(ConfigFileError::TooLarge {
            limit,
            actual: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_platform::paths::test_helpers::unique_path_under;

    #[test]
    fn config_file_accepts_yaml() -> Result<(), String> {
        let path = unique_path_under("caly-config-file", "yaml");
        std::fs::write(
            &path,
            "schema_version: 1\ncore: mihomo\ndaemon:\n  listen: 127.0.0.1:17890\n",
        )
        .map_err(|e| e.to_string())?;
        let config = load_config_file(&path).map_err(|e| format!("{e:?}"))?;
        assert_eq!(config.core, crate::schema::CoreConfig::Mihomo);
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn config_file_accepts_json() -> Result<(), String> {
        let path = unique_path_under("caly-config-file", "json");
        std::fs::write(&path, r#"{"schema_version":1,"core":"sing-box"}"#)
            .map_err(|e| e.to_string())?;
        let config = load_config_file(&path).map_err(|e| format!("{e:?}"))?;
        assert_eq!(config.core, crate::schema::CoreConfig::SingBox);
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn config_file_rejects_invalid() {
        let path = unique_path_under("caly-config-file", "bad");
        std::fs::write(&path, "not: [valid\n").unwrap_or_default();
        assert!(load_config_file(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
