#!/usr/bin/env python3
"""Metadata-driven dependency discipline gate (crate-replan.md v4 §4/§8).

Replaces the hand-maintained manifest matrix. Every workspace package declares
its coordinates in Cargo.toml:

    [package.metadata.caly]
    axis  = "role"            # or "capability"
    layer = 2                 # dependency partial order: only high -> low
    # allow-peer = ["caly-x"] # same-layer edges must be declared with a reason

This script runs `cargo metadata --no-deps`, derives the internal dependency
graph, and enforces:

  1. every internal edge strictly descends in layer, unless the target is
     declared in the source package's `allow-peer` list;
  2. the internal graph is acyclic;
  3. every workspace package carries `[package.metadata.caly]` coordinates;
  4. external-crate boundaries declared in crate-budgets.toml
     `[external-policy]` (e.g. reqwest) hold for every manifest.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def cargo_metadata() -> dict:
    env = os.environ.copy()
    env["PATH"] = f"{Path.home() / '.cargo' / 'bin'}:{env.get('PATH', '')}"
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"dependency discipline failed: cargo metadata:\n{result.stderr.strip()}")
        raise SystemExit(1)
    return json.loads(result.stdout)


def manifest_policy() -> dict[str, list[str]]:
    budgets = ROOT / "crate-budgets.toml"
    if not budgets.exists():
        return {}
    data = tomllib.loads(budgets.read_text(encoding="utf-8"))
    return {str(k): list(v) for k, v in data.get("external-policy", {}).items()}


def main() -> int:
    metadata = cargo_metadata()
    members = set(metadata["workspace_members"])
    packages = {pkg["id"]: pkg for pkg in metadata["packages"] if pkg["id"] in members}
    names = {pkg["name"]: pkg for pkg in packages.values()}
    errors: list[str] = []

    coords: dict[str, int] = {}
    peers: dict[str, set[str]] = {}
    for pkg in packages.values():
        caly = pkg.get("metadata", {}).get("caly")
        if not isinstance(caly, dict) or "layer" not in caly or "axis" not in caly:
            errors.append(f"{pkg['name']}: missing [package.metadata.caly] axis/layer")
            continue
        coords[pkg["name"]] = int(caly["layer"])
        peers[pkg["name"]] = set(caly.get("allow-peer", []))

    # Internal edges: path dependencies (source is None), normal/build kinds.
    edges: dict[str, set[str]] = {pkg["name"]: set() for pkg in packages.values()}
    for pkg in packages.values():
        for dep in pkg.get("dependencies", []):
            if dep.get("kind") == "dev":
                continue
            if dep.get("source") is not None:
                continue
            target = dep["name"]
            if target not in names:
                continue
            edges[pkg["name"]].add(target)

    for source, targets in edges.items():
        for target in sorted(targets):
            if source not in coords or target not in coords:
                continue
            if coords[source] > coords[target]:
                continue
            if coords[source] == coords[target] and target in peers.get(source, set()):
                continue
            errors.append(
                f"edge {source}(L{coords[source]}) -> {target}(L{coords[target]}): "
                "must strictly descend in layer (or be declared in allow-peer)"
            )

    # Cycle check (Kahn) over the internal graph.
    indegree = {name: 0 for name in edges}
    for source, targets in edges.items():
        for target in targets:
            indegree[target] += 1
    ready = [name for name, count in indegree.items() if count == 0]
    visited = 0
    while ready:
        node = ready.pop()
        visited += 1
        for target in edges[node] - {node}:
            indegree[target] -= 1
            if indegree[target] == 0:
                ready.append(target)
    if visited != len(edges):
        errors.append("internal dependency graph has a cycle")

    # External-crate boundaries (whole-manifest scan, matching the legacy check).
    policy = manifest_policy()
    for pkg in packages.values():
        manifest_text = Path(pkg["manifest_path"]).read_text(encoding="utf-8")
        for external, allowed in policy.items():
            if pkg["name"] in allowed:
                continue
            if re.search(rf"^\s*{re.escape(external)}\s*=", manifest_text, re.M):
                errors.append(
                    f"forbidden external dependency: {pkg['name']} -> {external} "
                    f"(allowed: {', '.join(allowed)})"
                )

    if errors:
        for error in errors:
            print(f"dependency discipline failed: {error}")
        return 1
    edge_count = sum(len(targets) for targets in edges.values())
    print(
        f"dependency discipline passed (metadata-driven: "
        f"{len(packages)} packages, {edge_count} internal edges)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
