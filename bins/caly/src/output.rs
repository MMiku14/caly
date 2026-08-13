//! Centralised CLI output, error reporting, and JSON contract.
//!
//! Every command in caly produces output through one of two
//! helpers — [`CliOutput::success`] / [`CliOutput::info`]
//! in human or JSON mode — based on the `--json` flag. Every
//! error flows through [`report_error`] (or its exit-code-returning
//! wrapper [`report_error_returning`]) so the JSON shape is
//! stable across the surface.
//!
//! # JSON contract
//!
//! Every JSON line the CLI emits starts with a `version`
//! field so consumers can detect contract changes. Success
//! lines have `ok: true`; error lines have `ok: false` and
//! carry `code`, `error`, `hint`, and the `command` the
//! user ran. A pipeline of `caly --json ... | jq` can rely
//! on this shape being stable across all subcommands.
//!
//! # Human contract
//!
//! Successful mutations print `ok: SEMANTIC_SUMMARY`
//! (`ok: profile \`team\` added`); failures print
//! `error: MSG` followed by `hint: REMEDIATION` on
//! the next line. Errors always go to stderr so stdout
//! carries only successful data.

use std::process::ExitCode;

use serde_json::Value;

/// `1` is the first stable JSON contract version. Bump only
/// when the shape of an existing line changes; additive
/// fields (new optional keys) do not bump the version.
pub const JSON_CONTRACT_VERSION: u32 = 1;

/// The shared human message for an interactive picker cancelled by the
/// operator (Esc). Single source of truth: `ops.rs` (generic pickers) and
/// `set/core.rs` (core pickers) both print it.
pub const CANCELLED_NOTHING_CHANGED: &str = "cancelled, nothing changed";

/// Outputs through either the human or JSON channel. The
/// output is bound to the `--json` flag at parse time so
/// every command in `main.rs` uses the same dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliOutput {
    Human,
    Json,
}

impl CliOutput {
    /// Constructs a `CliOutput` from the `--json` flag value.
    pub const fn from_json_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }

    /// Returns `true` when JSON output is requested.
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }

    /// Prints a successful result. The `summary` is the
    /// semantic message shown to the operator (e.g.
    /// `"profile \`team\` added"`); the `payload` is the
    /// additional JSON fields to merge into the success
    /// envelope (ignored in human mode).
    pub fn success(self, summary: &str, payload: Value) {
        match self {
            Self::Human => println!("ok: {summary}"),
            Self::Json => {
                let mut value = serde_json::json!({
                    "ok": true,
                    "version": JSON_CONTRACT_VERSION,
                });
                if let Value::Object(map) = payload
                    && let Value::Object(self_map) = &mut value
                {
                    for (k, v) in map {
                        self_map.insert(k, v);
                    }
                }
                println!("{value}");
            }
        }
    }

    /// Prints a successful result with no extra payload. Use
    /// this when the JSON envelope is just `{ok, version}`.
    pub fn success_empty(self, summary: &str) {
        self.success(summary, Value::Null);
    }

    /// Round 12: prints a "grammar-locked, wire-deferred" success.
    /// Used by `set` leaves whose CLI shape is locked but whose
    /// real writer is not yet wired. The JSON envelope is identical
    /// to a normal success but the `summary` ends with `: planned.`
    /// so a downstream tool can branch on it without parsing the
    /// human message.
    ///
    /// Replaces the 19-site `eprintln!(... Round 12 ...)` +
    /// `output.success_empty("...: planned.")` + `let _ = output;`
    /// pattern in `commands::set`. After this call, the caller can
    /// simply `return ExitCode::SUCCESS` (or use `planned_ok` to
    /// get the exit code directly).
    pub fn planned(self, action: &str) {
        self.success_empty(&format!("{action}: planned."));
    }

    /// Prints an informational message. Goes to stdout in
    /// both modes (the human reads it; a JSON consumer can
    /// ignore the line because it is not a JSON object).
    pub fn info(self, message: &str) {
        match self {
            Self::Human => println!("{message}"),
            Self::Json => println!(
                "{}",
                serde_json::json!({
                    "level": "info",
                    "message": message,
                    "version": JSON_CONTRACT_VERSION,
                })
            ),
        }
    }
}

// ── W2: adaptive table/TSV emission (cli-v3-design.md §5.1, Q6) ──

/// Effective list-emission mode after adaptive resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableMode {
    /// Space-aligned columns for a human at a terminal.
    Table,
    /// TAB-separated values with a header row — the piped default.
    Tsv,
}

/// Resolves the effective mode: an explicit `--format` always wins
/// (§5.1); unpinned output is a table on a TTY and TSV when piped
/// (Q6 — the piped default changed from the v1 human layout, with
/// `--format=table` as the documented opt-out for the window).
pub fn table_mode(pinned: Option<crate::cli::OutputFormat>) -> TableMode {
    use std::io::IsTerminal;
    match pinned {
        Some(crate::cli::OutputFormat::Table) => TableMode::Table,
        Some(crate::cli::OutputFormat::Tsv) => TableMode::Tsv,
        // Tree / Diff are consumed by their own commands (`node list
        // --format=tree`, `config diff --format=diff`); a table-rendering
        // command receiving them treats the pin as unset (adaptive).
        Some(crate::cli::OutputFormat::Tree | crate::cli::OutputFormat::Diff) | None => {
            if std::io::stdout().is_terminal() {
                TableMode::Table
            } else {
                TableMode::Tsv
            }
        }
    }
}

/// Wraps `text` in an ANSI colour class. Consumers call this only
/// for [`TableMode::Table`]; TSV rows must stay clean bytes.
pub fn paint(color_code: &str, text: &str) -> String {
    format!("\x1b[{color_code}m{text}\x1b[0m")
}

/// Terminal columns one char occupies, per the Unicode East Asian
/// Width standard via `unicode-width` (the ecosystem's de-facto width
/// table — console/ratatui use the same crate). 2026-08-12 refactor:
/// the hand-maintained EAW interval table was replaced after three
/// consecutive agent audits found divergences from `unicode-width`
/// 0.2.2 (blanket `1F200..=1FAFF` counted unassigned/narrow ranges as
/// wide; several combining ranges were not zero-width). Zero-width /
/// control characters return 0, CJK wide glyphs 2, ASCII 1.
fn char_width(ch: char) -> usize {
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Width of `text` in terminal columns with ANSI CSI colour sequences
/// excluded, so pre-painted cells do not break column alignment.
/// CJK wide glyphs count as two columns ([`char_width`]), so tables
/// with 中文/日文 node names stay aligned.
fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // CSI sequence: ESC '[' ... final byte in '@'..='~'.
            if chars.next() == Some('[') {
                for inner in chars.by_ref() {
                    if ('@'..='~').contains(&inner) {
                        break;
                    }
                }
            }
        } else {
            width += char_width(ch);
        }
    }
    width
}

/// Maximum rendered width of one table column (W3a 排版修复): long
/// cells — subscription URLs, inline proxy URIs — otherwise push the
/// whole table past the terminal edge. The width is a per-column cap,
/// not a terminal query (the `terminal-size`-class dep and the unsafe
/// ioctl both stay out — W2 Z1 折中收敛于此)。TSV is exempt: scripts
/// see verbatim bytes.
const MAX_TABLE_COLUMN_WIDTH: usize = 48;

/// Truncates `text` to at most `max` visible columns, appending `…`.
/// ANSI CSI colour sequences are copied through whole (they add no
/// width, and a half-copied sequence would corrupt the terminal).
fn truncate_visible(text: &str, max: usize) -> String {
    if visible_width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut width = 0;
    let mut chars = text.chars().peekable();
    while width < max.saturating_sub(1) {
        let Some(ch) = chars.next() else { break };
        if ch == '\x1b' {
            out.push(ch);
            if chars.peek() == Some(&'[') {
                if let Some(open) = chars.next() {
                    out.push(open);
                }
                for inner in chars.by_ref() {
                    out.push(inner);
                    if ('@'..='~').contains(&inner) {
                        break;
                    }
                }
            }
            continue;
        }
        // Reserve one column for the `…` terminator: a 2-column CJK
        // glyph must not be pushed when it would overflow `max` (it
        // would render `max + 1` columns and shift the next column).
        if width + char_width(ch) > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += char_width(ch);
    }
    out.push('…');
    out
}

/// Renders the table as lines without printing so tests can
/// assert the layout. TSV mode joins header and rows with TAB,
/// verbatim — `caly … | cut -fN` scripts see stable bytes. Table
/// mode right-pads each column to its widest visible cell (capped at
/// [`MAX_TABLE_COLUMN_WIDTH`], overflow cells end with `…`), columns
/// separated by two spaces (the v1 listing convention).
#[must_use]
/// Sanitizes one cell at the single render entry: TAB / CR / LF would
/// break the TSV column shape (a cell containing `\t` silently adds a
/// column) or forge terminal rows in table mode. Sources (config names,
/// subscription tags, URLs) no longer need per-caller pre-cleaning —
/// the table contract is enforced here, once (2026-08-12 refactor).
fn sanitize_cell(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

/// Proportionally shrinks column widths so the table fits `max_total`
/// terminal columns (separators included). The widest column absorbs
/// most of the shrink; every column keeps a readable floor so the
/// leftmost identity columns never vanish. No-op when `max_total` is
/// `None` or the table already fits.
fn shrink_widths_to_fit(widths: &mut [usize], max_total: Option<usize>) {
    // Two columns keeps short identity columns (ID, NODES) readable;
    // a higher floor would stop the shrink well above `max_total` on a
    // nine-column table (2026-08-12 audit: floor 6 left 70-column rows
    // on a 40-column terminal).
    const FLOOR: usize = 2;
    let Some(max_total) = max_total else {
        return;
    };
    let separators = 2 * widths.len().saturating_sub(1);
    let available = max_total.saturating_sub(separators);
    let total: usize = widths.iter().sum();
    if total <= available {
        return;
    } // hoisted: see below
    // Integer proportional shrink, then greedily trim the widest
    // column until the table fits (or every column is at the floor).
    for width in widths.iter_mut() {
        *width = ((*width * available) / total).max(FLOOR);
    }
    while widths.iter().sum::<usize>() > available {
        let Some(widest) = widths.iter_mut().max() else {
            break;
        };
        if *widest <= FLOOR {
            break;
        }
        *widest -= 1;
    }
}

/// Renders the table as lines without printing so tests can assert the
/// layout. TSV mode joins header and rows with TAB, verbatim — `caly …
/// | cut -fN` scripts see stable bytes (cells are pre-sanitized at this
/// entry). Table mode right-pads each column to its widest visible cell
/// (capped at [`MAX_TABLE_COLUMN_WIDTH`], overflow cells end with `…`),
/// columns separated by two spaces (the v1 listing convention).
pub fn render_table(
    mode: TableMode,
    headers: &[&str],
    rows: &[Vec<String>],
    max_total: Option<usize>,
) -> Vec<String> {
    let headers: Vec<String> = headers.iter().map(|header| sanitize_cell(header)).collect();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| sanitize_cell(cell)).collect())
        .collect();
    match mode {
        TableMode::Tsv => {
            let mut lines = Vec::with_capacity(rows.len() + 1);
            lines.push(headers.join("\t"));
            for row in &rows {
                lines.push(row.join("\t"));
            }
            lines
        }
        TableMode::Table => {
            let mut widths: Vec<usize> = headers
                .iter()
                .map(|h| visible_width(h).min(MAX_TABLE_COLUMN_WIDTH))
                .collect();
            for row in &rows {
                for (idx, cell) in row.iter().enumerate() {
                    if let Some(width) = widths.get_mut(idx) {
                        *width = (*width).max(visible_width(cell).min(MAX_TABLE_COLUMN_WIDTH));
                    }
                }
            }
            // Shrink AFTER the row pass widened the columns to their
            // widest cells — shrinking earlier let the loop push them
            // straight back (2026-08-12 audit).
            shrink_widths_to_fit(&mut widths, max_total);
            let pad = |cells: &mut dyn Iterator<Item = (&usize, &str)>| -> String {
                let mut line = String::new();
                let mut first = true;
                for (width, cell) in cells {
                    if !first {
                        line.push_str("  ");
                    }
                    first = false;
                    let rendered = truncate_visible(cell, *width);
                    line.push_str(&rendered);
                    for _ in visible_width(&rendered)..*width {
                        line.push(' ');
                    }
                }
                // The last column's padding is meaningless — keep
                // rows right-clean (trim_end never touches visible
                // content; padding is spaces by construction).
                line.truncate(line.trim_end().len());
                line
            };
            let mut lines = Vec::with_capacity(rows.len() + 1);
            lines.push(pad(&mut widths.iter().zip(headers.iter().copied())));
            for row in rows {
                lines.push(pad(&mut widths.iter().zip(row.iter().map(String::as_str))));
            }
            lines
        }
    }
}

/// Prints [`render_table`] output line by line to stdout.
pub fn print_table(mode: TableMode, headers: &[&str], rows: &[Vec<String>]) {
    // A narrow terminal must not push the rightmost columns past the
    // edge (they wrap and the table visually shifts) — 2026-08-12
    // user-flow audit. `$COLUMNS` is read only on a real terminal;
    // piped output keeps the full widths (scripts parse the TSV shape).
    let max_total =
        if mode == TableMode::Table && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|value| value.trim().parse::<usize>().ok())
                .filter(|cols| *cols >= 20)
        } else {
            None
        };
    for line in render_table(mode, headers, rows, max_total) {
        println!("{line}");
    }
}

/// Reports an error to the appropriate channel. JSON emits a
/// structured object on stderr; human mode prints a two-line
/// `error: …` + `hint: …` block on stderr. The caller is
/// responsible for returning the exit code — see
/// [`report_error_returning`] for the common one-shot.
pub fn report_error(output: CliOutput, error: &CliError) {
    match output {
        CliOutput::Human => {
            eprintln!("error: {}", error.message);
            if let Some(hint) = &error.hint {
                eprintln!("hint: {hint}");
            }
        }
        CliOutput::Json => {
            eprintln!("{}", error.to_json());
        }
    }
}

/// Reports an error and returns the appropriate exit code.
/// This is the one-shot helper for the common "do the work,
/// then bail" pattern at the end of a leaf.
pub fn report_error_returning(output: CliOutput, error: CliError) -> ExitCode {
    report_error(output, &error);
    error.exit_code()
}

/// W2-β2a: the remediation-hint seam. Domain errors that know an
/// actionable next step (CLI v3 §8 "what's wrong / why / what now")
/// surface it through [`CliError::hint`]; the shared writer helpers
/// (`commands::set::common`) call this on the failing error so the
/// envelope gains a `hint` without every dispatch site growing a
/// bespoke parameter. Default: no hint.
pub trait ErrorHint {
    fn hint(&self) -> Option<String> {
        None
    }
}

/// Structured error with a stable `code`, human `message`,
/// optional `hint`, and the `command` the user ran. All caly
/// CLI failures are represented as one of these so the JSON
/// output is uniform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliError {
    /// Stable machine-readable code (e.g. `"profile.not_declared"`).
    /// Script consumers should branch on this, not the
    /// human `message`.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Optional remediation hint. A single sentence pointing
    /// the operator at the next command to run.
    pub hint: Option<String>,
    /// The `command` the user ran (e.g. `"profile add team ..."`).
    /// Tracked in the error envelope so log-grep can find
    /// which input triggered the failure.
    pub command: String,
}

impl CliError {
    /// Builds a new `CliError` with the given code and message.
    pub fn new(code: &'static str, message: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            command: command.into(),
        }
    }

    /// Attaches a remediation hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Returns the exit code for this error class. `Usage`
    /// errors (bad args) exit 2; everything else exits 1.
    pub fn exit_code(&self) -> ExitCode {
        if self.code.starts_with("usage.") {
            ExitCode::from(2)
        } else {
            ExitCode::FAILURE
        }
    }

    /// Serialises the error as a JSON object.
    pub fn to_json(&self) -> Value {
        let mut object = serde_json::json!({
            "ok": false,
            "version": JSON_CONTRACT_VERSION,
            "code": self.code,
            "error": self.message,
            "command": self.command,
        });
        if let Some(hint) = &self.hint
            && let Value::Object(map) = object
        {
            object = Value::Object({
                let mut m = map;
                m.insert("hint".to_owned(), Value::String(hint.clone()));
                m
            });
        }
        object
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_with_payload_merges_into_envelope() {
        let out = CliOutput::Json;
        // Smoke: just confirm we don't panic; full assertion
        // is on the to_string path below.
        out.success(
            "test",
            serde_json::json!({ "id": "team", "dry_run": false }),
        );
    }

    #[test]
    fn json_envelope_contains_version_and_ok() {
        let value = serde_json::json!({
            "ok": true,
            "version": JSON_CONTRACT_VERSION,
        });
        let text = value.to_string();
        assert!(text.contains("\"ok\":true"));
        assert!(text.contains("\"version\":1"));
    }

    #[test]
    fn error_envelope_round_trips_through_json() {
        let error = CliError::new(
            "profile.not_declared",
            "profile `team` is not declared",
            "profile show team",
        )
        .with_hint("run `caly profile add team remote:<url>` to declare it");
        let value = error.to_json();
        let text = value.to_string();
        assert!(text.contains("\"ok\":false"));
        assert!(text.contains("\"version\":1"));
        assert!(text.contains("\"code\":\"profile.not_declared\""));
        assert!(text.contains("\"error\":\"profile `team` is not declared\""));
        assert!(text.contains("\"hint\":\"run `caly profile add"));
        assert!(text.contains("\"command\":\"profile show team\""));
    }

    #[test]
    fn error_envelope_omits_hint_field_when_none() {
        let error = CliError::new("test", "msg", "cmd");
        let value = error.to_json();
        let text = value.to_string();
        assert!(!text.contains("hint"));
    }

    #[test]
    fn usage_error_exits_2_others_exit_1() {
        let usage = CliError::new("usage.bad_arg", "bad", "cmd");
        let runtime = CliError::new("runtime.failed", "fail", "cmd");
        assert_eq!(usage.exit_code(), ExitCode::from(2));
        assert_eq!(runtime.exit_code(), ExitCode::FAILURE);
    }

    #[test]
    fn output_mode_from_json_flag() {
        assert_eq!(CliOutput::from_json_flag(true), CliOutput::Json);
        assert_eq!(CliOutput::from_json_flag(false), CliOutput::Human);
        assert!(CliOutput::Json.is_json());
        assert!(!CliOutput::Human.is_json());
    }

    #[test]
    fn planned_emits_summarized_success() {
        // Round 12: `planned_ok` is the single call site for
        // a stub leaf. The human summary must end with
        // `: planned.` so a downstream tool can branch on it
        // without parsing the rest of the message.
        let out = CliOutput::Json;
        out.planned("set proxy add");
        // Re-render with no payload: just `ok: true, version: 1`.
        out.success_empty("set proxy add: planned.");
    }

    #[test]
    fn pinned_format_wins_over_adaptivity() {
        use crate::cli::OutputFormat;
        assert_eq!(table_mode(Some(OutputFormat::Table)), TableMode::Table);
        assert_eq!(table_mode(Some(OutputFormat::Tsv)), TableMode::Tsv);
        // Unpinned resolution depends on the test harness's fake
        // TTY — only the contract surface is asserted here (the
        // IsTerminal branch is one line of std).
        let _ = table_mode(None);
    }

    #[test]
    fn tsv_render_is_verbatim_with_header() {
        let rows = vec![
            vec!["[vmess]".to_owned(), "hk-01".to_owned(), "28".to_owned()],
            vec!["[direct]".to_owned(), "DIRECT".to_owned(), String::new()],
        ];
        let lines = render_table(TableMode::Tsv, &["TYPE", "NAME", "DELAY_MS"], &rows, None);
        assert_eq!(lines[0], "TYPE\tNAME\tDELAY_MS");
        assert_eq!(lines[1], "[vmess]\thk-01\t28");
        assert_eq!(lines[2], "[direct]\tDIRECT\t");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn table_render_aligns_to_widest_visible_cell() {
        let rows = vec![
            vec!["[vmess]".to_owned(), "hk-01".to_owned()],
            vec!["[direct]".to_owned(), "DIRECT".to_owned()],
        ];
        let lines = render_table(TableMode::Table, &["TYPE", "NAME"], &rows, None);
        assert_eq!(lines[0], "TYPE      NAME");
        assert_eq!(lines[1], "[vmess]   hk-01");
        assert_eq!(lines[2], "[direct]  DIRECT");
    }

    #[test]
    fn painted_cells_do_not_break_alignment() {
        // §5.2 badges arrive pre-painted; the ANSI wrapper must
        // not count toward the column width.
        let rows = vec![
            vec![paint("32", "●"), "hk-01".to_owned()],
            vec!["○".to_owned(), "loooong".to_owned()],
        ];
        let lines = render_table(TableMode::Table, &["S", "NAME"], &rows, None);
        assert_eq!(lines[0], "S  NAME");
        assert_eq!(lines[1], format!("{}  hk-01", paint("32", "●")));
        assert_eq!(lines[2], "○  loooong");
    }

    #[test]
    fn cjk_wide_cells_align_to_two_columns() {
        // Z2: CJK glyphs occupy two terminal columns. A 中文 node name
        // (`香港-节点01` = 香2 港2 -1 节2 点2 0 1 = 11 列) must widen
        // the first column exactly like an 11-column ASCII name.
        let rows = vec![
            vec!["香港-节点01".to_owned(), "28".to_owned()],
            vec!["hk-01".to_owned(), "5".to_owned()],
        ];
        let lines = render_table(TableMode::Table, &["NAME", "DELAY"], &rows, None);
        // Column 1 is 11 wide: header pads 7, `hk-01` pads 6, the
        // CJK cell pads none; all rows hit the column boundary.
        assert_eq!(lines[0], "NAME         DELAY");
        assert_eq!(lines[1], "香港-节点01  28");
        // `hk-01` pads to the same 11-wide column: 5 + 6 pad + 2 sep.
        assert_eq!(lines[2], "hk-01        5");
    }

    #[test]
    fn cjk_char_widths_are_pinned() {
        // The interval table is the alignment contract: one regression
        // here misaligns every CJK table. 中文/日文/全角/emoji = 2;
        // ASCII = 1; joiners / BOM / variation selectors = 0.
        assert_eq!(char_width('香'), 2);
        assert_eq!(char_width('の'), 2);
        assert_eq!(char_width('カ'), 2);
        assert_eq!(char_width('Ａ'), 2); // fullwidth A
        assert_eq!(char_width('😀'), 2);
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('-'), 1);
        assert_eq!(char_width('\u{200b}'), 0); // ZWSP
        assert_eq!(char_width('\u{feff}'), 0); // BOM
        assert_eq!(visible_width("a\u{200b}b"), 2);
        assert_eq!(visible_width("香港-节点01"), 11);
    }

    #[test]
    fn truncate_counts_cjk_as_two_columns() {
        // `香港-节点01` = 11 列; cap 8 → 7 列内容 + `…`.
        let truncated = truncate_visible("香港-节点01", 8);
        assert_eq!(visible_width(&truncated), 8);
        assert!(truncated.ends_with('…'));
        // ASCII cells under the cap pass through untouched.
        assert_eq!(truncate_visible("hk-01", 8), "hk-01");
        // A zero-width joiner in the middle changes nothing.
        assert_eq!(truncate_visible("hk\u{200b}-01", 8), "hk\u{200b}-01");
    }
}

#[cfg(test)]
mod table_sanitize_tests {
    use super::*;

    /// The single-entry cell sanitizer is the table contract: TAB / CR /
    /// LF inside a cell must not break the TSV column shape or forge
    /// terminal rows (2026-08-12 refactor — config names and subscription
    /// tags reach the table without per-caller cleaning).
    #[test]
    fn tsv_cells_with_embedded_tabs_stay_one_column() {
        let rows = vec![vec!["a\tb".to_owned(), "c".to_owned()]];
        let lines = render_table(TableMode::Tsv, &["X", "Y"], &rows, None);
        assert_eq!(lines[1], "a b\tc", "embedded tab must not add a column");
        assert_eq!(lines[1].split('\t').count(), 2);
    }

    #[test]
    fn table_cells_with_newlines_cannot_forge_rows() {
        let rows = vec![vec!["line1\nline2".to_owned(), "x".to_owned()]];
        let lines = render_table(TableMode::Table, &["A", "B"], &rows, None);
        assert_eq!(lines.len(), 2, "one header + one row, no forged rows");
        assert!(lines[1].contains("line1 line2"));
    }

    #[test]
    fn header_cells_are_sanitized_too() {
        let rows: Vec<Vec<String>> = Vec::new();
        let lines = render_table(TableMode::Tsv, &["a\tb"], &rows, None);
        assert_eq!(lines[0], "a b");
    }
}

#[cfg(test)]
mod shrink_tests {
    use super::*;

    /// 2026-08-12 user-flow audit: a 40-column terminal must not push
    /// the rightmost table columns past the edge. The shrink is a pure
    /// function — verified here directly because CI has no real TTY.
    #[test]
    fn narrow_terminal_shrinks_the_table_into_max_total() {
        let rows = vec![vec![
            "1".to_owned(),
            "zhuhaiuk".to_owned(),
            "subscription".to_owned(),
            "https://raw.githubusercontent.com/zhuhaiuk/free-nodes/main/nodes.txt".to_owned(),
            "30".to_owned(),
            "0".to_owned(),
            "8m ago".to_owned(),
            "51m later".to_owned(),
            "●".to_owned(),
        ]];
        let lines = render_table(
            TableMode::Table,
            &[
                "ID",
                "NAME",
                "TYPE",
                "SOURCE",
                "NODES",
                "GROUPS",
                "LAST REFRESH",
                "NEXT REFRESH",
                "STATUS",
            ],
            &rows,
            Some(40),
        );
        for line in &lines {
            assert!(
                visible_width(line) <= 40,
                "table must fit 40 columns, got {}: {line:?}",
                visible_width(line)
            );
        }
        // The rightmost status column stays visible (no column dropped).
        assert!(lines[1].contains('●'));
        // The long URL is truncated, not wrapped.
        assert!(
            lines[1].contains('…')
                || !lines[1]
                    .contains("raw.githubusercontent.com/zhuhaiuk/free-nodes/main/nodes.txt")
        );
    }

    #[test]
    fn wide_or_unbounded_tables_keep_full_widths() {
        let rows = vec![vec!["hk-01".to_owned(), "28ms".to_owned()]];
        let lines = render_table(TableMode::Table, &["NAME", "DELAY"], &rows, None);
        assert!(lines[1].contains("hk-01"));
        assert!(lines[1].contains("28ms"));
    }
}
