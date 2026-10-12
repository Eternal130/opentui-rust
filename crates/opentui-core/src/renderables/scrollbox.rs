//! `ScrollBoxRenderable` — port of
//! `packages/core/src/renderables/ScrollBox.ts` (+ the viewport-culling
//! helper `lib/objects-in-viewport.ts`).
//!
//! Upstream composes: root Box (row) → [wrapper Box (column) → [viewport
//! Box (column, overflow hidden, flexGrow 1) → content ContentRenderable],
//! horizontal ScrollBar], vertical ScrollBar (ScrollBox.ts:305-383). The
//! port builds the same subtree with the same ids
//! (`scroll-box-{wrapper,viewport,content,vertical-scrollbar,horizontal-scrollbar}-{n}`,
//! ScrollBox.ts:322,336,349,360,376) and reproduces the event-driven state
//! machine exactly: `recalculateBarProps` fires from viewport/content
//! `onSizeChange` (ScrollBox.ts:332,345-347), scroll positions flow through
//! the ScrollBar setter cascade into the slider and back out via onChange
//! to `content.translateY = -position` (ScrollBox.ts:362-365), and sticky
//! scroll tracks manual vs sticky state (ScrollBox.ts:190-273, 749-805).
//!
//! Upstream expresses these as property writes on objects; the port's
//! renderables are owned by the renderer tree, so the setters are free
//! functions over `(root, tree, ids)` that report their effects (display
//! flips, translates) so the renderer's settle loop can re-layout — the
//! same frames upstream's `requestRender` scheduling produces.

use crate::layout::{Dimension, LayoutOptions};
use crate::renderable::{Content, MeasureCtx, Renderable, WalkEffects};
use crate::renderables::box_::{BoxOptions, BoxRenderableState};
use crate::renderables::scrollbar::{
    default_thumb_bg, default_track_bg, ArrowDirection, ArrowState, Orientation, ScrollBarState,
    ScrollUnit, SliderState,
};
use crate::scroll_acceleration::ScrollAcceleration;
use taffy::tree::TaffyTree;

/// `ScrollBoxRenderable.idCounter` — ScrollBox.ts:110.
static ID_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn next_internal_id() -> u32 {
    ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// `stickyStart` — ScrollBox.ts:67.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickyStart {
    Bottom,
    Top,
    Left,
    Right,
}

impl StickyStart {
    pub fn parse(s: &str) -> Option<StickyStart> {
        match s {
            "bottom" => Some(StickyStart::Bottom),
            "top" => Some(StickyStart::Top),
            "left" => Some(StickyStart::Left),
            "right" => Some(StickyStart::Right),
            _ => None,
        }
    }
}

/// `ScrollBoxOptions` — ScrollBox.ts:58-72 (the option subset the port
/// consumes; box-ish options pass through to the root Box).
#[derive(Default)]
pub struct ScrollBoxOptions {
    pub box_options: BoxOptions,
    /// `rootOptions` — merged after the inline box options (ScrollBox.ts:309-310)
    pub root_options: Option<BoxOptions>,
    pub wrapper_options: Option<BoxOptions>,
    pub viewport_options: Option<BoxOptions>,
    /// content options — root padding forwards here, content wins
    /// (ScrollBox.ts:293-303)
    pub content_options: Option<BoxOptions>,
    /// `scrollbarOptions.trackOptions` subset: thumb = slider foreground,
    /// track = slider background (Slider.ts:38-39)
    pub scrollbar_thumb_color: Option<String>,
    pub scrollbar_track_color: Option<String>,
    /// `stickyScroll` default false (ScrollBox.ts:284)
    pub sticky_scroll: bool,
    pub sticky_start: Option<StickyStart>,
    /// `scrollX` default false (ScrollBox.ts:285)
    pub scroll_x: bool,
    /// `scrollY` default true (ScrollBox.ts:286)
    pub scroll_y: bool,
    /// `scrollAcceleration` — None = `LinearScrollAccel` (ScrollBox.ts:316)
    pub scroll_acceleration: Option<Box<dyn ScrollAcceleration + Send>>,
    /// `viewportCulling` default true (ScrollBox.ts:289)
    pub viewport_culling: bool,
}

/// `ScrollBoxRenderable` machine state — the private fields of
/// ScrollBox.ts:118-144 (structural ids appended).
pub struct ScrollBoxState {
    pub internal_id: u32,
    pub sticky_scroll: bool,
    pub sticky_scroll_top: bool,
    pub sticky_scroll_bottom: bool,
    pub sticky_scroll_left: bool,
    pub sticky_scroll_right: bool,
    pub sticky_start: Option<StickyStart>,
    pub has_manual_scroll: bool,
    pub is_applying_sticky_scroll: bool,
    /// `scrollAccel` (ScrollBox.ts:144) behind a mutex so the wheel seam
    /// can tick the shared curve
    pub scroll_accel: std::sync::Mutex<Box<dyn ScrollAcceleration + Send>>,
    /// `scrollAccumulatorX/Y` — ScrollBox.ts:133-134
    pub scroll_accumulator_x: f64,
    pub scroll_accumulator_y: f64,
    /// auto-scroll state — ScrollBox.ts:121-131 (constants 123-127)
    pub auto_scroll_mouse_x: f32,
    pub auto_scroll_mouse_y: f32,
    pub auto_scroll_threshold_vertical: f32,
    pub auto_scroll_threshold_horizontal: f32,
    pub auto_scroll_speed_slow: f32,
    pub auto_scroll_speed_medium: f32,
    pub auto_scroll_speed_fast: f32,
    pub is_auto_scrolling: bool,
    pub cached_auto_scroll_speed: f32,
    pub auto_scroll_accumulator_x: f64,
    pub auto_scroll_accumulator_y: f64,
    pub wrapper_id: String,
    pub viewport_id: String,
    pub content_id: String,
    pub vbar_id: String,
    pub hbar_id: String,
}

impl std::fmt::Debug for ScrollBoxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollBoxState")
            .field("internal_id", &self.internal_id)
            .field("sticky_scroll", &self.sticky_scroll)
            .field("sticky_start", &self.sticky_start)
            .field("has_manual_scroll", &self.has_manual_scroll)
            .finish()
    }
}

/// The content node's culling filter — `ContentRenderable`
/// (ScrollBox.ts:11-56).
#[derive(Debug, Clone, Default)]
pub struct ViewportCulling {
    pub enabled: bool,
    /// primary axis is a row? (content flexDirection)
    pub primary_axis_row: bool,
    /// viewport screen rect, refreshed each update walk (the content
    /// node's parent IS the viewport — ScrollBox.ts:44-47)
    pub viewport: (i32, i32, u32, u32),
}

/// One object for `getObjectsInViewport` — objects-in-viewport.ts:7-13.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportObject {
    pub screen_x: f32,
    pub screen_y: f32,
    pub width: f32,
    pub height: f32,
    pub z_index: i32,
}

/// `ViewportBounds` — lib/types.ts.
pub struct ViewportBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// `getObjectsInViewport` — lib/objects-in-viewport.ts:25-155 (exact):
/// objects overlapping the viewport (± padding), sorted by zIndex. Fewer
/// than `minTriggerSize` = 16 objects short-circuits to ALL of them
/// (objects-in-viewport.ts:49-51) — the common scrollbox case.
#[must_use]
pub fn get_objects_in_viewport(
    viewport: ViewportBounds,
    objects: &[ViewportObject],
    direction: &str,
    padding: f32,
) -> Vec<ViewportObject> {
    const MIN_TRIGGER_SIZE: usize = 16;
    if viewport.width <= 0.0 || viewport.height <= 0.0 {
        return vec![];
    }
    if objects.is_empty() {
        return vec![];
    }
    if objects.len() < MIN_TRIGGER_SIZE {
        return objects.to_vec();
    }

    let viewport_top = viewport.y - padding;
    let viewport_bottom = viewport.y + viewport.height + padding;
    let viewport_left = viewport.x - padding;
    let viewport_right = viewport.x + viewport.width + padding;

    let is_row = direction == "row";
    let (vp_start, vp_end) = if is_row {
        (viewport_left, viewport_right)
    } else {
        (viewport_top, viewport_bottom)
    };
    let start_of = |o: &ViewportObject| if is_row { o.screen_x } else { o.screen_y };
    let end_of = |o: &ViewportObject| {
        if is_row {
            o.screen_x + o.width
        } else {
            o.screen_y + o.height
        }
    };

    // binary search for any overlapping object (75-91)
    let mut lo = 0usize;
    let mut hi = objects.len() as i64 - 1;
    let mut candidate: i64 = -1;
    while lo as i64 <= hi {
        let mid = ((lo as i64 + hi) / 2) as usize;
        let (start, end) = (start_of(&objects[mid]), end_of(&objects[mid]));
        if end < vp_start {
            lo = mid + 1;
        } else if start > vp_end {
            hi = mid as i64 - 1;
        } else {
            candidate = mid as i64;
            break;
        }
    }
    // viewport in a gap — start from the last object before it (94-98)
    if candidate == -1 {
        candidate = if lo > 0 { (lo - 1) as i64 } else { 0 };
    }

    // expand left across gaps (maxLookBehind 50, 102-118)
    const MAX_LOOK_BEHIND: usize = 50;
    let mut left = candidate as usize;
    let mut gap_count = 0usize;
    while left >= 1 {
        if end_of(&objects[left - 1]) <= vp_start {
            gap_count += 1;
            if gap_count >= MAX_LOOK_BEHIND {
                break;
            }
        } else {
            gap_count = 0;
        }
        left -= 1;
    }
    // expand right (121-125)
    let mut right = candidate as usize + 1;
    while right < objects.len() {
        if start_of(&objects[right]) >= vp_end {
            break;
        }
        right += 1;
    }

    // collect with cross-axis overlap (128-150)
    let mut visible: Vec<ViewportObject> = Vec::new();
    for child in &objects[left..right] {
        if end_of(child) <= vp_start {
            continue;
        }
        if start_of(child) >= vp_end {
            break;
        }
        if is_row {
            let child_bottom = child.screen_y + child.height;
            if child_bottom < viewport_top {
                continue;
            }
            if child.screen_y > viewport_bottom {
                continue;
            }
        } else {
            let child_right = child.screen_x + child.width;
            if child_right < viewport_left {
                continue;
            }
            if child.screen_x > viewport_right {
                continue;
            }
        }
        visible.push(*child);
    }
    // zIndex sort (153-155)
    if visible.len() > 1 {
        visible.sort_by_key(|o| o.z_index);
    }
    visible
}

impl ViewportCulling {
    /// `_getVisibleChildren` — ScrollBox.ts:38-55: objects in the viewport
    /// (terminal coords, padding 0). Returns child indices in insertion
    /// order — upstream filters `_childrenInZIndexOrder` through the
    /// visible set (Renderable.ts:1444-1451).
    pub fn visible_child_indices(&self, children: &[Renderable]) -> Vec<usize> {
        // getChildrenSortedByPrimaryAxis — sort by screen position along
        // the content's primary axis
        let mut sorted: Vec<usize> = (0..children.len()).collect();
        sorted.sort_by(|&a, &b| {
            let (ka, kb) = if self.primary_axis_row {
                (children[a].screen_x, children[b].screen_x)
            } else {
                (children[a].screen_y, children[b].screen_y)
            };
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        let objects: Vec<(usize, ViewportObject)> = sorted
            .iter()
            .map(|&i| {
                (
                    i,
                    ViewportObject {
                        screen_x: children[i].screen_x as f32,
                        screen_y: children[i].screen_y as f32,
                        width: children[i].w,
                        height: children[i].h,
                        z_index: 0,
                    },
                )
            })
            .collect();
        let objs: Vec<ViewportObject> = objects.iter().map(|(_, o)| *o).collect();
        let visible = get_objects_in_viewport(
            ViewportBounds {
                x: self.viewport.0 as f32,
                y: self.viewport.1 as f32,
                width: self.viewport.2 as f32,
                height: self.viewport.3 as f32,
            },
            &objs,
            if self.primary_axis_row {
                "row"
            } else {
                "column"
            },
            0.0,
        );
        // map back to child indices, then restore zIndex (insertion) order
        let mut idxs: Vec<usize> = visible
            .iter()
            .filter_map(|v| objects.iter().find(|(_, o)| o == v).map(|(i, _)| *i))
            .collect();
        idxs.sort_unstable();
        idxs.dedup();
        idxs
    }
}

// ---------------------------------------------------------------------------
// construction — ScrollBox.ts:275-398
// ---------------------------------------------------------------------------

/// Build the ScrollBox subtree. `id` is the user-facing root id; internal
/// nodes follow the upstream scheme (ScrollBox.ts:322,336,349,360,376).
#[allow(clippy::too_many_lines)]
pub fn build_scrollbox(
    tree: &mut TaffyTree<MeasureCtx>,
    id: &str,
    opts: &mut ScrollBoxOptions,
) -> Renderable {
    let internal_id = next_internal_id();
    let ids = ScrollIds {
        wrapper: format!("scroll-box-wrapper-{internal_id}"),
        viewport: format!("scroll-box-viewport-{internal_id}"),
        content: format!("scroll-box-content-{internal_id}"),
        vbar: format!("scroll-box-vertical-scrollbar-{internal_id}"),
        hbar: format!("scroll-box-horizontal-scrollbar-{internal_id}"),
    };

    // Root options: inline box options + rootOptions, padding stripped and
    // forwarded to the content (ScrollBox.ts:74-107, 293-311)
    let mut root_box = opts.box_options.clone();
    if let Some(ro) = &opts.root_options {
        merge_box_options(&mut root_box, ro);
    }
    let forwarded_padding = collect_padding(&root_box);
    strip_padding(&mut root_box);

    // ScrollBox.ts:306-308 — flexDirection row + alignItems stretch come
    // FIRST, user box options may override
    let mut root_layout = root_box.layout.clone();
    if root_layout.flex_direction.is_none() {
        root_layout.flex_direction = Some("row".into());
    }
    if root_layout.align_items.is_none() {
        root_layout.align_items = Some("stretch".into());
    }
    let root_state = BoxRenderableState::from_options(&root_box);
    root_state.apply_yoga_borders(&mut root_layout);

    let state = ScrollBoxState {
        internal_id,
        sticky_scroll: opts.sticky_scroll,
        sticky_scroll_top: false,
        sticky_scroll_bottom: false,
        sticky_scroll_left: false,
        sticky_scroll_right: false,
        sticky_start: opts.sticky_start,
        has_manual_scroll: false,
        is_applying_sticky_scroll: false,
        scroll_accel: std::sync::Mutex::new(
            opts.scroll_acceleration
                .take()
                .unwrap_or_else(|| Box::new(crate::scroll_acceleration::LinearScrollAccel)),
        ),
        scroll_accumulator_x: 0.0,
        scroll_accumulator_y: 0.0,
        auto_scroll_mouse_x: 0.0,
        auto_scroll_mouse_y: 0.0,
        auto_scroll_threshold_vertical: 3.0,
        auto_scroll_threshold_horizontal: 3.0,
        auto_scroll_speed_slow: 6.0,
        auto_scroll_speed_medium: 36.0,
        auto_scroll_speed_fast: 72.0,
        is_auto_scrolling: false,
        cached_auto_scroll_speed: 3.0,
        auto_scroll_accumulator_x: 0.0,
        auto_scroll_accumulator_y: 0.0,
        wrapper_id: ids.wrapper.clone(),
        viewport_id: ids.viewport.clone(),
        content_id: ids.content.clone(),
        vbar_id: ids.vbar.clone(),
        hbar_id: ids.hbar.clone(),
    };

    // ScrollBox.ts:387-389 + 244-273: applyStickyStart at construction
    // (all sizes are 0, so only the flags land)
    let (flag_top, flag_bottom) = match opts.sticky_start {
        Some(StickyStart::Top) if opts.sticky_scroll => (true, false),
        Some(StickyStart::Bottom) if opts.sticky_scroll => (false, true),
        _ => (false, false),
    };
    let (flag_left, flag_right) = match opts.sticky_start {
        Some(StickyStart::Left) if opts.sticky_scroll => (true, false),
        Some(StickyStart::Right) if opts.sticky_scroll => (false, true),
        _ => (false, false),
    };
    let state = ScrollBoxState {
        sticky_scroll_top: flag_top,
        sticky_scroll_bottom: flag_bottom,
        sticky_scroll_left: flag_left,
        sticky_scroll_right: flag_right,
        ..state
    };

    let mut root = Renderable::new(
        tree,
        id,
        root_layout,
        root_box.width.unwrap_or(Dimension::Auto),
        root_box.height.unwrap_or(Dimension::Auto),
        Content::ScrollBox(root_state, state),
        None,
    );

    // wrapper — ScrollBox.ts:318-324
    let mut wrapper_box = BoxOptions {
        layout: LayoutOptions {
            flex_direction: Some("column".into()),
            flex_grow: Some(1.0),
            ..Default::default()
        },
        ..Default::default()
    };
    if let Some(w) = &opts.wrapper_options {
        merge_box_options(&mut wrapper_box, w);
    }
    let wrapper = make_box_node(tree, &ids.wrapper, &wrapper_box);
    mount_internal(&mut root, tree, id, id, wrapper);

    // viewport — ScrollBox.ts:326-338 (overflow hidden)
    let mut viewport_box = BoxOptions {
        layout: LayoutOptions {
            flex_direction: Some("column".into()),
            flex_grow: Some(1.0),
            overflow: Some("hidden".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    if let Some(v) = &opts.viewport_options {
        merge_box_options(&mut viewport_box, v);
    }
    let mut viewport = make_box_node(tree, &ids.viewport, &viewport_box);
    viewport.size_hook = Some(id.to_owned());
    mount_internal(&mut root, tree, id, &ids.wrapper.clone(), viewport);

    // content — ScrollBox.ts:340-350
    let mut content_layout = LayoutOptions {
        align_self: Some("flex-start".into()),
        flex_shrink: Some(0.0),
        min_width: Some(Dimension::Percent(1.0)),
        min_height: Some(Dimension::Percent(1.0)),
        ..Default::default()
    };
    if !opts.scroll_x {
        content_layout.max_width = Some(Dimension::Percent(1.0));
    }
    if !opts.scroll_y {
        content_layout.max_height = Some(Dimension::Percent(1.0));
    }
    let mut content_box = BoxOptions {
        layout: content_layout,
        ..Default::default()
    };
    apply_padding(&mut content_box, &forwarded_padding);
    if let Some(c) = &opts.content_options {
        merge_box_options(&mut content_box, c);
    }
    let primary_axis_row = content_box
        .layout
        .flex_direction
        .as_deref()
        .is_some_and(|s| s == "row");
    let mut content = make_box_node(tree, &ids.content, &content_box);
    if let Content::Box(bs) = content.content {
        content.content = Content::ScrollContent(
            bs,
            ViewportCulling {
                enabled: opts.viewport_culling,
                primary_axis_row,
                viewport: (0, 0, 0, 0),
            },
        );
    }
    content.size_hook = Some(id.to_owned());
    mount_internal(&mut root, tree, id, &ids.viewport.clone(), content);

    // vertical scrollbar — ScrollBox.ts:353-367 (root child, after wrapper)
    let vbar = build_scrollbar(
        tree,
        &ids.vbar,
        Orientation::Vertical,
        opts.scrollbar_thumb_color.as_deref(),
        opts.scrollbar_track_color.as_deref(),
    );
    root.add(tree, vbar);

    // horizontal scrollbar — ScrollBox.ts:369-383 (wrapper child)
    let hbar = build_scrollbar(
        tree,
        &ids.hbar,
        Orientation::Horizontal,
        opts.scrollbar_thumb_color.as_deref(),
        opts.scrollbar_track_color.as_deref(),
    );
    mount_internal(&mut root, tree, id, &ids.wrapper.clone(), hbar);

    root
}

/// The structural ids of one scrollbox subtree.
#[derive(Debug, Clone)]
pub struct ScrollIds {
    pub wrapper: String,
    pub viewport: String,
    pub content: String,
    pub vbar: String,
    pub hbar: String,
}

fn mount_internal(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    _owner: &str,
    parent: &str,
    child: Renderable,
) {
    if crate::renderable::mount_under(root, parent, tree, child).is_some() {
        panic!("scrollbox internal parent {parent} not found");
    }
}

/// Build one ScrollBar subtree — ScrollBar.ts:103-224: children
/// [startArrow, slider, endArrow] (178-180); the slider carries
/// `width max(1, min(2, bar.width)) = 1` for vertical (137 — the bar has
/// no size at construction) and flexGrow 1 / flexShrink 1 (146-147).
fn build_scrollbar(
    tree: &mut TaffyTree<MeasureCtx>,
    id: &str,
    orientation: Orientation,
    thumb_color: Option<&str>,
    track_color: Option<&str>,
) -> Renderable {
    // ScrollBar.ts:107-112 — flexDirection by orientation, alignSelf
    // stretch, alignItems stretch
    let layout = LayoutOptions {
        flex_direction: Some(
            match orientation {
                Orientation::Vertical => "column",
                Orientation::Horizontal => "row",
            }
            .into(),
        ),
        align_self: Some("stretch".into()),
        align_items: Some("stretch".into()),
        flex_shrink: Some(1.0), // Renderable.ts:728-737 (no explicit size)
        ..Default::default()
    };

    // ScrollBar.ts:119-129 — scrollRange 0, stepSize max(1, 0) = 1
    let slider_state = SliderState {
        orientation,
        min: 0.0,
        max: 0.0,
        value: 0.0,
        view_port_size: 1.0,
        background_color: track_color.map_or_else(default_track_bg, crate::color::parse_color),
        foreground_color: thumb_color.map_or_else(default_thumb_bg, crate::color::parse_color),
    };

    let bar_state = ScrollBarState {
        orientation,
        scroll_size: 0.0,
        scroll_position: 0.0,
        viewport_size: 0.0,
        show_arrows: false,
        manual_visibility: false,
        scroll_step: None,
        slider_id: format!("{id}::slider"),
        start_arrow_id: format!("{id}::arrow-start"),
        end_arrow_id: format!("{id}::arrow-end"),
    };

    let mut bar = Renderable::new(
        tree,
        id,
        layout,
        Dimension::Auto,
        Dimension::Auto,
        Content::ScrollBar(bar_state),
        None,
    );

    // arrows — ScrollBar.ts:162-176: alignSelf center, size 1, hidden
    let (start_dir, end_dir) = match orientation {
        Orientation::Vertical => (ArrowDirection::Up, ArrowDirection::Down),
        Orientation::Horizontal => (ArrowDirection::Left, ArrowDirection::Right),
    };
    let mut start_arrow = Renderable::new(
        tree,
        &format!("{id}::arrow-start"),
        LayoutOptions {
            align_self: Some("center".into()),
            ..Default::default()
        },
        Dimension::Points(1.0),
        Dimension::Points(1.0),
        Content::Arrow(ArrowState {
            direction: start_dir,
            ..Default::default()
        }),
        None,
    );
    start_arrow.visible = false; // showArrows = false (ScrollBar.ts:117,164)
    let mut end_arrow = Renderable::new(
        tree,
        &format!("{id}::arrow-end"),
        LayoutOptions {
            align_self: Some("center".into()),
            ..Default::default()
        },
        Dimension::Points(1.0),
        Dimension::Points(1.0),
        Content::Arrow(ArrowState {
            direction: end_dir,
            ..Default::default()
        }),
        None,
    );
    end_arrow.visible = false;

    // slider — ScrollBar.ts:135-148: flexGrow 1, flexShrink 1; vertical →
    // width max(1, min(2, bar.width=0)) = 1 (the bar has no size at
    // construction, ScrollBar.ts:137), height 100%, marginLeft auto;
    // horizontal → width 100%, height 1, marginTop auto
    let (slider_layout, slider_w, slider_h) = match orientation {
        Orientation::Vertical => (
            LayoutOptions {
                margin_left: Some(Dimension::Auto),
                flex_grow: Some(1.0),
                flex_shrink: Some(1.0),
                ..Default::default()
            },
            Dimension::Points(1.0),
            Dimension::Percent(1.0),
        ),
        Orientation::Horizontal => (
            LayoutOptions {
                margin_top: Some(Dimension::Auto),
                flex_grow: Some(1.0),
                flex_shrink: Some(1.0),
                ..Default::default()
            },
            Dimension::Percent(1.0),
            Dimension::Points(1.0),
        ),
    };
    let slider = Renderable::new(
        tree,
        &format!("{id}::slider"),
        slider_layout,
        slider_w,
        slider_h,
        Content::Slider(slider_state),
        None,
    );

    bar.add(tree, start_arrow);
    bar.add(tree, slider);
    bar.add(tree, end_arrow);
    apply_initial_displays(&mut bar, tree);
    bar
}

fn apply_initial_displays(node: &mut Renderable, tree: &mut TaffyTree<MeasureCtx>) {
    node.apply_initial_display(tree);
    for c in &mut node.children {
        apply_initial_displays(c, tree);
    }
}

fn make_box_node(tree: &mut TaffyTree<MeasureCtx>, id: &str, opts: &BoxOptions) -> Renderable {
    let state = BoxRenderableState::from_options(opts);
    let mut layout = opts.layout.clone();
    state.apply_yoga_borders(&mut layout);
    Renderable::new(
        tree,
        id,
        layout,
        opts.width.unwrap_or(Dimension::Auto),
        opts.height.unwrap_or(Dimension::Auto),
        Content::Box(state),
        None,
    )
}

#[derive(Default, Clone, Copy)]
struct PaddingFields {
    padding: Option<Dimension>,
    padding_x: Option<Dimension>,
    padding_y: Option<Dimension>,
    padding_top: Option<Dimension>,
    padding_right: Option<Dimension>,
    padding_bottom: Option<Dimension>,
    padding_left: Option<Dimension>,
}

/// `pickScrollBoxPadding` — ScrollBox.ts:87-99.
fn collect_padding(opts: &BoxOptions) -> PaddingFields {
    PaddingFields {
        padding: opts.layout.padding,
        padding_x: opts.layout.padding_x,
        padding_y: opts.layout.padding_y,
        padding_top: opts.layout.padding_top,
        padding_right: opts.layout.padding_right,
        padding_bottom: opts.layout.padding_bottom,
        padding_left: opts.layout.padding_left,
    }
}

/// `stripScrollBoxPadding` — ScrollBox.ts:101-107.
fn strip_padding(opts: &mut BoxOptions) {
    opts.layout.padding = None;
    opts.layout.padding_x = None;
    opts.layout.padding_y = None;
    opts.layout.padding_top = None;
    opts.layout.padding_right = None;
    opts.layout.padding_bottom = None;
    opts.layout.padding_left = None;
}

fn apply_padding(content: &mut BoxOptions, p: &PaddingFields) {
    let l = &mut content.layout;
    if p.padding.is_some() {
        l.padding = p.padding;
    }
    if p.padding_x.is_some() {
        l.padding_x = p.padding_x;
    }
    if p.padding_y.is_some() {
        l.padding_y = p.padding_y;
    }
    if p.padding_top.is_some() {
        l.padding_top = p.padding_top;
    }
    if p.padding_right.is_some() {
        l.padding_right = p.padding_right;
    }
    if p.padding_bottom.is_some() {
        l.padding_bottom = p.padding_bottom;
    }
    if p.padding_left.is_some() {
        l.padding_left = p.padding_left;
    }
}

/// Options merge (`{...a, ...b}` — later wins per field).
fn merge_box_options(base: &mut BoxOptions, over: &BoxOptions) {
    if over.width.is_some() {
        base.width = over.width;
    }
    if over.height.is_some() {
        base.height = over.height;
    }
    if over.background_color.is_some() {
        base.background_color = over.background_color.clone();
    }
    if over.border.is_some() {
        base.border = over.border.clone();
    }
    if over.border_style.is_some() {
        base.border_style = over.border_style.clone();
    }
    if over.border_color.is_some() {
        base.border_color = over.border_color.clone();
    }
    if over.should_fill.is_some() {
        base.should_fill = over.should_fill;
    }
    if over.title.is_some() {
        base.title = over.title.clone();
    }
    if over.title_color.is_some() {
        base.title_color = over.title_color.clone();
    }
    if over.title_alignment.is_some() {
        base.title_alignment = over.title_alignment.clone();
    }
    if over.bottom_title.is_some() {
        base.bottom_title = over.bottom_title.clone();
    }
    if over.bottom_title_alignment.is_some() {
        base.bottom_title_alignment = over.bottom_title_alignment.clone();
    }
    if over.focused_border_color.is_some() {
        base.focused_border_color = over.focused_border_color.clone();
    }
    merge_layout(&mut base.layout, &over.layout);
}

fn merge_layout(base: &mut LayoutOptions, over: &LayoutOptions) {
    macro_rules! m {
        ($f:ident) => {
            if over.$f.is_some() {
                base.$f = over.$f.clone();
            }
        };
    }
    m!(flex_grow);
    m!(flex_shrink);
    m!(flex_direction);
    m!(flex_wrap);
    m!(align_items);
    m!(justify_content);
    m!(align_self);
    m!(flex_basis);
    m!(position);
    m!(overflow);
    m!(top);
    m!(right);
    m!(bottom);
    m!(left);
    m!(min_width);
    m!(min_height);
    m!(max_width);
    m!(max_height);
    m!(margin);
    m!(margin_x);
    m!(margin_y);
    m!(margin_top);
    m!(margin_right);
    m!(margin_bottom);
    m!(margin_left);
    m!(gap);
    m!(row_gap);
    m!(column_gap);
    m!(border_top);
    m!(border_right);
    m!(border_bottom);
    m!(border_left);
}

// ---------------------------------------------------------------------------
// the state machine — scrollbar setters + owner wiring, over the tree
// ---------------------------------------------------------------------------

/// Read a scrollbar's (scroll_size, scroll_position, viewport_size).
fn bar_state_of(root: &Renderable, bar_id: &str) -> Option<(f32, f32, f32)> {
    root.find(bar_id).and_then(|n| match &n.content {
        Content::ScrollBar(b) => Some((b.scroll_size, b.scroll_position, b.viewport_size)),
        _ => None,
    })
}
/// `updateSliderFromScrollState` — ScrollBar.ts:237-244:
/// `slider.min = 0; slider.max = scrollRange; slider.value =
/// min(_scrollPosition, scrollRange)` — value/max writes fire the onChange
/// chain (Slider.ts:48-84) which routes back here via
/// [`slider_on_change`].
fn update_slider_from_scroll_state(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    effects: &mut WalkEffects,
) {
    let Some(bar_node) = root.find(bar_id) else {
        return;
    };
    let Some((scroll_size, viewport_size)) = (match &bar_node.content {
        Content::ScrollBar(b) => Some((b.scroll_size, b.viewport_size)),
        _ => None,
    }) else {
        return;
    };
    let slider_id = match &bar_node.content {
        Content::ScrollBar(b) => b.slider_id.clone(),
        _ => return,
    };
    let scroll_range = (scroll_size - viewport_size).max(0.0);

    // slider writes in upstream order (ScrollBar.ts:240-243); each write's
    // value-change fires the onChange chain, and `_scrollPosition` is
    // re-read after earlier chains (they round-write it back)
    let min_change = {
        let Some(n) = root.find_mut(&slider_id) else {
            return;
        };
        let Content::Slider(slider) = &mut n.content else {
            return;
        };
        slider.set_min(0.0)
    };
    if let Some(v) = min_change {
        slider_on_change(root, tree, bar_id, v, effects);
    }
    let max_change = {
        let Some(n) = root.find_mut(&slider_id) else {
            return;
        };
        let Content::Slider(slider) = &mut n.content else {
            return;
        };
        slider.set_max(scroll_range)
    };
    if let Some(v) = max_change {
        slider_on_change(root, tree, bar_id, v, effects);
    }
    // `this._scrollPosition` re-read after the chains (ScrollBar.ts:243)
    let pos_now = bar_state_of(root, bar_id).map_or(0.0, |t| t.1);
    let value_change = {
        let Some(n) = root.find_mut(&slider_id) else {
            return;
        };
        let Content::Slider(slider) = &mut n.content else {
            return;
        };
        let (changed, _) = slider.set_value(pos_now.min(scroll_range));
        changed.then_some(slider.value)
    };
    if let Some(v) = value_change {
        slider_on_change(root, tree, bar_id, v, effects);
    }
}

/// The slider's onChange chain — ScrollBar.ts:130-134 +
/// ScrollBox.ts:362-365: `scrollPosition = round(value)`; the owner's
/// onChange sets `content.translateY = -position` (vertical) or
/// `content.translateX = -position` (horizontal) and `updateStickyState`.
fn slider_on_change(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    value: f32,
    effects: &mut WalkEffects,
) {
    let Some(bar_node) = root.find_mut(bar_id) else {
        return;
    };
    let (orientation, new_position) = match &mut bar_node.content {
        Content::ScrollBar(b) => {
            // ScrollBar.ts:131 — _scrollPosition = Math.round(value)
            b.scroll_position = value.round();
            (b.orientation, b.scroll_position)
        }
        _ => return,
    };

    // find the owning scrollbox: the bar id starts with the owner's
    // internal ids — walk up instead: search every ScrollBox for the bar
    let owner = find_owner_of_bar(root, bar_id);
    let Some(owner) = owner else { return };
    let Some(sb) = root.find_mut(&owner) else {
        return;
    };
    let Content::ScrollBox(_, state) = &mut sb.content else {
        return;
    };
    let content_id = state.content_id.clone();

    let Some(content) = root.find_mut(&content_id) else {
        return;
    };
    let translated = match orientation {
        crate::renderables::scrollbar::Orientation::Vertical => {
            content.set_translate_y(-new_position)
        }
        crate::renderables::scrollbar::Orientation::Horizontal => {
            content.set_translate_x(-new_position)
        }
    };
    if translated {
        effects.translated = true;
        let _ = tree;
    }

    // updateStickyState (ScrollBox.ts:364)
    update_sticky_state(root, &owner);
}

fn find_owner_of_bar(root: &Renderable, bar_id: &str) -> Option<String> {
    if let Content::ScrollBox(_, state) = &root.content {
        if state.vbar_id == bar_id || state.hbar_id == bar_id {
            return Some(root.id.0.clone());
        }
    }
    root.children
        .iter()
        .find_map(|c| find_owner_of_bar(c, bar_id))
}

/// `set scrollSize` — ScrollBar.ts:64-70.
pub(crate) fn scrollbar_set_scroll_size(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    value: f32,
    effects: &mut WalkEffects,
) {
    let Some((scroll_size, _, _)) = bar_state_of(root, bar_id) else {
        return;
    };
    if value == scroll_size {
        return;
    }
    if let Some(bar) = root.find_mut(bar_id) {
        if let Content::ScrollBar(b) = &mut bar.content {
            b.scroll_size = value;
        }
    }
    recalculate_visibility(root, tree, bar_id, effects);
    update_slider_from_scroll_state(root, tree, bar_id, effects);
    // `this.scrollPosition = this.scrollPosition` (ScrollBar.ts:69)
    let pos = bar_state_of(root, bar_id).map_or(0.0, |t| t.1);
    scrollbar_set_scroll_position(root, tree, bar_id, pos, effects);
}

/// `set viewportSize` — ScrollBar.ts:83-90.
pub(crate) fn scrollbar_set_viewport_size(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    value: f32,
    effects: &mut WalkEffects,
) {
    let Some((_, _, viewport_size)) = bar_state_of(root, bar_id) else {
        return;
    };
    if value == viewport_size {
        return;
    }
    let slider_id = {
        let Some(bar) = root.find_mut(bar_id) else {
            return;
        };
        let Content::ScrollBar(b) = &mut bar.content else {
            return;
        };
        b.viewport_size = value;
        b.slider_id.clone()
    };
    // slider.viewPortSize = max(1, value) — the CLAMPED setter
    // (ScrollBar.ts:86 → Slider.ts:86-92)
    if let Some(slider_node) = root.find_mut(&slider_id) {
        if let Content::Slider(slider) = &mut slider_node.content {
            slider.set_view_port_size(value.max(1.0));
        }
    }
    recalculate_visibility(root, tree, bar_id, effects);
    update_slider_from_scroll_state(root, tree, bar_id, effects);
    let pos = bar_state_of(root, bar_id).map_or(0.0, |t| t.1);
    scrollbar_set_scroll_position(root, tree, bar_id, pos, effects);
}

/// `set scrollPosition` — ScrollBar.ts:72-81: clamp + round; when the
/// value changes, refresh the slider (events flow through it, 77-79).
pub(crate) fn scrollbar_set_scroll_position(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    value: f32,
    effects: &mut WalkEffects,
) {
    let Some((scroll_size, scroll_position, viewport_size)) = bar_state_of(root, bar_id) else {
        return;
    };
    let new_position =
        crate::renderables::scrollbar::clamp_scroll_position(value, scroll_size, viewport_size);
    if new_position != scroll_position {
        if let Some(bar) = root.find_mut(bar_id) {
            if let Content::ScrollBar(b) = &mut bar.content {
                b.scroll_position = new_position;
            }
        }
        update_slider_from_scroll_state(root, tree, bar_id, effects);
    }
}

/// `recalculateVisibility` — ScrollBar.ts:260-265: unless the user wrote
/// `visible` manually, auto-hide when the content fits.
fn recalculate_visibility(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    effects: &mut WalkEffects,
) {
    let Some(bar) = root.find_mut(bar_id) else {
        return;
    };
    let Content::ScrollBar(b) = &mut bar.content else {
        return;
    };
    if b.manual_visibility {
        return;
    }
    let size_ratio = if b.scroll_size <= b.viewport_size {
        1.0
    } else {
        b.viewport_size / b.scroll_size
    };
    let should_show = size_ratio < 1.0;
    if bar.set_visible(tree, should_show) {
        effects.display_flipped = true;
    }
}

/// `updateStickyState` — ScrollBox.ts:190-222 (+ syncManualScrollState
/// 224-242 + isAtStickyPosition 475-495).
fn update_sticky_state(root: &mut Renderable, owner: &str) {
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    if !state.sticky_scroll {
        // ScrollBox.ts:191-194
        if let Some(sb2) = root.find_mut(owner) {
            if let Content::ScrollBox(_, st) = &mut sb2.content {
                st.has_manual_scroll = false;
            }
        }
        return;
    }
    let (content_h, content_w, vp_h, vp_w) = scroll_extents(root, state);
    let max_scroll_top = (content_h - vp_h).max(0.0);
    let max_scroll_left = (content_w - vp_w).max(0.0);
    let scroll_top = scroll_top_of(root, state);
    let scroll_left = scroll_left_of(root, state);

    let (new_top, new_bottom) = if scroll_top <= 0.0 {
        (true, false)
    } else if scroll_top >= max_scroll_top {
        (false, true)
    } else {
        (false, false)
    };
    let (new_left, new_right) = if scroll_left <= 0.0 {
        (true, false)
    } else if scroll_left >= max_scroll_left {
        (false, true)
    } else {
        (false, false)
    };
    if let Some(sb2) = root.find_mut(owner) {
        if let Content::ScrollBox(_, st) = &mut sb2.content {
            st.sticky_scroll_top = new_top;
            st.sticky_scroll_bottom = new_bottom;
            st.sticky_scroll_left = new_left;
            st.sticky_scroll_right = new_right;
        }
    }
    sync_manual_scroll_state(root, owner);
}

/// `syncManualScrollState` — ScrollBox.ts:224-242.
fn sync_manual_scroll_state(root: &mut Renderable, owner: &str) {
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    if !state.sticky_scroll {
        if let Some(sb2) = root.find_mut(owner) {
            if let Content::ScrollBox(_, st) = &mut sb2.content {
                st.has_manual_scroll = false;
            }
        }
        return;
    }
    let (content_h, content_w, vp_h, vp_w) = scroll_extents(root, state);
    let max_scroll_top = (content_h - vp_h).max(0.0);
    let max_scroll_left = (content_w - vp_w).max(0.0);
    let has_scrollable_content = max_scroll_top > 1.0 || max_scroll_left > 1.0;

    if state.is_applying_sticky_scroll {
        // ScrollBox.ts:234-239
        let at_sticky = is_at_sticky_position(root, state);
        let manual = state.has_manual_scroll;
        if manual && has_scrollable_content && at_sticky {
            if let Some(sb2) = root.find_mut(owner) {
                if let Content::ScrollBox(_, st) = &mut sb2.content {
                    st.has_manual_scroll = false;
                }
            }
        }
        return;
    }
    let at_sticky = is_at_sticky_position(root, state);
    if let Some(sb2) = root.find_mut(owner) {
        if let Content::ScrollBox(_, st) = &mut sb2.content {
            st.has_manual_scroll = has_scrollable_content && !at_sticky;
        }
    }
}

/// `isAtStickyPosition` — ScrollBox.ts:475-495.
fn is_at_sticky_position(root: &Renderable, state: &ScrollBoxState) -> bool {
    let Some(start) = state.sticky_start else {
        return false;
    };
    if !state.sticky_scroll {
        return false;
    }
    let (content_h, content_w, vp_h, vp_w) = scroll_extents(root, state);
    let max_scroll_top = (content_h - vp_h).max(0.0);
    let max_scroll_left = (content_w - vp_w).max(0.0);
    match start {
        StickyStart::Top => scroll_top_of(root, state) == 0.0,
        StickyStart::Bottom => scroll_top_of(root, state) >= max_scroll_top,
        StickyStart::Left => scroll_left_of(root, state) == 0.0,
        StickyStart::Right => scroll_left_of(root, state) >= max_scroll_left,
    }
}

fn scroll_extents(root: &Renderable, state: &ScrollBoxState) -> (f32, f32, f32, f32) {
    let content = root.find(&state.content_id);
    let viewport = root.find(&state.viewport_id);
    (
        content.map_or(0.0, |n| n.h),
        content.map_or(0.0, |n| n.w),
        viewport.map_or(0.0, |n| n.h),
        viewport.map_or(0.0, |n| n.w),
    )
}

fn scroll_top_of(root: &Renderable, state: &ScrollBoxState) -> f32 {
    root.find(&state.vbar_id)
        .and_then(|n| match &n.content {
            Content::ScrollBar(b) => Some(b.scroll_position),
            _ => None,
        })
        .unwrap_or(0.0)
}

fn scroll_left_of(root: &Renderable, state: &ScrollBoxState) -> f32 {
    root.find(&state.hbar_id)
        .and_then(|n| match &n.content {
            Content::ScrollBar(b) => Some(b.scroll_position),
            _ => None,
        })
        .unwrap_or(0.0)
}

/// `applyStickyStart` — ScrollBox.ts:244-273.
fn apply_sticky_start(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    start: StickyStart,
    effects: &mut WalkEffects,
) {
    // save + set the guard (245-246)
    let was_applying = {
        let Some(sb) = root.find_mut(owner) else {
            return;
        };
        let Content::ScrollBox(_, st) = &mut sb.content else {
            return;
        };
        let was = st.is_applying_sticky_scroll;
        st.is_applying_sticky_scroll = true;
        was
    };
    let (vbar, hbar) = {
        let Some(sb) = root.find(owner) else { return };
        let Content::ScrollBox(_, st) = &sb.content else {
            return;
        };
        (st.vbar_id.clone(), st.hbar_id.clone())
    };

    match start {
        StickyStart::Top => {
            if let Some(sb) = root.find_mut(owner) {
                if let Content::ScrollBox(_, st) = &mut sb.content {
                    st.sticky_scroll_top = true;
                    st.sticky_scroll_bottom = false;
                }
            }
            scrollbar_set_scroll_position(root, tree, &vbar, 0.0, effects);
        }
        StickyStart::Bottom => {
            let (content_h, vp_h) = {
                let Some(sb) = root.find(owner) else { return };
                let Content::ScrollBox(_, st) = &sb.content else {
                    return;
                };
                let (ch, _, vh, _) = scroll_extents(root, st);
                (ch, vh)
            };
            if let Some(sb) = root.find_mut(owner) {
                if let Content::ScrollBox(_, st) = &mut sb.content {
                    st.sticky_scroll_top = false;
                    st.sticky_scroll_bottom = true;
                }
            }
            // ScrollBox.ts:257 — max(0, scrollHeight - viewport.height)
            scrollbar_set_scroll_position(root, tree, &vbar, (content_h - vp_h).max(0.0), effects);
        }
        StickyStart::Left => {
            if let Some(sb) = root.find_mut(owner) {
                if let Content::ScrollBox(_, st) = &mut sb.content {
                    st.sticky_scroll_left = true;
                    st.sticky_scroll_right = false;
                }
            }
            scrollbar_set_scroll_position(root, tree, &hbar, 0.0, effects);
        }
        StickyStart::Right => {
            let (content_w, vp_w) = {
                let Some(sb) = root.find(owner) else { return };
                let Content::ScrollBox(_, st) = &sb.content else {
                    return;
                };
                let (_, cw, _, vw) = scroll_extents(root, st);
                (cw, vw)
            };
            if let Some(sb) = root.find_mut(owner) {
                if let Content::ScrollBox(_, st) = &mut sb.content {
                    st.sticky_scroll_left = false;
                    st.sticky_scroll_right = true;
                }
            }
            scrollbar_set_scroll_position(root, tree, &hbar, (content_w - vp_w).max(0.0), effects);
        }
    }

    // restore the saved guard (270-272)
    if let Some(sb) = root.find_mut(owner) {
        if let Content::ScrollBox(_, st) = &mut sb.content {
            st.is_applying_sticky_scroll = was_applying;
        }
    }
}

/// `recalculateBarProps` — ScrollBox.ts:749-805. `stale_content` carries
/// the at-fire-time content size when the VIEWPORT fired (upstream hooks
/// run inline, so the content below is not yet updated — pinned against
/// the real 0.5.14 oracle).
pub(crate) fn apply_recalc(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    stale_content: Option<(f32, f32)>,
    effects: &mut WalkEffects,
) {
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    let vbar = state.vbar_id.clone();
    let hbar = state.hbar_id.clone();
    let content_id = state.content_id.clone();
    let viewport_id = state.viewport_id.clone();
    let sticky = state.sticky_scroll;

    let (cw, ch) = stale_content.unwrap_or_else(|| {
        root.find(&content_id)
            .map(|n| (n.w, n.h))
            .unwrap_or((0.0, 0.0))
    });
    let (vw, vh) = root
        .find(&viewport_id)
        .map(|n| (n.w, n.h))
        .unwrap_or((0.0, 0.0));

    // guard (750-752) — save + set, restore at the end (789-791)
    let was_applying = {
        let Some(sb2) = root.find_mut(owner) else {
            return;
        };
        let Content::ScrollBox(_, st) = &mut sb2.content else {
            return;
        };
        let was = st.is_applying_sticky_scroll;
        st.is_applying_sticky_scroll = true;
        was
    };

    // ScrollBox.ts:755-758
    scrollbar_set_scroll_size(root, tree, &vbar, ch, effects);
    scrollbar_set_viewport_size(root, tree, &vbar, vh, effects);
    scrollbar_set_scroll_size(root, tree, &hbar, cw, effects);
    scrollbar_set_viewport_size(root, tree, &hbar, vw, effects);

    // sticky maintenance (760-788)
    if sticky {
        let (flags, manual, start, extents, top_now, left_now) = {
            let Some(sb2) = root.find(owner) else { return };
            let Content::ScrollBox(_, st) = &sb2.content else {
                return;
            };
            (
                (
                    st.sticky_scroll_top,
                    st.sticky_scroll_bottom,
                    st.sticky_scroll_left,
                    st.sticky_scroll_right,
                ),
                st.has_manual_scroll,
                st.sticky_start,
                scroll_extents(root, st),
                scroll_top_of(root, st),
                scroll_left_of(root, st),
            )
        };
        let (sticky_top, sticky_bottom, sticky_left, sticky_right) = flags;
        let (content_h2, content_w2, vp_h2, vp_w2) = extents;
        let new_max_top = (content_h2 - vp_h2).max(0.0);
        let new_max_left = (content_w2 - vp_w2).max(0.0);
        let scroll_top_now = top_now;
        let scroll_left_now = left_now;

        if let Some(s) = start {
            if !manual {
                apply_sticky_start(root, tree, owner, s, effects);
            } else {
                // re-engage at the sticky edge (767-775)
                let reengage = match s {
                    StickyStart::Top => new_max_top > 0.0 && scroll_top_now <= 0.0,
                    StickyStart::Bottom => new_max_top > 0.0 && scroll_top_now >= new_max_top - 1.0,
                    StickyStart::Left => new_max_left > 0.0 && scroll_left_now <= 0.0,
                    StickyStart::Right => {
                        new_max_left > 0.0 && scroll_left_now >= new_max_left - 1.0
                    }
                };
                if reengage {
                    if let Some(sb2) = root.find_mut(owner) {
                        if let Content::ScrollBox(_, st) = &mut sb2.content {
                            st.has_manual_scroll = false;
                        }
                    }
                    apply_sticky_start(root, tree, owner, s, effects);
                }
            }
        } else if !manual {
            // 775-787
            if sticky_top {
                set_scroll_top(root, tree, owner, 0.0, effects);
            } else if sticky_bottom && new_max_top > 0.0 {
                set_scroll_top(root, tree, owner, new_max_top, effects);
            }
            if sticky_left {
                set_scroll_left(root, tree, owner, 0.0, effects);
            } else if sticky_right && new_max_left > 0.0 {
                set_scroll_left(root, tree, owner, new_max_left, effects);
            }
        }
    }

    // restore guard (789-791)
    if let Some(sb2) = root.find_mut(owner) {
        if let Content::ScrollBox(_, st) = &mut sb2.content {
            st.is_applying_sticky_scroll = was_applying;
        }
    }
}

// ---------------------------------------------------------------------------
// public API over the tree (the `scrollTop`/`scrollBy`/… surface v2 uses)
// ---------------------------------------------------------------------------

/// `set scrollTop` — ScrollBox.ts:168-171.
pub fn set_scroll_top(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    value: f32,
    effects: &mut WalkEffects,
) {
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    let vbar = state.vbar_id.clone();
    scrollbar_set_scroll_position(root, tree, &vbar, value, effects);
    update_sticky_state(root, owner);
}

/// `set scrollLeft` — ScrollBox.ts:177-180.
pub fn set_scroll_left(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    value: f32,
    effects: &mut WalkEffects,
) {
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    let hbar = state.hbar_id.clone();
    scrollbar_set_scroll_position(root, tree, &hbar, value, effects);
    update_sticky_state(root, owner);
}

/// `get scrollTop` — ScrollBox.ts:164-166.
#[must_use]
pub fn scroll_top(root: &Renderable, owner: &str) -> Option<f32> {
    let sb = root.find(owner)?;
    let Content::ScrollBox(_, state) = &sb.content else {
        return None;
    };
    Some(scroll_top_of(root, state))
}

#[must_use]
pub fn scroll_left(root: &Renderable, owner: &str) -> Option<f32> {
    let sb = root.find(owner)?;
    let Content::ScrollBox(_, state) = &sb.content else {
        return None;
    };
    Some(scroll_left_of(root, state))
}

/// `ScrollBar.scrollBy` — ScrollBar.ts:246-258: unit multiplier then the
/// clamped position setter.
pub fn scrollbar_scroll_by(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    bar_id: &str,
    delta: f32,
    unit: ScrollUnit,
    effects: &mut WalkEffects,
) {
    let Some((scroll_size, scroll_position, viewport_size)) = bar_state_of(root, bar_id) else {
        return;
    };
    let scroll_step = root
        .find(bar_id)
        .and_then(|n| match &n.content {
            Content::ScrollBar(b) => b.scroll_step,
            _ => None,
        })
        .unwrap_or(1.0);
    let multiplier = match unit {
        ScrollUnit::Absolute => 1.0,
        ScrollUnit::Viewport => viewport_size,
        ScrollUnit::Content => scroll_size,
        ScrollUnit::Step => scroll_step,
    };
    let resolved = multiplier * delta;
    scrollbar_set_scroll_position(root, tree, bar_id, scroll_position + resolved, effects);
}

/// `ScrollBox.scrollBy` — ScrollBox.ts:404-413.
pub fn scroll_by(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    dx: f32,
    dy: f32,
    unit: ScrollUnit,
    effects: &mut WalkEffects,
) {
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    let vbar = state.vbar_id.clone();
    let hbar = state.hbar_id.clone();
    scrollbar_scroll_by(root, tree, &vbar, dy, unit, effects);
    scrollbar_scroll_by(root, tree, &hbar, dx, unit, effects);
}

/// `ScrollBox.scrollTo` — ScrollBox.ts:464-473.
pub fn scroll_to(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    x: f32,
    y: f32,
    effects: &mut WalkEffects,
) {
    set_scroll_top(root, tree, owner, y, effects);
    set_scroll_left(root, tree, owner, x, effects);
}

/// `scrollChildIntoView` — ScrollBox.ts:415-462: nearest-delta scroll to
/// reveal a descendant (child coords are root-relative via the `x`/`y`
/// getters, Renderable.ts:601-614).
pub fn scroll_child_into_view(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    child_id: &str,
    effects: &mut WalkEffects,
) {
    /// `getNearestDelta` — ScrollBox.ts:419-442 (exact).
    fn nearest_delta(
        element_start: f32,
        element_end: f32,
        viewport_start: f32,
        viewport_end: f32,
    ) -> f32 {
        let element_size = element_end - element_start;
        let viewport_size = viewport_end - viewport_start;
        let element_start_outside = element_start < viewport_start;
        let element_end_outside = element_end > viewport_end;
        if element_start_outside && element_end_outside {
            return 0.0;
        }
        if (element_start_outside && element_size < viewport_size)
            || (element_end_outside && element_size > viewport_size)
        {
            return element_start - viewport_start;
        }
        if (element_start_outside && element_size > viewport_size)
            || (element_end_outside && element_size < viewport_size)
        {
            return element_end - viewport_end;
        }
        0.0
    }

    // absolute (root-relative) positions: accumulate from the tree root
    fn abs_pos(root: &Renderable, id: &str) -> Option<(f32, f32, f32, f32)> {
        fn walk(n: &Renderable, id: &str, ox: f32, oy: f32) -> Option<(f32, f32, f32, f32)> {
            let x = ox + n.x + n.translate_x;
            let y = oy + n.y + n.translate_y;
            if n.id.0 == id {
                return Some((x, y, n.w, n.h));
            }
            for c in &n.children {
                if let Some(r) = walk(c, id, x, y) {
                    return Some(r);
                }
            }
            None
        }
        walk(root, id, 0.0, 0.0)
    }

    let Some((cx, cy, cw, chh)) = abs_pos(root, child_id) else {
        return;
    };
    let Some(sb) = root.find(owner) else { return };
    let Content::ScrollBox(_, state) = &sb.content else {
        return;
    };
    let Some((vx, vy, vw, vh)) = abs_pos(root, &state.viewport_id) else {
        return;
    };

    let dy = nearest_delta(cy, cy + chh, vy, vy + vh);
    let dx = nearest_delta(cx, cx + cw, vx, vx + vw);
    if dx != 0.0 || dy != 0.0 {
        scroll_by(root, tree, owner, dx, dy, ScrollUnit::Absolute, effects);
    }
}

// ---------------------------------------------------------------------------
// event seams (the events lane owns the parsers; these are the ScrollBox
// tie-ins)
// ---------------------------------------------------------------------------

/// Scroll wheel direction — MouseEvent.scroll.direction (renderer.ts:653+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// The slice of `MouseEvent` the ScrollBox consumes (renderer.ts:653-696).
pub struct ScrollEvent {
    pub direction: ScrollDirection,
    pub delta: f64,
    pub shift: bool,
    /// monotonic ms for the acceleration curve
    pub now_ms: Option<f64>,
}

/// `onMouseEvent`'s scroll branch — ScrollBox.ts:542-584 (exact): shift
/// remaps the axis, the acceleration curve scales the delta, fractional
/// accumulate + truncate keeps sub-cell deltas for the next tick.
pub fn on_scroll_event(
    root: &mut Renderable,
    tree: &mut TaffyTree<MeasureCtx>,
    owner: &str,
    event: &ScrollEvent,
    effects: &mut WalkEffects,
) {
    let mut dir = event.direction;
    if event.shift {
        dir = match dir {
            ScrollDirection::Up => ScrollDirection::Left,
            ScrollDirection::Down => ScrollDirection::Right,
            ScrollDirection::Right => ScrollDirection::Down,
            ScrollDirection::Left => ScrollDirection::Up,
        };
    }
    // multiplier = this.scrollAccel.tick(now) (ScrollBox.ts:549-551)
    let multiplier = {
        let Some(sb) = root.find_mut(owner) else {
            return;
        };
        let Content::ScrollBox(_, state) = &mut sb.content else {
            return;
        };
        let mut accel = state.scroll_accel.lock().expect("scroll accel mutex");
        accel.tick(event.now_ms)
    };
    let scroll_amount = event.delta * multiplier;

    let (vbar, hbar) = {
        let Some(sb) = root.find(owner) else { return };
        let Content::ScrollBox(_, state) = &sb.content else {
            return;
        };
        (state.vbar_id.clone(), state.hbar_id.clone())
    };
    let _ = (&vbar, &hbar);
    let axis = if matches!(dir, ScrollDirection::Up | ScrollDirection::Down) {
        Axis::Vertical
    } else {
        Axis::Horizontal
    };
    let sign = match dir {
        ScrollDirection::Up | ScrollDirection::Left => -1.0,
        ScrollDirection::Down | ScrollDirection::Right => 1.0,
    };

    // accumulate on the axis, scroll the integer part (ScrollBox.ts:553-581)
    {
        let Some(sb) = root.find_mut(owner) else {
            return;
        };
        let Content::ScrollBox(_, state) = &mut sb.content else {
            return;
        };
        match axis {
            Axis::Vertical => state.scroll_accumulator_y += sign * scroll_amount,
            Axis::Horizontal => state.scroll_accumulator_x += sign * scroll_amount,
        }
    }
    let integer = {
        let Some(sb) = root.find(owner) else { return };
        let Content::ScrollBox(_, state) = &sb.content else {
            return;
        };
        match axis {
            Axis::Vertical => state.scroll_accumulator_y.trunc(),
            Axis::Horizontal => state.scroll_accumulator_x.trunc(),
        }
    };
    if integer != 0.0 {
        match axis {
            Axis::Vertical => {
                let top = scroll_top(root, owner).unwrap_or(0.0);
                set_scroll_top(root, tree, owner, top + integer as f32, effects);
            }
            Axis::Horizontal => {
                let left = scroll_left(root, owner).unwrap_or(0.0);
                set_scroll_left(root, tree, owner, left + integer as f32, effects);
            }
        }
        if let Some(sb) = root.find_mut(owner) {
            if let Content::ScrollBox(_, state) = &mut sb.content {
                match axis {
                    Axis::Vertical => state.scroll_accumulator_y -= integer,
                    Axis::Horizontal => state.scroll_accumulator_x -= integer,
                }
            }
        }
    }
    // syncManualScrollState (583)
    sync_manual_scroll_state(root, owner);
}

enum Axis {
    Vertical,
    Horizontal,
}

/// `startAutoScroll` / `updateAutoScroll` / `stopAutoScroll` /
/// `handleAutoScroll` — ScrollBox.ts:615-747. The drag/selection plumbing
/// belongs to the events lane; the math is ported here.
pub mod auto_scroll {
    use super::ScrollBoxState;

    /// `getAutoScrollDirectionX` — ScrollBox.ts:701-713.
    #[must_use]
    pub fn direction_x(
        state: &ScrollBoxState,
        mouse_x: f32,
        scroll_left: f32,
        width: f32,
        max_scroll_left: f32,
    ) -> f32 {
        let relative_x = mouse_x;
        let dist_to_left = relative_x;
        let dist_to_right = width - relative_x;
        if dist_to_left <= state.auto_scroll_threshold_horizontal {
            if scroll_left > 0.0 {
                -1.0
            } else {
                0.0
            }
        } else if dist_to_right <= state.auto_scroll_threshold_horizontal {
            if scroll_left < max_scroll_left {
                1.0
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    /// `getAutoScrollDirectionY` — ScrollBox.ts:715-727.
    #[must_use]
    pub fn direction_y(
        state: &ScrollBoxState,
        mouse_y: f32,
        scroll_top: f32,
        height: f32,
        max_scroll_top: f32,
    ) -> f32 {
        let relative_y = mouse_y;
        let dist_to_top = relative_y;
        let dist_to_bottom = height - relative_y;
        if dist_to_top <= state.auto_scroll_threshold_vertical {
            if scroll_top > 0.0 {
                -1.0
            } else {
                0.0
            }
        } else if dist_to_bottom <= state.auto_scroll_threshold_vertical {
            if scroll_top < max_scroll_top {
                1.0
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    /// `getAutoScrollSpeed` — ScrollBox.ts:729-747.
    #[must_use]
    pub fn speed(
        state: &ScrollBoxState,
        mouse_x: f32,
        mouse_y: f32,
        width: f32,
        height: f32,
    ) -> f32 {
        let dist_to_left = mouse_x;
        let dist_to_right = width - mouse_x;
        let dist_to_top = mouse_y;
        let dist_to_bottom = height - mouse_y;
        let min_distance = dist_to_left
            .min(dist_to_right)
            .min(dist_to_top)
            .min(dist_to_bottom);
        if min_distance <= 1.0 {
            state.auto_scroll_speed_fast
        } else if min_distance <= 2.0 {
            state.auto_scroll_speed_medium
        } else {
            state.auto_scroll_speed_slow
        }
    }

    /// `handleAutoScroll`'s accumulation step — ScrollBox.ts:663-699:
    /// `speed * (deltaTime / 1000)` accumulated per axis, truncated to
    /// integer scrolls.
    #[must_use]
    pub fn step(
        state: &ScrollBoxState,
        delta_time_ms: f64,
        dir_x: f32,
        dir_y: f32,
    ) -> (f64, f64, i32, i32) {
        let amount = f64::from(state.cached_auto_scroll_speed) * (delta_time_ms / 1000.0);
        let ax = state.auto_scroll_accumulator_x + f64::from(dir_x) * amount;
        let ay = state.auto_scroll_accumulator_y + f64::from(dir_y) * amount;
        let ix = ax.trunc();
        let iy = ay.trunc();
        (ax - ix, ay - iy, ix as i32, iy as i32)
    }
}

/// `ScrollBar.handleKeyPress` — ScrollBar.ts:267-304: the key → scrollBy
/// mapping. Returns the (delta, unit) when the key is consumed.
#[must_use]
pub fn scrollbar_key_to_scroll(orientation: Orientation, key: &str) -> Option<(f32, ScrollUnit)> {
    use ScrollUnit::Viewport;
    let v = orientation == Orientation::Vertical;
    match key {
        "left" | "h" if !v => Some((-1.0 / 5.0, Viewport)),
        "right" | "l" if !v => Some((1.0 / 5.0, Viewport)),
        "up" | "k" if v => Some((-1.0 / 5.0, Viewport)),
        "down" | "j" if v => Some((1.0 / 5.0, Viewport)),
        "pageup" => Some((-1.0 / 2.0, Viewport)),
        "pagedown" => Some((1.0 / 2.0, Viewport)),
        "home" => Some((-1.0, ScrollUnit::Content)),
        "end" => Some((1.0, ScrollUnit::Content)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(x: f32, y: f32, w: f32, h: f32) -> ViewportObject {
        ViewportObject {
            screen_x: x,
            screen_y: y,
            width: w,
            height: h,
            z_index: 0,
        }
    }

    #[test]
    fn small_object_lists_short_circuit() {
        // objects-in-viewport.ts:49-51 — under 16 objects, all visible
        let objects: Vec<ViewportObject> = (0..15)
            .map(|i| obj(0.0, i as f32 * 100.0, 10.0, 1.0))
            .collect();
        let out = get_objects_in_viewport(
            ViewportBounds {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
            &objects,
            "column",
            0.0,
        );
        assert_eq!(out.len(), 15);
    }

    #[test]
    fn window_selection_over_sorted_list() {
        // 30 rows of height 1 at y=0..29; viewport y=10..15 → objects 10..=14
        let objects: Vec<ViewportObject> = (0..30).map(|i| obj(0.0, i as f32, 10.0, 1.0)).collect();
        let out = get_objects_in_viewport(
            ViewportBounds {
                x: 0.0,
                y: 10.0,
                width: 10.0,
                height: 5.0,
            },
            &objects,
            "column",
            0.0,
        );
        let ys: Vec<f32> = out.iter().map(|o| o.screen_y).collect();
        assert_eq!(ys, vec![10.0, 11.0, 12.0, 13.0, 14.0]);
    }

    #[test]
    fn gap_viewport_finds_nothing_between_objects() {
        // 20 objects spaced 3 apart; the viewport lands exactly on y=30
        let mut sorted: Vec<ViewportObject> = (0..20)
            .map(|i| obj(0.0, (i * 3) as f32, 10.0, 1.0))
            .collect();
        sorted.sort_by(|a, b| a.screen_y.partial_cmp(&b.screen_y).unwrap());
        let out = get_objects_in_viewport(
            ViewportBounds {
                x: 0.0,
                y: 30.0,
                width: 10.0,
                height: 1.0,
            },
            &sorted,
            "column",
            0.0,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].screen_y, 30.0);
    }

    #[test]
    fn key_map_matches_scrollbar_ts() {
        use crate::renderables::scrollbar::Orientation::{Horizontal, Vertical};
        assert_eq!(
            scrollbar_key_to_scroll(Vertical, "down"),
            Some((1.0 / 5.0, ScrollUnit::Viewport))
        );
        assert_eq!(scrollbar_key_to_scroll(Vertical, "right"), None);
        assert_eq!(
            scrollbar_key_to_scroll(Horizontal, "left"),
            Some((-1.0 / 5.0, ScrollUnit::Viewport))
        );
        assert_eq!(
            scrollbar_key_to_scroll(Vertical, "home"),
            Some((-1.0, ScrollUnit::Content))
        );
    }
}
