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
| `opentui-core` | `@opentui/core` — colors (intent metadata), cell grid + draw ops, flexbox layout (taffy), renderable tree, Box/Text renderables, StyledText/TextNode content model (multi-chunk styles, align/tab/wrap none+char+word), headless renderer + SGR emission | **differentially IDENTICAL** on 17 scenarios |
| `opentui-keymap` | `@opentui/keymap` — key stringify (exact port), Keymap facade, layered dispatch engine (adapted from opencode-rust's proven `oc-tui-core` port, cited) | skeleton |
| `opentui-solid` | `@opentui/solid` — the reactive glue equivalent: signal boundary + builder component tree (design record in the crate docs) | design + mount-only |

## Layout

```
crates/            the three port crates (cargo workspace)
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

Bootstrap + render-primitives lane results (see `tools/diff/out/reports/`):

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

Each run also vt-replays the port's SGR stream through a vt100 parser to
prove the emitted bytes repaint the same grid.

## Roadmap (next lanes, per API family)

1. **render-primitives** — **done** (StyledText multi-chunk, TextNode
   tree, text alignment, tab indicators, wrapping modes char/none —
   17/17 differential scenarios IDENTICAL vs `@opentui/core@0.5.14`).
2. **scrollbox-lane** — ScrollBox + ScrollBar + scroll accel + nested
   clipping (scissor rects port from buffer.zig).
3. **textarea-lane** — EditBuffer/TextBuffer/editor primitives + selection.
4. **input-lane** — Input renderable + key events (KeyEvent/PasteEvent
   parsing from lib/parse.keypress*.ts).
5. **events-lane** — mouse parser, hit grid, focus traversal, renderer
   event loop (requestRender scheduling semantics).
6. **keymap-lane** — full `@opentui/keymap` type surface + opentui host +
   solid provider + addons.
7. **solid-glue-lane** — keyed reconciler diffing (props update, child
   reorder, unmount), Portal, scrollback writer.
8. **markdown/code-lane** — MarkdownRenderable, CodeRenderable +
   tree-sitter styled text (consumer bridges its existing syntax crate).
9. **perf-lane** — frame budgets vs the native Zig renderer, allocation
   audit of the cell grid.

## License attribution

OpenTUI upstream (vendored under `third_party/opentui`) is © its authors,
**MIT licensed**. This port cites that source per semantic and reproduces
the upstream LICENSE file in place. The port code itself is MIT.
