//! Round 16: offline inline-proxy writer for the
//! `set proxy …` leaves.
//!
//! The 4 CRUD leaves (`add` / `edit` / `remove` / `import`)
//! round-trip inline proxy URIs as JSON files under
//! `<state>/inline-proxies/<id>.json`. The on-disk shape
//! is intentionally minimal — `{ "uri": "...", "group":
//! optional, "added_at_ms": u64 }` — so the kernel's
//! own URI parser is the only source of truth for the
//! node structure (a single proxy URI maps to one node).
//!
//! # Why a separate module (not inside `subscription.rs`)
//!
//! `subscription` is for *fetched* content (subscription
//! sources refresh over the network). Inline proxies are
//! operator-typed content: the URI is the body, the
//! identity is a hash of the URI, and the file lifetime
//! matches the operator's `set proxy add/remove` flow.
//! Splitting the two resources keeps the writer
//! invariants simple (no daemon refresh, no `Merge`
//! recursion, no source-name de-duplication).

use std::path::{Path, PathBuf};

use caly_platform::paths::AppPaths;

const INLINE_PROXY_DIR: &str = "inline-proxies";

/// Round 16: convenience re-export so the dispatch in
/// `commands::set::proxy` doesn't have to reach into
/// `caly_platform::paths` directly.
pub fn resolve_paths_pub() -> AppPaths {
    AppPaths::from_env()
}

/// Single inline proxy entry. The URI is the operator's
/// raw input; the kernel's URI parser is the source of
/// truth for the structured node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineProxy {
    /// The operator-typed proxy URI (`vmess://...`,
    /// `ss://...`, `trojan://...`, etc.). Stored as
    /// received — round-trip through `serde_json` is
    /// faithful.
    pub uri: String,
    /// Optional proxy group the inline node belongs to.
    /// Round 16: advisory; the kernel reads it on the
    /// `config apply` path.
    pub group: Option<String>,
    /// Wall-clock ms when the entry was added. Used as
    /// a stable ordering hint for the kernel.
    pub added_at_ms: u64,
}

/// Single error type for the 4 CRUD writers. The CLI maps
/// each variant to a stable `code:` so the operator gets
/// a consistent envelope across writers.
#[derive(Debug)]
pub enum InlineProxyError {
    /// The URI is empty or fails basic shape validation
    /// (`scheme://...`). The kernel's parser is the
    /// source of truth for full validation, but a
    /// trivially-malformed URI never reaches disk.
    InvalidUri(String),
    /// `add` / `import` on a URI whose hash is already
    /// present in `<state>/inline-proxies/`.
    AlreadyDeclared(String),
    /// `remove` / `edit` on a URI whose hash isn't in
    /// the directory.
    NotDeclared(String),
    /// The proxy file is unreadable / unwriteable.
    Io(String),
}

impl core::fmt::Display for InlineProxyError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidUri(reason) => write!(formatter, "invalid proxy URI: {reason}"),
            Self::AlreadyDeclared(id) => {
                write!(formatter, "inline proxy `{id}` is already declared")
            }
            Self::NotDeclared(id) => write!(formatter, "inline proxy `{id}` is not declared"),
            Self::Io(reason) => write!(formatter, "inline proxy I/O failed: {reason}"),
        }
    }
}

impl std::error::Error for InlineProxyError {}

// W2-β2a: no remediation hint of its own yet — the default
// `None` from the shared seam keeps the §8 envelope shape unchanged.
impl crate::output::ErrorHint for InlineProxyError {}

/// What an `add` / `remove` / `edit` call did.
/// `DryRun` is what `--dry-run` (the default) returns
/// when the call would have written.
///
/// Round 23: every variant carries the on-disk `id`
/// (a SHA-1-derived 16-hex hash of the URI) so the
/// shared `commands::set::common::run_writer` envelope
/// can project it into the success envelope's JSON
/// payload. The previous `enum { Applied, DryRun }`
/// shape required the dispatch to thread a separate
/// `(outcome, id)` tuple from `add_proxy`; folding
/// the id into the outcome lets the `is_dry_run` and
/// `extra_payload` closures in `run_writer` extract
/// both pieces from a single value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineProxyOutcome {
    /// The call wrote (or would have written) the
    /// on-disk entry identified by `id`.
    Applied { id: String },
    /// The call validated but did not write; the
    /// would-be id is reported so the operator can
    /// reference it.
    DryRun { id: String },
}

fn inline_dir(paths: &AppPaths) -> PathBuf {
    paths.state.join(INLINE_PROXY_DIR)
}

fn validate_uri(uri: &str) -> Result<(), InlineProxyError> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err(InlineProxyError::InvalidUri("URI is empty".to_owned()));
    }
    if !(uri.starts_with("vmess://")
        || uri.starts_with("ss://")
        || uri.starts_with("trojan://")
        || uri.starts_with("vless://")
        || uri.starts_with("hysteria://")
        || uri.starts_with("hysteria2://")
        || uri.starts_with("tuic://")
        || uri.starts_with("wireguard://"))
    {
        return Err(InlineProxyError::InvalidUri(format!(
            "URI must start with a known proxy scheme (vmess, ss, trojan, vless, hysteria, hysteria2, tuic, wireguard); got `{}`",
            &uri[..uri.len().min(20)]
        )));
    }
    Ok(())
}

/// Stable id derived from the URI. The pre-Round 27
/// docstring claimed this was "SHA-256 hex of the URI
/// bytes, truncated to 16 hex chars" — it is not.
/// The actual id is `DefaultHasher::finish()` (a
/// SipHash-2-4 with a Rust 1.88 deterministic key —
/// **stable across processes**, so the on-disk
/// filename is portable between the CLI and any
/// other reader of `<state>/inline-proxies/`).
/// 16 hex chars is 64 bits — enough to make
/// operator-visible collisions effectively zero
/// for any realistic inline-proxy collection size.
///
/// Round 27 (debug): the doc-vs-implementation drift
/// is not a behavioural bug (the id is stable
/// across processes and the on-disk filename
/// round-trips through `remove_proxy` /
/// `import_proxy`), but it is a *maintenance*
/// trap. A future Round that migrates this
/// function to a real SHA-256 must also migrate
/// every existing on-disk file (rename the JSON
/// files under `<state>/inline-proxies/` from
/// the SipHash id to the SHA-256 id), and the
/// cost of that migration is hidden if the
/// docstring keeps the "SHA-256" lie. The
/// corrected docstring is the warning the next
/// reader needs.
fn id_for_uri(uri: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    uri.hash(&mut hasher);
    let h = hasher.finish();
    format!("{h:016x}")
}

/// Returns the on-disk path for `id` *without* touching
/// the filesystem. The dry-run writers use this so a
/// `set proxy add` dry-run is truly read-only: the parent
/// directory is not created, the file is not opened,
/// the inline-proxies dir is left untouched if it
/// didn't already exist. Apply mode still uses
/// [`path_for_id_and_ensure_dir`] below.
fn path_for_id(paths: &AppPaths, id: &str) -> PathBuf {
    inline_dir(paths).join(format!("{id}.json"))
}

/// Returns the on-disk path for `id`, creating the
/// inline-proxies directory if it doesn't exist. Used
/// by the apply path and the read-back helpers
/// (`list_proxies`, `read_entry`).
fn path_for_id_and_ensure_dir(paths: &AppPaths, id: &str) -> Result<PathBuf, InlineProxyError> {
    let dir = inline_dir(paths);
    std::fs::create_dir_all(&dir).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    Ok(dir.join(format!("{id}.json")))
}

fn read_entry(path: &Path) -> Result<InlineProxy, InlineProxyError> {
    let bytes = std::fs::read(path).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    serde_json::from_slice::<InlineProxyEntry>(&bytes)
        .map(InlineProxy::from)
        .map_err(|e| InlineProxyError::Io(format!("parse {}: {e}", path.display())))
}

fn write_entry(path: &Path, entry: &InlineProxy) -> Result<(), InlineProxyError> {
    let serialized = serde_json::to_vec_pretty(&InlineProxyEntry::from(entry.clone()))
        .map_err(|e| InlineProxyError::Io(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &serialized).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InlineProxyEntry {
    uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    added_at_ms: u64,
}

impl From<InlineProxy> for InlineProxyEntry {
    fn from(p: InlineProxy) -> Self {
        Self {
            uri: p.uri,
            group: p.group,
            added_at_ms: p.added_at_ms,
        }
    }
}

impl From<InlineProxyEntry> for InlineProxy {
    fn from(e: InlineProxyEntry) -> Self {
        Self {
            uri: e.uri,
            group: e.group,
            added_at_ms: e.added_at_ms,
        }
    }
}

mod writers;

#[cfg(test)]
use writers::GROUP_COMMENT_PREFIX;

pub use writers::{ImportOutcome, add_proxy, edit_proxy, import_proxy, remove_proxy};

/// Whether `value` has the canonical inline-proxy id shape (16 lowercase hex
/// characters — the SipHash digest `id_for_uri` produces). Anything else is
/// *operator input*, never a filename: without this gate, `remove ../../x`
/// resolved to `<state>/inline-proxies/../../x.json` (path traversal).
fn is_canonical_id(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Resolves a user-supplied id (16-hex hash or the URI
/// itself) to the canonical 16-hex id. Returns
/// `NotDeclared` if the resolved file doesn't exist.
/// Non-canonical input (including traversal attempts such
/// as `../../x`) is rejected as an invalid id *before* any
/// path is formed.
fn resolve_id(paths: &AppPaths, id_or_uri: &str) -> Result<String, InlineProxyError> {
    let candidate = if id_or_uri.contains("://") {
        // operator supplied the raw URI; hash it.
        id_for_uri(id_or_uri)
    } else {
        id_or_uri.to_owned()
    };
    if !is_canonical_id(&candidate) {
        return Err(InlineProxyError::NotDeclared(format!(
            "{id_or_uri} (inline proxy ids are 16 hexadecimal characters)"
        )));
    }
    let path = path_for_id(paths, &candidate);
    if path.exists() {
        Ok(candidate)
    } else {
        Err(InlineProxyError::NotDeclared(candidate))
    }
}

/// Lists all inline proxies (id, uri) pairs, in the order
/// they appear on disk (a sorted `BTreeMap` insertion
/// order — proxy ids are random hex so filesystem order
/// is arbitrary, but stable for the lifetime of the
/// directory).
///
/// Used by `commands::set::proxy::tests` as the canonical
/// "how many inline proxies did the writer persist?"
/// assertion helper. The CLI no longer has a dedicated
/// `show proxy list` leaf (Round 12 retired it; the
/// `set proxy add|remove|edit|import` envelope reports
/// the on-disk count through its `Applied { count }` /
/// `DryRun { count }` outcomes instead). The function
/// stays public so future operator-facing surfaces
/// (e.g. a `show proxy list` re-introduction, a status
/// line) can reuse the same `(id, uri)` projection.
#[allow(dead_code)] // test-only consumer in the bin unit
// (clippy `--no-deps` does not see
// `#[cfg(test)] mod tests`); the
// forward-compat note above still
// applies to any future CLI surface.
pub fn list_proxies(paths: &AppPaths) -> Vec<(String, String)> {
    let dir = inline_dir(paths);
    let mut out = Vec::new();
    // Round 19: if the inline-proxies directory
    // doesn't exist yet (no inline proxies added),
    // report an empty list instead of erroring.
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(entry) = read_entry(&path) {
            out.push((stem.to_owned(), entry.uri));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests;
