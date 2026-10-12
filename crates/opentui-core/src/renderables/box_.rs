//! `BoxRenderable` — `packages/core/src/renderables/Box.ts`.
//!
//! Defaults (Box.ts:62-71): backgroundColor transparent, borderStyle
//! single, border false, borderColor #FFFFFF, shouldFill true, focused
//! borderColor #00AAFF. A border-ish option (`borderStyle`/`borderColor`/
//! `focusedBorderColor`/`customBorderChars`) without `border` turns the
//! border ON (Box.ts:82-87). Borders also feed the yoga node as 1pt edge
//! borders (Box.ts:308-315 `applyYogaBorders`).

use crate::buffer::{DrawBoxOptions, OptimizedBuffer};
use crate::color::{parse_color, Rgba};
use crate::layout::LayoutOptions;

pub type BorderStyle = &'static str; // "single" | "double" | "rounded" | "heavy"

/// `BorderSides` — lib/border.ts:14.
pub const BORDER_SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// `getBorderSides(border)` — lib/border.ts (boolean → all, list → set).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BorderSidesConfig {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

impl BorderSidesConfig {
    #[must_use]
    pub fn all() -> Self {
        BorderSidesConfig { top: true, right: true, bottom: true, left: true }
    }

    #[must_use]
    pub fn none() -> Self {
        BorderSidesConfig::default()
    }

    #[must_use]
    pub fn from_border(border: Option<&BorderSpec>) -> Self {
        match border {
            None | Some(BorderSpec::Off) => BorderSidesConfig::none(),
            Some(BorderSpec::All) => BorderSidesConfig::all(),
            Some(BorderSpec::Sides(list)) => BorderSidesConfig {
                top: list.iter().any(|s| s == "top"),
                right: list.iter().any(|s| s == "right"),
                bottom: list.iter().any(|s| s == "bottom"),
                left: list.iter().any(|s| s == "left"),
            },
        }
    }

    #[must_use]
    pub fn any(self) -> bool {
        self.top || self.right || self.bottom || self.left
    }
}

/// `border` option: boolean or side list (Box.ts:20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BorderSpec {
    Off,
    All,
    Sides(Vec<String>),
}

/// One style's glyphs — the used subset of `BorderCharacters`
/// (lib/border.ts:46-98 exact values).
pub struct BorderCharSet {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
}

const SINGLE: BorderCharSet = BorderCharSet {
    top_left: '┌',
    top_right: '┐',
    bottom_left: '└',
    bottom_right: '┘',
    horizontal: '─',
    vertical: '│',
};
const DOUBLE: BorderCharSet = BorderCharSet {
    top_left: '╔',
    top_right: '╗',
    bottom_left: '╚',
    bottom_right: '╝',
    horizontal: '═',
    vertical: '║',
};
const ROUNDED: BorderCharSet = BorderCharSet {
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
    horizontal: '─',
    vertical: '│',
};
const HEAVY: BorderCharSet = BorderCharSet {
    top_left: '┏',
    top_right: '┓',
    bottom_left: '┗',
    bottom_right: '┛',
    horizontal: '━',
    vertical: '┃',
};

/// `parseBorderStyle` — lib/border.ts:22-31 (unknown → fallback "single").
#[must_use]
pub fn border_char_set(style: &str) -> &'static BorderCharSet {
    match style {
        "double" => &DOUBLE,
        "rounded" => &ROUNDED,
        "heavy" => &HEAVY,
        _ => &SINGLE,
    }
}

/// `BoxOptions` (Box.ts:17-34) merged with `RenderableOptions`.
#[derive(Debug, Clone, Default)]
pub struct BoxOptions {
    pub layout: LayoutOptions,
    pub width: Option<crate::layout::Dimension>,
    pub height: Option<crate::layout::Dimension>,
    pub background_color: Option<String>,
    pub border: Option<BorderSpec>,
    pub border_style: Option<String>,
    pub border_color: Option<String>,
    pub should_fill: Option<bool>,
    pub title: Option<String>,
    pub title_color: Option<String>,
    pub title_alignment: Option<String>,
    pub bottom_title: Option<String>,
    pub bottom_title_alignment: Option<String>,
    pub focused_border_color: Option<String>,
}

/// The Box's drawn state — the protected fields of BoxRenderable
/// (Box.ts:46-108 constructor).
pub struct BoxRenderableState {
    pub background_color: Rgba,
    pub border: BorderSpec,
    pub border_sides: BorderSidesConfig,
    pub border_style: String,
    pub border_color: Rgba,
    pub focused_border_color: Rgba,
    pub should_fill: bool,
    pub title: Option<String>,
    pub title_color: Option<Rgba>,
    pub title_alignment: String,
    pub bottom_title: Option<String>,
    pub bottom_title_alignment: String,
}

fn alignment_code(align: Option<&str>) -> u8 {
    match align.unwrap_or("left") {
        "center" => 1,
        "right" => 2,
        _ => 0,
    }
}

impl BoxRenderableState {
    pub fn from_options(opts: &BoxOptions) -> Self {
        // Box.ts:82-87: border-ish options imply border
        let mut border = opts.border.clone().unwrap_or(BorderSpec::Off);
        if matches!(border, BorderSpec::Off)
            && (opts.border_style.is_some()
                || opts.border_color.is_some()
                || opts.focused_border_color.is_some())
        {
            border = BorderSpec::All;
        }
        let border_style = match opts.border_style.as_deref() {
            Some("double") | Some("rounded") | Some("heavy") | Some("single") => {
                opts.border_style.clone().unwrap_or_else(|| "single".into())
            }
            _ => "single".into(),
        };
        BoxRenderableState {
            background_color: parse_color(
                opts.background_color.as_deref().unwrap_or("transparent"),
            ),
            border_sides: BorderSidesConfig::from_border(Some(&border)),
            border,
            border_style,
            border_color: parse_color(opts.border_color.as_deref().unwrap_or("#FFFFFF")),
            focused_border_color: parse_color(
                opts.focused_border_color.as_deref().unwrap_or("#00AAFF"),
            ),
            should_fill: opts.should_fill.unwrap_or(true),
            title: opts.title.clone(),
            title_color: opts.title_color.as_deref().map(parse_color),
            title_alignment: opts.title_alignment.clone().unwrap_or_else(|| "left".into()),
            bottom_title: opts.bottom_title.clone(),
            bottom_title_alignment: opts
                .bottom_title_alignment
                .clone()
                .unwrap_or_else(|| "left".into()),
        }
    }

    /// Feed border widths into layout options — `applyYogaBorders`
    /// (Box.ts:308-315): each drawn side contributes 1pt taffy border.
    pub fn apply_yoga_borders(&self, layout: &mut LayoutOptions) {
        layout.border_top = Some(if self.border_sides.top { 1.0 } else { 0.0 });
        layout.border_right = Some(if self.border_sides.right { 1.0 } else { 0.0 });
        layout.border_bottom = Some(if self.border_sides.bottom { 1.0 } else { 0.0 });
        layout.border_left = Some(if self.border_sides.left { 1.0 } else { 0.0 });
    }

    /// `renderSelf` (Box.ts:255-286): skip when no border and invisible
    /// fill, then `buffer.drawBox(...)` at screen coords.
    pub fn paint(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32, w: u32, h: u32) {
        let has_border = self.border_sides.any();
        let has_visible_fill = self.should_fill && self.background_color.alpha() > 0;
        if !has_border && !has_visible_fill {
            return;
        }
        let border_color = self.border_color;
        buffer.draw_box(DrawBoxOptions {
            x: screen_x,
            y: screen_y,
            width: w,
            height: h,
            border_sides: self.border_sides,
            border_chars: border_char_set(&self.border_style),
            border_color,
            background_color: self.background_color,
            should_fill: self.should_fill,
            title: self.title.clone(),
            title_color: self.title_color.unwrap_or(border_color),
            title_alignment: alignment_code(Some(&self.title_alignment)),
            bottom_title: self.bottom_title.clone(),
            bottom_title_alignment: alignment_code(Some(&self.bottom_title_alignment)),
        });
    }
}
