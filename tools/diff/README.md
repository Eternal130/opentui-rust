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

## Event-injection mode (input lane)

`tools/diff/js/char-corpus.ts` + `event-corpus.ts` drive the REAL
parsers/renderer and freeze the oracle outputs under
`crates/opentui-core/tests/data/` (see ARCHITECTURE.md "The events
family"). Regenerate with:

```sh
cd tools/diff/sandbox
bun ../js/char-corpus.ts  ../../crates/opentui-core/tests/data/char-corpus.json
bun ../js/event-corpus.ts ../../crates/opentui-core/tests/data/event-corpus.json
```

## Frozen corpora (markdown/code lane)

Two more generators freeze the REAL package's intermediate outputs so
the port's internals are pinned below the renderable level:

* `js/highlight-corpus.ts` → `crates/opentui-core/tests/data/highlight-corpus.json`
  — 27 cases through the REAL `TreeSitterClient` (`getSimpleHighlights`):
  markdown constructs, ts/js/zig fences, unicode (CJK/emoji), unknown
  langs. Replayed by `crates/opentui-core/tests/highlight_corpus.rs`.
* `js/marked-corpus.ts` → `crates/opentui-core/tests/data/marked-corpus.json`
  — 40 cases through the REAL `marked` 17.0.1 `Lexer.lex()`: token
  trees for every block/inline construct. Replayed by
  `crates/opentui-core/tests/marked_corpus.rs` against the hand-ported
  lexer (its regexes use lookaheads/backreferences the Rust `regex`
  crate lacks — the port re-implements the lexer, the corpus pins it).

```sh
cd tools/diff/sandbox
bun ../js/highlight-corpus.ts ../../crates/opentui-core/tests/data/highlight-corpus.json
bun ../js/marked-corpus.ts    ../../crates/opentui-core/tests/data/marked-corpus.json
```

(bun resolves imports relative to the script's dir, hence the
`js/node_modules → ../sandbox/node_modules` symlink, gitignored.)


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

The input lane adds: `input` (options: value, placeholder, width,
maxLength, textColor, focusedTextColor, backgroundColor,
placeholderColor), `focus` (focus a renderable — id), and `key`
(`sequence`: raw bytes through the REAL stdin → parser → key-handler
pipeline on both sides). Input captures additionally compare the
**terminal cursor** (row/col/visible) parsed from the oracle's SGR
stream vs the port's `setCursorPosition` state.

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

Markdown/code lane vocabulary (both drivers; the JS side `await
import`s `MarkdownRenderable`/`CodeRenderable`/`SyntaxStyle` lazily so
non-md scenarios keep the oracle module graph unchanged):

* `{ "op": "markdown", "id", "parent", "options": { content, fg, bg,
  conceal, concealCode, styles } }` — mounts a `MarkdownRenderable`.
  `styles` is an object of `{ token: { fg, bg, bold, italic,
  underline, dim } }` fed to `SyntaxStyle.fromStyles` on the JS side /
  `register_style` loop in the port (the theme/syntax styling matrix).
* `{ "op": "code", "id", "parent", "options": { content, filetype,
  conceal, fg, bg, styles } }` — mounts a `CodeRenderable`.

Two driver mechanics make the async oracle comparable with the port's
synchronous highlight-at-construction:

* **settle loop** — after mount, the driver awaits every
  `CodeRenderable`'s `highlightingDone` then flush + renderOnce, and
  repeats while any node reports `_highlightsDirty || isHighlighting`
  (up to 6 passes; mount invalidates mid-flight — snapshot mismatch
  re-dirties). The captured frame is therefore the SETTLED state,
  which is what the port builds in one pass.
* **cursor-teardown strip** — the oracle's frame-end teardown parks
  the terminal cursor (`ESC ]12;… ESC \ ESC[0 q ESC[r;cH ESC[?25h`)
  after the cursor style reset; that's the renderer's exit park, not a
  renderable's cursor. The driver strips it before extracting the
  cursor CUP for input-lane cursor compare (the full SGR stream keeps
  it — it's part of the oracle's byte-true output). Relatedly, cursor
  compare is scoped to scenarios with focus/key/input ops
  (`compare_filtered` in `src/compare.rs`).

## Full-suite refresh — the final gate

Cached oracles are for ITERATION. The FINAL gate for a lane (and the
only local run that means "green") refreshes every oracle from the
pinned package:

```sh
tools/dev-gate.sh 'tools/diff/run-all.sh'               # all scenarios, refreshed
tools/dev-gate.sh 'tools/diff/run-all.sh -k scrollbox'  # subset by name
```

CI is equivalent by construction (clean checkout, no cache — each
scenario captures fresh). A lane is done when `run-all.sh` is 100%
IDENTICAL locally AND CI is green.

## The resolution incident (2026-10-13) — and the cache discipline it forced

`js/driver.ts` lives in `js/`, OUTSIDE `sandbox/`. Bun resolves imports
by walking `node_modules` up from the IMPORTING FILE — not the cwd.
Before the solid lane added the `js/node_modules → ../sandbox/node_modules`
symlink, that walk found nothing and bun's auto-install silently served
bare `@opentui/core` from the GLOBAL cache — the LATEST version
(0.6.1), not the lock-pinned 0.5.14 the sandbox had installed.

0.6.x is not 0.5.14: it clips the transparent-border fast path to the
scissor (0.5.14 writes those border cells raw — only the whole-rect
`isRectInScissor` gate applies; vendored source `buffer.zig:2405-2409`,
pinned by the `scrollbox-scissor-escape` differential). Consequences:

* **scroll lane passed false** — its oracle capture (and the "11/11
  IDENTICAL" claim) was a 0.6.x capture; the port's fast path had been
  calibrated (an added `is_point_in_scissor` check + a comment
  rationalizing "published binary clips") against the WRONG version.
* **the fusion's 10:35 full-suite refresh captured 0.6.x oracles for
  everything** — and the suite "passed" against them (73 scenarios
  happen to be cell-identical across 0.5.14/0.6.1; scissor-escape
  matched because BOTH sides clipped). It did not error because the
  driver pre-solid-merge was functionally correct — only its module
  RESOLUTION was wrong. The `const r` shadow that broke the scrollbox
  arm landed later (solid lane, 10:56) and made scroll-scenario
  captures ERROR outright; that is a different, loud failure — the
  0.6.x captures were the silent kind.
* **clean CI exposed it** once the symlink shipped (solid lane):
  resolution went back to the pinned 0.5.14, the oracle started
  escaping, and the port mismatched — the only honest signal in the
  chain.

Structural fixes now in place:

1. **Driver version assertion** — `js/driver.ts` hard-fails unless the
   RESOLVED `@opentui/core` is exactly `0.5.14`. Wrong-version
   resolution can never render an oracle again.
2. **Capture fingerprints** — every capture stores
   `capture: { core, driver, scenario }` (core version + FNV-1a of the
   driver and scenario files). `src/main.rs` recomputes the live
   fingerprint and REFUSES a cached oracle that doesn't match,
   self-healing into a fresh capture (`oracle cache stale —
   refreshing`). Editing the driver, editing a scenario, or replaying
   a pre-incident cache all invalidate automatically.
3. **`run-all.sh`** — the always-refresh full suite (above): the final
   gate never trusts a cache, locally or in CI.

