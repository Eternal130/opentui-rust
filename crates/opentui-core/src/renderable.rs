//! The renderable tree — port of `packages/core/src/Renderable.ts`.
//!
//! Upstream is class-based (`BaseRenderable` → `Renderable` → concrete
//! renderables) with a yoga node per layout-enabled renderable. The Rust
//! port keeps one struct with a [`Content`] enum; the taffy tree lives in
//! the renderer and nodes are linked by id.
//!
//! Ported semantics (cited):
//! * options → style mapping incl. the conditional flexShrink default
//!   (Renderable.ts:724-737) — see [`crate::layout`].
//! * `updateLayout` screen-coordinate propagation: `_screenX =
//!   parent._screenX + _x + _translateX` (Renderable.ts:526, 1107-1109);
//!   translations update the absolute cache immediately (Renderable.ts:523-543).
//! * size clamp: `Math.max(layout.width, 1)` (Renderable.ts:1106-1107) for
//!   VISIBLE nodes — `updateLayout` early-returns on `!visible`
//!   (Renderable.ts:1386), and `visible` maps to yoga `display: flex|none`
//!   (Renderable.ts:315, 351).
//! * render walk: 3-pass — root `calculateLayout` (Renderable.ts:1866),
//!   collect render list, then `renderSelf` per renderable
//!   (Renderable.ts:1829-1848; RootRenderable.render:1789-1850).
//! * scissor: an `overflow != visible` node with positive size pushes its
//!   scissor rect around its CHILDREN (the node itself renders unclipped)
//!   — Renderable.ts:1425-1432, 1462; the Box override insets by its
//!   borders (Box.ts:296-315).

use crate::buffer::OptimizedBuffer;
use crate::layout::{taffy_style, Dimension, LayoutOptions};
use crate::renderables::box_::BoxRenderableState;
use crate::renderables::scrollbar::{ArrowState, ScrollBarState, SliderState};
use crate::renderables::scrollbox::{ScrollBoxState, ViewportCulling};
use crate::renderables::text::TextRenderableState;
use crate::renderables::textarea::{MeasuredEditor, TextareaState};
use taffy::prelude as tp;
use taffy::tree::TaffyTree;

/// Per-node measure context (taffy `NodeContext`): what the leaf measures
/// from. Text leaves carry their content; container leaves measure nothing.
#[derive(Debug, Clone, Default)]
pub struct MeasureCtx {
    pub kind: MeasureKind,
    /// wrapped text for Text leaves
    pub text: Option<crate::renderables::text::MeasuredText>,
    /// editor content for Textarea leaves
    pub editor: Option<MeasuredEditor>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MeasureKind {
    #[default]
    None,
    Text,
    /// `NativeMeasureTargetKind.EditorView` (zig.ts)
    Editor,
}

/// What a renderable paints.
#[allow(clippy::large_enum_variant)]
pub enum Content {
    /// Plain container (upstream `Renderable` base).
    Group,
    Box(BoxRenderableState),
    Text(TextRenderableState),
    /// `TextareaRenderable` — the editor (EditBufferRenderable.ts:85).
    Textarea(TextareaState),
    /// `SliderRenderable` (Slider.ts:20).
    Slider(SliderState),
    /// `ArrowRenderable` (ScrollBar.ts:320).
    Arrow(ArrowState),
    /// `ScrollBarRenderable`'s own state — the node still paints nothing
    /// itself (its slider/arrows are child renderables, ScrollBar.ts:178-180).
    ScrollBar(ScrollBarState),
    /// The ScrollBox's content node — a `ContentRenderable`
    /// (ScrollBox.ts:11-56): paints like a Box but culls its children to
    /// the viewport when enabled.
    ScrollContent(BoxRenderableState, ViewportCulling),
    /// `ScrollBoxRenderable` root — paints like a Box (it IS a
    /// BoxRenderable subclass, ScrollBox.ts:109) + carries the scroll
    /// machine state.
    ScrollBox(BoxRenderableState, ScrollBoxState),
}

/// `OverflowString` — Renderable.ts:71 ("visible" | "hidden" | "scroll");
/// option parse at Renderable.ts:762 (anything else → "visible").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Scroll,
}

impl Overflow {
    pub fn parse(s: &str) -> Overflow {
        match s {
            "hidden" => Overflow::Hidden,
            "scroll" => Overflow::Scroll,
            _ => Overflow::Visible,
        }
    }
}

/// Fires when a node's computed size changes mid-walk — upstream
/// `onLayoutResize → onSizeChange` (Renderable.ts:1128-1140). The ScrollBox
/// wires its viewport + content to `recalculateBarProps` (ScrollBox.ts:332,
/// 345-347). The event snapshots the sizes the recalc reads AT FIRE TIME —
/// upstream hooks run inline, so a viewport resize sees the content's
/// still-stale size (observed against the real 0.5.14 oracle).
#[derive(Debug, Clone, PartialEq)]
pub struct RecalcEvent {
    /// the owning ScrollBox root id
    pub owner: String,
    /// content (w,h) snapshotted at fire time — `Some` when the VIEWPORT
    /// fired (its content subtree is stale mid-walk)
    pub stale_content_size: Option<(f32, f32)>,
}

/// Effects collected during an update walk so the caller (renderer settle
/// loop) knows whether another frame is needed: display flips and
/// translate writes both `requestRender` upstream.
#[derive(Debug, Default, Clone)]
pub struct WalkEffects {
    pub display_flipped: bool,
    pub translated: bool,
    pub size_changed: bool,
    /// size-change hooks fired (applied in order after the walk)
    pub events: Vec<RecalcEvent>,
}

impl WalkEffects {
    pub fn any(&self) -> bool {
        self.display_flipped || self.translated || self.size_changed || !self.events.is_empty()
    }
}

/// Stable identity (upstream `id` option; auto ids `r_N`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderableId(pub String);

pub struct Renderable {
    pub id: RenderableId,
    pub node: tp::NodeId,
    pub layout: LayoutOptions,
    pub width: Dimension,
    pub height: Dimension,
    pub visible: bool,
    pub content: Content,
    pub children: Vec<Renderable>,
    /// `_translateX/_translateY` (Renderable.ts:230-231) — paint-only
    /// offset folded into the screen cache (523-543).
    pub translate_x: f32,
    pub translate_y: f32,
    /// `_overflow` (Renderable.ts:264, 762-764)
    pub overflow: Overflow,
    /// computed layout (post `updateLayout`)
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub screen_x: i32,
    pub screen_y: i32,
    /// inline size-change hook owner (ScrollBox root id) — fires
    /// `recalculateBarProps` on size change
    pub(crate) size_hook: Option<String>,
}

impl Renderable {
    /// Create a renderable and its taffy node (upstream constructor +
    /// `applyYogaProps`, Renderable.ts:700-760).
    pub fn new(
        tree: &mut TaffyTree<MeasureCtx>,
        id: &str,
        layout: LayoutOptions,
        width: Dimension,
        height: Dimension,
        content: Content,
        measure: Option<MeasureCtx>,
    ) -> Self {
        let overflow = layout
            .overflow
            .as_deref()
            .map_or(Overflow::Visible, Overflow::parse);
        let style = taffy_style(&layout, width, height);
        let node = match measure {
            Some(ctx) => tree.new_leaf_with_context(style, ctx).expect("taffy leaf"),
            None => tree.new_leaf(style).expect("taffy leaf"),
        };
        Renderable {
            id: RenderableId(id.to_owned()),
            node,
            layout,
            width,
            height,
            visible: true,
            content,
            children: Vec::new(),
            translate_x: 0.0,
            translate_y: 0.0,
            overflow,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            screen_x: 0,
            screen_y: 0,
            size_hook: None,
        }
    }

    /// `add(child)` — appends to children + taffy child list (Renderable.ts
    /// `add`, children order preserved).
    pub fn add(&mut self, tree: &mut TaffyTree<MeasureCtx>, child: Renderable) {
        tree.add_child(self.node, child.node)
            .expect("taffy add_child");
        self.children.push(child);
    }

    /// `visible` setter's yoga half (Renderable.ts:347-351):
    /// `setDisplay(visible ? Flex : None)`. Returns true when the display
    /// value flipped (→ re-layout needed).
    pub fn set_visible(&mut self, tree: &mut TaffyTree<MeasureCtx>, visible: bool) -> bool {
        if self.visible == visible {
            return false;
        }
        self.visible = visible;
        let mut style = tree.style(self.node).expect("taffy style").clone();
        style.display = if visible {
            tp::Display::Flex
        } else {
            tp::Display::None
        };
        let _ = tree.set_style(self.node, style);
        let _ = tree.mark_dirty(self.node);
        true
    }

    /// Apply the initial display for a freshly created node whose
    /// `visible` was set at construction (Renderable.ts:308-315).
    pub fn apply_initial_display(&self, tree: &mut TaffyTree<MeasureCtx>) {
        if !self.visible {
            let mut style = tree.style(self.node).expect("taffy style").clone();
            style.display = tp::Display::None;
            let _ = tree.set_style(self.node, style);
        }
    }

    /// `translateY = value` (Renderable.ts:537-545): no-op when equal;
    /// otherwise update the cache (parent screen + layout y + translate).
    /// Returns true when the value changed.
    pub fn set_translate_y(&mut self, value: f32) -> bool {
        if self.translate_y == value {
            return false;
        }
        self.translate_y = value;
        true
    }

    pub fn set_translate_x(&mut self, value: f32) -> bool {
        if self.translate_x == value {
            return false;
        }
        self.translate_x = value;
        true
    }

    /// `updateLayout`/`updateFromLayout` — pull taffy layout, propagate
    /// screen coords (Renderable.ts:1100-1109), clamp size to ≥1
    /// (Renderable.ts:1106-1107), fire the size-change hook on change
    /// (Renderable.ts:1128-1140 `onLayoutResize` → `onSizeChange`).
    /// Invisible subtrees are skipped entirely (Renderable.ts:1385-1386).
    pub fn update_layout(
        &mut self,
        tree: &TaffyTree<MeasureCtx>,
        parent_screen: (f32, f32),
        parent_size: (f32, f32),
        effects: &mut WalkEffects,
    ) {
        if !self.visible {
            return;
        }
        let Ok(l) = tree.layout(self.node) else {
            return;
        };
        self.x = l.location.x;
        self.y = l.location.y;
        // border-box: upstream draws in outer coordinates; the yoga border
        // padding is already reflected in children positions
        let sx = parent_screen.0 + self.x + self.translate_x;
        let sy = parent_screen.1 + self.y + self.translate_y;
        self.screen_x = sx.round() as i32;
        self.screen_y = sy.round() as i32;
        // Math.max(layout.width, 1) — Renderable.ts:1106-1107. The width
        // drives the viewport + wrap width even for mode none (alignment
        // + horizontal clip) — onResize → setViewport (TextBufferRenderable.ts:371-376).
        let new_w = l.size.width.max(1.0);
        let new_h = l.size.height.max(1.0);
        let size_changed = self.w != new_w || self.h != new_h;
        self.w = new_w;
        self.h = new_h;
        // mark measured text with the final width for wrapping
        if let Content::Text(state) = &mut self.content {
            state.update_layout(self.w.max(0.0) as u32, self.h.max(0.0) as u32);
        }
        // EditBufferRenderable.onResize → setViewportSize
        // (EditBufferRenderable.ts:481-483): same clamp, keeps offsets.
        if let Content::Textarea(state) = &mut self.content {
            state.update_layout(self.w.max(0.0) as u32, self.h.max(0.0) as u32);
        }
        if size_changed {
            effects.size_changed = true;
            if let Some(owner) = &self.size_hook {
                // Snapshot the recalc inputs at fire time (upstream hooks
                // run inline mid-walk): when the VIEWPORT resizes, the
                // content below it has not been updated yet this walk, so
                // `recalculateBarProps` reads the stale content size —
                // pinned against the real 0.5.14 oracle. When the CONTENT
                // resizes, the viewport above it is already current and is
                // read from the tree at apply time.
                let stale_content = if !matches!(self.content, Content::ScrollContent(_, _)) {
                    // self is the viewport; its first child IS the content
                    self.children.first().map(|c| (c.w, c.h))
                } else {
                    None
                };
                if std::env::var("OT_DBG").is_ok() {
                    eprintln!("  [hook] {} ({},{})", self.id.0, self.w, self.h);
                }
                effects.events.push(RecalcEvent {
                    owner: owner.clone(),
                    stale_content_size: stale_content,
                });
            }
        }
        // refresh the ScrollBox content culling viewport (the parent of a
        // ScrollContent node IS the viewport — ScrollBox.ts:42-48)
        if let Content::ScrollContent(_, culling) = &mut self.content {
            culling.viewport = (
                parent_screen.0.round() as i32,
                parent_screen.1.round() as i32,
                parent_size.0.max(0.0) as u32,
                parent_size.1.max(0.0) as u32,
            );
        }
        for child in &mut self.children {
            child.update_layout(tree, (sx, sy), (self.w, self.h), effects);
        }
    }

    /// `getScissorRect` — Renderable.ts:1544-1556: the node's screen rect;
    /// the Box override insets by its borders (Box.ts:296-315).
    #[must_use]
    pub fn scissor_rect(&self) -> (i32, i32, u32, u32) {
        let (mut x, mut y, mut w, mut h) = (self.screen_x, self.screen_y, self.w, self.h);
        let sides = match &self.content {
            Content::Box(state) => state.border_sides,
            Content::ScrollBox(state, _) => state.border_sides,
            _ => return (x, y, w.max(0.0) as u32, h.max(0.0) as u32),
        };
        let left = i32::from(sides.left);
        let right = i32::from(sides.right);
        let top = i32::from(sides.top);
        let bottom = i32::from(sides.bottom);
        x += left;
        y += top;
        w = (w - left as f32 - right as f32).max(0.0);
        h = (h - top as f32 - bottom as f32).max(0.0);
        (x, y, w as u32, h as u32)
    }

    /// `renderSelf` + render-list scissor semantics (RootRenderable.render
    /// pass 3, Renderable.ts:1829-1848; pushScissorRect around children,
    /// 1425-1432): paint self unclipped, then clip children to the
    /// overflow rect, then pop.
    pub fn paint(&self, buffer: &mut OptimizedBuffer) {
        if !self.visible {
            return;
        }
        self.paint_self(buffer);
        // Renderable.ts:1425-1432 — push around children when overflow
        // clips and the node has positive size
        let should_push_scissor =
            self.overflow != Overflow::Visible && self.w > 0.0 && self.h > 0.0;
        if should_push_scissor {
            let (sx, sy, sw, sh) = self.scissor_rect();
            buffer.push_scissor_rect(sx, sy, sw, sh);
        }
        // viewport culling of content children (ContentRenderable
        // `_getVisibleChildren`, ScrollBox.ts:38-55)
        match &self.content {
            Content::ScrollContent(_, culling) if culling.enabled => {
                let visible = culling.visible_child_indices(&self.children);
                for i in visible {
                    self.children[i].paint(buffer);
                }
            }
            _ => {
                for child in &self.children {
                    child.paint(buffer);
                }
            }
        }
        if should_push_scissor {
            buffer.pop_scissor_rect();
        }
    }

    fn paint_self(&self, buffer: &mut OptimizedBuffer) {
        match &self.content {
            Content::Group | Content::ScrollBar(_) => {}
            Content::Box(state)
            | Content::ScrollBox(state, _)
            | Content::ScrollContent(state, _) => state.paint(
                buffer,
                self.screen_x,
                self.screen_y,
                self.w as u32,
                self.h as u32,
            ),
            Content::Text(state) => state.paint(buffer, self.screen_x, self.screen_y),
            // draws from the snapshot `prepare_paint` stashed
            // (upstream render() = renderSelf + renderCursor; the port
            // splits because renderCursor writes renderer state,
            // EditBufferRenderable.ts:991-1004)
            Content::Textarea(state) => state.paint(buffer, self.screen_x, self.screen_y),
            Content::Slider(state) => state.paint(
                buffer,
                self.screen_x,
                self.screen_y,
                self.w as u32,
                self.h as u32,
            ),
            Content::Arrow(state) => state.paint(buffer, self.screen_x, self.screen_y),
        }
    }

    /// `render()` phase 1 (EditBufferRenderable.ts:991-1004): stash the
    /// draw snapshot + collect the terminal-cursor output. Runs as a
    /// `&mut` walk before the `&self` paint.
    pub fn prepare_paint(&mut self, cursor_out: &mut Vec<(String, u32, u32)>) {
        if !self.visible {
            return;
        }
        if let Content::Textarea(state) = &mut self.content {
            if let Some((x, y)) = state.prepare_render(self.screen_x, self.screen_y) {
                cursor_out.push((self.id.0.clone(), x, y));
            }
        }
        for child in &mut self.children {
            child.prepare_paint(cursor_out);
        }
    }

    /// Mark the taffy subtree dirty for the next layout pass
    /// (`requestRender` + yoga dirty marking, Renderable.ts).
    pub fn mark_dirty(&self, tree: &mut TaffyTree<MeasureCtx>) {
        if let Err(e) = tree.mark_dirty(self.node) {
            let _ = e;
        }
    }

    /// Depth-first find by id (`findDescendantById`, Renderable.ts:509-517).
    pub fn find(&self, id: &str) -> Option<&Renderable> {
        if self.id.0 == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Renderable> {
        if self.id.0 == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }
}

/// The root — `RootRenderable` (Renderable.ts:1743-1770): fixed terminal
/// size, column direction, laid out immediately.
pub struct RootRenderable {
    pub inner: Renderable,
}

/// Recursive nested mount: walk `node` looking for `parent_id`; on match
/// attach `child`, otherwise carry it back up for the next branch.
pub(crate) fn mount_under(
    node: &mut Renderable,
    parent_id: &str,
    tree: &mut TaffyTree<MeasureCtx>,
    child: Renderable,
) -> Option<Renderable> {
    if node.id.0 == parent_id {
        node.add(tree, child);
        return None;
    }
    let mut carry = Some(child);
    for c in node.children.iter_mut() {
        let Some(ch) = carry.take() else { break };
        carry = mount_under(c, parent_id, tree, ch);
        if carry.is_none() {
            break;
        }
    }
    carry
}

impl RootRenderable {
    pub fn new(tree: &mut TaffyTree<MeasureCtx>, width: u32, height: u32) -> Self {
        let layout = LayoutOptions {
            flex_direction: Some("column".into()),
            ..Default::default()
        };
        RootRenderable {
            inner: Renderable::new(
                tree,
                "__root__",
                layout,
                Dimension::Points(width as f32),
                Dimension::Points(height as f32),
                Content::Group,
                None,
            ),
        }
    }

    pub fn add(&mut self, tree: &mut TaffyTree<MeasureCtx>, child: Renderable) {
        self.inner.add(tree, child);
    }
}
