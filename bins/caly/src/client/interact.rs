//! W2 interactive pickers (cli-v3-design.md §7, Q2/D18).
//!
//! `dialoguer` drives the menus; this module is the single seam so a
//! rollback to an in-house prompt (D18 step 4) touches one file.
//!
//! Non-interactive behaviour is the caller's job: check
//! [`interactive_capable`] first and render the §7 usage error
//! (`exit 2` + available values) when it returns `false`.

use std::io::IsTerminal;

/// C6 gate: interactive mode requires stdin, stdout AND stderr to be
/// terminals, and TERM to be set and not `dumb`.
///
/// stderr is part of the gate because dialoguer renders the menu and
/// reads keys through `Term::stderr()`: with stderr redirected
/// (`caly node select 2>/dev/null`), a >10-entry pool reaches
/// `FuzzySelect`'s read loop, whose console `read_key` returns
/// `Key::Unknown` on a non-terminal — the loop never resolves and the
/// process spins at 100% CPU instead of exiting.
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

/// Renders a single-select menu and reports the outcome. Callers
/// must have checked [`interactive_capable`] first; reaching the
/// picker without a TTY would block on keys nobody can press.
///
/// Long lists (above [`FUZZY_THRESHOLD`]) automatically switch to
/// [`dialoguer::FuzzySelect`], so the operator can type to filter
/// (e.g. `hk` narrows a 30-node pool to the Hong-Kong entries) —
/// the plain menu is kept for short lists because the filter box
/// would be noise for 3 choices.
/// Rows the picker should show at once: terminal height minus the prompt
/// and input rows (clamped to a sane band), or a generous fixed window when
/// the terminal size is unavailable (piped/CI).
pub(crate) fn picker_window() -> usize {
    use dialoguer::console::Term;
    let fixed = 20usize;
    Term::stderr().size_checked().map_or(fixed, |(_, height)| {
        usize::from(height).saturating_sub(6).clamp(5, 40)
    })
}

pub(crate) fn pick(prompt: &str, items: &[PickItem]) -> PickOutcome {
    use dialoguer::theme::ColorfulTheme;
    // Duplicate labels get a `(2)`/`(3)` suffix: FuzzySelect resolves
    // the chosen row back to an index by `position(label)` (fuzzy_select
    // 0.12), so two identical labels — same node name + same latency
    // across overlapping subscriptions is common — would always select
    // the first one and the payload would be the wrong node. The suffix
    // keeps every label unique without touching the payloads.
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
    if uses_fuzzy(labels.len()) {
        match dialoguer::FuzzySelect::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .items(&labels)
            .default(0)
            // 2026-08-13: the picker window was a fixed 10 rows, which made
            // long node pools scroll heavily. Use the terminal height when
            // available (reserving rows for the prompt and input line), falling
            // back to a generous fixed window.
            .max_length(picker_window())
            .interact_opt()
        {
            Ok(Some(index)) => PickOutcome::Selected(index),
            Ok(None) => PickOutcome::Escaped,
            Err(_error) => PickOutcome::Interrupted,
        }
    } else {
        match dialoguer::Select::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .items(&labels)
            .default(0)
            .interact_opt()
        {
            Ok(Some(index)) => PickOutcome::Selected(index),
            Ok(None) => PickOutcome::Escaped,
            // Ctrl-C surfaces as an interrupted read; with the C6 gate
            // in front, any other dialoguer error here is so unlikely
            // that folding it into the same 130 contract is honest.
            Err(_error) => PickOutcome::Interrupted,
        }
    }
}

/// How a free-text prompt ended (W2-β2a, `sub add` interactive
/// naming / C-G). Two states only: dialoguer's `Input` has no
/// Esc-to-cancel (unlike `Select::interact_opt`), so the §7 Esc
/// contract stays with the pickers; here Ctrl-C is the only
/// cancellation and empty-input means "no value".
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TextOutcome {
    /// The operator submitted the entry; `None` = submitted empty
    /// (the prompt allows empty — the caller decides what that
    /// means, for `sub add` it is "no display name").
    Entered(Option<String>),
    /// Ctrl-C — exit 130.
    Interrupted,
}

/// Renders a free-text prompt and reports the outcome. Callers
/// must have checked [`interactive_capable`] first.
pub(crate) fn prompt_text(prompt: &str) -> TextOutcome {
    use dialoguer::theme::ColorfulTheme;
    match dialoguer::Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .allow_empty(true)
        .interact()
    {
        Ok(text) => {
            let text = text.trim().to_owned();
            TextOutcome::Entered((!text.is_empty()).then_some(text))
        }
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
}
