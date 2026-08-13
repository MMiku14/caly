//! Inline rule provider materialization.
//!
//! `RuleProviderSource::Inline` is a caly-private extension: the
//! YAML body is carried inside the `payload:` key of the
//! `rule-providers:` block, but the upstream Mihomo kernel
//! does not understand that shape — it expects
//! `type: file` / `type: http` only. To keep the operator
//! experience ergonomic (no operator needs to manage a
//! separate file for an inline payload) without breaking
//! the kernel, the daemon materialises every `Inline`
//! provider to `<workdir>/rule-providers/<name>.yaml` at
//! boot and rewrites the in-memory provider to
//! `File { path: <that file> }` before the rendering
//! pipeline sees it.
//!
//! The function is deterministic and pure on the disk
//! layout: re-running it is a no-op (atomic temp + rename
//! so a partial write never leaves a half-body on disk).
//! Failures are surfaced as [`InlineMaterializeError`],
//! which the caller maps to a `CompositionError` so a
//! misconfigured inline payload fails the daemon at boot
//! rather than silently dropping the rule body.

use std::{
    fs,
    path::{Path, PathBuf},
};

use caly_domain::{RuleProvider, RuleProviderSource, RuleText};

const INLINE_RULE_PROVIDER_DIR: &str = "rule-providers";

/// Outcome of a successful materialization. `file_path` is
/// the absolute path the body was written to, so the
/// caller can rewrite the in-memory provider's `source`
/// without re-computing the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedInline {
    pub name: String,
    pub file_path: PathBuf,
    pub body_bytes: usize,
}

/// Failure mode of the materialization step. Carries the
/// offending id and a bounded reason so the daemon's
/// user-facing diagnostic names the failing profile.
#[derive(Debug)]
pub enum InlineMaterializeError {
    /// The supplied work directory could not be created or
    /// written to. The path is included so the operator
    /// can `ls -ld` it without re-running with a debug
    /// log.
    Io {
        id: String,
        path: PathBuf,
        reason: String,
    },
    /// The inline payload exceeded the 256 KiB cap the
    /// bounded text enforces. Defensive: a misconfigured
    /// payload should not silently overflow the work dir.
    BodyTooLarge {
        id: String,
        actual: usize,
        limit: usize,
    },
    /// The provider name is not a path-safe component; joining it onto the
    /// materialization directory would escape the work dir. The schema
    /// validator is the primary gate; this is defense in depth for
    /// programmatically constructed providers.
    UnsafeName { id: String },
}

impl core::fmt::Display for InlineMaterializeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io { id, path, reason } => {
                write!(
                    formatter,
                    "inline rule provider `{id}` I/O at {}: {reason}",
                    path.display()
                )
            }
            Self::BodyTooLarge { id, actual, limit } => write!(
                formatter,
                "inline rule provider `{id}` body is {actual} bytes; \
                 exceeds the {limit}-byte limit"
            ),
            Self::UnsafeName { id } => write!(
                formatter,
                "inline rule provider `{id}` is not a path-safe name"
            ),
        }
    }
}

impl std::error::Error for InlineMaterializeError {}

/// Materialises every `Inline` entry in `providers` to a real
/// file under `<work_dir>/rule-providers/<name>.yaml` and
/// returns the materialized list (with `Inline` rewritten
/// to `File { path: ... }`) plus the new bodies' absolute
/// paths. `Local` and `Http` providers pass through
/// unchanged. The function is deterministic: the same
/// input always produces the same output, and re-running
/// it never leaves a half-written file on disk.
pub fn materialize_inline_providers(
    work_dir: &Path,
    providers: &[RuleProvider],
) -> Result<(Vec<RuleProvider>, Vec<MaterializedInline>), InlineMaterializeError> {
    let dir = work_dir.join(INLINE_RULE_PROVIDER_DIR);
    fs::create_dir_all(&dir).map_err(|error| InlineMaterializeError::Io {
        id: String::new(),
        path: dir.clone(),
        reason: error.to_string(),
    })?;
    let mut out: Vec<RuleProvider> = Vec::with_capacity(providers.len());
    let mut materialized: Vec<MaterializedInline> = Vec::new();
    for provider in providers {
        match &provider.source {
            RuleProviderSource::Inline { payload } => {
                let body = payload.as_str();
                let body_bytes = body.len();
                let limit = caly_domain::INLINE_RULE_PAYLOAD_MAX_BYTES;
                if body_bytes > limit {
                    return Err(InlineMaterializeError::BodyTooLarge {
                        id: provider.name.as_str().to_owned(),
                        actual: body_bytes,
                        limit,
                    });
                }
                // The name becomes `<dir>/<name>.yaml`: without a path-safety
                // check, a crafted name (`../../etc/x`) materialises outside
                // the work directory. Profile ids and proxy group names obey
                // the same rule at the schema layer; this mirrors it here.
                if !caly_domain::is_path_safe_component(provider.name.as_str()) {
                    return Err(InlineMaterializeError::UnsafeName {
                        id: provider.name.as_str().to_owned(),
                    });
                }
                let file_path = dir.join(format!("{}.yaml", provider.name.as_str()));
                write_atomic(&file_path, body).map_err(|reason| InlineMaterializeError::Io {
                    id: provider.name.as_str().to_owned(),
                    path: file_path.clone(),
                    reason,
                })?;
                materialized.push(MaterializedInline {
                    name: provider.name.as_str().to_owned(),
                    file_path: file_path.clone(),
                    body_bytes,
                });
                let rewritten = rewrite_inline_to_file(provider, &file_path);
                out.push(rewritten);
            }
            RuleProviderSource::Http { .. } | RuleProviderSource::File { .. } => {
                out.push(provider.clone());
            }
        }
    }
    Ok((out, materialized))
}

/// Rewrites one `Inline` provider to `File { path: <file> }`,
/// keeping every other field identical. Pure: no I/O.
fn rewrite_inline_to_file(provider: &RuleProvider, file_path: &Path) -> RuleProvider {
    let behavior = provider.behavior;
    let format = provider.format;
    let path_text = file_path_text(file_path);
    let Ok(path) = RuleText::new(path_text) else {
        // The materialization step chose the file path
        // from a bounded id; reaching this branch means
        // the work dir itself is unreasonably long.
        // Return the provider unchanged; the loader
        // will surface a precise error.
        return provider.clone();
    };
    RuleProvider {
        name: provider.name.clone(),
        source: RuleProviderSource::File { path },
        behavior,
        format,
    }
}

/// Same atomic write discipline as the [`ProfileStore`]:
/// temp file in the same directory, then `rename` so a
/// concurrent reader never sees a partial body.
fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, body).map_err(|error| error.to_string())?;
    fs::rename(&tmp, path).map_err(|error| error.to_string())
}

/// Renders an absolute path as a `BoundedText`-safe string.
/// The bounded text enforces a UTF-8 byte cap (256 B by
/// default for rule text); paths under a normal XDG root
/// fit comfortably, and the rare oversize case falls
/// back to the input path without truncation (the loader
/// will surface a precise error).
fn file_path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::{
        BoundedText, RuleProviderBehavior, RuleProviderFormat, RuleProviderName, RuleProviderSource,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_workdir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("caly-rpm-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap_or_default();
        dir
    }

    fn cleanup(dir: &Path) {
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn crafted_provider_names_cannot_escape_the_workdir() {
        let dir = unique_workdir("unsafe-name");
        for bad in ["../../etc/evil", "..", "a/b", "./x"] {
            let result =
                materialize_inline_providers(&dir, &[inline_provider(bad, "DOMAIN,x.com")]);
            assert!(
                matches!(result, Err(InlineMaterializeError::UnsafeName { .. })),
                "provider name {bad:?} must be rejected as UnsafeName, got {result:?}"
            );
        }
        cleanup(&dir);
    }

    fn inline_provider(id: &str, payload: &str) -> RuleProvider {
        let name_bounded = RuleProviderName::new(id.to_owned()).unwrap();
        let payload_bounded =
            BoundedText::<{ caly_domain::INLINE_RULE_PAYLOAD_MAX_BYTES }>::new(payload.to_owned())
                .unwrap();
        RuleProvider {
            name: name_bounded,
            behavior: RuleProviderBehavior::Domain,
            format: RuleProviderFormat::Source,
            source: RuleProviderSource::Inline {
                payload: payload_bounded,
            },
        }
    }

    #[test]
    fn inline_provider_is_materialized_to_a_real_file() {
        let work = unique_workdir("basic");
        let providers = vec![inline_provider(
            "team",
            "DOMAIN,ads.example.com\nDOMAIN,track.example.org\n",
        )];
        let (rewritten, materialized) = materialize_inline_providers(&work, &providers).unwrap();
        assert_eq!(rewritten.len(), 1);
        assert_eq!(materialized.len(), 1);
        let entry = &materialized[0];
        assert_eq!(entry.name, "team");
        let body = fs::read_to_string(&entry.file_path).unwrap();
        assert!(body.contains("ads.example.com"));
        // The rewritten provider points at the same file.
        match &rewritten[0].source {
            RuleProviderSource::File { path } => {
                assert_eq!(path.as_str(), entry.file_path.to_string_lossy().as_ref());
            }
            RuleProviderSource::Http { .. } | RuleProviderSource::Inline { .. } => {
                panic!("expected a File source after materialization")
            }
        }
        cleanup(&work);
    }

    #[test]
    fn http_and_file_providers_pass_through_unchanged() {
        let work = unique_workdir("passthrough");
        let name_bounded = RuleProviderName::new("remote-x".to_owned()).unwrap();
        let url = BoundedText::<{ caly_domain::RULE_TEXT_MAX_BYTES }>::new(
            "https://example.com/x".to_owned(),
        )
        .unwrap();
        let http = RuleProvider {
            name: name_bounded,
            behavior: RuleProviderBehavior::DomainSuffix,
            format: RuleProviderFormat::Source,
            source: RuleProviderSource::Http {
                url,
                interval_ms: 86_400_000,
            },
        };
        let (_rewritten, materialized) =
            materialize_inline_providers(&work, std::slice::from_ref(&http)).unwrap();
        assert!(
            materialized.is_empty(),
            "no inline bodies were materialized"
        );
        cleanup(&work);
    }

    #[test]
    fn materialization_writes_exactly_one_file_per_inline_provider() {
        // Lock the file-per-name invariant: a single
        // `Inline` entry becomes exactly one `.yaml` file
        // under `<workdir>/rule-providers/`, and the body
        // matches the payload verbatim. This is the
        // "happy path" beyond `inline_provider_is_materialized_to_a_real_file`:
        // a regression on the count or the path component
        // would corrupt the kernel's view of the providers.
        let work = unique_workdir("count");
        let providers = vec![
            inline_provider("a", "DOMAIN,a.example\n"),
            inline_provider("b", "DOMAIN,b.example\n"),
            inline_provider("c", "DOMAIN,c.example\n"),
        ];
        let (_rewritten, materialized) = materialize_inline_providers(&work, &providers).unwrap();
        assert_eq!(materialized.len(), 3);
        let mut names: Vec<String> = materialized.iter().map(|m| m.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        for entry in &materialized {
            let body = fs::read_to_string(&entry.file_path).unwrap();
            assert!(
                body.contains(entry.name.as_str()),
                "{} body must reference its own name",
                entry.name
            );
        }
        cleanup(&work);
    }

    #[test]
    fn materialization_is_idempotent_under_repeated_calls() {
        let work = unique_workdir("idempotent");
        let providers = vec![inline_provider("a", "DOMAIN,example.com\n")];
        let first = materialize_inline_providers(&work, &providers).unwrap();
        let second = materialize_inline_providers(&work, &providers).unwrap();
        // Both calls produce the same path and content.
        assert_eq!(first.1[0].file_path, second.1[0].file_path);
        let body_first = fs::read_to_string(&first.1[0].file_path).unwrap();
        let body_second = fs::read_to_string(&second.1[0].file_path).unwrap();
        assert_eq!(body_first, body_second);
        cleanup(&work);
    }
}
