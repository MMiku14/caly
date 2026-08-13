#!/usr/bin/env bash
# Single gate entry for every caly check script (crate-replan.md v4 §8).
#
# Fails (exit 1) on ANY check crash or nonzero exit — a dead gate must
# surface as a red build, not as silent green. Run from the workspace
# root, e.g. `scripts/check-all.sh`; extra args are forwarded to each
# check (e.g. `--strict`).
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root_dir="$(cd "${script_dir}/.." && pwd)"
cd "${root_dir}"

# Order: cheap structural checks first, metadata-driven gate last.
checks=(
	actor-topology
	code-standards
	crate-budgets
	infrastructure-invariants
	integration-invariants
	presentation-invariants
	dependency-discipline
)

failed=0
for name in "${checks[@]}"; do
	script="${script_dir}/check-${name}.py"
	if [ ! -f "${script}" ]; then
		echo "check-all: MISSING ${script}" >&2
		failed=$((failed + 1))
		continue
	fi
	printf '== %s\n' "${name}"
	if ! python3 "${script}" "$@"; then
		echo "check-all: FAILED ${name}" >&2
		failed=$((failed + 1))
	fi
done

if [ "${failed}" -ne 0 ]; then
	echo "check-all: ${failed} of ${#checks[@]} checks failed" >&2
	exit 1
fi
echo "check-all: all ${#checks[@]} checks passed"
