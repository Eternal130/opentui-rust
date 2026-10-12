//! The cell-grid raster buffer — port of `OptimizedBuffer`
//! (`packages/native/src/buffer.zig` + its TS mirror
//! `packages/core/src/buffer.ts`).
//!
//! Cell layout (buffer.zig:150-200): `char` u32 (codepoint or packed
//! grapheme/continuation marker), `fg`/`bg` packed [`Rgba`], `attributes`
//! u32 (8 style bits + 24-bit link id — see [`crate::ansi`]).
//!
//! The slice implements the draw ops the vertical slice needs with
//! upstream-exact semantics: [`clear`](OptimizedBuffer::clear),
//! [`set_cell`](OptimizedBuffer::set_cell),
//! [`draw_text`](OptimizedBuffer::draw_text),
//! [`fill_rect`](OptimizedBuffer::fill_rect),
//! [`draw_box`](OptimizedBuffer::draw_box) — plus the two capture forms the
//! differential harness compares: span lines (buffer.ts:172-234
//! `getSpanLines`) and the char frame.

use crate::color::Rgba;
use crate::renderables::box_::{BorderCharSet, BorderSidesConfig};

// --- char word packing (grapheme.zig:17-19, 44-46, 675-696) ----------------

pub const CHAR_FLAG_IMAGE: u32 = 0x4000_0000;
pub const CHAR_FLAG_GRAPHEME: u32 = 0x8000_0000;
pub const CHAR_FLAG_CONTINUATION: u32 = 0xC000_0000;
pub const CHAR_TYPE_MASK: u32 = 0xC000_0000;
pub const GRAPHEME_ID_MASK: u32 = 0x03FF_FFFF;
const CHAR_EXT_RIGHT_SHIFT: u32 = 28;
const CHAR_EXT_LEFT_SHIFT: u32 = 26;
const CHAR_EXT_MASK: u32 = 0x3;

/// `DEFAULT_SPACE_CHAR` — buffer.zig (clear fills with this).
const DEFAULT_SPACE_CHAR: u32 = b' ' as u32;

#[must_use]
pub fn is_grapheme_char(c: u32) -> bool {
    (c & CHAR_TYPE_MASK) == CHAR_FLAG_GRAPHEME
}

#[must_use]
pub fn is_continuation_char(c: u32) -> bool {
    (c & CHAR_TYPE_MASK) == CHAR_FLAG_CONTINUATION
}

#[must_use]
pub fn grapheme_id_from_char(c: u32) -> u32 {
    c & GRAPHEME_ID_MASK
}

#[must_use]
pub fn char_right_extent(c: u32) -> u32 {
    (c >> CHAR_EXT_RIGHT_SHIFT) & CHAR_EXT_MASK
}

/// left extent — bits 26-27 (grapheme.zig packing; read by span cleanup,
/// buffer.zig:565).
#[must_use]
pub fn char_left_extent(c: u32) -> u32 {
    (c >> CHAR_EXT_LEFT_SHIFT) & CHAR_EXT_MASK
}

#[must_use]
pub fn pack_grapheme_start(gid: u32, total_width: u32) -> u32 {
    let width_minus_one = (total_width - 1).min(CHAR_EXT_MASK);
    CHAR_FLAG_GRAPHEME | (width_minus_one << CHAR_EXT_RIGHT_SHIFT) | (gid & GRAPHEME_ID_MASK)
}

#[must_use]
pub fn pack_continuation(left: u32, right: u32, gid: u32) -> u32 {
    CHAR_FLAG_CONTINUATION
        | ((left & CHAR_EXT_MASK) << CHAR_EXT_LEFT_SHIFT)
        | ((right & CHAR_EXT_MASK) << CHAR_EXT_RIGHT_SHIFT)
        | (gid & GRAPHEME_ID_MASK)
}

/// One cell — the four parallel arrays of upstream collapsed into a struct
/// (the arrays are a native-memory artifact; semantics identical).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub char: u32,
    pub fg: Rgba,
    pub bg: Rgba,
    pub attributes: u32,
}

impl Cell {
    fn space() -> Cell {
        Cell {
            char: DEFAULT_SPACE_CHAR,
            fg: Rgba::rgb(255, 255, 255),
            bg: Rgba::rgb(0, 0, 0),
            attributes: 0,
        }
    }
}

fn make_cell(char: u32, fg: Rgba, bg: Rgba, attributes: u32) -> Cell {
    Cell {
        char,
        fg,
        bg,
        attributes,
    }
}

/// A span of same-style cells — the capture unit (`CapturedSpan`,
/// core/src/types.ts).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CapturedSpan {
    pub text: String,
    #[serde(with = "rgba_serde")]
    pub fg: Rgba,
    #[serde(with = "rgba_serde")]
    pub bg: Rgba,
    pub attributes: u8,
    pub width: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CapturedLine {
    pub spans: Vec<CapturedSpan>,
}

mod rgba_serde {
    use super::Rgba;
    use serde::ser::SerializeTuple;

    pub fn serialize<S: serde::Serializer>(rgba: &Rgba, s: S) -> Result<S::Ok, S::Error> {
        let words = rgba.0;
        let mut tup = s.serialize_tuple(4)?;
        for w in words {
            tup.serialize_element(&w)?;
        }
        tup.end()
    }
}

/// `ClipRect` — buffer.zig:190-198 (scissor stack entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The raster target. Width/height in cells; origin top-left.
pub struct OptimizedBuffer {
    pub width: u32,
    pub height: u32,
    cells: Vec<Cell>,
    /// Grapheme byte pool — the `GraphemePool` (grapheme.zig) in its
    /// simplest arena form: ids are 1-based, 0 = none.
    graphemes: Vec<String>,
    /// `GraphemeTracker.used_ids` (grapheme.zig:770+) — live grapheme id →
    /// cell refcount. Drives the drawBox transparent-border fast path's
    /// `!grapheme_tracker.hasAny()` condition (buffer.zig:2255).
    grapheme_refs: std::collections::HashMap<u32, u32>,
    /// The scissor stack (buffer.zig:199 `scissor_stack`). Empty = no clip.
    scissor_stack: Vec<ClipRect>,
    /// `blendBackdropColor` — the color transparent destinations blend
    /// against (renderer sets it from its background color made opaque,
    /// renderer.zig:495; default opaque black for a transparent bg).
    blend_backdrop: Option<Rgba>,
}

fn mul_div_255(a: u32, b: u32) -> u32 {
    (a * b + 127) / 255
}

fn round_div(n: u32, d: u32) -> u8 {
    ((n + d / 2) / d).min(255) as u8
}

/// `blendColors` — buffer.zig:114-143. Exact integer math; results carry
/// literal RGB intent (upstream returns via `ansi.rgbColor`).
fn blend_colors(src: Rgba, dst0: Rgba, backdrop: Option<Rgba>) -> Rgba {
    let sa = u32::from(src.alpha());
    if sa == 0 {
        return dst0;
    }
    let dst = if dst0.alpha() == 0 {
        backdrop.unwrap_or(dst0)
    } else {
        dst0
    };
    if sa == 255 {
        return Rgba::rgb(src.red(), src.green(), src.blue());
    }
    let da = u32::from(dst.alpha());
    let inv = 255 - sa;
    let out_a = sa + mul_div_255(da, inv);
    if out_a == 0 {
        return Rgba::from_ints(0, 0, 0, 0);
    }
    if da == 255 {
        return Rgba::rgb(
            ((u32::from(src.red()) * sa + u32::from(dst.red()) * inv + 127) / 255) as u8,
            ((u32::from(src.green()) * sa + u32::from(dst.green()) * inv + 127) / 255) as u8,
            ((u32::from(src.blue()) * sa + u32::from(dst.blue()) * inv + 127) / 255) as u8,
        );
    }
    Rgba::from_ints(
        round_div(
            u32::from(src.red()) * sa + mul_div_255(u32::from(dst.red()) * da, inv),
            out_a,
        ),
        round_div(
            u32::from(src.green()) * sa + mul_div_255(u32::from(dst.green()) * da, inv),
            out_a,
        ),
        round_div(
            u32::from(src.blue()) * sa + mul_div_255(u32::from(dst.blue()) * da, inv),
            out_a,
        ),
        out_a as u8,
    )
}

fn is_rgba_with_alpha(c: Rgba) -> bool {
    c.alpha() < 255
}

fn is_fully_opaque(fg: Rgba, bg: Rgba) -> bool {
    !is_rgba_with_alpha(fg) && !is_rgba_with_alpha(bg)
}

fn is_fully_transparent(fg: Rgba, bg: Rgba) -> bool {
    fg.alpha() == 0 && bg.alpha() == 0
}

impl OptimizedBuffer {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let mut buf = OptimizedBuffer {
            width,
            height,
            cells: vec![Cell::space(); (width * height) as usize],
            graphemes: Vec::new(),
            grapheme_refs: std::collections::HashMap::new(),
            scissor_stack: Vec::new(),
            blend_backdrop: Some(Rgba::rgb(0, 0, 0)),
        };
        // mirror the renderer's frame-clear default: transparent black
        // (renderer.ts:819 backgroundColor = fromInts(0,0,0,0))
        buf.clear(Rgba::from_ints(0, 0, 0, 0));
        buf
    }

    /// `clear` — buffer.zig:479-488: space chars, attributes 0, fg literal
    /// white, bg = argument. Also clears the scissor stack
    /// (grapheme/link trackers cleared alongside).
    pub fn clear(&mut self, bg: Rgba) {
        for cell in &mut self.cells {
            cell.char = DEFAULT_SPACE_CHAR;
            cell.attributes = 0;
            cell.fg = Rgba::rgb(255, 255, 255);
            cell.bg = bg;
        }
        self.graphemes.clear();
        self.grapheme_refs.clear();
        self.scissor_stack.clear();
    }

    // --- scissor stack (buffer.zig:322-399, exact) -------------------------

    /// `getCurrentScissorRect` — buffer.zig:322-325.
    #[must_use]
    pub fn current_scissor_rect(&self) -> Option<ClipRect> {
        self.scissor_stack.last().copied()
    }

    /// `isPointInScissor` — buffer.zig:327-331. No scissor = everything in.
    #[must_use]
    pub fn is_point_in_scissor(&self, x: i32, y: i32) -> bool {
        let Some(s) = self.current_scissor_rect() else {
            return true;
        };
        x >= s.x && x < s.x + s.width as i32 && y >= s.y && y < s.y + s.height as i32
    }

    /// `isRectInScissor` — buffer.zig:333-343: overlap test (touching edges
    /// do not overlap).
    #[must_use]
    pub fn is_rect_in_scissor(&self, x: i32, y: i32, width: u32, height: u32) -> bool {
        let Some(s) = self.current_scissor_rect() else {
            return true;
        };
        let rect_end_x = x.saturating_add(width as i32);
        let rect_end_y = y.saturating_add(height as i32);
        let scissor_end_x = s.x.saturating_add(s.width as i32);
        let scissor_end_y = s.y.saturating_add(s.height as i32);
        !(x >= scissor_end_x || rect_end_x <= s.x || y >= scissor_end_y || rect_end_y <= s.y)
    }

    /// `clipRectToScissor` — buffer.zig:345-372: intersection or None.
    #[must_use]
    pub fn clip_rect_to_scissor(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Option<ClipRect> {
        let scissor = self.current_scissor_rect().unwrap_or(ClipRect {
            x,
            y,
            width,
            height,
        });
        let rect_end_x = x.saturating_add(width as i32);
        let rect_end_y = y.saturating_add(height as i32);
        let scissor_end_x = scissor.x.saturating_add(scissor.width as i32);
        let scissor_end_y = scissor.y.saturating_add(scissor.height as i32);

        let intersect_x = x.max(scissor.x);
        let intersect_y = y.max(scissor.y);
        let intersect_end_x = rect_end_x.min(scissor_end_x);
        let intersect_end_y = rect_end_y.min(scissor_end_y);

        if intersect_x >= intersect_end_x || intersect_y >= intersect_end_y {
            return None; // No intersection
        }

        Some(ClipRect {
            x: intersect_x,
            y: intersect_y,
            width: (intersect_end_x - intersect_x) as u32,
            height: (intersect_end_y - intersect_y) as u32,
        })
    }

    /// `pushScissorRect` — buffer.zig:375-393: intersect with the current
    /// scissor so nested rects always clip to parents; a rect completely
    /// outside degenerates to 0×0 (nothing renders).
    pub fn push_scissor_rect(&mut self, x: i32, y: i32, width: u32, height: u32) {
        let mut rect = ClipRect {
            x,
            y,
            width,
            height,
        };
        if self.current_scissor_rect().is_some() {
            match self.clip_rect_to_scissor(rect.x, rect.y, rect.width, rect.height) {
                Some(clipped) => rect = clipped,
                None => {
                    rect = ClipRect {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    }
                }
            }
        }
        self.scissor_stack.push(rect);
    }

    /// `popScissorRect` — buffer.zig:395-399.
    pub fn pop_scissor_rect(&mut self) {
        self.scissor_stack.pop();
    }

    /// `clearScissorRects` — buffer.zig:401-403.
    pub fn clear_scissor_rects(&mut self) {
        self.scissor_stack.clear();
    }

    /// `grapheme_tracker.hasAny()` (grapheme.zig:822-824) — any live
    /// grapheme cell in the buffer.
    #[must_use]
    pub fn grapheme_tracker_has_any(&self) -> bool {
        !self.grapheme_refs.is_empty()
    }

    fn tracker_add(&mut self, id: u32) {
        *self.grapheme_refs.entry(id).or_insert(0) += 1;
    }

    fn tracker_remove(&mut self, id: u32) {
        if let Some(c) = self.grapheme_refs.get_mut(&id) {
            *c -= 1;
            if *c == 0 {
                self.grapheme_refs.remove(&id);
            }
        }
    }

    /// `GraphemeTracker.replace` (grapheme.zig:808-813).
    fn tracker_replace(&mut self, old_id: Option<u32>, new_id: Option<u32>) {
        if old_id.is_some() && new_id.is_some() && old_id == new_id {
            return;
        }
        if let Some(id) = new_id {
            self.tracker_add(id);
        }
        if let Some(id) = old_id {
            self.tracker_remove(id);
        }
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some((y * self.width + x) as usize)
    }

    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> Option<Cell> {
        self.index(x, y).map(|i| self.cells[i])
    }

    /// `set` — buffer.zig:513 (`setInternal(span_cleanup = true)`), exact:
    /// bounds + scissor gate (`validateAndIndex`, buffer.zig:650-655), span
    /// cleanup of an overwritten wide span (fg/bg of cleared cells KEPT —
    /// buffer.zig:571-590), continuation planting, grapheme tracker
    /// accounting (buffer.zig:533,553,612), and the end-of-row clip for
    /// graphemes that don't fit (fg/bg/attributes come from the CELL,
    /// buffer.zig:595-620).
    pub fn set(&mut self, x: u32, y: u32, cell: Cell) {
        // validateAndIndex (buffer.zig:650-655)
        let Some(i) = self.index(x, y) else { return };
        if !self.is_point_in_scissor(x as i32, y as i32) {
            return;
        }
        let prev = self.cells[i].char;
        let mut tracker_replaced = false;

        // span cleanup (buffer.zig:558-590)
        if (is_grapheme_char(prev) || is_continuation_char(prev)) && prev != cell.char {
            let row_start = (y * self.width) as usize;
            let row_end = row_start + self.width as usize - 1;
            let left = char_left_extent(prev) as usize;
            let right = char_right_extent(prev) as usize;
            let id = grapheme_id_from_char(prev);

            let new_grapheme_id: Option<u32> = if is_grapheme_char(cell.char) {
                let new_width = char_right_extent(cell.char) + 1;
                if x + new_width <= self.width {
                    Some(grapheme_id_from_char(cell.char))
                } else {
                    None
                }
            } else {
                None
            };
            self.tracker_replace(Some(id), new_grapheme_id);
            tracker_replaced = true;

            let span_start = i.saturating_sub(left.min(i - row_start));
            let span_end = (i + right.min(row_end - i)).min(self.cells.len() - 1);
            for c in &mut self.cells[span_start..=span_end] {
                if (is_grapheme_char(c.char) || is_continuation_char(c.char))
                    && grapheme_id_from_char(c.char) == id
                {
                    // buffer.zig:586-587 — only char + attributes reset
                    c.char = DEFAULT_SPACE_CHAR;
                    c.attributes = 0;
                }
            }
        }

        if is_grapheme_char(cell.char) {
            let right = char_right_extent(cell.char);
            let width = 1 + right;
            if x + width > self.width {
                // EOL clip (buffer.zig:595-620): spaces to end of row, but
                // fg/bg/attributes from the cell
                let end_of_line = (((y + 1) * self.width) as usize).min(self.cells.len());
                for c in &mut self.cells[i..end_of_line] {
                    c.char = DEFAULT_SPACE_CHAR;
                    c.fg = cell.fg;
                    c.bg = cell.bg;
                    c.attributes = cell.attributes;
                }
                return;
            }
            self.cells[i] = cell;
            let id = grapheme_id_from_char(cell.char);
            let is_same_grapheme_start = is_grapheme_char(prev) && prev == cell.char;
            if !tracker_replaced && !is_same_grapheme_start {
                self.tracker_add(id);
            }
            if width > 1 {
                let row_end_index = (y * self.width + self.width - 1) as usize;
                let max_right = (right as usize).min(row_end_index - i);
                if max_right > 0 {
                    for c in &mut self.cells[i + 1..=i + max_right] {
                        c.fg = cell.fg;
                        c.bg = cell.bg;
                        c.attributes = cell.attributes;
                    }
                    for k in 1..=max_right {
                        self.cells[i + k].char =
                            pack_continuation(k as u32, (max_right - k) as u32, id);
                    }
                }
            }
        } else {
            // writeCellAndLinks (buffer.zig:658-669) — links always 0 in
            // this port (no link attributes exist yet)
            self.cells[i] = cell;
        }
    }

    /// `setCell` (TS) / `setCellWithAlphaBlending` when alpha < 1 — for the
    /// slice, alpha blending applies when the incoming bg has 0 < a < 1.
    pub fn set_cell(&mut self, x: u32, y: u32, ch: char, fg: Rgba, bg: Rgba, attributes: u32) {
        let cell = make_cell(ch as u32, fg, bg, attributes);
        self.set(x, y, cell);
    }

    /// `setTextCell` — buffer.zig:1234-1245: transparent-bg cells go
    /// through the alpha-blending writer, others plain set.
    fn set_text_cell(&mut self, x: u32, y: u32, cell: Cell) {
        if is_rgba_with_alpha(cell.bg) {
            self.set_cell_with_alpha_blending(x, y, cell);
            return;
        }
        self.set(x, y, cell);
    }

    /// `setCellWithAlphaBlending` (TS) — maps to the native
    /// `setCellWithAlphaBlendingCell(WithoutImages)` pair (buffer.zig:925-950);
    /// identical when no image placements exist, which is this port's scope.
    /// Scissor gate at the top (buffer.zig:930).
    pub fn set_cell_with_alpha_blending(&mut self, x: u32, y: u32, cell: Cell) {
        if !self.is_point_in_scissor(x as i32, y as i32) {
            return;
        }
        if is_fully_transparent(cell.fg, cell.bg) {
            return;
        }
        if is_fully_opaque(cell.fg, cell.bg) {
            self.set(x, y, cell);
            return;
        }
        let dest = self.get(x, y).unwrap_or_else(Cell::space);
        let blended = self.blend_cells(cell, dest);
        self.set(x, y, blended);
    }

    /// `setRaw` — direct write with no span cleanup / tracker / scissor
    /// (buffer.zig:671-677 `writeCellAndLinks` minus link tracking).
    fn set_raw(&mut self, x: u32, y: u32, cell: Cell) {
        let Some(i) = self.index(x, y) else { return };
        self.cells[i] = cell;
    }

    /// `setCellWithAlphaBlendingRawCell` — buffer.zig:985-1015 (opacity
    /// ≡ 1): scissor gate, opaque → raw write, else `blendCells` over the
    /// destination then raw write (no span cleanup, no tracker).
    fn set_cell_alpha_blending_raw_cell(&mut self, x: u32, y: u32, cell: Cell) {
        if !self.is_point_in_scissor(x as i32, y as i32) {
            return;
        }
        if is_fully_opaque(cell.fg, cell.bg) {
            self.set_raw(x, y, cell);
            return;
        }
        if is_fully_transparent(cell.fg, cell.bg) {
            return;
        }
        match self.get(x, y) {
            Some(dest) => {
                let blended = self.blend_cells(cell, dest);
                self.set_raw(x, y, blended);
            }
            None => self.set_raw(x, y, cell),
        }
    }

    /// `blendCells` — buffer.zig:803-853 (exact).
    fn blend_cells(&self, overlay: Cell, dest: Cell) -> Cell {
        let has_bg_alpha = is_rgba_with_alpha(overlay.bg);
        let has_fg_alpha = is_rgba_with_alpha(overlay.fg);
        if !has_bg_alpha && !has_fg_alpha {
            return overlay;
        }
        let blended_bg = if has_bg_alpha {
            blend_colors(overlay.bg, dest.bg, self.blend_backdrop)
        } else {
            overlay.bg
        };
        let char_is_default_space = overlay.char == DEFAULT_SPACE_CHAR;
        let dest_not_zero = dest.char != 0;
        let dest_not_default_space = dest.char != DEFAULT_SPACE_CHAR;
        let dest_width_is_one = char_right_extent(dest.char) == 0;
        let preserve_char =
            char_is_default_space && dest_not_zero && dest_not_default_space && dest_width_is_one;
        let final_char = if preserve_char {
            dest.char
        } else {
            overlay.char
        };
        let final_fg = if preserve_char {
            blend_colors(overlay.bg, dest.fg, self.blend_backdrop)
        } else if has_fg_alpha {
            blend_colors(overlay.fg, blended_bg, self.blend_backdrop)
        } else {
            overlay.fg
        };
        // attributes: preserved char keeps dest BASE attrs; the link id
        // ALWAYS comes from the overlay (buffer.zig:833-842)
        let base_attrs = if preserve_char {
            u32::from(crate::ansi::get_base_attributes(dest.attributes))
        } else {
            u32::from(crate::ansi::get_base_attributes(overlay.attributes))
        };
        let overlay_link = crate::ansi::get_link_id(overlay.attributes);
        let final_attributes = crate::ansi::set_link_id(base_attrs, overlay_link);
        Cell {
            char: final_char,
            fg: final_fg,
            bg: blended_bg,
            attributes: final_attributes,
        }
    }

    fn alloc_grapheme(&mut self, bytes: &str) -> u32 {
        self.graphemes.push(bytes.to_owned());
        // ids are 1-based within the mask
        (self.graphemes.len() as u32) & GRAPHEME_ID_MASK
    }

    #[must_use]
    pub fn grapheme_bytes(&self, gid: u32) -> &str {
        self.graphemes
            .get((gid - 1) as usize)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// `drawText` — buffer.zig:1246-1420. The ASCII fast path needs
    /// EXPLICIT OPAQUE colors (buffer.zig:1297-1300); `bg: None` reads
    /// through to the existing cell's bg (buffer.zig:1363-1370) and
    /// writes via the blend-aware [`set_text_cell`]. Tab expands to 2
    /// columns (`tab_width = 2`, buffer.zig:1325). Scissor: clusters whose
    /// start cell is outside are skipped but still advance
    /// (buffer.zig:1357-1361); wide clusters additionally require every
    /// span cell inside or the cluster is skipped whole
    /// (buffer.zig:1383-1390); tab cells outside are dropped per cell
    /// (buffer.zig:1396-1397).
    pub fn draw_text(
        &mut self,
        text: &str,
        x: u32,
        y: u32,
        fg: Rgba,
        bg: Option<Rgba>,
        attributes: u32,
    ) {
        if x >= self.width || y >= self.height || text.is_empty() {
            return;
        }
        // ASCII fast path (buffer.zig:1303-1320): explicit opaque colors
        let explicit_opaque = bg.is_some_and(|b| is_fully_opaque(fg, b));
        if explicit_opaque && text.is_ascii() && text.bytes().all(|b| (32..=126).contains(&b)) {
            let bg_cell = bg.unwrap_or(Rgba::rgb(0, 0, 0));
            for (offset, byte) in text.bytes().enumerate() {
                let cx = x + offset as u32;
                if cx >= self.width {
                    break;
                }
                // per-cell scissor via set (buffer.zig:1314 → set → 651-655)
                self.set(cx, y, make_cell(u32::from(byte), fg, bg_cell, attributes));
            }
            return;
        }

        use unicode_segmentation::UnicodeSegmentation;
        let mut advance: u32 = 0;
        'text_loop: for cluster in text.graphemes(true) {
            let cx = x + advance;
            if cx >= self.width {
                break;
            }
            let width = cluster_width(cluster);
            if width == 0 {
                // zero-width clusters fold into the next cell upstream
                // (combining marks join the preceding grapheme); we drop
                // them at the slice level — harness will flag if visible.
                continue;
            }
            let is_tab = cluster == "\t";
            // cluster start outside the scissor: skip, keep advancing
            // (buffer.zig:1357-1361)
            if !is_tab && !self.is_point_in_scissor(cx as i32, y as i32) {
                advance += width;
                continue;
            }
            // wide clusters need every span cell inside (buffer.zig:1383-1390)
            if width > 1 && !is_tab {
                for span_offset in 1..width {
                    if !self.is_point_in_scissor((cx + span_offset) as i32, y as i32) {
                        advance += width;
                        continue 'text_loop;
                    }
                }
            }
            if is_tab {
                // tab → 2 blank cells (buffer.zig:1392-1405)
                for t in 0..2 {
                    let tx = x + advance + t;
                    if tx >= self.width {
                        break;
                    }
                    if !self.is_point_in_scissor(tx as i32, y as i32) {
                        continue;
                    }
                    let bg_cell = match bg {
                        Some(b) => b,
                        None => self.get(tx, y).map_or_else(|| Rgba::rgb(0, 0, 0), |c| c.bg),
                    };
                    let cell = make_cell(DEFAULT_SPACE_CHAR, fg, bg_cell, attributes);
                    self.set_text_cell(tx, y, cell);
                }
                advance += 2;
                continue;
            }
            if cx + width > self.width {
                // buffer.zig:1377-1381 — overflow cluster: skip, keep advancing
                advance += width;
                continue;
            }
            let bg_cell = match bg {
                Some(b) => b,
                None => self.get(cx, y).map_or_else(|| Rgba::rgb(0, 0, 0), |c| c.bg),
            };
            let encoded = if cluster.len() == 1 && width == 1 {
                u32::from(cluster.as_bytes()[0])
            } else if cluster.chars().count() == 1 && width == 1 {
                cluster
                    .chars()
                    .next()
                    .map_or(DEFAULT_SPACE_CHAR, |c| c as u32)
            } else {
                let gid = self.alloc_grapheme(cluster);
                pack_grapheme_start(gid, width)
            };
            self.set_text_cell(cx, y, make_cell(encoded, fg, bg_cell, attributes));
            advance += width;
        }
    }

    /// `fillRect` — buffer.zig:1080-1200 (the non-image subset, opacity ≡ 1):
    /// transparent bg is a no-op; whole-rect scissor early-out; then fill
    /// only the scissor intersection. Grapheme-aware buffers fill through
    /// the blending cell setter (`setCellWithAlphaBlendingCell`), alpha bg
    /// through the raw blend writer, opaque bg through direct row writes.
    pub fn fill_rect(&mut self, x: i32, y: i32, width: u32, height: u32, bg: Rgba) {
        if self.width == 0 || self.height == 0 || width == 0 || height == 0 {
            return;
        }
        // upstream takes u32 coords: negative callers coerce to huge values
        // and early out on the bounds check — mirrored
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let (x, y) = (x as u32, y as u32);
        if !self.is_rect_in_scissor(x as i32, y as i32, width, height) {
            return;
        }
        // fully_transparent + no images → return (buffer.zig:1098-1099)
        let fully_transparent = is_fully_transparent(Rgba::from_ints(0, 0, 0, 0), bg);
        if fully_transparent {
            return;
        }
        let start_x = x;
        let start_y = y;
        let end_x = (self.width - 1).min(x + width - 1);
        let end_y = (self.height - 1).min(y + height - 1);
        if start_x > end_x || start_y > end_y {
            return;
        }
        // clipRectToScissor (buffer.zig:1108)
        let Some(clipped) = self.clip_rect_to_scissor(
            start_x as i32,
            start_y as i32,
            end_x - start_x + 1,
            end_y - start_y + 1,
        ) else {
            return;
        };
        let cx0 = (start_x as i32).max(clipped.x) as u32;
        let cy0 = (start_y as i32).max(clipped.y) as u32;
        let cx1 = (end_x as i32).min(clipped.x + clipped.width as i32 - 1) as u32;
        let cy1 = (end_y as i32).min(clipped.y + clipped.height as i32 - 1) as u32;
        if cx0 > cx1 || cy0 > cy1 {
            return;
        }

        if self.grapheme_tracker_has_any() {
            // buffer.zig:1164-1174 — tracker-safe per-cell setter
            for py in cy0..=cy1 {
                for px in cx0..=cx1 {
                    self.set_cell_with_alpha_blending(
                        px,
                        py,
                        make_cell(DEFAULT_SPACE_CHAR, Rgba::rgb(255, 255, 255), bg, 0),
                    );
                }
            }
        } else if is_rgba_with_alpha(bg) {
            // buffer.zig:1175-1189 — raw blend writer
            for py in cy0..=cy1 {
                for px in cx0..=cx1 {
                    self.set_cell_alpha_blending_raw_cell(
                        px,
                        py,
                        make_cell(DEFAULT_SPACE_CHAR, Rgba::rgb(255, 255, 255), bg, 0),
                    );
                }
            }
        } else {
            // buffer.zig:1190-1205 — direct row fill
            for py in cy0..=cy1 {
                for px in cx0..=cx1 {
                    let i = (py * self.width + px) as usize;
                    self.cells[i].char = DEFAULT_SPACE_CHAR;
                    self.cells[i].attributes = 0;
                    self.cells[i].fg = Rgba::rgb(255, 255, 255);
                    self.cells[i].bg = bg;
                }
            }
        }
    }

    /// `drawBox` — buffer.zig:2267-2567 (drawBox + drawVisibleBox): early
    /// whole-rect scissor cull, the border+bg-fully-transparent early-out,
    /// then fill + borders + titles. Border cells take one of three paths
    /// (buffer.zig:2379-2384): the **transparent-border fast path** (opaque
    /// border over transparent bg, no live graphemes/links: direct
    /// char/fg/attributes write — destination bg KEPT, and notably NO
    /// scissor check), the opaque fast path (`set`), or the alpha-blending
    /// cell writer. Both latter paths are scissor-gated.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_box(&mut self, opts: DrawBoxOptions) {
        let x = opts.x;
        let y = opts.y;
        let width = opts.width;
        let height = opts.height;
        let start_x = x.max(0);
        let start_y = y.max(0);
        let end_x = (self.width as i32 - 1).min(x + width as i32 - 1);
        let end_y = (self.height as i32 - 1).min(y + height as i32 - 1);
        if start_x > end_x || start_y > end_y {
            return;
        }

        let sides = opts.border_sides;
        let box_width = (end_x - start_x + 1) as u32;
        let box_height = (end_y - start_y + 1) as u32;

        // buffer.zig:2343 — whole-rect scissor cull
        if !self.is_rect_in_scissor(start_x, start_y, box_width, box_height) {
            return;
        }
        // buffer.zig:2287-2292 — border+bg fully transparent and no title
        // → nothing to draw (no images in this port)
        let border_bg_transparent =
            opts.border_color.alpha() == 0 && opts.background_color.alpha() == 0;
        let has_title = opts.title.is_some() || opts.bottom_title.is_some();
        let title_visible =
            has_title && !(opts.title_color.alpha() == 0 && opts.background_color.alpha() == 0);
        if border_bg_transparent && !title_visible {
            return;
        }

        let is_at_left = start_x == x;
        let is_at_right = end_x == x + width as i32 - 1;
        let is_at_top = start_y == y;
        let is_at_bottom = end_y == y + height as i32 - 1;

        let chars = opts.border_chars;

        // buffer.zig:2379-2384 — border write path selection
        let use_transparent_border_fast_path = opts.border_color.alpha() == 255
            && opts.background_color.alpha() == 0
            && !self.grapheme_tracker_has_any()
            && chars.all_single_width();
        let use_opaque_fast_path =
            opts.border_color.alpha() == 255 && opts.background_color.alpha() == 255;

        let title_layout = Self::compute_title_layout(
            opts.title.as_deref(),
            sides.top,
            is_at_top,
            start_x,
            end_x,
            width,
            opts.title_alignment,
        );
        let bottom_layout = Self::compute_title_layout(
            opts.bottom_title.as_deref(),
            sides.bottom,
            is_at_bottom,
            start_x,
            end_x,
            width,
            opts.bottom_title_alignment,
        );

        // fill (buffer.zig:2354-2371)
        if opts.should_fill {
            if !sides.top && !sides.right && !sides.bottom && !sides.left {
                self.fill_rect(
                    start_x,
                    start_y,
                    (end_x - start_x + 1) as u32,
                    (end_y - start_y + 1) as u32,
                    opts.background_color,
                );
            } else {
                let ix0 = start_x + i32::from(sides.left && is_at_left);
                let iy0 = start_y + i32::from(sides.top && is_at_top);
                let ix1 = end_x - i32::from(sides.right && is_at_right);
                let iy1 = end_y - i32::from(sides.bottom && is_at_bottom);
                if ix1 >= ix0 && iy1 >= iy0 {
                    self.fill_rect(
                        ix0,
                        iy0,
                        (ix1 - ix0 + 1) as u32,
                        (iy1 - iy0 + 1) as u32,
                        opts.background_color,
                    );
                }
            }
        }

        // The three border-cell writers (buffer.zig:2400-2414 per border cell)
        let write_border_cell = |buf: &mut Self, cx: i32, cy: i32, ch: u32| {
            if use_transparent_border_fast_path {
                // direct: char/fg/attributes, destination bg kept
                // (buffer.zig:2405-2409). NOTE: the vendored Zig source
                // omits the scissor check here, but the PUBLISHED 0.5.14
                // native binary clips these writes (pinned by the
                // scrollbox-scissor-escape differential: a border-only box
                // whose top edge sits above an overflow-hidden viewport
                // paints nothing outside it, while keeping the destination
                // bg inside) — source/binary drift, oracle wins.
                if cx >= 0 && cy >= 0 && cx < buf.width as i32 && cy < buf.height as i32 {
                    if !buf.is_point_in_scissor(cx, cy) {
                        return;
                    }
                    let i = (cy as u32 * buf.width + cx as u32) as usize;
                    buf.cells[i].char = ch;
                    buf.cells[i].fg = opts.border_color;
                    buf.cells[i].attributes = 0;
                }
            } else if use_opaque_fast_path {
                buf.set(
                    cx as u32,
                    cy as u32,
                    make_cell(ch, opts.border_color, opts.background_color, 0),
                );
            } else {
                buf.set_cell_with_alpha_blending(
                    cx as u32,
                    cy as u32,
                    make_cell(ch, opts.border_color, opts.background_color, 0),
                );
            }
        };

        // top border (buffer.zig:2388-2421)
        if sides.top && is_at_top {
            let mut dx = start_x;
            while dx <= end_x {
                if title_layout.draw && dx >= title_layout.start_x && dx <= title_layout.end_x {
                    dx += 1;
                    continue;
                }
                let mut ch = chars.horizontal;
                if dx == start_x && is_at_left {
                    ch = if sides.left {
                        chars.top_left
                    } else {
                        chars.horizontal
                    };
                } else if dx == end_x && is_at_right {
                    ch = if sides.right {
                        chars.top_right
                    } else {
                        chars.horizontal
                    };
                }
                write_border_cell(self, dx, start_y, u32::from(ch));
                dx += 1;
            }
        }
        // bottom border (buffer.zig:2424-2457)
        if sides.bottom && is_at_bottom {
            let mut dx = start_x;
            while dx <= end_x {
                if bottom_layout.draw && dx >= bottom_layout.start_x && dx <= bottom_layout.end_x {
                    dx += 1;
                    continue;
                }
                let mut ch = chars.horizontal;
                if dx == start_x && is_at_left {
                    ch = if sides.left {
                        chars.bottom_left
                    } else {
                        chars.horizontal
                    };
                } else if dx == end_x && is_at_right {
                    ch = if sides.right {
                        chars.bottom_right
                    } else {
                        chars.horizontal
                    };
                }
                write_border_cell(self, dx, end_y, u32::from(ch));
                dx += 1;
            }
        }
        // verticals (buffer.zig:2460-2511) — plain: full height minus
        // horizontal borders
        let v_start = start_y + i32::from(sides.top && is_at_top);
        let v_end = end_y - i32::from(sides.bottom && is_at_bottom);
        if sides.left && is_at_left {
            let mut dy = v_start;
            while dy <= v_end {
                write_border_cell(self, start_x, dy, u32::from(chars.vertical));
                dy += 1;
            }
        }
        if sides.right && is_at_right {
            let mut dy = v_start;
            while dy <= v_end {
                write_border_cell(self, end_x, dy, u32::from(chars.vertical));
                dy += 1;
            }
        }
        // titles (buffer.zig:2513-2523)
        if title_layout.draw {
            if let Some(t) = opts.title.as_deref() {
                self.draw_text(
                    t,
                    title_layout.x as u32,
                    start_y as u32,
                    opts.title_color,
                    Some(opts.background_color),
                    0,
                );
            }
        }
        if bottom_layout.draw {
            if let Some(t) = opts.bottom_title.as_deref() {
                self.draw_text(
                    t,
                    bottom_layout.x as u32,
                    end_y as u32,
                    opts.title_color,
                    Some(opts.background_color),
                    0,
                );
            }
        }
    }

    /// `computeBoxTitleLayout` — buffer.zig:2526-2567. Alignment 0=left,
    /// 1=center, 2=right; padding 2; minimum box width = title + 4.
    #[allow(clippy::too_many_arguments)]
    fn compute_title_layout(
        title: Option<&str>,
        border_side: bool,
        is_at_side: bool,
        start_x: i32,
        end_x: i32,
        width: u32,
        alignment: u8,
    ) -> TitleLayout {
        let Some(text) = title else {
            return TitleLayout::none(start_x);
        };
        if text.is_empty() || !border_side || !is_at_side {
            return TitleLayout::none(start_x);
        }
        let title_len = display_width(text) as i32;
        let min_space = 4;
        if (width as i32) < title_len + min_space {
            return TitleLayout::none(start_x);
        }
        let padding = 2;
        let mut tx = start_x + padding;
        if alignment == 1 {
            tx = start_x + padding.max((width as i32 - title_len).div_euclid(2));
        } else if alignment == 2 {
            tx = start_x + width as i32 - padding - title_len;
        }
        tx = (start_x + padding).max(tx.min(end_x - title_len));
        TitleLayout {
            draw: true,
            x: tx,
            start_x: tx,
            end_x: tx + title_len - 1,
        }
    }

    /// `getSpanLines` — buffer.ts:172-234: per row, coalesce runs of equal
    /// (fg, bg, attributes); continuation cells contribute width with no
    /// text.
    #[must_use]
    pub fn get_span_lines(&self) -> Vec<CapturedLine> {
        let mut lines = Vec::with_capacity(self.height as usize);
        for y in 0..self.height {
            let mut spans: Vec<CapturedSpan> = Vec::new();
            let mut cur: Option<CapturedSpan> = None;
            for x in 0..self.width {
                let Some(cell) = self.get(x, y) else { break };
                let cell_text = if is_continuation_char(cell.char) {
                    String::new()
                } else if is_grapheme_char(cell.char) {
                    self.grapheme_bytes(grapheme_id_from_char(cell.char))
                        .to_owned()
                } else {
                    char::from_u32(cell.char).map_or_else(String::new, |c| c.to_string())
                };
                let attrs = (cell.attributes & 0xff) as u8;
                match &mut cur {
                    Some(span)
                        if span.fg == cell.fg && span.bg == cell.bg && span.attributes == attrs =>
                    {
                        span.text.push_str(&cell_text);
                        span.width += 1;
                    }
                    _ => {
                        if let Some(span) = cur.take() {
                            spans.push(span);
                        }
                        cur = Some(CapturedSpan {
                            text: cell_text,
                            fg: cell.fg,
                            bg: cell.bg,
                            attributes: attrs,
                            width: 1,
                        });
                    }
                }
            }
            if let Some(span) = cur.take() {
                spans.push(span);
            }
            lines.push(CapturedLine { spans });
        }
        lines
    }

    /// The char frame — `getRealCharBytes(true)` + decode: rows joined with
    /// `\n`, continuation cells emitting nothing (buffer.ts:164-170 +
    /// getSpanLines' char walk).
    #[must_use]
    pub fn char_frame(&self) -> String {
        let mut rows = Vec::with_capacity(self.height as usize);
        for y in 0..self.height {
            let mut row = String::new();
            for x in 0..self.width {
                let Some(cell) = self.get(x, y) else { break };
                if is_continuation_char(cell.char) {
                    continue;
                }
                if is_grapheme_char(cell.char) {
                    row.push_str(self.grapheme_bytes(grapheme_id_from_char(cell.char)));
                } else if let Some(c) = char::from_u32(cell.char) {
                    row.push(c);
                }
            }
            rows.push(row);
        }
        rows.join("\n")
    }
}

struct TitleLayout {
    draw: bool,
    x: i32,
    start_x: i32,
    end_x: i32,
}

impl TitleLayout {
    fn none(x: i32) -> Self {
        TitleLayout {
            draw: false,
            x,
            start_x: x,
            end_x: x,
        }
    }
}

/// Options bag for [`OptimizedBuffer::draw_box`] — mirrors the TS
/// `drawBox(options)` shape (buffer.ts:485-527).
pub struct DrawBoxOptions {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub border_sides: BorderSidesConfig,
    pub border_chars: &'static BorderCharSet,
    pub border_color: Rgba,
    pub background_color: Rgba,
    pub should_fill: bool,
    pub title: Option<String>,
    pub title_color: Rgba,
    pub title_alignment: u8,
    pub bottom_title: Option<String>,
    pub bottom_title_alignment: u8,
}

/// Terminal cell width of a grapheme cluster — the `unicode` widthMethod:
/// `unicode_width` per codepoint (wide CJK = 2, emoji = 2), combining marks
/// = 0 folded upstream into the cluster start.
fn cluster_width(cluster: &str) -> u32 {
    cluster
        .chars()
        .map(|c| c.width().map(|w| w as u32).unwrap_or(0))
        .max()
        .unwrap_or(1)
        .max(1)
}

/// Display width of a string (clusters summed).
fn display_width(s: &str) -> u32 {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).map(cluster_width).sum()
}

use unicode_width::UnicodeWidthChar;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderables::box_::border_char_set;

    fn white() -> Rgba {
        Rgba::rgb(255, 255, 255)
    }

    // --- scissor stack (buffer.zig:322-399) ------------------------------

    #[test]
    fn scissor_gates_set_and_nests() {
        let mut b = OptimizedBuffer::new(8, 2);
        b.push_scissor_rect(2, 0, 4, 2); // x 2..5
        b.set_cell(0, 0, 'a', white(), Rgba::rgb(1, 1, 1), 0);
        b.set_cell(3, 0, 'b', white(), Rgba::rgb(1, 1, 1), 0);
        assert_eq!(b.get(0, 0).unwrap().char, b' ' as u32);
        assert_eq!(b.get(3, 0).unwrap().char, b'b' as u32);
        // nested: intersect with parent (buffer.zig:383-392)
        b.push_scissor_rect(4, 0, 4, 2); // x 4..7 ∩ 2..5 = 4..5
        b.set_cell(3, 1, 'c', white(), Rgba::rgb(1, 1, 1), 0);
        b.set_cell(4, 1, 'd', white(), Rgba::rgb(1, 1, 1), 0);
        assert_eq!(b.get(3, 1).unwrap().char, b' ' as u32);
        assert_eq!(b.get(4, 1).unwrap().char, b'd' as u32);
        // degenerate rect when completely outside (buffer.zig:386-390)
        b.push_scissor_rect(100, 100, 2, 2);
        assert_eq!(
            b.current_scissor_rect(),
            Some(ClipRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0
            })
        );
        b.pop_scissor_rect();
        b.pop_scissor_rect();
        // back to the outer [2..5] scissor: (0,1) still blocked
        b.set_cell(0, 1, 'e', white(), Rgba::rgb(1, 1, 1), 0);
        assert_eq!(b.get(0, 1).unwrap().char, b' ' as u32);
        b.pop_scissor_rect();
        b.set_cell(0, 1, 'e', white(), Rgba::rgb(1, 1, 1), 0);
        assert_eq!(b.get(0, 1).unwrap().char, b'e' as u32);
    }

    #[test]
    fn scissor_rect_predicates_match_zig() {
        let mut b = OptimizedBuffer::new(10, 10);
        // no scissor: everything in
        assert!(b.is_point_in_scissor(-5, 100));
        assert!(b.is_rect_in_scissor(0, 0, 10, 10));
        b.push_scissor_rect(2, 2, 3, 3); // [2..4]×[2..4]
        assert!(b.is_point_in_scissor(2, 2));
        assert!(!b.is_point_in_scissor(5, 2));
        // touching edges do not overlap (buffer.zig:341-342)
        assert!(!b.is_rect_in_scissor(5, 2, 1, 1));
        assert!(!b.is_rect_in_scissor(0, 0, 2, 10)); // ends exactly at 2
        assert!(b.is_rect_in_scissor(4, 4, 1, 1));
        // intersection
        assert_eq!(
            b.clip_rect_to_scissor(0, 0, 4, 4),
            Some(ClipRect {
                x: 2,
                y: 2,
                width: 2,
                height: 2
            })
        );
        assert_eq!(b.clip_rect_to_scissor(5, 5, 2, 2), None);
    }

    #[test]
    fn fill_rect_clips_to_scissor_intersection() {
        let mut b = OptimizedBuffer::new(6, 3);
        b.push_scissor_rect(1, 1, 3, 2); // [1..3]×[1..2]
        b.fill_rect(0, 0, 6, 3, Rgba::rgb(9, 9, 9));
        for y in 0..3 {
            for x in 0..6 {
                let inside = (1..=3).contains(&x) && (1..=2).contains(&y);
                let expect = if inside {
                    Rgba::rgb(9, 9, 9)
                } else {
                    Rgba::from_ints(0, 0, 0, 0)
                };
                assert_eq!(b.get(x, y).unwrap().bg, expect, "({x},{y})");
            }
        }
    }

    #[test]
    fn draw_text_skips_clusters_outside_scissor() {
        // buffer.zig:1357-1361 — clusters outside skip but advance
        let mut b = OptimizedBuffer::new(6, 1);
        b.push_scissor_rect(2, 0, 2, 1); // cols 2..3
        b.draw_text("abcdef", 0, 0, white(), Some(Rgba::rgb(1, 2, 3)), 0);
        assert_eq!(b.char_frame(), "  cd  ");
    }

    #[test]
    fn draw_box_fast_path_respects_scissor() {
        // the PUBLISHED 0.5.14 binary clips the transparent-border fast
        // path (source/binary drift — pinned by scrollbox-scissor-escape)
        let mut b = OptimizedBuffer::new(10, 4);
        b.fill_rect(0, 0, 10, 4, Rgba::rgb(50, 50, 50));
        b.push_scissor_rect(0, 2, 10, 2); // rows 2..3
        b.draw_box(DrawBoxOptions {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
            border_sides: crate::renderables::box_::BorderSidesConfig::all(),
            border_chars: border_char_set("single"),
            border_color: white(),
            background_color: Rgba::from_ints(0, 0, 0, 0),
            should_fill: true,
            title: None,
            title_color: white(),
            title_alignment: 0,
            bottom_title: None,
            bottom_title_alignment: 0,
        });
        // top border (row 0) clipped; bottom border (row 3) paints and
        // KEEPS the destination bg (fast path signature)
        assert_eq!(b.get(0, 0).unwrap().char, b' ' as u32);
        assert_eq!(b.get(0, 3).unwrap().char, '└' as u32);
        assert_eq!(b.get(0, 3).unwrap().bg, Rgba::rgb(50, 50, 50));
    }

    // --- exactness fixes pinned this lane ---------------------------------

    #[test]
    fn span_cleanup_keeps_fg_bg_of_cleared_cells() {
        // buffer.zig:586-587 — only char + attributes reset
        let mut b = OptimizedBuffer::new(4, 1);
        b.draw_text("世", 0, 0, white(), Some(Rgba::rgb(7, 7, 7)), 0);
        // overwrite the continuation cell with a plain char
        b.set(1, 0, make_cell(b'x' as u32, white(), Rgba::rgb(9, 9, 9), 0));
        // the span-cleared start cell keeps its fg/bg
        let c = b.get(0, 0).unwrap();
        assert_eq!(c.char, b' ' as u32);
        assert_eq!(c.attributes, 0);
        assert_eq!(c.bg, Rgba::rgb(7, 7, 7));
    }

    #[test]
    fn grapheme_eol_clip_takes_cell_colors() {
        // buffer.zig:595-620 — a wide grapheme that overflows the row:
        // chars to spaces FROM THE GRAPHENE START to end of row, with
        // fg/bg/attributes from the CELL (cells before it untouched)
        let mut b = OptimizedBuffer::new(3, 1);
        let cell = make_cell(
            pack_grapheme_start(1, 2),
            Rgba::rgb(1, 2, 3),
            Rgba::rgb(4, 5, 6),
            0,
        );
        b.set(2, 0, cell);
        let c = b.get(2, 0).unwrap();
        assert_eq!(c.char, b' ' as u32);
        assert_eq!(c.fg, Rgba::rgb(1, 2, 3));
        assert_eq!(c.bg, Rgba::rgb(4, 5, 6));
        // the cells before the clipped grapheme keep the cleared defaults
        assert_eq!(b.get(0, 0).unwrap().bg, Rgba::from_ints(0, 0, 0, 0));
    }

    #[test]
    fn grapheme_tracker_clears_with_buffer() {
        let mut b = OptimizedBuffer::new(4, 1);
        assert!(!b.grapheme_tracker_has_any());
        b.draw_text("世", 0, 0, white(), None, 0);
        assert!(b.grapheme_tracker_has_any());
        b.clear(Rgba::from_ints(0, 0, 0, 0));
        assert!(!b.grapheme_tracker_has_any());
    }

    #[test]
    fn clear_fills_space_white_fg() {
        let mut b = OptimizedBuffer::new(4, 2);
        b.clear(Rgba::rgb(9, 9, 9));
        let c = b.get(0, 0).unwrap();
        assert_eq!(c.char, b' ' as u32);
        assert_eq!(c.fg, Rgba::rgb(255, 255, 255));
        assert_eq!(c.bg, Rgba::rgb(9, 9, 9));
        assert_eq!(c.attributes, 0);
    }

    #[test]
    fn ascii_fast_path() {
        let mut b = OptimizedBuffer::new(10, 1);
        b.draw_text("hello", 0, 0, white(), None, 0);
        assert_eq!(b.char_frame(), "hello     ");
        let c = b.get(0, 0).unwrap();
        assert_eq!(c.char, b'h' as u32);
        // bg absent → reads through to the cleared cell's transparent bg
        assert_eq!(c.bg, Rgba::from_ints(0, 0, 0, 0));
    }

    #[test]
    fn text_over_fill_keeps_box_bg() {
        // the box-border-text oracle semantic: text with no explicit bg
        // over a filled box adopts the box's bg (buffer.zig:1363-1370)
        let mut b = OptimizedBuffer::new(4, 1);
        b.fill_rect(0, 0, 4, 1, Rgba::rgb(0x11, 0x22, 0x33));
        b.draw_text("ab", 0, 0, white(), None, 0);
        let c = b.get(0, 0).unwrap();
        assert_eq!(c.char, b'a' as u32);
        assert_eq!(c.bg, Rgba::rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn wide_char_plants_continuation() {
        let mut b = OptimizedBuffer::new(4, 1);
        b.draw_text("世", 0, 0, white(), None, 0);
        assert!(is_grapheme_char(b.get(0, 0).unwrap().char));
        assert!(is_continuation_char(b.get(1, 0).unwrap().char));
        assert_eq!(b.char_frame(), "世  ");
        // one span: cleared bg is transparent and the grapheme's bg
        // read-through keeps it transparent, so all 4 cells coalesce
        let lines = b.get_span_lines();
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].width, 4);
        assert_eq!(lines[0].spans[0].text, "世  ");
    }

    #[test]
    fn box_borders_and_title() {
        let mut b = OptimizedBuffer::new(12, 4);
        b.draw_box(DrawBoxOptions {
            x: 0,
            y: 0,
            width: 12,
            height: 4,
            border_sides: BorderSidesConfig::all(),
            border_chars: border_char_set("single"),
            border_color: white(),
            background_color: Rgba::from_values(0.0, 0.0, 0.0, 0.0),
            should_fill: false,
            title: Some("hi".into()),
            title_color: white(),
            title_alignment: 0,
            bottom_title: None,
            bottom_title_alignment: 0,
        });
        let frame = b.char_frame();
        let rows: Vec<&str> = frame.split('\n').collect();
        // 12-wide box, title "hi" at x=2 (padding 2): ┌ ─ h i ─────── ┐
        assert_eq!(rows[0], "┌─hi───────┐");
        assert_eq!(rows[1], "│          │");
        assert_eq!(rows[2], "│          │");
        assert_eq!(rows[3], "└──────────┘");
    }

    #[test]
    fn span_coalescing_by_style() {
        let mut b = OptimizedBuffer::new(3, 1);
        b.draw_text("ab", 0, 0, white(), None, 0);
        b.draw_text("c", 2, 0, Rgba::rgb(1, 1, 1), None, 0);
        let spans = b.get_span_lines()[0].spans.clone();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "ab");
        assert_eq!(spans[1].fg, Rgba::rgb(1, 1, 1));
    }

    #[test]
    fn tab_expands_two() {
        let mut b = OptimizedBuffer::new(6, 1);
        b.draw_text("a\tb", 0, 0, white(), None, 0);
        assert_eq!(b.char_frame(), "a  b  ");
    }
}
