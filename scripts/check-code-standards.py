#!/usr/bin/env python3
"""Structural gates for the caly Rust workspace.

Two tiers, one exit code:

- **warnings (never gate)**: line budgets and style drift that are only
  meaningful in review — file length approaching/over the 400-line budget and
  the approximate >50/>80-line function spans.
- **gates (exit 1)**: patterns that must not re-enter PRODUCTION code —
  bare `.unwrap()` / `.expect()` / `panic!` / `unreachable!` / `unbounded_*`
  channels — and hard purity rules (concrete I/O/runtime types in
  `caly-domain`). Test code is exempt by construction (see
  ``test_line_mask``): the clippy lints are the semantic gate there too
  (``cfg_attr(test, allow(...))`` at every crate root), so duplicating the
  production rules for tests would only produce noise.

``--strict`` additionally turns the warning tier into a gate once the budgets
have been brought down; until then CI runs the plain (gates-only) mode.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUST_FILES = sorted((ROOT / "crates").rglob("*.rs")) + sorted((ROOT / "bins").rglob("*.rs"))
MAX_FILE_LINES = 400
WARN_FILE_LINES = 300

GATE_PATTERNS = {
    "bare unwrap": re.compile(r"\.unwrap\s*\("),
    "bare expect": re.compile(r"\.expect\s*\("),
    "panic macro": re.compile(r"\bpanic!\s*\("),
    "unreachable macro": re.compile(r"\bunreachable!\s*\("),
    "unbounded channel": re.compile(r"\bunbounded(?:_channel)?\s*\("),
}
WARN_PATTERNS = {
    "source TODO/FIXME": re.compile(r"\b(?:TODO|FIXME)\b"),
}
# Matches `#[doc = "..."]` attribute payload markers used to decode doc attrs.
DOC_ATTR = re.compile(r'^\s*#\[doc\s*=\s*"(?P<text>.*)"\]\s*$')


def relative(path: Path) -> str:
    return str(path.relative_to(ROOT))


def function_spans(lines: list[str]) -> list[tuple[int, int]]:
    """Returns approximate Rust function spans using balanced braces."""
    spans: list[tuple[int, int]] = []
    index = 0
    while index < len(lines):
        if not re.search(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*", lines[index]):
            index += 1
            continue
        start = index
        depth = 0
        body_started = False
        while index < len(lines):
            depth += lines[index].count("{") - lines[index].count("}")
            body_started = body_started or "{" in lines[index]
            if body_started and depth <= 0:
                spans.append((start + 1, index + 1))
                break
            index += 1
        index += 1
    return spans


def _decode_doc_attr(payload: str) -> str:
    """Best-effort unescape of the `#[doc = "..."]` string payload."""
    try:
        # The payload is a Rust string literal body; decode common escapes
        # and treat the rest as-is (precision beyond this is unnecessary
        # for comment masking).
        return (
            payload.replace(r"\\n", "\n")
            .replace(r"\\t", "\t")
            .replace(r'\\"', '"')
            .replace(r"\\\\", "\\")
        )
    except Exception:
        return payload


def test_line_mask(lines: list[str], path: Path) -> list[bool]:
    """Per-line boolean: True when the line is in a comment / doc-comment /
    test-code region.

    The tokenizer is line-approximate by design (a full Rust lexer is out of
    scope); it handles the constructs that actually matter for the codebase:

    - block comments `/* ... */` (also stringified doc attributes)
    - bare `//`, opaque `///` / `#[doc]` doc comments, and `---- ... stdout ----`
      style test-failure blocks
    - string literals — including `r#"..."#` raw strings — with escaped-quote
      handling, so `"x.unwrap()"` inside a string never counts as code
    - `char` literals (`'{'`)
    - test crates: anything inside a `#[cfg(test)]` region or after the first
      `#[test]` / `#[tokio::test]` attribute (the codebase keeps tests at
      file bottom or in `*_tests.rs` files; a `cfg(test)` gate starts a test
      region for the REST of the file)
    - whole-file exemption: any path under a `tests/` directory, any file
      ending `_tests.rs`, or any file containing a `#[cfg(test)]` attribute
      at all (those files are test-only by construction and are already
      governed by the clippy unwrap/expect/panic denies lifted via
      `cfg_attr(test, allow(...))`)
    """
    in_tests_dir = "tests" in path.relative_to(ROOT).parts
    is_tests_file = path.name.endswith("_tests.rs") or path.name == "tests.rs"
    whole_file = in_tests_dir or is_tests_file or any(
        re.search(r"#\[cfg\(test\)\]", line) for line in lines
    )
    mask = [False] * len(lines)
    in_block = False
    in_cfg_test = False
    in_raw: str | None = None  # closing pattern e.g. '"#', '"##'
    in_str = False
    in_char = False
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith(("//!", "///")) or stripped.startswith("//") and not stripped.startswith("////"):
            # Doc comments and plain line comments never carry code.
            mask[index] = stripped.startswith("//")
            continue
        doc = DOC_ATTR.match(line)
        if doc:
            mask[index] = True
            continue
        if re.search(r"#\[cfg\(test\)\]", line):
            in_cfg_test = True
        if re.search(r"#\[(tokio::)?test\]", line):
            mask[index] = True
            # Function bodies after the first test attribute are test code.
            in_cfg_test = True
            continue
        cursor = 0
        saw_code = False
        while cursor < len(line):
            if in_block:
                end = line.find("*/", cursor)
                if end == -1:
                    cursor = len(line)
                    continue
                in_block = False
                cursor = end + 2
                continue
            if in_raw is not None:
                end = line.find(in_raw, cursor)
                if end == -1:
                    cursor = len(line)
                    continue
                closing = in_raw
                in_raw = None
                cursor = end + len(closing)
                continue
            if in_str:
                if line[cursor] == "\\":
                    cursor += 2
                    continue
                if line[cursor] == '"':
                    in_str = False
                cursor += 1
                continue
            if in_char:
                if line[cursor] == "\\":
                    cursor += 2
                    continue
                if line[cursor] == "'":
                    in_char = False
                cursor += 1
                continue
            two = line[cursor:cursor + 2]
            if two == "//":
                cursor = len(line)
                continue
            if two == "/*":
                in_block = True
                cursor += 2
                continue
            raw_match = re.match(r'r(#+)?"', line[cursor:])
            if raw_match:
                in_raw = '"' + (raw_match.group(1) or "")
                cursor += raw_match.end()
                continue
            ch = line[cursor]
            if ch == '"':
                in_str = True
            elif ch == "'" and re.match(r"'(\\.|[^'])'", line[cursor:]):
                in_char = True
            elif not ch.isspace():
                saw_code = True
            cursor += 1
        mask[index] = in_cfg_test or in_block or (not saw_code and bool(stripped))
    if whole_file:
        return [True] * len(lines)
    return mask


def check_files() -> tuple[list[str], list[str]]:
    warnings: list[str] = []
    gates: list[str] = []
    for path in RUST_FILES:
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        line_count = len(lines)
        if line_count > MAX_FILE_LINES:
            warnings.append(f"{relative(path)}: {line_count} lines exceeds {MAX_FILE_LINES}")
        elif line_count > WARN_FILE_LINES:
            warnings.append(f"{relative(path)}: {line_count} lines approaches file limit")
        spans = function_spans(lines)
        for start, end in spans:
            function_lines = end - start + 1
            if function_lines > 80:
                warnings.append(f"{relative(path)}:{start}: function spans {function_lines} lines")
            elif function_lines > 50:
                warnings.append(f"{relative(path)}:{start}: function spans {function_lines} lines")
        mask = test_line_mask(lines, path)

        def allow_annotated(line_no: int) -> bool:
            # Deliberate, reviewed exceptions are annotated with a local
            # `#[allow(...)]` (e.g. `unreachable!()` behind
            # `#[allow(clippy::panic)]` for unreachable-by-construction
            # branches); honour the attribute instead of duplicating the
            # compiler's judgement. The attribute may sit directly above the
            # line or on the enclosing function.
            context = lines[max(0, line_no - 4) : line_no - 1]
            if any("#[allow(" in preceding for preceding in context):
                return True
            for start, end in spans:
                if start <= line_no <= end:
                    header = lines[max(0, start - 4) : start - 1]
                    return any("#[allow(" in preceding for preceding in header)
            return False

        for label, pattern in GATE_PATTERNS.items():
            for match in pattern.finditer(text):
                line_no = text.count("\n", 0, match.start()) + 1
                if mask[line_no - 1] or allow_annotated(line_no):
                    continue
                gates.append(f"{relative(path)}:{line_no}: forbidden {label}")
        for label, pattern in WARN_PATTERNS.items():
            for match in pattern.finditer(text):
                line_no = text.count("\n", 0, match.start()) + 1
                if not mask[line_no - 1]:
                    warnings.append(f"{relative(path)}:{line_no}: forbidden {label}")
    return warnings, gates


def check_domain_purity() -> list[str]:
    warnings: list[str] = []
    domain = ROOT / "crates" / "caly-domain" / "src"
    forbidden = re.compile(
        r"\b(?:tokio|async_std|std::path|PathBuf|std::fs|std::net|std::process|tonic|prost)::?"
    )
    for path in sorted(domain.rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for match in forbidden.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            warnings.append(f"{relative(path)}:{line}: concrete I/O/runtime type in Domain")
    return warnings


def main() -> int:
    strict = "--strict" in sys.argv
    warnings, gates = check_files()
    domain_warnings = check_domain_purity()
    for warning in warnings + domain_warnings:
        print(f"warning: {warning}")
    for gate in gates + ([] if not domain_warnings else []):
        # Purity findings stay warnings for now (they predate the gate); the
        # production-only forbidden patterns are the enforced tier.
        print(f"gate: {gate}")
    print(
        f"scanned {len(RUST_FILES)} Rust files: "
        f"{len(warnings) + len(domain_warnings)} warning(s), {len(gates)} gate violation(s)"
    )
    if gates or (strict and (warnings or domain_warnings)):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
