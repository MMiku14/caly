//! Targeted line surgery on `config.yaml` list blocks (audit #15 / #16).
//!
//! The `set <resource>` writers used to round-trip the whole file
//! through `serde_norway::Value`, which:
//!
//! - dropped every user comment and all hand-tuned formatting,
//! - re-serialised unrelated sections, materialising every schema
//!   default explicitly (`subscriptions:` gained `url: null`,
//!   `connect_timeout_ms: 5000`, …), and
//! - rewrote — and backed up — the file even for idempotent no-ops
//!   (#16), because the write pipeline had no before/after compare.
//!
//! [`edit_yaml_list`] instead derives a minimal patch from the typed
//! before/after pair and edits only the line range of the target list:
//!
//! - equal before/after returns [`EditOutcome::Unchanged`]; the caller
//!   performs no backup, no validation, no write;
//! - an append splices the new `- ` entries after the last existing
//!   entry (or auto-creates a missing list / parent mapping);
//! - a removal deletes only the removed entries' own lines (comments
//!   stay);
//! - a replacement (e.g. an `enabled` toggle) splices the re-rendered
//!   entry over its old range, preserving trailing inline comments when
//!   the rendered line count matches;
//! - a reorder / complex diff re-renders the list block only, so the
//!   rest of the file is still untouched;
//! - layouts the line-oriented locator cannot understand (flow-style
//!   values, anchors, nested multi-line flow) fall back to the previous
//!   whole-file re-serialisation, so behaviour stays correct at worst.
//!
//! The locator is a heuristic by design; a post-edit self-check
//! re-parses the produced text and compares the typed list against the
//! requested `after` state, so a mis-located edit can never produce a
//! wrong file — it degrades to the conservative fallback instead.

use serde::{Serialize, de::DeserializeOwned};

/// Failure inside the surgery pipeline itself (parse / serialise).
/// Callers map this into their per-resource error type.
#[derive(Debug)]
pub enum SurgeryError {
    /// The original file text did not parse as a YAML mapping, or a
    /// navigated node had an unexpected shape.
    Parse(String),
    /// Typed element (de)serialisation failed.
    Value(String),
}

impl core::fmt::Display for SurgeryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(reason) | Self::Value(reason) => write!(formatter, "{reason}"),
        }
    }
}

impl std::error::Error for SurgeryError {}

/// What [`edit_yaml_list`] produced.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EditOutcome {
    /// The typed before/after are identical: no write is required.
    Unchanged,
    /// The new file text. Validation, backup and the atomic write are
    /// the caller's job.
    Changed(String),
}

/// A minimal list patch derived from the typed before/after pair.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListEdit<T> {
    /// `after == before`.
    NoChange,
    /// `after = before ++ tail`.
    Append(Vec<T>),
    /// `after = before` with the given indexes removed (ascending).
    Remove(Vec<usize>),
    /// Same length, elements at the given indexes replaced.
    Replace(Vec<(usize, T)>),
    /// Anything else (reorders, mixed add/remove): the whole list
    /// block is re-rendered.
    Rewrite(Vec<T>),
}

fn derive_list_edit<T: Clone + PartialEq>(before: &[T], after: &[T]) -> ListEdit<T> {
    if before == after {
        return ListEdit::NoChange;
    }
    if after.len() > before.len() && after[..before.len()] == before[..] {
        return ListEdit::Append(after[before.len()..].to_vec());
    }
    if after.len() == before.len() {
        let replaces: Vec<(usize, T)> = before
            .iter()
            .zip(after.iter())
            .enumerate()
            .filter(|(_, (old, new))| old != new)
            .map(|(index, (_, new))| (index, new.clone()))
            .collect();
        return ListEdit::Replace(replaces);
    }
    if after.len() < before.len() {
        // Accept only when `after` is a strict subsequence of `before`
        // (a pure removal); a greedy left-to-right scan is exact for
        // that predicate.
        let mut removed = Vec::new();
        let mut after_index = 0;
        for (before_index, old) in before.iter().enumerate() {
            if after_index < after.len() && *old == after[after_index] {
                after_index += 1;
            } else {
                removed.push(before_index);
            }
        }
        if after_index == after.len() {
            return ListEdit::Remove(removed);
        }
    }
    ListEdit::Rewrite(after.to_vec())
}

// The line-oriented locator (classification, key-site scanning) and
// the render/splice planning live in sibling files so this one stays
// within the file-length budget (audit #70).
mod locate;
mod rewrite;

use rewrite::apply_edit;

#[cfg(test)]
use locate::split_trailing_comment;

// ── The public entry point ──────────────────────────────────

/// Edit the YAML list at `key_path` (e.g. `["subscriptions", "sources"]`
/// or `["proxy_groups"]`) inside `original`, applying the caller's
/// typed mutation closure.
///
/// - Reads the current list as `Vec<T>` (missing list ⇒ empty, mirroring
///   the previous full-round-trip behaviour; a present non-sequence
///   value is a parse error).
/// - Runs `mutate(&current) -> Vec<T>`.
/// - Returns [`EditOutcome::Unchanged`] when nothing changed (no
///   backup / validation / write should happen — audit #16).
/// - Otherwise produces the new file text with minimal line edits
///   (audit #15), self-checks it against the requested typed state,
///   and falls back to whole-file re-serialisation when the layout is
///   not understood or the self-check disagrees.
pub fn edit_yaml_list<T, F, E>(
    original: &str,
    key_path: &[&str],
    parse_error: impl Fn(String) -> E,
    mutate: F,
) -> Result<EditOutcome, E>
where
    T: DeserializeOwned + Serialize + Clone + PartialEq,
    F: FnOnce(&[T]) -> Result<Vec<T>, E>,
    E: From<SurgeryError>,
{
    // Audit #100: the empty-path precondition used to be a `debug_assert`
    // — compiled out in release, where `key_path[len - 1]` would underflow
    // and panic. Make it a hard boundary.
    if key_path.is_empty() || key_path.len() > 2 {
        return Err(E::from(SurgeryError::Parse(
            "key path must name a list, optionally under one parent".to_owned(),
        )));
    }
    let list_key = key_path[key_path.len() - 1];
    let value: serde_norway::Value = serde_norway::from_str(original)
        .map_err(|error| parse_error(format!("cannot parse config.yaml: {error}")))?;
    let current: Vec<T> = navigate_typed(&value, key_path).map_err(E::from)?;
    let after = mutate(&current)?;
    let edit = derive_list_edit(&current, &after);
    if matches!(edit, ListEdit::NoChange) {
        return Ok(EditOutcome::Unchanged);
    }
    let surgical = apply_edit(original, key_path, list_key, &edit)?;
    let checked = surgical.and_then(|new_text| {
        // Self-check: the surgical text must re-parse to exactly the
        // typed state the caller asked for. A mismatch means the line
        // locator misread the file; degrade safely.
        let reparsed: serde_norway::Value = serde_norway::from_str(&new_text).ok()?;
        let typed = navigate_typed::<T>(&reparsed, key_path).ok()?;
        (typed == after).then_some(new_text)
    });
    match checked {
        Some(new_text) => Ok(EditOutcome::Changed(new_text)),
        None => legacy_rewrite(value, key_path, &after)
            .map(EditOutcome::Changed)
            .map_err(E::from),
    }
}

// ── typed navigation / conservative fallback ────────────────

/// The conservative fallback: re-serialise the whole document with
/// only the target list swapped. Used when the line locator cannot
/// understand the layout (flow values, anchors, …). Behaviourally
/// matches the pre-#15 writers (comments are lost in this path only).
fn legacy_rewrite<T: Serialize>(
    mut value: serde_norway::Value,
    key_path: &[&str],
    after: &[T],
) -> Result<String, SurgeryError> {
    let rendered = serde_norway::to_value(after)
        .map_err(|error| SurgeryError::Value(format!("serialise list: {error}")))?;
    let mut node = &mut value;
    for key in &key_path[..key_path.len() - 1] {
        let mapping = node
            .as_mapping_mut()
            .ok_or_else(|| SurgeryError::Parse("config root must be a mapping".to_owned()))?;
        let key_value = serde_norway::Value::String((*key).to_owned());
        if !matches!(
            mapping.get(&key_value),
            Some(serde_norway::Value::Mapping(_))
        ) {
            mapping.insert(
                key_value.clone(),
                serde_norway::Value::Mapping(serde_norway::Mapping::default()),
            );
        }
        node = mapping
            .get_mut(&key_value)
            .ok_or_else(|| SurgeryError::Parse("parent mapping missing after insert".to_owned()))?;
    }
    let mapping = node
        .as_mapping_mut()
        .ok_or_else(|| SurgeryError::Parse("config root must be a mapping".to_owned()))?;
    mapping.insert(
        serde_norway::Value::String(key_path[key_path.len() - 1].to_owned()),
        rendered,
    );
    serde_norway::to_string(&value)
        .map_err(|error| SurgeryError::Value(format!("serialise document: {error}")))
}

/// Pull the typed list out of a parsed YAML tree. A missing list (or
/// missing parent mapping) yields an empty `Vec`, mirroring the
/// schema's `#[serde(default)]` contract.
fn navigate_typed<T: DeserializeOwned>(
    value: &serde_norway::Value,
    key_path: &[&str],
) -> Result<Vec<T>, SurgeryError> {
    let mut node = value;
    for key in &key_path[..key_path.len() - 1] {
        let mapping = node
            .as_mapping()
            .ok_or_else(|| SurgeryError::Parse("config root must be a mapping".to_owned()))?;
        node = match mapping.get(serde_norway::Value::String((*key).to_owned())) {
            Some(child @ serde_norway::Value::Mapping(_)) => child,
            Some(_) => return Err(SurgeryError::Parse(format!("`{key}:` must be a mapping"))),
            None => return Ok(Vec::new()),
        };
    }
    let list_key = key_path[key_path.len() - 1];
    let mapping = node
        .as_mapping()
        .ok_or_else(|| SurgeryError::Parse("config root must be a mapping".to_owned()))?;
    match mapping.get(serde_norway::Value::String(list_key.to_owned())) {
        Some(list @ serde_norway::Value::Sequence(_)) => serde_norway::from_value(list.clone())
            .map_err(|error| {
                SurgeryError::Parse(format!(
                    "`{list_key}:` is not a list of the expected shape: {error}"
                ))
            }),
        Some(_) => Err(SurgeryError::Parse(format!("`{list_key}:` must be a list"))),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
