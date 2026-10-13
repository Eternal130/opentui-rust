#!/usr/bin/env bash
# ensure-solid-client-build.sh — repoint solid-js's "node" export
# condition to the CLIENT build.
#
# solid-js's exports map serves dist/server.js under the "node"
# condition (SSR — createSignal/createEffect are stubs, so the solid
# reconciler's spread effects never re-run). Real @opentui/solid apps
# bundle with the browser condition (packages/solid's bun plugin,
# scripts/solid-plugin.ts). The oracle driver runs unbundled under
# `bun run`, so the installed tree's node condition must point at
# dist/solid.js (the client bundle).
#
# Idempotent; run from the repo root (the diff harness does this
# before every oracle capture).
set -euo pipefail

PJ="$(dirname "$0")/../sandbox/node_modules/solid-js/package.json"
if [[ ! -f "$PJ" ]]; then
  echo "ensure-solid-client-build: $PJ missing (bun install first)" >&2
  exit 1
fi

if grep -q 'opentui-solid-client-patch' "$PJ"; then
  exit 0
fi

bun -e '
const fs = require("node:fs")
const path = process.argv[1]
const pkg = JSON.parse(fs.readFileSync(path, "utf8"))
const dot = pkg.exports?.["."]
if (dot?.node && dot.node.import?.includes("server")) {
  dot.node = { ...dot.browser }
  pkg["// opentui-solid-client-patch"] =
    "node condition repointed to the client build for the oracle driver (see tools/diff/js/ensure-solid-client-build.sh)"
  fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + "\n")
  console.log("ensure-solid-client-build: patched solid-js node condition -> dist/solid.js")
} else {
  pkg["// opentui-solid-client-patch"] = "already client build"
  fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + "\n")
}
' "$PJ"
