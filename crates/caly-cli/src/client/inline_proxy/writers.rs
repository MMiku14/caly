//! CRUD writers for the offline inline-proxy store.
//!
//! Split out of `client/inline_proxy.rs` (audit #70 file-length
//! budget): each writer round-trips the JSON entry files under
//! `<state>/inline-proxies/<id>.json` through the shared
//! [`super::write_entry`] / [`super::read_entry`] helpers, with the
//! dry-run flag living in the writer so the dispatch is a 1-line
//! `run_writer` call (the Round 22 / Round 30 contract).

use std::path::Path;

use caly_platform::paths::AppPaths;

use super::super::resource_writer::current_unix_ms;
use super::{
    InlineProxy, InlineProxyError, InlineProxyOutcome, id_for_uri, path_for_id,
    path_for_id_and_ensure_dir, read_entry, resolve_id, validate_uri, write_entry,
};

pub fn add_proxy(
    paths: &AppPaths,
    uri: &str,
    group: Option<&str>,
    apply: bool,
) -> Result<InlineProxyOutcome, InlineProxyError> {
    validate_uri(uri)?;
    // Round 27 (debug): the pre-Round 27 shape
    // called `id_for_uri(uri)` with the *raw*
    // user input, then wrote `uri.trim()` into
    // the file body. The two operations were
    // based on different strings, so a
    // whitespace-padded URI (`" vmess://abc"`)
    // and the trimmed version
    // (`"vmess://abc"`) produced *two* on-disk
    // files with *identical* content but
    // different ids — operator confusion
    // (the file list showed two entries, both
    // with the same URI) and wasted storage.
    // The post-Round 27 contract: the id and
    // the file body are both derived from the
    // trimmed URI. The trim is idempotent
    // (trim(trim(s)) == trim(s)) so the
    // round-trip path (`remove_proxy` /
    // `import_proxy` re-trim before computing
    // the id) stays stable.
    let trimmed = uri.trim();
    let id = id_for_uri(trimmed);
    // Round 19: dry-run uses the read-only path
    // helper (no `create_dir_all` side-effect), so
    // `set proxy add --dry-run` against a fresh
    // state root is truly side-effect-free. Apply
    // mode uses the ensure-dir helper.
    let path = path_for_id(paths, &id);
    if path.exists() {
        return Err(InlineProxyError::AlreadyDeclared(id));
    }
    if !apply {
        return Ok(InlineProxyOutcome::DryRun { id });
    }
    let path = path_for_id_and_ensure_dir(paths, &id)?;
    let entry = InlineProxy {
        uri: trimmed.to_owned(),
        group: group.map(str::to_owned),
        added_at_ms: current_unix_ms(),
    };
    write_entry(&path, &entry)?;
    Ok(InlineProxyOutcome::Applied { id })
}

/// `set proxy remove <id>` (id is the 16-hex id from
/// `list` or the URI itself).
///
/// Round 23: the `id` field of the outcome is the
/// resolved 16-hex id (canonical form after the user
/// input has been resolved against the inline-proxies
/// directory). The dispatch projects it into the
/// envelope so `set proxy remove` reports the id of
/// the entry actually removed (or that would have
/// been removed in dry-run mode).
pub fn remove_proxy(
    paths: &AppPaths,
    id: &str,
    apply: bool,
) -> Result<InlineProxyOutcome, InlineProxyError> {
    let resolved = resolve_id(paths, id)?;
    if !apply {
        return Ok(InlineProxyOutcome::DryRun { id: resolved });
    }
    let path = path_for_id(paths, &resolved);
    if !path.exists() {
        return Err(InlineProxyError::NotDeclared(resolved));
    }
    std::fs::remove_file(&path).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    Ok(InlineProxyOutcome::Applied { id: resolved })
}

/// `set proxy edit <id>` — spawn `$EDITOR` (or `vi`) with
/// the current URI line, write the result back.
///
/// Round 23: the `id` field of the outcome is the
/// **post-edit** id. If the operator typed a new URI,
/// the old entry was removed and a new one created at
/// the new id, so the JSON envelope reports the new
/// id (the one that actually lives on disk now).
pub fn edit_proxy(
    paths: &AppPaths,
    id: &str,
    apply: bool,
) -> Result<InlineProxyOutcome, InlineProxyError> {
    let resolved = resolve_id(paths, id)?;
    let path = path_for_id_and_ensure_dir(paths, &resolved)?;
    let entry = read_entry(&path)?;
    if !apply {
        return Ok(InlineProxyOutcome::DryRun { id: resolved });
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
    // The buffer the operator edits is plain text
    // (comment lines + one URI line), so name it
    // `.txt` — the old `.json` suffix made editors
    // apply JSON syntax validation against a file
    // that was never JSON.
    let tmp = std::env::temp_dir().join(format!("caly-inline-proxy-{resolved}.txt"));
    let editor_body = format!(
        "# Edit the URI line below; one URI per file.\n# group (optional): {}\n# id: {resolved}\n{}\n",
        entry.group.as_deref().unwrap_or(""),
        entry.uri,
    );
    std::fs::write(&tmp, editor_body).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    let status = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .map_err(|error| InlineProxyError::Io(format!("cannot spawn `{editor}`: {error}")))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(InlineProxyError::Io(format!(
            "editor `{editor}` exited with {status}"
        )));
    }
    let edited = std::fs::read_to_string(&tmp).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    let _ = std::fs::remove_file(&tmp);
    // The `# group (optional):` line is functional,
    // not cosmetic: edits to it now apply (previously
    // it was echoed back as an inert comment, so the
    // operator could never change the group via the
    // editor). Leaving the line value empty clears
    // the group; deleting the whole line keeps it.
    let mut new_group = entry.group.clone();
    for line in edited.lines() {
        if let Some(rest) = line.strip_prefix(GROUP_COMMENT_PREFIX) {
            let value = rest.trim();
            new_group = (!value.is_empty()).then(|| value.to_owned());
        }
    }
    let new_uri = edited
        .lines()
        .find(|line| !line.starts_with('#') && !line.trim().is_empty())
        .ok_or_else(|| {
            InlineProxyError::InvalidUri("editor left the body empty; aborting".to_owned())
        })?
        .trim()
        .to_owned();
    validate_uri(&new_uri)?;
    let new_id = id_for_uri(&new_uri);
    if new_id == resolved {
        // Same URI. The group may still have changed;
        // only rewrite when the entry actually moved
        // (a blind rewrite would churn the mtime).
        if new_group != entry.group {
            let new_entry = InlineProxy {
                uri: entry.uri.clone(),
                group: new_group,
                added_at_ms: entry.added_at_ms,
            };
            write_entry(&path, &new_entry)?;
        }
    } else {
        // The URI changed. WRITE FIRST, THEN REMOVE the
        // old entry — the old order deleted the old
        // file before writing the new one, so a write
        // failure destroyed the operator's entry.
        let new_path = path_for_id_and_ensure_dir(paths, &new_id)?;
        if new_path != path && new_path.exists() {
            // The edited URI hashes to an entry that
            // already exists: overwriting it would
            // silently replace a different proxy.
            return Err(InlineProxyError::AlreadyDeclared(new_id));
        }
        let new_entry = InlineProxy {
            uri: new_uri,
            group: new_group,
            added_at_ms: current_unix_ms(),
        };
        write_entry(&new_path, &new_entry)?;
        std::fs::remove_file(&path).map_err(|e| InlineProxyError::Io(e.to_string()))?;
    }
    Ok(InlineProxyOutcome::Applied { id: new_id })
}

/// The comment prefix the edit buffer uses for the
/// (functional) group line. Everything after it is
/// trimmed and stored as the new group value; an
/// empty value clears the group.
pub(super) const GROUP_COMMENT_PREFIX: &str = "# group (optional):";

/// `set proxy import --file <path>` — bulk-add URIs from
/// a file (one URI per line, `#` comments, blank lines
/// skipped). Already-declared URIs are reported but not
/// re-added; the run is atomic per-URI.
///
/// The outcome distinguishes three shapes so the
/// dispatch can report a meaningful envelope:
/// - `Applied { count }` — at least one entry was
///   written in apply mode.
/// - `DryRun { count }` — at least one entry *would* be
///   written in apply mode, but the dry-run short-
///   circuits before any I/O.
/// - `NoOp` — nothing would change (file empty, all
///   entries already declared, or apply mode running
///   against a state with no new entries). This case
///   still counts as a successful idempotent
///   operation; the dispatch's `extra_payload`
///   closure maps it onto `count: 0` in the JSON
///   envelope, and the operator's `--apply` flag
///   decides the `dry_run` field.
pub fn import_proxy(
    paths: &AppPaths,
    file: &Path,
    apply: bool,
) -> Result<ImportOutcome, InlineProxyError> {
    let body = std::fs::read_to_string(file)
        .map_err(|e| InlineProxyError::Io(format!("cannot read {}: {e}", file.display())))?;
    // Pass 1 — pure validation + planning. NOTHING is
    // written while any line might still fail: the old
    // single-pass loop wrote entries as it validated
    // them, so one bad line halfway through the file
    // left a partial import on disk with no rollback
    // (and no report of WHICH line failed). The plan
    // also dedupes repeated URIs inside the file and
    // against the on-disk state, so pass 2 is
    // collision-free by construction.
    let mut plan: Vec<(String, String)> = Vec::new();
    for (index, raw) in body.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        validate_uri(line).map_err(|error| {
            InlineProxyError::InvalidUri(format!("line {}: {error}", index + 1))
        })?;
        let id = id_for_uri(line);
        if path_for_id(paths, &id).exists() || plan.iter().any(|(planned, _)| *planned == id) {
            continue;
        }
        plan.push((id, line.to_owned()));
    }
    // Pass 2 — write the validated plan. A genuine I/O
    // failure here can still leave the entries written
    // so far on disk, but every one of them is a valid,
    // non-duplicate entry (the operator can re-run the
    // import idempotently).
    let mut count = 0_usize;
    for (id, uri) in &plan {
        // Round 19: dry-run uses the read-only path
        // helper. The apply path uses the ensure-dir
        // helper only when it actually needs to
        // create the file. This way a dry-run
        // import against a fresh state root is
        // side-effect-free.
        if !apply {
            count += 1;
            continue;
        }
        let path = path_for_id_and_ensure_dir(paths, id)?;
        let entry = InlineProxy {
            uri: uri.clone(),
            group: None,
            added_at_ms: current_unix_ms(),
        };
        write_entry(&path, &entry)?;
        count += 1;
    }
    if count == 0 {
        // Round 19: the apply path is no-op idempotent
        // (file empty / all dupes), the dry-run path
        // is "nothing to do". The dispatch maps the
        // `apply` flag onto `dry_run` in the envelope;
        // here we just report the count.
        return Ok(ImportOutcome::NoOp);
    }
    Ok(if apply {
        ImportOutcome::Applied { count }
    } else {
        ImportOutcome::DryRun { count }
    })
}

/// `import_proxy` outcome. The two enriched variants
/// carry the count of entries that were (or would
/// have been) written so the dispatch's `extra_payload`
/// closure can project it into the JSON envelope.
///
/// Round 23: the `count` field moved from a tuple
/// wrapper to a struct field (the previous
/// `Applied(usize)` shape forced the dispatch to
/// destructure the enum and reconstruct the payload
/// in three places). The `NoOp` arm stays a unit
/// variant because the dispatch maps it onto
/// `count: 0` in the envelope regardless of `apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Apply mode wrote `count` new entries.
    Applied { count: usize },
    /// Dry-run: `count` entries would be written.
    DryRun { count: usize },
    /// Apply or dry-run: no new entries. Reported
    /// with `dry_run:false` in apply mode (idempotent
    /// success) or `dry_run:true` in dry-run mode.
    NoOp,
}
