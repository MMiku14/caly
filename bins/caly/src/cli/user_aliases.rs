//! User alias loading (`~/.config/caly/aliases.yaml`, cli-v3-design.md
//! §9.2 / D5②).
//!
//! File shape:
//!
//! ```yaml
//! # User-defined command aliases. Each key is one alias word; the
//! # value is the command line it expands to (string or token list).
//! aliases:
//!   tree: node list --offline --format=tree
//!   fast: [node, ping, --all]
//! ```
//!
//! Load rules:
//!
//! - a missing file is an empty table (never an error);
//! - a malformed file degrades to an empty table with a stderr warning
//!   (a broken aliases.yaml must never block every command);
//! - a key must be a single whitespace-free token and the expansion a
//!   non-empty token list;
//! - the expansion's first token must be a built-in command head
//!   (`BUILTIN_HEADS`), otherwise the entry is skipped with a warning —
//!   user aliases cannot reach non-builtin targets;
//! - a key colliding with a built-in command head (e.g. `node`) is
//!   skipped with a warning — user aliases never shadow real commands;
//! - keys colliding with built-in SHORTCUTS are allowed — the user table
//!   wins (that is the customization value of the file).

use std::path::Path;

use serde_norway::Value;

/// One loaded user alias: `(key tokens, expansion tokens)`. Keys are
/// always single tokens by construction.
pub(crate) type UserRule = (Vec<String>, Vec<String>);

/// Loads the user alias table from `<config>/aliases.yaml`.
pub(crate) fn load_from(path: &Path) -> Vec<UserRule> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let document: Value = match serde_norway::from_str(&body) {
        Ok(document) => document,
        Err(error) => {
            eprintln!(
                "warning: ignoring {} (malformed YAML: {error})",
                path.display()
            );
            return Vec::new();
        }
    };
    let Some(aliases) = document.get("aliases") else {
        return Vec::new();
    };
    let Value::Mapping(entries) = aliases else {
        eprintln!(
            "warning: ignoring {} (`aliases:` must be a mapping)",
            path.display()
        );
        return Vec::new();
    };
    let mut rules: Vec<UserRule> = Vec::new();
    for (key, value) in entries {
        let Some(key) = key.as_str() else {
            eprintln!(
                "warning: {}: alias keys must be strings; entry skipped",
                path.display()
            );
            continue;
        };
        if key.is_empty() || key.split_whitespace().count() != 1 {
            eprintln!(
                "warning: {}: alias `{key}` must be a single word; entry skipped",
                path.display()
            );
            continue;
        }
        if super::aliases::is_builtin_head(key) {
            // User aliases never shadow real commands (D12 set).
            eprintln!(
                "warning: {}: alias `{key}` collides with a built-in command; entry skipped",
                path.display()
            );
            continue;
        }
        let Some(expansion) = expansion_tokens(value) else {
            eprintln!(
                "warning: {}: alias `{key}` needs a string or list of strings; entry skipped",
                path.display()
            );
            continue;
        };
        if expansion.is_empty() {
            eprintln!(
                "warning: {}: alias `{key}` expands to nothing; entry skipped",
                path.display()
            );
            continue;
        }
        if !super::aliases::is_builtin_head(&expansion[0]) {
            eprintln!(
                "warning: {}: alias `{key}` expands to `{}`, which is not a built-in command; entry skipped",
                path.display(),
                expansion.join(" "),
            );
            continue;
        }
        rules.push((vec![key.to_owned()], expansion));
    }
    rules
}

/// Extracts the expansion token list from a YAML value: a string is
/// whitespace-split, a sequence yields its string items.
fn expansion_tokens(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(text) => Some(text.split_whitespace().map(str::to_owned).collect()),
        Value::Sequence(items) => {
            let mut tokens = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(token) = item else {
                    return None;
                };
                if token.is_empty() {
                    return None;
                }
                tokens.push(token.clone());
            }
            Some(tokens)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_file(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("caly-user-alias-{label}-{}", std::process::id()))
    }

    fn write_and_load(label: &str, body: &str) -> Vec<UserRule> {
        let path = unique_file(label);
        std::fs::write(&path, body).expect("write fixture");
        let rules = load_from(&path);
        let _ = std::fs::remove_file(&path);
        rules
    }

    #[test]
    fn missing_file_is_an_empty_table() {
        assert!(load_from(&unique_file("missing")).is_empty());
    }

    #[test]
    fn string_and_list_values_load() {
        let rules = write_and_load(
            "basic",
            "aliases:\n  tree: node list --offline --format=tree\n  fast: [node, ping, --all]\n",
        );
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].0, vec!["tree".to_owned()]);
        assert_eq!(
            rules[0].1,
            vec![
                "node".to_owned(),
                "list".to_owned(),
                "--offline".to_owned(),
                "--format=tree".to_owned()
            ]
        );
        assert_eq!(
            rules[1].1,
            vec!["node".to_owned(), "ping".to_owned(), "--all".to_owned()]
        );
    }

    #[test]
    fn malformed_yaml_degrades_to_empty_with_warning() {
        let rules = write_and_load("bad-yaml", "aliases: [unclosed");
        assert!(rules.is_empty());
    }

    #[test]
    fn non_builtin_targets_are_skipped() {
        let rules = write_and_load(
            "bad-target",
            "aliases:\n  x: frobnicate --all\n  y: node ping\n",
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].0, vec!["y".to_owned()]);
    }

    #[test]
    fn builtin_head_keys_are_skipped() {
        // `node` is a real command head; user aliases never shadow it.
        let rules = write_and_load("shadow", "aliases:\n  node: status\n  ok: status\n");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].0, vec!["ok".to_owned()]);
    }

    #[test]
    fn multiword_and_empty_keys_are_skipped() {
        let rules = write_and_load(
            "keys",
            "aliases:\n  \"n s\": status\n  \"\": status\n  ok: status\n",
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].0, vec!["ok".to_owned()]);
    }

    #[test]
    fn non_string_values_are_skipped() {
        let rules = write_and_load(
            "values",
            "aliases:\n  a: 42\n  b: [node, 7]\n  ok: status\n",
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].0, vec!["ok".to_owned()]);
    }

    #[test]
    fn no_aliases_key_is_an_empty_table() {
        let rules = write_and_load("no-key", "other: true\n");
        assert!(rules.is_empty());
    }
}
