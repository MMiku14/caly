#!/usr/bin/env python3
"""Non-compiling checks for cross-layer integration boundaries.

Advisory only: findings are reported as warnings and the script always exits 0.
The hard gates are cargo fmt/check/test/clippy.
"""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ERRORS: list[str] = []


def source(path: str) -> str | None:
    """Read a workspace-relative source file; report (not crash on) misses."""
    full = ROOT / path
    if not full.exists():
        ERRORS.append(f"{path}: missing source file")
        return None
    return full.read_text(encoding="utf-8")


def compact(value: str) -> str:
    return "".join(value.split())


def require(path: str, token: str) -> None:
    text = source(path)
    if text is None:
        return
    if compact(token) not in compact(text):
        ERRORS.append(f"{path}: missing {token}")


def forbid(path: str, token: str) -> None:
    text = source(path)
    if text is None:
        return
    if path.endswith("Cargo.toml"):
        # Manifest forbids apply to production deps only: dev-dependencies
        # (test-only) never reach the boundary, so scoping them out avoids
        # false positives like caly-server's test-only caly-platform.
        text = _manifest_section(text, "dependencies")
    if token in text:
        ERRORS.append(f"{path}: forbidden {token}")


def _manifest_section(text: str, section: str) -> str:
    """Body of the first exactly-named `[section]` block in a manifest."""
    body: list[str] = []
    inside = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            inside = stripped == f"[{section}]"
        elif inside:
            body.append(line)
    return "\n".join(body)


def require_order(path: str, tokens: list[str]) -> None:
    require_order_after(path, "", tokens)


def require_order_after(path: str, marker: str, tokens: list[str]) -> None:
    raw = source(path)
    if raw is None:
        return
    text = compact(raw)
    marker = compact(marker)
    cursor = text.find(marker)
    if cursor < 0:
        ERRORS.append(f"{path}: missing order marker {marker}")
        return
    for token in tokens:
        token = compact(token)
        found = text.find(token, cursor)
        if found < 0:
            ERRORS.append(f"{path}: missing order {' -> '.join(tokens)}")
            return
        cursor = found + len(token)


def main() -> int:
    require("crates/caly-server/Cargo.toml", "caly-application.workspace")
    forbid("crates/caly-server/Cargo.toml", "caly-platform.workspace")
    forbid("crates/caly-server/Cargo.toml", "caly-profile.workspace")
    forbid("crates/caly-protocol/Cargo.toml", "caly-application.workspace")
    require("crates/caly-protocol/src/protocol/v2/operation.rs", "pub enum WireCommand")
    require("crates/caly-server/src/json/command.rs", "UnknownCommand")
    require("crates/caly-protocol/src/conversion/event.rs", "DecodedProjectionEvent::Unknown")
    # TUI-era session deltas moved to the projection layer: the projection
    # apply path must use checked sequence arithmetic (no overflow panics).
    require("crates/caly-application/src/projection/projector.rs", "fn apply")
    require("crates/caly-application/src/projection/projector.rs", "checked_add(1)")
    require("crates/caly-application/src/operations/admission.rs", "IdempotencyConflict")
    require("crates/caly-application/src/projection/runtime.rs", "poisoned")
    require("crates/caly-application/src/runtime/actor_loop.rs", "recv_timeout")
    require("crates/caly-application/src/operations/admission.rs", "mark_committed")
    for command in (
        "ApplyConfig", "SwitchCore", "SelectProxy", "SetMode", "SetTun",
        "SetSystemProxy", "RefreshSubscription", "CloseAllConnections",
    ):
        require("crates/caly-application/src/routing.rs", f"Command::{command}")
    require("crates/caly-application/src/service/runtime_service.rs", "self.admission.fail")
    require("crates/caly-application/src/service/runtime_service.rs", "self.command_support.validate")
    require("crates/caly-application/src/service/runtime_service.rs", "CancelledBeforeDispatch")
    require("crates/caly-application/src/service/runtime_service.rs", "cancellation_token")
    require("crates/caly-application/src/service/results.rs", "OperationState::Cancelled")
    require("crates/caly-application/src/routing.rs", "OperationCancellationToken")
    require("crates/caly-application/src/operations/record.rs", "supports_running_cancellation")
    require("crates/caly-application/src/actors/lifecycle_handler.rs", "ActorFailureKind::Unsupported")
    require("crates/caly-application/src/runtime/tokio_executor.rs", "with_runtime_guard")
    require("crates/caly-application/src/runtime/fatal.rs", "fatal_signal.send_replace")
    require("crates/caly-server/src/uds.rs", "serve_owner_only_until")
    require("bins/caly/src/daemon.rs", "wait_for_fatal")
    require("crates/caly-application/src/runtime/fatal.rs", "record_actor_result_error")
    require_order("crates/caly-application/src/operations/admission.rs", [
        "self.store.insert", "self.ingress.try_submit", "self.store.remove_pending",
    ])
    require_order("bins/caly/src/bootstrap.rs", [
        "AcquireInstanceLock", "RestorePendingEffects", "BuildOwnedRuntime", "BindTransport", "Ready",
    ])
    for warning in ERRORS:
        print(f"warning: {warning}")
    print(f"integration invariant scan (advisory): {len(ERRORS)} finding(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
