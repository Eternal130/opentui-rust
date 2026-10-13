//! The editor core — `EditBuffer` + `EditorView`.
//!
//! Source translation of the native half of `@opentui/core`'s editor stack:
//!
//! * [`EditBuffer`] — `packages/native/src/edit-buffer.zig` (the TS facade
//!   `packages/core/src/edit-buffer.ts` is a thin ptr-wrapping shim; the
//!   semantics live native-side). The rope-of-segments representation
//!   collapses to `Vec<LogicalLine>` (one text chunk per logical line —
//!   the same shape the O2a text engine models), with the **display-width
//!   offset** model preserved exactly: each logical line contributes
//!   `width_cols + 1` (the trailing newline cell; `walkLinesAndSegments`,
//!   text-buffer-iterators.zig:90-92) and offsets are line-start weights
//!   plus the in-line column (`coordsToOffset`, :104-113).
//! * [`EditorView`] — `packages/native/src/editor-view.zig` (+ the
//!   selection core of `text-buffer-view.zig:622-1222`): viewport /
//!   scroll-margin management, wrapping-aware visual↔logical cursor
//!   translation, placeholder switching, and the selection model.
//!
//! Rendering goes through the O2a engine ([`crate::text_buffer`]):
//! [`draw_editor_view`] ports the EditorView branch of
//! `drawTextBufferInternal` (buffer.zig:1652-2087) — viewport background
//! prefill (1671-1679), selection highlight per display cell
//! (1861-1977), tab indicators, INVERSE fg/bg swap after selection
//! (1990-1994).

use crate::buffer::OptimizedBuffer;
use crate::color::Rgba;
use crate::renderables::text::WrapMode;
use crate::styled_text::TextChunk;
use crate::text_buffer::{
    alignment_pad_cols, compute_virtual_lines, Cluster, LogicalLine, TextAlign, TextDefaults,
    TextDocument, Viewport, VirtualLine,
};

/// Default tab width — `tab_width = 2` (text-buffer.zig:291).
pub const EDITOR_TAB_WIDTH: u32 = 2;

// ---------------------------------------------------------------------------
// EditBuffer — edit-buffer.zig
// ---------------------------------------------------------------------------

/// `Cursor` — edit-buffer.zig:30-35. `col` is a display-width column
/// within the logical line; `offset` is the global display-width offset
/// from buffer start (newlines count 1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub row: u32,
    pub col: u32,
    pub desired_col: u32,
    pub offset: u32,
}

/// `LogicalCursor` — the public shape (zig.ts `ExternalLogicalCursor`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogicalCursor {
    pub row: u32,
    pub col: u32,
    pub offset: u32,
}

impl From<Cursor> for LogicalCursor {
    fn from(c: Cursor) -> Self {
        LogicalCursor {
            row: c.row,
            col: c.col,
            offset: c.offset,
        }
    }
}

/// Test surface for [`CursorMeta::decode`] (the meta string format is
/// part of the public undo return contract).
pub mod testutil {
    /// `CursorMeta.decode` (edit-buffer.zig:61-87) —
    /// `cursor:row:col:desired[:tab]` → (row, col, desired, tab).
    pub fn decode_cursor_meta(bytes: &str) -> Option<(u32, u32, u32, Option<u32>)> {
        super::CursorMeta::decode(bytes).map(|m| (m.row, m.col, m.desired_col, m.tab_width))
    }
}

/// One undo point — a rope-root snapshot in upstream (`UndoNode`,
/// rope.zig:1049-1058) + the cursor meta encoded alongside it
/// (`CursorMeta`, edit-buffer.zig:39-95). The port snapshots the line
/// model; observable behavior (restore text + cursor) is identical.
#[derive(Debug, Clone)]
struct UndoSnapshot {
    lines: Vec<LogicalLine>,
    meta: CursorMeta,
}

/// `CursorMeta` — edit-buffer.zig:39-95. `tab_width` is present in
/// stored metas (encoded `cursor:row:col:desired[:tab]`); the public
/// return strips it (`CursorMeta.publicBytes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorMeta {
    row: u32,
    col: u32,
    desired_col: u32,
    tab_width: Option<u32>,
}

impl CursorMeta {
    fn from_cursor(cursor: Cursor, tab_width: u32) -> CursorMeta {
        CursorMeta {
            row: cursor.row,
            col: cursor.col,
            desired_col: cursor.desired_col,
            tab_width: Some(tab_width),
        }
    }

    /// `CursorMeta.decode` (61-87) — `cursor:row:col:desired[:tab]`.
    fn decode(bytes: &str) -> Option<CursorMeta> {
        let rest = bytes.strip_prefix("cursor:")?;
        let mut parts = rest.split(':');
        let row = parts.next()?.parse().ok()?;
        let col = parts.next()?.parse().ok()?;
        let desired_col = parts.next()?.parse().ok()?;
        let tab_width = match parts.next() {
            Some(part) => Some(part.parse().ok()?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(CursorMeta {
            row,
            col,
            desired_col,
            tab_width,
        })
    }

    /// `CursorMeta.publicBytes` (89-94) — strip the tab-width tail.
    fn public_string(&self) -> String {
        format!("cursor:{}:{}:{}", self.row, self.col, self.desired_col)
    }
}

/// `EditBuffer` — edit-buffer.zig:143. One primary cursor (index 0 of
/// upstream's `cursors` list; the TS facade only exposes the primary).
#[derive(Debug, Clone)]
pub struct EditBuffer {
    lines: Vec<LogicalLine>,
    tab_width: u32,
    cursor: Cursor,
    /// `text_buffer.defaults()` — the fg/bg/attributes every cell
    /// starts from (set by EditBufferRenderable, EditBufferRenderable.ts:166-168).
    defaults: TextDefaults,
    /// `undo_history` (rope.zig push/pop; newest last here).
    undo_stack: Vec<UndoSnapshot>,
    /// `redo_history` — linear: cleared by `store_undo`
    /// (`push_redo_branch`, rope.zig:1100-1111).
    redo_stack: Vec<UndoSnapshot>,
    /// `curr_history != null` — an undo/redo boundary is current
    /// (rope.zig:1117/1131 set it; store_undo's `curr_history = null`
    /// at 1045 clears it). `canRedo` requires it (1144).
    redo_boundary: bool,
    /// Event-pump tokens: bumped by the methods that emit
    /// `cursorChanged` / `contentChanged` (edit-buffer.zig `events.emit`
    /// calls). [`EditorView`] pumps them lazily — the upstream event
    /// emitter becomes a version check.
    cursor_version: u64,
    content_version: u64,
}

impl Default for EditBuffer {
    fn default() -> Self {
        EditBuffer::new()
    }
}

/// Split text into logical lines — `findLineBreaks` (utf8.zig,
/// SIMD \n / \r / \r\n scan) + `textToSegments` (text-buffer.zig:632-680):
/// CRLF is ONE break, the chunk excludes both bytes. Empty text ⇒ no
/// lines (`walkLinesAndSegments` early-return, iterators.zig:20-22).
fn text_to_lines(text: &str, tab_width: u32) -> Vec<LogicalLine> {
    if text.is_empty() {
        // the empty document still has ONE (empty) line — the rope's
        // end invariant (oracle: `setText("")` → getLineCount() == 1)
        return vec![build_line("", tab_width)];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            let piece = &text[start..i];
            out.push(build_line(piece, tab_width));
            // CRLF: skip the \n too
            i += if b == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                2
            } else {
                1
            };
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(build_line(&text[start..], tab_width));
    out
}

/// Build a [`LogicalLine`] with a parameterized tab width (the upstream
/// rope chunks re-measure on `setTabWidth`, text-buffer.zig:1330-1360).
fn build_line(text: &str, tab_width: u32) -> LogicalLine {
    let mut clusters = Vec::new();
    let mut byte = 0u32;
    for g in text.graphemes(true) {
        let len = g.len() as u32;
        let width = if g == "\t" {
            tab_width
        } else {
            crate::text_buffer::cluster_width(g)
        };
        clusters.push(Cluster {
            byte_start: byte,
            byte_len: len,
            width,
        });
        byte += len;
    }
    let width_cols = clusters.iter().map(|c| c.width).sum();
    LogicalLine {
        text: text.to_owned(),
        clusters,
        width_cols,
        spans: Vec::new(),
    }
}

impl EditBuffer {
    /// `EditBuffer.init` (edit-buffer.zig:153-189): empty buffer, cursor
    /// `{0,0}`.
    pub fn new() -> Self {
        EditBuffer {
            lines: vec![build_line("", EDITOR_TAB_WIDTH)],
            tab_width: EDITOR_TAB_WIDTH,
            cursor: Cursor::default(),
            defaults: TextDefaults::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            redo_boundary: false,
            cursor_version: 0,
            content_version: 0,
        }
    }

    // -- geometry (text-buffer-iterators.zig) ------------------------------

    /// `lineWidthAt` (iterators.zig:166+): width of a logical line; 0
    /// outside the buffer.
    pub fn line_width(&self, row: u32) -> u32 {
        self.lines.get(row as usize).map_or(0, |l| l.width_cols)
    }

    /// `getLineCount` — the logical line count (`lineCount`,
    /// text-buffer.zig:682). 0 for a fully empty buffer.
    pub fn line_count(&self) -> u32 {
        self.lines.len() as u32
    }

    /// `textEndOffset` (text-buffer.zig:183) = rope total weight = Σ
    /// (line widths) + (line_count − 1) newlines.
    pub fn text_end_offset(&self) -> u32 {
        let n = self.lines.len() as u32;
        if n == 0 {
            return 0;
        }
        self.lines.iter().map(|l| l.width_cols).sum::<u32>() + n - 1
    }

    /// Line-start weight — the `linestart` marker's `global_weight`
    /// (`coordsToOffset`, iterators.zig:108).
    fn line_start_offset(&self, row: u32) -> Option<u32> {
        if row as usize >= self.lines.len() {
            return None;
        }
        let mut weight = 0u32;
        for line in &self.lines[..row as usize] {
            weight += line.width_cols + 1;
        }
        Some(weight)
    }

    /// `getLineStartOffset` (TS facade; edit-buffer.zig line-start
    /// weight). 0 outside the buffer (upstream returns the clamped
    /// marker weight via `editBufferGetLineStartOffset`).
    pub fn get_line_start_offset(&self, row: u32) -> u32 {
        self.line_start_offset(row).unwrap_or(0)
    }

    /// `coordsToOffset` (iterators.zig:104-113): row must exist, col ≤
    /// line width; `None` otherwise.
    pub fn coords_to_offset(&self, row: u32, col: u32) -> Option<u32> {
        if row as usize >= self.lines.len() {
            return None;
        }
        let width = self.lines[row as usize].width_cols;
        if col > width {
            return None;
        }
        Some(self.line_start_offset(row).unwrap_or(0) + col)
    }

    /// `offsetToCoords` (iterators.zig:117+, binary search over linestart
    /// markers): offset ≤ total weight. Empty buffer ⇒ `None`
    /// (`linestart_count == 0`).
    pub fn offset_to_coords(&self, offset: u32) -> Option<(u32, u32)> {
        if self.lines.is_empty() {
            return None;
        }
        if offset > self.text_end_offset() {
            return None;
        }
        let mut weight = 0u32;
        for (row, line) in self.lines.iter().enumerate() {
            let start = weight;
            let end = start + line.width_cols;
            if offset <= end {
                let col = offset - start;
                // offsets past this line's end-but-before-newline are
                // clamped to the line (col <= width by construction)
                return Some((row as u32, col.min(line.width_cols)));
            }
            weight = end + 1;
        }
        // offset == total: last line, EOL
        let last = self.lines.len() - 1;
        Some((last as u32, self.lines[last].width_cols))
    }

    /// Byte offset of a display column within a line — the inverse of
    /// the cluster walk (`findPosByWidth`). Columns always land on
    /// cluster boundaries for valid cursors.
    fn col_to_byte(line: &LogicalLine, col: u32) -> usize {
        let mut cols = 0u32;
        for c in &line.clusters {
            if cols >= col {
                return c.byte_start as usize;
            }
            cols += c.width;
        }
        line.text.len()
    }

    /// `getTextUnitBoundsAt(…, cluster_graphemes=false/true)`
    /// (iterators.zig): the cursor unit at `col` — the first cluster
    /// whose start column is ≥ col (`findPosByWidth` with
    /// `include_start_before=false` snaps forward past wide clusters).
    /// Returns (start_col, end_col) of the unit, `None` at/past EOL.
    fn unit_bounds_at(line: &LogicalLine, col: u32) -> Option<(u32, u32)> {
        if col >= line.width_cols {
            return None;
        }
        let mut cols = 0u32;
        for c in &line.clusters {
            if cols >= col {
                return Some((cols, cols + c.width));
            }
            cols += c.width;
        }
        None
    }

    /// `getGraphemeWidthAt(row, col)` (text-buffer.zig:175): width of the
    /// cursor unit at col.
    pub fn grapheme_width_at(&self, row: u32, col: u32) -> u32 {
        let Some(line) = self.lines.get(row as usize) else {
            return 0;
        };
        Self::unit_bounds_at(line, col).map_or(0, |(s, e)| e - s)
    }

    /// `getPrevGraphemeWidth(row, col)` (text-buffer.zig:179 /
    /// iterators.zig:190+): the width of the grapheme ending at the
    /// cursor unit's start. Clamps col to the line width; 0 at BOL.
    pub fn prev_grapheme_width(&self, row: u32, col: u32) -> u32 {
        let Some(line) = self.lines.get(row as usize) else {
            return 0;
        };
        if col == 0 {
            return 0;
        }
        let clamped = col.min(line.width_cols);
        // unit at clamped col; the previous grapheme ends where it starts
        let Some((unit_start, _)) = Self::unit_bounds_at(line, clamped) else {
            // clamped == line width: previous grapheme = the last cluster
            return line.clusters.last().map_or(0, |c| c.width);
        };
        if unit_start == 0 {
            return 0;
        }
        // the cluster whose end column == unit_start
        let mut cols = 0u32;
        for c in &line.clusters {
            let end = cols + c.width;
            if end == unit_start {
                return c.width;
            }
            cols = end;
        }
        0
    }

    /// `graphemeBoundsAtOffset` (text-buffer.zig:187): global bounds
    /// {start,end} of the grapheme containing the offset; `None` if the
    /// offset is past the end or at a line end (no cluster).
    pub fn grapheme_bounds_at_offset(&self, offset: u32) -> Option<(u32, u32)> {
        let (row, col) = self.offset_to_coords(offset)?;
        let line = self.lines.get(row as usize)?;
        let line_start = self.line_start_offset(row)?;
        let (s, e) = Self::unit_bounds_at(line, col)?;
        Some((line_start + s, line_start + e))
    }

    /// `cursorUnitBoundsAtOffset` (text-buffer.zig:202) — same shape;
    /// identical for the port's atomic clusters.
    pub fn cursor_unit_bounds_at_offset(&self, offset: u32) -> Option<(u32, u32)> {
        self.grapheme_bounds_at_offset(offset)
    }

    // -- cursor ------------------------------------------------------------

    /// `getPrimaryCursor` (edit-buffer.zig:225).
    pub fn primary_cursor(&self) -> Cursor {
        self.cursor
    }

    /// `getCursorPosition` (edit-buffer.zig:705-713).
    pub fn cursor_position(&self) -> LogicalCursor {
        LogicalCursor {
            row: self.cursor.row,
            col: self.cursor.col,
            offset: self.cursor.offset,
        }
    }

    /// `setCursor` (edit-buffer.zig:230-247): clamp row to lineCount−1
    /// (saturating — an empty buffer stays row 0), col to the line width;
    /// desired_col follows; emits cursorChanged.
    pub fn set_cursor(&mut self, row: u32, col: u32) {
        let line_count = self.line_count();
        let clamped_row = row.min(line_count.saturating_sub(1));
        let line_width = self.line_width(clamped_row);
        let clamped_col = col.min(line_width);
        let offset = self.coords_to_offset(clamped_row, clamped_col).unwrap_or(0);
        self.cursor = Cursor {
            row: clamped_row,
            col: clamped_col,
            desired_col: clamped_col,
            offset,
        };
        self.cursor_version += 1;
    }

    /// `setCursorByOffset` (edit-buffer.zig:249-252): offset → coords →
    /// setCursor (`offsetToCoords orelse {0,0}`).
    pub fn set_cursor_by_offset(&mut self, offset: u32) {
        let (row, col) = self.offset_to_coords(offset).unwrap_or((0, 0));
        self.set_cursor(row, col);
    }

    /// `setCursorToLineCol` — same as `setCursor` (lib.zig:2680 routes
    /// both to the same native call).
    pub fn set_cursor_to_line_col(&mut self, row: u32, col: u32) {
        self.set_cursor(row, col);
    }

    // -- text ---------------------------------------------------------------

    /// `getPlainTextIntoBuffer` (text-buffer.zig:739): lines joined by
    /// `\n` (CRLF/CR normalized to `\n` by the split).
    pub fn get_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `setText` (edit-buffer.zig:616-631): full reset — history cleared
    /// (`rope.clear_history`), add-buffer reset, cursor (0,0),
    /// content-changed.
    pub fn set_text(&mut self, text: &str) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.lines = text_to_lines(text, self.tab_width);
        self.content_version += 1;
        self.set_cursor(0, 0);
    }

    /// `replaceText` (edit-buffer.zig:634-648): undoable set —
    /// autoStoreUndo first, then set + cursor (0,0).
    pub fn replace_text(&mut self, text: &str) {
        self.auto_store_undo();
        self.lines = text_to_lines(text, self.tab_width);
        self.content_version += 1;
        self.set_cursor(0, 0);
    }

    /// `clear` (edit-buffer.zig:798-802): empty the buffer, cursor
    /// (0,0) — history is NOT cleared (rope roots stay reachable); the
    /// one-line invariant holds (rope.clear keeps the empty root).
    pub fn clear(&mut self) {
        self.lines = vec![build_line("", self.tab_width)];
        self.content_version += 1;
        self.set_cursor(0, 0);
    }

    /// `insertText` (edit-buffer.zig:375-436): insert at the cursor;
    /// multi-line text moves the cursor below the inserted block
    /// (`num_breaks` rows, `width_after_last_break` col), single-line
    /// advances by the inserted width; desired_col follows; marks views
    /// dirty; emits cursor+content changed.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.auto_store_undo();

        let cursor = self.cursor;
        let (insert_row, insert_byte) = (
            cursor.row,
            Self::col_to_byte(&self.lines[cursor.row as usize], cursor.col),
        );

        // split into lines (CRLF/CR aware) and splice
        let new_lines = text_to_lines(text, self.tab_width);
        let num_breaks = new_lines.len().saturating_sub(1) as u32;
        let width_after_last_break = new_lines.last().map_or(0, |l| l.width_cols);
        let inserted_width: u32 = new_lines.iter().map(|l| l.width_cols).sum::<u32>();

        splice_lines(
            &mut self.lines,
            insert_row,
            insert_byte,
            new_lines,
            self.tab_width,
        );
        self.content_version += 1;

        if num_breaks > 0 {
            let new_row = insert_row + num_breaks;
            let new_col = width_after_last_break;
            let new_offset = self.coords_to_offset(new_row, new_col).unwrap_or(0);
            self.cursor = Cursor {
                row: new_row,
                col: new_col,
                desired_col: new_col,
                offset: new_offset,
            };
        } else {
            let new_col = cursor.col + inserted_width;
            let new_offset = self
                .coords_to_offset(cursor.row, new_col)
                .unwrap_or(cursor.offset);
            self.cursor = Cursor {
                row: cursor.row,
                col: new_col,
                desired_col: new_col,
                offset: new_offset,
            };
        }
        self.cursor_version += 1;
    }

    /// `insertChar` — lib.zig:2802 routes to insertText with the char's
    /// UTF-8 bytes.
    pub fn insert_char(&mut self, ch: &str) {
        self.insert_text(ch);
    }

    /// `deleteRange(start_cursor, end_cursor)` (edit-buffer.zig:438-473):
    /// normalize order, no-op when equal, autoStoreUndo, splice, cursor
    /// to the clamped start.
    pub fn delete_range(&mut self, start_row: u32, start_col: u32, end_row: u32, end_col: u32) {
        let (start, end) = normalize_range(start_row, start_col, end_row, end_col);
        if start == end {
            return;
        }
        self.auto_store_undo();

        let Some(start_offset) = self.coords_to_offset(start.0, start.1) else {
            return;
        };
        let Some(end_offset) = self.coords_to_offset(end.0, end.1) else {
            return;
        };
        if start_offset >= end_offset {
            return;
        }
        self.delete_offset_range(start_offset, end_offset);

        // cursor → clamped start (edit-buffer.zig:460-468)
        let line_count = self.line_count();
        let clamped_row = if start.0 >= line_count {
            line_count.saturating_sub(1)
        } else {
            start.0
        };
        let line_width = if line_count > 0 {
            self.line_width(clamped_row)
        } else {
            0
        };
        let clamped_col = start.1.min(line_width);
        let offset = self.coords_to_offset(clamped_row, clamped_col).unwrap_or(0);
        self.cursor = Cursor {
            row: clamped_row,
            col: clamped_col,
            desired_col: clamped_col,
            offset,
        };
        self.content_version += 1;
        self.cursor_version += 1;
    }

    /// Offset-range splice shared by deleteRange + selection deletion
    /// (`deleteRangeByWeight`, rope.zig): convert both offsets to byte
    /// positions and rebuild the affected lines.
    fn delete_offset_range(&mut self, start_offset: u32, end_offset: u32) {
        let Some((sr, sc)) = self.offset_to_coords(start_offset) else {
            return;
        };
        let Some((er, ec)) = self.offset_to_coords(end_offset) else {
            return;
        };
        let s_byte = Self::col_to_byte(&self.lines[sr as usize], sc);
        let e_byte = Self::col_to_byte(&self.lines[er as usize], ec);

        if sr == er {
            let line = &mut self.lines[sr as usize];
            let text = format!("{}{}", &line.text[..s_byte], &line.text[e_byte..]);
            *line = build_line(&text, self.tab_width);
        } else {
            // merge tail of start with head of end; drop lines between
            let head = self.lines[sr as usize].text[..s_byte].to_owned();
            let tail = self.lines[er as usize].text[e_byte..].to_owned();
            let merged = build_line(&format!("{head}{tail}"), self.tab_width);
            self.lines
                .splice(sr as usize..=er as usize, std::iter::once(merged));
        }
    }

    /// `backspace` (edit-buffer.zig:475-501): col 0 → join with the
    /// previous line (deleteRange prev-EOL..row-BOL); else delete the
    /// previous grapheme.
    pub fn backspace(&mut self) {
        let cursor = self.cursor;
        if cursor.row == 0 && cursor.col == 0 {
            return;
        }
        if cursor.col == 0 {
            if cursor.row > 0 {
                let prev_line_width = self.line_width(cursor.row - 1);
                self.delete_range(cursor.row - 1, prev_line_width, cursor.row, 0);
            }
        } else {
            let prev_width = self.prev_grapheme_width(cursor.row, cursor.col);
            if prev_width == 0 {
                return;
            }
            let target_col = cursor.col - prev_width;
            self.delete_range(cursor.row, target_col, cursor.row, cursor.col);
        }
    }

    /// `deleteForward` (edit-buffer.zig:503-528). NOTE: upstream stores
    /// an undo point here AND `deleteRange` stores another — two
    /// snapshots of the same state; the second undo is a same-state
    /// restore. Ported faithfully.
    pub fn delete_forward(&mut self) {
        let cursor = self.cursor;
        self.auto_store_undo();

        let line_width = self.line_width(cursor.row);
        let line_count = self.line_count();
        if cursor.col >= line_width {
            if cursor.row + 1 < line_count {
                self.delete_range(cursor.row, line_width, cursor.row + 1, 0);
            }
        } else {
            let width = self.grapheme_width_at(cursor.row, cursor.col);
            if width > 0 {
                self.delete_range(cursor.row, cursor.col, cursor.row, cursor.col + width);
            }
        }
    }

    /// `newLine` — lib.zig:2808 routes to insertText("\n").
    pub fn new_line(&mut self) {
        self.insert_text("\n");
    }

    /// `deleteLine` (edit-buffer.zig:654-691): three cases — middle line
    /// (delete to next line's BOL), last line > 0 (delete from previous
    /// EOL, cursor to prev EOL), only line (delete its content only).
    pub fn delete_line(&mut self) {
        let cursor = self.cursor;
        let line_count = self.line_count();
        if cursor.row >= line_count {
            return;
        }
        if cursor.row + 1 < line_count {
            self.delete_range(cursor.row, 0, cursor.row + 1, 0);
        } else if cursor.row > 0 {
            let prev_line_width = self.line_width(cursor.row - 1);
            let curr_line_width = self.line_width(cursor.row);
            self.delete_range(cursor.row - 1, prev_line_width, cursor.row, curr_line_width);
            let new_row = cursor.row - 1;
            let new_col = prev_line_width;
            let new_offset = self.coords_to_offset(new_row, new_col).unwrap_or(0);
            self.cursor = Cursor {
                row: new_row,
                col: new_col,
                desired_col: new_col,
                offset: new_offset,
            };
            self.cursor_version += 1;
        } else {
            let line_width = self.line_width(cursor.row);
            if line_width > 0 {
                self.delete_range(cursor.row, 0, cursor.row, line_width);
            }
        }
    }

    // -- motion (edit-buffer.zig 530-613) -----------------------------------

    /// `moveLeft` (530-549): previous grapheme, else end of the previous
    /// line; desired_col follows.
    pub fn move_left(&mut self) {
        let mut cursor = self.cursor;
        if cursor.col > 0 {
            let prev_width = self.prev_grapheme_width(cursor.row, cursor.col);
            cursor.col -= prev_width.min(cursor.col);
        } else if cursor.row > 0 {
            cursor.row -= 1;
            cursor.col = self.line_width(cursor.row);
        }
        cursor.desired_col = cursor.col;
        cursor.offset = self.coords_to_offset(cursor.row, cursor.col).unwrap_or(0);
        self.cursor = cursor;
        self.cursor_version += 1;
    }

    /// `moveRight` (551-570): next grapheme, else BOL of the next line.
    pub fn move_right(&mut self) {
        let mut cursor = self.cursor;
        let line_width = self.line_width(cursor.row);
        let line_count = self.line_count();
        if cursor.col < line_width {
            let width = self.grapheme_width_at(cursor.row, cursor.col);
            cursor.col += width;
        } else if cursor.row + 1 < line_count {
            cursor.row += 1;
            cursor.col = 0;
        }
        cursor.desired_col = cursor.col;
        cursor.offset = self.coords_to_offset(cursor.row, cursor.col).unwrap_or(0);
        self.cursor = cursor;
        self.cursor_version += 1;
    }

    /// `moveUp` (572-591): row − 1, col = min(desired_col, line width);
    /// desired_col seeds from col when 0.
    pub fn move_up(&mut self) {
        let mut cursor = self.cursor;
        if cursor.row > 0 {
            if cursor.desired_col == 0 {
                cursor.desired_col = cursor.col;
            }
            cursor.row -= 1;
            let line_width = self.line_width(cursor.row);
            cursor.col = cursor.desired_col.min(line_width);
            cursor.offset = self.coords_to_offset(cursor.row, cursor.col).unwrap_or(0);
        }
        self.cursor = cursor;
        self.cursor_version += 1;
    }

    /// `moveDown` (593-613).
    pub fn move_down(&mut self) {
        let mut cursor = self.cursor;
        let line_count = self.line_count();
        if cursor.row + 1 < line_count {
            if cursor.desired_col == 0 {
                cursor.desired_col = cursor.col;
            }
            cursor.row += 1;
            let line_width = self.line_width(cursor.row);
            cursor.col = cursor.desired_col.min(line_width);
            cursor.offset = self.coords_to_offset(cursor.row, cursor.col).unwrap_or(0);
        }
        self.cursor = cursor;
        self.cursor_version += 1;
    }

    /// `gotoLine` (693-703): past-the-end lines land at the last line's
    /// EOL; in-range lines at BOL.
    pub fn goto_line(&mut self, line: u32) {
        let line_count = self.line_count();
        let target = line.min(line_count.saturating_sub(1));
        if line >= line_count {
            let last_width = self.line_width(target);
            self.set_cursor(target, last_width);
        } else {
            self.set_cursor(target, 0);
        }
    }

    // -- word boundaries (edit-buffer.zig 804-946) --------------------------

    /// `getNextWordBoundary` (804-891): the next layout break whose end
    /// column exceeds the cursor column (script transitions, wrap
    /// breaks from the layout scan), else next line BOL, else EOL.
    pub fn get_next_word_boundary(&self) -> Cursor {
        let cursor = self.cursor;
        let line_count = self.line_count();
        if cursor.row >= line_count {
            return cursor;
        }
        let line = &self.lines[cursor.row as usize];
        let line_width = line.width_cols;

        let (breaks, _first, _last) = crate::text_buffer::layout_wrap_breaks(&line.text);
        // single chunk per line ⇒ cols_before == 0 for every break
        for wrap_break in &breaks {
            let target_col = wrap_break.col_end();
            // break must be strictly after the cursor and within the line
            if target_col > cursor.col && target_col <= line_width {
                // A boundary whose grapheme starts BEFORE the cursor never
                // qualifies; one starting AT the cursor does only when it
                // starts on a word codepoint (script-transition cases
                // like "a日" — edit-buffer.zig:861-873); one after the
                // cursor always does (851-856).
                let qualifies = if wrap_break.col_start > cursor.col {
                    true
                } else if wrap_break.col_start == cursor.col {
                    let byte = (wrap_break.byte_start as usize).min(line.text.len());
                    line.text[byte..]
                        .chars()
                        .next()
                        .is_some_and(|cp| is_word_codepoint(cp as u32))
                } else {
                    false
                };
                if qualifies {
                    let offset = self
                        .coords_to_offset(cursor.row, target_col)
                        .unwrap_or(cursor.offset);
                    return Cursor {
                        row: cursor.row,
                        col: target_col,
                        desired_col: target_col,
                        offset,
                    };
                }
            }
        }

        if cursor.row + 1 < line_count {
            let offset = self
                .coords_to_offset(cursor.row + 1, 0)
                .unwrap_or(cursor.offset);
            return Cursor {
                row: cursor.row + 1,
                col: 0,
                desired_col: 0,
                offset,
            };
        }
        let offset = self
            .coords_to_offset(cursor.row, line_width)
            .unwrap_or(cursor.offset);
        Cursor {
            row: cursor.row,
            col: line_width,
            desired_col: line_width,
            offset,
        }
    }

    /// `getPrevWordBoundary` (893-946): the last break end before the
    /// cursor column, else previous line EOL, else (0,0).
    pub fn get_prev_word_boundary(&self) -> Cursor {
        let cursor = self.cursor;
        if cursor.row == 0 && cursor.col == 0 {
            return cursor;
        }
        let line = &self.lines[cursor.row as usize];
        let mut last_boundary: Option<u32> = None;
        let (breaks, _first, _last) = crate::text_buffer::layout_wrap_breaks(&line.text);
        for wrap_break in &breaks {
            let boundary_col = wrap_break.col_end();
            if boundary_col < cursor.col {
                last_boundary = Some(boundary_col);
            }
        }
        if let Some(boundary_col) = last_boundary {
            let offset = self
                .coords_to_offset(cursor.row, boundary_col)
                .unwrap_or(cursor.offset);
            return Cursor {
                row: cursor.row,
                col: boundary_col,
                desired_col: boundary_col,
                offset,
            };
        }
        if cursor.row > 0 {
            let prev_line_width = self.line_width(cursor.row - 1);
            let offset = self
                .coords_to_offset(cursor.row - 1, prev_line_width)
                .unwrap_or(cursor.offset);
            return Cursor {
                row: cursor.row - 1,
                col: prev_line_width,
                desired_col: prev_line_width,
                offset,
            };
        }
        Cursor {
            row: 0,
            col: 0,
            desired_col: 0,
            offset: 0,
        }
    }

    /// `getEOL` (948-959).
    pub fn get_eol(&self) -> Cursor {
        let cursor = self.cursor;
        let line_count = self.line_count();
        if cursor.row >= line_count {
            return cursor;
        }
        let line_width = self.line_width(cursor.row);
        let offset = self
            .coords_to_offset(cursor.row, line_width)
            .unwrap_or(cursor.offset);
        Cursor {
            row: cursor.row,
            col: line_width,
            desired_col: line_width,
            offset,
        }
    }

    // -- ranges (text-buffer.zig 1380-1407 + iterators extract) -------------

    /// `getTextRange` (edit-buffer.zig:966): text between two
    /// display-width offsets, snapped to grapheme boundaries — start
    /// excludes graphemes starting before it, end includes graphemes
    /// starting before it (`extractTextBetweenOffsets`,
    /// iterators.zig:290+). Newlines join lines.
    pub fn get_text_range(&self, start_offset: u32, end_offset: u32) -> String {
        if start_offset >= end_offset {
            return String::new();
        }
        let total = self.text_end_offset();
        if start_offset >= total {
            return String::new();
        }
        let clamped_end = end_offset.min(total);
        let Some((sr, sc)) = self.offset_to_coords(start_offset) else {
            return String::new();
        };
        let Some((er, ec)) = self.offset_to_coords(clamped_end) else {
            return String::new();
        };

        let mut out = String::new();
        for row in sr..=er {
            let Some(line) = self.lines.get(row as usize) else {
                continue;
            };
            let (local_start, local_end) = if row == sr && row == er {
                (sc, ec)
            } else if row == sr {
                (sc, line.width_cols)
            } else if row == er {
                (0, ec)
            } else {
                (0, line.width_cols)
            };
            // start snaps forward to the grapheme boundary at/after the
            // column (include_start_before = false)
            let byte_start = Self::col_to_byte(line, local_start);
            // end includes graphemes starting before it
            // (include_start_before = true) — the first cluster boundary
            // at/after local_end
            let byte_end = Self::col_to_byte_inclusive(line, local_end);
            if byte_start < byte_end {
                out.push_str(&line.text[byte_start..byte_end]);
            }
            // newline when the break offset is inside the range and this
            // is not the last line (iterators.zig:345-352)
            if row < er {
                let newline_offset = self.line_start_offset(row).unwrap_or(0) + line.width_cols;
                if newline_offset >= start_offset && newline_offset < clamped_end {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// First cluster boundary at/after `col` — `findGraphemePosByWidth(…,
    /// include_start_before = true)`: a wide cluster starting before the
    /// end column is fully included.
    fn col_to_byte_inclusive(line: &LogicalLine, col: u32) -> usize {
        let mut cols = 0u32;
        for c in &line.clusters {
            if cols >= col {
                return c.byte_start as usize;
            }
            cols += c.width;
        }
        line.text.len()
    }

    /// `getTextRangeByCoords` (text-buffer.zig:1403).
    pub fn get_text_range_by_coords(
        &self,
        start_row: u32,
        start_col: u32,
        end_row: u32,
        end_col: u32,
    ) -> String {
        let Some(start_offset) = self.coords_to_offset(start_row, start_col) else {
            return String::new();
        };
        let Some(end_offset) = self.coords_to_offset(end_row, end_col) else {
            return String::new();
        };
        self.get_text_range(start_offset, end_offset)
    }

    // -- history (edit-buffer.zig 719-796 + rope.zig 1042-1157) -------------

    /// `autoStoreUndo` (744-748): snapshot (lines, cursor-meta) before a
    /// mutation; clears the redo stack (`push_redo_branch`).
    fn auto_store_undo(&mut self) {
        let meta = CursorMeta::from_cursor(self.cursor, self.tab_width);
        self.undo_stack.push(UndoSnapshot {
            lines: self.lines.clone(),
            meta,
        });
        self.redo_stack.clear();
        self.redo_boundary = false;
    }

    /// `undo` (750-767): restore the newest snapshot; the state being
    /// left is pushed to redo with the CURRENT meta. Returns the stored
    /// (public) meta, or `None` when there is nothing to undo
    /// (`error.Stop` → null through lib.zig:2820).
    pub fn undo(&mut self) -> Option<String> {
        let current_meta = CursorMeta::from_cursor(self.cursor, self.tab_width);
        let snapshot = self.undo_stack.pop()?;
        // push the current state onto redo (create_undo_node of the
        // current root at undo time — rope.zig:1114)
        self.redo_stack.push(UndoSnapshot {
            lines: std::mem::replace(&mut self.lines, snapshot.lines),
            meta: current_meta,
        });
        // the restored root re-measures with the CURRENT tab width
        // (`refreshTabWidthMetrics`, text-buffer.zig:151-163)
        self.refresh_restored_lines(snapshot.meta);
        self.restore_cursor_from_meta(snapshot.meta);
        self.redo_boundary = true;
        self.content_version += 1;
        self.cursor_version += 1;
        Some(snapshot.meta.public_string())
    }

    /// `redo` (769-784).
    pub fn redo(&mut self) -> Option<String> {
        let snapshot = self.redo_stack.pop()?;
        let current_meta = CursorMeta::from_cursor(self.cursor, self.tab_width);
        self.undo_stack.push(UndoSnapshot {
            lines: std::mem::replace(&mut self.lines, snapshot.lines),
            meta: current_meta,
        });
        self.refresh_restored_lines(snapshot.meta);
        self.restore_cursor_from_meta(snapshot.meta);
        self.redo_boundary = true;
        self.content_version += 1;
        self.cursor_version += 1;
        Some(snapshot.meta.public_string())
    }

    /// Re-measure restored lines under the current tab width when the
    /// snapshot was taken under a different one (the native rope's
    /// `refreshTabWidthMetrics` after undo/redo, text-buffer.zig:152-163).
    fn refresh_restored_lines(&mut self, meta: CursorMeta) {
        if meta.tab_width.is_some_and(|w| w != self.tab_width) {
            self.lines = self
                .lines
                .iter()
                .map(|l| build_line(&l.text, self.tab_width))
                .collect();
        }
    }

    /// `restoreCursorFromMeta` (724-742): setCursor (with tab-width
    /// remap when the stored width differs) + desired_col restore.
    fn restore_cursor_from_meta(&mut self, meta: CursorMeta) {
        let width_changed = meta.tab_width.is_some_and(|w| w != self.tab_width);
        let col = if width_changed {
            self.remap_col(meta.row, meta.col, meta.tab_width.unwrap_or(0))
                .unwrap_or(meta.col)
        } else {
            meta.col
        };
        self.set_cursor(meta.row, col);
        if width_changed && meta.desired_col == meta.col {
            self.cursor.desired_col = col;
        } else {
            self.cursor.desired_col = meta.desired_col;
        }
    }

    /// `remapCol` (254-288): walk the stored line's clusters with the OLD
    /// tab width to the target column, re-measure the prefix with the
    /// current width.
    fn remap_col(&self, row: u32, target_col: u32, old_tab_width: u32) -> Option<u32> {
        let line = self.lines.get(row as usize)?;
        let mut old_col = 0u32;
        let mut new_col = 0u32;
        for c in &line.clusters {
            let w_old = if c.is_tab(&line.text) {
                old_tab_width
            } else {
                c.width
            };
            let w_new = if c.is_tab(&line.text) {
                self.tab_width
            } else {
                c.width
            };
            let next_col = old_col + w_old;
            if target_col <= next_col {
                // position found at/inside this cluster
                if target_col == next_col {
                    return Some(new_col + w_new);
                }
                // mid-cluster: upstream finds the byte at the old width
                // and measures the prefix — a mid-cluster target is not a
                // boundary; snap to the cluster start
                return Some(new_col);
            }
            old_col = next_col;
            new_col += w_new;
        }
        if target_col == old_col {
            Some(new_col)
        } else {
            None
        }
    }

    /// `canUndo` (786).
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// `canRedo` (788) — `redo_history != null and curr_history != null`:
    /// redo entries exist AND an undo/redo boundary is current.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty() && self.redo_boundary
    }

    /// `clearHistory` (794).
    pub fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.redo_boundary = false;
    }

    // -- tabs ----------------------------------------------------------------

    /// `setTabWidth` (290-305): re-measure lines, remap cursor columns.
    pub fn set_tab_width(&mut self, width: u32) {
        let old_width = self.tab_width;
        self.tab_width = width;
        if old_width == width {
            return;
        }
        self.lines = self
            .lines
            .iter()
            .map(|l| build_line(&l.text, width))
            .collect();
        let cursor = self.cursor;
        let new_col = self
            .remap_col(cursor.row, cursor.col, old_width)
            .unwrap_or_else(|| cursor.col.min(self.line_width(cursor.row)));
        let mut new_cursor = cursor;
        if cursor.desired_col == cursor.col {
            new_cursor.desired_col = new_col;
        }
        new_cursor.col = new_col;
        new_cursor.offset = self.coords_to_offset(new_cursor.row, new_col).unwrap_or(0);
        self.cursor = new_cursor;
        self.content_version += 1;
        self.cursor_version += 1;
    }

    /// `getTabWidth`.
    pub fn tab_width(&self) -> u32 {
        self.tab_width
    }

    // -- event pump tokens ----------------------------------------------------

    pub fn cursor_version(&self) -> u64 {
        self.cursor_version
    }

    pub fn content_version(&self) -> u64 {
        self.content_version
    }

    // -- render/view bridge ----------------------------------------------------

    /// Borrow the raw line model (the `EditorView` layout cache reads
    /// it without cloning).
    pub fn lines_for_render(&self) -> &[LogicalLine] {
        &self.lines
    }

    /// The line at a row (view helpers).
    fn line_at(&self, row: u32) -> Option<&LogicalLine> {
        self.lines.get(row as usize)
    }

    /// Direct cursor write for the view's internal moves (upstream
    /// writes `cursors.items[0]` without emitting the event; the view
    /// manages the event pump itself).
    fn set_cursor_internal(&mut self, row: u32, col: u32, offset: u32) {
        self.cursor = Cursor {
            row,
            col,
            desired_col: col,
            offset,
        };
        self.cursor_version += 1;
    }

    /// `setDefaultFg` / `setDefaultBg` / `setDefaultAttributes`
    /// (text-buffer.zig 484-492, EditBuffer.ts:353-371).
    pub fn set_default_fg(&mut self, fg: Rgba) {
        self.defaults.fg = fg;
    }

    pub fn set_default_bg(&mut self, bg: Rgba) {
        self.defaults.bg = bg;
    }

    pub fn set_default_attributes(&mut self, attributes: u32) {
        self.defaults.attributes = attributes;
    }

    pub fn defaults(&self) -> TextDefaults {
        self.defaults
    }

    /// The buffer as a [`TextDocument`] for the O2a raster (plain text —
    /// spans are the syntax-style lane's concern).
    pub fn document(&self) -> TextDocument {
        TextDocument {
            lines: self.lines.clone(),
            defaults: self.defaults,
        }
    }
}

/// Order-normalize a (row,col) pair — `deleteRange`'s swap
/// (edit-buffer.zig:441-445).
fn normalize_range(
    start_row: u32,
    start_col: u32,
    end_row: u32,
    end_col: u32,
) -> ((u32, u32), (u32, u32)) {
    let start = (start_row, start_col);
    let end = (end_row, end_col);
    if start_row > end_row || (start_row == end_row && start_col > end_col) {
        (end, start)
    } else {
        (start, end)
    }
}

/// Splice `new_lines` (a text split) into `lines` at (row, byte): the
/// first piece merges into the target line's head, the last piece
/// merges the original tail; middle lines insert whole
/// (`insertSliceByWeight`, edit-buffer.zig:408-409).
fn splice_lines(
    lines: &mut Vec<LogicalLine>,
    row: u32,
    byte: usize,
    new_lines: Vec<LogicalLine>,
    tab_width: u32,
) {
    if new_lines.is_empty() {
        return;
    }
    let target = &lines[row as usize];
    let head = target.text[..byte].to_owned();
    let tail = target.text[byte..].to_owned();

    if new_lines.len() == 1 {
        let merged = format!("{head}{}{tail}", new_lines[0].text);
        lines[row as usize] = build_line(&merged, tab_width);
        return;
    }
    // multi-line: head+first | middles | last+tail
    let first = build_line(&format!("{head}{}", new_lines[0].text), tab_width);
    let last = build_line(
        &format!("{}{tail}", new_lines[new_lines.len() - 1].text),
        tab_width,
    );
    let replacement: Vec<LogicalLine> = std::iter::once(first)
        .chain(new_lines[1..new_lines.len() - 1].iter().cloned())
        .chain(std::iter::once(last))
        .collect();
    lines.splice(row as usize..=row as usize, replacement);
}

/// `isWordCodepoint` — utf8.zig:270-272: `classifyWordClass(cp) != .other`
/// — ASCII word bytes (229-234) AND CJK word codepoints (236-260).
fn is_word_codepoint(cp: u32) -> bool {
    if cp <= 0x7F {
        let b = cp as u8;
        return b.is_ascii_lowercase() || b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_';
    }
    crate::text_buffer::is_cjk_word_codepoint(cp)
}

// ---------------------------------------------------------------------------
// Selection — text-buffer-view.zig:622-1222
// ---------------------------------------------------------------------------

/// `TextSelection` (text-buffer.zig) — exclusive [start, end) display
/// offsets + the override colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub start: u32,
    pub end: u32,
    pub bg_color: Option<Rgba>,
    pub fg_color: Option<Rgba>,
}

/// `SelectionOccupancy` (text-buffer-view.zig:38-44): how a zero-width
/// caret position maps to occupied cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionOccupancy {
    /// occupy the grapheme under the endpoint
    #[default]
    Cell,
    /// the insertion gap before it
    Boundary,
}

/// `SelectionBehavior` (text-buffer-view.zig:46-51) — mouse-drag
/// expansion semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionBehavior {
    #[default]
    Cell,
    Word,
    Line,
}

/// `SelectionEndpoints` (text-buffer-view.zig:87) — stored anchor/focus
/// (cell-pinned by mouse/keyboard selection so later changes don't
/// re-expand).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectionEndpoints {
    anchor: u32,
    focus: u32,
}

/// `Viewport` re-export with the editor's field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorViewport {
    pub offset_x: u32,
    pub offset_y: u32,
    pub width: u32,
    pub height: u32,
}

impl EditorViewport {
    fn to_viewport(self) -> Viewport {
        Viewport {
            x: self.offset_x,
            y: self.offset_y,
            width: self.width,
            height: self.height,
        }
    }
}

/// `VisualCursor` — editor-view.zig:25-31. Visual coordinates are
/// viewport-relative; logical are document-absolute; offset is the
/// global display-width offset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisualCursor {
    pub visual_row: u32,
    pub visual_col: u32,
    pub logical_row: u32,
    pub logical_col: u32,
    pub offset: u32,
}

/// `CursorVisualAffinity` — editor-view.zig:33-37.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorAffinity {
    offset: u32,
    visual_row: u32,
    visual_col: u32,
}

/// The computed virtual-line layout with the per-vline global offsets
/// (`document_cell_offset`, text-buffer-view.zig:162) and per-logical-
/// line vline indices (`WrapInfo`, :558-564).
#[derive(Debug, Clone, Default)]
pub struct EditorVlines {
    pub vlines: Vec<VirtualLine>,
    /// document cell offset of each vline's first cell
    pub cell_offsets: Vec<u32>,
    /// first vline index per logical line
    pub line_first_vline: Vec<u32>,
    /// vline count per logical line
    pub line_vline_counts: Vec<u32>,
}

/// unreachable fallback for [`EditorView::active_vlines`] — the cache is
/// always `Some` by construction on both branches; keeps the borrow
/// checker happy without an owned return.
static EMPTY_VLINES: EditorVlines = EditorVlines {
    vlines: Vec::new(),
    cell_offsets: Vec::new(),
    line_first_vline: Vec::new(),
    line_vline_counts: Vec::new(),
};

impl EditorVlines {
    fn compute(lines: &[LogicalLine], wrap_mode: WrapMode, wrap_width: Option<u32>) -> Self {
        let doc = TextDocument {
            lines: lines.to_vec(),
            defaults: TextDefaults::default(),
        };
        let vlines = compute_virtual_lines(&doc, wrap_mode, wrap_width);
        // line-start weights over the same slice
        let mut starts = Vec::with_capacity(lines.len());
        let mut weight = 0u32;
        for line in lines {
            starts.push(weight);
            weight += line.width_cols + 1;
        }
        let mut cell_offsets = Vec::with_capacity(vlines.len());
        for v in &vlines {
            let start = starts.get(v.source_line as usize).copied().unwrap_or(0);
            cell_offsets.push(start + v.source_col_start);
        }
        let line_count = lines.len();
        let mut line_first_vline = vec![0u32; line_count];
        let mut line_vline_counts = vec![0u32; line_count];
        for (i, v) in vlines.iter().enumerate() {
            let src = v.source_line as usize;
            if src < line_count {
                if line_vline_counts[src] == 0 {
                    line_first_vline[src] = i as u32;
                }
                line_vline_counts[src] += 1;
            }
        }
        EditorVlines {
            vlines,
            cell_offsets,
            line_first_vline,
            line_vline_counts,
        }
    }

    /// `findVisualLineIndex` (text-buffer-view.zig:566-603): the vline
    /// holding a logical position — a soft-wrap boundary belongs to the
    /// FOLLOWING vline (col >= next source_col_start).
    fn find_visual_line_index(&self, logical_row: u32, logical_col: u32) -> u32 {
        if self.vlines.is_empty() {
            return 0;
        }
        let clamped_row = if logical_row as usize >= self.line_first_vline.len() {
            self.line_first_vline.len().saturating_sub(1) as u32
        } else {
            logical_row
        };
        if clamped_row as usize >= self.line_first_vline.len() {
            return 0;
        }
        let first = self.line_first_vline[clamped_row as usize];
        let count = self.line_vline_counts[clamped_row as usize];
        if count == 0 {
            return first;
        }
        for i in 1..count {
            let idx = first + i;
            if idx as usize >= self.vlines.len() {
                break;
            }
            if logical_col < self.vlines[idx as usize].source_col_start {
                return idx - 1;
            }
        }
        let last = first + count - 1;
        if (last as usize) < self.vlines.len() {
            return last;
        }
        first
    }
}

/// The placeholder document (built from styled chunks) + its layout.
#[derive(Debug, Clone)]
struct Placeholder {
    doc: TextDocument,
}

/// `EditorView` — editor-view.zig:41. Owns the buffer (upstream holds a
/// borrowed pointer; the port composes).
pub struct EditorView {
    pub buffer: EditBuffer,
    viewport: Option<EditorViewport>,
    wrap_mode: WrapMode,
    /// `scroll_margin` — fraction of the viewport (editor-view.zig:44);
    /// default 0.15 (:86), EditBufferRenderable sets 0.2.
    scroll_margin: f32,
    desired_visual_col: Option<u32>,
    cursor_visual_affinity: Option<CursorAffinity>,
    selection: Option<TextSelection>,
    selection_endpoints: Option<SelectionEndpoints>,
    selection_occupancy: SelectionOccupancy,
    selection_behavior: SelectionBehavior,
    selection_updates_cursor: bool,
    /// `selection_follow_cursor` — keep the viewport synced to the
    /// cursor while a selection is active (editor-view.zig:48).
    selection_follow_cursor: bool,
    placeholder: Option<Placeholder>,
    placeholder_active: bool,
    // caches
    vlines: Option<EditorVlines>,
    vlines_key: (u64, u64, WrapMode, Option<u32>),
    placeholder_vlines: Option<EditorVlines>,
    placeholder_key: (u64, u64, WrapMode, Option<u32>),
    placeholder_content_version: u64,
    seen_cursor_version: u64,
}

impl EditorView {
    /// `EditorView.init` (editor-view.zig:75-113).
    pub fn new(buffer: EditBuffer, viewport_width: u32, viewport_height: u32) -> Self {
        let seen_cursor_version = buffer.cursor_version();
        EditorView {
            buffer,
            viewport: Some(EditorViewport {
                offset_x: 0,
                offset_y: 0,
                width: viewport_width,
                height: viewport_height,
            }),
            wrap_mode: WrapMode::None,
            scroll_margin: 0.15,
            desired_visual_col: None,
            cursor_visual_affinity: None,
            selection: None,
            selection_endpoints: None,
            selection_occupancy: SelectionOccupancy::Cell,
            selection_behavior: SelectionBehavior::Cell,
            selection_updates_cursor: false,
            selection_follow_cursor: false,
            placeholder: None,
            placeholder_active: false,
            vlines: None,
            vlines_key: (0, 0, WrapMode::None, None),
            placeholder_vlines: None,
            placeholder_key: (0, 0, WrapMode::None, None),
            placeholder_content_version: 0,
            seen_cursor_version,
        }
    }

    // -- cursor-changed pump (editor-view.zig:58-73) -------------------------

    /// The upstream view subscribes to the buffer's `cursorChanged`
    /// event; the port pumps the version token lazily at every entry
    /// point.
    fn pump_cursor_events(&mut self) {
        let version = self.buffer.cursor_version();
        if version == self.seen_cursor_version {
            return;
        }
        self.seen_cursor_version = version;
        self.on_cursor_changed();
    }

    /// `onCursorChanged` (editor-view.zig:58-73): clear desired col,
    /// placeholder visibility, affinity validation, ensure-cursor-
    /// visible (unless a selection is active without follow).
    fn on_cursor_changed(&mut self) {
        self.desired_visual_col = None;
        self.update_placeholder_visibility();

        let cursor = self.buffer.primary_cursor();
        if let Some(affinity) = self.cursor_visual_affinity {
            if affinity.offset != cursor.offset {
                self.cursor_visual_affinity = None;
            }
        }

        let has_selection = self.selection.is_some();
        if !has_selection || self.selection_follow_cursor {
            let vcursor = self.primary_visual_cursor_absolute();
            self.ensure_cursor_visible(vcursor.visual_row);
        }
    }

    // -- viewport ------------------------------------------------------------

    /// `getViewport` (editor-view.zig:148).
    pub fn get_viewport(&self) -> EditorViewport {
        self.viewport.unwrap_or(EditorViewport {
            offset_x: 0,
            offset_y: 0,
            width: 0,
            height: 0,
        })
    }

    /// `setViewport` (editor-view.zig:136-146): width change clears
    /// affinity; `move_cursor` runs `makeCursorVisible`.
    pub fn set_viewport(&mut self, x: u32, y: u32, width: u32, height: u32, move_cursor: bool) {
        self.pump_cursor_events();
        let old = self.viewport;
        self.viewport = Some(EditorViewport {
            offset_x: x,
            offset_y: y,
            width,
            height,
        });
        if old.is_none_or(|vp| vp.width != width) {
            self.cursor_visual_affinity = None;
        }
        if move_cursor {
            self.make_cursor_visible();
        }
    }

    /// `setViewportSize` (editor-view.zig:470-499): keep offsets, clamp
    /// to the valid range, ensure cursor visible.
    pub fn set_viewport_size(&mut self, width: u32, height: u32) {
        self.pump_cursor_events();
        let old_width = self.viewport.map_or(0, |vp| vp.width);
        if old_width != width {
            self.cursor_visual_affinity = None;
        }
        let mut vp = self.viewport.unwrap_or(EditorViewport {
            offset_x: 0,
            offset_y: 0,
            width,
            height,
        });
        vp.width = width;
        vp.height = height;
        self.viewport = Some(vp);

        let vp = self.viewport.unwrap();
        let total_lines = self.total_virtual_line_count();
        let max_offset_y = total_lines.saturating_sub(vp.height);

        let mut new_offset_x = vp.offset_x;
        if self.wrap_mode == WrapMode::None {
            let max_line_width = self.max_line_width();
            let max_offset_x = max_line_width.saturating_sub(vp.width);
            if vp.offset_x > max_offset_x {
                new_offset_x = max_offset_x;
            }
        }
        if vp.offset_y > max_offset_y || new_offset_x != vp.offset_x {
            self.viewport = Some(EditorViewport {
                offset_x: new_offset_x,
                offset_y: vp.offset_y.min(max_offset_y),
                width: vp.width,
                height: vp.height,
            });
        }

        let vcursor = self.primary_visual_cursor_absolute();
        self.ensure_cursor_visible(vcursor.visual_row);
    }

    /// `setScrollMargin` (198-200) — clamped to [0, 0.5].
    pub fn set_scroll_margin(&mut self, margin: f32) {
        self.scroll_margin = margin.clamp(0.0, 0.5);
    }

    /// `setWrapMode` (501-504).
    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        self.pump_cursor_events();
        self.cursor_visual_affinity = None;
        self.wrap_mode = mode;
    }

    pub fn wrap_mode(&self) -> WrapMode {
        self.wrap_mode
    }

    // -- virtual lines ---------------------------------------------------------

    fn wrap_width(&self) -> Option<u32> {
        match self.wrap_mode {
            WrapMode::None => None,
            _ => self.viewport.map(|vp| vp.width).filter(|w| *w > 0),
        }
    }

    /// Fill the appropriate vline cache (placeholder-aware) and borrow
    /// it. Keys on the buffer content version, the placeholder content
    /// version, the wrap mode, and the wrap width (`virtual_lines_dirty`
    /// machinery, tbv 448-485). Perf lane: returns `&EditorVlines` —
    /// the owned return cloned all four vecs at every call site
    /// (`ensure_cursorVisible` alone hits it several times per frame).
    fn active_vlines(&mut self) -> &EditorVlines {
        let content_version = self.buffer.content_version();
        let wrap = self.wrap_width();
        if self.placeholder_active {
            let pv = self.placeholder_content_version;
            let key = (content_version, pv, self.wrap_mode, wrap);
            if self.placeholder_vlines.is_none() || self.placeholder_key != key {
                let lines: Vec<LogicalLine> = self
                    .placeholder
                    .as_ref()
                    .map_or_else(Vec::new, |p| p.doc.lines.clone());
                self.placeholder_vlines = Some(EditorVlines::compute(&lines, self.wrap_mode, wrap));
                self.placeholder_key = key;
            }
            self.placeholder_vlines.as_ref().unwrap_or(&EMPTY_VLINES)
        } else {
            let key = (
                content_version,
                self.placeholder_content_version,
                self.wrap_mode,
                wrap,
            );
            if self.vlines.is_none() || self.vlines_key != key {
                // measure straight off the buffer's line slice (no
                // intermediate line-list clone)
                self.vlines = Some(EditorVlines::compute(
                    self.buffer.lines_for_render(),
                    self.wrap_mode,
                    wrap,
                ));
                self.vlines_key = key;
            }
            self.vlines.as_ref().unwrap_or(&EMPTY_VLINES)
        }
    }

    /// `getVirtualLineCount` (text-buffer-view.zig:487) — the viewport
    /// slice length (virtualLineCount getter on the TS facade returns
    /// the visible count).
    pub fn get_virtual_line_count(&mut self) -> u32 {
        self.update_before_render();
        let vp = self.get_viewport();
        let vlines = self.active_vlines();
        let start = (vp.offset_y as usize).min(vlines.vlines.len());
        let end = (start + vp.height as usize).min(vlines.vlines.len());
        (end - start) as u32
    }

    /// `getTotalVirtualLineCount` (editor-view.zig:301).
    pub fn total_virtual_line_count(&mut self) -> u32 {
        self.active_vlines().vlines.len() as u32
    }

    /// `getMaxLineWidth` (iterators.zig) — wrap none's x clamp.
    fn max_line_width(&self) -> u32 {
        self.buffer
            .lines_for_render()
            .iter()
            .map(|l| l.width_cols)
            .max()
            .unwrap_or(0)
    }

    // -- scroll ----------------------------------------------------------------

    /// `ensureCursorVisible` (editor-view.zig:208-263): adjust viewport
    /// y (and x in wrap-none) so the cursor line sits inside the margin
    /// band.
    pub fn ensure_cursor_visible(&mut self, cursor_line: u32) {
        let Some(vp) = self.viewport else { return };
        if vp.height == 0 || vp.width == 0 {
            return;
        }
        let margin_lines_raw = 1u32.max((vp.height as f32 * self.scroll_margin) as u32);
        let max_margin_lines = if vp.height > 1 {
            (vp.height - 1) / 2
        } else {
            0
        };
        let margin_lines = margin_lines_raw.min(max_margin_lines);

        let margin_cols_raw = 1u32.max((vp.width as f32 * self.scroll_margin) as u32);
        let max_margin_cols = if vp.width > 1 { (vp.width - 1) / 2 } else { 0 };
        let margin_cols = margin_cols_raw.min(max_margin_cols);

        let total_lines = self.active_vlines().vlines.len() as u32;
        let max_offset_y = total_lines.saturating_sub(vp.height);

        let mut new_offset_y = vp.offset_y;
        let mut new_offset_x = vp.offset_x;

        if cursor_line < vp.offset_y + margin_lines {
            new_offset_y = cursor_line.saturating_sub(margin_lines);
        } else if cursor_line >= vp.offset_y + vp.height - margin_lines {
            let desired = cursor_line + margin_lines - vp.height + 1;
            new_offset_y = desired.min(max_offset_y);
        }

        if self.wrap_mode == WrapMode::None {
            let cursor = self.buffer.primary_cursor();
            let cursor_col = cursor.col;
            if cursor_col < vp.offset_x + margin_cols {
                new_offset_x = cursor_col.saturating_sub(margin_cols);
            } else if cursor_col >= vp.offset_x + vp.width - margin_cols {
                new_offset_x = cursor_col + margin_cols - vp.width + 1;
            }
        }

        if new_offset_y != vp.offset_y || new_offset_x != vp.offset_x {
            self.viewport = Some(EditorViewport {
                offset_x: new_offset_x,
                offset_y: new_offset_y,
                width: vp.width,
                height: vp.height,
            });
        }
    }

    /// `makeCursorVisible` (editor-view.zig:155-194): move the CURSOR
    /// (not the viewport) inside the margin band — used by setViewport
    /// with moveCursor=true.
    pub fn make_cursor_visible(&mut self) {
        let Some(vp) = self.viewport else { return };
        let cursor = self.buffer.primary_cursor();
        let vcursor = self.primary_visual_cursor_absolute();

        let margin_lines = 1u32.max((vp.height as f32 * self.scroll_margin) as u32);

        let cursor_above = vcursor.visual_row < vp.offset_y;
        let cursor_below = vcursor.visual_row >= vp.offset_y + vp.height;
        let too_close_top = vcursor.visual_row < vp.offset_y + margin_lines;
        let too_close_bottom = vcursor.visual_row + margin_lines >= vp.offset_y + vp.height;

        if cursor_above || cursor_below || too_close_top || too_close_bottom {
            let target_visual_row = if cursor_above || too_close_top {
                vp.offset_y + margin_lines
            } else {
                vp.offset_y + vp.height - margin_lines - 1
            };
            let vlines = self.active_vlines();
            if (target_visual_row as usize) < vlines.vlines.len() {
                let target_vline = &vlines.vlines[target_visual_row as usize];
                let target_logical_row = target_vline.source_line;
                let line_width = self.buffer.line_width(target_logical_row);
                let target_col = cursor.col.min(line_width);
                if let Some(offset) = self.buffer.coords_to_offset(target_logical_row, target_col) {
                    self.buffer
                        .set_cursor_internal(target_logical_row, target_col, offset);
                }
            }
        }
    }

    /// `updateBeforeRender` (editor-view.zig:267-276).
    fn update_before_render(&mut self) {
        self.pump_cursor_events();
        self.update_placeholder_visibility();
        if self.selection.is_none() || self.selection_follow_cursor {
            let vcursor = self.primary_visual_cursor_absolute();
            self.ensure_cursor_visible(vcursor.visual_row);
        }
    }

    // -- visual cursor (editor-view.zig 523-680) --------------------------------

    /// `getPrimaryVisualCursorAbsolute` (527-544): document-absolute
    /// visual coordinates, honoring stored affinity.
    fn primary_visual_cursor_absolute(&mut self) -> VisualCursor {
        self.pump_cursor_events();
        let cursor = self.buffer.primary_cursor();
        if let Some(affinity) = self.cursor_visual_affinity {
            if affinity.offset == cursor.offset {
                self.set_cursor_affinity_for_absolute_row(affinity.visual_row);
                if let Some(validated) = self.cursor_visual_affinity {
                    return VisualCursor {
                        visual_row: validated.visual_row,
                        visual_col: validated.visual_col,
                        logical_row: cursor.row,
                        logical_col: cursor.col,
                        offset: cursor.offset,
                    };
                }
            }
        }
        self.logical_to_visual_cursor(cursor.row, cursor.col)
    }

    /// `setCursorAffinityForAbsoluteRow` (546-579).
    fn set_cursor_affinity_for_absolute_row(&mut self, visual_row: u32) {
        let cursor = self.buffer.primary_cursor();
        let occupancy = self.selection_occupancy;
        let viewport_width_cache = self.viewport;
        let vlines = self.active_vlines();
        if visual_row as usize >= vlines.vlines.len() {
            self.cursor_visual_affinity = None;
            return;
        }
        let vline = &vlines.vlines[visual_row as usize];
        let cell_offset = vlines.cell_offsets[visual_row as usize];
        if vline.source_line != cursor.row || cursor.offset < cell_offset {
            self.cursor_visual_affinity = None;
            return;
        }
        let visual_col = cursor.offset - cell_offset;
        let mut visual_col_max = vline.width_cols;
        let viewport_width = viewport_width_cache.map_or(vline.width_cols, |vp| vp.width);
        // An overwide visual line may be one atomic grapheme; do not
        // reserve a viewport-edge cell (563-573).
        if occupancy == SelectionOccupancy::Cell
            && vline.width_cols > 0
            && vline.width_cols <= viewport_width
            && visual_row + 1 < vlines.vlines.len() as u32
            && vlines.vlines[(visual_row + 1) as usize].source_line == vline.source_line
        {
            visual_col_max -= 1;
        }
        if visual_col > visual_col_max {
            self.cursor_visual_affinity = None;
            return;
        }
        self.cursor_visual_affinity = Some(CursorAffinity {
            offset: cursor.offset,
            visual_row,
            visual_col,
        });
    }

    /// `getVisualCursor` (591-611): viewport-relative.
    pub fn get_visual_cursor(&mut self) -> VisualCursor {
        self.update_before_render();
        let vcursor = self.primary_visual_cursor_absolute();
        let Some(vp) = self.viewport else {
            return vcursor;
        };
        let viewport_relative_row = vcursor.visual_row.saturating_sub(vp.offset_y);
        let viewport_relative_col = if self.wrap_mode == WrapMode::None {
            vcursor.visual_col.saturating_sub(vp.offset_x)
        } else {
            vcursor.visual_col
        };
        VisualCursor {
            visual_row: viewport_relative_row,
            visual_col: viewport_relative_col,
            logical_row: vcursor.logical_row,
            logical_col: vcursor.logical_col,
            offset: vcursor.offset,
        }
    }

    /// `logicalToVisualCursor` (615-656): document-absolute.
    pub fn logical_to_visual_cursor(&mut self, logical_row: u32, logical_col: u32) -> VisualCursor {
        let line_count = self.buffer.line_count();
        let clamped_row = if line_count > 0 {
            logical_row.min(line_count - 1)
        } else {
            0
        };
        let line_width = self.buffer.line_width(clamped_row);
        let clamped_col = logical_col.min(line_width);

        let vlines = self.active_vlines();
        let visual_row_idx = vlines.find_visual_line_index(clamped_row, clamped_col);
        if vlines.vlines.is_empty() || visual_row_idx as usize >= vlines.vlines.len() {
            let offset = self
                .buffer
                .coords_to_offset(clamped_row, clamped_col)
                .unwrap_or(0);
            return VisualCursor {
                visual_row: 0,
                visual_col: 0,
                logical_row: clamped_row,
                logical_col: clamped_col,
                offset,
            };
        }
        let vline = &vlines.vlines[visual_row_idx as usize];
        let vline_start_col = vline.source_col_start;
        let visual_col = if clamped_col >= vline_start_col {
            (clamped_col - vline_start_col).min(vline.width_cols)
        } else {
            0
        };
        let offset = self
            .buffer
            .coords_to_offset(clamped_row, clamped_col)
            .unwrap_or(0);
        VisualCursor {
            visual_row: visual_row_idx,
            visual_col,
            logical_row: clamped_row,
            logical_col: clamped_col,
            offset,
        }
    }

    /// `visualToLogicalCursor` (660-680): document-absolute input.
    fn visual_to_logical_cursor(
        &mut self,
        visual_row: u32,
        visual_col: u32,
    ) -> Option<VisualCursor> {
        let vlines = self.active_vlines();
        if visual_row as usize >= vlines.vlines.len() {
            return None;
        }
        let vline = &vlines.vlines[visual_row as usize];
        let clamped_visual_col = visual_col.min(vline.width_cols);
        let logical_col = vline.source_col_start + clamped_visual_col;
        let logical_row = vline.source_line;
        let offset = self
            .buffer
            .coords_to_offset(logical_row, logical_col)
            .unwrap_or(0);
        Some(VisualCursor {
            visual_row,
            visual_col: clamped_visual_col,
            logical_row,
            logical_col,
            offset,
        })
    }

    /// `clampVisualColToStayOnVisualRow` (682-698).
    fn clamp_visual_col(vlines: &EditorVlines, visual_row: u32, visual_col: u32) -> u32 {
        if visual_row as usize >= vlines.vlines.len() {
            return visual_col;
        }
        let vline = &vlines.vlines[visual_row as usize];
        let mut target = visual_col.min(vline.width_cols);
        // A wrap boundary is also the start of the next visual line —
        // step back so the target canonicalizes onto THIS line.
        if target == vline.width_cols
            && vline.width_cols > 0
            && ((visual_row + 1) as usize) < vlines.vlines.len()
            && vlines.vlines[(visual_row + 1) as usize].source_line == vline.source_line
        {
            target = vline.width_cols - 1;
        }
        target
    }

    /// `moveUpVisual` (700-739).
    pub fn move_up_visual(&mut self) {
        self.pump_cursor_events();
        let vcursor = self.primary_visual_cursor_absolute();
        if vcursor.visual_row == 0 {
            return;
        }
        let target_row = vcursor.visual_row - 1;
        if self.desired_visual_col.is_none() {
            self.desired_visual_col = Some(vcursor.visual_col);
        }
        let desired = self.desired_visual_col.unwrap();
        // clamp the target col onto the target row (682-698) — gather
        // only the rows the clamp reads, then re-borrow mutably
        let boundary = self.selection_occupancy == SelectionOccupancy::Boundary;
        let target_col = {
            let vlines = self.active_vlines();
            if target_row as usize >= vlines.vlines.len() {
                return;
            }
            if boundary {
                desired.min(vlines.vlines[target_row as usize].width_cols)
            } else {
                Self::clamp_visual_col(vlines, target_row, desired)
            }
        };
        if let Some(new_vcursor) = self.visual_to_logical_cursor(target_row, target_col) {
            self.buffer.set_cursor_internal(
                new_vcursor.logical_row,
                new_vcursor.logical_col,
                new_vcursor.offset,
            );
            self.cursor_visual_affinity = None;
            if self.selection_occupancy == SelectionOccupancy::Boundary {
                self.set_cursor_affinity_for_absolute_row(target_row);
            }
            self.ensure_cursor_visible(new_vcursor.visual_row);
            // restore desired col past the cursor-change reset
            self.desired_visual_col = Some(desired);
            self.seen_cursor_version = self.buffer.cursor_version();
        }
    }

    /// `moveDownVisual` (741-780).
    pub fn move_down_visual(&mut self) {
        self.pump_cursor_events();
        let vcursor = self.primary_visual_cursor_absolute();
        let total = self.active_vlines().vlines.len() as u32;
        if vcursor.visual_row + 1 >= total {
            return;
        }
        let target_row = vcursor.visual_row + 1;
        if self.desired_visual_col.is_none() {
            self.desired_visual_col = Some(vcursor.visual_col);
        }
        let desired = self.desired_visual_col.unwrap();
        let boundary = self.selection_occupancy == SelectionOccupancy::Boundary;
        let target_col = {
            let vlines = self.active_vlines();
            if boundary {
                desired.min(vlines.vlines[target_row as usize].width_cols)
            } else {
                Self::clamp_visual_col(vlines, target_row, desired)
            }
        };
        if let Some(new_vcursor) = self.visual_to_logical_cursor(target_row, target_col) {
            self.buffer.set_cursor_internal(
                new_vcursor.logical_row,
                new_vcursor.logical_col,
                new_vcursor.offset,
            );
            self.cursor_visual_affinity = None;
            if self.selection_occupancy == SelectionOccupancy::Boundary {
                self.set_cursor_affinity_for_absolute_row(target_row);
            }
            self.ensure_cursor_visible(new_vcursor.visual_row);
            self.desired_visual_col = Some(desired);
            self.seen_cursor_version = self.buffer.cursor_version();
        }
    }

    /// `getVisualSOL` (832-862).
    pub fn get_visual_sol(&mut self) -> VisualCursor {
        self.pump_cursor_events();
        let cursor = self.buffer.primary_cursor();
        let vcursor = self.primary_visual_cursor_absolute();
        let vlines = self.active_vlines();
        if vcursor.visual_row as usize >= vlines.vlines.len() {
            let offset = self.buffer.coords_to_offset(cursor.row, 0).unwrap_or(0);
            return VisualCursor {
                visual_row: vcursor.visual_row,
                visual_col: 0,
                logical_row: cursor.row,
                logical_col: 0,
                offset,
            };
        }
        let vline = &vlines.vlines[vcursor.visual_row as usize];
        let logical_row = vline.source_line;
        let logical_col = vline.source_col_start;
        let offset = self
            .buffer
            .coords_to_offset(logical_row, logical_col)
            .unwrap_or(0);
        VisualCursor {
            visual_row: vcursor.visual_row,
            visual_col: 0,
            logical_row,
            logical_col,
            offset,
        }
    }

    /// `getVisualEOL` (867-908) — cell occupancy targets the last cell,
    /// boundary the gap after it.
    pub fn get_visual_eol(&mut self) -> VisualCursor {
        self.pump_cursor_events();
        let vcursor = self.primary_visual_cursor_absolute();
        let occupancy = self.selection_occupancy;
        let viewport_cache = self.viewport;
        if vcursor.visual_row as usize >= self.active_vlines().vlines.len() {
            let logical = self.buffer.get_eol();
            return self.logical_to_visual_cursor(logical.row, logical.col);
        }
        // gather everything read off the vline cache into Copy locals
        // (perf lane: `active_vlines` now borrows — the buffer reads
        // below stay in their ORIGINAL order)
        let (cell_offset, width_cols, logical_row, logical_col, oversized, mut target_visual_col) = {
            let vlines = self.active_vlines();
            let idx = vcursor.visual_row as usize;
            let vline = &vlines.vlines[idx];
            let cell_offset = vlines.cell_offsets[idx];
            let viewport_width = viewport_cache.map_or(vline.width_cols, |vp| vp.width);
            let oversized = vline.width_cols > viewport_width;
            let target_visual_col = if occupancy == SelectionOccupancy::Boundary {
                vline.width_cols
            } else if oversized {
                vcursor.visual_col.min(vline.width_cols)
            } else {
                Self::clamp_visual_col(vlines, vcursor.visual_row, vline.width_cols)
            };
            (
                cell_offset,
                vline.width_cols,
                vline.source_line,
                vline.source_col_start,
                oversized,
                target_visual_col,
            )
        };
        if occupancy == SelectionOccupancy::Cell && !oversized {
            let target_offset = cell_offset + target_visual_col;
            if let Some((start, end)) = self.buffer.cursor_unit_bounds_at_offset(target_offset) {
                if start < target_offset {
                    let target = if start >= cell_offset { start } else { end };
                    target_visual_col = (target.saturating_sub(cell_offset)).min(width_cols);
                }
            }
        }
        let logical_col = logical_col + target_visual_col;
        let offset = self
            .buffer
            .coords_to_offset(logical_row, logical_col)
            .unwrap_or(0);
        VisualCursor {
            visual_row: vcursor.visual_row,
            visual_col: target_visual_col,
            logical_row,
            logical_col,
            offset,
        }
    }

    /// `gotoVisualLineEnd` (910-917): set cursor with visual affinity.
    pub fn goto_visual_line_end(&mut self) {
        self.pump_cursor_events();
        let eol = self.get_visual_eol();
        self.cursor_visual_affinity = Some(CursorAffinity {
            offset: eol.offset,
            visual_row: eol.visual_row,
            visual_col: eol.visual_col,
        });
        // setCursor clears affinity on offset mismatch — write directly
        // then keep the affinity (upstream setCursor → event → the
        // affinity set BEFORE would be invalidated; upstream sets
        // affinity AFTER setCursor returns)
        self.buffer.set_cursor(eol.logical_row, eol.logical_col);
        self.cursor_visual_affinity = Some(CursorAffinity {
            offset: eol.offset,
            visual_row: eol.visual_row,
            visual_col: eol.visual_col,
        });
        self.seen_cursor_version = self.buffer.cursor_version();
        self.desired_visual_col = None;
    }

    /// `setCursorByOffset` (810-813): buffer op + updateBeforeRender.
    pub fn set_cursor_by_offset(&mut self, offset: u32) {
        self.buffer.set_cursor_by_offset(offset);
        self.update_before_render();
    }

    // -- selection (text-buffer-view.zig 622-1222, editor-view.zig 325-417) ----

    /// `offsetSelectionRange` (tbv 626-643): clamp to text end; snap
    /// start to the grapheme start; extend end over a grapheme that
    /// starts before it.
    fn offset_selection_range(&self, start: u32, end: u32) -> (u32, u32) {
        let text_end = self.buffer.text_end_offset();
        let mut range_start = start.min(end).min(text_end);
        let mut range_end = start.max(end).min(text_end);
        if range_start == range_end {
            if let Some((s, _)) = self.buffer.grapheme_bounds_at_offset(range_start) {
                range_start = s;
            }
            return (range_start, range_start);
        }
        if let Some((s, _)) = self.buffer.grapheme_bounds_at_offset(range_start) {
            range_start = s;
        }
        if let Some((s, e)) = self.buffer.grapheme_bounds_at_offset(range_end) {
            if s < range_end {
                range_end = e;
            }
        }
        (range_start, range_end.min(text_end))
    }

    /// `selectionRange` (tbv 1081-1101): anchor/focus → exclusive range.
    fn selection_range(&self, anchor: u32, focus: u32) -> (u32, u32) {
        let text_end = self.buffer.text_end_offset();
        let mut start = anchor.min(focus);
        let mut end = anchor.max(focus);
        if anchor == focus {
            let offset = start.min(text_end);
            return (offset, offset);
        }
        if let Some((s, _)) = self.buffer.grapheme_bounds_at_offset(start) {
            start = s;
        }
        if let Some((s, e)) = self.buffer.grapheme_bounds_at_offset(end) {
            if self.selection_occupancy == SelectionOccupancy::Cell || s < end {
                end = e;
            }
        }
        (start.min(text_end), end.min(text_end))
    }

    /// `setSelection` (tbv 645-656 via editor-view.zig:329): offsets are
    /// already-exclusive; clears stored endpoints.
    pub fn set_selection(&mut self, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.pump_cursor_events();
        self.selection_endpoints = None;
        let (s, e) = self.offset_selection_range(start, end);
        self.selection = Some(TextSelection {
            start: s,
            end: e,
            bg_color: bg,
            fg_color: fg,
        });
    }

    /// `updateSelection` (tbv 658-668): keep the start, move the end.
    pub fn update_selection(&mut self, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.pump_cursor_events();
        if let Some(sel) = self.selection {
            self.selection_endpoints = None;
            let (s, e) = self.offset_selection_range(sel.start, end);
            self.selection = Some(TextSelection {
                start: s,
                end: e,
                bg_color: bg,
                fg_color: fg,
            });
        }
    }

    /// `setSelectionInclusiveStyle` (tbv 703-711): cell occupancy
    /// extends the end over the grapheme at it.
    pub fn set_selection_inclusive(
        &mut self,
        start: u32,
        end: u32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
    ) {
        self.pump_cursor_events();
        let lo = start.min(end);
        let hi = start.max(end);
        let exclusive_end = if self.selection_occupancy == SelectionOccupancy::Cell {
            self.buffer
                .grapheme_bounds_at_offset(hi)
                .map_or(hi, |(_, e)| e)
        } else {
            hi
        };
        self.set_selection(lo, exclusive_end, bg, fg);
    }

    /// `setSelectionColors` (tbv 670-675).
    pub fn set_selection_colors(&mut self, bg: Option<Rgba>, fg: Option<Rgba>) {
        if let Some(sel) = &mut self.selection {
            sel.bg_color = bg;
            sel.fg_color = fg;
        }
    }

    /// `resetSelection` / `resetLocalSelection` (tbv 677-681, 1103;
    /// lib.zig:3047-3051 also clears selection_follow_cursor).
    pub fn reset_selection(&mut self) {
        self.selection = None;
        self.selection_endpoints = None;
        self.selection_behavior = SelectionBehavior::Cell;
        self.selection_updates_cursor = false;
        self.selection_follow_cursor = false;
    }

    pub fn get_selection(&self) -> Option<(u32, u32)> {
        self.selection.map(|s| (s.start, s.end))
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// `setSelectionOccupancy` (tbv 683-694) + the editor's cursor sync
    /// (editor-view.zig:398-413).
    pub fn set_selection_occupancy(&mut self, occupancy: SelectionOccupancy) {
        self.pump_cursor_events();
        // editor-view.zig:398-413 — snapshot the affinity + local
        // selection state before the view recomputes the range
        let visual_row = self.cursor_visual_affinity.map(|a| a.visual_row);
        let had_local = self.selection_updates_cursor
            && self.selection_endpoints.is_some()
            && self.selection.is_some_and(|s| s.start != s.end);
        let cursor_offset_before = self.buffer.primary_cursor().offset;

        self.selection_occupancy = occupancy;
        if let Some(endpoints) = self.selection_endpoints {
            if self.selection.is_some() {
                let text_end = self.buffer.text_end_offset();
                let anchor = endpoints.anchor.min(text_end);
                let focus = endpoints.focus.min(text_end);
                let range = self
                    .expand_selection_range(anchor, focus, self.selection_behavior)
                    .or_else(|| Some(self.selection_range(anchor, focus)));
                if let Some((s, e)) = range {
                    if let Some(sel) = &mut self.selection {
                        sel.start = s;
                        sel.end = e;
                    }
                }
            }
        }
        if had_local {
            self.sync_cursor_to_selection_focus();
            let cursor = self.buffer.primary_cursor();
            if cursor.offset != cursor_offset_before {
                self.buffer.set_cursor(cursor.row, cursor.col);
            }
        }
        if let Some(row) = visual_row {
            self.set_cursor_affinity_for_absolute_row(row);
        }
    }

    pub fn selection_occupancy(&self) -> SelectionOccupancy {
        self.selection_occupancy
    }

    /// `expandSelectionRange` (tbv 871-891): behavior dispatch over the
    /// anchor/focus endpoints.
    fn expand_selection_range(
        &self,
        anchor: u32,
        focus: u32,
        behavior: SelectionBehavior,
    ) -> Option<(u32, u32)> {
        match behavior {
            SelectionBehavior::Cell => Some(self.selection_range(anchor, focus)),
            SelectionBehavior::Word => self.expand_word_range(anchor, focus),
            SelectionBehavior::Line => self.expand_line_range(anchor, focus),
        }
    }

    /// `expandSelectionRange` word branch (tbv 880-884): union of
    /// selectWordBetween(anchor→focus) and (focus→anchor).
    fn expand_word_range(&self, anchor: u32, focus: u32) -> Option<(u32, u32)> {
        let a = self.select_word_between(anchor, focus);
        let b = self.select_word_between(focus, anchor);
        match (a, b) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x),
            (Some(x), Some(y)) => Some((x.0.min(y.0), x.1.max(y.1))),
        }
    }

    /// `expandSelectionRange` line branch (tbv 885-889).
    fn expand_line_range(&self, anchor: u32, focus: u32) -> Option<(u32, u32)> {
        let a = self.select_line(anchor);
        let b = self.select_line(focus);
        match (a, b) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x),
            (Some(x), Some(y)) => Some((x.0.min(y.0), x.1.max(y.1))),
        }
    }

    /// `setLocalSelectionBehavior` (tbv 743-801 + editor-view.zig
    /// 353-363): local x/y → offsets via `coordsToCharOffset`, store
    /// endpoints, expand by behavior.
    pub fn set_local_selection(
        &mut self,
        anchor: (i32, i32),
        focus: (i32, i32),
        colors: (Option<Rgba>, Option<Rgba>),
        update_cursor: bool,
        follow_cursor: bool,
        behavior: SelectionBehavior,
    ) -> bool {
        let (anchor_x, anchor_y) = anchor;
        let (focus_x, focus_y) = focus;
        let (bg, fg) = colors;
        self.pump_cursor_events();
        self.selection_follow_cursor = follow_cursor;
        let anchor_above = anchor_y < 0;
        let focus_above = focus_y < 0;
        let max_y = self.active_vlines().vlines.len() as i32 - 1;
        let anchor_below = anchor_y > max_y;
        let focus_below = focus_y > max_y;

        if (anchor_above && focus_above) || (anchor_below && focus_below) {
            let had = self.selection.is_some();
            self.reset_selection();
            return had;
        }
        let text_end = self.buffer.text_end_offset();

        let anchor_offset = if anchor_below {
            Some(text_end)
        } else if anchor_above || anchor_x < 0 {
            Some(0)
        } else {
            self.coords_to_char_offset(anchor_x, anchor_y)
        };
        let anchor_offset = match anchor_offset {
            Some(o) => o,
            None => {
                let had = self.selection.is_some();
                self.reset_selection();
                return had;
            }
        };
        let focus_offset = if focus_below {
            Some(text_end)
        } else if focus_above || focus_x < 0 {
            Some(0)
        } else {
            self.coords_to_char_offset(focus_x, focus_y)
        };
        let focus_offset = match focus_offset {
            Some(o) => o,
            None => {
                let had = self.selection.is_some();
                self.reset_selection();
                return had;
            }
        };

        self.selection_endpoints = Some(SelectionEndpoints {
            anchor: anchor_offset,
            focus: focus_offset,
        });
        self.selection_behavior = behavior;

        let _ = text_end;
        let range = self
            .expand_selection_range(anchor_offset, focus_offset, behavior)
            .or_else(|| Some(self.selection_range(anchor_offset, focus_offset)));
        let Some((s, e)) = range else {
            return false;
        };
        // always store, even zero-width, to preserve the anchor (792)
        let new_selection = TextSelection {
            start: s,
            end: e,
            bg_color: bg,
            fg_color: fg,
        };
        let changed = match &self.selection {
            Some(old) => old.start != new_selection.start || old.end != new_selection.end,
            None => true,
        };
        self.selection = Some(new_selection);
        self.selection_updates_cursor = update_cursor;
        if update_cursor && self.selection_endpoints.is_some() {
            self.sync_cursor_to_selection_focus();
            // editor-view.zig:359
            self.set_cursor_affinity_for_local_row(focus_y);
        }
        changed
    }

    /// `updateLocalSelectionStyle` (tbv 812-818 + editor-view.zig
    /// 369-384): focus-only update when endpoints exist.
    pub fn update_local_selection(
        &mut self,
        anchor: (i32, i32),
        focus: (i32, i32),
        colors: (Option<Rgba>, Option<Rgba>),
        update_cursor: bool,
        follow_cursor: bool,
        behavior: SelectionBehavior,
    ) -> bool {
        let (anchor_x, anchor_y) = anchor;
        let (focus_x, focus_y) = focus;
        let (bg, fg) = colors;
        self.pump_cursor_events();
        self.selection_follow_cursor = follow_cursor;
        if self.selection_endpoints.is_some() {
            self.update_local_selection_focus_only(focus_x, focus_y, bg, fg, behavior);
            if update_cursor && self.selection_endpoints.is_some() {
                self.selection_updates_cursor = true;
                self.sync_cursor_to_selection_focus();
                self.set_cursor_affinity_for_local_row(focus_y);
            }
            true
        } else {
            self.set_local_selection(
                (anchor_x, anchor_y),
                (focus_x, focus_y),
                (bg, fg),
                update_cursor,
                follow_cursor,
                behavior,
            )
        }
    }

    /// `updateLocalSelectionFocusOnly` (tbv 820-849).
    fn update_local_selection_focus_only(
        &mut self,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) {
        let Some(endpoints) = self.selection_endpoints else {
            return;
        };
        let anchor_offset = endpoints.anchor;

        let focus_above = focus_y < 0;
        let max_y = self.active_vlines().vlines.len() as i32 - 1;
        let focus_below = focus_y > max_y;
        let text_end = self.buffer.text_end_offset();

        let focus_offset = if focus_below {
            Some(text_end)
        } else if focus_above || focus_x < 0 {
            Some(0)
        } else {
            self.coords_to_char_offset(focus_x, focus_y)
        };
        let Some(focus_offset) = focus_offset else {
            return;
        };

        self.selection_endpoints = Some(SelectionEndpoints {
            anchor: anchor_offset,
            focus: focus_offset,
        });
        self.selection_behavior = behavior;
        let _ = text_end;
        let Some((s, e)) = self.expand_selection_range(anchor_offset, focus_offset, behavior)
        else {
            return;
        };
        self.selection = Some(TextSelection {
            start: s,
            end: e,
            bg_color: bg,
            fg_color: fg,
        });
    }

    /// `convertSelectionToCell` (tbv 855-869 + editor-view.zig:386-391).
    pub fn convert_selection_to_cell(&mut self) -> bool {
        self.pump_cursor_events();
        let Some(sel) = self.selection else {
            return false;
        };
        if sel.start == sel.end {
            return false;
        }
        let focus = if self.selection_occupancy == SelectionOccupancy::Boundary {
            sel.end
        } else {
            self.buffer
                .grapheme_bounds_at_offset(sel.end - 1)
                .map_or(sel.end - 1, |(s, _)| s)
        };
        self.selection_endpoints = Some(SelectionEndpoints {
            anchor: sel.start,
            focus,
        });
        self.selection_behavior = SelectionBehavior::Cell;
        self.selection_updates_cursor = true;
        self.sync_cursor_to_selection_focus();
        true
    }

    /// `syncCursorToSelectionFocus` (editor-view.zig:421-458).
    fn sync_cursor_to_selection_focus(&mut self) {
        let Some(sel) = self.selection else { return };
        let mut focus_offset = sel.end;
        if let Some(endpoints) = self.selection_endpoints {
            focus_offset = endpoints.focus;
            if self.selection_occupancy == SelectionOccupancy::Boundary
                && focus_offset < endpoints.anchor
            {
                focus_offset = sel.start;
            } else if self.selection_occupancy == SelectionOccupancy::Boundary
                && focus_offset > endpoints.anchor
            {
                focus_offset = sel.end;
            } else if let Some((start, _)) = self.buffer.cursor_unit_bounds_at_offset(focus_offset)
            {
                focus_offset = start;
            }
        }
        let Some((row, col)) = self.buffer.offset_to_coords(focus_offset) else {
            return;
        };
        let line_count = self.buffer.line_count();
        if row >= line_count {
            return;
        }
        let line_width = self.buffer.line_width(row);
        if col > line_width {
            return;
        }
        self.buffer.set_cursor_internal(row, col, focus_offset);
        self.seen_cursor_version = self.buffer.cursor_version();
    }

    /// `setCursorAffinityForLocalRow` (editor-view.zig:581-588).
    fn set_cursor_affinity_for_local_row(&mut self, visual_row: i32) {
        let line_count = self.active_vlines().vlines.len() as i64;
        if line_count == 0 {
            return;
        }
        let viewport_y = self.viewport.map_or(0i64, |vp| i64::from(vp.offset_y));
        let absolute_row = (i64::from(visual_row) + viewport_y).clamp(0, line_count - 1);
        self.set_cursor_affinity_for_absolute_row(absolute_row as u32);
    }

    /// `coordsToCharOffset` (tbv 1138-1202): local viewport x/y → the
    /// offset of the grapheme under the cell (cell occupancy clamps to
    /// the last cell of a wrapped line).
    fn coords_to_char_offset(&mut self, x: i32, y: i32) -> Option<u32> {
        let vp = self.viewport?;
        let y_offset = vp.offset_y as i32;
        let x_offset = if self.wrap_mode == WrapMode::None {
            vp.offset_x as i32
        } else {
            0
        };
        let occupancy = self.selection_occupancy;
        let vlines = self.active_vlines();
        if vlines.vlines.is_empty() {
            return Some(0);
        }
        let abs_y = y + y_offset;
        let abs_x = x + x_offset;
        let clamped_y = abs_y.clamp(0, vlines.vlines.len() as i32 - 1);
        let vline_idx = clamped_y as usize;
        let vline = &vlines.vlines[vline_idx];
        let line_start = vlines.cell_offsets[vline_idx];

        // cell occupancy clamps wrap padding to the last cell (1124-1136)
        let max_local_x = if vline.width_cols == 0 {
            0
        } else if occupancy == SelectionOccupancy::Cell
            && vline_idx + 1 < vlines.vlines.len()
            && vlines.vlines[vline_idx + 1].source_line == vline.source_line
        {
            vline.width_cols - 1
        } else {
            vline.width_cols
        };

        // undo the draw-time alignment offset
        let align_pad = alignment_pad_cols(TextAlign::Left, vp.width, vline.width_cols) as i32;
        let aligned_abs_x = abs_x - align_pad;
        let local_x = aligned_abs_x.clamp(0, max_local_x as i32) as u32;

        Some(line_start + local_x)
    }

    // -- word/line selection (tbv 902-988) ----------------------------------

    /// `selectWord` (tbv 903-926): the word around an offset; `None` on
    /// newline/end. `/` is not a boundary.
    fn select_word(&self, offset: u32) -> Option<(u32, u32)> {
        let grapheme = self.selection_grapheme_at(offset)?;
        if grapheme.is_break {
            return None;
        }
        let expect_boundary = is_word_boundary(grapheme.first_cp);
        let mut start = grapheme.start;
        let mut end = grapheme.end;
        while let Some(prev) = self.prev_selection_grapheme(start) {
            if prev.is_break
                || is_word_boundary(prev.first_cp) != expect_boundary
                || prev.start >= start
            {
                break;
            }
            start = prev.start;
        }
        while let Some(next) = self.selection_grapheme_at(end) {
            if next.is_break
                || is_word_boundary(next.first_cp) != expect_boundary
                || next.end <= end
            {
                break;
            }
            end = next.end;
        }
        Some((start, end))
    }

    /// `selectWordBetween` (tbv 929-951).
    fn select_word_between(&self, from: u32, toward: u32) -> Option<(u32, u32)> {
        let text_end = self.buffer.text_end_offset();
        let mut offset = from.min(text_end);
        let limit = toward.min(text_end);
        if offset <= limit {
            while offset <= limit {
                if let Some(range) = self.select_word(offset) {
                    return Some(range);
                }
                let next = self.selection_grapheme_at(offset)?;
                if next.end <= offset {
                    return None;
                }
                offset = next.end;
            }
            None
        } else {
            loop {
                if let Some(range) = self.select_word(offset) {
                    return Some(range);
                }
                if offset <= limit {
                    return None;
                }
                let prev = self.prev_selection_grapheme(offset)?;
                if prev.start >= offset {
                    return None;
                }
                offset = prev.start;
            }
        }
    }

    /// `selectLine` (tbv 955-957) + `trimLineWhitespace` (968-988).
    fn select_line(&self, offset: u32) -> Option<(u32, u32)> {
        let range = self.source_line_range_at(offset)?;
        // trim ASCII space/tab edges; all-whitespace keeps the line
        let mut start = range.0;
        let mut end = range.1;
        let mut saw_content = false;
        let mut walk = range.0;
        while walk < range.1 {
            let Some(g) = self.selection_grapheme_at(walk) else {
                break;
            };
            if g.is_break {
                break;
            }
            if g.first_cp != ' ' as u32 && g.first_cp != '\t' as u32 {
                if !saw_content {
                    start = g.start;
                }
                end = g.end;
                saw_content = true;
            }
            if g.end <= walk {
                break;
            }
            walk = g.end;
        }
        if !saw_content {
            return Some(range);
        }
        Some((start, end))
    }

    /// `sourceLineRangeAt` (tbv 959-966).
    fn source_line_range_at(&self, offset: u32) -> Option<(u32, u32)> {
        let text_end = self.buffer.text_end_offset();
        if offset > text_end {
            return None;
        }
        let (row, col) = self.buffer.offset_to_coords(offset)?;
        let start = offset - col;
        Some((start, start + self.buffer.line_width(row)))
    }

    /// `selectionGraphemeAt` (tbv 997-1065) — the grapheme at an offset;
    /// line ends report a synthetic `\n` break grapheme.
    fn selection_grapheme_at(&self, offset: u32) -> Option<SelectionGrapheme> {
        let text_end = self.buffer.text_end_offset();
        if offset >= text_end {
            return None;
        }
        let (row, col) = self.buffer.offset_to_coords(offset)?;
        let line = self.buffer.line_at(row)?;
        let line_start = offset - col;
        let line_width = line.width_cols;
        if col >= line_width {
            // the newline cell
            return Some(SelectionGrapheme {
                start: line_start + line_width,
                end: line_start + line_width + 1,
                first_cp: '\n' as u32,
                is_break: true,
            });
        }
        // find the cluster containing col
        let mut cols = 0u32;
        for c in &line.clusters {
            let end_col = cols + c.width;
            if col < end_col {
                let bytes = &line.text[c.byte_start as usize..c.byte_end() as usize];
                let first_cp = bytes.chars().next().map_or(0xFFFD, |ch| ch as u32);
                let start = line_start + cols;
                return Some(SelectionGrapheme {
                    start,
                    end: start + c.width,
                    first_cp,
                    is_break: first_cp == '\n' as u32 || first_cp == '\r' as u32,
                });
            }
            cols = end_col;
        }
        None
    }

    /// `prevSelectionGrapheme` (tbv 1067-1070).
    fn prev_selection_grapheme(&self, offset: u32) -> Option<SelectionGrapheme> {
        if offset == 0 {
            return None;
        }
        self.selection_grapheme_at(offset - 1)
    }

    /// `getSelectedTextIntoBuffer` (tbv 1219-1223).
    pub fn get_selected_text(&self) -> String {
        match self.selection {
            Some(sel) => self.buffer.get_text_range(sel.start, sel.end),
            None => String::new(),
        }
    }

    /// `deleteSelectedText` (editor-view.zig:782-808).
    pub fn delete_selected_text(&mut self) {
        self.pump_cursor_events();
        let Some(sel) = self.selection else { return };
        let Some((sr, sc)) = self.buffer.offset_to_coords(sel.start) else {
            return;
        };
        let Some((er, ec)) = self.buffer.offset_to_coords(sel.end) else {
            return;
        };
        self.buffer.delete_range(sr, sc, er, ec);
        self.reset_selection();
        self.update_before_render();
    }

    // -- placeholder (editor-view.zig 919-981) --------------------------------

    /// `setPlaceholderStyledText` (923-962).
    pub fn set_placeholder_styled_text(&mut self, chunks: &[TextChunk]) {
        self.pump_cursor_events();
        // the placeholder content version keys the placeholder vline
        // cache (switchToBuffer marks the view dirty, tbv 721-725)
        self.placeholder_content_version += 1;
        if chunks.is_empty() {
            self.placeholder = None;
            if self.placeholder_active {
                self.placeholder_active = false;
            }
            return;
        }
        // placeholder buffer: unset defaults (white fg / transparent bg
        // at the raster); the chunks carry their own colors
        self.placeholder = Some(Placeholder {
            doc: TextDocument::from_styled(chunks, TextDefaults::default()),
        });
        self.update_placeholder_visibility();
    }

    /// `shouldShowPlaceholder` (964-967): rope total weight 0.
    fn should_show_placeholder(&self) -> bool {
        self.buffer.text_end_offset() == 0 && self.placeholder.is_some()
    }

    /// `updatePlaceholderVisibility` (969-981).
    fn update_placeholder_visibility(&mut self) {
        let should = self.should_show_placeholder();
        if should && !self.placeholder_active {
            if self.placeholder.is_some() {
                self.placeholder_active = true;
            }
        } else if !should && self.placeholder_active {
            self.placeholder_active = false;
        }
    }

    pub fn placeholder_active(&self) -> bool {
        self.placeholder_active
    }

    // -- draw bridge ------------------------------------------------------------

    /// The active document (placeholder-aware) for rendering. The
    /// placeholder buffer keeps the raster defaults (unset
    /// `text_buffer.defaults()`); the edit buffer carries the
    /// renderable's colors.
    pub fn active_document(&self) -> TextDocument {
        if self.placeholder_active {
            self.placeholder
                .as_ref()
                .map_or_else(TextDocument::default, |p| {
                    let mut doc = p.doc.clone();
                    doc.defaults = TextDefaults::default();
                    doc
                })
        } else {
            self.buffer.document()
        }
    }

    pub fn selection(&self) -> Option<TextSelection> {
        self.selection
    }

    /// Build the draw snapshot: update-before-render (placeholder +
    /// cursor visibility), then the active document + vlines.
    pub fn draw_state(&mut self) -> (TextDocument, EditorVlines) {
        self.update_before_render();
        let doc = self.active_document();
        let vlines = self.active_vlines().clone();
        (doc, vlines)
    }

    /// Viewport for the draw snapshot.
    pub fn draw_viewport(&self) -> Viewport {
        self.get_viewport().to_viewport()
    }
}

/// `SelectionGrapheme` (tbv 990-995).
struct SelectionGrapheme {
    start: u32,
    end: u32,
    first_cp: u32,
    is_break: bool,
}

/// `drawEditorView` → the EditorView branch of `drawTextBufferInternal`
/// (buffer.zig:1652-2087): viewport background prefill (1671-1679),
/// per-cluster style resolution, per-cell selection highlight
/// (1861-1977), tab indicator cells (2007-2034), INVERSE swap after
/// selection (1990-1994).
pub fn draw_editor_view(buffer: &mut OptimizedBuffer, view: &EditorDrawState, x: i32, y: i32) {
    // viewport bg prefill: only when the (edit buffer's) default bg is
    // opaque — fillRectClipped over the viewport rect (1671-1679)
    if let Some(bg) = view.prefill_bg {
        if bg.alpha() > 0 {
            buffer.fill_rect(x, y, view.viewport.width, view.viewport.height, bg);
        }
    }

    let vlines = &view.vlines;
    if vlines.is_empty() {
        return;
    }
    // getVirtualLines: [vp.y, vp.y + vp.height) clamped (tbv 501-505)
    let start = (view.viewport.y as usize).min(vlines.len());
    let end = (start + view.viewport.height as usize).min(vlines.len());
    let vl = &vlines[start..end];
    if vl.is_empty() {
        return;
    }
    let h_off = view.viewport.x;
    let vp_w = view.viewport.width;
    let defaults = view.defaults;

    let first_visible: usize = if y < 0 { (-y) as usize } else { 0 };
    let bottom = buffer.height as i32;
    let last_possible: usize = if y >= bottom {
        0
    } else if y < 0 {
        vl.len().min(first_visible + bottom as usize)
    } else {
        vl.len().min((bottom - y) as usize)
    };
    if first_visible >= vl.len() || last_possible == 0 || first_visible >= last_possible {
        return;
    }

    for (slice_idx, vline) in vl[first_visible..last_possible].iter().enumerate() {
        let current_y = y + first_visible as i32 + slice_idx as i32;
        if current_y >= bottom {
            break;
        }
        // absolute vline index (for alignment + cell offsets)
        let vline_idx = view.viewport.y as usize + first_visible + slice_idx;
        let document_cell_offset = view.cell_offsets[vline_idx];

        // per-vline alignment (1716-1717) — editor views are left-aligned
        // (EditorView exposes no align API)
        let align_pad = alignment_pad_cols(TextAlign::Left, vp_w, vline.width_cols) as i32;
        let mut current_x = x + align_pad;
        let mut rendered_col: u32 = 0;

        let line = &view.doc.lines[vline.source_line as usize];
        let spans = &line.spans;

        // initial span state (1725-1756)
        let mut line_fg = defaults.fg;
        let mut line_bg = defaults.bg;
        let mut line_attributes = defaults.attributes;
        let start_col = vline.source_col_start + h_off;
        let mut span_idx = 0usize;
        while span_idx < spans.len() && spans[span_idx].col_end <= start_col {
            span_idx += 1;
        }
        let mut next_change_col = if span_idx < spans.len() {
            spans[span_idx].col_end
        } else {
            u32::MAX
        };
        if span_idx < spans.len() && spans[span_idx].col_start <= start_col {
            let span = &spans[span_idx];
            if let Some(fg) = span.fg {
                line_fg = fg;
            }
            if let Some(bg) = span.bg {
                line_bg = bg;
            }
            line_attributes |= span.attributes;
        }

        for vchunk in &vline.slices {
            if current_x >= buffer.width as i32 {
                current_x += vchunk.width_cols as i32;
                continue;
            }
            let slice_end = vchunk.byte_start + vchunk.byte_len;
            let clusters = line
                .clusters
                .iter()
                .filter(|c| c.byte_start >= vchunk.byte_start && c.byte_end() <= slice_end);

            for cluster in clusters {
                let w = cluster.width;
                let is_tab = cluster.is_tab(&line.text);

                if rendered_col < h_off {
                    rendered_col += w;
                    continue;
                }
                if rendered_col >= h_off + vp_w {
                    break;
                }
                if current_x + w as i32 <= 0 {
                    current_x += w as i32;
                    rendered_col += w;
                    continue;
                }
                if current_x >= buffer.width as i32 {
                    break;
                }
                if w > 1
                    && !is_tab
                    && (rendered_col + w > h_off + vp_w
                        || current_x < 0
                        || current_x + w as i32 > buffer.width as i32)
                {
                    current_x += w as i32;
                    rendered_col += w;
                    continue;
                }

                // style resolution at the source column (1876-1910)
                let source_col = vline.source_col_start + rendered_col;
                while source_col >= next_change_col && span_idx + 1 < spans.len() {
                    span_idx += 1;
                    let span = &spans[span_idx];
                    line_fg = defaults.fg;
                    line_bg = defaults.bg;
                    line_attributes = defaults.attributes;
                    if let Some(fg) = span.fg {
                        line_fg = fg;
                    }
                    if let Some(bg) = span.bg {
                        line_bg = bg;
                    }
                    line_attributes |= span.attributes;
                    next_change_col = span.col_end;
                }

                if w == 0 {
                    continue;
                }

                // selection styling per cell (1960-1977): a cluster is
                // styled when ANY of its cells is selected
                let mut final_fg = line_fg;
                let mut final_bg = line_bg;
                if let Some(sel) = view.selection {
                    let sel_offset = document_cell_offset + rendered_col;
                    let mut cell_idx = 0u32;
                    while cell_idx < w {
                        let is_selected =
                            sel_offset + cell_idx >= sel.start && sel_offset + cell_idx < sel.end;
                        if is_selected {
                            if let Some(sel_bg) = sel.bg_color {
                                final_bg = sel_bg;
                                if let Some(sel_fg) = sel.fg_color {
                                    final_fg = sel_fg;
                                }
                            } else {
                                // swap fg/bg (1970-1973): opaque line bg
                                // becomes fg, else opaque black
                                let temp = line_fg;
                                final_fg = if line_bg.alpha() > 0 {
                                    line_bg
                                } else {
                                    Rgba::from_ints(0, 0, 0, 255)
                                };
                                final_bg = temp;
                            }
                            break;
                        }
                        cell_idx += 1;
                    }
                }

                let draw_attributes = line_attributes;
                let mut draw_fg = final_fg;
                let mut draw_bg = final_bg;
                // INVERSE swaps fg/bg AFTER selection (1990-1994)
                if draw_attributes & crate::ansi::text_attributes::INVERSE_U32 != 0 {
                    core::mem::swap(&mut draw_fg, &mut draw_bg);
                }

                if is_tab {
                    let mut tab_col: u32 = 0;
                    while tab_col < w {
                        if rendered_col + tab_col >= h_off + vp_w {
                            break;
                        }
                        let tab_x = current_x + tab_col as i32;
                        if tab_x < 0 {
                            tab_col += 1;
                            continue;
                        }
                        if tab_x >= buffer.width as i32 {
                            break;
                        }
                        let ch = if tab_col == 0 {
                            view.tab_indicator
                                .unwrap_or(crate::buffer::DEFAULT_SPACE_CHAR)
                        } else {
                            crate::buffer::DEFAULT_SPACE_CHAR
                        };
                        let fg = if tab_col == 0 {
                            view.tab_indicator_color.unwrap_or(draw_fg)
                        } else {
                            draw_fg
                        };
                        buffer.set_cell_with_alpha_blending(
                            tab_x as u32,
                            current_y as u32,
                            crate::buffer::Cell {
                                char: ch,
                                fg,
                                bg: draw_bg,
                                attributes: draw_attributes,
                            },
                        );
                        tab_col += 1;
                    }
                } else {
                    let bytes =
                        &line.text[cluster.byte_start as usize..cluster.byte_end() as usize];
                    let encoded = if bytes.len() == 1 && w == 1 && bytes.as_bytes()[0] >= 32 {
                        u32::from(bytes.as_bytes()[0])
                    } else {
                        let gid = buffer.alloc_grapheme_pub(bytes);
                        crate::buffer::pack_grapheme_start(gid, w)
                    };
                    buffer.set_cell_with_alpha_blending(
                        current_x as u32,
                        current_y as u32,
                        crate::buffer::Cell {
                            char: encoded,
                            fg: draw_fg,
                            bg: draw_bg,
                            attributes: draw_attributes,
                        },
                    );
                }

                current_x += w as i32;
                rendered_col += w;
            }
        }
    }
}

/// `default_word_boundaries` (tbv 65-83) — the `isWordBoundary`
/// codepoint set (note: `/` and `.` are NOT boundaries).
fn is_word_boundary(cp: u32) -> bool {
    const BOUNDARIES: &[u32] = &[
        0x20, 0x09, 0x27, 0x22, 0x2502, 0x60, 0x7C, 0x3A, 0x3B, 0x2C, 0x28, 0x29, 0x5B, 0x5D, 0x7B,
        0x7D, 0x3C, 0x3E, 0x24,
    ];
    BOUNDARIES.contains(&cp)
}

/// Everything `drawEditorView` needs — the EditorView's draw-side state
/// snapshot (the upstream draw takes the view pointer and reads its
/// fields; the port passes an owned snapshot so the renderable can hold
/// the view mutably while drawing).
pub struct EditorDrawState<'a> {
    pub doc: &'a TextDocument,
    pub vlines: &'a [VirtualLine],
    pub cell_offsets: &'a [u32],
    pub viewport: Viewport,
    pub selection: Option<TextSelection>,
    pub defaults: TextDefaults,
    /// the EDIT buffer's default bg (opaque ⇒ viewport prefill) —
    /// buffer.zig:1671-1679 reads `edit_buffer.getTextBuffer().defaults()`
    pub prefill_bg: Option<Rgba>,
    pub tab_indicator: Option<u32>,
    pub tab_indicator_color: Option<Rgba>,
}

use unicode_segmentation::UnicodeSegmentation;
