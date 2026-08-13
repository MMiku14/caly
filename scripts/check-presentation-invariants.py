#!/usr/bin/env python3
"""Non-compiling checks for thin-client and truthful-UX invariants.

Retargeted after the TUI-era crate (caly-tui) was removed: the client
surface now lives in `crates/caly-cli/src/client/` (RPC) plus the
protocol crate's conversion/budget seams, and the offline tree face
lives in `crates/caly-cli/src/entry_tree.rs` (P8a: presentation layer
moved out of `bins/caly`).

Invariants kept (hard gates, exit 1 on violation):

  - wire decoding is bounded (`DecodeBudget`) and never panics on
    unknown event kinds — unknown kinds surface as
    `DecodedProjectionEvent::Unknown`, so a newer daemon cannot crash an
    older client;
  - the daemon applies decode limits at accept time;
  - daemon snapshot reads are best-effort (`ok()?`), keeping offline
    commands usable without a daemon;
  - the offline entry tree never talks to the daemon (pure config-side
    projection).
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ERRORS: list[str] = []


def source(rel: str) -> str | None:
    """Read a workspace-relative source file; report (not crash on) misses."""
    full = ROOT / rel
    if not full.exists():
        ERRORS.append(f"{rel}: missing source file")
        return None
    return full.read_text(encoding="utf-8")


def compact(value: str) -> str:
    return "".join(value.split())


def require(rel: str, token: str) -> None:
    text = source(rel)
    if text is None:
        return
    if compact(token) not in compact(text):
        ERRORS.append(f"{rel}: missing {token}")


def forbid(rel: str, token: str) -> None:
    text = source(rel)
    if text is None:
        return
    if token in text:
        ERRORS.append(f"{rel}: forbidden {token}")


def main() -> int:
    # Bounded, panic-free wire decode (protocol v2).
    require("crates/caly-protocol/src/conversion/event.rs", "event_from_wire")
    require("crates/caly-protocol/src/conversion/event.rs", "DecodedProjectionEvent::Unknown")
    require("crates/caly-protocol/src/protocol/v2/budget.rs", "DecodeBudget::new(DecodeLimits::v2_default())")
    # Server applies decode limits at accept time.
    require("bins/caly/src/daemon.rs", "DecodeLimits::v2_default()")
    # Client snapshot reads are best-effort (offline commands stay usable).
    require("crates/caly-cli/src/client/mod.rs", "active_core_kind_label")
    require("crates/caly-cli/src/client/mod.rs", "client.snapshot().ok()?")
    # Offline entry tree is pure config-side projection: no daemon RPC.
    require("crates/caly-cli/src/entry_tree.rs", "offline")
    forbid("crates/caly-cli/src/entry_tree.rs", "UdsClient")
    forbid("crates/caly-cli/src/entry_tree.rs", "caly_server")

    for error in ERRORS:
        print(f"error: {error}", file=sys.stderr)
    print(f"presentation invariant scan: {len(ERRORS)} error(s)")
    return 1 if ERRORS else 0


if __name__ == "__main__":
    raise SystemExit(main())
