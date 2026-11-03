//! `TextBufferView` — port of `packages/core/src/text-buffer-view.ts`
//! (the full wrapper surface, :12-235).
//!
//! Upstream is a thin handle over the native Zig view
//! (`lib.createTextBufferView`); this port owns the same layout +
//! selection state directly over the pure-Rust primitives
//! (`TextDocument` = the buffer, `compute_virtual_lines`/`apply_truncation`
//! = the native view's layout pass, the selection state machine from
//! text-buffer-view.zig:622-1222 as already sliced into `editor.rs`).
//! mechanism ≠ upstream (no native handle), output == upstream at the
//! contract level.
//!
//! Intentional omissions (no native pointer in the port):
//! * `ptr` getter (text-buffer-view.ts:33-36) — the handle itself;
//! * async/`RenderLib` plumbing.
//!
//! `firstLineOffset` (text-buffer-view.ts:199) is stored and pinned by
//! the API surface; its raster effect (skipping the first N columns of
//! the first virtual line) has no consumer in the headless profile and
//! is deferred — noted in PORT-MATRIX.

use crate::color::Rgba;
use crate::editor::{SelectionBehavior, SelectionOccupancy};
use crate::renderables::text::WrapMode;
use crate::text_buffer::{
    apply_truncation, compute_virtual_lines, tab_indicator_codepoint, TextAlign, TextDocument,
    Viewport, VirtualLine,
};

/// The error thrown by every method after `destroy()` — upstream's
/// "Fail loud and clear" `guard()` (text-buffer-view.ts:24-27).
pub const DESTROYED_MESSAGE: &str = "TextBufferView is destroyed";

/// `LineInfo` (types.ts:214-225) — the visual-line geometry snapshot
/// `lineInfo`/`logicalLineInfo` return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInfo {
    /// Display-column offset for each visual line start.
    pub line_start_cols: Vec<u32>,
    /// Display-column width for each visual line.
    pub line_width_cols: Vec<u32>,
    /// Maximum display-column width across the reported lines.
    pub line_width_cols_max: u32,
    /// Source logical line index for each visual line.
    pub line_sources: Vec<u32>,
    /// Wrap index within each source logical line.
    pub line_wraps: Vec<u32>,
}

/// `MeasureResult` (zig-structs.ts:178-181).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasureResult {
    pub line_count: u32,
    pub width_cols_max: u32,
}

/// The selection state — upstream stores it inside the native view;
/// here it is the `TextSelection` triple plus the
/// selection-clear bookkeeping from text-buffer-view.ts:16-19.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectionState {
    start: u32,
    end: u32,
    bg: Option<Rgba>,
    fg: Option<Rgba>,
}

/// `TextBufferView` (text-buffer-view.ts:12) — a layout/selection view
/// over a text buffer. Created with [`TextBufferView::create`]; every
/// method panics with [`DESTROYED_MESSAGE`] once [`destroy`] ran.
pub struct TextBufferView {
    document: TextDocument,
    destroyed: bool,
    /// Only this wrapper sets the view's selection and a reset clears
    /// all of it (text-buffer-view.ts:16-19): while `true` a reset can
    /// skip the (here: state) call.
    selection_clear: bool,
    selection: Option<SelectionState>,
    selection_occupancy: SelectionOccupancy,
    // layout configuration (setters below, one per upstream method)
    wrap_width: Option<u32>,
    wrap_mode: WrapMode,
    text_align: TextAlign,
    first_line_offset: u32,
    viewport: Option<Viewport>,
    tab_indicator: Option<u32>,
    tab_indicator_color: Option<Rgba>,
    truncate: bool,
    // cached virtual lines (invalidated by layout setters)
    vlines: Option<Vec<VirtualLine>>,
}

impl std::fmt::Debug for TextBufferView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextBufferView")
            .field("destroyed", &self.destroyed)
            .field("wrap_mode", &self.wrap_mode)
            .field("wrap_width", &self.wrap_width)
            .field("text_align", &self.text_align)
            .field("viewport", &self.viewport)
            .field("truncate", &self.truncate)
            .finish_non_exhaustive()
    }
}

impl TextBufferView {
    /// `TextBufferView.create(textBuffer)` (text-buffer-view.ts:22-27) —
    /// takes ownership of the buffer (`&TextBuffer` upstream is a live
    /// handle; `TextDocument` is the port's value-shaped buffer).
    #[must_use]
    pub fn create(document: TextDocument) -> TextBufferView {
        TextBufferView {
            document,
            destroyed: false,
            selection_clear: true,
            selection: None,
            selection_occupancy: SelectionOccupancy::Cell,
            wrap_width: None,
            wrap_mode: WrapMode::Word,
            text_align: TextAlign::Left,
            first_line_offset: 0,
            viewport: None,
            tab_indicator: None,
            tab_indicator_color: None,
            truncate: false,
            vlines: None,
        }
    }

    /// `guard()` (text-buffer-view.ts:24-27) — fail loud and clear.
    fn guard(&self) {
        if self.destroyed {
            panic!("{DESTROYED_MESSAGE}");
        }
    }

    /// `destroy()` (text-buffer-view.ts:231-235) — idempotent.
    pub fn destroy(&mut self) {
        self.destroyed = true;
    }

    /// `isDestroyed` — not an upstream getter on the view itself, but
    /// the ref's renderables guard with it (`input.isDestroyed`); the
    /// port pins the observable.
    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    // ------------------------------------------------------------------
    // Absolute-offset selection — text-buffer-view.ts:39-66
    // ------------------------------------------------------------------

    /// `setSelection(start, end, bgColor?, fgColor?)`
    /// (text-buffer-view.ts:39-46).
    pub fn set_selection(&mut self, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.guard();
        self.selection_clear = false;
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.selection = Some(SelectionState { start, end, bg, fg });
    }

    /// `updateSelection(end, ...)` (text-buffer-view.ts:47-54) —
    /// keeps the anchor, moves the focus.
    pub fn update_selection(&mut self, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.guard();
        self.selection_clear = false;
        self.selection = Some(match self.selection {
            Some(sel) => {
                let (start, end) = if sel.start <= end {
                    (sel.start, end)
                } else {
                    (end, sel.start)
                };
                SelectionState {
                    start,
                    end,
                    bg: bg.or(sel.bg),
                    fg: fg.or(sel.fg),
                }
            }
            None => SelectionState {
                start: 0,
                end,
                bg,
                fg,
            },
        });
    }

    /// `resetSelection()` (text-buffer-view.ts:55-61) — skipped when
    /// already clear (the `selectionClear` fast path, :16-19).
    pub fn reset_selection(&mut self) {
        self.guard();
        if self.selection_clear {
            return;
        }
        self.selection = None;
        self.selection_clear = true;
    }

    /// `getSelection()` (text-buffer-view.ts:62-65).
    #[must_use]
    pub fn get_selection(&self) -> Option<(u32, u32)> {
        self.guard();
        self.selection.map(|s| (s.start, s.end))
    }

    /// `hasSelection()` (text-buffer-view.ts:66-68).
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.guard();
        self.selection.is_some()
    }

    // ------------------------------------------------------------------
    // Local (cell-coordinate) selection — text-buffer-view.ts:70-121
    // ------------------------------------------------------------------

    /// `setLocalSelection(anchorX, anchorY, focusX, focusY, ...,
    /// behavior)` (text-buffer-view.ts:70-95): maps viewport cells to
    /// buffer offsets, orders them, and — for `Word`/`Line` —
    /// expands the endpoints (`SelectionBehavior`,
    /// text-buffer-view.zig:46-51). Returns whether a non-empty
    /// selection resulted.
    pub fn set_local_selection(
        &mut self,
        anchor_x: u32,
        anchor_y: u32,
        focus_x: u32,
        focus_y: u32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> bool {
        self.guard();
        self.selection_clear = false;
        let (start, end) = self.expand_local_range(anchor_x, anchor_y, focus_x, focus_y, behavior);
        if start == end {
            self.selection = None;
            self.selection_clear = true;
            return false;
        }
        self.selection = Some(SelectionState { start, end, bg, fg });
        true
    }

    /// `updateLocalSelection(...)` (text-buffer-view.ts:96-114) — keeps
    /// the anchor cell, moves the focus cell.
    pub fn update_local_selection(
        &mut self,
        _anchor_x: u32,
        _anchor_y: u32,
        focus_x: u32,
        focus_y: u32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> bool {
        self.guard();
        // The anchor cell is not retained upstream either — the view
        // keeps buffer offsets, so update recomputes from the stored
        // selection start (offset semantics, :106-113).
        self.selection_clear = false;
        let anchor_offset = self.selection.map_or(0, |s| s.start);
        let (ax, ay) = self.offset_to_cell(anchor_offset);
        let (start, end) = self.expand_local_range(ax, ay, focus_x, focus_y, behavior);
        if start == end {
            self.selection = None;
            self.selection_clear = true;
            return false;
        }
        self.selection = Some(SelectionState { start, end, bg, fg });
        true
    }

    /// `resetLocalSelection()` (text-buffer-view.ts:115-121).
    pub fn reset_local_selection(&mut self) {
        self.guard();
        if self.selection_clear {
            return;
        }
        self.selection = None;
        self.selection_clear = true;
    }

    /// Endpoint expansion for the local-selection setters
    /// (`expandLocalSelection`, text-buffer-view.zig:622-1222 subset):
    /// cell→offset mapping via the virtual lines, then
    /// behavior-gated expansion to word/line boundaries.
    fn expand_local_range(
        &mut self,
        anchor_x: u32,
        anchor_y: u32,
        focus_x: u32,
        focus_y: u32,
        behavior: SelectionBehavior,
    ) -> (u32, u32) {
        let a = self.cell_to_offset(anchor_x, anchor_y);
        let f = self.cell_to_offset(focus_x, focus_y);
        let (lo, hi) = if a <= f { (a, f) } else { (f, a) };
        let lo = match behavior {
            SelectionBehavior::Cell => lo,
            SelectionBehavior::Word => self.expand_word(lo, hi, true),
            SelectionBehavior::Line => self.expand_line(lo, hi, true),
        };
        let hi = match behavior {
            SelectionBehavior::Cell => hi,
            SelectionBehavior::Word => self.expand_word(lo, hi, false),
            SelectionBehavior::Line => self.expand_line(lo, hi, false),
        };
        (lo, hi)
    }

    fn expand_word(&self, lo: u32, hi: u32, lower: bool) -> u32 {
        let text = self.plain_text_bytes();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if lower {
            // walk back to the start of the word containing `lo`
            let mut o = (lo as usize).min(text.len());
            loop {
                if o == 0 {
                    return 0;
                }
                let mut back = o - 1;
                while back > 0 && !text.is_char_boundary(back) {
                    back -= 1;
                }
                match text[back..].chars().next() {
                    Some(c) if is_word(c) => o = back,
                    _ => return o as u32,
                }
            }
        } else {
            // walk forward to the end of the word containing `hi`
            let mut o = (hi as usize).min(text.len());
            while let Some(c) = text[o..].chars().next() {
                if is_word(c) {
                    o += c.len_utf8();
                } else {
                    break;
                }
            }
            o as u32
        }
    }

    fn expand_line(&self, lo: u32, hi: u32, lower: bool) -> u32 {
        let text = self.plain_text_bytes();
        if lower {
            let byte = (lo as usize).min(text.len());
            let mut start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
            while start < byte && !text.is_char_boundary(start) {
                start += 1;
            }
            start as u32
        } else {
            let byte = (hi as usize).min(text.len());
            let end = text[byte..].find('\n').map_or(text.len(), |i| byte + i);
            end as u32
        }
    }

    /// Map a (column, vline-row) cell to a document byte offset:
    /// the vline's slices carry (col → byte) per source line.
    fn cell_to_offset(&mut self, x: u32, y: u32) -> u32 {
        let vlines = self.virtual_lines().to_vec();
        let Some(vline) = vlines.get(y as usize) else {
            return self.total_bytes();
        };
        let col = x.min(vline.width_cols);
        let line = &self.document.lines[vline.source_line as usize];
        let base = self.line_start_offset(vline.source_line);
        // walk the slices for the byte at `col` past the vline start col
        let mut cursor = vline.source_col_start;
        for slice in &vline.slices {
            if slice.is_ellipsis {
                continue;
            }
            if col < cursor + slice.width_cols {
                // columns walk clusters from the slice start byte
                let within = col - cursor;
                let mut cols = 0u32;
                for cluster in &line.clusters {
                    if cluster.byte_end() <= slice.byte_start {
                        continue;
                    }
                    if cols >= within {
                        return base + cluster.byte_start;
                    }
                    cols += cluster.width;
                }
                return base + line.text.len() as u32;
            }
            cursor += slice.width_cols;
        }
        base + line.text.len() as u32
    }

    /// Inverse of [`cell_to_offset`] (used by `updateLocalSelection`
    /// to recover the anchor cell).
    fn offset_to_cell(&mut self, offset: u32) -> (u32, u32) {
        let vlines = self.virtual_lines().to_vec();
        let mut y = 0;
        for vline in vlines.iter() {
            let base = self.line_start_offset(vline.source_line);
            let end_base = base + self.document.lines[vline.source_line as usize].text.len() as u32;
            if offset <= end_base {
                // find the column: walk clusters within [base, offset)
                let line = &self.document.lines[vline.source_line as usize];
                let within = offset.saturating_sub(base).min(line.text.len() as u32);
                let mut cols = vline.source_col_start;
                for cluster in &line.clusters {
                    if cluster.byte_end() <= within {
                        cols += cluster.width;
                    } else {
                        break;
                    }
                }
                return (cols, y);
            }
            y += 1;
        }
        (0, y.saturating_sub(1))
    }

    fn line_start_offset(&self, line: u32) -> u32 {
        self.document.lines[..line as usize]
            .iter()
            .map(|l| l.text.len() as u32 + 1)
            .sum()
    }

    fn total_bytes(&self) -> u32 {
        self.document
            .lines
            .iter()
            .map(|l| l.text.len() as u32 + 1)
            .sum::<u32>()
            .saturating_sub(1)
    }

    fn plain_text_bytes(&self) -> String {
        self.document
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ------------------------------------------------------------------
    // Occupancy — text-buffer-view.ts:122-129
    // ------------------------------------------------------------------

    /// `setSelectionOccupancy(occupancy)` (text-buffer-view.ts:122-125).
    pub fn set_selection_occupancy(&mut self, occupancy: SelectionOccupancy) {
        self.guard();
        self.selection_occupancy = occupancy;
    }

    /// `getSelectionOccupancy()` (text-buffer-view.ts:126-129).
    #[must_use]
    pub fn selection_occupancy(&self) -> SelectionOccupancy {
        self.guard();
        self.selection_occupancy
    }

    // ------------------------------------------------------------------
    // Layout configuration — text-buffer-view.ts:131-171
    // ------------------------------------------------------------------

    /// `setWrapWidth(width | null)` (text-buffer-view.ts:131-134).
    pub fn set_wrap_width(&mut self, width: Option<u32>) {
        self.guard();
        self.wrap_width = width;
        self.vlines = None;
    }

    /// `setWrapMode("none"|"char"|"word")` (text-buffer-view.ts:135-138).
    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        self.guard();
        self.wrap_mode = mode;
        self.vlines = None;
    }

    /// `setTextAlign("left"|"center"|"right")` (text-buffer-view.ts:139-142).
    pub fn set_text_align(&mut self, alignment: TextAlign) {
        self.guard();
        self.text_align = alignment;
    }

    /// `setFirstLineOffset(offset)` (text-buffer-view.ts:199-202) —
    /// stored; the raster skip of the first `offset` columns of the
    /// first virtual line is deferred (no headless consumer).
    pub fn set_first_line_offset(&mut self, offset: u32) {
        self.guard();
        self.first_line_offset = offset;
        self.vlines = None;
    }

    /// `setViewportSize(width, height)` (text-buffer-view.ts:144-147).
    pub fn set_viewport_size(&mut self, width: u32, height: u32) {
        self.guard();
        let prev = self.viewport.unwrap_or(Viewport {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
        self.viewport = Some(Viewport {
            x: prev.x,
            y: prev.y,
            width,
            height,
        });
        self.vlines = None;
    }

    /// `setViewport(x, y, width, height)` (text-buffer-view.ts:148-151).
    pub fn set_viewport(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.guard();
        self.viewport = Some(Viewport {
            x,
            y,
            width,
            height,
        });
        self.vlines = None;
    }

    // ------------------------------------------------------------------
    // Line info — text-buffer-view.ts:153-172
    // ------------------------------------------------------------------

    /// `lineInfo` (text-buffer-view.ts:153-156) — the VISUAL-line
    /// geometry (types.ts:214-225).
    pub fn line_info(&mut self) -> LineInfo {
        self.guard();
        let vlines = self.virtual_lines();
        let mut info = LineInfo::default();
        let mut wrap_counters: Vec<u32> = Vec::new();
        for vline in vlines {
            let idx = vline.source_line as usize;
            if idx >= wrap_counters.len() {
                wrap_counters.resize(idx + 1, 0);
            }
            info.line_start_cols.push(vline.source_col_start);
            info.line_width_cols.push(vline.width_cols);
            info.line_width_cols_max = info.line_width_cols_max.max(vline.width_cols);
            info.line_sources.push(vline.source_line);
            info.line_wraps.push(wrap_counters[idx]);
            wrap_counters[idx] += 1;
        }
        info
    }

    /// `logicalLineInfo` (text-buffer-view.ts:158-161) — one entry per
    /// LOGICAL line, unwrapped.
    pub fn logical_line_info(&self) -> LineInfo {
        self.guard();
        let mut info = LineInfo::default();
        for (i, line) in self.document.lines.iter().enumerate() {
            info.line_start_cols.push(0);
            info.line_width_cols.push(line.width_cols);
            info.line_width_cols_max = info.line_width_cols_max.max(line.width_cols);
            info.line_sources.push(i as u32);
            info.line_wraps.push(0);
        }
        info
    }

    /// `getLineSources(startLine, lineCount)` (text-buffer-view.ts:162-165)
    /// — bounded visual-row → source-line read.
    pub fn get_line_sources(&mut self, start_line: u32, line_count: u32) -> Vec<u32> {
        self.guard();
        let vlines = self.virtual_lines();
        let start = (start_line as usize).min(vlines.len());
        let end = (start + line_count as usize).min(vlines.len());
        vlines[start..end].iter().map(|v| v.source_line).collect()
    }

    // ------------------------------------------------------------------
    // Text extraction — text-buffer-view.ts:174-197
    // ------------------------------------------------------------------

    /// `getSelectedText()` (text-buffer-view.ts:174-186).
    #[must_use]
    pub fn get_selected_text(&self) -> String {
        self.guard();
        let Some(sel) = self.selection else {
            return String::new();
        };
        let text = self.plain_text_bytes();
        let start = (sel.start as usize).min(text.len());
        let end = (sel.end as usize).min(text.len());
        if start >= end {
            return String::new();
        }
        // keep char boundaries
        let mut s = start;
        let mut e = end;
        while s < text.len() && !text.is_char_boundary(s) {
            s += 1;
        }
        while e < text.len() && !text.is_char_boundary(e) {
            e += 1;
        }
        text[s..e].to_string()
    }

    /// `getPlainText()` (text-buffer-view.ts:188-197).
    #[must_use]
    pub fn get_plain_text(&self) -> String {
        self.guard();
        self.plain_text_bytes()
    }

    // ------------------------------------------------------------------
    // Tab indicator / truncate — text-buffer-view.ts:204-216
    // ------------------------------------------------------------------

    /// `setTabIndicator(indicator: string | number)`
    /// (text-buffer-view.ts:204-207) — the string's first codepoint.
    pub fn set_tab_indicator(&mut self, indicator: Option<&str>, numeric: Option<u32>) {
        self.guard();
        self.tab_indicator = tab_indicator_codepoint(indicator, numeric);
    }

    /// `setTabIndicatorColor(color)` (text-buffer-view.ts:209-211).
    pub fn set_tab_indicator_color(&mut self, color: Option<Rgba>) {
        self.guard();
        self.tab_indicator_color = color;
    }

    /// `setTruncate(truncate)` (text-buffer-view.ts:213-216) — gates
    /// `applyTruncation` on the layout pass.
    pub fn set_truncate(&mut self, truncate: bool) {
        self.guard();
        self.truncate = truncate;
        self.vlines = None;
    }

    // ------------------------------------------------------------------
    // Measure / counts — text-buffer-view.ts:218-229
    // ------------------------------------------------------------------

    /// `measureForDimensions(width, height)` (text-buffer-view.ts:218-225):
    /// the wrapped line count + max width at `width` — the
    /// autosize-measure contract (`MeasureResult`, zig-structs.ts:178).
    /// `Some` unless the width is degenerate (0 → `null` upstream).
    pub fn measure_for_dimensions(&mut self, width: u32, height: u32) -> Option<MeasureResult> {
        self.guard();
        if width == 0 || height == 0 {
            return None;
        }
        let mut vlines = compute_virtual_lines(&self.document, self.wrap_mode, Some(width));
        if self.truncate {
            apply_truncation(&self.document, &mut vlines, width);
        }
        let width_cols_max = vlines.iter().map(|v| v.width_cols).max().unwrap_or(0);
        Some(MeasureResult {
            line_count: vlines.len() as u32,
            width_cols_max,
        })
    }

    /// `getVirtualLineCount()` (text-buffer-view.ts:227-229).
    pub fn get_virtual_line_count(&mut self) -> u32 {
        self.guard();
        self.virtual_lines().len() as u32
    }

    // ------------------------------------------------------------------
    // layout cache
    // ------------------------------------------------------------------

    /// The view's layout pass: `updateVirtualLines`
    /// (text-buffer-view.zig:448-485) + viewport-gated
    /// `applyTruncation` (:1280-1445).
    fn virtual_lines(&mut self) -> &[VirtualLine] {
        if self.vlines.is_none() {
            let mut vlines = compute_virtual_lines(&self.document, self.wrap_mode, self.wrap_width);
            if self.truncate {
                let vp_width = self.viewport.map_or(0, |vp| vp.width);
                apply_truncation(&self.document, &mut vlines, vp_width);
            }
            self.vlines = Some(vlines);
        }
        self.vlines.as_deref().unwrap()
    }
}

// NOTE: `LogicalLine` import is used in docstring references only via
// `TextDocument`; keep imports minimal.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styled_text::TextChunk;
    use crate::text_buffer::{TextAlign, TextDocument};

    fn doc(text: &str) -> TextDocument {
        TextDocument::from_styled(
            &[TextChunk::new(text)],
            crate::text_buffer::TextDefaults::default(),
        )
    }

    #[test]
    fn create_destroy_lifecycle() {
        let mut view = TextBufferView::create(doc("hello"));
        assert!(!view.is_destroyed());
        view.set_wrap_mode(WrapMode::None);
        view.destroy();
        view.destroy(); // idempotent
        assert!(view.is_destroyed());
    }

    #[test]
    #[should_panic(expected = "TextBufferView is destroyed")]
    fn guard_fails_loud_after_destroy() {
        let mut view = TextBufferView::create(doc("hello"));
        view.destroy();
        let _ = view.get_plain_text();
    }

    #[test]
    fn absolute_selection_lifecycle() {
        let mut view = TextBufferView::create(doc("hello world"));
        assert!(!view.has_selection());
        assert_eq!(view.get_selection(), None);

        view.set_selection(0, 5, None, None);
        assert!(view.has_selection());
        assert_eq!(view.get_selection(), Some((0, 5)));
        assert_eq!(view.get_selected_text(), "hello");

        // updateSelection keeps the anchor, orders the range
        view.update_selection(3, None, None);
        assert_eq!(view.get_selection(), Some((0, 3)));

        // reset twice: the second is the selectionClear fast path
        view.reset_selection();
        view.reset_selection();
        assert!(!view.has_selection());
        assert_eq!(view.get_selected_text(), "");
    }

    #[test]
    fn local_selection_maps_cells_to_offsets() {
        let mut view = TextBufferView::create(doc("alpha\nbeta"));
        view.set_wrap_mode(WrapMode::None);
        assert!(view.set_local_selection(0, 0, 4, 0, None, None, SelectionBehavior::Cell));
        assert_eq!(view.get_selection(), Some((0, 4)));
        assert_eq!(view.get_selected_text(), "alph");

        // second row starts after "alpha\n" → offset 6
        assert!(view.set_local_selection(0, 1, 1, 1, None, None, SelectionBehavior::Cell));
        assert_eq!(view.get_selected_text(), "b");

        view.reset_local_selection();
        view.reset_local_selection(); // fast path
        assert!(!view.has_selection());
    }

    #[test]
    fn local_selection_word_behavior_expands() {
        let mut view = TextBufferView::create(doc("foo bar baz"));
        view.set_wrap_mode(WrapMode::None);
        assert!(view.set_local_selection(4, 0, 5, 0, None, None, SelectionBehavior::Word));
        // "bar" expands to the whole word
        assert_eq!(view.get_selected_text(), "bar");
    }

    #[test]
    fn line_info_wrap_and_logical() {
        let mut view = TextBufferView::create(doc("ab cd\nxyz"));
        view.set_wrap_mode(WrapMode::None);
        let info = view.line_info();
        assert_eq!(info.line_sources, vec![0, 1]);
        assert_eq!(info.line_wraps, vec![0, 0]);
        assert_eq!(info.line_width_cols_max, 5);

        view.set_wrap_mode(WrapMode::Char);
        view.set_wrap_width(Some(3));
        let info = view.line_info();
        // "ab cd" wraps at 3 cols → 2 vlines; "xyz" → 1
        assert_eq!(info.line_sources, vec![0, 0, 1]);
        assert_eq!(info.line_wraps, vec![0, 1, 0]);

        let logical = view.logical_line_info();
        assert_eq!(logical.line_sources, vec![0, 1]);
        assert_eq!(logical.line_width_cols, vec![5, 3]);
    }

    #[test]
    fn get_line_sources_bounded() {
        let mut view = TextBufferView::create(doc("one\ntwo\nthree"));
        view.set_wrap_mode(WrapMode::None);
        assert_eq!(view.get_line_sources(0, 2), vec![0, 1]);
        assert_eq!(view.get_line_sources(5, 4), Vec::<u32>::new());
    }

    #[test]
    fn occupancy_roundtrip() {
        let mut view = TextBufferView::create(doc("x"));
        assert_eq!(view.selection_occupancy(), SelectionOccupancy::Cell);
        view.set_selection_occupancy(SelectionOccupancy::Boundary);
        assert_eq!(view.selection_occupancy(), SelectionOccupancy::Boundary);
    }

    #[test]
    fn tab_indicator_first_codepoint() {
        let mut view = TextBufferView::create(doc("a\tb"));
        view.set_tab_indicator(Some("→"), None);
        view.set_tab_indicator(None, Some(0x2192));
        view.set_tab_indicator_color(Some(crate::color::Rgba::rgb(1, 2, 3)));
    }

    #[test]
    fn measure_and_virtual_line_count() {
        let mut view = TextBufferView::create(doc("ab\ncd"));
        view.set_wrap_mode(WrapMode::None);
        assert_eq!(view.get_virtual_line_count(), 2);
        let m = view.measure_for_dimensions(10, 10).expect("measure");
        assert_eq!((m.line_count, m.width_cols_max), (2, 2));
        assert!(view.measure_for_dimensions(0, 10).is_none());
    }

    #[test]
    fn viewport_setters_and_plain_text() {
        let mut view = TextBufferView::create(doc("line"));
        view.set_viewport_size(4, 2);
        view.set_viewport(1, 0, 3, 1);
        view.set_first_line_offset(1);
        view.set_text_align(TextAlign::Center);
        assert_eq!(view.get_plain_text(), "line");
    }
}
