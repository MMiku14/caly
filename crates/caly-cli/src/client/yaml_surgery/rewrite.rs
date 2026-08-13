//! Rendering and splicing for the `yaml_surgery` module.
//!
//! Split out of `client/yaml_surgery.rs` (audit #70 file-length
//! budget): given a derived [`ListEdit`] this file plans the
//! minimal text splices against the located key site, renders new
//! entries, and re-joins the line list with the file's original
//! newline convention. `Ok(None)` always means "layout not
//! understood" — the caller falls back to the conservative
//! whole-file rewrite.

use serde::Serialize;

use super::locate::{
    KeyInline, Line, classify, find_child_key, find_top_key, read_key_site, split_trailing_comment,
};
use super::{ListEdit, SurgeryError};

/// Render one list element as YAML sequence-item lines, indented by
/// `indent` spaces.
fn render_entry<T: Serialize>(element: &T, indent: usize) -> Result<Vec<String>, SurgeryError> {
    let text = serde_norway::to_string(&[element])
        .map_err(|error| SurgeryError::Value(format!("serialise list element: {error}")))?;
    let pad = " ".repeat(indent);
    Ok(text.lines().map(|line| format!("{pad}{line}")).collect())
}

fn key_line_text(indent: usize, key: &str, inline_suffix: &str) -> String {
    format!("{}{}:{}", " ".repeat(indent), key, inline_suffix)
}

/// The trailing comment of a `key:` line, if any (`" # note"`).
fn key_line_comment(lines: &[String], key_line: usize) -> String {
    let rest = lines[key_line]
        .trim_start()
        .split_once(':')
        .map_or("", |(_, rest)| rest);
    let (_, comment) = split_trailing_comment(rest);
    comment.trim().to_owned()
}

/// One text splice on the ORIGINAL line indexing: replace
/// `start..end` with the given replacement lines.
type Splice = (usize, usize, Vec<String>);

/// Apply the derived edit to the raw text. `Ok(None)` means the layout
/// was not understood; the caller falls back to the legacy rewrite.
pub(super) fn apply_edit<T: Serialize>(
    original: &str,
    key_path: &[&str],
    list_key: &str,
    edit: &ListEdit<T>,
) -> Result<Option<String>, SurgeryError> {
    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let lines: Vec<String> = original.lines().map(str::to_owned).collect();

    let key_position: Option<(usize, usize)> = match key_path.len() {
        1 => find_top_key(&lines, list_key),
        _ => match find_top_key(&lines, key_path[0]) {
            Some((parent_line, parent_indent)) => {
                find_child_key(&lines, parent_line, parent_indent, list_key)
            }
            None => None,
        },
    };

    // Splices on the ORIGINAL line indexing, applied descending by
    // start so earlier edits never invalidate later coordinates.
    let mut splices: Vec<Splice> = Vec::new();
    let planned = match (key_position, edit) {
        (_, ListEdit::NoChange) => false, // caller short-circuits
        (Some((key_line, indent)), ListEdit::Append(tail)) => {
            plan_append(&lines, key_line, indent, list_key, tail, &mut splices)?
        }
        (Some((key_line, indent)), ListEdit::Remove(indexes)) => {
            plan_remove(&lines, key_line, indent, list_key, indexes, &mut splices)
        }
        (Some((key_line, indent)), ListEdit::Replace(pairs)) => {
            plan_replace(&lines, key_line, indent, pairs, &mut splices)?
        }
        (Some((key_line, indent)), ListEdit::Rewrite(after)) => {
            plan_rewrite(&lines, key_line, indent, list_key, after, &mut splices)?
        }
        (None, ListEdit::Append(tail)) => {
            let Some((position, block)) = creation_block(&lines, key_path, list_key, tail)? else {
                return Ok(None);
            };
            splices.push((position, position, block));
            true
        }
        (None, _) => false,
    };
    if !planned {
        return Ok(None);
    }
    Ok(finish(lines, splices, newline))
}

/// Plan an append against a located list key. Returns `false` when the
/// existing layout is not a plain block sequence.
fn plan_append<T: Serialize>(
    lines: &[String],
    key_line: usize,
    indent: usize,
    list_key: &str,
    tail: &[T],
    splices: &mut Vec<Splice>,
) -> Result<bool, SurgeryError> {
    let Some(site) = read_key_site(lines, key_line, indent) else {
        return Ok(false);
    };
    let item_indent = site.item_indent.unwrap_or(indent + 2);
    let mut rendered = Vec::new();
    for element in tail {
        rendered.extend(render_entry(element, item_indent)?);
    }
    match site.inline {
        KeyInline::Empty => {
            let position = site.entries.last().map_or(key_line + 1, |&(_, end)| end);
            splices.push((position, position, rendered));
        }
        KeyInline::EmptySeq => {
            let comment = key_line_comment(lines, key_line);
            let suffix = if comment.is_empty() {
                String::new()
            } else {
                format!("  {comment}")
            };
            let mut block = vec![key_line_text(indent, list_key, &suffix)];
            block.extend(rendered);
            splices.push((key_line, key_line + 1, block));
        }
        KeyInline::Other => return Ok(false),
    }
    Ok(true)
}

/// Plan a removal against a located list key. Removing every entry
/// collapses the key to an inline empty sequence (a bare `key:` would
/// parse as `null`, which the typed layer rejects).
fn plan_remove(
    lines: &[String],
    key_line: usize,
    indent: usize,
    list_key: &str,
    indexes: &[usize],
    splices: &mut Vec<Splice>,
) -> bool {
    let Some(site) = read_key_site(lines, key_line, indent) else {
        return false;
    };
    if indexes.iter().any(|&index| index >= site.entries.len()) {
        return false;
    }
    if indexes.len() == site.entries.len() {
        let comment = key_line_comment(lines, key_line);
        let suffix = if comment.is_empty() {
            " []".to_owned()
        } else {
            format!(" []  {comment}")
        };
        splices.push((
            key_line,
            key_line + 1,
            vec![key_line_text(indent, list_key, &suffix)],
        ));
    }
    for &index in indexes.iter().rev() {
        let (start, end) = site.entries[index];
        splices.push((start, end, Vec::new()));
    }
    true
}

/// Plan in-place entry replacements. Trailing inline comments survive
/// line-by-line when the render shape matches the old entry.
fn plan_replace<T: Serialize>(
    lines: &[String],
    key_line: usize,
    indent: usize,
    pairs: &[(usize, T)],
    splices: &mut Vec<Splice>,
) -> Result<bool, SurgeryError> {
    let Some(site) = read_key_site(lines, key_line, indent) else {
        return Ok(false);
    };
    let Some(item_indent) = site.item_indent else {
        return Ok(false);
    };
    for &(index, ref element) in pairs {
        let Some(&(start, end)) = site.entries.get(index) else {
            return Ok(false);
        };
        let mut rendered = render_entry(element, item_indent)?;
        if rendered.len() == end - start {
            for (line_index, rendered_line) in rendered.iter_mut().enumerate() {
                let (_, comment) = split_trailing_comment(&lines[start + line_index]);
                if !comment.is_empty() && split_trailing_comment(rendered_line).1.is_empty() {
                    *rendered_line = format!("{}  {}", rendered_line, comment.trim());
                }
            }
        }
        splices.push((start, end, rendered));
    }
    Ok(true)
}

/// Plan a whole-list re-render (reorders / mixed diffs). Only the key
/// line and the entry ranges are replaced; the rest of the file —
/// including comments between entries — is untouched.
fn plan_rewrite<T: Serialize>(
    lines: &[String],
    key_line: usize,
    indent: usize,
    list_key: &str,
    after: &[T],
    splices: &mut Vec<Splice>,
) -> Result<bool, SurgeryError> {
    let Some(site) = read_key_site(lines, key_line, indent) else {
        return Ok(false);
    };
    if site.inline == KeyInline::Other && !site.entries.is_empty() {
        return Ok(false);
    }
    let item_indent = site.item_indent.unwrap_or(indent + 2);
    let key_new = if after.is_empty() {
        key_line_text(indent, list_key, " []")
    } else {
        key_line_text(indent, list_key, "")
    };
    splices.push((key_line, key_line + 1, vec![key_new]));
    for &(start, end) in site.entries.iter().rev() {
        splices.push((start, end, Vec::new()));
    }
    let mut rendered = Vec::new();
    for element in after {
        rendered.extend(render_entry(element, item_indent)?);
    }
    if !rendered.is_empty() {
        splices.push((key_line + 1, key_line + 1, rendered));
    }
    Ok(true)
}

/// Build the block that creates a missing list (and a missing parent
/// mapping), returning the insertion position and the lines to insert.
/// `Ok(None)` means the creation site could not be determined.
fn creation_block<T: Serialize>(
    lines: &[String],
    key_path: &[&str],
    list_key: &str,
    tail: &[T],
) -> Result<Option<(usize, Vec<String>)>, SurgeryError> {
    if key_path.len() == 1 {
        let mut block = vec![key_line_text(0, list_key, "")];
        for element in tail {
            block.extend(render_entry(element, 2)?);
        }
        return Ok(Some((lines.len(), block)));
    }
    let Some((parent_line, _)) = find_top_key(lines, key_path[0]) else {
        let mut block = vec![
            key_line_text(0, key_path[0], ""),
            key_line_text(2, list_key, ""),
        ];
        for element in tail {
            block.extend(render_entry(element, 4)?);
        }
        return Ok(Some((lines.len(), block)));
    };
    // Insert at the end of the parent's block (after its last content
    // line); the child indent mirrors the first existing child, else
    // defaults to parent + 2.
    let mut child_indent = 2;
    let mut last_content: Option<usize> = None;
    for (index, line) in lines.iter().enumerate().skip(parent_line + 1) {
        match classify(line) {
            Line::Content(0) => break,
            Line::Content(indent) => {
                if last_content.is_none() {
                    child_indent = indent;
                }
                last_content = Some(index);
            }
            Line::Blank | Line::Comment => {}
        }
    }
    let position = last_content.map_or(parent_line + 1, |index| index + 1);
    let mut block = vec![key_line_text(child_indent, list_key, "")];
    for element in tail {
        block.extend(render_entry(element, child_indent + 2)?);
    }
    Ok(Some((position, block)))
}

/// Apply non-overlapping splices (sorted late-first) to the line list
/// and re-join with the file's original newline convention. The
/// result always ends with a trailing newline.
fn finish(
    mut lines: Vec<String>,
    mut splices: Vec<(usize, usize, Vec<String>)>,
    newline: &str,
) -> Option<String> {
    splices.sort_by_key(|(start, ..)| std::cmp::Reverse(*start));
    for (start, end, replacement) in splices {
        if start > end || end > lines.len() {
            return None;
        }
        lines.splice(start..end, replacement);
    }
    let mut text = lines.join(newline);
    text.push_str(newline);
    Some(text)
}
