//! `ScrollBarRenderable` + `SliderRenderable` + `ArrowRenderable` — port of
//! `packages/core/src/renderables/ScrollBar.ts` and `Slider.ts`.
//!
//! The ScrollBar owns its slider/arrows as CHILD renderables
//! (ScrollBar.ts:178-180): child 0 = startArrow, 1 = slider, 2 = endArrow.
//! Because the port's renderable tree is owned by the renderer, the setter
//! cascades (`scrollSize`/`viewportSize`/`scrollPosition`, ScrollBar.ts:64-90)
//! live as free functions over the tree in [`crate::renderables::scrollbox`].
//! This module carries the per-node state + painting.

use crate::buffer::OptimizedBuffer;
use crate::color::Rgba;

/// `defaultThumbBackgroundColor` — Slider.ts:6 (`RGBA.fromHex("#9a9ea3")`).
#[must_use]
pub fn default_thumb_bg() -> Rgba {
    Rgba::rgb(0x9a, 0x9e, 0xa3)
}

/// `defaultTrackBackgroundColor` — Slider.ts:7 (`RGBA.fromHex("#252527")`).
#[must_use]
pub fn default_track_bg() -> Rgba {
    Rgba::rgb(0x25, 0x25, 0x27)
}

/// `orientation` — ScrollBar.ts:24.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

impl Orientation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Orientation::Vertical => "vertical",
            Orientation::Horizontal => "horizontal",
        }
    }
}

/// `ScrollUnit` — ScrollBar.ts:18.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollUnit {
    Absolute,
    Viewport,
    Content,
    Step,
}

/// `SliderRenderable` state — Slider.ts:20-42 (constructor defaults cited
/// per field).
#[derive(Debug, Clone)]
pub struct SliderState {
    pub orientation: Orientation,
    /// `_value` — `options.value ?? min` (Slider.ts:35)
    pub value: f32,
    /// `_min` — `options.min ?? 0` (Slider.ts:33)
    pub min: f32,
    /// `_max` — `options.max ?? 100` (Slider.ts:34)
    pub max: f32,
    /// `_viewPortSize` — `options.viewPortSize ?? max(1, (max-min)*0.1)`
    /// (Slider.ts:36); the setter clamps to `[0.01, max-min]` (Slider.ts:87)
    pub view_port_size: f32,
    /// `_backgroundColor` = TRACK color (Slider.ts:38)
    pub background_color: Rgba,
    /// `_foregroundColor` = THUMB color (Slider.ts:39)
    pub foreground_color: Rgba,
}

impl Default for SliderState {
    fn default() -> Self {
        SliderState {
            orientation: Orientation::Vertical,
            value: 0.0,
            min: 0.0,
            max: 100.0,
            view_port_size: 10.0,
            background_color: default_track_bg(),
            foreground_color: default_thumb_bg(),
        }
    }
}

impl SliderState {
    /// `set viewPortSize` — Slider.ts:86-92: clamp to
    /// `[0.01, max - min]`. Returns true when the stored value changed.
    pub fn set_view_port_size(&mut self, size: f32) -> bool {
        let clamped = (0.01f32).max(size.min(self.max - self.min));
        if clamped != self.view_port_size {
            self.view_port_size = clamped;
            return true;
        }
        false
    }

    /// `set value` — Slider.ts:48-56: clamp to `[min, max]`; returns
    /// `(changed, clamped)` — changed drives the onChange chain.
    pub fn set_value(&mut self, new_value: f32) -> (bool, f32) {
        let clamped = self.min.max(self.max.min(new_value));
        if clamped != self.value {
            self.value = clamped;
            return (true, clamped);
        }
        (false, clamped)
    }

    /// `set min` — Slider.ts:62-70 (value lifts to the new min). Returns
    /// Some(new value) when the value changed (the value setter's own
    /// onChange chain, Slider.ts:48-56).
    pub fn set_min(&mut self, new_min: f32) -> Option<f32> {
        if new_min == self.min {
            return None;
        }
        self.min = new_min;
        if self.value < new_min {
            let (changed, v) = self.set_value(new_min);
            if changed {
                return Some(v);
            }
        }
        None
    }

    /// `set max` — Slider.ts:76-84 (value clamps down to the new max,
    /// firing the value-setter chain). Returns Some(new value) when the
    /// clamp changed the value.
    pub fn set_max(&mut self, new_max: f32) -> Option<f32> {
        if new_max == self.max {
            return None;
        }
        self.max = new_max;
        if self.value > new_max {
            let (changed, v) = self.set_value(new_max);
            if changed {
                return Some(v);
            }
        }
        None
    }

    /// `getVirtualThumbSize` — Slider.ts:314-329 (exact).
    #[must_use]
    pub fn virtual_thumb_size(&self, track_cells: u32) -> f32 {
        let virtual_track_size = (track_cells * 2) as f32;
        let range = self.max - self.min;
        if range == 0.0 {
            return virtual_track_size;
        }
        let viewport_size = self.view_port_size.max(1.0);
        let content_size = range + viewport_size;
        if content_size <= viewport_size {
            return virtual_track_size;
        }
        let thumb_ratio = viewport_size / content_size;
        let calculated = (virtual_track_size * thumb_ratio).floor();
        calculated.max(1.0).min(virtual_track_size)
    }

    /// `getVirtualThumbStart` — Slider.ts:331-341 (exact).
    #[must_use]
    pub fn virtual_thumb_start(&self, track_cells: u32) -> f32 {
        let virtual_track_size = (track_cells * 2) as f32;
        let range = self.max - self.min;
        if range == 0.0 {
            return 0.0;
        }
        let value_ratio = (self.value - self.min) / range;
        let virtual_thumb_size = self.virtual_thumb_size(track_cells);
        (value_ratio * (virtual_track_size - virtual_thumb_size)).round()
    }

    /// `renderSelf` — Slider.ts:228-312: track fill + half-cell thumb.
    /// `w`/`h` are the node's computed cell size.
    pub fn paint(&self, buffer: &mut OptimizedBuffer, x: i32, y: i32, w: u32, h: u32) {
        match self.orientation {
            Orientation::Horizontal => self.paint_horizontal(buffer, x, y, w),
            Orientation::Vertical => self.paint_vertical(buffer, x, y, h),
        }
    }

    /// `renderHorizontal` — Slider.ts:236-273.
    fn paint_horizontal(&self, buffer: &mut OptimizedBuffer, x: i32, y: i32, w: u32) {
        let vts = self.virtual_thumb_size(w);
        let vstart = self.virtual_thumb_start(w);
        let vend = vstart + vts;
        buffer.fill_rect(x, y, w, 1, self.background_color);
        let real_start = (vstart / 2.0).floor() as i32;
        let real_end = ((vend / 2.0).ceil() as i32) - 1;
        let start_x = 0.max(real_start);
        let end_x = (w as i32 - 1).min(real_end);
        for real_x in start_x..=end_x {
            let vcell_start = (real_x * 2) as f32;
            let vcell_end = vcell_start + 2.0;
            let thumb_start_in_cell = vstart.max(vcell_start);
            let thumb_end_in_cell = vend.min(vcell_end);
            let coverage = thumb_end_in_cell - thumb_start_in_cell;
            let ch: char = if coverage >= 2.0 {
                '█'
            } else {
                // Slider.ts:261-266 — left half vs right half
                let is_left_half = thumb_start_in_cell == vcell_start;
                if is_left_half {
                    '▌'
                } else {
                    '▐'
                }
            };
            buffer.set_cell_with_alpha_blending(
                (x + real_x) as u32,
                y as u32,
                crate::buffer::Cell {
                    char: u32::from(ch),
                    fg: self.foreground_color,
                    bg: self.background_color,
                    attributes: 0,
                },
            );
        }
    }

    /// `renderVertical` — Slider.ts:275-312.
    fn paint_vertical(&self, buffer: &mut OptimizedBuffer, x: i32, y: i32, h: u32) {
        let vts = self.virtual_thumb_size(h);
        let vstart = self.virtual_thumb_start(h);
        let vend = vstart + vts;
        buffer.fill_rect(x, y, 1, h, self.background_color);
        let real_start = (vstart / 2.0).floor() as i32;
        let real_end = ((vend / 2.0).ceil() as i32) - 1;
        let start_y = 0.max(real_start);
        let end_y = (h as i32 - 1).min(real_end);
        for real_y in start_y..=end_y {
            let vcell_start = (real_y * 2) as f32;
            let vcell_end = vcell_start + 2.0;
            let thumb_start_in_cell = vstart.max(vcell_start);
            let thumb_end_in_cell = vend.min(vcell_end);
            let coverage = thumb_end_in_cell - thumb_start_in_cell;
            let ch: char = if coverage >= 2.0 {
                '█'
            } else if coverage > 0.0 {
                // Slider.ts:299-305 — top half vs bottom half
                let vpos = thumb_start_in_cell - vcell_start;
                if vpos == 0.0 {
                    '▀'
                } else {
                    '▄'
                }
            } else {
                continue;
            };
            buffer.set_cell_with_alpha_blending(
                x as u32,
                (y + real_y) as u32,
                crate::buffer::Cell {
                    char: u32::from(ch),
                    fg: self.foreground_color,
                    bg: self.background_color,
                    attributes: 0,
                },
            );
        }
    }
}

/// `ArrowRenderable` state — ScrollBar.ts:320-350.
#[derive(Debug, Clone)]
pub struct ArrowState {
    pub direction: ArrowDirection,
    pub foreground_color: Rgba,
    pub background_color: Rgba,
    pub attributes: u32,
    pub up: char,
    pub down: char,
    pub left: char,
    pub right: char,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowDirection {
    Up,
    Down,
    Left,
    Right,
}

impl Default for ArrowState {
    fn default() -> Self {
        // ScrollBar.ts:335-345
        ArrowState {
            direction: ArrowDirection::Up,
            foreground_color: Rgba::from_values(1.0, 1.0, 1.0, 1.0),
            background_color: Rgba::from_values(0.0, 0.0, 0.0, 0.0),
            attributes: 0,
            up: '▲',
            down: '▼',
            left: '◀',
            right: '▶',
        }
    }
}

impl ArrowState {
    /// `renderSelf` — ScrollBar.ts:404-407: single glyph via drawText.
    pub fn paint(&self, buffer: &mut OptimizedBuffer, x: i32, y: i32) {
        let ch = match self.direction {
            ArrowDirection::Up => self.up,
            ArrowDirection::Down => self.down,
            ArrowDirection::Left => self.left,
            ArrowDirection::Right => self.right,
        };
        buffer.draw_text(
            &ch.to_string(),
            x as u32,
            y as u32,
            self.foreground_color,
            Some(self.background_color),
            self.attributes,
        );
    }
}

/// `ScrollBarRenderable` state — ScrollBar.ts:20-36.
#[derive(Debug, Clone)]
pub struct ScrollBarState {
    pub orientation: Orientation,
    /// `_scrollSize` — ScrollBar.ts:28
    pub scroll_size: f32,
    /// `_scrollPosition` — ScrollBar.ts:29
    pub scroll_position: f32,
    /// `_viewportSize` — ScrollBar.ts:30
    pub viewport_size: f32,
    /// `_showArrows` — ScrollBar.ts:31 (default false)
    pub show_arrows: bool,
    /// `_manualVisibility` — ScrollBar.ts:32: a user `visible` write
    /// freezes auto-visibility
    pub manual_visibility: bool,
    /// `scrollStep` — ScrollBar.ts:36
    pub scroll_step: Option<f32>,
    /// slider node id (child 1)
    pub slider_id: String,
    /// start/end arrow node ids (children 0/2)
    pub start_arrow_id: String,
    pub end_arrow_id: String,
}

/// The clamp+round of the `scrollPosition` setter — ScrollBar.ts:72-74
/// (the math opencode v2 cites): `round(clamp(value, 0, scrollSize -
/// viewportSize))`.
#[must_use]
pub fn clamp_scroll_position(value: f32, scroll_size: f32, viewport_size: f32) -> f32 {
    value.max(0.0).min(scroll_size - viewport_size).round()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_position_clamp_matches_scrollbar_ts_72_74() {
        // round(clamp(v, 0, scrollSize - viewportSize))
        assert_eq!(clamp_scroll_position(3.0, 10.0, 8.0), 2.0);
        assert_eq!(clamp_scroll_position(100.0, 10.0, 8.0), 2.0);
        assert_eq!(clamp_scroll_position(-5.0, 10.0, 8.0), 0.0);
        assert_eq!(clamp_scroll_position(0.5, 10.0, 8.0), 1.0); // round
                                                                // viewport ≥ scroll: min(max(0,v), negative) can go NEGATIVE —
                                                                // the oracle trace shows scrollPosition = -7 during the ScrollBox
                                                                // settle (scrollSize 0, viewportSize 7); harmless: the slider
                                                                // clamps its value back to 0
        assert_eq!(clamp_scroll_position(3.0, 5.0, 8.0), -3.0);
        assert_eq!(clamp_scroll_position(0.0, 0.0, 7.0), -7.0);
    }

    #[test]
    fn thumb_geometry_matches_slider_ts() {
        // Slider.ts:314-341. The oracle-pinned static scenario: track 8
        // cells (vTrack 16), range 2, slider vp 3 → thumb floor(16 × 3/5)=9
        // half-cells → rows ████▀
        let s = SliderState {
            orientation: Orientation::Vertical,
            min: 0.0,
            max: 2.0,
            value: 0.0,
            view_port_size: 3.0,
            ..Default::default()
        };
        assert_eq!(s.virtual_thumb_size(8), 9.0);
        assert_eq!(s.virtual_thumb_start(8), 0.0);
        // scrolled to end: start = round(1 × (16 - 9)) = 7
        let s = SliderState { value: 2.0, ..s };
        assert_eq!(s.virtual_thumb_start(8), 7.0);
        // range 0 → thumb is the whole track, start 0
        let s = SliderState {
            max: 0.0,
            view_port_size: 1.0,
            ..s
        };
        assert_eq!(s.virtual_thumb_size(8), 16.0);
        assert_eq!(s.virtual_thumb_start(8), 0.0);
    }

    #[test]
    fn view_port_size_clamp_matches_slider_ts_86_92() {
        let mut s = SliderState {
            min: 0.0,
            max: 0.0,
            view_port_size: 1.0,
            ..Default::default()
        };
        // clamp to [0.01, max - min]
        assert!(s.set_view_port_size(8.0)); // max-min 0 → 0.01
        assert!((s.view_port_size - 0.01).abs() < 1e-9);
        s.max = 3.0;
        assert!(s.set_view_port_size(8.0)); // clamps to 3 (oracle-pinned)
        assert_eq!(s.view_port_size, 3.0);
        assert!(!s.set_view_port_size(8.0)); // no change → no event
    }

    #[test]
    fn max_clamp_fires_value_change_like_slider_ts_76_84() {
        // the sticky-bottom oracle pin: max 3→2 clamps value 3→2 and the
        // change must surface (drives content.translateY)
        let mut s = SliderState {
            min: 0.0,
            max: 3.0,
            value: 3.0,
            view_port_size: 1.0,
            ..Default::default()
        };
        assert_eq!(s.set_max(2.0), Some(2.0));
        assert_eq!(s.value, 2.0);
    }
}
