//! `LineNumberRenderable` + its `GutterRenderable` — port of
//! `packages/core/src/renderables/LineNumberRenderable.ts` (0.5.14).
//!
//! Upstream is a wrapper (`LineNumberRenderable`, flexDirection row,
//! height forced `"auto"` — LineNumberRenderable.ts:406-413) that owns
//! two children: the gutter (a measured, buffered leaf drawing the
//! numbers/signs, :37-352) and the target (`Renderable &
//! LineInfoProvider`, :444-479). The wrapper's `renderSelf` paints
//! full-width **content** backgrounds after the gutter
//! (:540-565); the gutter paints its own column
//! (`GutterRenderable.render` :246-270 + `refreshFrameBuffer`
//! :272-351).
//!
//! Port shape: the wrapper is a [`crate::renderable::Content::LineNumber`]
//! container whose `paint_self` performs BOTH the renderSelf fills and
//! the gutter painting (it can see the target sibling's virtual lines);
//! the gutter child is a measured leaf whose taffy measure mirrors
//! `setupMeasureFunc` (:106-128) — width from `calculateWidth`
//! (:171-185), height from the target's virtual line count. Like the
//! Code lane, the buffered-framebuffer caching (`_paintedSources`,
//! :278-288) collapses: the port paints the settled state directly.
//!
//! `lineInfo`/`getLineSources` come from the target's wrapped virtual
//! lines mapped through the Code concealment source map
//! (`getLineSources` helper :354-358 over Code.ts:162-169).

use crate::buffer::OptimizedBuffer;
use crate::color::{parse_color, Rgba};
use crate::layout::Dimension;
use crate::text_buffer::cluster_width;
use std::collections::{HashMap, HashSet};

/// `DEFAULT_GUTTER_FG` — LineNumberRenderable.ts:34.
pub const DEFAULT_GUTTER_FG: &str = "#888888";
/// `DEFAULT_GUTTER_BG` — LineNumberRenderable.ts:35.
pub const DEFAULT_GUTTER_BG: &str = "transparent";

/// `LineSign` — LineNumberRenderable.ts:8-13.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineSign {
    pub before: Option<String>,
    pub before_color: Option<Rgba>,
    pub after: Option<String>,
    pub after_color: Option<Rgba>,
}

/// `LineColorConfig` — LineNumberRenderable.ts:15-18.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineColorConfig {
    pub gutter: Option<Rgba>,
    pub content: Option<Rgba>,
}

/// `string | RGBA | LineColorConfig` — the two `lineColors` formats
/// (LineNumberRenderable.ts:385-404).
#[derive(Debug, Clone, PartialEq)]
pub enum LineColorValue {
    Simple(Rgba),
    Config(LineColorConfig),
}

/// `darkenColor` — LineNumberRenderable.ts:361-363: 80% per component,
/// alpha kept, quantized like `RGBA.fromValues`.
#[must_use]
pub fn darken_color(color: Rgba) -> Rgba {
    let [r, g, b, a] = color.floats();
    Rgba::from_values(r * 0.8, g * 0.8, b * 0.8, a)
}

/// `parseLineColor` — LineNumberRenderable.ts:385-404: the config
/// format splits gutter/content (content defaults to a *darker* gutter,
/// :394-397); the simple format uses the color for the gutter and its
/// 80% darkening for content (:398-403).
pub fn parse_line_color(
    color: &LineColorValue,
    out_gutter: &mut HashMap<i64, Rgba>,
    out_content: &mut HashMap<i64, Rgba>,
    line: i64,
) {
    match color {
        LineColorValue::Config(config) => {
            if let Some(gutter) = config.gutter {
                out_gutter.insert(line, gutter);
            }
            if let Some(content) = config.content {
                out_content.insert(line, content);
            } else if let Some(gutter) = config.gutter {
                // If only gutter is specified, use a darker version for content
                out_content.insert(line, darken_color(gutter));
            }
        }
        LineColorValue::Simple(parsed) => {
            out_gutter.insert(line, *parsed);
            out_content.insert(line, darken_color(*parsed));
        }
    }
}

/// `calculateSignWidths` — LineNumberRenderable.ts:155-169: the widest
/// before/after sign strings, in display columns.
#[must_use]
pub fn calculate_sign_widths(line_signs: &HashMap<i64, LineSign>) -> (u32, u32) {
    let mut max_before = 0u32;
    let mut max_after = 0u32;
    for sign in line_signs.values() {
        if let Some(before) = &sign.before {
            max_before = max_before.max(sign_width(before));
        }
        if let Some(after) = &sign.after {
            max_after = max_after.max(sign_width(after));
        }
    }
    (max_before, max_after)
}

/// `stringWidth` over grapheme clusters (the platform runtime's width —
/// first-codepoint width with VS16/emoji handling; see
/// `crate::text_buffer::cluster_width`).
fn sign_width(text: &str) -> u32 {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true).map(cluster_width).sum()
}

/// The gutter-leaf measure inputs — `setupMeasureFunc`
/// (LineNumberRenderable.ts:106-128) reads them off the live renderable;
/// the port snapshots them into the taffy measure context
/// ([`crate::renderable::MeasureCtx`]). `total_lines` is the target's
/// current `virtualLineCount`, refreshed through the settle loop
/// (upstream: `line-info-change` → `remeasure`, :379-383).
#[derive(Debug, Clone, Default)]
pub struct GutterMeasure {
    pub min_width: u32,
    pub padding_right: u32,
    pub line_number_offset: i64,
    /// max of the custom line numbers (`calculateWidth`, :176-180)
    pub max_custom_number: Option<i64>,
    pub max_before_width: u32,
    pub max_after_width: u32,
    /// the target's current virtual line count
    pub total_lines: u32,
}

impl GutterMeasure {
    /// `calculateWidth` — LineNumberRenderable.ts:171-185.
    #[must_use]
    pub fn calculate_width(&self) -> u32 {
        // Find max line number, considering both calculated and custom line numbers
        let mut max_line_number = i64::from(self.total_lines) + self.line_number_offset;
        if let Some(custom) = self.max_custom_number {
            max_line_number = max_line_number.max(custom);
        }
        let digits = if max_line_number > 0 {
            (max_line_number as f64).log10().floor() as i64 + 1
        } else {
            1
        };
        let base = self
            .min_width
            .max((digits + i64::from(self.padding_right) + 1) as u32); // +1 for left padding
        base + self.max_before_width + self.max_after_width
    }

    /// The measure func's return — `setupMeasureFunc`
    /// (LineNumberRenderable.ts:107-125): content-derived dimensions,
    /// ignoring the parent constraints.
    #[must_use]
    pub fn measure(&self) -> (f32, f32) {
        (self.calculate_width() as f32, self.total_lines as f32)
    }
}

/// `GutterRenderable` paint state — everything `refreshFrameBuffer`
/// reads (LineNumberRenderable.ts:272-351).
#[derive(Debug, Clone)]
pub struct GutterState {
    pub fg: Rgba,
    pub bg: Rgba,
    pub min_width: u32,
    pub padding_right: u32,
    pub line_colors_gutter: HashMap<i64, Rgba>,
    pub line_signs: HashMap<i64, LineSign>,
    pub line_number_offset: i64,
    pub hide_line_numbers: HashSet<i64>,
    pub line_numbers: HashMap<i64, i64>,
    pub max_before_width: u32,
    pub max_after_width: u32,
}

impl GutterState {
    /// Constructor assembly (LineNumberRenderable.ts:80-92): the
    /// pre-parsed colors/signs plus the sign-width scan.
    #[must_use]
    pub fn new(options: &LineNumberOptions) -> Self {
        let mut line_colors_gutter = HashMap::new();
        let mut line_colors_content = HashMap::new();
        for (line, color) in &options.line_colors {
            parse_line_color(
                color,
                &mut line_colors_gutter,
                &mut line_colors_content,
                *line,
            );
        }
        let line_signs = options.line_signs.clone();
        let (max_before_width, max_after_width) = calculate_sign_widths(&line_signs);
        let _ = line_colors_content; // owned by the wrapper state
        GutterState {
            fg: options.fg,
            bg: options.bg,
            min_width: options.min_width,
            padding_right: options.padding_right,
            line_colors_gutter,
            line_signs,
            line_number_offset: options.line_number_offset,
            hide_line_numbers: options.hide_line_numbers.clone(),
            line_numbers: options.line_numbers.clone(),
            max_before_width,
            max_after_width,
        }
    }

    /// The measure snapshot for the taffy leaf context.
    #[must_use]
    pub fn measure_ctx(&self, total_lines: u32) -> GutterMeasure {
        GutterMeasure {
            min_width: self.min_width,
            padding_right: self.padding_right,
            line_number_offset: self.line_number_offset,
            max_custom_number: self.line_numbers.values().copied().max(),
            max_before_width: self.max_before_width,
            max_after_width: self.max_after_width,
            total_lines,
        }
    }
}

/// The wrapper's paint state — `renderSelf`
/// (LineNumberRenderable.ts:540-565) reads the **content** color map
/// plus the Code concealment source map (`lineInfo` remapping,
/// Code.ts:147-169).
#[derive(Debug, Clone, Default)]
pub struct LineNumberState {
    pub line_colors_content: HashMap<i64, Rgba>,
    /// CodeRenderable `_renderedLineSources` — rendered row → source
    /// line (None = identity; Code.ts:525-582).
    pub source_map: Option<Vec<i64>>,
}

/// `LineNumberOptions` — LineNumberRenderable.ts:20-32 (colors
/// pre-parsed like the constructor does, :415-421).
#[derive(Debug, Clone)]
pub struct LineNumberOptions {
    pub fg: Rgba,
    pub bg: Rgba,
    pub min_width: u32,
    pub padding_right: u32,
    pub line_colors: HashMap<i64, LineColorValue>,
    pub line_signs: HashMap<i64, LineSign>,
    pub line_number_offset: i64,
    pub hide_line_numbers: HashSet<i64>,
    pub line_numbers: HashMap<i64, i64>,
    pub show_line_numbers: bool,
    /// the wrapper's own width ("100%" | "50%" — createOrUpdateSide,
    /// Diff.ts:434)
    pub width: Dimension,
}

impl Default for LineNumberOptions {
    fn default() -> Self {
        LineNumberOptions {
            fg: parse_color(DEFAULT_GUTTER_FG),
            bg: parse_color(DEFAULT_GUTTER_BG),
            min_width: 3,
            padding_right: 1,
            line_colors: HashMap::new(),
            line_signs: HashMap::new(),
            line_number_offset: 0,
            hide_line_numbers: HashSet::new(),
            line_numbers: HashMap::new(),
            show_line_numbers: true,
            width: Dimension::Percent(1.0),
        }
    }
}

/// The target's line sources at a window — `getLineSources`
/// (LineNumberRenderable.ts:354-358) over CodeRenderable.getLineSources
/// (Code.ts:162-169): the virtual lines' source rows mapped through
/// the concealment source map. `slice(startLine, startLine + lineCount)`
/// semantics (a short window past the end).
#[must_use]
pub fn line_sources_window(
    vline_sources: &[u32],
    source_map: Option<&[i64]>,
    start_line: i64,
    line_count: i64,
) -> Vec<i64> {
    if start_line < 0 || line_count <= 0 {
        return Vec::new();
    }
    let start = start_line as usize;
    if start >= vline_sources.len() {
        return Vec::new();
    }
    let end = (start + line_count as usize).min(vline_sources.len());
    vline_sources[start..end]
        .iter()
        .map(|&row| {
            source_map
                .and_then(|map| map.get(row as usize).copied())
                .unwrap_or(i64::from(row))
        })
        .collect()
}

/// `GutterRenderable.render` — LineNumberRenderable.ts:246-270 +
/// `refreshFrameBuffer` :272-351, painted straight into the frame
/// buffer (the buffered framebuffer + `_paintedSources` cache collapse
/// into the settled paint).
///
/// * `gutter_width`/`gutter_height` — the gutter's laid out size
///   (`this.width`/`this.height`).
/// * `vline_sources`/`source_map` — the target's wrapped line sources.
#[allow(clippy::too_many_arguments)] // the upstream draw inputs (fb + sources + geometry)
pub fn paint_gutter(
    state: &GutterState,
    gutter_width: u32,
    gutter_height: u32,
    vline_sources: &[u32],
    source_map: Option<&[i64]>,
    scroll_y: i64,
    buffer: &mut OptimizedBuffer,
    screen_x: i32,
    screen_y: i32,
) {
    // Match native integer-cell drawing before deriving the source window (:248-252)
    let x = screen_x;
    let y = screen_y;
    let start: i64 = 0.max(-i64::from(y));
    let end: i64 = (gutter_height as i64).min(i64::from(buffer.height) - i64::from(y));
    if end <= start {
        return;
    }
    if x >= buffer.width as i32 || x + gutter_width as i32 <= 0 {
        return;
    }
    let fb_height = (end - start) as u32; // framebuffer height (:255-262)

    // refreshFrameBuffer(buffer, trunc(target.scrollY) + start) (:266,
    // :272-277): the line *before* the first visible one decides
    // whether row 0 is a wrapped continuation.
    let start_line = scroll_y + start;
    let source_start = 0.max(start_line - 1);
    let source_offset = start_line - source_start;
    let sources = line_sources_window(
        vline_sources,
        source_map,
        source_start,
        i64::from(fb_height) + source_offset,
    );

    // buffer.clear(this._bg) (:289) — the cleared gutter rect; the
    // port paints it into the frame buffer directly (transparent bg is
    // a fill no-op, matching the respectAlpha composite upstream).
    buffer.fill_rect(x, y + start as i32, gutter_width, fb_height, state.bg);

    let mut last_source: i64 = if source_offset > 0 {
        sources.first().copied().unwrap_or(-1)
    } else {
        -1
    };

    for i in 0..fb_height as i64 {
        let visual_line_index = source_offset + i;
        if visual_line_index as usize >= sources.len() {
            break;
        }
        let logical_line = sources[visual_line_index as usize];
        let line_bg = state
            .line_colors_gutter
            .get(&logical_line)
            .copied()
            .unwrap_or(state.bg);

        // Fill background for this line if it has a custom color (:299-302)
        if line_bg != state.bg {
            buffer.fill_rect(x, y + (start + i) as i32, gutter_width, 1, line_bg);
        }

        // Draw line number only for the first visual line of a logical
        // line (wrapping) — (:304-347)
        if logical_line == last_source {
            // Continuation line, maybe draw a dot or nothing
        } else {
            // Draw 'before' sign if present (:311-322) — right-padded
            // to maxBeforeWidth for alignment (`currentX` upstream is
            // only consumed here)
            let sign = state.line_signs.get(&logical_line);
            if let Some(sign) = sign.filter(|s| s.before.is_some()) {
                let before = sign.before.as_deref().unwrap_or_default();
                let before_width = sign_width(before);
                let padding = i64::from(state.max_before_width) - i64::from(before_width);
                let before_color = sign.before_color.unwrap_or(state.fg);
                draw_gutter_text(
                    buffer,
                    before,
                    x + padding as i32,
                    y + (start + i) as i32,
                    before_color,
                    line_bg,
                );
            }

            // Draw line number (right-aligned in its space with left padding of 1) (:324-337)
            if !state.hide_line_numbers.contains(&logical_line) {
                let custom = state.line_numbers.get(&logical_line).copied();
                let line_num = custom.unwrap_or(logical_line + 1 + state.line_number_offset);
                let line_num_str = line_num.to_string();
                let line_num_width = line_num_str.chars().count() as i64;
                let available_space = i64::from(gutter_width)
                    - i64::from(state.max_before_width)
                    - i64::from(state.max_after_width)
                    - i64::from(state.padding_right);
                let line_num_x =
                    i64::from(state.max_before_width) + 1 + available_space - line_num_width - 1;

                if line_num_x > i64::from(state.max_before_width) {
                    draw_gutter_text(
                        buffer,
                        &line_num_str,
                        x + line_num_x as i32,
                        y + (start + i) as i32,
                        state.fg,
                        line_bg,
                    );
                }
            }

            // Draw 'after' sign if present (:339-344)
            if let Some(sign) = sign.filter(|s| s.after.is_some()) {
                let after = sign.after.as_deref().unwrap_or_default();
                let after_x =
                    gutter_width as i64 - state.padding_right as i64 - state.max_after_width as i64;
                let after_color = sign.after_color.unwrap_or(state.fg);
                draw_gutter_text(
                    buffer,
                    after,
                    x + after_x as i32,
                    y + (start + i) as i32,
                    after_color,
                    line_bg,
                );
            }
        }

        last_source = logical_line;
    }
}

/// `buffer.drawText(text, x, y, fg, bg)` into the frame buffer at
/// gutter-relative coordinates — a fully transparent bg reads through
/// to the existing cell (buffer.zig:1363-1370).
fn draw_gutter_text(buffer: &mut OptimizedBuffer, text: &str, x: i32, y: i32, fg: Rgba, bg: Rgba) {
    if x < 0 || y < 0 || x >= buffer.width as i32 || y >= buffer.height as i32 {
        return;
    }
    let bg = if bg.alpha() == 0 { None } else { Some(bg) };
    buffer.draw_text(text, x as u32, y as u32, fg, bg, 0);
}

/// `LineNumberRenderable.renderSelf` — :540-565: full-width content
/// backgrounds, from after the gutter to the end of the wrapper.
/// Bug-for-bug: the fills use `this.x`/`this.y` — the PARENT-RELATIVE
/// layout position, not the screen cache (correct at root level,
/// mispositioned when nested — same class as TimeToFirstDraw.ts:105).
///
/// * `sources` — the target's line sources window
///   (`getLineSources(trunc(scrollY) + start, end - start)`, :549).
#[allow(clippy::too_many_arguments)] // the upstream fill inputs (fills + sources + geometry)
pub fn paint_line_number_self(
    state: &LineNumberState,
    buffer: &mut OptimizedBuffer,
    x: i64,
    y: i64,
    width: u32,
    height: u32,
    gutter_visible: bool,
    gutter_width: u32,
    vline_sources: &[u32],
    source_map: Option<&[i64]>,
    scroll_y: i64,
) {
    // Draw full-width line backgrounds before children render (:542)
    if state.line_colors_content.is_empty() {
        return;
    }

    let start = 0.max(-y);
    let end = (height as i64).min(i64::from(buffer.height) - y);
    if end <= start {
        return;
    }
    if x >= i64::from(buffer.width) || x + width as i64 <= 0 {
        return;
    }

    let sources = line_sources_window(vline_sources, source_map, scroll_y + start, end - start);

    // Calculate the area to fill: from after the gutter (if visible) to
    // the end of our width (:552-553)
    let gutter_w = if gutter_visible { gutter_width } else { 0 };
    let content_width = (width as i64 - gutter_w as i64).max(0) as u32;

    // Draw full-width background colors for lines with custom colors (:556-564)
    for (i, &logical_line) in sources.iter().enumerate() {
        if let Some(&line_bg) = state.line_colors_content.get(&logical_line) {
            buffer.fill_rect(
                (x + gutter_w as i64) as i32,
                (y + start + i as i64) as i32,
                content_width,
                1,
                line_bg,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(after: &str, color: &str) -> LineSign {
        LineSign {
            after: Some(after.to_owned()),
            after_color: Some(parse_color(color)),
            ..Default::default()
        }
    }

    #[test]
    fn gutter_width_digits_and_padding() {
        // calculateWidth — LineNumberRenderable.ts:171-185:
        // baseWidth = max(minWidth, digits + paddingRight + 1)
        let m = GutterMeasure {
            min_width: 3,
            padding_right: 1,
            total_lines: 5, // maxLineNumber 5 → 1 digit
            ..Default::default()
        };
        assert_eq!(m.calculate_width(), 3);
        // 9 lines → maxLineNumber 9 → digits 1 → base 3
        let m = GutterMeasure {
            total_lines: 9,
            ..m
        };
        assert_eq!(m.calculate_width(), 3);
        // 10 lines → 2 digits → base max(3, 2+1+1) = 4
        let m = GutterMeasure {
            total_lines: 10,
            ..m
        };
        assert_eq!(m.calculate_width(), 4);
        // signs add their max widths (Diff uses after " +" → 2)
        let m = GutterMeasure {
            max_after_width: 2,
            ..m
        };
        assert_eq!(m.calculate_width(), 6);
        // custom line numbers participate in the max (:176-180)
        let m = GutterMeasure {
            max_custom_number: Some(42),
            total_lines: 3,
            ..m
        };
        // maxLineNumber 42 → 2 digits → base 4 + 2 = 6
        assert_eq!(m.calculate_width(), 6);
        // offset participates via maxLineNumber (:175): 3 + 97 = 100 →
        // 3 digits → base 5 + after 2 = 7
        let m = GutterMeasure {
            line_number_offset: 97,
            max_custom_number: None,
            total_lines: 3,
            ..m
        };
        assert_eq!(m.calculate_width(), 7);
        // non-positive max → 1 digit (:182)
        let m = GutterMeasure {
            line_number_offset: -10,
            total_lines: 3,
            ..m
        };
        assert_eq!(m.calculate_width(), 3 + 2);
    }

    #[test]
    fn simple_color_darkens_content() {
        // parseLineColor simple format — LineNumberRenderable.ts:398-403
        let mut gutter = HashMap::new();
        let mut content = HashMap::new();
        parse_line_color(
            &LineColorValue::Simple(parse_color("#ff0000")),
            &mut gutter,
            &mut content,
            3,
        );
        let [r, g, b, a] = gutter[&3].floats();
        assert_eq!((r, g, b, a), (1.0, 0.0, 0.0, 1.0));
        let [r, g, b, a] = content[&3].floats();
        // 80% per component (darkenColor :361-363)
        assert!((r - 0.8).abs() < 0.01 && g == 0.0 && b == 0.0 && a == 1.0);
    }

    #[test]
    fn config_color_gutter_only_defaults_darker_content() {
        // :386-397 — content absent + gutter present → darker gutter
        let mut gutter = HashMap::new();
        let mut content = HashMap::new();
        parse_line_color(
            &LineColorValue::Config(LineColorConfig {
                gutter: Some(parse_color("#1a4d1a")),
                content: None,
            }),
            &mut gutter,
            &mut content,
            0,
        );
        assert!(gutter.contains_key(&0) && content.contains_key(&0));
        // explicit content wins (:392-393)
        let mut content = HashMap::new();
        parse_line_color(
            &LineColorValue::Config(LineColorConfig {
                gutter: Some(parse_color("#1a4d1a")),
                content: Some(parse_color("#ffffff")),
            }),
            &mut HashMap::new(),
            &mut content,
            0,
        );
        assert_eq!(content[&0], parse_color("#ffffff"));
        // gutter-only config with no gutter → nothing set
        let mut gutter = HashMap::new();
        let mut content = HashMap::new();
        parse_line_color(
            &LineColorValue::Config(LineColorConfig::default()),
            &mut gutter,
            &mut content,
            0,
        );
        assert!(gutter.is_empty() && content.is_empty());
    }

    #[test]
    fn sign_widths_scan_max() {
        // calculateSignWidths — :155-169
        let mut signs = HashMap::new();
        signs.insert(1, sign(" +", "#22c55e"));
        signs.insert(
            2,
            LineSign {
                before: Some("⚠️".to_owned()),
                ..Default::default()
            },
        );
        let (before, after) = calculate_sign_widths(&signs);
        assert_eq!(after, 2);
        assert_eq!(before, 2, "⚠️ is a 2-column presentation cluster");
    }

    #[test]
    fn sources_window_slices_and_maps() {
        // getLineSources — :354-358 + Code.ts:162-169
        let sources = [0u32, 0, 1, 2, 2];
        assert_eq!(
            line_sources_window(&sources, None, 0, 5),
            vec![0, 0, 1, 2, 2]
        );
        assert_eq!(line_sources_window(&sources, None, 1, 2), vec![0, 1]);
        assert_eq!(line_sources_window(&sources, None, 4, 10), vec![2]);
        assert!(line_sources_window(&sources, None, 5, 3).is_empty());
        // concealment source map remaps rendered rows (Code.ts:157)
        let map = vec![1i64, 1, 2];
        assert_eq!(
            line_sources_window(&[0, 1, 2], Some(&map), 0, 3),
            vec![1, 1, 2]
        );
        assert_eq!(
            line_sources_window(&[0, 0, 1], Some(&map), 0, 3),
            vec![1, 1, 1]
        );
    }

    #[test]
    fn gutter_paint_positions_number_and_signs() {
        // refreshFrameBuffer :292-348 — the " 1 + " gutter geometry:
        // number right-aligned at lineNumX = maxBefore + 1 +
        // availableSpace - numWidth - 1, sign at width - paddingRight -
        // maxAfter (:331-343)
        let mut opts = LineNumberOptions::default();
        opts.line_signs.insert(
            0,
            LineSign {
                after: Some(" +".to_owned()),
                after_color: Some(parse_color("#22c55e")),
                ..Default::default()
            },
        );
        opts.line_numbers.insert(0, 7);
        let state = GutterState::new(&opts);
        // width: digits(7)=1 → base max(3, 3)=3 + after 2 = 5
        let m = state.measure_ctx(1);
        assert_eq!(m.calculate_width(), 5);

        let mut buffer = OptimizedBuffer::new(20, 3);
        let sources = [0u32];
        paint_gutter(&state, 5, 1, &sources, None, 0, &mut buffer, 0, 0);
        // row 0: col0 pad, col1 '7', col2 ' ', col3 '+', col4 padding
        let frame = buffer.char_frame();
        let row0: String = frame
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(7)
            .collect();
        assert!(row0.starts_with(" 7 +"), "gutter row was {row0:?}");
    }

    #[test]
    fn gutter_paint_skips_continuation_rows() {
        // :304-307 — wrapped continuation rows draw no number/sign
        let state = GutterState::new(&LineNumberOptions::default());
        let mut buffer = OptimizedBuffer::new(20, 3);
        let sources = [0u32, 0, 1];
        paint_gutter(&state, 3, 3, &sources, None, 0, &mut buffer, 0, 0);
        let frame = buffer.char_frame();
        let mut lines = frame.lines();
        let row0 = lines.next().unwrap_or_default();
        let row1 = lines.next().unwrap_or_default();
        let row2 = lines.next().unwrap_or_default();
        assert!(row0.starts_with(" 1"), "row0: {row0:?}");
        // row 1 continues logical line 0 — number region stays empty
        assert!(row1.starts_with("   "), "row1 (continuation): {row1:?}");
        assert!(row2.starts_with(" 2"), "row2: {row2:?}");
    }

    #[test]
    fn wrapper_fills_content_backgrounds_after_gutter() {
        // renderSelf :540-565 — fill from x+gutterWidth for contentWidth
        let mut state = LineNumberState::default();
        state.line_colors_content.insert(1, parse_color("#1a4d1a"));
        let mut buffer = OptimizedBuffer::new(20, 2);
        let sources = [0u32, 1];
        paint_line_number_self(&state, &mut buffer, 0, 0, 20, 2, true, 4, &sources, None, 0);
        let cell = buffer.get(5, 1).expect("filled cell");
        assert_eq!(cell.bg, parse_color("#1a4d1a"));
        // before the gutter untouched
        let before = buffer.get(3, 1).expect("gutter cell");
        assert_ne!(before.bg, parse_color("#1a4d1a"));
    }
}
