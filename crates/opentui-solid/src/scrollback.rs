//! `createScrollbackWriter` — the split-footer scrollback write path
//! (upstream `scrollback.ts:145-227`; v2's mini uses it).
//!
//! Upstream: build a `BoxRenderable` root (`position: absolute`,
//! `left/top: 0`, fixed width, `height: "auto"`, `border: false`,
//! `backgroundColor: "transparent"`, `shouldFill: false`,
//! `flexDirection: "column"` — scrollback.ts:158-169), render the
//! node fn against a SYNTHETIC renderer view of the root (width +
//! height signals over the snapshot context, :47-94), measure the
//! height (`measureSnapshotHeight`: fresh RootRenderable +
//! calculateLayout, :108-122), converging over up to 4 passes
//! (`MAX_AUTO_HEIGHT_PASSES`, :23, 124-143), and hand the snapshot
//! `{ root, width, height, rowColumns, startOnNewLine,
//! trailingNewline, teardown }` to the host's
//! `renderer.writeToScrollback`.
//!
//! The Rust port keeps the same writer SHAPE: the scratch
//! `SolidRenderer` that owns the painted tree travels with the
//! snapshot (core `Renderable`s are bound to their taffy tree), and
//! [`ScrollbackSnapshot::paint_lines`] replaces the native
//! scrollback emission (paint the tree to a width×height buffer and
//! return the text lines — what the TUI host writes above the
//! split footer).

use crate::element::Element;
use crate::reconcile::SolidRenderer;
use opentui_core::renderer::CliRendererConfig;

/// `SolidScrollbackWriterOptions` (scrollback.ts:25-31).
#[derive(Debug, Clone, Default)]
pub struct SolidScrollbackWriterOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub row_columns: Option<u32>,
    pub start_on_new_line: Option<bool>,
    pub trailing_newline: Option<bool>,
}

/// `ScrollbackRenderContext` (renderer.ts:295-300) — the slice the
/// writer consumes.
pub struct ScrollbackRenderContext {
    pub width: u32,
    pub tail_column: u32,
}

/// `ScrollbackSnapshot` (renderer.ts:304-313) — `root` here is the
/// scratch renderer owning the painted tree (see the module doc).
pub struct ScrollbackSnapshot {
    pub renderer: SolidRenderer,
    /// The snapshot root's live id inside `renderer`.
    pub root_id: String,
    pub width: u32,
    pub height: u32,
    pub row_columns: Option<u32>,
    pub start_on_new_line: bool,
    pub trailing_newline: bool,
}

impl ScrollbackSnapshot {
    /// The measured + settled height (post `resolveSnapshotHeight`).
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Paint the snapshot tree to text lines — the bytes the host
    /// writes into the scrollback region (`teardown` upstream; the
    /// drop of the scratch renderer is the port's teardown).
    #[must_use]
    pub fn paint_lines(&self) -> Vec<String> {
        let buf = &self.renderer.renderer.current_render_buffer;
        let frame = buf.char_frame();
        let mut lines: Vec<String> = frame.split('\n').map(str::to_owned).collect();
        lines.truncate(self.height as usize);
        lines
    }
}

/// `createScrollbackWriter(node, options)` (scrollback.ts:145-219):
/// returns the writer fn the host invokes per scrollback commit.
pub fn create_scrollback_writer(
    node: impl Fn() -> Element + 'static,
    options: SolidScrollbackWriterOptions,
) -> ScrollbackWriter {
    ScrollbackWriter {
        node: Box::new(node),
        options,
    }
}

/// `ScrollbackWriter = (ctx) => ScrollbackSnapshot`
/// (renderer.ts:314).
pub struct ScrollbackWriter {
    node: Box<dyn Fn() -> Element>,
    options: SolidScrollbackWriterOptions,
}

impl ScrollbackWriter {
    /// The writer invocation (scrollback.ts:149-219).
    #[must_use]
    pub fn write(&self, ctx: &ScrollbackRenderContext) -> ScrollbackSnapshot {
        // normalizeSnapshotDimension (scrollback.ts:35-44)
        let width = self
            .options
            .width
            .map(|w| w.max(1))
            .unwrap_or_else(|| ctx.width.max(1));
        let height_opt = self.options.height.map(|h| h.max(1));
        let start_on_new_line = self.options.start_on_new_line.unwrap_or(true);
        // firstLineWidth / firstLineOffset (:153-157) — recorded for
        // the host (the port's paint is full-width; the offset shapes
        // the host's first-line splice)
        let first_line_width =
            if !start_on_new_line && ctx.tail_column > 0 && ctx.tail_column < ctx.width {
                width.min(ctx.width - ctx.tail_column)
            } else {
                width
            };
        let _first_line_offset = width - first_line_width;

        // the snapshot root (scrollback.ts:158-169), mounted through
        // the reconciler so the shadow + live trees stay aligned; the
        // node fn renders under it (the synthetic renderer view,
        // scrollback.ts:47-94)
        let mut sr = SolidRenderer::new(CliRendererConfig {
            width,
            height: height_opt.unwrap_or_else(|| ctx.width.max(1)).max(1),
            ..Default::default()
        });
        let root_key = "solid-scrollback-root".to_owned();
        let root_element = crate::builder::box_()
            .key(root_key.clone())
            .position("absolute")
            .left(0.0)
            .top(0.0)
            .width(width as f32)
            .border(false)
            .background_color("transparent")
            .should_fill(false)
            .flex_direction("column");
        let root_element = if let Some(h) = height_opt {
            root_element.height(h as f32)
        } else {
            root_element // height "auto"
        };
        sr.mount(SolidRenderer::ROOT, usize::MAX, root_element.into_element());
        let node = (self.node)();
        sr.mount(&root_key, usize::MAX, node);
        sr.flush();

        // resolveSnapshotHeight (scrollback.ts:124-143): measure,
        // re-render at the measured height, up to 4 passes
        let mut measured = measure_height(&sr, &root_key);
        let mut passes = 0;
        while height_opt.is_none() && passes < 4 {
            let current = sr.renderer.height();
            if measured == current {
                break;
            }
            sr.renderer.resize(width, measured);
            sr.flush();
            measured = measure_height(&sr, &root_key);
            passes += 1;
        }

        ScrollbackSnapshot {
            renderer: sr,
            root_id: root_key,
            width,
            height: height_opt.unwrap_or(measured.max(1)),
            row_columns: self.options.row_columns,
            start_on_new_line,
            trailing_newline: self.options.trailing_newline.unwrap_or(true),
        }
    }
}

/// `measureSnapshotHeight` (scrollback.ts:108-122): the root's
/// computed layout height, clamped ≥ 1.
fn measure_height(sr: &SolidRenderer, root_key: &str) -> u32 {
    sr.renderer
        .root
        .inner
        .find(root_key)
        .map_or(1, |n| n.h.max(1.0) as u32)
}
