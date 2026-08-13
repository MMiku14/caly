//! Config publication helpers for core lifecycle builders: atomic owner-only
//! writes and controller-secret re-keying of reused generations.

use std::path::PathBuf;

use super::CompositionError;

pub(super) fn publish_owner_only_config(
    destination: &std::path::Path,
    bytes: Vec<u8>,
    generation: u64,
) -> Result<(), CompositionError> {
    let contents = caly_platform::fs::AtomicFileContents::try_from_vec(bytes)
        .map_err(|error| {
            tracing::error!(?error, destination = ?destination, "config body rejected for atomic publish");
            CompositionError::BackendUnavailable
        })?;
    let mut temporary = destination.as_os_str().to_os_string();
    temporary.push(format!(".tmp.{generation}"));
    let temporary_path = PathBuf::from(temporary);
    // A crashed writer can leave `{destination}.tmp.{generation}` behind;
    // with a fixed temporary name the next create-new (O_EXCL) would fail
    // forever and block every later publish (observed: a stale `mihomo.yaml
    // .tmp.1` made start_runtime fail with BackendUnavailable). The name is
    // deterministic and this is the single writer, so clearing a leftover
    // is safe; the atomic replace still guarantees the destination is never
    // observed half-written.
    if temporary_path.exists() {
        std::fs::remove_file(&temporary_path).map_err(|error| {
            tracing::error!(?error, path = ?temporary_path, "cannot clear a stale config temp file");
            CompositionError::BackendUnavailable
        })?;
    }
    caly_platform::fs::atomic_write(
        &mut caly_platform::fs::LinuxAtomicFileBackend,
        caly_platform::fs::AtomicWritePlan {
            destination: destination.to_path_buf(),
            temporary: temporary_path,
            contents,
        },
    )
    .map_err(|error| {
        tracing::error!(?error, destination = ?destination, "atomic config publish failed");
        CompositionError::BackendUnavailable
    })
}

/// Rewrites the controller `secret` in a previously published config so a
/// daemon restart (which rotates the secret) keeps the running kernel
/// reachable without discarding committed subscription nodes.
pub(super) fn refresh_config_secret(
    path: &std::path::Path,
    secret: &str,
) -> Result<(), CompositionError> {
    if !path.is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|error| {
        tracing::error!(?error, path = ?path, "cannot read the published config for secret re-keying");
        CompositionError::BackendUnavailable
    })?;
    let updated = if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        refresh_json_secret(&bytes, secret)
    } else {
        refresh_yaml_secret(&bytes, secret)
    };
    let Some(updated) = updated else {
        // Audit #87: refuse to publish a document we could not safely re-key;
        // the previous shape silently wrote a corrupted/unrekeyed config.
        tracing::error!(path = ?path, "secret re-key aborted: config body not safely editable");
        return Err(CompositionError::BackendUnavailable);
    };
    // Audit #87: the staging suffix only names the O_EXCL temporary — scope
    // it to a dedicated rekey identity instead of hard-coding generation 1,
    // so the temporary can never be confused with a regular `config apply`
    // staging file.
    publish_owner_only_config(path, updated, REKEY_STAGING_SUFFIX)
}

/// Staging suffix dedicated to the secret re-key transaction (see #87).
const REKEY_STAGING_SUFFIX: u64 = u64::MAX;

/// Whether `secret` is safe to embed verbatim. Controller secrets are
/// hex-encoded (`generate_secret_hex`), so this is a belt-and-braces gate
/// against ever writing an escaping character into a config document (#87).
fn secret_is_plain(secret: &str) -> bool {
    !secret.is_empty()
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'='))
}

/// Replaces the sing-box `experimental.clash_api.secret` value. The document
/// is serde_json-rendered (compact `"secret":"..."`), so a targeted string
/// replacement is exact and dependency-free — but the search runs only
/// *within the `clash_api` object*, so an unrelated `"secret"` field
/// elsewhere in the document is never rewritten, and a backslash-escaped
/// quote inside the old value cannot misalign the replacement (#87).
fn refresh_json_secret(bytes: &[u8], secret: &str) -> Option<Vec<u8>> {
    if !secret_is_plain(secret) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let scope = text.find("\"clash_api\"")?;
    let after_scope = &text[scope..];
    let marker = "\"secret\":\"";
    let rel = after_scope.find(marker)?;
    let value_start = scope + rel + marker.len();
    // Find the terminating quote: a `"` not preceded by an odd run of `\`.
    let rest = &text[value_start..];
    let mut terminate = None;
    for (index, _) in rest.match_indices('"') {
        let backslashes = rest[..index]
            .chars()
            .rev()
            .take_while(|ch| *ch == '\\')
            .count();
        if backslashes % 2 == 0 {
            terminate = Some(index);
            break;
        }
    }
    let end = terminate?;
    let mut out = text.into_owned();
    out.replace_range(value_start..value_start + end, secret);
    Some(out.into_bytes())
}

/// Replaces the Mihomo top-level `secret:` line, appending one when absent.
/// The value is written single-quoted with `'` doubled (YAML single-quote
/// escaping), so the document stays valid even when the secret contains
/// YAML-significant characters (#87).
fn refresh_yaml_secret(bytes: &[u8], secret: &str) -> Option<Vec<u8>> {
    if !secret_is_plain(secret) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let line = format!(
        "secret: '{escaped}'\n",
        escaped = secret.replace('\'', "''")
    );
    let mut out = String::new();
    let mut replaced = false;
    for existing in text.lines() {
        if existing.starts_with("secret:") {
            out.push_str(&line);
            replaced = true;
        } else {
            out.push_str(existing);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&line);
    }
    Some(out.into_bytes())
}

/// Renders and publishes the first-run Mihomo base generation (no subscription
/// nodes exist yet); later generations come from `config apply`.
pub(super) fn render_mihomo_bootstrap(
    config: &std::path::Path,
    controller: &str,
    secret: Option<String>,
    tuning: &crate::RuntimeTuning,
    tun: Option<caly_domain::TunConfig>,
) -> Result<(), CompositionError> {
    // The rendered `external-controller` must match the address the control
    // clients dial; derive the port from the configured controller endpoint.
    let controller_port = controller
        .parse::<std::net::SocketAddr>()
        .map_or(9090, |address| address.port());
    let settings = caly_coreconf::mihomo::MihomoConfigSettings {
        mixed_port: tuning.mixed_port,
        external_controller_port: controller_port,
        allow_lan: tuning.allow_lan,
        log_level: tuning.log_level.clone(),
        dns: tuning.dns.clone(),
        tun,
        secret,
        transparent_port: if tuning.transparent.enabled {
            tuning.transparent.port
        } else {
            0
        },
        transparent_tproxy: matches!(
            tuning.transparent.mode,
            caly_profile::schema::TransparentMode::Tproxy
        ),
        ..caly_coreconf::mihomo::MihomoConfigSettings::default()
    };
    // P3a: the renderer went pure (caly-coreconf); publication uses the
    // shared owner-only atomic writer this module already provides.
    let bytes = caly_coreconf::mihomo::MihomoConfigRenderer
        .render(&settings)
        .map_err(|error| {
            tracing::error!(?error, path = ?config, "cannot render Mihomo bootstrap config");
            CompositionError::BackendUnavailable
        })?;
    publish_owner_only_config(config, bytes.as_slice().to_vec(), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "caly-publish-test-{}-{:?}",
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
    fn json_rekey_targets_clash_api_scope_and_tolerates_escaped_old_value() {
        // Audit #87: a `"secret"` key outside clash_api must survive; an old
        // value containing an escaped quote must not misalign the splice.
        let doc = br#"{"tls":{"secret":"keep-me"},"experimental":{"clash_api":{"secret":"old\"x","external_controller":"127.0.0.1:9097"}}}"#;
        let updated = refresh_json_secret(doc, "abc123").unwrap_or_default();
        let text = String::from_utf8(updated).unwrap_or_default();
        assert!(text.contains(r#""secret":"keep-me""#));
        assert!(text.contains(r#""secret":"abc123""#));
    }

    #[test]
    fn json_rekey_refuses_unsafe_secret() {
        let doc = br#"{"experimental":{"clash_api":{"secret":"old"}}}"#;
        assert!(refresh_json_secret(doc, "bad\"quote").is_none());
    }

    #[test]
    fn yaml_rekey_quotes_the_value() {
        let doc = b"external-controller: 127.0.0.1:9090\nsecret: old\n".to_vec();
        let updated = refresh_yaml_secret(&doc, "abc123").unwrap_or_default();
        let text = String::from_utf8(updated).unwrap_or_default();
        assert_eq!(
            text,
            "external-controller: 127.0.0.1:9090\nsecret: 'abc123'\n"
        );
    }

    #[test]
    fn yaml_rekey_appends_when_absent() {
        let doc = b"external-controller: 127.0.0.1:9090\n".to_vec();
        let updated = refresh_yaml_secret(&doc, "deadbeef").unwrap_or_default();
        let text = String::from_utf8(updated).unwrap_or_default();
        assert!(text.ends_with("secret: 'deadbeef'\n"));
    }

    #[test]
    fn stale_temporary_does_not_block_publication() {
        // Regression: a crashed writer's left-over `{dest}.tmp.{gen}` used to
        // make every later publish fail (`create_new` O_EXCL), blocking daemon
        // starts and config applies until manual cleanup.
        let dir = scratch();
        let destination = dir.join("mihomo.yaml");
        let stale = dir.join("mihomo.yaml.tmp.1");
        std::fs::write(&stale, b"half-written garbage").ok();
        assert!(publish_owner_only_config(&destination, b"fresh".to_vec(), 1).is_ok());
        assert_eq!(
            std::fs::read(&destination).ok().as_deref(),
            Some(b"fresh".as_slice())
        );
        // The stale temporary is gone and the destination is the new one.
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
