# tools/diff — the differential acceptance instrument

Runs a scenario on both sides and compares cell grids byte-level:

* **oracle** — the REAL `@opentui 0.5.14` (npm tarballs, installed under
  `sandbox/`, `bun.lock` tracked), driven by `js/driver.ts` through
  upstream's `createTestRenderer` with a recording stdout. Capture:
  span lines (text + packed fg/bg u16×4 + attributes), char frame, raw
  SGR byte stream.
* **port** — `opentui-core` renders the same scenario in-process
  (`src/scenario.rs`), same capture shape.
* **compare** (`src/compare.rs`) — spans expand to cells; char, fg, bg,
  attributes compared per cell; markdown report lands in
  `out/reports/<name>.md`. Exit 0 iff identical.
* **vt-replay** (`src/vt.rs`, adapted from opencode-rust's proven
  `tui_diff.rs` vt-grid mirror) — the port's SGR stream parsed through
  a vt100 parser must repaint the port's grid (validates emission).

## Usage

```sh
cd tools/diff/sandbox && bun install        # once
cd .. && cargo run -q -- scenarios/smoke-hello.json
cargo run -q -- scenarios/box-border-text.json --refresh-oracle  # re-run oracle
```

Oracle captures cache under `out/oracle/` (gitignored); reports under
`out/reports/` (gitignored — diff noise; regenerate on demand).

## Scenario vocabulary (grows per lane)

```json
{
  "name": "…", "width": 20, "height": 5,
  "steps": [
    { "op": "box",  "id": "b1", "parent": "root", "options": { "border": true, "width": 10, "height": 3, "borderColor": "#00FF00", "backgroundColor": "#112233" } },
    { "op": "text", "id": "t1", "parent": "b1",   "options": { "content": "hi", "fg": "#FFAA00" } }
  ]
}
```

Both drivers implement the same option subset; unknown options on either
side fail loudly (JS throws, Rust `bail!`s) so the vocabulary stays
honest.

Scroll-lane vocabulary (harness-generic — no scroll-specific driver
branching, only option plumbing):

* `{ "op": "scrollbox", "id": "sb", … }` — mounts a `ScrollBoxRenderable`
  (`width`/`height`, `scrollX`, `scrollY`, `stickyScroll`,
  `stickyStart`, box-ish border options, `scrollbarOptions: { thumbColor,
  trackColor }`, plus `viewportOptions`/`contentOptions`/
  `wrapperOptions` layout subsets). Children added with
  `parent: "sb"` route into the content viewport (upstream
  `ScrollBox.add`).
* `{ "op": "flush" }` — a deterministic settle point (upstream
  `setup.flush()`): render until visually idle. Scripted mutations
  between flushes are observable in the final capture.
* `{ "op": "set", "id": "sb", "prop": "scrollTop" | "scrollLeft",
  "value": n }` — generic scripted property mutation.
* `{ "op": "call", "id": "sb", "method": "scrollBy" | "scrollTo" |
  "scrollChildIntoView", "args": […] }` — generic scripted invocation.
* `{ "op": "resize", "width": w, "height": h }` — `renderer.resize`,
  then the next flush captures the reflow.

Solid-lane vocabulary (`"mode": "solid"` — every step drives BOTH the
Rust `opentui-solid` reconciler AND the REAL `@opentui/solid` 0.5.14
through solid-js `Show`/`For`/`Portal` over the package's own
`createElement`/`spread`/`insertNode`/`setProp`):

* `{ "op": "signal", "id": "s", "value": … }` — create-or-set a signal.
  An array of `{key, value}` objects declares a keyed LIST signal
  (the For shape); a boolean drives Show.
* `{ "op": "el", "tag": "box"|"text"|"scrollbox"|"portal"|"badge_box",
  "id", "parent", "anchor"?, "options": { prop: value | {"$signal": "s"} },
  "children": [...] }` — mount through the reconciler. `{"$signal"}`
  binds a reactive prop (the compiled-JSX getter form). The
  `badge_box` custom element (a BoxRenderable subclass upstream, a
  box-backed factory in the port) is the extend() fixture both drivers
  register.
* `{ "op": "show", "id", "parent", "signal", "children": [...] }` —
  the Show construct (a slot marker anchors the insert, mirroring the
  compiled control-flow shape).
* `{ "op": "for", "id", "parent", "signal", "item": { el template with
  "$item" } }` — the keyed For construct (item identity = the pooled
  `{key, value}` object upstream, the item key in the port).
* `{ "op": "prop", "id", "prop", "value" }` — a static prop patch
  (`setProp`).
* `{ "op": "el-remove", "id" }` — unmount through `_removeNode`
  semantics (disposable scopes tear Portals down).

Sandbox notes for solid mode:

* the driver resolves packages from `tools/diff/node_modules` — a
  symlink to `sandbox/node_modules` (gitignored). Without it bun's
  auto-install silently serves whatever is in the GLOBAL cache — the
  oracle was observed loading `@opentui/solid@0.6.1` while the lock
  pins 0.5.14.
* `js/ensure-solid-client-build.sh` (run by the harness before every
  oracle capture) repoints solid-js's `node` export condition — which
  serves the SSR stub build (createSignal/createEffect are no-ops) —
  to the client bundle `dist/solid.js`. Real apps get this from
  packages/solid's bun plugin; the driver runs unbundled.

