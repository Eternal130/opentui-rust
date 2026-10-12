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
