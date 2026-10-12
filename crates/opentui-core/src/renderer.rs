//! The renderer — `createCliRenderer` / `CliRenderer`
//! (`packages/core/src/renderer.ts:723-748`, native `renderer.zig`).
//!
//! The slice implements the **headless render target**: explicit width/
//! height (the harness profile — `createTestRenderer` passes fake streams
//! with `columns`/`rows`, renderer.ts:733-734), no terminal I/O. The frame
//! lifecycle mirrors upstream:
//!
//! 1. clear `nextRenderBuffer` with `backgroundColor` (default transparent
//!    black `fromInts(0,0,0,0)` — renderer.ts:819; cleared per-frame at the
//!    end of native `prepareRenderFrame`, renderer.zig:3052),
//! 2. `root.render(nextRenderBuffer)` — layout pass + paint
//!    (renderer.ts:4726, RootRenderable.render:1789+),
//! 3. swap current/next (the diff loop syncs current=next,
//!    renderer.zig:2833),
//! 4. [`render_frame_sgr`] emits the diff-based SGR byte stream
//!    (`prepareRenderFrameWithWriter`, renderer.zig:2584-2838).

use crate::buffer::OptimizedBuffer;
use crate::color::Rgba;
use crate::layout::Dimension;
use crate::renderable::{Content, MeasureCtx, MeasureKind, Renderable, RootRenderable};
use crate::renderables::box_::{BoxOptions, BoxRenderableState};
use crate::renderables::text::{MeasuredText, TextOptions, TextRenderableState};
use taffy::prelude as tp;
use taffy::tree::TaffyTree;

/// `CliRendererConfig` (renderer.ts:122-243) — the slice subset.
#[derive(Debug, Clone)]
pub struct CliRendererConfig {
    pub width: u32,
    pub height: u32,
    /// renderer.ts:217 `backgroundColor`
    pub background_color: Rgba,
}

impl Default for CliRendererConfig {
    fn default() -> Self {
        CliRendererConfig {
            width: 80,
            height: 24,
            background_color: Rgba::from_ints(0, 0, 0, 0),
        }
    }
}

/// Where output goes — the headless slice has two targets: the cell grid
/// (differential harness) and an in-memory SGR byte stream.
pub enum RenderTarget {
    Headless,
}

/// `CliRenderer` — the render loop owner.
pub struct CliRenderer {
    pub config: CliRendererConfig,
    tree: TaffyTree<MeasureCtx>,
    pub root: RootRenderable,
    pub next_render_buffer: OptimizedBuffer,
    pub current_render_buffer: OptimizedBuffer,
    /// SGR bytes emitted by the last [`CliRenderer::render`]
    last_frame_sgr: String,
    frame_id: u64,
}

impl Default for CliRenderer {
    fn default() -> Self {
        create_cli_renderer(CliRendererConfig::default())
    }
}

/// `createCliRenderer(config)` — renderer.ts:723-748. The upstream factory
/// awaits terminal setup; the headless port is synchronous and never
/// touches stdin/stdout.
#[must_use]
pub fn create_cli_renderer(config: CliRendererConfig) -> CliRenderer {
    let mut tree: TaffyTree<MeasureCtx> = TaffyTree::new();
    // rounding enabled by default in taffy — yoga's point-scale-1 behavior
    let root = RootRenderable::new(&mut tree, config.width, config.height);
    CliRenderer {
        config: config.clone(),
        tree,
        root,
        next_render_buffer: OptimizedBuffer::new(config.width, config.height),
        current_render_buffer: OptimizedBuffer::new(config.width, config.height),
        last_frame_sgr: String::new(),
        frame_id: 0,
    }
}

impl CliRenderer {
    /// `renderer.width` (ctx.width).
    #[must_use]
    pub fn width(&self) -> u32 {
        self.config.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// `renderer.root.add(child)` — mount a renderable under the root.
    pub fn add(&mut self, child: Renderable) {
        self.root.add(&mut self.tree, child);
    }

    /// Mount convenience (the reconciler path): construct + mount a text
    /// renderable in one step.
    pub fn add_text(&mut self, id: &str, opts: &TextOptions) {
        let r = self.make_text(id, opts);
        self.add(r);
    }

    /// Mount convenience: construct + mount a box renderable (borders
    /// feed the taffy node).
    pub fn add_box(&mut self, id: &str, opts: &BoxOptions) {
        let r = self.make_box(id, opts);
        self.add(r);
    }

    /// Construct a text renderable without mounting (nested mounting
    /// pairs this with [`CliRenderer::mount_child`]).
    #[must_use]
    pub fn make_text(&mut self, id: &str, opts: &TextOptions) -> Renderable {
        let state = TextRenderableState::from_options(opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
        };
        Renderable::new(
            &mut self.tree,
            id,
            opts.layout.clone(),
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Text(state),
            Some(ctx),
        )
    }

    /// Construct a box renderable without mounting.
    #[must_use]
    pub fn make_box(&mut self, id: &str, opts: &BoxOptions) -> Renderable {
        let state = BoxRenderableState::from_options(opts);
        let mut layout = opts.layout.clone();
        state.apply_yoga_borders(&mut layout);
        Renderable::new(
            &mut self.tree,
            id,
            layout,
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Box(state),
            None,
        )
    }

    /// `requestRender` → `loop()` tick: one deterministic frame.
    pub fn render(&mut self) -> u64 {
        // 1. clear next with the background color
        self.next_render_buffer.clear(self.config.background_color);

        // 2. layout: root calculateLayout(width, height) —
        //    RootRenderable.render pass 1 (Renderable.ts:1798-1802, 1865-67)
        let root_node = self.root.inner.node;
        let available = tp::Size {
            width: tp::AvailableSpace::Definite(self.config.width as f32),
            height: tp::AvailableSpace::Definite(self.config.height as f32),
        };
        self.tree
            .compute_layout_with_measure(root_node, available, Self::measure_text)
            .expect("taffy layout");

        // pass 2: update layout + screen coords
        self.root.inner.update_layout(&self.tree, (0, 0));

        // pass 3: paint into next buffer
        self.root.inner.paint(&mut self.next_render_buffer);

        // 3. SGR emission (diff current vs next) — before the swap
        let mut sgr = String::new();
        self.emit_frame_sgr(&mut sgr);
        self.last_frame_sgr = sgr;

        // swap: current := next (renderer.zig syncs cell-by-cell; a frame
        // ends with current == next)
        core::mem::swap(
            &mut self.current_render_buffer,
            &mut self.next_render_buffer,
        );

        self.frame_id += 1;
        self.frame_id
    }

    /// Mount a renderable under an arbitrary node (nested `add`).
    /// Returns false when `parent_id` is not in the tree.
    pub fn mount_child(&mut self, parent_id: &str, child: Renderable) -> bool {
        let Self { tree, root, .. } = self;
        crate::renderable::mount_under(&mut root.inner, parent_id, tree, child).is_none()
    }

    /// The taffy measure function: Text leaves measure their wrapped
    /// content; everything else uses taffy defaults.
    fn measure_text(
        known_dimensions: tp::Size<Option<f32>>,
        available_space: tp::Size<tp::AvailableSpace>,
        _node_id: tp::NodeId,
        ctx: Option<&mut MeasureCtx>,
        _style: &tp::Style,
    ) -> tp::Size<f32> {
        let Some(ctx) = ctx else {
            return tp::Size::ZERO;
        };
        if ctx.kind != MeasureKind::Text {
            return tp::Size::ZERO;
        }
        let text = ctx.text.clone().unwrap_or(MeasuredText {
            content: String::new(),
            wrap_mode: Default::default(),
        });
        let avail_w = match available_space.width {
            tp::AvailableSpace::Definite(w) => w,
            tp::AvailableSpace::MaxContent => f32::INFINITY,
            tp::AvailableSpace::MinContent => 0.0,
        };
        let (w, h) = crate::renderables::text::TextRenderableState::measure(
            &text,
            known_dimensions.width,
            avail_w,
        );
        tp::Size {
            width: w,
            height: h,
        }
    }

    /// The SGR frame writer — `prepareRenderFrameWithWriter` simplified to
    /// the full-repaint first frame + diffed subsequent frames with
    /// row-equality fast path (renderer.zig:2650-2838).
    fn emit_frame_sgr(&mut self, out: &mut String) {
        let width = self.config.width;
        let height = self.config.height;
        let force = self.frame_id == 0;
        let mut current_fg: Option<Rgba> = None;
        let mut current_bg: Option<Rgba> = None;
        let mut current_attrs: Option<u32> = None;
        let mut run_start: i64 = -1;
        let mut run_length: u32 = 0;

        for y in 0..height {
            // row-equality fast path (renderer.zig:2655-2662)
            if !force {
                let mut equal = true;
                for x in 0..width {
                    match (
                        self.current_render_buffer.get(x, y),
                        self.next_render_buffer.get(x, y),
                    ) {
                        (Some(a), Some(b)) if a == b => {}
                        _ => {
                            equal = false;
                            break;
                        }
                    }
                }
                if equal {
                    continue;
                }
            }
            for x in 0..width {
                let (Some(cur), Some(next)) = (
                    self.current_render_buffer.get(x, y),
                    self.next_render_buffer.get(x, y),
                ) else {
                    continue;
                };
                if !force && cur == next {
                    if run_length > 0 {
                        out.push_str(crate::ansi::ansi_consts::RESET);
                        run_start = -1;
                        run_length = 0;
                    }
                    continue;
                }
                let fg_match = current_fg.is_some_and(|c| c == next.fg);
                let bg_match = current_bg.is_some_and(|c| c == next.bg);
                let same_attrs = fg_match && bg_match && current_attrs == Some(next.attributes);
                if !same_attrs || run_start == -1 {
                    if run_length > 0 {
                        out.push_str(crate::ansi::ansi_consts::RESET);
                    }
                    run_start = i64::from(x);
                    run_length = 0;
                    current_fg = Some(next.fg);
                    current_bg = Some(next.bg);
                    current_attrs = Some(next.attributes);
                    crate::ansi::move_to(out, x + 1, y + 1);
                    crate::ansi::emit_color(out, next.fg, false);
                    crate::ansi::emit_color(out, next.bg, true);
                    crate::ansi::TextAttributes::apply_attributes(out, next.attributes);
                }
                // print the cell's text
                if crate::buffer::is_continuation_char(next.char) {
                    // no bytes; cursor advances implicitly
                } else if crate::buffer::is_grapheme_char(next.char) {
                    out.push_str(
                        self.next_render_buffer
                            .grapheme_bytes(crate::buffer::grapheme_id_from_char(next.char)),
                    );
                } else if let Some(c) = char::from_u32(next.char) {
                    out.push(c);
                }
                run_length += 1;
            }
        }
        if run_length > 0 {
            out.push_str(crate::ansi::ansi_consts::RESET);
        }
    }

    /// SGR bytes of the last rendered frame (harness observability).
    #[must_use]
    pub fn last_frame_sgr(&self) -> &str {
        &self.last_frame_sgr
    }

    #[must_use]
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    /// Update a text leaf's measure context after content changes
    /// (`content` setter → updateTextBuffer, Text.ts:75-83).
    pub fn refresh_text_measure(&mut self, id: &str) {
        let Some(node) = self.find_node(&self.root.inner, id) else {
            return;
        };
        if let Content::Text(state) = &node.content {
            let m = state.measured();
            let n = node.node;
            if let Some(ctx) = self.tree.get_node_context_mut(n) {
                ctx.text = Some(m);
            }
            let _ = self.tree.mark_dirty(n);
        }
    }

    fn find_node<'a>(&self, root: &'a Renderable, id: &str) -> Option<&'a Renderable> {
        if root.id.0 == id {
            return Some(root);
        }
        root.children.iter().find_map(|c| self.find_node(c, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dimension, LayoutOptions};
    use crate::renderable::{Content, Renderable};
    use crate::renderables::box_::BoxOptions;
    use crate::renderables::text::TextOptions;

    fn text_renderable(id: &str, content: &str) -> Renderable {
        let opts = TextOptions {
            content: content.into(),
            ..Default::default()
        };
        let state = crate::renderables::text::TextRenderableState::from_options(&opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
        };
        Renderable::new(
            &mut TaffyTree::new(),
            id,
            LayoutOptions::default(),
            Dimension::Auto,
            Dimension::Auto,
            Content::Text(state),
            Some(ctx),
        )
    }

    // helper: build with a real renderer-owned tree
    fn add_text(renderer: &mut CliRenderer, id: &str, content: &str) {
        let opts = TextOptions {
            content: content.into(),
            ..Default::default()
        };
        let state = crate::renderables::text::TextRenderableState::from_options(&opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
        };
        let r = Renderable::new(
            &mut renderer.tree,
            id,
            LayoutOptions::default(),
            Dimension::Auto,
            Dimension::Auto,
            Content::Text(state),
            Some(ctx),
        );
        renderer.add(r);
    }

    fn add_box(renderer: &mut CliRenderer, id: &str, opts: BoxOptions) {
        let state = crate::renderables::box_::BoxRenderableState::from_options(&opts);
        let mut layout = opts.layout.clone();
        state.apply_yoga_borders(&mut layout);
        let r = Renderable::new(
            &mut renderer.tree,
            id,
            layout,
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Box(state),
            None,
        );
        renderer.add(r);
    }

    #[test]
    fn hello_smoke_renders_text_at_origin() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 5,
            ..Default::default()
        });
        add_text(&mut renderer, "t1", "hello");
        renderer.render();
        let frame = renderer.current_render_buffer.char_frame();
        let rows: Vec<&str> = frame.split('\n').collect();
        assert_eq!(rows[0].trim_end(), "hello");
        // fg = literal white, bg transparent black (TextBufferRenderable defaults)
        let spans = renderer.current_render_buffer.get_span_lines();
        let s0 = &spans[0].spans[0];
        assert_eq!(s0.text.trim_end(), "hello");
        assert_eq!(s0.fg, Rgba::from_values(1.0, 1.0, 1.0, 1.0));
        assert_eq!(s0.bg.alpha(), 0);
    }

    #[test]
    fn sgr_stream_contains_text_and_colors() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 10,
            height: 3,
            ..Default::default()
        });
        add_text(&mut renderer, "t1", "hi");
        renderer.render();
        let sgr = renderer.last_frame_sgr().to_owned();
        // first frame is a full repaint: contains CUP + fg SGR + text + reset
        assert!(sgr.contains("\x1b[1;1H"));
        assert!(sgr.contains("hi"));
        assert!(sgr.contains("\x1b[38;2;255;255;255m"));
        assert!(sgr.ends_with("\x1b[0m"));
        // second frame with no changes emits nothing (no-op suppression)
        renderer.render();
        assert_eq!(renderer.last_frame_sgr(), "");
    }

    #[test]
    fn box_lays_out_children_in_column() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 10,
            ..Default::default()
        });
        add_box(
            &mut renderer,
            "b1",
            BoxOptions {
                border: Some(crate::renderables::box_::BorderSpec::All),
                width: Some(Dimension::Points(10.0)),
                height: Some(Dimension::Points(5.0)),
                ..Default::default()
            },
        );
        renderer.render();
        let frame = renderer.current_render_buffer.char_frame();
        let rows: Vec<&str> = frame.split('\n').collect();
        assert_eq!(rows[0].trim_end(), "┌────────┐");
        assert_eq!(rows[1].trim_end(), "│        │");
        assert_eq!(rows[4].trim_end(), "└────────┘");
    }

    #[test]
    fn flex_grow_splits_space() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 3,
            ..Default::default()
        });
        let layout = LayoutOptions {
            flex_direction: Some("row".into()),
            ..Default::default()
        };
        let b1_layout = LayoutOptions {
            flex_grow: Some(1.0),
            ..Default::default()
        };
        let b1 = Renderable::new(
            &mut renderer.tree,
            "b1",
            b1_layout,
            Dimension::Auto,
            Dimension::Points(1.0),
            Content::Box(crate::renderables::box_::BoxRenderableState::from_options(
                &BoxOptions {
                    background_color: Some("#FF0000".into()),
                    should_fill: Some(true),
                    ..Default::default()
                },
            )),
            None,
        );
        renderer.root.inner.layout = layout;
        let mut root_style = tp::Style {
            flex_direction: tp::FlexDirection::Row,
            ..Default::default()
        };
        // keep the root's terminal size (RootRenderable sets it; a bare
        // default Style would wipe width/height to AUTO)
        root_style.size.width = tp::Dimension::length(20.0);
        root_style.size.height = tp::Dimension::length(3.0);
        let _ = renderer
            .tree
            .set_style(renderer.root.inner.node, root_style);
        renderer.add(b1);
        renderer.render();
        // flexGrow 1 in a 20-wide row → fills the row with red
        let cell = renderer.current_render_buffer.get(19, 0).unwrap();
        assert_eq!(cell.bg, Rgba::rgb(255, 0, 0));
    }

    #[test]
    fn standalone_text_renderable_helper_works() {
        let r = text_renderable("t", "x");
        assert_eq!(r.id.0, "t");
    }
}
