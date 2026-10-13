# tools/bench — the perf rig (frame budgets)

Benchmarks `opentui-core`'s hot paths at the frame level. The trees are
the exact ones the differential harness proves byte-identical against
the REAL `@opentui 0.5.14` (scenario JSONs replayed through the same
`opentui_diff::scenario::replay` machinery) plus synthetic trees
mirroring **upstream's own** `render-traversal-benchmark.ts` shapes —
so the port's numbers compare against the real package driven under
bun **on the same box** (`oracle/render-traversal-oracle.ts`).

## Rig design

Plain bencher (`harness = false`), no criterion: stable toolchain,
deterministic iteration counts (300 timed + 40 warmup; `--quick` =
30 + 5 for CI), medians/p95/min/max — upstream's own benchmark stat
shape. `std::hint::black_box` on consumed outputs.

| suite | what it measures |
|---|---|
| `buffer/*` | `OptimizedBuffer` draw paths at 80x24/120x30/200x50: `clear`, `draw_text` (ascii fast path / `bg: None` destination-read / wide clusters), `fill_rect`, `draw_box` (bordered + fill), scattered `set` under a scissor |
| `measure/*` | the taffy measure target: `TextRenderableState::measure` word-wrap at width 78 + intrinsic |
| `render/*` | full render cycle (clear → taffy layout → settle walk → paint → SGR diff → swap) on synthetic trees: OpenCode-shaped wrappers (upstream's `buildOpencodeLayoutTree`), mixed (visible panels + text), culling scrollbox (steady + alternating ±1 scroll dirty frames), word-wrap paragraphs, deep box chain, and cold mount (`render/first-frame` = construct + first render) |
| `fixture/*` | the differential scenario trees re-rendered: `cold-mount` (load + full step replay + first frame — markdown/code include marked + tree-sitter), `steady` (settled re-render), `scroll-dirty` (alternate scrollTop 0/1) |
| `sgr/*` | isolated full-repaint SGR emission (the `prepareRenderFrameWithWriter` path, via the doc-hidden bench hooks on `CliRenderer`) |

A paint-sanity test (`tests/sanity.rs`) guards the tree builders: a
regression that stops painting would silently rot every number.

```sh
cargo run --release -p opentui-bench                     # full table
cargo run --release -p opentui-bench -- --filter 120x30  # subset
cargo run --release -p opentui-bench -- --list           # inventory
tools/bench/oracle/run-oracle.sh                         # REAL 0.5.14 under bun
```

## Numbers (dev box, 2026-10-13, release profile)

### Frame budgets vs the gates

The consumer gate (opencode-rust TUI) is **40ms TEMP**; the lane
target was "well under, optimize if >5ms median at 120x30":

| path (120x30) | median | vs 5ms target |
|---|---|---|
| full render cycle, mixed tree | **0.029 ms** | 170x under |
| full render cycle, word-wrap paragraphs | **0.055 ms** | 90x under |
| dirty scroll frame, culling scrollbox (960 children) | **0.047 ms** | 106x under |
| full-repaint SGR emission | **0.016 ms** | — |
| cold mount (construct + first frame) | **4.18 ms** | — |

Every steady frame path is 2–3 orders of magnitude inside the
interactive budget; the worst path measured anywhere is the 200x50
cold mount at 7.5ms (tree construction dominated, one-off).

### Optimization before/after (medians, ms)

The allocation audit found three per-frame clone hot spots (all fixed
WITHOUT observable change — `tools/diff/run-all.sh` 74/74 IDENTICAL
with refreshed oracles):

1. **text paint cloned the virtual-line vec** per text node per frame
   (`TextRenderableState::paint`) — and the cache was never filled, so
   every paint also RECOMPUTED the wrap. Now `prepare_paint` fills the
   cache (`renderable.rs`), paint borrows it, and
   `refresh_text_measure` invalidates on content change (the same seam
   that refreshes the taffy measure context — the only correct-mutation
   path).
2. **the taffy measure callback cloned the whole `MeasuredText` /
   `MeasuredEditor`** (document included) per measure call — now
   borrows (`renderer.rs::measure_text`).
3. **`EditorView::active_vlines` returned an owned `EditorVlines`**
   (4 vecs) at 19 call sites — several hits per frame
   (`ensureCursorVisible`, draw snapshot, motion). Now borrows; the
   draw snapshot clones once at the frame boundary instead of per call
   (`editor.rs`).

| benchmark | before | after | delta |
|---|---|---|---|
| render/wordwrap/80x24 | 0.217 | 0.030 | **-86%** |
| render/wordwrap/120x30 | 0.297 | 0.055 | **-81%** |
| render/wordwrap/200x50 | 0.554 | 0.150 | **-73%** |
| render/deep/80x24 | 0.022 | 0.014 | -34% |
| render/deep/120x30 | 0.045 | 0.034 | -23% |
| fixture/md-mixed/steady | 0.018 | 0.009 | -52% |
| fixture/word-wrap-edges/steady | 0.005 | 0.002 | -59% |
| fixture/scrollbox-long-math/scroll-dirty | 0.010 | 0.006 | -38% |
| fixture/scrollbox-nested/scroll-dirty | 0.007 | 0.004 | -46% |
| fixture/code-ts/cold-mount | 0.115 | 0.077 | -33% |
| fixture/attr-matrix/steady | 0.004 | 0.002 | -59% |
| buffer/*, measure/*, sgr/* | (unchanged — no allocation work landed there) | | |

Full tables: the rig prints them; `--json` writes them.

### Oracle comparison — the REAL 0.5.14 under bun, same box

`oracle/render-traversal-oracle.ts` (adapted from upstream's
`packages/core/src/benchmark/render-traversal-benchmark.ts` @ 0.5.14:
same trees, same stats loop; asserts the resolved core is exactly
0.5.14 before timing). Frame medians, ms:

| tree | size | opentui-rust | REAL 0.5.14 (bun) | ratio |
|---|---|---|---|---|
| layout-only wrappers | 80x24 | 0.022 | 0.015 | 1.5x |
| layout-only wrappers | 120x30 | 0.025 | 0.017 | 1.5x |
| layout-only wrappers | 200x50 | 0.046 | 0.034 | 1.4x |
| mixed (panels + text) | 80x24 | 0.024 | 0.048 | **0.5x** |
| mixed (panels + text) | 120x30 | 0.029 | 0.037 | **0.8x** |
| mixed (panels + text) | 200x50 | 0.059 | 0.067 | **0.9x** |
| culling scroll ±1/frame | 80x24 | 0.033 | 0.084 | **0.4x** |
| culling scroll ±1/frame | 120x30 | 0.047 | 0.145 | **0.3x** |
| culling scroll ±1/frame | 200x50 | 0.109 | 0.279 | **0.4x** |
| word-wrap paragraphs | 80x24 | 0.030 | 0.034 | **0.9x** |
| word-wrap paragraphs | 120x30 | 0.055 | 0.061 | **0.9x** |
| word-wrap paragraphs | 200x50 | 0.150 | 0.177 | **0.8x** |
| deep box chain | 80x24 | 0.014 | 0.039 | **0.4x** |
| deep box chain | 120x30 | 0.034 | 0.076 | **0.4x** |
| deep box chain | 200x50 | 0.111 | 0.238 | **0.5x** |

The port is at parity or faster on every painted path (upstream's
number includes the TS facade + FFI into the native Zig renderer; the
pure-layout wrapper tree — where paint is nearly empty — is the one
shape where the oracle's steady walk is ~1.5x faster). Both stacks sit
orders of magnitude below the interactive budget on this hardware.

## CI ceilings

The `perf` job runs the rig's `--quick --check` tier with ceilings in
`src/lib.rs::CEILINGS` — deliberately ~10x the dev-box medians
(shared runners are noisy, 2-5x slower). Philosophy: **"no
catastrophic regression"**, not a budget — e.g. the headline
`render/mixed/120x30 < 50ms` ceiling is the named interactive-budget
guard (the 40ms opencode-rust TUI gate with shared-runner headroom);
a per-frame clone regression returning to the paint walk would show
up as a 10-100x median jump and trip it, scheduler jitter will not.

Perf CI is separate from the differential job by construction:
correctness there is byte-identity; this job only guards budgets.
