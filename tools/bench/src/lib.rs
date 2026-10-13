//! opentui-bench — the perf rig for `opentui-core`'s hot paths.
//!
//! Frame-budget benchmarks over the exact semantics the differential
//! harness proves: scenario JSONs (the same staged trees `tools/diff`
//! renders byte-identical vs the REAL `@opentui` 0.5.14) are replayed
//! to their settled state and re-rendered in a loop; synthetic trees
//! mirror upstream's own `render-traversal-benchmark.ts` shapes so the
//! numbers compare against the real package driven under bun
//! (`oracle/render-traversal-oracle.ts` — same trees, same stats).
//!
//! Plain bencher (harness = false, no criterion): stable toolchain,
//! deterministic iteration counts, medians robust to noise. `--check`
//! enforces generous ceilings for the CI perf job ("no catastrophic
//! regression", NOT a budget — shared runners are noisy; see
//! `tools/bench/README.md`).
//!
//! Run: `cargo run --release -p opentui-bench -- [--filter substr]
//! [--quick] [--check] [--json path] [--list]`
//! (equivalently `cargo bench -p opentui-bench -- …`).

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use opentui_core::buffer::{DrawBoxOptions, OptimizedBuffer};
use opentui_core::color::Rgba;
use opentui_core::layout::{Dimension, LayoutOptions};
use opentui_core::renderables::box_::{border_char_set, BorderSidesConfig, BoxOptions};
use opentui_core::renderables::scrollbox::ScrollBoxOptions;
use opentui_core::renderables::text::{TextOptions, TextRenderableState, WrapMode};
use opentui_core::renderer::{create_cli_renderer, CliRenderer, CliRendererConfig};

// ---------------------------------------------------------------------------
// measurement core
// ---------------------------------------------------------------------------

/// One measured benchmark: wall-time statistics over N timed iterations
/// (after warmup), in nanoseconds.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Sample {
    pub name: String,
    pub warmup: u32,
    pub iterations: u32,
    pub min_ns: u128,
    pub median_ns: u128,
    pub p95_ns: u128,
    pub max_ns: u128,
}

impl Sample {
    #[must_use]
    pub fn median_ms(&self) -> f64 {
        self.median_ns as f64 / 1e6
    }
}

/// Measure `f(iteration)` — upstream's benchmark stat shape
/// (`render-traversal-benchmark.ts` `runScenario`): warmup, then one
/// `performance.now()` pair per iteration; median/p95/min/max.
fn measure<F: FnMut(u32)>(name: &str, warmup: u32, iterations: u32, mut f: F) -> Sample {
    for i in 0..warmup {
        f(i);
    }
    let mut samples: Vec<u128> = Vec::with_capacity(iterations as usize);
    for i in 0..iterations {
        let start = Instant::now();
        f(i);
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let pick = |q: f64| -> u128 {
        let idx = ((samples.len() as f64 - 1.0) * q).floor() as usize;
        samples[idx.min(samples.len() - 1)]
    };
    Sample {
        name: name.to_owned(),
        warmup,
        iterations,
        min_ns: samples[0],
        median_ns: pick(0.5),
        p95_ns: pick(0.95),
        max_ns: samples[samples.len() - 1],
    }
}

/// The rig: name filtering + quick/CI iteration tiers + collected samples.
pub struct Rig {
    /// `--quick`: CI tier (fewer iterations; ceilings unchanged).
    pub quick: bool,
    /// `--list`: register names only, measure nothing.
    pub list_only: bool,
    filter: String,
    pub samples: Vec<Sample>,
}

impl Rig {
    #[must_use]
    pub fn new(filter: &str, quick: bool, list_only: bool) -> Self {
        Rig {
            quick,
            list_only,
            filter: filter.to_owned(),
            samples: Vec::new(),
        }
    }

    fn matches(&self, name: &str) -> bool {
        self.filter.is_empty() || name.contains(&self.filter)
    }

    /// Standard tier: ~300 iterations (30 + 5 quick).
    pub fn run<F: FnMut(u32)>(&mut self, name: &str, f: F) {
        let (warmup, iters) = if self.quick { (5, 30) } else { (40, 300) };
        self.run_tiered(name, warmup, iters, f);
    }

    /// Slow tier for construction-heavy benches (cold mount incl.
    /// markdown lexing + tree-sitter): 25 iterations (6 + 2 quick).
    pub fn run_slow<F: FnMut(u32)>(&mut self, name: &str, f: F) {
        let (warmup, iters) = if self.quick { (2, 6) } else { (5, 25) };
        self.run_tiered(name, warmup, iters, f);
    }

    fn run_tiered<F: FnMut(u32)>(&mut self, name: &str, warmup: u32, iterations: u32, f: F) {
        if !self.matches(name) {
            return;
        }
        if self.list_only {
            self.samples.push(Sample {
                name: name.to_owned(),
                warmup,
                iterations,
                min_ns: 0,
                median_ns: 0,
                p95_ns: 0,
                max_ns: 0,
            });
            return;
        }
        let sample = measure(name, warmup, iterations, f);
        eprintln!(
            "  {name}: median {:.3} ms (p95 {:.3} ms, min {:.3} ms)",
            sample.median_ms(),
            sample.p95_ns as f64 / 1e6,
            sample.min_ns as f64 / 1e6,
        );
        self.samples.push(sample);
    }
}

// ---------------------------------------------------------------------------
// suites
// ---------------------------------------------------------------------------

const SIZES: [(u32, u32); 3] = [(80, 24), (120, 30), (200, 50)];

/// Upstream's benchmark palette (`render-traversal-benchmark.ts` COLORS)
/// as hex — same values the oracle trees paint with.
mod colors {
    pub const PANEL: &str = "#1c2026"; // RGBA.fromInts(28, 32, 38)
    pub const ELEMENT: &str = "#282e38"; // RGBA.fromInts(40, 46, 56)
    pub const MENU: &str = "#232830"; // RGBA.fromInts(35, 40, 48)
}

/// Run every suite (each bench self-filters via [`Rig::run`]).
pub fn run_suites(rig: &mut Rig) -> anyhow::Result<()> {
    suite_buffer(rig);
    suite_measure(rig);
    suite_render_trees(rig)?;
    suite_fixtures(rig)?;
    suite_sgr(rig)?;
    Ok(())
}

/// `tools/diff/scenarios` — the fixture source (CARGO_MANIFEST_DIR is
/// `<repo>/tools/bench`).
#[must_use]
pub fn scenario_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../diff/scenarios")
}

// --- A. OptimizedBuffer draw paths -----------------------------------------

fn suite_buffer(rig: &mut Rig) {
    for &(w, h) in &SIZES {
        let mut buf = OptimizedBuffer::new(w, h);
        let fg = Rgba::rgb(220, 220, 220);
        let bg_opaque = Rgba::rgb(24, 26, 32);
        let ascii_line = "the quick brown fox jumps over the lazy dog ".repeat(8);
        let ascii_line = &ascii_line[..(w as usize).min(ascii_line.len()) - 1];
        let wide_line: String = "你好世界渲染测试"
            .chars()
            .take(w as usize / 2 + 2)
            .collect();

        rig.run(&format!("buffer/clear/{w}x{h}"), |_| {
            buf.clear(Rgba::from_ints(0, 0, 0, 0));
        });
        rig.run(&format!("buffer/draw_text/ascii/{w}x{h}"), |_| {
            for y in 0..h {
                buf.draw_text(ascii_line, 0, y, fg, Some(bg_opaque), 0);
            }
        });
        // bg: None — the destination-bg read path (text-over-filled-box)
        rig.run(&format!("buffer/draw_text/bg-none/{w}x{h}"), |_| {
            for y in 0..h {
                buf.draw_text(ascii_line, 0, y, fg, None, 0);
            }
        });
        // wide clusters: grapheme pool + continuation planting + span
        // cleanup of the previous iteration's clusters
        rig.run(&format!("buffer/draw_text/wide/{w}x{h}"), |_| {
            for y in 0..h {
                buf.draw_text(&wide_line, 0, y, fg, Some(bg_opaque), 0);
            }
        });
        rig.run(&format!("buffer/fill_rect/{w}x{h}"), |_| {
            buf.fill_rect(0, 0, w, h, bg_opaque);
        });
        rig.run(&format!("buffer/draw_box/bordered/{w}x{h}"), |_| {
            buf.draw_box(DrawBoxOptions {
                x: 0,
                y: 0,
                width: w,
                height: h,
                border_sides: BorderSidesConfig::all(),
                border_chars: border_char_set("single"),
                border_color: Rgba::rgb(96, 140, 200),
                background_color: bg_opaque,
                should_fill: true,
                title: None,
                title_color: Rgba::rgb(255, 255, 255),
                title_alignment: 0,
                bottom_title: None,
                bottom_title_alignment: 0,
            });
        });
        // scattered set() under an active scissor (the scroll-viewport
        // write pattern)
        rig.run(&format!("buffer/set/scissored/{w}x{h}"), |_| {
            buf.push_scissor_rect(2, 1, w - 4, h - 2);
            let cell = Cell {
                char: u32::from(b'x'),
                fg,
                bg: bg_opaque,
                attributes: 0,
            };
            let mut i = 0u32;
            for y in 0..h {
                for x in (0..w).step_by(3) {
                    buf.set(x, (y + i) % h, cell);
                }
                i = i.wrapping_add(1);
            }
            buf.pop_scissor_rect();
        });
    }
}

// --- B. the taffy measure path ----------------------------------------------

fn suite_measure(rig: &mut Rig) {
    // a representative document: 24 logical lines, ~5 wrapped lines each
    // at width 78 (word wrap) — the Text leaf measure target.
    let paragraph = "The settle loop drives layout through the measure target: \
every Text leaf re-measures its wrapped content whenever taffy dirties the node, \
so the per-call cost bounds how fast a large document tree can reflow."
        .repeat(3);
    let content = (0..24)
        .map(|i| format!("line {i:02}: {paragraph}"))
        .collect::<Vec<_>>()
        .join("\n");
    let state = TextRenderableState::from_options(&TextOptions::with_str_content(&content));
    let measured = state.measured();
    rig.run("measure/text-word-wrap/78", |_| {
        let size = TextRenderableState::measure(black_box(&measured), None, 78.0);
        black_box(size);
    });
    rig.run("measure/text-intrinsic", |_| {
        let size = TextRenderableState::measure(black_box(&measured), None, f32::INFINITY);
        black_box(size);
    });
}

// --- C. synthetic render trees (upstream's shapes) --------------------------

fn box_options(
    width: Option<Dimension>,
    height: Option<Dimension>,
    background: Option<&str>,
    layout: LayoutOptions,
) -> BoxOptions {
    BoxOptions {
        layout,
        width,
        height,
        background_color: background.map(str::to_owned),
        ..Default::default()
    }
}

fn add_box_child(r: &mut CliRenderer, parent: &str, id: &str, opts: BoxOptions) {
    let node = r.make_box(id, &opts);
    if parent == "root" {
        // the root renderable's id is `__root__` — root-level nodes go
        // through `renderer.add` (same as the scenario mount machinery)
        r.add(node);
    } else {
        r.mount_child(parent, node);
    }
}

fn pct() -> Dimension {
    Dimension::Percent(1.0)
}

/// `buildOpencodeLayoutTree` (render-traversal-benchmark.ts) — the
/// OpenCode-like wrapper tree: header chips, sidebar, message rows,
/// footer. `visible` adds painted panels; `text` adds a Text leaf every
/// 4th message.
fn build_opencode_tree(r: &mut CliRenderer, w: u32, h: u32, visible: bool, text: bool) {
    let message_count = if visible {
        40.max(h + 8)
    } else {
        48.max(h + 12)
    };
    let root_layout = LayoutOptions {
        flex_direction: Some("column".into()),
        ..Default::default()
    };
    let root = box_options(Some(pct()), Some(pct()), None, root_layout.clone());
    add_box_child(r, "root", "bench-root", root);

    // header: height 3, row, padding-x 1, gap 1 + 5 chips
    let header_layout = LayoutOptions {
        flex_direction: Some("row".into()),
        padding_left: Some(Dimension::Points(1.0)),
        padding_right: Some(Dimension::Points(1.0)),
        gap: Some(Dimension::Points(1.0)),
        ..Default::default()
    };
    add_box_child(
        r,
        "bench-root",
        "bench-header",
        box_options(
            Some(pct()),
            Some(Dimension::Points(3.0)),
            None,
            header_layout,
        ),
    );
    for i in 0..5 {
        let chip = LayoutOptions {
            flex_shrink: Some(0.0),
            padding_left: Some(Dimension::Points(1.0)),
            padding_right: Some(Dimension::Points(1.0)),
            ..Default::default()
        };
        add_box_child(
            r,
            "bench-header",
            &format!("bench-chip-{i}"),
            box_options(None, None, None, chip),
        );
    }

    // body: grow 1, row, gap 1, padding-x 1
    let body_layout = LayoutOptions {
        flex_grow: Some(1.0),
        flex_direction: Some("row".into()),
        padding_left: Some(Dimension::Points(1.0)),
        padding_right: Some(Dimension::Points(1.0)),
        gap: Some(Dimension::Points(1.0)),
        ..Default::default()
    };
    add_box_child(
        r,
        "bench-root",
        "bench-body",
        box_options(Some(pct()), None, None, body_layout),
    );

    // sidebar: width 22 fixed, column, gap 1, 12 rows height 1 padding-x 1
    let sidebar_layout = LayoutOptions {
        flex_direction: Some("column".into()),
        flex_shrink: Some(0.0),
        gap: Some(Dimension::Points(1.0)),
        min_width: Some(Dimension::Points(22.0)),
        max_width: Some(Dimension::Points(22.0)),
        ..Default::default()
    };
    add_box_child(
        r,
        "bench-body",
        "bench-sidebar",
        box_options(
            Some(Dimension::Points(22.0)),
            Some(Dimension::Points(22.0)),
            None,
            sidebar_layout,
        ),
    );
    for i in 0..12 {
        let row = LayoutOptions {
            padding_left: Some(Dimension::Points(1.0)),
            padding_right: Some(Dimension::Points(1.0)),
            ..Default::default()
        };
        add_box_child(
            r,
            "bench-sidebar",
            &format!("bench-sidebar-row-{i}"),
            box_options(None, Some(Dimension::Points(1.0)), None, row),
        );
    }

    // main: grow 1, column, gap 1 — the message rows
    let main_layout = LayoutOptions {
        flex_grow: Some(1.0),
        flex_direction: Some("column".into()),
        gap: Some(Dimension::Points(1.0)),
        ..Default::default()
    };
    add_box_child(
        r,
        "bench-body",
        "bench-main",
        box_options(Some(pct()), None, None, main_layout),
    );
    for i in 0..message_count {
        let row = LayoutOptions {
            flex_direction: Some("row".into()),
            ..Default::default()
        };
        add_box_child(
            r,
            "bench-main",
            &format!("bench-row-{i}"),
            box_options(Some(pct()), None, None, row),
        );
        let rail = LayoutOptions {
            flex_shrink: Some(0.0),
            ..Default::default()
        };
        add_box_child(
            r,
            &format!("bench-row-{i}"),
            &format!("bench-rail-{i}"),
            box_options(Some(Dimension::Points(3.0)), None, None, rail),
        );
        let content = LayoutOptions {
            flex_grow: Some(1.0),
            flex_direction: Some("column".into()),
            ..Default::default()
        };
        add_box_child(
            r,
            &format!("bench-row-{i}"),
            &format!("bench-content-{i}"),
            box_options(None, None, None, content.clone()),
        );
        let meta = LayoutOptions {
            flex_direction: Some("row".into()),
            justify_content: Some("space-between".into()),
            ..Default::default()
        };
        add_box_child(
            r,
            &format!("bench-content-{i}"),
            &format!("bench-meta-{i}"),
            box_options(Some(pct()), Some(Dimension::Points(1.0)), None, meta),
        );
        let badges = LayoutOptions {
            flex_direction: Some("row".into()),
            gap: Some(Dimension::Points(1.0)),
            ..Default::default()
        };
        add_box_child(
            r,
            &format!("bench-meta-{i}"),
            &format!("bench-badges-{i}"),
            box_options(None, None, None, badges),
        );
        let actions = LayoutOptions {
            flex_direction: Some("row".into()),
            gap: Some(Dimension::Points(1.0)),
            flex_shrink: Some(0.0),
            ..Default::default()
        };
        add_box_child(
            r,
            &format!("bench-meta-{i}"),
            &format!("bench-actions-{i}"),
            box_options(None, None, None, actions),
        );
        if visible {
            let leaf_h = if i % 5 == 0 {
                3.0
            } else if i % 2 == 0 {
                2.0
            } else {
                1.0
            };
            let bg = match i % 3 {
                0 => colors::MENU,
                1 => colors::PANEL,
                _ => colors::ELEMENT,
            };
            add_box_child(
                r,
                &format!("bench-content-{i}"),
                &format!("bench-leaf-{i}"),
                box_options(
                    Some(pct()),
                    Some(Dimension::Points(leaf_h)),
                    Some(bg),
                    LayoutOptions::default(),
                ),
            );
        }
        if text && i % 4 == 0 {
            let opts = TextOptions::with_str_content(&format!("message-{i}"));
            let node = r.make_text(&format!("bench-text-{i}"), &opts);
            r.mount_child(&format!("bench-content-{i}"), node);
        }
    }

    // footer: height 4, row, gap 1, padding-x 1 + 6 items
    let footer_layout = LayoutOptions {
        flex_direction: Some("row".into()),
        padding_left: Some(Dimension::Points(1.0)),
        padding_right: Some(Dimension::Points(1.0)),
        gap: Some(Dimension::Points(1.0)),
        ..Default::default()
    };
    add_box_child(
        r,
        "bench-root",
        "bench-footer",
        box_options(
            Some(pct()),
            Some(Dimension::Points(4.0)),
            None,
            footer_layout,
        ),
    );
    for i in 0..6 {
        let item = LayoutOptions {
            flex_direction: Some("row".into()),
            gap: Some(Dimension::Points(1.0)),
            ..Default::default()
        };
        add_box_child(
            r,
            "bench-footer",
            &format!("bench-footer-item-{i}"),
            box_options(None, None, None, item),
        );
    }
    let _ = w;
}

/// `scrollbox_viewport_culling` / `createCullingScalingScenario`
/// (render-traversal-benchmark.ts): a viewport-culling scrollbox with
/// `max(120, h*8)` single-row children, alternating panel/element
/// backgrounds, scrolled to the middle.
fn build_culling_scrollbox(r: &mut CliRenderer, _w: u32, _h: u32, item_count: usize) -> String {
    let root = box_options(Some(pct()), Some(pct()), None, LayoutOptions::default());
    add_box_child(r, "root", "bench-cull-root", root);
    let mut opts = ScrollBoxOptions {
        viewport_culling: true,
        scroll_y: true,
        ..Default::default()
    };
    opts.box_options = box_options(Some(pct()), Some(pct()), None, LayoutOptions::default());
    let sb = r.make_scrollbox("bench-sb", &mut opts);
    r.mount_child("bench-cull-root", sb);
    for i in 0..item_count {
        let bg = if i % 2 == 0 {
            colors::PANEL
        } else {
            colors::ELEMENT
        };
        let row = LayoutOptions {
            flex_shrink: Some(0.0),
            ..Default::default()
        };
        add_box_child(
            r,
            "bench-sb",
            &format!("bench-cull-row-{i}"),
            box_options(Some(pct()), Some(Dimension::Points(1.0)), Some(bg), row),
        );
    }
    r.render();
    r.scroll_to("bench-sb", 0.0, (item_count / 2) as f32);
    "bench-sb".to_owned()
}

/// Long word-wrapped paragraphs filling the viewport — the text-heavy
/// document shape (measure + vlines + paint per leaf).
fn build_wordwrap_paragraphs(r: &mut CliRenderer, _w: u32, h: u32) {
    let sentence = "wraps at the viewport width with word wrap, measuring \
the widest wrapped line and painting every virtual line through the cluster sweep";
    let paragraphs: Vec<String> = (0..h.saturating_sub(2))
        .map(|i| format!("para {i:02}: {sentence} {sentence} {sentence}"))
        .collect();
    for (i, p) in paragraphs.iter().enumerate() {
        let opts = TextOptions {
            wrap_mode: Some(WrapMode::Word),
            ..TextOptions::with_str_content(p)
        };
        let node = r.make_text(&format!("bench-para-{i}"), &opts);
        r.add(node); // root-level, like the scenario machinery's mount
    }
}

/// A deep nested box chain (depth = terminal height, capped 24) with
/// overflow-hidden mid-chain (scissor-stack depth) and a text leaf at
/// the bottom.
fn build_deep_tree(r: &mut CliRenderer, w: u32, h: u32) {
    let depth = h.min(24) as usize;
    let mut parent = "root".to_owned();
    for i in 0..depth {
        let layout = LayoutOptions {
            padding: Some(Dimension::Points(1.0)),
            overflow: if i % 6 == 3 {
                Some("hidden".into())
            } else {
                None
            },
            ..Default::default()
        };
        let id = format!("bench-deep-{i}");
        add_box_child(
            r,
            &parent,
            &id,
            box_options(
                Some(pct()),
                Some(Dimension::Points((h - i as u32).max(1) as f32)),
                Some(colors::PANEL),
                layout,
            ),
        );
        parent = id;
    }
    let opts = TextOptions::with_str_content(&"deep leaf text ".repeat(w as usize / 4 + 1));
    let node = r.make_text("bench-deep-leaf", &opts);
    r.mount_child(&parent, node);
}

fn suite_render_trees(rig: &mut Rig) -> anyhow::Result<()> {
    for &(w, h) in &SIZES {
        // steady-state frame: layout walk + paint + SGR diff over an
        // unchanged tree (the row-equality fast path)
        let mut r = make_renderer(w, h);
        build_opencode_tree(&mut r, w, h, false, false);
        r.render();
        rig.run(&format!("render/wrappers/{w}x{h}"), |_| {
            black_box(r.render());
        });

        let mut r = build_mixed_tree(w, h);
        rig.run(&format!("render/mixed/{w}x{h}"), |_| {
            black_box(r.render());
        });

        // the streaming/scrolling frame: alternate 1-row scrolls — every
        // frame is a real translate change (upstream's culling-scaling
        // iteration shape)
        let mut r = make_renderer(w, h);
        let item_count = 120.max(h * 8) as usize;
        let sb = build_culling_scrollbox(&mut r, w, h, item_count);
        rig.run(&format!("render/culling-scroll/{w}x{h}"), |i| {
            let dy = if i % 2 == 0 { 1.0 } else { -1.0 };
            r.scroll_by(
                &sb,
                0.0,
                dy,
                opentui_core::renderables::scrollbar::ScrollUnit::Absolute,
            );
        });

        // settled culling tree, no mutation
        let mut r = build_culling_tree(w, h, item_count);
        rig.run(&format!("render/culling-steady/{w}x{h}"), |_| {
            black_box(r.render());
        });

        let mut r = build_wordwrap_tree(w, h);
        rig.run(&format!("render/wordwrap/{w}x{h}"), |_| {
            black_box(r.render());
        });

        let mut r = make_renderer(w, h);
        build_deep_tree(&mut r, w, h);
        r.render();
        rig.run(&format!("render/deep/{w}x{h}"), |_| {
            black_box(r.render());
        });

        // cold mount: construct renderer + tree + first frame (taffy
        // full layout + measure of every leaf + full-repaint SGR)
        rig.run_slow(&format!("render/first-frame/{w}x{h}"), |_| {
            let mut r = make_renderer(w, h);
            build_opencode_tree(&mut r, w, h, true, true);
            black_box(r.render());
        });
    }
    Ok(())
}

fn make_renderer(w: u32, h: u32) -> CliRenderer {
    create_cli_renderer(CliRendererConfig {
        width: w,
        height: h,
        ..Default::default()
    })
}

/// Settled mixed tree (visible panels + text leaves) — the rig's
/// headline tree, public for the paint-sanity test.
#[must_use]
pub fn build_mixed_tree(w: u32, h: u32) -> CliRenderer {
    let mut r = make_renderer(w, h);
    build_opencode_tree(&mut r, w, h, true, true);
    r.render();
    r
}

/// Settled word-wrap paragraph tree (public for the paint-sanity test).
#[must_use]
pub fn build_wordwrap_tree(w: u32, h: u32) -> CliRenderer {
    let mut r = make_renderer(w, h);
    build_wordwrap_paragraphs(&mut r, w, h);
    r.render();
    r
}

/// Settled culling scrollbox scrolled mid-content (public for the
/// paint-sanity test).
#[must_use]
pub fn build_culling_tree(w: u32, h: u32, item_count: usize) -> CliRenderer {
    let mut r = make_renderer(w, h);
    build_culling_scrollbox(&mut r, w, h, item_count);
    r
}

// --- D. scenario fixtures (the differential trees) ---------------------------

/// Representative fixtures per shape (the full 74 live in
/// `tools/diff/scenarios`; these cover every family the oracle pins):
/// deep boxes, node trees, long word-wrapped text, CJK wrap, textarea
/// wrap, scrollbox variants, styled text, attr matrix, markdown + code.
const FIXTURES: &[&str] = &[
    "node-deep-attrs",
    "node-tree-nested",
    "word-wrap-edges",
    "wrap-char-cjk",
    "textarea-word-wrap",
    "scrollbox-static",
    "scrollbox-nested",
    "scrollbox-long-math",
    "styled-multichunk",
    "attr-matrix",
    "md-mixed",
    "md-fence-ts",
    "md-wrap-para",
    "code-ts",
];

fn suite_fixtures(rig: &mut Rig) -> anyhow::Result<()> {
    let dir = scenario_dir();
    for name in FIXTURES {
        let path = dir.join(format!("{name}.json"));
        let scenario = opentui_diff::scenario::load(&path)?;

        // cold mount: scenario load + full step replay + first frame —
        // the markdown/code fixtures include marked lexing + tree-sitter
        rig.run_slow(&format!("fixture/{name}/cold-mount"), |_| {
            let scenario =
                opentui_diff::scenario::load(black_box(&path)).expect("scenario re-load");
            let mut r = opentui_diff::scenario::replay(&scenario).expect("replay");
            black_box(r.render());
        });

        // steady-state re-render of the settled tree
        let mut r = opentui_diff::scenario::replay(&scenario)?;
        rig.run(&format!("fixture/{name}/steady"), |_| {
            black_box(r.render());
        });

        // dirty frames on scrollable fixtures: alternate scrollTop
        // between 0 and 1 (translate + repaint + SGR diff + settle)
        let scrollable = scenario
            .steps
            .iter()
            .find(|s| s.op == "scrollbox")
            .and_then(|s| s.id.clone());
        if let Some(sb) = scrollable {
            let mut r = opentui_diff::scenario::replay(&scenario)?;
            rig.run(&format!("fixture/{name}/scroll-dirty"), |i| {
                let top = if i % 2 == 0 { 0.0 } else { 1.0 };
                r.set_scroll_top(&sb, top);
            });
        }
    }
    Ok(())
}

// --- E. SGR frame emission ---------------------------------------------------

fn suite_sgr(rig: &mut Rig) -> anyhow::Result<()> {
    for &(w, h) in &SIZES {
        // forced full-repaint emission of a settled mixed tree — the
        // renderer.zig prepareRenderFrameWithWriter hot path isolated
        // from layout + paint
        let mut r = build_mixed_tree(w, h);
        let mut out = String::new();
        rig.run(&format!("sgr/full-frame/{w}x{h}"), |_| {
            r.bench_force_full_frame();
            out.clear();
            r.bench_emit_frame_sgr(&mut out);
            black_box(out.len());
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ceilings (--check): CI "no catastrophic regression" guards
// ---------------------------------------------------------------------------

/// (pattern, ceiling_ms) — FIRST match (substring) wins, most specific
/// first. Medians are compared (robust to scheduler spikes); the
/// ceilings are deliberately ~10x the dev-box medians because shared CI
/// runners are noisy and 2-5x slower. These are catastrophe guards,
/// not budgets — the frame budgets live in `tools/bench/README.md`.
const CEILINGS: &[(&str, f64)] = &[
    // the named interactive budget: full render cycle at 120x30 must
    // stay well under the 40ms TUI gate (opencode-rust) even on a
    // thrashing shared runner
    ("render/mixed/120x30", 50.0),
    ("render/first-frame", 500.0),
    ("/cold-mount", 1000.0),
    ("render/", 50.0),
    ("fixture/", 100.0),
    ("buffer/", 20.0),
    ("measure/", 20.0),
    ("sgr/", 20.0),
];

/// Returns the failed (name, median_ms, ceiling_ms) list — empty = pass.
#[must_use]
pub fn check_ceilings(samples: &[Sample]) -> Vec<(String, f64, f64)> {
    let mut failures = Vec::new();
    for s in samples {
        if s.median_ns == 0 {
            continue; // --list stubs
        }
        let ceiling = CEILINGS
            .iter()
            .find(|(pat, _)| s.name.contains(pat))
            .map(|(_, c)| *c);
        if let Some(ceiling) = ceiling {
            let median = s.median_ms();
            if median > ceiling {
                failures.push((s.name.clone(), median, ceiling));
            }
        }
    }
    failures
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

/// The rig main (bin + bench facade share it).
pub fn bench_main() -> anyhow::Result<()> {
    let mut filter = String::new();
    let mut quick = false;
    let mut check = false;
    let mut list = false;
    let mut json: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--filter" => filter = args.next().unwrap_or_default(),
            "--quick" => quick = true,
            "--check" => check = true,
            "--list" => list = true,
            "--json" => json = args.next().map(PathBuf::from),
            other => {
                anyhow::bail!("unknown flag {other} (use --filter/--quick/--check/--list/--json)")
            }
        }
    }

    let mut rig = Rig::new(&filter, quick, list);
    eprintln!("opentui-bench: filter={filter:?} quick={quick} list={list}");
    run_suites(&mut rig)?;

    if list {
        let mut names: Vec<String> = rig.samples.iter().map(|s| s.name.clone()).collect();
        names.sort();
        for n in names {
            println!("{n}");
        }
        println!("{} benchmarks", rig.samples.len());
        return Ok(());
    }

    // table
    println!("| benchmark | iterations | median | p95 | min |",);
    println!("|---|---|---|---|---|");
    let mut samples = rig.samples.clone();
    samples.sort_by(|a, b| a.name.cmp(&b.name));
    for s in &samples {
        println!(
            "| {} | {} | {:.3} ms | {:.3} ms | {:.3} ms |",
            s.name,
            s.iterations,
            s.median_ms(),
            s.p95_ns as f64 / 1e6,
            s.min_ns as f64 / 1e6,
        );
    }

    if let Some(path) = json {
        let doc = serde_json::to_string_pretty(&samples)?;
        std::fs::write(&path, doc)?;
        eprintln!("wrote {}", path.display());
    }

    if check {
        let failures = check_ceilings(&samples);
        if failures.is_empty() {
            println!("perf ceilings: PASS ({} samples)", samples.len());
        } else {
            for (name, median, ceiling) in &failures {
                eprintln!("perf ceiling EXCEEDED: {name} median {median:.3} ms > {ceiling:.0} ms");
            }
            anyhow::bail!("{} perf ceiling(s) exceeded", failures.len());
        }
    }
    Ok(())
}

// Small shims used above ------------------------------------------------------
// (BoxOptions helper impls live in-crate via free functions; nothing to
// see here.)

// `make_cell` is pub(crate) upstream; Cell's fields are public, so the
// set-path bench constructs the same shape directly.
use opentui_core::buffer::Cell;
