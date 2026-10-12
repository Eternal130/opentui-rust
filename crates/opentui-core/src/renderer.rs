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
use crate::renderable::{
    Content, MeasureCtx, MeasureKind, Renderable, RootRenderable, WalkEffects,
};
use crate::renderables::box_::{BoxOptions, BoxRenderableState};
use crate::renderables::scrollbox;
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
    pub tree: TaffyTree<MeasureCtx>,
    pub root: RootRenderable,
    pub next_render_buffer: OptimizedBuffer,
    pub current_render_buffer: OptimizedBuffer,
    /// SGR bytes emitted by the last [`CliRenderer::render`]
    last_frame_sgr: String,
    /// every frame's SGR concatenated — the full terminal emission
    /// (upstream's stdout stream; the differential harness replays it)
    sgr_history: String,
    frame_id: u64,
    /// scrollbox root id → content node id (`ScrollBox.add` routes user
    /// children into the content viewport, ScrollBox.ts:514-516)
    content_of: std::collections::HashMap<String, String>,
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
        sgr_history: String::new(),
        frame_id: 0,
        content_of: std::collections::HashMap::new(),
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
        self.layout_pass();

        // pass 2: update layout + screen coords + inline size hooks
        // (effects drive re-layout frames exactly like upstream's
        // requestRender scheduling — the observed 0.5.14 settle sequence:
        // frame A lays out with both bars visible, the recalc hides the
        // horizontal bar, frame B re-lays the viewport one row taller)
        let mut effects = WalkEffects::default();
        self.root.inner.update_layout(
            &self.tree,
            (0.0, 0.0),
            (f32::INFINITY, f32::INFINITY),
            &mut effects,
        );
        self.apply_recalcs(&mut effects);
        let mut guard = 0;
        while effects.any() && guard < 16 {
            self.layout_pass();
            effects = WalkEffects::default();
            self.root.inner.update_layout(
                &self.tree,
                (0.0, 0.0),
                (f32::INFINITY, f32::INFINITY),
                &mut effects,
            );
            if std::env::var("OT_DBG").is_ok() {
                let vp = self
                    .root
                    .inner
                    .find("scroll-box-viewport-0")
                    .map(|n| format!("({},{})", n.w, n.h))
                    .unwrap_or_default();
                eprintln!("[walk{}] vp={}", guard + 2, vp);
            }
            self.apply_recalcs(&mut effects);
            guard += 1;
        }

        // pass 3: paint into next buffer
        self.root.inner.paint(&mut self.next_render_buffer);

        // 3. SGR emission (diff current vs next) — before the swap
        let mut sgr = String::new();
        self.emit_frame_sgr(&mut sgr);
        self.sgr_history.push_str(&sgr);
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

    fn layout_pass(&mut self) {
        let root_node = self.root.inner.node;
        let available = tp::Size {
            width: tp::AvailableSpace::Definite(self.config.width as f32),
            height: tp::AvailableSpace::Definite(self.config.height as f32),
        };
        self.tree
            .compute_layout_with_measure(root_node, available, Self::measure_text)
            .expect("taffy layout");
    }

    /// Apply size-change hook events in fire order — each is a
    /// `recalculateBarProps` run with the at-fire-time sizes
    /// (ScrollBox.ts:749-805).
    fn apply_recalcs(&mut self, effects: &mut WalkEffects) {
        let Self { root, tree, .. } = self;
        for event in std::mem::take(&mut effects.events) {
            scrollbox::apply_recalc(
                &mut root.inner,
                tree,
                &event.owner,
                event.stale_content_size,
                effects,
            );
        }
    }

    /// Mount a renderable under an arbitrary node (nested `add`).
    /// Returns false when `parent_id` is not in the tree. ScrollBox ids
    /// route into the content viewport (`ScrollBox.add`, ScrollBox.ts:514-516).
    pub fn mount_child(&mut self, parent_id: &str, child: Renderable) -> bool {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        crate::renderable::mount_under(&mut root.inner, &target, tree, child).is_none()
    }

    /// Construct a ScrollBox subtree without mounting (children added via
    /// [`CliRenderer::mount_child`] land in its content).
    #[must_use]
    pub fn make_scrollbox(
        &mut self,
        id: &str,
        opts: &mut scrollbox::ScrollBoxOptions,
    ) -> Renderable {
        let r = scrollbox::build_scrollbox(&mut self.tree, id, opts);
        let content_id = match &r.content {
            Content::ScrollBox(_, state) => state.content_id.clone(),
            _ => unreachable!("build_scrollbox yields ScrollBox root"),
        };
        self.content_of.insert(id.to_owned(), content_id);
        r
    }

    /// `scrollBox.scrollTop = value` (ScrollBox.ts:168-171) followed by a
    /// settle render — the harness's scripted-mutation step.
    pub fn set_scroll_top(&mut self, owner: &str, value: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::set_scroll_top(&mut root.inner, tree, owner, value, &mut effects);
        let _ = effects;
        self.render();
    }

    pub fn set_scroll_left(&mut self, owner: &str, value: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::set_scroll_left(&mut root.inner, tree, owner, value, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollBy(delta, unit)` (ScrollBox.ts:404-413).
    pub fn scroll_by(
        &mut self,
        owner: &str,
        dx: f32,
        dy: f32,
        unit: crate::renderables::scrollbar::ScrollUnit,
    ) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_by(&mut root.inner, tree, owner, dx, dy, unit, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollTo({x, y})` (ScrollBox.ts:464-473).
    pub fn scroll_to(&mut self, owner: &str, x: f32, y: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_to(&mut root.inner, tree, owner, x, y, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollChildIntoView(childId)` (ScrollBox.ts:415-462).
    pub fn scroll_child_into_view(&mut self, owner: &str, child_id: &str) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_child_into_view(&mut root.inner, tree, owner, child_id, &mut effects);
        let _ = effects;
        self.render();
    }

    /// Wheel-event seam (`onMouseEvent` scroll branch, ScrollBox.ts:542-584).
    pub fn scroll_event(&mut self, owner: &str, event: &scrollbox::ScrollEvent) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::on_scroll_event(&mut root.inner, tree, owner, event, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `renderer.resize(w, h)` (renderer.ts:4073-4076 → processResize):
    /// new buffers, root re-sized; cached node sizes PERSIST (upstream
    /// `_widthValue` keeps the last size across resizes, Renderable.ts:1096-1119)
    /// so only genuinely-resized nodes fire their size hooks.
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.next_render_buffer = OptimizedBuffer::new(width, height);
        self.current_render_buffer = OptimizedBuffer::new(width, height);
        // RootRenderable size is fixed at construction (Renderable.ts:1743+)
        let node = self.root.inner.node;
        let mut style = self.tree.style(node).expect("root style").clone();
        style.size.width = tp::Dimension::length(width as f32);
        style.size.height = tp::Dimension::length(height as f32);
        let _ = self.tree.set_style(node, style);
        let _ = self.tree.mark_dirty(node);
        self.render();
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

    /// The FULL SGR emission across all rendered frames — what a terminal
    /// consumed (the harness's vt-replay validates this end-to-end).
    #[must_use]
    pub fn full_sgr_stream(&self) -> &str {
        &self.sgr_history
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

    // --- scroll family -----------------------------------------------------

    mod scroll {
        use super::*;
        use crate::renderables::scrollbar::ScrollUnit;
        use crate::renderables::scrollbox::{
            self, ScrollBoxOptions, ScrollDirection, ScrollEvent, StickyStart,
        };
        use crate::scroll_acceleration::MacOSScrollAccel;

        fn mounted_scrollbox() -> CliRenderer {
            let mut renderer = create_cli_renderer(CliRendererConfig {
                width: 20,
                height: 8,
                ..Default::default()
            });
            let mut opts = ScrollBoxOptions {
                scroll_y: true,
                ..Default::default()
            };
            opts.box_options.width = Some(Dimension::Points(20.0));
            opts.box_options.height = Some(Dimension::Points(8.0));
            let sb = renderer.make_scrollbox("sb", &mut opts);
            renderer.add(sb);
            for i in 0..10 {
                let t =
                    text_renderable_node(&mut renderer, &format!("t{i}"), &format!("line-{i:02}"));
                renderer.mount_child("sb", t);
            }
            renderer.render();
            renderer
        }

        fn text_renderable_node(renderer: &mut CliRenderer, id: &str, content: &str) -> Renderable {
            let opts = TextOptions {
                content: content.into(),
                ..Default::default()
            };
            renderer.make_text(id, &opts)
        }

        fn scroll_top_of(renderer: &CliRenderer) -> f32 {
            scrollbox::scroll_top(&renderer.root.inner, "sb").unwrap()
        }

        fn translate_y_of(renderer: &CliRenderer) -> f32 {
            renderer
                .root
                .inner
                .find("scroll-box-content-0")
                .unwrap()
                .translate_y
        }

        #[test]
        fn settles_with_visible_vertical_bar() {
            // the oracle-pinned settled state: scrollSize 10, viewportSize
            // 8, slider max 2 / viewPortSize 3 (path-dependent transient)
            let r = mounted_scrollbox();
            assert_eq!(scroll_top_of(&r), 0.0);
            assert_eq!(translate_y_of(&r), 0.0);
            let vp = r.root.inner.find("scroll-box-viewport-0").unwrap();
            assert_eq!((vp.w, vp.h), (19.0, 8.0));
            let bar = r
                .root
                .inner
                .find("scroll-box-vertical-scrollbar-0")
                .unwrap();
            let crate::renderable::Content::ScrollBar(b) = &bar.content else {
                panic!("bar state")
            };
            assert_eq!((b.scroll_size, b.viewport_size), (10.0, 8.0));
            assert!(bar.visible);
            let hbar = r
                .root
                .inner
                .find("scroll-box-horizontal-scrollbar-0")
                .unwrap();
            assert!(!hbar.visible);
        }

        #[test]
        fn scroll_top_clamps_and_translates() {
            let mut r = mounted_scrollbox();
            r.set_scroll_top("sb", 3.0);
            // clamp to max = 10 - 8 = 2 (ScrollBar.ts:72-74)
            assert_eq!(scroll_top_of(&r), 2.0);
            assert_eq!(translate_y_of(&r), -2.0);
            r.set_scroll_top("sb", f32::MIN);
            assert_eq!(scroll_top_of(&r), 0.0);
            assert_eq!(translate_y_of(&r), 0.0);
        }

        #[test]
        fn scroll_by_viewport_units() {
            // ScrollBar.ts:246-258 — viewport unit multiplies by viewportSize
            let mut r = mounted_scrollbox();
            r.scroll_by("sb", 0.0, 0.25, ScrollUnit::Viewport);
            assert_eq!(scroll_top_of(&r), 2.0); // 0.25 × 8 = 2, clamped
            r.scroll_by("sb", 0.0, -0.25, ScrollUnit::Viewport);
            assert_eq!(scroll_top_of(&r), 0.0);
        }

        #[test]
        fn sticky_bottom_applies_on_first_layout() {
            let mut renderer = create_cli_renderer(CliRendererConfig {
                width: 20,
                height: 8,
                ..Default::default()
            });
            let mut opts = ScrollBoxOptions {
                scroll_y: true,
                sticky_scroll: true,
                sticky_start: Some(StickyStart::Bottom),
                ..Default::default()
            };
            opts.box_options.width = Some(Dimension::Points(20.0));
            opts.box_options.height = Some(Dimension::Points(8.0));
            let sb = renderer.make_scrollbox("sb", &mut opts);
            renderer.add(sb);
            for i in 0..10 {
                let t =
                    text_renderable_node(&mut renderer, &format!("t{i}"), &format!("sline-{i:02}"));
                renderer.mount_child("sb", t);
            }
            renderer.render();
            // applyStickyStart → max(0, 10-8) = 2 (ScrollBox.ts:257)
            assert_eq!(scroll_top_of(&renderer), 2.0);
            assert_eq!(translate_y_of(&renderer), -2.0);
        }

        #[test]
        fn wheel_seam_accumulates_and_uses_accel_curve() {
            // ScrollBox.ts:542-584 via MacOSScrollAccel: fractional deltas
            // accumulate; only integer parts scroll
            let mut r = mounted_scrollbox();
            r.scroll_event(
                "sb",
                &ScrollEvent {
                    direction: ScrollDirection::Down,
                    delta: 0.6, // LinearAccel ⇒ multiplier 1 (first tick)
                    shift: false,
                    now_ms: Some(1000.0),
                },
            );
            // 0.6 truncates to 0 → no scroll, accumulator keeps 0.6
            assert_eq!(scroll_top_of(&r), 0.0);
            r.scroll_event(
                "sb",
                &ScrollEvent {
                    direction: ScrollDirection::Down,
                    delta: 0.6,
                    shift: false,
                    now_ms: Some(1100.0),
                },
            );
            // 1.2 → trunc 1 → scrollTop 1
            assert_eq!(scroll_top_of(&r), 1.0);
            // shift remaps down → right (ScrollBox.ts:545-546)
            r.scroll_event(
                "sb",
                &ScrollEvent {
                    direction: ScrollDirection::Down,
                    delta: 5.0,
                    shift: true,
                    now_ms: Some(1200.0),
                },
            );
            let left = scrollbox::scroll_left(&r.root.inner, "sb").unwrap();
            assert_eq!(left, 0.0); // horizontal scroll clamps (content fits)
        }

        #[test]
        fn macos_accel_scales_wheel_deltas() {
            // burst ticks: multiplier > 1 → each delta pushes several cells
            // (here it saturates the 2-row scroll range and leaves residue
            // in the accumulator — the linear curve would leave < 1)
            let mut r = mounted_scrollbox();
            if let Some(sb) = r.root.inner.find_mut("sb") {
                if let crate::renderable::Content::ScrollBox(_, st) = &mut sb.content {
                    st.scroll_accel = std::sync::Mutex::new(Box::new(MacOSScrollAccel::default()));
                }
            }
            for t in [1000.0, 1020.0, 1040.0] {
                r.scroll_event(
                    "sb",
                    &ScrollEvent {
                        direction: ScrollDirection::Down,
                        delta: 1.0,
                        shift: false,
                        now_ms: Some(t),
                    },
                );
            }
            // dt=20ms ⇒ velocity 5 ⇒ multiplier 1 + 0.8·(e^{5/3} − 1) ≈ 4.4;
            // the range (2) saturates and the accumulator holds the excess
            assert_eq!(scroll_top_of(&r), 2.0);
            let residue = {
                let sb = r.root.inner.find("sb").unwrap();
                let crate::renderable::Content::ScrollBox(_, st) = &sb.content else {
                    unreachable!()
                };
                st.scroll_accumulator_y
            };
            assert!(
                residue > 0.5,
                "accelerated residue: {residue} (linear would be 0)"
            );
        }
    }
}
