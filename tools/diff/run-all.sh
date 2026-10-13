#!/usr/bin/env bash
# run-all.sh — the differential FULL SUITE with every oracle refreshed.
#
# Stale oracle caches are how the scissor-escape regression hid for a
# day (a wrong-version 0.6.x capture matched the buggy port — see
# tools/diff/README "the resolution incident"). This script deletes the
# trust question entirely: each scenario re-captures its oracle from
# the REAL pinned @opentui 0.5.14 before comparing. It is the FINAL
# gate for a lane; per-scenario cached runs are for iteration only.
#
# CI runs the plain loop (clean checkout, no cache) — equivalent.
#
# Usage (through the dev gate, like every cargo command):
#   tools/dev-gate.sh 'tools/diff/run-all.sh'          # from repo root
#   tools/dev-gate.sh 'tools/diff/run-all.sh -k pattern'  # subset by name
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
scenarios_dir="$here/scenarios"

filter=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -k) filter="$2"; shift 2 ;;
    *) echo "usage: $0 [-k name-substring]" >&2; exit 2 ;;
  esac
done

pass=0
fail=0
failed_scenarios=()

for s in "$scenarios_dir"/*.json; do
  name="$(basename "$s" .json)"
  if [[ -n "$filter" && "$name" != *"$filter"* ]]; then
    continue
  fi
  echo "=== $name ==="
  if cargo run -q --locked -p opentui-diff -- "$s" --refresh-oracle; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failed_scenarios+=("$name")
  fi
done

echo
echo "differential suite: $pass IDENTICAL, $fail MISMATCH/ERROR, $((pass + fail)) total (all oracles refreshed)"
if [[ $fail -gt 0 ]]; then
  printf 'failed: %s\n' "${failed_scenarios[*]}"
  exit 1
fi
