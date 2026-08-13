#!/usr/bin/env python3
"""Non-compiling cycle check for the declared default actor wait graph."""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/caly-application/src/supervision/topology.rs"
EDGE = re.compile(
    r"DependencyEdge\s*\{\s*from:\s*Component::(\w+),\s*to:\s*Component::(\w+),?\s*\}"
)


def main() -> int:
    text = SOURCE.read_text(encoding="utf-8")
    default = text.split("pub fn default_topology", 1)[1].split("#[cfg(test)]", 1)[0]
    edges = EDGE.findall(default)
    if not edges:
        print("actor topology failed: no default edges parsed")
        return 1
    nodes = {node for edge in edges for node in edge}
    outgoing = {node: [] for node in nodes}
    indegree = {node: 0 for node in nodes}
    for source, target in edges:
        if source == target:
            print(f"actor topology failed: self edge {source}")
            return 1
        outgoing[source].append(target)
        indegree[target] += 1
    ready = [node for node, count in indegree.items() if count == 0]
    visited = 0
    while ready:
        node = ready.pop()
        visited += 1
        for target in outgoing[node]:
            indegree[target] -= 1
            if indegree[target] == 0:
                ready.append(target)
    if visited != len(nodes):
        print("actor topology failed: synchronous wait cycle detected")
        return 1
    print(f"actor topology passed: {len(nodes)} components, {len(edges)} edges")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
