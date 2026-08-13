//! Line-oriented locator for the `yaml_surgery` module.
//!
//! Split out of `client/yaml_surgery.rs` (audit #70 file-length
//! budget): this file classifies raw lines and locates the
//! `<key>:` line and the block-sequence entries beneath it. It is
//! a heuristic by design — callers fall back to the conservative
//! whole-file rewrite when the layout is not understood.

/// What a raw line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Line {
    Blank,
    Comment,
    /// Content line; payload is the indentation width in spaces.
    Content(usize),
}

pub(super) fn classify(line: &str) -> Line {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        Line::Blank
    } else if trimmed.starts_with('#') {
        Line::Comment
    } else {
        Line::Content(line.len() - trimmed.len())
    }
}

fn is_dash_item(trimmed: &str) -> bool {
    trimmed == "-" || trimmed.starts_with("- ")
}

/// Split `code # comment` at the first `#` that sits at quote depth 0
/// and is preceded by whitespace (or line start). Quotes track both
/// `'` and `"` with backslash escapes, so `"a # b"` never splits.
pub(super) fn split_trailing_comment(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_double => escaped = true,
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single
                && !in_double
                && (index == 0 || bytes[index - 1].is_ascii_whitespace()) =>
            {
                return (&text[..index], &text[index..]);
            }
            _ => {}
        }
    }
    (text, "")
}

/// Extract the inline value after `<key>:` on a mapping line.
/// Returns `None` when `trimmed` does not start with the exact
/// `key:` prefix (`sources:` matches; `sources_extra:` does not).
fn match_key_line<'text>(trimmed: &'text str, key: &str) -> Option<&'text str> {
    let rest = trimmed.strip_prefix(key)?.strip_prefix(':')?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim())
}

/// What follows `key:` on its own line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyInline {
    /// Nothing (a block value follows on the next lines, or the value
    /// is YAML `null`, which callers treat as an error at the typed
    /// layer).
    Empty,
    /// `[]` — an explicitly empty flow sequence.
    EmptySeq,
    /// Any other inline value (flow sequence, scalar, anchor): entry
    /// edits do not apply; only a whole-block rewrite is safe.
    Other,
}

/// The block-sequence structure beneath a located `<key>:` line.
#[derive(Debug)]
pub(super) struct KeySite {
    pub(super) inline: KeyInline,
    /// Entry content-line ranges `[start, end)` (blank/comment lines
    /// never extend an entry's range; they belong to the file, not to
    /// the entry).
    pub(super) entries: Vec<(usize, usize)>,
    pub(super) item_indent: Option<usize>,
}

/// Find the line of a top-level `<key>:` (indent 0). Returns
/// `Some((line_index, indent))`.
pub(super) fn find_top_key(lines: &[String], key: &str) -> Option<(usize, usize)> {
    for (index, line) in lines.iter().enumerate() {
        match classify(line) {
            Line::Content(0) => {
                let trimmed = line.trim_start();
                if trimmed == "---" || trimmed == "..." {
                    continue;
                }
                if match_key_line(trimmed, key).is_some() {
                    return Some((index, 0));
                }
            }
            // A deeper-indented content line belongs to a previous
            // top-level key's value: skip it.
            Line::Content(_) | Line::Blank | Line::Comment => {}
        }
    }
    None
}

/// Find `<key>:` among the direct children of a mapping whose key line
/// is `parent_line` (at `parent_indent`). The child indent is
/// established by the first child content line.
pub(super) fn find_child_key(
    lines: &[String],
    parent_line: usize,
    parent_indent: usize,
    key: &str,
) -> Option<(usize, usize)> {
    let mut child_indent: Option<usize> = None;
    for (index, line) in lines.iter().enumerate().skip(parent_line + 1) {
        match classify(line) {
            Line::Content(indent) => {
                if indent <= parent_indent {
                    return None;
                }
                match child_indent {
                    None => child_indent = Some(indent),
                    Some(established) if indent > established => {
                        // Part of a previous sibling's value.
                        continue;
                    }
                    Some(established) if indent < established => return None,
                    _ => {}
                }
                if match_key_line(line.trim_start(), key).is_some() {
                    return Some((index, indent));
                }
            }
            Line::Blank | Line::Comment => {}
        }
    }
    None
}

/// Parse the structure of a located `<key>:` line: inline value shape
/// plus the block-sequence entries beneath it. `Ok(None)` means the
/// block contents are not a plain block sequence (weird nesting,
/// mixed indents): the caller must fall back to a conservative path.
pub(super) fn read_key_site(lines: &[String], key_line: usize, indent: usize) -> Option<KeySite> {
    let mut block_end = lines.len();
    for (index, line) in lines.iter().enumerate().skip(key_line + 1) {
        if let Line::Content(other) = classify(line)
            && other <= indent
        {
            block_end = index;
            break;
        }
    }
    let inline_value = {
        let trimmed = lines[key_line].trim_start();
        let (_, rest) = trimmed.split_once(':')?;
        let (value_text, _) = split_trailing_comment(rest.trim());
        value_text.trim().to_owned()
    };
    let inline = if inline_value.is_empty() {
        KeyInline::Empty
    } else if inline_value == "[]" {
        KeyInline::EmptySeq
    } else {
        KeyInline::Other
    };
    let mut entries: Vec<(usize, usize)> = Vec::new();
    let mut item_indent: Option<usize> = None;
    if inline == KeyInline::Empty {
        let mut index = key_line + 1;
        while index < block_end {
            match classify(&lines[index]) {
                Line::Content(entry_indent) => {
                    if !is_dash_item(lines[index].trim_start()) {
                        return None;
                    }
                    match item_indent {
                        None => item_indent = Some(entry_indent),
                        Some(established) if established != entry_indent => return None,
                        _ => {}
                    }
                    let start = index;
                    let mut last = index;
                    let mut cursor = index + 1;
                    while cursor < block_end {
                        match classify(&lines[cursor]) {
                            Line::Content(depth) if depth > entry_indent => {
                                last = cursor;
                                cursor += 1;
                            }
                            Line::Content(depth)
                                if depth == entry_indent
                                    && is_dash_item(lines[cursor].trim_start()) =>
                            {
                                break;
                            }
                            Line::Content(_) => return None,
                            Line::Blank | Line::Comment => cursor += 1,
                        }
                    }
                    entries.push((start, last + 1));
                    index = cursor;
                }
                Line::Blank | Line::Comment => index += 1,
            }
        }
    }
    Some(KeySite {
        inline,
        entries,
        item_indent,
    })
}
