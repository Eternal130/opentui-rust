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
