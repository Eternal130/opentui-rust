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
//!   parent._screenX + _x + _translateX` (Renderable.ts:526, 1107-1109).
//! * render walk: 3-pass — root `calculateLayout` (Renderable.ts:1866),
//!   collect render list, then `renderSelf` per renderable
//!   (Renderable.ts:1829-1848; RootRenderable.render:1789-1850).

use crate::buffer::OptimizedBuffer;
use crate::layout::{taffy_style, Dimension, LayoutOptions};
use crate::renderables::box_::BoxRenderableState;
use crate::renderables::text::TextRenderableState;
use taffy::prelude as tp;
use taffy::tree::TaffyTree;

/// Per-node measure context (taffy `NodeContext`): what the leaf measures
/// from. Text leaves carry their content; container leaves measure nothing.
#[derive(Debug, Clone, Default)]
pub struct MeasureCtx {
    pub kind: MeasureKind,
    /// wrapped text for Text leaves
    pub text: Option<crate::renderables::text::MeasuredText>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MeasureKind {
    #[default]
    None,
    Text,
}

/// What a renderable paints.
#[allow(clippy::large_enum_variant)]
pub enum Content {
    /// Plain container (upstream `Renderable` base).
    Group,
    Box(BoxRenderableState),
    Text(TextRenderableState),
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
    /// computed layout (post `updateLayout`)
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub screen_x: i32,
    pub screen_y: i32,
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
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            screen_x: 0,
            screen_y: 0,
        }
    }

    /// `add(child)` — appends to children + taffy child list (Renderable.ts
    /// `add`, children order preserved).
    pub fn add(&mut self, tree: &mut TaffyTree<MeasureCtx>, child: Renderable) {
        tree.add_child(self.node, child.node)
            .expect("taffy add_child");
        self.children.push(child);
        // Border-derived padding: a Box's taffy borders shrink its content
        // box automatically; nothing extra needed here.
    }

    /// `updateLayout` — pull taffy layout, propagate screen coords
    /// (Renderable.ts:1100-1109).
    pub fn update_layout(&mut self, tree: &TaffyTree<MeasureCtx>, parent_screen: (i32, i32)) {
        let Ok(l) = tree.layout(self.node) else {
            return;
        };
        self.x = l.location.x;
        self.y = l.location.y;
        self.w = l.size.width;
        self.h = l.size.height;
        // border-box: upstream draws in outer coordinates; the yoga border
        // padding is already reflected in children positions
        self.screen_x = parent_screen.0 + self.x.round() as i32;
        self.screen_y = parent_screen.1 + self.y.round() as i32;
        // onResize → setViewport (TextBufferRenderable.ts:371-376):
        // the laid-out size (clamped ≥ 1) drives the viewport + wrap
        // width. Note the width drives wrapping even for mode none
        // (alignment + horizontal clip).
        if let Content::Text(state) = &mut self.content {
            state.update_layout(self.w.max(0.0) as u32, self.h.max(0.0) as u32);
        }
        for child in &mut self.children {
            child.update_layout(tree, (self.screen_x, self.screen_y));
        }
    }

    /// `renderSelf` dispatch (RootRenderable.render pass 3, Renderable.ts:1829+).
    pub fn paint(&self, buffer: &mut OptimizedBuffer) {
        if !self.visible {
            return;
        }
        match &self.content {
            Content::Group => {}
            Content::Box(state) => state.paint(
                buffer,
                self.screen_x,
                self.screen_y,
                self.w as u32,
                self.h as u32,
            ),
            Content::Text(state) => state.paint(buffer, self.screen_x, self.screen_y),
        }
        for child in &self.children {
            child.paint(buffer);
        }
    }

    /// Mark the taffy subtree dirty for the next layout pass
    /// (`requestRender` + yoga dirty marking, Renderable.ts).
    pub fn mark_dirty(&self, tree: &mut TaffyTree<MeasureCtx>) {
        if let Err(e) = tree.mark_dirty(self.node) {
            let _ = e;
        }
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
