//! W2 interactive pickers (cli-v3-design.md §7, Q2/D18).
//!
//! `inquire` drives the menus; this module is the single seam so a
//! rollback to an in-house prompt (D18 step 4) touches one file.
//!
//! Non-interactive behaviour is the caller's job: check
//! [`interactive_capable`] first and render the §7 usage error
//! (`exit 2` + available values) when it returns `false`.

use inquire::{InquireError, Select, Text};
use std::io::IsTerminal;

/// C6 gate: interactive mode requires stdin, stdout AND stderr to be
/// terminals, and TERM to be set and not `dumb`.
///
/// inquire renders its menus on stdout (crossterm), so a redirected
/// stdout is the hard failure (inquire answers `NotTTY` instead of
/// spinning — unlike the dialoguer-era FuzzySelect whose read loop
/// spun at 100% CPU on a non-terminal stderr). stderr stays in the
/// gate conservatively: the §7/C6 contract predates the picker crate
/// and the pickers' error/usage output goes there.
pub(crate) fn interactive_capable() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
        && std::env::var_os("TERM").is_some_and(|term| term != "dumb")
}

/// One picker entry: the stable payload plus the rendered label.
pub(crate) struct PickItem {
    /// The value the caller acts on (a node id, a name, …).
    pub(crate) payload: String,
    /// The one-line label shown in the menu.
    pub(crate) label: String,
}

/// How an interactive pick ended (§7 cancellation contract).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PickOutcome {
    /// An item was chosen; carries its index into the offered items.
    Selected(usize),
    /// Esc — exit 1 with `cancelled, nothing changed`.
    Escaped,
    /// Ctrl-C — exit 130.
    Interrupted,
}

/// Lists above this size get the fuzzy filter (type-to-search) instead
/// of a plain arrow-key menu; below it the plain [`Select`] stays
/// cleaner (no search box). Matches §7's interactive contract: a long
/// node list must not force arrow-key paging on the operator.
const FUZZY_THRESHOLD: usize = 10;

/// Picker-selection policy: long pools switch to the fuzzy filter.
fn uses_fuzzy(item_count: usize) -> bool {
    item_count > FUZZY_THRESHOLD
}

/// Rows the picker should show at once: terminal height minus the prompt
/// and input rows (clamped to a sane band), or a generous fixed window when
/// the terminal size is unavailable (piped/CI).
pub(crate) fn picker_window() -> usize {
    let fixed = 20usize;
    // inquire has no stderr-terminal handle; crossterm's stdout size is
    // the closest equivalent (the C6 gate already required a real TTY).
    crossterm::terminal::size().map_or(fixed, |(_columns, height)| {
        usize::from(height).saturating_sub(6).clamp(5, 40)
    })
}

/// Renders a single-select menu and reports the outcome. Callers
/// must have checked [`interactive_capable`] first; reaching the
/// picker without a TTY would block on keys nobody can press.
///
/// Long lists (above [`FUZZY_THRESHOLD`]) keep inquire's built-in
/// fuzzy filter enabled (type-to-search, the `fuzzy` feature), so
/// the operator can type to filter (e.g. `hk` narrows a 30-node
/// pool to the Hong-Kong entries) — the plain menu is kept for
/// short lists because the filter box would be noise for 3
/// choices.
pub(crate) fn pick(prompt: &str, items: &[PickItem]) -> PickOutcome {
    // Duplicate labels get a `(2)`/`(3)` suffix: inquire's Select
    // resolves the chosen row by the rendered label, so two identical
    // labels — same node name + same latency across overlapping
    // subscriptions is common — would always select the first one and
    // the payload would be the wrong node. The suffix keeps every
    // label unique without touching the payloads.
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let labels: Vec<String> = items
        .iter()
        .map(|item| {
            let count = seen.entry(item.label.as_str()).or_insert(0);
            *count += 1;
            if *count == 1 {
                item.label.clone()
            } else {
                format!("{} ({})", item.label, *count)
            }
        })
        .collect();
    let window = picker_window();
    let mut selection = Select::new(prompt, labels.clone()).with_page_size(window);
    if !uses_fuzzy(labels.len()) {
        selection = selection.without_filtering();
    }
    match selection.prompt() {
        Ok(label) => PickOutcome::Selected(index_of(&labels, &label)),
        Err(InquireError::OperationCanceled) => PickOutcome::Escaped,
        // Ctrl-C surfaces as OperationInterrupted; any other error
        // (e.g. NotTTY despite the C6 gate) folds into the same
        // 130 contract — honest, and the gate makes it unreachable
        // in practice.
        Err(_error) => PickOutcome::Interrupted,
    }
}

/// Maps a picker-returned label back to its index. The label came from
/// `labels` (unique by construction), so the position always exists.
fn index_of(labels: &[String], label: &str) -> usize {
    labels
        .iter()
        .position(|candidate| candidate == label)
        .expect("picker returned a label from the offered list")
}

/// How a free-text prompt ended (W2-β2a, `sub add` interactive
/// naming / C-G). Two states only: inquire's `Text` treats Esc as
/// cancellation, which we fold into the Ctrl-C contract — the §7
/// Esc contract (exit 1) stays with the pickers; empty input means
/// "no value".
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TextOutcome {
    /// The operator submitted the entry; `None` = submitted empty
    /// (the prompt allows empty — the caller decides what that
    /// means, for `sub add` it is "no display name").
    Entered(Option<String>),
    /// Esc or Ctrl-C — exit 130.
    Interrupted,
}

/// Renders a free-text prompt and reports the outcome. Callers
/// must have checked [`interactive_capable`] first.
pub(crate) fn prompt_text(prompt: &str) -> TextOutcome {
    // inquire's Text submits an empty input as `Ok("")` when no default
    // and no validator are set — the allow-empty contract of the old
    // dialoguer `Input::allow_empty(true)`.
    match Text::new(prompt).prompt() {
        Ok(text) => {
            let text = text.trim().to_owned();
            TextOutcome::Entered((!text.is_empty()).then_some(text))
        }
        // Esc (OperationCanceled) and Ctrl-C (OperationInterrupted) both
        // exit 130 here, matching the dialoguer-era contract where
        // `Input` had no Esc-to-cancel and every error was Ctrl-C.
        Err(_error) => TextOutcome::Interrupted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c6_gate_is_false_in_the_test_harness() {
        // libtest captures stdout and there is no stdin TTY, so
        // the gate must report non-interactive — this doubles as
        // the "piped shell" contract check (§7/C6).
        assert!(!interactive_capable());
    }

    #[test]
    fn fuzzy_threshold_keeps_short_lists_on_the_plain_menu() {
        // The picker-selection policy is the only unit-testable
        // surface here (the dialogs themselves need a TTY): short
        // lists stay plain, long lists go fuzzy.
        assert!(!uses_fuzzy(10), "the threshold itself stays menu-only");
        assert!(uses_fuzzy(11), "11+ entries must filter");
        assert!(!uses_fuzzy(0), "an empty pool is not fuzzy");
    }

    #[test]
    fn index_of_resolves_unique_labels_back_to_positions() {
        let labels = vec!["hk".to_owned(), "hk (2)".to_owned(), "jp".to_owned()];
        assert_eq!(index_of(&labels, "hk"), 0);
        assert_eq!(index_of(&labels, "hk (2)"), 1);
        assert_eq!(index_of(&labels, "jp"), 2);
    }
}
