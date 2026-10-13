//! Prop application — the port of upstream `setProperty`
//! (reconciler.ts:230-367) plus the per-tag option builders
//! (`BoxOptions`/`TextOptions`/`ScrollBoxOptions` from a flat prop
//! map — upstream constructs renderables from options objects the JSX
//! props feed field-by-field).
//!
//! Minimal patching (universal.js:198-206, `spreadExpression`): a prop
//! is applied only when its value differs from the last applied one
//! (`value === prevProps[prop]` → continue). Event handlers compare by
//! identity (upstream compares `prev` handler references).
//!
//! Data-ish props (colors, layout, content) are applied by REBUILDING
//! the renderable's state from the full prop map — upstream assigns
//! the single field (`node[name] = value`); since the prop map holds
//! exactly the last-set values, the rebuilt state is field-identical.

use crate::element::{Element, PropValue, Tag};
use opentui_core::color::parse_color;
use opentui_core::layout::{Dimension, LayoutOptions};
use opentui_core::renderable::{Content, MeasureKind};
use opentui_core::renderables::box_::{BorderSpec, BoxOptions};
use opentui_core::renderables::scrollbox::ScrollBoxOptions;
use opentui_core::renderables::text::TextRenderableState;
use opentui_core::renderables::text::{ContentInput, TextOptions, WrapMode};
use opentui_core::renderer::CliRenderer;
use opentui_core::styled_text::{string_to_styled_text, TextChunk};
use opentui_core::text_buffer::TextAlign;
use opentui_core::text_node::{TextNodeOptions, TextNodeRenderable};

/// The last-applied prop map (prevProps in universal.js:192-208).
pub type PropMap = Vec<(String, PropValue)>;

pub fn prop_get<'a>(map: &'a PropMap, name: &str) -> Option<&'a PropValue> {
    map.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

pub fn prop_set(map: &mut PropMap, name: &str, value: PropValue) {
    if let Some(slot) = map.iter_mut().find(|(n, _)| n == name) {
        slot.1 = value;
    } else {
        map.push((name.to_owned(), value));
    }
}

/// A dimension prop: number | "10" | "50%" (upstream `DimensionInput`).
pub fn parse_dimension(v: &PropValue) -> Option<Dimension> {
    match v {
        PropValue::Num(n) => Some(Dimension::Points(*n)),
        PropValue::Str(s) | PropValue::Dimension(s) => parse_dimension_str(s),
        _ => None,
    }
}

fn parse_dimension_str(s: &str) -> Option<Dimension> {
    if let Some(pct) = s.strip_suffix('%') {
        pct.trim().parse::<f32>().ok().map(Dimension::Percent)
    } else {
        s.trim().parse::<f32>().ok().map(Dimension::Points)
    }
}

fn f32_of(v: &PropValue) -> Option<f32> {
    match v {
        PropValue::Num(n) => Some(*n),
        PropValue::Str(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn bool_of(v: &PropValue) -> Option<bool> {
    match v {
        PropValue::Bool(b) => Some(*b),
        PropValue::Str(s) => match s.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        PropValue::Num(n) => Some(*n != 0.0),
        _ => None,
    }
}

fn str_of(v: &PropValue) -> Option<&str> {
    match v {
        PropValue::Str(s) | PropValue::Color(s) | PropValue::Dimension(s) => Some(s),
        PropValue::Text(s) => Some(s),
        _ => None,
    }
}

/// attributes — number bitmask or name list (`createTextAttributes`,
/// utils.ts:4-29; the JSON harness shares this vocabulary).
pub fn parse_attributes(v: &PropValue) -> Option<u32> {
    match v {
        PropValue::Attrs(bits) => Some(*bits),
        PropValue::Num(n) => Some(*n as u32),
        PropValue::Str(_) | PropValue::Style(_) => {
            // "bold,italic" or style-ish list form
            let names: Vec<String> = match v {
                PropValue::Str(s) => s.split(',').map(str::to_owned).collect(),
                PropValue::Style(entries) => entries.iter().map(|(n, _)| n.clone()).collect(),
                _ => unreachable!(),
            };
            let mut bits = 0u32;
            for name in names {
                bits |= match name.trim() {
                    "bold" => 1,
                    "dim" => 1 << 1,
                    "italic" => 1 << 2,
                    "underline" => 1 << 3,
                    "blink" => 1 << 4,
                    "inverse" | "reverse" => 1 << 5,
                    "hidden" => 1 << 6,
                    "strikethrough" => 1 << 7,
                    other => return Err(other).ok()?,
                };
            }
            Some(bits)
        }
        _ => None,
    }
}

/// Layout-ish props → [`LayoutOptions`] (upstream option names pass
/// through to `applyYogaProps`, Renderable.ts:700-760).
pub fn apply_layout(layout: &mut LayoutOptions, name: &str, value: &PropValue) {
    let dim = || parse_dimension(value);
    let num = || f32_of(value);
    match name {
        "flexDirection" => layout.flex_direction = str_of(value).map(str::to_owned),
        "flexGrow" => layout.flex_grow = num(),
        "flexShrink" => layout.flex_shrink = num(),
        "flexBasis" => layout.flex_basis = dim(),
        "flexWrap" => layout.flex_wrap = str_of(value).map(str::to_owned),
        "alignItems" => layout.align_items = str_of(value).map(str::to_owned),
        "justifyContent" => layout.justify_content = str_of(value).map(str::to_owned),
        "alignSelf" => layout.align_self = str_of(value).map(str::to_owned),
        "position" => layout.position = str_of(value).map(str::to_owned),
        "overflow" => layout.overflow = str_of(value).map(str::to_owned),
        "top" => layout.top = dim(),
        "right" => layout.right = dim(),
        "bottom" => layout.bottom = dim(),
        "left" => layout.left = dim(),
        "minWidth" => layout.min_width = dim(),
        "maxWidth" => layout.max_width = dim(),
        "minHeight" => layout.min_height = dim(),
        "maxHeight" => layout.max_height = dim(),
        "margin" => layout.margin = dim(),
        "marginX" => layout.margin_x = dim(),
        "marginY" => layout.margin_y = dim(),
        "marginTop" => layout.margin_top = dim(),
        "marginRight" => layout.margin_right = dim(),
        "marginBottom" => layout.margin_bottom = dim(),
        "marginLeft" => layout.margin_left = dim(),
        "padding" => layout.padding = dim(),
        "paddingX" => layout.padding_x = dim(),
        "paddingY" => layout.padding_y = dim(),
        "paddingTop" => layout.padding_top = dim(),
        "paddingRight" => layout.padding_right = dim(),
        "paddingBottom" => layout.padding_bottom = dim(),
        "paddingLeft" => layout.padding_left = dim(),
        _ => {}
    }
}

pub fn is_layout_prop(name: &str) -> bool {
    matches!(
        name,
        "flexDirection"
            | "flexGrow"
            | "flexShrink"
            | "flexBasis"
            | "flexWrap"
            | "alignItems"
            | "justifyContent"
            | "alignSelf"
            | "position"
            | "overflow"
            | "top"
            | "right"
            | "bottom"
            | "left"
            | "minWidth"
            | "maxWidth"
            | "minHeight"
            | "maxHeight"
            | "margin"
            | "marginX"
            | "marginY"
            | "marginTop"
            | "marginRight"
            | "marginBottom"
            | "marginLeft"
            | "padding"
            | "paddingX"
            | "paddingY"
            | "paddingTop"
            | "paddingRight"
            | "paddingBottom"
            | "paddingLeft"
            | "width"
            | "height"
    )
}

fn border_spec(v: &PropValue) -> Option<BorderSpec> {
    match v {
        PropValue::Bool(true) => Some(BorderSpec::All),
        PropValue::Bool(false) => Some(BorderSpec::Off),
        PropValue::Str(s) => {
            // "top,left" side list (Box.ts:20 border: boolean | string[])
            let sides: Vec<String> = s.split(',').map(|x| x.trim().to_owned()).collect();
            if sides.is_empty() {
                Some(BorderSpec::Off)
            } else {
                Some(BorderSpec::Sides(sides))
            }
        }
        _ => None,
    }
}

/// Content: string | styled chunks | prop-encoded chunks.
fn content_input(v: &PropValue) -> ContentInput {
    match v {
        PropValue::None => ContentInput::None,
        PropValue::Str(s) | PropValue::Text(s) => ContentInput::Str(s.clone()),
        PropValue::Styled(st) => ContentInput::Styled(st.clone()),
        PropValue::Style(entries) => {
            // [{text, fg?, bg?, attributes?}] via the Style bag form
            let mut chunks = Vec::new();
            for (text, meta) in entries {
                let mut chunk = TextChunk {
                    text: text.clone(),
                    fg: None,
                    bg: None,
                    attributes: 0,
                    link: None,
                };
                if let PropValue::Style(pairs) = meta {
                    for (k, val) in pairs {
                        match k.as_str() {
                            "fg" => chunk.fg = str_of(val).map(parse_color),
                            "bg" => chunk.bg = str_of(val).map(parse_color),
                            "attributes" => chunk.attributes = parse_attributes(val).unwrap_or(0),
                            _ => {}
                        }
                    }
                }
                chunks.push(chunk);
            }
            ContentInput::Styled(opentui_core::styled_text::StyledText::new(chunks))
        }
        _ => ContentInput::None,
    }
}

/// Build a [`BoxOptions`] from a prop map (Box.ts option names).
#[must_use]
pub fn box_options(props: &PropMap) -> BoxOptions {
    let mut opts = BoxOptions::default();
    for (name, v) in props {
        match name.as_str() {
            "backgroundColor" => opts.background_color = str_of(v).map(str::to_owned),
            "border" => opts.border = border_spec(v),
            "borderStyle" => opts.border_style = str_of(v).map(str::to_owned),
            "borderColor" => opts.border_color = str_of(v).map(str::to_owned),
            "focusedBorderColor" => opts.focused_border_color = str_of(v).map(str::to_owned),
            "shouldFill" => opts.should_fill = bool_of(v),
            "title" => opts.title = str_of(v).map(str::to_owned),
            "titleColor" => opts.title_color = str_of(v).map(str::to_owned),
            "titleAlignment" => opts.title_alignment = str_of(v).map(str::to_owned),
            "bottomTitle" => opts.bottom_title = str_of(v).map(str::to_owned),
            "bottomTitleAlignment" => {
                opts.bottom_title_alignment = str_of(v).map(str::to_owned);
            }
            "width" => opts.width = parse_dimension(v),
            "height" => opts.height = parse_dimension(v),
            n if is_layout_prop(n) => apply_layout(&mut opts.layout, n, v),
            _ => {}
        }
    }
    opts
}

/// The text node tree gathered from an element's children
/// (Text.ts:85-97 `updateTextFromNodes`): span elements and raw text
/// children fold into a [`TextNodeRenderable`] tree (the JSX
/// `<text><b>hi</b>…</text>` shape; the port rebuilds the tree on
/// child changes instead of mounting per-span renderables — the
/// gathered output is identical, per-span node reuse is not).
#[must_use]
pub fn gather_text_children(children: &[Element]) -> Option<TextNodeRenderable> {
    if children.is_empty() {
        return None;
    }
    let mut root = TextNodeRenderable::new("root-node", TextNodeOptions::default());
    let mut any = false;
    for child in children {
        match &child.tag {
            Tag::TextNode => {
                if let Some(p) = child.prop("text") {
                    if let PropValue::Text(s) | PropValue::Str(s) = p.read() {
                        root.add_text(&s);
                        any = true;
                    }
                }
            }
            Tag::Span(kind) => {
                root.add_node(span_node(child, *kind));
                any = true;
            }
            _ => {}
        }
    }
    if any {
        Some(root)
    } else {
        None
    }
}

fn span_node(el: &Element, kind: crate::element::SpanKind) -> TextNodeRenderable {
    let mut fg = None;
    let mut bg = None;
    let mut attributes = 0u32;
    let mut link = None;
    // the modifier classes set their attribute by kind
    // (catalogue.ts:34-47: b/strong → BOLD, i/em → ITALIC, u → UNDERLINE)
    attributes |= match kind {
        crate::element::SpanKind::Bold => 1,
        crate::element::SpanKind::Italic => 1 << 2,
        crate::element::SpanKind::Underline => 1 << 3,
        _ => 0,
    };
    for (name, prop) in &el.props {
        let v = prop.read();
        match name.as_str() {
            "fg" => fg = str_of(&v).map(str::to_owned),
            "bg" => bg = str_of(&v).map(str::to_owned),
            "attributes" => attributes |= parse_attributes(&v).unwrap_or(0),
            "style" => {
                // text-node style: attributes OR-merge + fg/bg
                // (reconciler.ts:249-254)
                if let PropValue::Style(pairs) = &v {
                    for (k, val) in pairs {
                        match k.as_str() {
                            "fg" => fg = str_of(val).map(str::to_owned),
                            "bg" => bg = str_of(val).map(str::to_owned),
                            "attributes" => {
                                attributes |= parse_attributes(val).unwrap_or(0);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "href" => link = Some(str_of(&v).map(|s| s.to_owned()).unwrap_or_default()),
            _ => {}
        }
    }
    let mut node = TextNodeRenderable::new(
        el.key.clone().unwrap_or_else(|| "span".into()).as_str(),
        TextNodeOptions {
            fg,
            bg,
            attributes: Some(attributes),
            link,
        },
    );
    // span children: text runs and nested spans
    if let crate::element::Children::Static(kids) = &el.children {
        for kid in kids {
            match &kid.tag {
                Tag::TextNode => {
                    if let Some(p) = kid.prop("text") {
                        if let PropValue::Text(s) | PropValue::Str(s) = p.read() {
                            node.add_text(&s);
                        }
                    }
                }
                Tag::Span(k) => {
                    node.add_node(span_node(kid, *k));
                }
                _ => {}
            }
        }
    }
    // `<br>` renders as a newline chunk (catalogue.ts:67-76)
    if matches!(kind, crate::element::SpanKind::Break) {
        node.add_text("\n");
    }
    node
}

/// Build [`TextOptions`] from a prop map (Text.ts / TextBufferOptions
/// option names).
#[must_use]
pub fn text_options(props: &PropMap, children: &[Element]) -> TextOptions {
    let mut opts = TextOptions::default();
    for (name, v) in props {
        match name.as_str() {
            "content" | "text" => opts.content = content_input(v),
            "fg" => opts.fg = str_of(v).map(parse_color),
            "bg" => opts.bg = str_of(v).map(parse_color),
            "attributes" => opts.attributes = parse_attributes(v).unwrap_or(0),
            "wrapMode" => opts.wrap_mode = str_of(v).map(WrapMode::parse),
            "textAlign" => opts.text_align = str_of(v).map(TextAlign::parse),
            "tabIndicator" => {
                opts.tab_indicator = match v {
                    PropValue::Str(s) => s.chars().next().map(|c| c as u32),
                    PropValue::Text(s) => s.chars().next().map(|c| c as u32),
                    PropValue::Num(n) => Some(*n as u32),
                    _ => None,
                };
            }
            "tabIndicatorColor" => opts.tab_indicator_color = str_of(v).map(parse_color),
            "truncate" => opts.truncate = bool_of(v).unwrap_or(false),
            "width" => opts.width = parse_dimension(v),
            "height" => opts.height = parse_dimension(v),
            n if is_layout_prop(n) => apply_layout(&mut opts.layout, n, v),
            _ => {}
        }
    }
    opts.node_tree = gather_text_children(children);
    opts
}

/// Build [`ScrollBoxOptions`] from a prop map (ScrollBox.ts options).
#[must_use]
pub fn scrollbox_options(props: &PropMap) -> ScrollBoxOptions {
    let mut opts = ScrollBoxOptions {
        viewport_culling: true,
        scroll_y: true,
        scroll_x: false,
        ..Default::default()
    };
    let mut box_map: PropMap = Vec::new();
    for (name, v) in props {
        match name.as_str() {
            "scrollX" => opts.scroll_x = bool_of(v).unwrap_or(false),
            "scrollY" => opts.scroll_y = bool_of(v).unwrap_or(true),
            "stickyScroll" => opts.sticky_scroll = bool_of(v).unwrap_or(false),
            "stickyStart" => {
                opts.sticky_start =
                    str_of(v).and_then(opentui_core::renderables::scrollbox::StickyStart::parse);
            }
            "scrollTop" | "scrollLeft" => { /* script-mutated, not an option */ }
            "scrollbarOptions" => {
                if let PropValue::Style(pairs) = v {
                    for (k, val) in pairs {
                        match k.as_str() {
                            "thumbColor" => {
                                opts.scrollbar_thumb_color = str_of(val).map(str::to_owned);
                            }
                            "trackColor" => {
                                opts.scrollbar_track_color = str_of(val).map(str::to_owned);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "viewportOptions" | "contentOptions" | "wrapperOptions" => {
                let mut b = BoxOptions::default();
                if let PropValue::Style(pairs) = v {
                    for (k, val) in pairs {
                        apply_layout(&mut b.layout, k, val);
                    }
                }
                match name.as_str() {
                    "viewportOptions" => opts.viewport_options = Some(b),
                    "contentOptions" => opts.content_options = Some(b),
                    "wrapperOptions" => opts.wrapper_options = Some(b),
                    _ => {}
                }
            }
            _ => box_map.push((name.clone(), v.clone())),
        }
    }
    // default ScrollBox sizing from the harness shape: scrollY default true
    opts.box_options = box_options(&box_map);
    if prop_get(&box_map, "scrollY").is_none() && prop_get(&box_map, "scrollX").is_none() {
        opts.scroll_y = true;
    }
    opts
}

/// Rebuild a live text node's state from the full prop map + children
/// (the `content`/`fg`/… assignment paths — Text.ts setters end in the
/// same re-derived `TextBuffer` state).
pub fn rebuild_text(
    renderer: &mut CliRenderer,
    live_id: &str,
    props: &PropMap,
    children: &[Element],
) -> bool {
    let Some(node) = renderer.root.inner.find_mut(live_id) else {
        return false;
    };
    let opts = text_options(props, children);
    let state = TextRenderableState::from_options(&opts);
    let measured = state.measured();
    node.content = Content::Text(state);
    let taffy_node = node.node;
    if let Some(ctx) = renderer.tree.get_node_context_mut(taffy_node) {
        ctx.kind = MeasureKind::Text;
        ctx.text = Some(measured);
    }
    let _ = renderer.tree.mark_dirty(taffy_node);
    let layout = opts.layout.clone();
    node.layout = layout.clone();
    node.width = opts.width.unwrap_or(Dimension::Auto);
    node.height = opts.height.unwrap_or(Dimension::Auto);
    let style = opentui_core::layout::taffy_style(&layout, node.width, node.height);
    let _ = renderer.tree.set_style(taffy_node, style);
    true
}

/// Rebuild a live box node's state (+ yoga borders) from the prop map.
pub fn rebuild_box(renderer: &mut CliRenderer, live_id: &str, props: &PropMap) -> bool {
    let Some(node) = renderer.root.inner.find_mut(live_id) else {
        return false;
    };
    let opts = box_options(props);
    let state = opentui_core::renderables::box_::BoxRenderableState::from_options(&opts);
    node.content = Content::Box(state);
    let taffy_node = node.node;
    let mut layout = opts.layout.clone();
    if let Content::Box(state) = &node.content {
        state.apply_yoga_borders(&mut layout);
    }
    node.layout = layout.clone();
    node.width = opts.width.unwrap_or(Dimension::Auto);
    node.height = opts.height.unwrap_or(Dimension::Auto);
    let style = opentui_core::layout::taffy_style(&layout, node.width, node.height);
    let _ = renderer.tree.set_style(taffy_node, style);
    let _ = renderer.tree.mark_dirty(taffy_node);
    true
}

/// Rebuild a live ScrollBox root's box-ish state from the prop map
/// (scroll setters route separately through the core scroll APIs; the
/// scroll machine state is preserved).
pub fn rebuild_scrollbox(renderer: &mut CliRenderer, live_id: &str, props: &PropMap) -> bool {
    let Some(node) = renderer.root.inner.find_mut(live_id) else {
        return false;
    };
    let opts = scrollbox_options(props);
    let box_state =
        opentui_core::renderables::box_::BoxRenderableState::from_options(&opts.box_options);
    let taffy_node = node.node;
    // replace the box half, keep the scroll machine state
    if let Content::ScrollBox(box_half, _) = &mut node.content {
        *box_half = box_state;
    } else {
        return false;
    }
    let mut layout = opts.box_options.layout.clone();
    if let Content::ScrollBox(state, _) = &node.content {
        state.apply_yoga_borders(&mut layout);
    }
    node.layout = layout.clone();
    node.width = opts.box_options.width.unwrap_or(Dimension::Auto);
    node.height = opts.box_options.height.unwrap_or(Dimension::Auto);
    let style = opentui_core::layout::taffy_style(&layout, node.width, node.height);
    let _ = renderer.tree.set_style(taffy_node, style);
    let _ = renderer.tree.mark_dirty(taffy_node);
    true
}

/// A raw-string styled text (helper for content props).
#[must_use]
pub fn styled_of(v: &PropValue) -> opentui_core::styled_text::StyledText {
    match v {
        PropValue::Styled(st) => st.clone(),
        other => string_to_styled_text(str_of(other).unwrap_or("")),
    }
}
