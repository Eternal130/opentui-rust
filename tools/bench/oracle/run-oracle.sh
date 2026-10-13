#!/usr/bin/env bash
# run-oracle.sh — run the REAL @opentui 0.5.14 frame benchmarks under bun
# (the oracle side of the perf lane's comparison table).
#
# Wires module resolution exactly like the differential harness: bun
# resolves imports from the SCRIPT's directory, so oracle/node_modules
# must point at the pinned sandbox install (tools/diff/sandbox —
# bun.lock tracked). Without it bun auto-installs from the global cache
# and may serve a different @opentui version — the driver ASSERTS
# 0.5.14 and refuses to run otherwise.
#
# Usage: tools/bench/oracle/run-oracle.sh [iterations] [warmup] [out.json]
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
sandbox="$here/../../diff/sandbox"

if [[ ! -d "$sandbox/node_modules/@opentui/core" ]]; then
  echo "run-oracle: sandbox install missing — running bun install" >&2
  (cd "$sandbox" && bun install --frozen-lockfile)
fi

# resolution symlink (gitignored, like tools/diff/js/node_modules)
ln -sfn "$sandbox/node_modules" "$here/node_modules"

exec bun "$here/render-traversal-oracle.ts" "${1:-300}" "${2:-40}" "${3:-$here/../out/oracle-perf.json}"
