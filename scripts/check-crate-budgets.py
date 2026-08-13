#!/usr/bin/env python3
"""Crate budget gate (crate-replan.md v4 §8).

`crate-budgets.toml` records, per workspace package, the maximum source LOC
(`src/**/*.rs`, excluding `tests/`) and the maximum number of in-workspace
direct dependents. Exceeding either fails the gate; raising a budget means
editing the file with a stated reason (comments are the audit trail).
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUDGETS = ROOT / "crate-budgets.toml"


def src_lines(directory: Path) -> int:
    return sum(
        len(path.read_text(encoding="utf-8").splitlines())
        for path in sorted(directory.rglob("*.rs"))
    )


def dependents(name: str) -> int:
    pattern = re.compile(rf"^\s*{re.escape(name)}(?:\.workspace)?\s*=", re.M)
    count = 0
    for manifest in sorted(ROOT.glob("crates/*/Cargo.toml")) + sorted(
        ROOT.glob("bins/*/Cargo.toml")
    ):
        if pattern.search(manifest.read_text(encoding="utf-8")):
            count += 1
    return count


def main() -> int:
    if not BUDGETS.exists():
        print("crate budgets failed: crate-budgets.toml missing")
        return 1
    data = tomllib.loads(BUDGETS.read_text(encoding="utf-8"))
    budgets = data.get("budgets", {})
    if not budgets:
        print("crate budgets failed: no [budgets.*] entries")
        return 1
    errors: list[str] = []
    for name, limits in sorted(budgets.items()):
        directory = ROOT / "crates" / name
        if not directory.is_dir():
            directory = ROOT / "bins" / name
        if not directory.is_dir() or not (directory / "src").is_dir():
            errors.append(f"{name}: no crates/ or bins/ directory found")
            continue
        lines = src_lines(directory / "src")
        max_loc = limits.get("max_loc")
        if max_loc is not None and lines > int(max_loc):
            errors.append(f"{name}: LOC {lines} exceeds budget {max_loc}")
        max_dependents = limits.get("max_dependents")
        if max_dependents is not None:
            count = dependents(name)
            if count > int(max_dependents):
                errors.append(
                    f"{name}: {count} direct dependents exceed budget {max_dependents}"
                )
    if errors:
        for error in errors:
            print(f"crate budgets failed: {error}")
        return 1
    print(f"crate budgets passed ({len(budgets)} packages within limits)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
