#!/usr/bin/env bash
# Metadata-driven since P0 (docs/crate-replan.md v4 §8): the authoritative
# checker is the Python implementation next to this wrapper. This file is kept
# so the six-script gate invocation (`scripts/check-dependency-discipline.sh`)
# stays stable.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "${script_dir}/check-dependency-discipline.py" "$@"
