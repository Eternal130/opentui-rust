//! Flexbox layout — the yoga replacement.
//!
//! Upstream wires every renderable to a yoga node (`Renderable.ts:2`
//! imports; `packages/core/src/yoga.ts` re-exports the bundled yoga). The
//! Rust port uses **taffy 0.9** with the yoga-compatible configuration
//! documented in ARCHITECTURE.md §layout. Two upstream-visible behaviors
//! drive this module:
//!
//! * **Default flex direction is COLUMN.** Yoga's own default is column
//!   (Facebook deviation from CSS); taffy's default is row. Every node we
//!   create therefore pins `FlexDirection::Column` unless options say
//!   otherwise — matching `Renderable.ts` option parsing
//!   (`parseFlexDirection`, Renderable.ts:1373+).
//! * **Layout runs root-down with explicit viewport size** —
//!   `RootRenderable.calculateLayout()` (Renderable.ts:1865-1867) calls
//!   `yogaNode.calculateLayout(width, height, LTR)`.
//!
//! Option strings (`"flex-start"`, `"wrap"`, percentages) parse exactly like
//! `lib/yoga.options.ts`.

use taffy::prelude as tp;
use taffy::style_helpers::{TaffyAuto, TaffyZero};

/// A yoga-style dimension: points, `"auto"`, or `"N%"` — Renderable.ts:55-64.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Dimension {
    #[default]
    Auto,
    Points(f32),
    Percent(f32),
}

/// `LayoutOptions` — Renderable.ts:55-105 (fields the slice consumes; the
/// full table lives in PORT-MATRIX.md tier 2).
#[derive(Debug, Clone, Default)]
pub struct LayoutOptions {
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub flex_direction: Option<String>,
    pub flex_wrap: Option<String>,
    pub align_items: Option<String>,
    pub justify_content: Option<String>,
    pub align_self: Option<String>,
    pub flex_basis: Option<Dimension>,
    pub position: Option<String>,
    pub overflow: Option<String>,
    pub top: Option<Dimension>,
    pub right: Option<Dimension>,
    pub bottom: Option<Dimension>,
    pub left: Option<Dimension>,
    pub min_width: Option<Dimension>,
    pub min_height: Option<Dimension>,
    pub max_width: Option<Dimension>,
    pub max_height: Option<Dimension>,
    pub margin: Option<Dimension>,
    pub margin_x: Option<Dimension>,
    pub margin_y: Option<Dimension>,
    pub margin_top: Option<Dimension>,
    pub margin_right: Option<Dimension>,
    pub margin_bottom: Option<Dimension>,
    pub margin_left: Option<Dimension>,
    pub padding: Option<Dimension>,
    pub padding_x: Option<Dimension>,
    pub padding_y: Option<Dimension>,
    pub padding_top: Option<Dimension>,
    pub padding_right: Option<Dimension>,
    pub padding_bottom: Option<Dimension>,
    pub padding_left: Option<Dimension>,
    pub gap: Option<Dimension>,
    pub row_gap: Option<Dimension>,
    pub column_gap: Option<Dimension>,
    pub border_top: Option<f32>,
    pub border_right: Option<f32>,
    pub border_bottom: Option<f32>,
    pub border_left: Option<f32>,
}

fn parse_dimension(d: Dimension) -> tp::LengthPercentageAuto {
    match d {
        Dimension::Auto => tp::LengthPercentageAuto::AUTO,
        Dimension::Points(p) => tp::LengthPercentageAuto::length(p),
        Dimension::Percent(p) => tp::LengthPercentageAuto::percent(p),
    }
}

fn parse_taffy_dimension(d: Dimension) -> tp::Dimension {
    match d {
        Dimension::Auto => tp::Dimension::AUTO,
        Dimension::Points(p) => tp::Dimension::length(p),
        Dimension::Percent(p) => tp::Dimension::percent(p),
    }
}

fn parse_dimension_length(d: Dimension) -> tp::LengthPercentage {
    match d {
        // LengthPercentage has no auto variant; unset padding/border = 0
        Dimension::Auto => tp::LengthPercentage::ZERO,
        Dimension::Points(p) => tp::LengthPercentage::length(p),
        Dimension::Percent(p) => tp::LengthPercentage::percent(p),
    }
}

/// The yoga-option parsers — lib/yoga.options.ts (string → enum), returning
/// the fallback for unknown values exactly like upstream `parseX(..., fallback)`.
fn flex_direction(s: &str) -> tp::FlexDirection {
    match s {
        "row" => tp::FlexDirection::Row,
        "row-reverse" => tp::FlexDirection::RowReverse,
        "column-reverse" => tp::FlexDirection::ColumnReverse,
        // yoga default "column"
        _ => tp::FlexDirection::Column,
    }
}

fn flex_wrap(s: &str) -> tp::FlexWrap {
    match s {
        "wrap" => tp::FlexWrap::Wrap,
        "wrap-reverse" => tp::FlexWrap::WrapReverse,
        _ => tp::FlexWrap::NoWrap,
    }
}

fn align_items(s: &str) -> tp::AlignItems {
    match s {
        "flex-start" => tp::AlignItems::FlexStart,
        "flex-end" => tp::AlignItems::FlexEnd,
        "center" => tp::AlignItems::Center,
        "baseline" => tp::AlignItems::Baseline,
        // upstream maps "auto"→Align.Auto (yoga.options.ts:77-78); taffy
        // has no AlignItems::Auto (that exists only for align_self) —
        // delta documented in ARCHITECTURE.md; falls to Stretch like null
        _ => tp::AlignItems::Stretch,
    }
}

fn justify_content(s: &str) -> tp::JustifyContent {
    match s {
        "flex-start" => tp::JustifyContent::FlexStart,
        "flex-end" => tp::JustifyContent::FlexEnd,
        "center" => tp::JustifyContent::Center,
        "space-between" => tp::JustifyContent::SpaceBetween,
        "space-around" => tp::JustifyContent::SpaceAround,
        "space-evenly" => tp::JustifyContent::SpaceEvenly,
        _ => tp::JustifyContent::FlexStart,
    }
}

fn align_self(s: &str) -> Option<tp::AlignItems> {
    match s {
        "flex-start" => Some(tp::AlignItems::FlexStart),
        "flex-end" => Some(tp::AlignItems::FlexEnd),
        "center" => Some(tp::AlignItems::Center),
        "stretch" => Some(tp::AlignItems::Stretch),
        "baseline" => Some(tp::AlignItems::Baseline),
        _ => None, // "auto"
    }
}

fn position_type(s: &str) -> tp::Position {
    match s {
        "absolute" => tp::Position::Absolute,
        "relative" => tp::Position::Relative,
        _ => tp::Position::Relative,
    }
}

/// Build a taffy `Style` from renderable options. `default` carries the
/// node's own defaults (e.g. Box borders set edge widths).
#[must_use]
pub fn taffy_style(opts: &LayoutOptions, width: Dimension, height: Dimension) -> tp::Style {
    // yoga's column default (yoga.options.ts:188-191 — null ⇒ Column;
    // taffy defaults to Row, so pin it). flexGrow defaults 0
    // (Renderable.ts:724-727); flexShrink is conditional
    // (Renderable.ts:728-737): explicit numeric width/height ⇒ 0, else
    // the web default 1.
    let flex_shrink = match opts.flex_shrink {
        Some(fs) => fs,
        None => {
            let explicit_size =
                matches!(width, Dimension::Points(_)) || matches!(height, Dimension::Points(_));
            if explicit_size {
                0.0
            } else {
                1.0
            }
        }
    };
    let mut style = tp::Style {
        flex_direction: tp::FlexDirection::Column,
        flex_grow: opts.flex_grow.unwrap_or(0.0),
        flex_shrink,
        ..Default::default()
    };
    // Yoga applies NO automatic minimum size to flex items (no CSS
    // min-content floor): items shrink below content and grow from 0.
    // Taffy implements the CSS automatic minimum — zero it unless the
    // options give an explicit min, else e.g. the ScrollBox's horizontal
    // scrollbar keeps 1 row during the first-layout shrink that upstream
    // collapses to 0 (pinned: scrollbox-scrollbar-geometry vs 0.5.14).
    if opts.min_width.is_none() {
        style.min_size.width = tp::Dimension::length(0.0);
    }
    if opts.min_height.is_none() {
        style.min_size.height = tp::Dimension::length(0.0);
    }
    // (The scroll-container special case collapses into the rule above;
    // kept for documentation: `overflow != visible` nodes zero their
    // auto-min in both engines — ScrollBox.ts:329-331 sets overflow
    // hidden on the viewport for exactly this sizing behavior.)
    let _ = matches!(opts.overflow.as_deref(), Some("hidden") | Some("scroll"));
    if let Some(s) = opts.flex_direction.as_deref() {
        style.flex_direction = flex_direction(s);
    }
    if let Some(s) = opts.flex_wrap.as_deref() {
        style.flex_wrap = flex_wrap(s);
    }
    if let Some(s) = opts.align_items.as_deref() {
        style.align_items = Some(align_items(s));
    }
    if let Some(s) = opts.justify_content.as_deref() {
        style.justify_content = Some(justify_content(s));
    }
    if let Some(s) = opts.align_self.as_deref() {
        style.align_self = align_self(s);
    }
    if let Some(b) = opts.flex_basis {
        style.flex_basis = parse_taffy_dimension(b);
    }
    if let Some(s) = opts.position.as_deref() {
        style.position = position_type(s);
    }
    if let Some(d) = opts.top {
        style.inset.top = parse_dimension(d);
    }
    if let Some(d) = opts.right {
        style.inset.right = parse_dimension(d);
    }
    if let Some(d) = opts.bottom {
        style.inset.bottom = parse_dimension(d);
    }
    if let Some(d) = opts.left {
        style.inset.left = parse_dimension(d);
    }
    if let Some(d) = opts.min_width {
        style.min_size.width = parse_taffy_dimension(d);
    }
    if let Some(d) = opts.min_height {
        style.min_size.height = parse_taffy_dimension(d);
    }
    if let Some(d) = opts.max_width {
        style.max_size.width = parse_taffy_dimension(d);
    }
    if let Some(d) = opts.max_height {
        style.max_size.height = parse_taffy_dimension(d);
    }
    if let Some(d) = opts.margin {
        style.margin = tp::Rect {
            left: parse_dimension(d),
            right: parse_dimension(d),
            top: parse_dimension(d),
            bottom: parse_dimension(d),
        };
    }
    if let Some(d) = opts.margin_x {
        style.margin.left = parse_dimension(d);
        style.margin.right = parse_dimension(d);
    }
    if let Some(d) = opts.margin_y {
        style.margin.top = parse_dimension(d);
        style.margin.bottom = parse_dimension(d);
    }
    if let Some(d) = opts.margin_top {
        style.margin.top = parse_dimension(d);
    }
    if let Some(d) = opts.margin_right {
        style.margin.right = parse_dimension(d);
    }
    if let Some(d) = opts.margin_bottom {
        style.margin.bottom = parse_dimension(d);
    }
    if let Some(d) = opts.margin_left {
        style.margin.left = parse_dimension(d);
    }
    if let Some(d) = opts.padding {
        style.padding = tp::Rect {
            left: parse_dimension_length(d),
            right: parse_dimension_length(d),
            top: parse_dimension_length(d),
            bottom: parse_dimension_length(d),
        };
    }
    if let Some(d) = opts.padding_x {
        style.padding.left = parse_dimension_length(d);
        style.padding.right = parse_dimension_length(d);
    }
    if let Some(d) = opts.padding_y {
        style.padding.top = parse_dimension_length(d);
        style.padding.bottom = parse_dimension_length(d);
    }
    if let Some(d) = opts.padding_top {
        style.padding.top = parse_dimension_length(d);
    }
    if let Some(d) = opts.padding_right {
        style.padding.right = parse_dimension_length(d);
    }
    if let Some(d) = opts.padding_bottom {
        style.padding.bottom = parse_dimension_length(d);
    }
    if let Some(d) = opts.padding_left {
        style.padding.left = parse_dimension_length(d);
    }
    if let Some(g) = opts.gap {
        style.gap = tp::Size {
            width: parse_dimension_length(g),
            height: parse_dimension_length(g),
        };
    }
    if let Some(g) = opts.row_gap {
        style.gap.height = parse_dimension_length(g);
    }
    if let Some(g) = opts.column_gap {
        style.gap.width = parse_dimension_length(g);
    }
    if let Some(b) = opts.border_top {
        style.border.top = tp::LengthPercentage::length(b);
    }
    if let Some(b) = opts.border_right {
        style.border.right = tp::LengthPercentage::length(b);
    }
    if let Some(b) = opts.border_bottom {
        style.border.bottom = tp::LengthPercentage::length(b);
    }
    if let Some(b) = opts.border_left {
        style.border.left = tp::LengthPercentage::length(b);
    }
    style.size.width = parse_taffy_dimension(width);
    style.size.height = parse_taffy_dimension(height);
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_yoga_column_shrink0() {
        let style = taffy_style(&LayoutOptions::default(), Dimension::Auto, Dimension::Auto);
        assert_eq!(style.flex_direction, tp::FlexDirection::Column);
        // no explicit size ⇒ web-default shrink 1 (Renderable.ts:734)
        assert_eq!(style.flex_shrink, 1.0);
        // explicit width ⇒ shrink 0 (Renderable.ts:733)
        let style = taffy_style(
            &LayoutOptions::default(),
            Dimension::Points(10.0),
            Dimension::Auto,
        );
        assert_eq!(style.flex_shrink, 0.0);
    }

    #[test]
    fn parsers_match_yoga_options_ts() {
        assert_eq!(flex_direction("column"), tp::FlexDirection::Column);
        assert_eq!(flex_direction("row"), tp::FlexDirection::Row);
        assert_eq!(
            justify_content("space-between"),
            tp::JustifyContent::SpaceBetween
        );
    }
}
