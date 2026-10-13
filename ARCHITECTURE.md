# ARCHITECTURE

## What this port is

`opentui-core` reproduces the **observable behavior** of `@opentui/core`
0.5.14 — the published npm artifact, whose hot paths are a native Zig
library (`packages/native/src`) driven by a TS facade
(`packages/core/src`). Because both halves are in the vendored spec, the
port translates from the **native source where the semantics live**
(buffer, renderer, ansi) and from the **TS facade where the API shape
lives** (Renderable options, renderables, events). Each Rust item cites
its upstream `file:line`.

The proof instrument is the differential harness: the real package
(imported from the pinned tarball under bun) and this port render the
same scenario; cell grids (char + packed colors + attributes) must be
**byte-identical**.

## Cell grid & raster design (mapped to ansi.zig / buffer.zig)

* **Cell** = `{ char: u32, fg: Rgba, bg: Rgba, attributes: u32 }` —
  upstream's four parallel arrays collapsed into one struct
  (memory layout is a native artifact; semantics identical).
* **`Rgba = [u16; 4]`** — each word: channel byte low, metadata byte
  high; metadata word = palette slot (bits 0-7) + intent (bits 8-9).
  Intent (`rgb | indexed | default`) decides SGR emission:
  `38;2;r;g;b` / `38;5;n` / `39|49` (`emitColor`, renderer.zig:844-885).
  Frame diffing compares colors by **exact integer equality** — so the
  port never normalizes colors, it preserves packed words end-to-end.
* **Char word packing** (grapheme.zig): bits 30-31 flag
  image/grapheme/continuation; bits 28-29 right extent, 26-27 left
  extent; bits 0-25 grapheme-pool id. Wide clusters plant continuation
  cells that **copy the start cell's fg/bg/attributes**
  (buffer.zig:635-637) — span capture emits no text for them.
* **Write paths** (`buffer.rs`): plain `set` (span cleanup + continuation
  planting), `set_text_cell` (text draws with alpha bg route to the
  blending writer), `blend_cells` (the exact `blendCells` integer math:
  bg read-through, char preservation over spaces, overlay-wins link
  ids). `draw_text`'s ASCII fast path requires **explicit opaque
  colors**; `bg: None` reads the destination cell's bg — the semantic
  that makes text-over-filled-box adopt the box color.
* **Frame lifecycle** (renderer): clear `next` with the background color
  (default transparent black) → layout → paint into `next` → SGR-diff
  `current` vs `next` (row-equality fast path, run coalescing with reset
  between attribute changes, CUP + SGR + text per run) → swap. The
  harness observes `current` post-swap — the same thing upstream's
  `captureCharFrame`/`getSpanLines` read.

## Layout: taffy 0.9 replaces yoga

**Choice:** [`taffy`](https://github.com/DioxusLabs/taffy) **0.9.2** —
the Rust flexbox engine with yoga-compatible semantics; the same version
opencode-rust (the consumer) already pins, so the two repos share layout
math when the synthesis migration lands.

**Compatibility configuration (what makes taffy behave like the yoga in
opentui):**

1. **Rounding ON** (taffy default, `use_rounding: true`) — yoga rounds
   layout to the pixel grid (point-scale 1); taffy's rounding reproduces
   this. Cell-grid parity would break without it.
2. **flex-direction defaults to COLUMN** — yoga's own default (Facebook
   deviation from CSS) that opentui's option parser relies on
   (`parseFlexDirection(null) → Column`, yoga.options.ts:188-191). taffy
   defaults to Row; every node we create pins Column.
3. **Conditional flexShrink** — opentui overrides the yoga default:
   explicit numeric width/height ⇒ shrink 0, else web default 1
   (Renderable.ts:728-737). Ported verbatim.
4. **Measure leaves** — text nodes measure
   `(widest wrapped line, line count)` with word wrap at the available
   width, mirroring the native measure target
   (TextBufferRenderable.ts:402-431).

**Known deltas (documented, none observable in the differential suite so
far):**

* `"auto"` align-items maps to taffy `Stretch` (yoga has an `Auto`
  align-items; taffy reserves auto for align-self only).
* taffy computes flexbox per CSS spec; yoga carries small historical
  deviations (e.g. stretch of items with auto margins). Any observable
  delta becomes a differential scenario + an explicit shim.
* Percentages: both engines use [0,1] fractions.
* **Yoga applies NO automatic minimum size to flex items** (no CSS
  min-content floor): items shrink below their content. Taffy implements
  the CSS automatic minimum — the port zeroes `min_size` unless the
  options set one. Without this, the ScrollBox's horizontal scrollbar
  keeps 1 row during the first-layout shrink that upstream collapses to
  0 (pinned by `scrollbox-scrollbar-geometry`).

## Scissor clipping (buffer.zig:322-399)

The scroll viewport clips its children through the buffer's **scissor
stack**: `pushScissorRect` intersects with the parent rect (nested clips
nest), every write path gates on `isPointInScissor` (`set` via
`validateAndIndex`, per-cluster `drawText`, per-cell `fillRect` after the
whole-rect `isRectInScissor` cull, `drawBox` early cull + per-cell
writes). The render walk pushes the rect around a node's CHILDREN only
(the node itself paints unclipped — Renderable.ts:1425-1432); the Box
override insets the rect by its borders (Box.ts:296-315).

**Published-binary drift (oracle wins):** the vendored Zig source's
transparent-border fast path (opaque border over transparent bg writes
char/fg/attributes directly, keeping the destination bg) omits the
scissor check — buffer.zig:2405-2409. The PUBLISHED 0.5.14 native binary
CLIPS those writes: a border-only box whose edge sits outside an
overflow-hidden viewport paints nothing there while keeping the
destination bg inside. Pinned by `scrollbox-scissor-escape`; the port
follows the binary.

## The scroll settle loop

Upstream's ScrollBox state is **event-driven and path-dependent**:
viewport/content `onSizeChange` fire `recalculateBarProps` INLINE during
the update walk (a viewport resize reads the content's still-stale size),
scrollbar setters cascade into the slider and back out via onChange to
`content.translateY = -position`, visibility flips toggle yoga
`display` and request another frame. The settled values depend on that
transient sequence — e.g. the vertical slider's `viewPortSize` ends
clamped against a stale `max` from the frame where the horizontal bar
was still visible (pinned: the oracle ends `vp=3` where a fixpoint-only
port would compute 6). The port reproduces the sequence exactly: an
update walk collects size-change events (with at-fire-time snapshots) in
tree order, applies them as `recalculateBarProps` runs, and re-walks
until a walk produces no effects — the same frames upstream's
`requestRender` scheduling produces, verified frame-equivalent by the
differential oracle (`setup.flush()` = walk until visually idle).

## Renderer & reactive glue

The slice renderer is **headless-deterministic**: no terminal I/O, one
`render()` = one frame (upstream's `loop()` tick with the animation
scheduler factored out — the scheduler is an events-lane port target).
`create_cli_renderer` keeps upstream's width/height resolution semantics
(explicit config → 80×24 fallback in the headless profile).

**opentui-solid (design record, in-crate):** solid-js does not exist in
Rust; the consumer runs its own Signal/Store core. The glue therefore
exposes a `Reactive` trait boundary (consumer adapts its signals;
standalone users get the built-in `Signal`), and components are **Rust
builder trees** — `box_! { flexDirection: "row", paddingLeft: 2,
children: […] }` macros with upstream camelCase prop names (plus
snake_case chains) — the 1:1 target of the TSX→Rust transplants.
`Show`/`For` are reactive-scope constructs (`show(signal, render)` /
`for_list(source, render)` over keyed `ForItem`s). The reconciler
(`reconcile.rs`) ports `@opentui/solid`'s custom renderer op-for-op:
`reconcileArrays` (universal.js:108-163) keyed by explicit element keys
over an immutable work list; per-prop minimal patching
(`spreadExpression`'s equality skip + `setProperty`'s switch — events,
id rename, style-bag sub-diff, scroll setters); unmount runs owner-
disposal cleanups (children first, LIFO). Dynamic bindings subscribe to
signals and enqueue patches; `SolidRenderer::flush` is the batched
frame boundary. Portal (marker + container at the mount target),
hooks (`useRenderer`, `useTerminalDimensions` as a signal,
`useKeyboard`/`usePaste` as event streams), `extend()` custom elements
and `createScrollbackWriter` (4-pass height convergence) complete the
upstream surface. Pinned by 8 differential scenarios against the REAL
`@opentui/solid` 0.5.14 (see `tools/diff`, mode `"solid"`).

## opentui-keymap

Upstream `@opentui/keymap` is a facade over host-provided services. The
dispatch engine semantics (layer precedence, focus scopes, mode gating,
pending sequences, interceptor precedence) were already ported once in
opencode-rust's `oc-tui-core::keymap` and pinned there by replay suites;
`opentui-keymap` adapts that proven logic onto the 0.5.14 API shape,
standalone (no opencode-rust dependency), with the upstream type names
(`Layer`, `Binding`, `Command`, `PendingPart`…). `stringifyKeyStroke`/
`stringifyKeySequence` are exact ports (modifier order, `return`→`enter`
aliasing).

## The differential harness (tools/diff)

* **Oracle:** `@opentui/core@0.5.14` from the npm registry (sandbox
  install, `bun.lock` tracked), driven through upstream's own
  `createTestRenderer` (deterministic headless profile) with a recording
  stdout (`bufferedOutput: "buffer"` routes native frames to the stream)
  — capture = span lines (packed colors) + char frame + raw SGR bytes.
* **Port side:** the same scenario JSON rendered by `opentui-core`
  in-process, same capture shape.
* **Compare:** span expansion to per-cell grids; byte-level compare of
  `text / fg[4] / bg[4] / attributes`; markdown report + JSON oracle
  cache per scenario (refresh with `--refresh-oracle`).
* **Secondary check:** vt-replay — the port's SGR stream parsed by a
  vt100 grid parser must repaint the port's cell grid (validates the
  emission, not just the raster).
* The JS driver and scenario vocabulary are intentionally tiny
  (`text`/`box` steps with the option subset both sides understand);
  each lane grows the vocabulary with its renderables.
