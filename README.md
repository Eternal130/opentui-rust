# opentui-rust

An **API-level Rust port of [OpenTUI](https://github.com/anomalyco/opentui)**,
spec pinned to **`@opentui` 0.5.14** — the exact version opencode v2.0.24
consumes. Upstream is MIT; the vendored spec source carries its license
(`third_party/opentui/LICENSE`). This project is an independent port with
upstream-attributed semantics; see `PORT-MATRIX.md` for the frozen surface.

## Charter

* **Spec = `@opentui` 0.5.14, pinned.** The vendored source
  (`third_party/opentui`, tag `v0.5.14`, commit `31a93fb`) is the
  byte-exact reference. Every ported semantic carries an upstream
  `file:line` citation in code.
* **Consumer = opencode-rust** (the synthesis migration): opencode's TUI
  moves onto these crates via TSX→Rust-builder 1:1 transplants, gated per
  surface by its differential rig.
* **Verification = differential against the REAL opentui.** The
  acceptance instrument (`tools/diff`) runs the published
  `@opentui@0.5.14` npm tarballs under bun and this port on the same
  scenario, then compares cell grids **byte-level** (char, packed fg/bg
  u16×4 with color intent, attributes). Unit tests cite spec lines; the
  harness proves frames.

## Crates

| crate | ports | status |
|---|---|---|
| `opentui-core` | `@opentui/core` — colors (intent metadata), cell grid + draw ops, flexbox layout (taffy), renderable tree, Box/Text renderables, StyledText/TextNode content model (multi-chunk styles, align/tab/wrap none+char+word), headless renderer + SGR emission, the events/input family (parse.keypress legacy+kitty, mouse, bracketed paste, KeyHandler dispatch, keybinding resolution, Input renderable, scheduler), and the editor stack (EditBuffer/EditorView/selection + TextareaRenderable with the 36-action executor, scroll-follow, placeholder, cursor SGR emission) | **differentially IDENTICAL** on 48 scenarios |
| `opentui-keymap` | `@opentui/keymap` — the full 0.5.14 surface: key stringify (exact port), Keymap facade, layered dispatch engine (adapted from opencode-rust's proven `oc-tui-core` port, cited), opentui host, solid provider + hooks, universal/opentui addons + extras (14 additions beyond the frozen 339) — 11 modules | **complete**: all 12 keymap matrix rows done, 64-test behavioral suite |
| `opentui-solid` | `@opentui/solid` — the reactive glue equivalent: `Reactive` boundary + builder trees (`box_!`/`text_!`/`scrollbox_!`), keyed reconciler (universal.js port), Show/For, Portal, hooks, extend(), scrollback writer; 8 differential scenarios vs the real package (mode `"solid"`) | reconciler + builders done |

## Layout

```
crates/            the three port crates (cargo workspace)
tools/bench/       the perf rig (frame budgets; see its README)
tools/diff/        the differential harness (see its README)
  js/driver.ts     bun driver → real @opentui 0.5.14 oracle capture
  scenarios/       JSON scenarios both sides render
  sandbox/         npm install of the pinned tarballs (bun.lock tracked)
third_party/opentui  vendored upstream source @ v0.5.14 (read-only spec)
PORT-MATRIX.md     the frozen API surface (339 symbols, tier 1 = 79)
ARCHITECTURE.md    taffy choice, raster design mapped to ansi.zig, glue design
```

## Differential harness

```sh
# from tools/diff (bun on PATH; sandbox deps installed once via `bun install`)
cargo run -q -- scenarios/smoke-hello.json      # oracle caches under out/
cargo run -q -- scenarios/box-border-text.json --refresh-oracle
```

Bootstrap + render-primitives + scrollbox + events + textarea lane results
(see `tools/diff/out/reports/`):

| scenario | cells | result |
|---|---|---|
| smoke-hello | 100 | **IDENTICAL** (0 diffs) |
| box-border-text | 96 | **IDENTICAL** (0 diffs) |
| flex-column-stack | 192 | **IDENTICAL** (0 diffs) |
| styled-multichunk | 200 | **IDENTICAL** (0 diffs) |
| attr-matrix | 360 | **IDENTICAL** (0 diffs) |
| text-align | 240 | **IDENTICAL** (0 diffs) |
| wrap-char-cjk | 240 | **IDENTICAL** (0 diffs) |
| wrap-none-long | 240 | **IDENTICAL** (0 diffs) |
| tab-indicators | 260 | **IDENTICAL** (0 diffs) |
| node-tree-nested | 264 | **IDENTICAL** (0 diffs) |
| word-wrap-edges | 420 | **IDENTICAL** (0 diffs) |
| styled-wrap-crossing | 288 | **IDENTICAL** (0 diffs) |
| text-ops-incremental | 320 | **IDENTICAL** (0 diffs) |
| blend-overlap | 480 | **IDENTICAL** (0 diffs) |
| wrap-mode-matrix | 520 | **IDENTICAL** (0 diffs) |
| node-deep-attrs | 320 | **IDENTICAL** (0 diffs) |
| attr-or-and-clip | 384 | **IDENTICAL** (0 diffs) |
| emoji-clusters | 288 | **IDENTICAL** (0 diffs) |
| scrollbox-static | 160 | **IDENTICAL** (0 diffs) |
| scrollbox-scroll-mid | 160 | **IDENTICAL** (0 diffs) |
| scrollbox-scroll-end | 160 | **IDENTICAL** (0 diffs) |
| scrollbox-scrollbar-geometry | 384 | **IDENTICAL** (0 diffs) |
| scrollbox-clipped-borders | 176 | **IDENTICAL** (0 diffs) |
| scrollbox-scissor-escape | 220 | **IDENTICAL** (0 diffs) |
| scrollbox-nested | 240 | **IDENTICAL** (0 diffs) |
| scrollbox-sticky-bottom | 160 | **IDENTICAL** (0 diffs) |
| scrollbox-long-math | 200 | **IDENTICAL** (0 diffs) |
| scrollbox-resize | 100 | **IDENTICAL** (0 diffs) |
| scrollbox-hscroll | 120 | **IDENTICAL** (0 diffs) |
| input-typed-text | 72 | **IDENTICAL** (0 diffs, cursor matches) |
| input-cursor-mid | 72 | **IDENTICAL** (0 diffs, cursor matches) |
| input-cursor-end | 72 | **IDENTICAL** (0 diffs, cursor matches) |
| input-overflow-scroll | 60 | **IDENTICAL** (0 diffs, cursor matches) |
| input-word-jump | 72 | **IDENTICAL** (0 diffs, cursor matches) |
| input-placeholder | 72 | **IDENTICAL** (0 diffs) |
| input-delete-word | 72 | **IDENTICAL** (0 diffs) |
| textarea-multiline | 192 | **IDENTICAL** (0 diffs) |
| textarea-cursor-scroll | 192 | **IDENTICAL** (0 diffs) |
| textarea-selection-colors | 144 | **IDENTICAL** (0 diffs) |
| textarea-selection-swap | 144 | **IDENTICAL** (0 diffs) |
| textarea-scrolltop | 120 | **IDENTICAL** (0 diffs) |
| textarea-placeholder | 120 | **IDENTICAL** (0 diffs) |
| textarea-word-wrap | 176 | **IDENTICAL** (0 diffs) |
| textarea-ops-incremental | 144 | **IDENTICAL** (0 diffs) |
| textarea-select-keyboard | 144 | **IDENTICAL** (0 diffs) |
| textarea-opaque-bg | 80 | **IDENTICAL** (0 diffs) |
| textarea-motion-matrix | 144 | **IDENTICAL** (0 diffs) |
| textarea-unicode-wide | 144 | **IDENTICAL** (0 diffs) |

Input scenarios also compare the **terminal cursor** (position +
visibility) captured from the oracle's byte stream against the port's
`setCursorPosition` tracking. The parser surface is additionally
characterized against a frozen oracle corpus (6307 key sequences x
legacy/kitty profiles, 162 mouse cases, 372 renderer event feeds) —
see `crates/opentui-core/tests/characterization.rs`.

Each run also vt-replays the port's SGR stream through a vt100 parser to
prove the emitted bytes repaint the same grid.

## Roadmap (next lanes, per API family)

1. **render-primitives** — **done** (StyledText multi-chunk, TextNode
   tree, text alignment, tab indicators, wrapping modes char/none —
   17/17 differential scenarios IDENTICAL vs `@opentui/core@0.5.14`).
2. ~~**scrollbox-lane**~~ — **done**: ScrollBox + ScrollBar + Slider +
   Arrow + scroll accel + scissor-rect clipping (viewport culling via the
   ported `getObjectsInViewport`), 11 differential scenarios IDENTICAL
   (incl. the 3 bootstrap regressions).
3. ~~**textarea-lane**~~ — **done** (EditBuffer/EditorView/selection core,
   TextareaRenderable + the 36-action executor, scrollTop/scroll-follow,
   placeholder, cursor SGR emission — 12 differential scenarios
   IDENTICAL + a 61-test behavior suite porting upstream's own
   edit-buffer/editor-view/keybinding tests with oracle-verified
   matrices).
4. ~~**input-lane**~~ — done: Input renderable + key events
   (KeyEvent/PasteEvent parsing from lib/parse.keypress*.ts).
5. ~~**events-lane**~~ — done: mouse parser, hit grid, focus
   traversal, renderer event loop (requestRender scheduling
   semantics).
6. ~~**keymap-lane**~~ — **done**: full `@opentui/keymap` 0.5.14 surface
   (commit `511fe8a`) — key stringify, Keymap facade, layered dispatch,
   opentui host, solid provider, addons; all 12 keymap matrix rows +
   14 O2c additions beyond the frozen 339, with a 64-test behavioral
   suite porting upstream's own addon/extras tests.
7. **solid-glue-lane** — ✅ keyed reconciler diffing (props update,
   child reorder, unmount), Portal, scrollback writer; builder API
   (`box_!`/`text_!`/`scrollbox_!`), hooks, extend(); 8 differential
   scenarios vs the real `@opentui/solid` 0.5.14 (mode `"solid"`).
8. ~~**markdown/code-lane**~~ — done: MarkdownRenderable,
   CodeRenderable + tree-sitter styled text (see ARCHITECTURE.md).
9. ~~**perf-lane**~~ — done: the frame-budget rig (`tools/bench`)
   benches buffer draw paths, the taffy measure target, the full
   render cycle at 80x24/120x30/200x50 on upstream-shaped trees +
   the differential scenario fixtures, and isolated SGR emission;
   the allocation audit removed three per-frame clone hot spots
   (word-wrap frames -73..-86%) with `run-all.sh` 74/74 IDENTICAL;
   a CI `perf` job guards generous ceilings. Full render cycle at
   120x30: **0.029 ms** median (~170x under the 5ms target, parity
   or faster vs the REAL 0.5.14 under bun on the same box).
10. **clipboard/ttfd + diff/line-number lanes** — done (landed in
   history): createClipboard, createRendererClipboardAdapter,
   TimeToFirstDraw; DiffRenderable + LineNumberRenderable (jsdiff
   parsePatch port), 21 differential scenarios, 95/95 IDENTICAL.
11. **embedded-terminal-lane** — the tier-1
   `EmbeddedTerminalRenderable`: the TS class ported 1:1 over
   `opentui-core::embedded_terminal`, a VT engine on the `vte` state
   machine with ghostty's compositor + input encoders
   (key/mouse/paste/focus) ported from the MIT source; 18 `et-*`
   differential scenarios (geometry/styling/OSC/wrap/cursor/scroll/
   scrollback/reflow/alt-screen/transparent/focus-cursor/selection)
   + 31 ported upstream tests, all IDENTICAL — marked **partial** in
   PORT-MATRIX (a vte-based engine, not full ghostty-vt emulation).

## License attribution

OpenTUI upstream (vendored under `third_party/opentui`) is © its authors,
**MIT licensed**. This port cites that source per semantic and reproduces
the upstream LICENSE file in place. The port code itself is MIT.
