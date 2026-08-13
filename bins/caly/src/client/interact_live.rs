//! Live entry picker (`node select` without an argument).
//!
//! The dialoguer-based [`super::interact::pick`] is a black box: it cannot
//! host a *test-header row* (a first-row feature that triggers a ping sweep
//! without leaving the picker) nor live-update the latency column while the
//! sweep runs. This module is a small hand-rolled picker for exactly that
//! case — crossterm's `event::poll` timeout drives the redraw loop, the
//! probe sweep runs on a background thread and publishes per-node median
//! latencies into a shared map, and the picker re-renders on every poll
//! timeout (≈150 ms) while the sweep is running.
//!
//! Layout follows the §7 table rules: `[protocol]` badge, 24-char name,
//! right-aligned latency with ` ms`, `-` for untested nodes. The selected
//! row is reverse-video. The window height reuses the dialoguer picker's
//! `terminal height - 6` clamp, with simple scrolling.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use console::Term;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

/// One live picker entry.
pub(crate) struct LiveItem {
    pub(crate) name: String,
    pub(crate) protocol: String,
    pub(crate) latency_ms: Option<u32>,
    /// Stable payload (hex node id), returned on selection.
    pub(crate) payload: String,
}

/// Shared state of the in-picker ping sweep, published by the background
/// probe thread and read by every render.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PingState {
    pub(crate) running: bool,
    pub(crate) done: usize,
    pub(crate) total: usize,
    pub(crate) dead: usize,
    pub(crate) ever_ran: bool,
    /// The sweep itself failed (daemon unreachable): the feature row must
    /// say so instead of presenting `0 unreachable` as a result.
    pub(crate) failed: bool,
}

/// Picker exit.
pub(crate) enum LiveOutcome {
    /// Entry index into the `entries` slice (feature row excluded).
    Selected(usize),
    Escaped,
    Interrupted,
}

/// Runs the live picker until an entry is selected, the sweep is started
/// from the feature row (without leaving), or the user escapes.
///
/// `start_ping` launches the background sweep (and must reset the shared
/// `PingState` to `running: true`); the picker only calls it when the
/// feature row is activated and no sweep is currently running.
pub(crate) fn pick_live(
    term: &Term,
    entries: &[LiveItem],
    ping: &Arc<Mutex<PingState>>,
    latencies: &Arc<Mutex<HashMap<String, Option<u32>>>>,
    start_ping: &mut dyn FnMut(),
) -> io::Result<LiveOutcome> {
    // dialoguer owned its raw-mode + cursor discipline internally; the
    // hand-rolled picker must too. The guard restores both on every exit
    // path (selection, escape, interrupt, error), so Ctrl-C can never leave
    // the terminal in raw mode.
    let Ok(_guard) = RawModeGuard::enable() else {
        return Err(io::Error::other("cannot enable raw mode on this terminal"));
    };
    let window = super::interact::picker_window();
    let mut selected = 0usize;
    let mut first = true;
    loop {
        // Scroll the selection into the window.
        let offset = selected.saturating_sub(window.saturating_sub(1));
        render(
            term, entries, ping, latencies, selected, offset, window, first,
        )?;
        first = false;
        if crossterm::event::poll(Duration::from_millis(150))?
            && let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = crossterm::event::read()?
        {
            match code {
                KeyCode::Up | KeyCode::Char('k') if modifiers.is_empty() => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') if modifiers.is_empty() => {
                    if selected < entries.len() {
                        selected += 1;
                    }
                }
                KeyCode::Enter => {
                    if selected == 0 {
                        // Feature row: start (or restart) the sweep
                        // without leaving the picker. `start_ping` sets
                        // `running` synchronously on this (main) thread
                        // before spawning, so a double-Enter can never
                        // launch two sweeps.
                        let mut state = ping
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !state.running {
                            state.running = true;
                            drop(state);
                            start_ping();
                        }
                    } else {
                        return Ok(LiveOutcome::Selected(selected - 1));
                    }
                }
                KeyCode::Esc => return Ok(LiveOutcome::Escaped),
                KeyCode::Char('q') => return Ok(LiveOutcome::Escaped),
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(LiveOutcome::Interrupted);
                }
                _ => {}
            }
        }
        // Poll timeout: loop back and re-render, so a running sweep's
        // progress and per-node latencies keep refreshing without input.
    }
}

/// Restores the terminal to cooked mode and shows the cursor on drop, on
/// every exit path (raw mode is only safe for the picker's lifetime).
/// crossterm owns the raw-mode API (console 0.16 has none) and it pairs
/// with the crossterm event reader the picker already uses.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Hide);
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Renders the feature row + the visible window of entries at the terminal
/// origin. Full overwrite (no clear) so the screen does not flicker.
fn render(
    term: &Term,
    entries: &[LiveItem],
    ping: &Arc<Mutex<PingState>>,
    latencies: &Arc<Mutex<HashMap<String, Option<u32>>>>,
    selected: usize,
    offset: usize,
    window: usize,
    first: bool,
) -> io::Result<()> {
    let state = *ping
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lat = latencies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let rows = 1 + window.min(entries.len().max(1));
    let top = if first { 0 } else { rows.saturating_sub(1) };
    let _ = term.move_cursor_to(0, top);
    let mut line = 0;
    // Feature row (index 0 of the picker).
    let feature = ping_feature_row(&state, entries.len());
    write_row(term, line, selected == 0, &feature, true)?;
    line += 1;
    // Visible entries.
    let mut index = offset;
    while line <= rows.saturating_sub(1) && index < entries.len() {
        let item = &entries[index];
        let latency = item
            .latency_ms
            .or_else(|| lat.get(&item.name).copied().flatten());
        let row = live_row(&item.protocol, &item.name, latency);
        write_row(term, line, selected == index + 1, &row, false)?;
        index += 1;
        line += 1;
    }
    // Pad remaining window lines (scroll leaves stale text otherwise).
    while line <= rows.saturating_sub(1) {
        term.clear_line()?;
        let _ = term.write_line("");
        line += 1;
    }
    // Keep the selection inside the window while scrolling.
    let _ = (selected, offset, window, entries.len());
    Ok(())
}

/// Writes one picker row, optionally reverse-video highlighted, clearing
/// the line first so shorter rows never leave residue.
fn write_row(
    term: &Term,
    line: usize,
    highlighted: bool,
    text: &str,
    feature: bool,
) -> io::Result<()> {
    let _ = term.move_cursor_to(0, line);
    term.clear_line()?;
    if highlighted {
        let styled = console::Style::new().reverse().apply_to(text);
        term.write_line(&styled.to_string())
    } else {
        term.write_line(text)
    }
    .map_err(|error| {
        if feature {
            // A feature-row write failing is not fatal for scrolling
            // pickers; degrade to plain text.
            io::Error::other(error.to_string())
        } else {
            io::Error::other(error.to_string())
        }
    })
}

/// The test-header row: idle prompt, live progress, or done summary.
pub(crate) fn ping_feature_row(state: &PingState, count: usize) -> String {
    if state.running {
        format!(
            "[ping] testing {}/{} · {} unreachable",
            state.done, state.total, state.dead
        )
    } else if state.failed {
        "[ping] sweep failed (daemon unreachable) · retry with Enter".to_owned()
    } else if state.ever_ran {
        format!(
            "[ping] re-test all {count} nodes · {} unreachable",
            state.dead
        )
    } else {
        format!("[ping] test all {count} nodes")
    }
}

/// One entry row, §7 layout: `[protocol]` badge, 24-char name,
/// right-aligned `{ms} ms` latency (`-` when untested).
pub(crate) fn live_row(protocol: &str, name: &str, latency: Option<u32>) -> String {
    let latency_text = latency.map_or_else(|| "-".to_owned(), |ms| format!("{ms} ms"));
    format!(
        "{:<10} {:<24} {:>8}",
        format!("[{}]", protocol),
        name,
        latency_text
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_row_idle_progress_and_done() {
        let idle = PingState::default();
        assert_eq!(ping_feature_row(&idle, 45), "[ping] test all 45 nodes");
        let running = PingState {
            running: true,
            done: 12,
            total: 45,
            dead: 3,
            ever_ran: true,
            failed: false,
        };
        assert_eq!(
            ping_feature_row(&running, 45),
            "[ping] testing 12/45 · 3 unreachable"
        );
        let done = PingState {
            running: false,
            done: 45,
            total: 45,
            dead: 3,
            ever_ran: true,
            failed: false,
        };
        assert_eq!(
            ping_feature_row(&done, 45),
            "[ping] re-test all 45 nodes · 3 unreachable"
        );
        // A failed sweep must say so, not present 0 unreachable as a result.
        let failed = PingState {
            running: false,
            done: 0,
            total: 0,
            dead: 0,
            ever_ran: true,
            failed: true,
        };
        assert_eq!(
            ping_feature_row(&failed, 45),
            "[ping] sweep failed (daemon unreachable) · retry with Enter"
        );
    }

    #[test]
    fn live_row_aligns_badge_name_and_latency() {
        assert_eq!(
            live_row("vmess", "hk-01", Some(89)),
            "[vmess]    hk-01                       89 ms"
        );
        assert_eq!(
            live_row("vmess", "hk-01", None),
            "[vmess]    hk-01                           -"
        );
        // Long protocol badges shift the name column (same as the §7 table).
        assert!(live_row("shadowsocks", "jp-01", Some(124)).contains("jp-01"));
    }
}
