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

## The editor stack (textarea lane, O2e)

`opentui-core::editor` ports the native editor trio — `edit-buffer.zig`,
`editor-view.zig`, and the selection core of `text-buffer-view.zig` —
onto the O2a line model (`Vec<LogicalLine>`, one text chunk per logical
line, like upstream's post-`setStyledText` rope):

* **Offset model** — upstream's display-width offsets (newlines cost 1,
  `coordsToOffset`/`offsetToCoords`) are preserved exactly; every
  cursor, selection, and `getTextRange` range is offset-based.
* **The one-line invariant** — an empty document still has one (empty)
  line (oracle: `setText("")` → `getLineCount() == 1`).
* **Undo/redo** — linear snapshot stacks with the cursor meta string
  (`cursor:row:col:desired[:tab]`) as the public undo return; tab-width
  changes re-measure restored lines (`refreshTabWidthMetrics`) and remap
  cursor columns.
* **The event pump** — upstream's `cursorChanged` event emitter becomes
  a version token: the view pumps it at every entry point (same ordering
  as the synchronous upstream emit), which drives
  `ensureCursorVisible`/placeholder visibility.
* **Deferred selection delivery** — upstream notifies selected
  renderables synchronously inside `ctx.updateSelection`; the port
  delivers at the action boundary gated on a dirty flag
  (observationally equivalent for the keyboard path — the start-notify's
  zero-width selection is immediately superseded).
* **Rendering** — `draw_editor_view` ports the EditorView branch of
  `drawTextBufferInternal` (viewport bg prefill, per-cell selection
  highlight, INVERSE-after-selection swap); the terminal cursor
  position/style/color emission (OSC12 + DECSCUSR + CUP + DECTCEM with
  the diff cache) extends the frame writer.

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

## The events family (input lane)

`opentui-core::parse_keypress` / `parse_keypress_kitty` /
`parse_mouse` / `paste` / `events` / `keybinding` / `input` /
`scheduler` port the input surface by source translation, and the
whole surface is pinned by TWO oracle instruments:

* **Parser characterization** — a frozen corpus generated by running
  the real `@opentui/core@0.5.14` parsers over 6307 key sequences
  (legacy + kitty profiles), 162 mouse cases and 12 paste decodes;
  the Rust tests replay every case field-for-field (including the
  `undefined`-vs-`false` distinction of upstream's optional fields).
* **Renderer event mode** — the harness feeds byte sequences to a REAL
  renderer (bun, `renderer.stdin.emit("data", …)`) and records the
  keypress/keyrelease/paste stream from `renderer.keyInput`; the Rust
  `CliRenderer::feed_bytes` must produce the EXACT same event
  sequence. 372 feeds are frozen in the corpus; incomplete units
  (lone ESC, split UTF-8) stay pending on both sides, matching the
  oracle's immediate-event behavior. The full protocol-context
  `StdinParser` state machine (CPR/DA/pixel-resolution deferral) is
  the documented follow-up.

`InputRenderable` ports the single-line slice of the
EditBuffer/EditorView stack: the native word-boundary scan
(`utf8.zig` word classes + script transitions, CJK runs moving as one
word), scroll-to-cursor (`editor-view.zig` wrap-none column window,
`scrollMargin` 0.2), the placeholder buffer switch, and the
transparent-text fast path. The multi-line EditBuffer (rope,
multi-cursor, extmarks) is the textarea lane. The scheduler
(`RenderScheduler`) ports the requestRender contract — coalescing,
`minTargetFrameTime`/`targetFrameTime` cadences, rAF draining — over
a deterministic virtual clock.

## The markdown + code family (markdown lane)

`MarkdownRenderable` (Markdown.ts:270) and `CodeRenderable`
(Code.ts:48) render THROUGH the O2a text engine: marked@17.0.1 (the
exact pinned dependency) lexes the document into block tokens; every
block becomes a Code/Box/Text renderable tree over
[`TextRenderableState`]; markdown text blocks are `CodeRenderable`s
with filetype `markdown` whose styling + concealment comes from
tree-sitter. Three instruments pin the pipeline:

1. **marked token corpus** — 40 documents through the real
   `Lexer.lex(content, {gfm: true})`, frozen under
   `tests/data/marked-corpus.json`; the Rust lexer port
   (`renderables::markdown_parser`) replays every token tree
   byte-identically (raw/text/depth/ordered/start/loose/items/header/
   rows/align/tokens recursion). The lexer is a hand port of marked's
   tokenizer chain (its regexes use lookaheads/backreferences the Rust
   `regex` crate lacks) — including the em/strong delimiter-run
   algorithm, the inline masking pre-passes (reflink/code-span/
   anyPunctuation), list continuation with the R-flag lazy rules, and
   marked's empirical raw quirks (e.g. hr keeps a single trailing
   newline but drops blank-line runs; the lone-`
` space token merges
   into the previous token's raw).
2. **highlight corpus** — 27 (content, filetype) cases through the
   REAL TreeSitterClient under bun (web-tree-sitter wasm + the shipped
   .wasm grammars + .scm queries), frozen under
   `tests/data/highlight-corpus.json`; the Rust tree-sitter port must
   reproduce every `[start, end, group, meta?]` tuple byte-identically
   (capture order, offsets, conceal/conceal_lines/injection meta).
3. **differential scenarios** — `md-*`/`code-*` (18) exercising every
   construct: headings (atx+setext), emphasis/codespans/strikethrough,
   links/autolinks, lists (tight/ordered/nested/tasks), blockquotes,
   hr, fences (ts/js/zig/unknown/plain), indented code, long-line
   wrapping inside and outside code, mixed documents, conceal=false,
   and two full theme palettes — all byte-identical vs 0.5.14.

### tree-sitter: in-process C grammars at upstream's exact pins

Upstream runs tree-sitter in a worker (web-tree-sitter 0.25.10 wasm)
loading the npm-shipped `.wasm` grammars (javascript v0.25.0,
typescript v0.23.2, markdown + markdown_inline v0.5.1, zig v1.1.2 —
parsers-config.ts). The port instead **links the same grammars as C
sources compiled in-process** (`third_party/tree-sitter-grammars/`,
vendored from the same tags via their PyPI sdists; build.rs compiles
parser.c + scanner.c with cc) and runs the byte-identical `.scm`
queries (`include_str!` from the spec's own assets) against the
`tree-sitter` crate 0.25.10 — the SAME C core version as
web-tree-sitter's wasm. This is the opencode-rust `oc-syntax` pattern
(in-process, crate-pinned grammars) but pinned to opentui's own asset
versions. **Grammar drift: zero** — all 27 corpus cases byte-identical
(including the ts/js/zig fence injections and CJK/emoji offsets).

Mechanism deltas (documented; output parity holds):
* no worker thread — the port highlights synchronously; upstream's
  async round-trip produces the same SETTLED frame the differential
  oracle captures after `flush()` (the driver's settle loop awaits
  `highlightingDone` until no CodeRenderable is dirty/highlighting —
  upstream's mount invalidates mid-flight and re-runs);
* web-tree-sitter reports node offsets as UTF-16 code units (verified
  against the oracle with CJK/emoji); the port converts byte→UTF-16 at
  the boundary (`Utf16Map`) so highlight tuples and the chunk sweep
  (which slices by those indices) match;
* `addDefaultParsers` takes compiled-in `LanguageProvider`s where
  upstream takes `.wasm` paths;
* streaming/`renderNode` reconciliation is upstream liveness
  machinery; the port constructs the settled block tree (the
  coalesced `internalBlockMode` path; tables render through the
  raw-markdown fallback until `TextTableRenderable` lands — the
  documented follow-up).

## The perf lane: frame budgets + the vline cache contract

`tools/bench` is the perf rig (see its README for design + numbers):
buffer draw paths, the taffy measure target, the full render cycle at
80x24/120x30/200x50 over upstream-shaped trees AND the differential
scenario fixtures (replayed through the same `replay` machinery the
oracle proves), plus isolated full-repaint SGR emission via two
doc-hidden bench hooks on `CliRenderer`. The oracle side of the
comparison runs upstream's own `render-traversal-benchmark.ts` trees
against the REAL 0.5.14 under bun (`tools/bench/oracle`), version-
asserted like every other oracle.

The allocation audit's one semantic-adjacent mechanism: **the Text
vline cache**. Virtual lines are cached per Text node and filled
during `prepare_paint` (the `&mut` pre-pass), so the `&self` paint
borrows instead of recomputing the wrap every frame. Invalidation
contract: `update_layout` invalidates on viewport change;
`refresh_text_measure` invalidates on content change — the SAME seam
that refreshes the taffy measure context (the only path that keeps
layout correct after a doc mutation), so no observable drift is
possible (pinned: `run-all.sh` 74/74 IDENTICAL). The editor stack's
`active_vlines` similarly borrows its cache (keyed on content version
+ wrap width); the draw snapshot clones once at the frame boundary.

Numbers (dev box): full render cycle at 120x30 = 0.029ms median
(mixed tree), 0.047ms dirty scroll frames (960-child culling
scrollbox) — 2-3 orders under the consumer's 40ms TUI gate, at parity
or faster than the real 0.5.14 under bun on every painted path.

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
