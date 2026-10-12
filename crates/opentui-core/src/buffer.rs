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

/// The raster target. Width/height in cells; origin top-left.
pub struct OptimizedBuffer {
    pub width: u32,
    pub height: u32,
    cells: Vec<Cell>,
    /// Grapheme byte pool — the `GraphemePool` (grapheme.zig) in its
    /// simplest arena form: ids are 1-based, 0 = none.
    graphemes: Vec<String>,
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
            blend_backdrop: Some(Rgba::rgb(0, 0, 0)),
        };
        // mirror the renderer's frame-clear default: transparent black
        // (renderer.ts:819 backgroundColor = fromInts(0,0,0,0))
        buf.clear(Rgba::from_ints(0, 0, 0, 0));
        buf
    }

    /// `clear` — buffer.zig:479-488: space chars, attributes 0, fg literal
    /// white, bg = argument.
    pub fn clear(&mut self, bg: Rgba) {
        for cell in &mut self.cells {
            cell.char = DEFAULT_SPACE_CHAR;
            cell.attributes = 0;
            cell.fg = Rgba::rgb(255, 255, 255);
            cell.bg = bg;
        }
        self.graphemes.clear();
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

    /// `set` (span-cleanup path) — buffer.zig:513. Wide graphemes overwrite
    /// their full prior span and plant continuation cells.
    pub fn set(&mut self, x: u32, y: u32, cell: Cell) {
        let Some(i) = self.index(x, y) else { return };
        // span cleanup: clear a previous wide span we're overwriting
        let prev = self.cells[i].char;
        if (is_grapheme_char(prev) || is_continuation_char(prev)) && prev != cell.char {
            let id = grapheme_id_from_char(prev);
            let row_start = (y * self.width) as usize;
            let row_end = row_start + self.width as usize - 1;
            let left = if is_grapheme_char(prev) {
                char_right_extent(prev) as usize
            } else {
                0
            };
            let span_start = i.saturating_sub(left.min(i - row_start));
            let span_end = i + char_right_extent(prev) as usize;
            let span_end = span_end.min(row_end).min(self.cells.len() - 1);
            for c in &mut self.cells[span_start..=span_end] {
                if (is_grapheme_char(c.char) || is_continuation_char(c.char))
                    && grapheme_id_from_char(c.char) == id
                {
                    *c = Cell::space();
                }
            }
        }
        self.cells[i] = cell;

        // continuation planting (buffer.zig:576-644): continuation cells
        // copy the CELL's fg/bg/attributes and carry the packed
        // continuation char with left/right extents
        if is_grapheme_char(cell.char) {
            let width = char_right_extent(cell.char) + 1;
            if x + width > self.width {
                // clip: clear rest of row to spaces (upstream EOL memset)
                for c in &mut self.cells[i..((y + 1) * self.width) as usize] {
                    *c = Cell::space();
                }
                return;
            }
            let gid = grapheme_id_from_char(cell.char);
            for offset in 1..width {
                let ci = i + offset as usize;
                self.cells[ci].char = pack_continuation(offset, width - 1 - offset, gid);
                self.cells[ci].fg = cell.fg;
                self.cells[ci].bg = cell.bg;
                self.cells[ci].attributes = cell.attributes;
            }
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

    /// `setCellWithAlphaBlendingCellWithoutImages` — buffer.zig:929-938
    /// (opacity stack not in the slice; opacity ≡ 1.0).
    fn set_cell_with_alpha_blending(&mut self, x: u32, y: u32, cell: Cell) {
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
    /// columns (`tab_width = 2`, buffer.zig:1325).
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
                self.set(cx, y, make_cell(u32::from(byte), fg, bg_cell, attributes));
            }
            return;
        }

        use unicode_segmentation::UnicodeSegmentation;
        let mut advance: u32 = 0;
        for cluster in text.graphemes(true) {
            let cx = x + advance;
            if cx >= self.width {
                break;
            }
            if cluster == "\t" {
                // tab → 2 blank cells (buffer.zig:1392-1405)
                for t in 0..2 {
                    let tx = x + advance + t;
                    if tx >= self.width {
                        break;
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
            let width = cluster_width(cluster);
            if width == 0 {
                // zero-width clusters fold into the next cell upstream
                // (combining marks join the preceding grapheme); we drop
                // them at the slice level — harness will flag if visible.
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

    /// `fillRect` — buffer.zig (fills with bg-colored spaces).
    pub fn fill_rect(&mut self, x: i32, y: i32, width: u32, height: u32, bg: Rgba) {
        for dy in 0..height as i32 {
            for dx in 0..width as i32 {
                let px = x + dx;
                let py = y + dy;
                if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
                    continue;
                }
                let i = (py as u32 * self.width + px as u32) as usize;
                self.cells[i].char = DEFAULT_SPACE_CHAR;
                self.cells[i].attributes = 0;
                self.cells[i].fg = Rgba::rgb(255, 255, 255);
                self.cells[i].bg = bg;
            }
        }
    }

    /// `drawBox` — buffer.zig:2267-2567 (drawVisibleBox + title layout).
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
        let is_at_left = start_x == x;
        let is_at_right = end_x == x + width as i32 - 1;
        let is_at_top = start_y == y;
        let is_at_bottom = end_y == y + height as i32 - 1;

        let chars = opts.border_chars;

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
                self.set(
                    dx as u32,
                    start_y as u32,
                    make_cell(u32::from(ch), opts.border_color, opts.background_color, 0),
                );
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
                self.set(
                    dx as u32,
                    end_y as u32,
                    make_cell(u32::from(ch), opts.border_color, opts.background_color, 0),
                );
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
                self.set(
                    start_x as u32,
                    dy as u32,
                    make_cell(
                        u32::from(chars.vertical),
                        opts.border_color,
                        opts.background_color,
                        0,
                    ),
                );
                dy += 1;
            }
        }
        if sides.right && is_at_right {
            let mut dy = v_start;
            while dy <= v_end {
                self.set(
                    end_x as u32,
                    dy as u32,
                    make_cell(
                        u32::from(chars.vertical),
                        opts.border_color,
                        opts.background_color,
                        0,
                    ),
                );
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
