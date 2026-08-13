#!/usr/bin/env python3
"""Non-compiling checks for critical Infrastructure transaction ordering."""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ERRORS: list[str] = []


def text(path: str) -> str | None:
    """Read a workspace-relative source file; report (not crash on) misses."""
    full = ROOT / path
    if not full.exists():
        ERRORS.append(f"{path}: missing source file")
        return None
    return full.read_text(encoding="utf-8")


def compact(value: str) -> str:
    return "".join(value.split())


def require_order(path: str, tokens: list[str]) -> None:
    raw = text(path)
    if raw is None:
        return
    source = compact(raw)
    cursor = 0
    for token in tokens:
        token = compact(token)
        position = source.find(token, cursor)
        if position < 0:
            ERRORS.append(f"{path}: required order missing: {' -> '.join(tokens)}")
            return
        cursor = position + len(token)


def require(path: str, token: str) -> None:
    raw = text(path)
    if raw is None:
        return
    if compact(token) not in compact(raw):
        ERRORS.append(f"{path}: missing invariant token {token}")


def forbid_outside_platform(token: str) -> None:
    for path in (ROOT / "crates").rglob("*.rs"):
        if "caly-platform" not in path.parts and token in path.read_text(encoding="utf-8"):
            ERRORS.append(f"{path.relative_to(ROOT)}: platform token outside caly-platform: {token}")


def main() -> int:
    require_order("crates/caly-platform/src/fs/mod.rs", [
        "create_new_owner_only", "write_and_sync_temporary", "replace_file", "sync_parent",
    ])
    require_order("crates/caly-platform/src/recovery/mod.rs", [
        "store.load()", "RecoveryPhase::Restoring", "store.persist", "restore_proxy", "clear_if_owner",
    ])
    # 原 caly-profile cache/mod.rs(世代/驱逐机制)在工作区内零引用,收敛优化
    # 时整体删除;其崩溃安全写序不变量由 platform/fs 与 backends/durable* 检查覆盖。
    require("crates/caly-platform/src/process/mod.rs", "UnixProcessGroup")
    require("crates/caly-platform/src/process/mod.rs", "WindowsJobObject")
    require("crates/caly-template/src/lib.rs", "TimedOut { kill:")
    require("crates/caly-template/src/lib.rs", "reap:")
    require("crates/caly-backends/src/platform/durable.rs", "persist_enabled_record")
    require("crates/caly-backends/src/platform/durable.rs", "clear_record")
    require("crates/caly-backends/src/platform/durable_tun.rs", "persist_enabled_record")
    require("crates/caly-backends/src/platform/durable_tun.rs", "clear_record")
    # P7:composition 子树已抽离为 caly-composition crate,哨兵路径随迁。
    require("crates/caly-composition/src/backends/lifecycle.rs", "publish_owner_only_config")
    require("crates/caly-composition/src/backends/lifecycle_support.rs", "AtomicFileContents::try_from_vec")
    forbid_outside_platform("cfg(target_os")
    for path in (ROOT / "crates").rglob("*.rs"):
        source = path.read_text(encoding="utf-8")
        if "spawn_blocking" in source and "template" in str(path):
            ERRORS.append(f"{path.relative_to(ROOT)}: template hard timeout uses spawn_blocking")
    for error in ERRORS:
        print(f"error: {error}", file=sys.stderr)
    print(f"infrastructure invariant scan: {len(ERRORS)} error(s)")
    return 1 if ERRORS else 0


if __name__ == "__main__":
    raise SystemExit(main())
