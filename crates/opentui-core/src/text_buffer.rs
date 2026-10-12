//! The styled text document + its virtual-line layout + the text-buffer
//! raster — ports of `packages/native/src/text-buffer.zig` (the styled
//! document), `text-buffer-view.zig` (wrap/align virtual lines) and the
//! `drawTextBufferInternal` loop from `buffer.zig:1652-2087`.
//!
//! Pipeline (mirrors upstream exactly):
//!
//! 1. [`TextDocument::from_styled`] — `setStyledText`
//!    (text-buffer.zig:1157-1269): chunk texts concatenate into lines;
//!    each chunk's style becomes a per-line column *span* (the native
//!    "highlight" ranges measured with a width cursor, 1220-1266).
//! 2. [`compute_virtual_lines`] — `calculateUnwrappedVirtualLines`
//!    (mode none), `calculateVirtualLinesGeneric(.char)` and
//!    `(.word)` (text-buffer-view.zig:1602-2405): logical lines →
//!    virtual lines with per-line slice windows.
//! 3. [`draw_text_view`] — `drawTextBufferInternal`
//!    (buffer.zig:1652-2087): per-vline alignment pad
//!    (`alignmentPadCols`, text-buffer-view.zig:224-231), per-cluster
//!    style resolution against the spans, INVERSE fg/bg swap
//!    (buffer.zig:1990-1994), tab indicator cells (2007-2034).
//!
//! Subset deltas (documented): selection, truncation (`truncate: true`),
//! scissor rects and scroll offsets ≠ 0 are later lanes; opacity is
//! always 1.0 in the headless profile. Styled chunks model as **one
//! native segment per logical line** (upstream re-segments the rope by
//! lines after `setStyledText`; the chunk *styles* live in the spans,
//! not the segments) — the cross-chunk grapheme/CJK probes in the word
//! walker (text-buffer-view.zig:2221-2249) therefore never fire.

use crate::ansi::text_attributes;
use crate::buffer::{pack_grapheme_start, OptimizedBuffer};
use crate::color::Rgba;
use crate::renderables::text::WrapMode;
use crate::styled_text::TextChunk;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Default tab width — `tab_width = 2` (text-buffer.zig:291). A tab
/// occupies exactly this many columns (fixed width, not tab stops).
pub const TAB_WIDTH: u32 = 2;

/// `TextAlign` — text-buffer-view.zig:215-219.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    #[must_use]
    pub fn parse(s: &str) -> TextAlign {
        match s {
            "center" => TextAlign::Center,
            "right" => TextAlign::Right,
            _ => TextAlign::Left,
        }
    }
}

/// `alignmentPadCols` — text-buffer-view.zig:224-231: left padding (in
/// columns) to align a line of `line_width_cols` within the viewport.
#[must_use]
pub fn alignment_pad_cols(alignment: TextAlign, viewport_width: u32, line_width_cols: u32) -> u32 {
    if line_width_cols >= viewport_width {
        return 0;
    }
    match alignment {
        TextAlign::Left => 0,
        TextAlign::Center => (viewport_width - line_width_cols) / 2,
        TextAlign::Right => viewport_width - line_width_cols,
    }
}

// --- clusters ------------------------------------------------------------

/// One grapheme cluster: byte window (line-relative) + display width
/// (`findRenderClusterInfo`, utf8.zig:1892-2103). Width may be 0
/// (zero-width clusters render nothing and advance no columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    pub byte_start: u32,
    pub byte_len: u32,
    pub width: u32,
}

impl Cluster {
    #[must_use]
    pub fn byte_end(&self) -> u32 {
        self.byte_start + self.byte_len
    }

    pub fn is_tab(&self, text: &str) -> bool {
        self.byte_len == 1 && text.as_bytes()[self.byte_start as usize] == b'\t'
    }
}

/// Display width of one grapheme cluster — `GraphemeWidthState`
/// (utf8.zig:598-660): first codepoint's width (first non-zero if it
/// had none), VS16 upgrades 1→2, regional-indicator pairs add. Tabs are
/// `TAB_WIDTH`; combining marks are 0. `unicode-width` implements the
/// same tables.
#[must_use]
pub fn cluster_width(cluster: &str) -> u32 {
    if cluster == "\t" {
        return TAB_WIDTH;
    }
    UnicodeWidthStr::width(cluster) as u32
}

/// Cluster list of a text (byte offsets relative to it).
#[must_use]
pub fn text_clusters(text: &str) -> Vec<Cluster> {
    let mut out = Vec::new();
    let mut byte = 0u32;
    for g in text.graphemes(true) {
        let len = g.len() as u32;
        out.push(Cluster {
            byte_start: byte,
            byte_len: len,
            width: cluster_width(g),
        });
        byte += len;
    }
    out
}

fn text_width(text: &str) -> u32 {
    text_clusters(text).iter().map(|c| c.width).sum()
}

/// `findWrapPosByWidthGraphemeSafe` (utf8.zig:783-794 → the Unicode
/// walk 797-913 with `handleClusterForWrap` 704-723): greedy cluster
/// fill — stop before the first cluster whose inclusion would exceed
/// `max_columns`. Returns (bytes_used, grapheme_count, columns_used).
#[must_use]
pub fn wrap_pos_by_width(clusters: &[Cluster], max_columns: u32) -> (u32, u32, u32) {
    let mut cols = 0u32;
    let mut bytes = 0u32;
    let mut count = 0u32;
    for c in clusters {
        if cols + c.width > max_columns {
            break;
        }
        cols += c.width;
        bytes += c.byte_len;
        count += 1;
    }
    (bytes, count, cols)
}

/// `findGraphemePosByWidth(…, include_start_before = true)`
/// (utf8.zig:998-1008 → `findPosByWidthUnicode` with the include-start
/// branch of `handleClusterForPos`, 735-763): include every cluster
/// that *starts* before `max_columns` — a wide cluster at the boundary
/// snaps forward.
#[must_use]
pub fn grapheme_pos_by_width(clusters: &[Cluster], max_columns: u32) -> (u32, u32, u32) {
    let mut cols = 0u32;
    let mut bytes = 0u32;
    let mut count = 0u32;
    for c in clusters {
        if cols >= max_columns {
            break;
        }
        cols += c.width;
        bytes += c.byte_len;
        count += 1;
    }
    (bytes, count, cols)
}

// --- the styled document -------------------------------------------------

/// Per-chunk style applied over a column range — the materialized form
/// of `registerStyleDefinition` + the per-line highlight spans
/// (text-buffer.zig:1249-1265), consumed at buffer.zig:1744-1756 and
/// 1887-1910: span colors override the text defaults, span attributes
/// OR onto them. `None` color = "keep the default".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleSpan {
    pub col_start: u32,
    pub col_end: u32,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
}

/// One logical line: bytes + precomputed clusters + style spans.
#[derive(Debug, Clone, Default)]
pub struct LogicalLine {
    pub text: String,
    pub clusters: Vec<Cluster>,
    /// display width (`lineWidthAt`)
    pub width_cols: u32,
    pub spans: Vec<StyleSpan>,
}

impl LogicalLine {
    fn build(text: String, spans: Vec<StyleSpan>) -> Self {
        let clusters = text_clusters(&text);
        let width_cols = clusters.iter().map(|c| c.width).sum();
        LogicalLine {
            text,
            clusters,
            width_cols,
            spans,
        }
    }
}

/// `text_buffer.defaults()` — the fg/bg/attributes every cell starts
/// from (`setDefaultFg/Bg/Attributes`, TextBufferRenderable.ts:91-93).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextDefaults {
    pub fg: Rgba,
    pub bg: Rgba,
    pub attributes: u32,
}

impl Default for TextDefaults {
    /// The raster's fallback defaults (buffer.zig:1725-1727): fg
    /// literal white, bg transparent black, no attributes.
    fn default() -> Self {
        TextDefaults {
            fg: Rgba::rgb(255, 255, 255),
            bg: Rgba::from_ints(0, 0, 0, 0),
            attributes: 0,
        }
    }
}

/// The styled document — what `TextBuffer.setStyledText` produces.
#[derive(Debug, Clone, Default)]
pub struct TextDocument {
    pub lines: Vec<LogicalLine>,
    pub defaults: TextDefaults,
}

impl TextDocument {
    /// `setStyledText` (text-buffer.zig:1157-1269):
    /// * chunk texts concatenate; `\n` splits lines (a trailing `\n`
    ///   creates a final empty line — `walkLinesAndSegments`,
    ///   text-buffer-iterators.zig:80-98);
    /// * an all-empty document yields **no** lines (`total_len == 0` →
    ///   `clear()`, 1173-1177);
    /// * per chunk, per line: a span over the chunk's columns on that
    ///   line — columns measured with the width cursor (tabs expand,
    ///   newlines count 0), zero-width ranges dropped
    ///   (`char_end > char_pos`, 1228).
    #[must_use]
    pub fn from_styled(chunks: &[TextChunk], defaults: TextDefaults) -> Self {
        let total_len: usize = chunks.iter().map(|c| c.text.len()).sum();
        if total_len == 0 {
            return TextDocument {
                lines: Vec::new(),
                defaults,
            };
        }

        let mut lines: Vec<LogicalLine> = Vec::new();
        let mut text = String::new();
        let mut col = 0u32;
        let mut spans: Vec<StyleSpan> = Vec::new();

        let close_line = |text: &mut String,
                          col: &mut u32,
                          spans: &mut Vec<StyleSpan>,
                          lines: &mut Vec<LogicalLine>| {
            lines.push(LogicalLine::build(
                std::mem::take(text),
                std::mem::take(spans),
            ));
            *col = 0;
        };

        for chunk in chunks {
            let pieces = chunk.text.split('\n');
            let mut is_first = true;
            for piece in pieces {
                if !is_first {
                    // the '\n' before this piece ended the current line
                    close_line(&mut text, &mut col, &mut spans, &mut lines);
                }
                is_first = false;
                let start_col = col;
                let piece_width = text_width(piece);
                if !piece.is_empty() {
                    text.push_str(piece);
                }
                col += piece_width;
                if col > start_col {
                    spans.push(StyleSpan {
                        col_start: start_col,
                        col_end: col,
                        fg: chunk.fg,
                        bg: chunk.bg,
                        attributes: chunk.attributes,
                    });
                }
            }
        }
        close_line(&mut text, &mut col, &mut spans, &mut lines);

        TextDocument { lines, defaults }
    }

    /// `measureForDimensions` (text-buffer-view.zig:1450-1510):
    /// width 0 or mode none → intrinsic (max line width, line count);
    /// otherwise the wrapped virtual lines. Callers clamp ≥ 1
    /// (native-renderable.zig:72-73).
    #[must_use]
    pub fn measure(&self, wrap_mode: WrapMode, width: u32) -> (u32, u32) {
        if width == 0 || wrap_mode == WrapMode::None {
            let w = self.lines.iter().map(|l| l.width_cols).max().unwrap_or(0);
            (w, self.lines.len() as u32)
        } else {
            let vlines = compute_virtual_lines(self, wrap_mode, Some(width));
            let w = vlines.iter().map(|v| v.width_cols).max().unwrap_or(0);
            (w, vlines.len() as u32)
        }
    }
}

// --- virtual lines ---------------------------------------------------------

/// A contiguous slice of a logical line rendered on one virtual line —
/// `VirtualChunk` (text-buffer-view.zig:108-120). Columns are
/// line-relative (`col_start_in_chunk`); byte offsets index the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VlineSlice {
    pub byte_start: u32,
    pub byte_len: u32,
    pub col_start: u32,
    pub width_cols: u32,
}

/// One rendered (virtual) line — `VirtualLine`
/// (text-buffer-view.zig:104-118).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VirtualLine {
    pub source_line: u32,
    /// column of the logical line where this vline starts
    pub source_col_start: u32,
    pub width_cols: u32,
    pub slices: Vec<VlineSlice>,
}

/// `updateVirtualLines` (text-buffer-view.zig:448-485): dispatch by
/// wrap mode. `wrap_width == None` (or mode none) → unwrapped.
#[must_use]
pub fn compute_virtual_lines(
    doc: &TextDocument,
    wrap_mode: WrapMode,
    wrap_width: Option<u32>,
) -> Vec<VirtualLine> {
    if wrap_mode == WrapMode::None || wrap_width.is_none() {
        // calculateUnwrappedVirtualLines (text-buffer-view.zig:1514-1599):
        // one vline per logical line, whole-line slice.
        return doc
            .lines
            .iter()
            .enumerate()
            .map(|(i, line)| VirtualLine {
                source_line: i as u32,
                source_col_start: 0,
                width_cols: line.width_cols,
                slices: if line.text.is_empty() {
                    Vec::new()
                } else {
                    vec![VlineSlice {
                        byte_start: 0,
                        byte_len: line.text.len() as u32,
                        col_start: 0,
                        width_cols: line.width_cols,
                    }]
                },
            })
            .collect();
    }
    let wrap_w = wrap_width.unwrap_or(0);
    let mut out = Vec::new();
    for (i, line) in doc.lines.iter().enumerate() {
        let mark = out.len();
        match wrap_mode {
            WrapMode::Char => char_mode_line(line, wrap_w, &mut out),
            WrapMode::Word => word_mode_line(line, wrap_w, &mut out),
            WrapMode::None => unreachable!(),
        }
        for v in &mut out[mark..] {
            v.source_line = i as u32;
        }
    }
    out
}

/// Current-vline accumulator — the slice/width parts of `WrapContext`
/// (text-buffer-view.zig:1614-1643).
#[derive(Default)]
struct CurrentLine {
    slices: Vec<VlineSlice>,
    width_cols: u32,
}

impl CurrentLine {
    /// `addVirtualChunk` (1690-1719): render+word mode merges
    /// contiguous slices of the same chunk (1693-1707).
    fn add_chunk(
        &mut self,
        byte_start: u32,
        byte_len: u32,
        col_start: u32,
        width_cols: u32,
        merge: bool,
    ) {
        if byte_len == 0 {
            return;
        }
        if merge && width_cols > 0 {
            if let Some(last) = self.slices.last_mut() {
                if last.byte_start + last.byte_len == byte_start
                    && last.col_start + last.width_cols == col_start
                {
                    last.byte_len += byte_len;
                    last.width_cols += width_cols;
                    self.width_cols += width_cols;
                    return;
                }
            }
        }
        self.slices.push(VlineSlice {
            byte_start,
            byte_len,
            col_start,
            width_cols,
        });
        self.width_cols += width_cols;
    }
}

/// `commitVirtualLine` (1653-1670) + `recordVirtualLine` (1672-1688).
fn commit_line(cur: &mut CurrentLine, source_col_start: &mut u32, out: &mut Vec<VirtualLine>) {
    let width = cur.width_cols;
    out.push(VirtualLine {
        source_line: 0, // stamped by the caller
        source_col_start: *source_col_start,
        width_cols: width,
        slices: std::mem::take(&mut cur.slices),
    });
    cur.width_cols = 0;
    // the source offset advances by the committed width (1663)
    *source_col_start += width;
}

/// `processCharChunk` (text-buffer-view.zig:2133-2198) — the whole
/// logical line is one native chunk in this port; then the
/// `line_end_callback` tail record (2304-2319).
fn char_mode_line(line: &LogicalLine, wrap_w: u32, out: &mut Vec<VirtualLine>) {
    let mut cur = CurrentLine::default();
    let mut source_col_start = 0u32;
    let mut byte_off = 0usize;
    let mut col_off = 0u32;

    while col_off < line.width_cols {
        let remaining = wrap_w.saturating_sub(cur.width_cols);

        if remaining == 0 {
            if cur.width_cols > 0 {
                commit_line(&mut cur, &mut source_col_start, out);
                continue;
            }
            // wrap width smaller than one grapheme: force-place it
            let clusters = clusters_from(line, byte_off);
            let (bytes, count, cols) = grapheme_pos_by_width(&clusters, 1);
            if count > 0 {
                cur.add_chunk(byte_off as u32, bytes, col_off, cols, false);
                col_off += cols;
                byte_off += bytes as usize;
            } else {
                break;
            }
            continue;
        }

        let clusters = clusters_from(line, byte_off);
        let (bytes, count, cols) = wrap_pos_by_width(&clusters, remaining);
        if count == 0 {
            if cur.width_cols > 0 {
                commit_line(&mut cur, &mut source_col_start, out);
                continue;
            }
            let (fbytes, fcount, fcols) = grapheme_pos_by_width(&clusters, 1);
            if fcount > 0 {
                cur.add_chunk(byte_off as u32, fbytes, col_off, fcols, false);
                col_off += fcols;
                byte_off += fbytes as usize;
                if col_off < line.width_cols {
                    commit_line(&mut cur, &mut source_col_start, out);
                    continue;
                }
            }
            break;
        }

        cur.add_chunk(byte_off as u32, bytes, col_off, cols, false);
        col_off += cols;
        byte_off += bytes as usize;

        if cur.width_cols >= wrap_w && col_off < line.width_cols {
            commit_line(&mut cur, &mut source_col_start, out);
        }
    }

    // line_end tail (2304-2319): record the trailing partial vline; a
    // zero-width line records an empty vline so blank lines stay visible.
    if !cur.slices.is_empty() || line.width_cols == 0 {
        out.push(VirtualLine {
            source_line: 0,
            source_col_start,
            width_cols: cur.width_cols,
            slices: std::mem::take(&mut cur.slices),
        });
    }
}

fn clusters_from(line: &LogicalLine, byte_off: usize) -> Vec<Cluster> {
    line.clusters
        .iter()
        .filter(|c| c.byte_start as usize >= byte_off)
        .copied()
        .collect()
}

// --- word mode: the layout-break scan (utf8.zig) ---------------------------

/// `LayoutWrapBreakKind` — utf8.zig:108-124.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakKind {
    None,
    Whitespace,
    PreservedWhitespace,
    Punctuation,
    ScriptTransition,
    CjkIntercharacter,
}

/// `LayoutWrapBreak` — utf8.zig:131-145. The window identifies the
/// grapheme that *creates* the break; consumers cross the boundary
/// with col_end/byte_end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutWrapBreak {
    pub byte_start: u32,
    pub col_start: u32,
    pub byte_len: u32,
    pub width_cols: u32,
    pub kind: BreakKind,
}

impl LayoutWrapBreak {
    #[must_use]
    pub fn col_end(&self) -> u32 {
        self.col_start + self.width_cols
    }
    #[must_use]
    pub fn byte_end(&self) -> u32 {
        self.byte_start + self.byte_len
    }
}

/// `WordClass` — utf8.zig:223-227.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordClass {
    AsciiWord,
    CjkWord,
    Other,
}

/// `isAsciiWordByte` — utf8.zig:229-234.
fn is_ascii_word_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
}

/// `isCjkWordCodepoint` — utf8.zig:236-260.
fn is_cjk_word_codepoint(cp: u32) -> bool {
    (0x3400..=0x4DBF).contains(&cp)
        || (0x4E00..=0x9FFF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0x20000..=0x2A6DF).contains(&cp)
        || (0x2A700..=0x2B73F).contains(&cp)
        || (0x2B740..=0x2B81F).contains(&cp)
        || (0x2B820..=0x2CEAF).contains(&cp)
        || (0x2CEB0..=0x2EBEF).contains(&cp)
        || (0x2EBF0..=0x2EE5D).contains(&cp)
        || (0x2F800..=0x2FA1F).contains(&cp)
        // Hiragana + Katakana
        || (0x3040..=0x309F).contains(&cp)
        || (0x30A0..=0x30FF).contains(&cp)
        || (0x31F0..=0x31FF).contains(&cp)
        || (0xFF66..=0xFF9D).contains(&cp)
        // Hangul
        || (0x1100..=0x11FF).contains(&cp)
        || (0x3130..=0x318F).contains(&cp)
        || (0xA960..=0xA97F).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
        || (0xD7B0..=0xD7FF).contains(&cp)
}

/// `classifyWordClass` — utf8.zig:262-268.
fn classify_word_class(cp: u32) -> WordClass {
    if cp <= 0x7F {
        return if is_ascii_word_byte(cp as u8) {
            WordClass::AsciiWord
        } else {
            WordClass::Other
        };
    }
    if is_cjk_word_codepoint(cp) {
        WordClass::CjkWord
    } else {
        WordClass::Other
    }
}

/// `isCjkIntercharacterBreak` — utf8.zig:277-281.
fn is_cjk_intercharacter_break(prev: WordClass, curr: WordClass, curr_cp: u32) -> bool {
    prev == WordClass::CjkWord
        && curr == WordClass::CjkWord
        && curr_cp != 0x3099
        && curr_cp != 0x309A
        && curr_cp != 0x30A0
        && curr_cp != 0x30FB
}

/// `isCjkAsciiTransition` — utf8.zig:283-286.
fn is_cjk_ascii_transition(prev: WordClass, curr: WordClass) -> bool {
    (prev == WordClass::CjkWord && curr == WordClass::AsciiWord)
        || (prev == WordClass::AsciiWord && curr == WordClass::CjkWord)
}

/// `asciiLayoutWrapBreakKind` — utf8.zig:159-165.
fn ascii_break_kind(cp: u32) -> BreakKind {
    match u8::try_from(cp) {
        Ok(b' ') | Ok(b'\t') => BreakKind::Whitespace,
        Ok(b'-') | Ok(b'/') | Ok(b'\\') | Ok(b'.') | Ok(b',') | Ok(b';') | Ok(b':') | Ok(b'!')
        | Ok(b'?') | Ok(b'(') | Ok(b')') | Ok(b'[') | Ok(b']') | Ok(b'{') | Ok(b'}') => {
            BreakKind::Punctuation
        }
        _ => BreakKind::None,
    }
}

/// `unicodeLayoutWrapBreakKind` — utf8.zig:197-218.
fn unicode_break_kind(cp: u32) -> BreakKind {
    match cp {
        0x00A0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 | 0x200B => {
            BreakKind::Whitespace
        }
        0x00AD | 0x2010 | 0x3001 | 0x3002 | 0xFF01 | 0xFF0C | 0xFF1A | 0xFF1F => {
            BreakKind::Punctuation
        }
        _ => BreakKind::None,
    }
}

fn cp_break_kind(cp: u32) -> BreakKind {
    if cp <= 0x7F {
        ascii_break_kind(cp)
    } else {
        unicode_break_kind(cp)
    }
}

/// Per-cluster scan state — `walkChunkLayoutInfoGeneric`
/// (utf8.zig:1762-1888): the cluster's own break kind (first cp sets
/// it, later cps merge whitespace kinds per 1858-1865) and the first
/// cp's word class.
fn cluster_scan(cluster_text: &str) -> (BreakKind, WordClass) {
    let mut chars = cluster_text.chars();
    let Some(first) = chars.next() else {
        return (BreakKind::None, WordClass::Other);
    };
    let mut kind = cp_break_kind(first as u32);
    for cp in chars {
        let next_kind = cp_break_kind(cp as u32);
        if kind == BreakKind::Whitespace || next_kind == BreakKind::Whitespace {
            // Only all-whitespace graphemes may be elided by wrapping.
            kind = if kind == BreakKind::Whitespace && next_kind == BreakKind::Whitespace {
                BreakKind::Whitespace
            } else {
                BreakKind::PreservedWhitespace
            };
        } else if kind == BreakKind::None {
            kind = next_kind;
        }
    }
    (kind, classify_word_class(first as u32))
}

/// `findChunkLayoutInfo` (utf8.zig:1638-1659 → the cluster walk
/// 1762-1888 + `commitLayoutCluster` 1562-1595): ordered break
/// opportunities + word-class edges. The pure-ASCII fast path
/// (`findAsciiLayoutWrapBreaks`, 1597-1635) emits the same breaks the
/// cluster walk produces for ASCII text (every byte its own cluster,
/// break kinds per byte, col == byte offset).
#[must_use]
pub fn layout_wrap_breaks(text: &str) -> (Vec<LayoutWrapBreak>, WordClass, WordClass) {
    let mut breaks: Vec<LayoutWrapBreak> = Vec::new();
    if text.is_empty() {
        return (breaks, WordClass::Other, WordClass::Other);
    }

    let clusters = text_clusters(text);
    let scans: Vec<(BreakKind, WordClass)> = clusters
        .iter()
        .map(|c| {
            let s = &text[c.byte_start as usize..c.byte_end() as usize];
            cluster_scan(s)
        })
        .collect();
    let first_cp = |i: usize| -> u32 {
        text[clusters[i].byte_start as usize..]
            .chars()
            .next()
            .map_or(0xFFFD, |c| c as u32)
    };

    let mut col = 0u32;
    for i in 1..clusters.len() {
        let prev = &clusters[i - 1];
        let (prev_kind, prev_class) = scans[i - 1];
        let next_class = scans[i].1;
        let next_cp = first_cp(i);

        let kind = if prev_kind != BreakKind::None {
            prev_kind
        } else if is_cjk_ascii_transition(prev_class, next_class) {
            BreakKind::ScriptTransition
        } else if prev.width > 0 && is_cjk_intercharacter_break(prev_class, next_class, next_cp) {
            BreakKind::CjkIntercharacter
        } else {
            BreakKind::None
        };
        if kind != BreakKind::None {
            breaks.push(LayoutWrapBreak {
                byte_start: prev.byte_start,
                col_start: col,
                byte_len: prev.byte_len,
                width_cols: prev.width,
                kind,
            });
        }
        col += prev.width;
    }

    // final cluster (utf8.zig:1872-1885): no successor → its own kind only
    let last = clusters.last().copied().unwrap();
    let (last_kind, _) = scans[scans.len() - 1];
    if last_kind != BreakKind::None {
        breaks.push(LayoutWrapBreak {
            byte_start: last.byte_start,
            col_start: col,
            byte_len: last.byte_len,
            width_cols: last.width,
            kind: last_kind,
        });
    }

    (breaks, scans[0].1, scans[scans.len() - 1].1)
}

// --- word mode: the pending-word machine (text-buffer-view.zig) -----------

/// `PendingWordPiece` — text-buffer-view.zig:1614 field set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingPiece {
    col_start: u32,
    width: u32,
    byte_start: u32,
    byte_end: u32,
}

impl PendingPiece {
    fn byte_len(&self) -> u32 {
        self.byte_end - self.byte_start
    }
}

/// `fitPendingWordPiece` result (1791-1793).
struct FitResult {
    width_cols: u32,
    bytes_used: u32,
}

/// The per-line word-wrap state — `WrapContext` fields
/// (text-buffer-view.zig:1614-1643). `source_line_cjk_breaks` stays
/// `true` (single chunk per line — the cross-chunk probe 2231-2249
/// never fires) and `pending_word_last_class` is only read by that
/// probe.
struct WordCtx<'a> {
    line: &'a LogicalLine,
    wrap_w: u32,
    cur: CurrentLine,
    source_col_start: u32,
    out: &'a mut Vec<VirtualLine>,
    pending: Vec<PendingPiece>,
    pending_width: u32,
    /// `source_line_has_non_whitespace` — leading indentation is
    /// content; only later separators may be elided (1956-1957).
    has_non_whitespace: bool,
    word_col_start: u32,
    word_byte_start: u32,
}

impl<'a> WordCtx<'a> {
    /// `wordWrapWidth` — `@max(lineWrapWidth(), 1)` (1649-1651).
    fn word_wrap_width(&self) -> u32 {
        self.wrap_w.max(1)
    }

    /// `addVirtualChunkSticky` (1722-1728) — render+word merges slices.
    fn add_chunk(&mut self, byte_start: u32, byte_len: u32, col_start: u32, width_cols: u32) {
        self.cur
            .add_chunk(byte_start, byte_len, col_start, width_cols, true);
    }

    /// `commitVirtualLineSticky` (1730-1736).
    fn commit(&mut self) {
        commit_line(&mut self.cur, &mut self.source_col_start, self.out);
    }

    /// `consumeDroppedWhitespace` (1738-1744): the separator hidden at
    /// a wrap point still advances the source offset.
    fn consume_dropped_whitespace(&mut self, width_cols: u32) {
        self.source_col_start += width_cols;
    }

    /// `queuePendingWordPiece` (1746-1770) — contiguous pieces merge.
    fn queue(&mut self, piece: PendingPiece) {
        if piece.width == 0 {
            return;
        }
        if let Some(last) = self.pending.last_mut() {
            if last.byte_end == piece.byte_start && last.col_start + last.width == piece.col_start {
                last.width += piece.width;
                last.byte_end = piece.byte_end;
                self.pending_width += piece.width;
                return;
            }
        }
        self.pending.push(piece);
        self.pending_width += piece.width;
    }

    /// `clearPendingWord` (1772-1776).
    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_width = 0;
    }

    fn clusters_in(&self, byte_start: u32, byte_end: u32) -> Vec<Cluster> {
        self.line
            .clusters
            .iter()
            .filter(|c| c.byte_start >= byte_start && c.byte_end() <= byte_end)
            .copied()
            .collect()
    }

    /// `fitPendingWordPiece` (1791-1836): grapheme-safe fit, optional
    /// forced single-grapheme overflow.
    fn fit_piece(&self, piece: &PendingPiece, max_width: u32, allow_forced: bool) -> FitResult {
        if piece.width <= max_width {
            return FitResult {
                width_cols: piece.width,
                bytes_used: piece.byte_len(),
            };
        }
        if max_width == 0 {
            return FitResult {
                width_cols: 0,
                bytes_used: 0,
            };
        }
        let clusters = self.clusters_in(piece.byte_start, piece.byte_end);
        let (bytes, _count, cols) = wrap_pos_by_width(&clusters, max_width);
        if cols > 0 && bytes > 0 {
            return FitResult {
                width_cols: cols.min(piece.width),
                bytes_used: bytes.min(piece.byte_len()),
            };
        }
        if !allow_forced {
            return FitResult {
                width_cols: 0,
                bytes_used: 0,
            };
        }
        let (fbytes, fcount, fcols) = grapheme_pos_by_width(&clusters, max_width);
        if fcount == 0 || fbytes == 0 {
            return FitResult {
                width_cols: 0,
                bytes_used: 0,
            };
        }
        FitResult {
            width_cols: fcols.min(piece.width),
            bytes_used: fbytes.min(piece.byte_len()),
        }
    }

    /// `consumePendingWordPrefix` (1838-1867).
    fn consume_pending_prefix(&mut self, max_width: u32) -> bool {
        if max_width == 0 || self.pending_width == 0 {
            return false;
        }
        let before = self.cur.width_cols;
        let mut remaining = max_width;
        let mut consumed = 0usize;
        while consumed < self.pending.len() && remaining > 0 {
            let piece = self.pending[consumed];
            if piece.width <= remaining {
                self.add_chunk(
                    piece.byte_start,
                    piece.byte_len(),
                    piece.col_start,
                    piece.width,
                );
                remaining -= piece.width;
                consumed += 1;
                continue;
            }
            let fit = self.fit_piece(&piece, remaining, self.cur.width_cols == before);
            if fit.width_cols == 0 {
                break;
            }
            self.add_chunk(
                piece.byte_start,
                fit.bytes_used,
                piece.col_start,
                fit.width_cols,
            );
            let p = &mut self.pending[consumed];
            p.col_start += fit.width_cols;
            p.width -= fit.width_cols;
            p.byte_start += fit.bytes_used;
            if p.width == 0 {
                consumed += 1;
            }
            break;
        }
        self.pending.drain(0..consumed);
        let consumed_cols = self.cur.width_cols - before;
        self.pending_width -= consumed_cols;
        consumed_cols > 0
    }

    /// `appendPendingWordToLine` (1869-1874).
    fn append_pending(&mut self) {
        for piece in self.pending.clone() {
            self.add_chunk(
                piece.byte_start,
                piece.byte_len(),
                piece.col_start,
                piece.width,
            );
        }
        self.clear_pending();
    }

    /// `finalizePendingWord` (1876-1890).
    fn finalize_pending(&mut self) {
        while self.pending_width > 0 {
            let limit = self.word_wrap_width();
            if self.cur.width_cols > 0 && self.cur.width_cols + self.pending_width > limit {
                self.commit();
                continue;
            }
            if self.cur.width_cols == 0 && self.pending_width > limit {
                if !self.consume_pending_prefix(limit) {
                    return;
                }
                if self.pending_width > 0 {
                    self.commit();
                }
                continue;
            }
            self.append_pending();
        }
    }

    /// `placeCompleteWordPiece` (1892-1919).
    fn place_complete_word_piece(&mut self, piece: PendingPiece) {
        if piece.width == 0 {
            return;
        }
        let mut piece = piece;
        while piece.width > 0 {
            let limit = self.word_wrap_width();
            if piece.width <= limit {
                if self.cur.width_cols > 0 && self.cur.width_cols + piece.width > limit {
                    self.commit();
                }
                self.add_chunk(
                    piece.byte_start,
                    piece.byte_len(),
                    piece.col_start,
                    piece.width,
                );
                return;
            }
            if self.cur.width_cols > 0 {
                self.commit();
            }
            let fit = self.fit_piece(&piece, self.word_wrap_width(), true);
            if fit.width_cols == 0 {
                return;
            }
            self.add_chunk(
                piece.byte_start,
                fit.bytes_used,
                piece.col_start,
                fit.width_cols,
            );
            piece.col_start += fit.width_cols;
            piece.width -= fit.width_cols;
            piece.byte_start += fit.bytes_used;
            if piece.width > 0 {
                self.commit();
            }
        }
    }

    /// `flushCompleteWordPiece` (1921-1929).
    fn flush_complete_word_piece(&mut self, piece: PendingPiece) {
        if piece.width == 0 {
            return;
        }
        if self.pending_width > 0 {
            self.queue(piece);
            self.finalize_pending();
        } else {
            self.place_complete_word_piece(piece);
        }
    }

    /// `processWhitespaceBreak` (1931-1978).
    fn process_whitespace_break(&mut self, wrap_break: LayoutWrapBreak) {
        let col_start = self.word_col_start;
        let byte_start = self.word_byte_start;
        // A fitting word and separator already coalesce into one chunk.
        if self.pending_width == 0
            && wrap_break.width_cols > 0
            && wrap_break.col_start > col_start
            && self.cur.width_cols + (wrap_break.col_end() - col_start) <= self.word_wrap_width()
        {
            self.add_chunk(
                byte_start,
                wrap_break.byte_end() - byte_start,
                col_start,
                wrap_break.col_end() - col_start,
            );
            self.has_non_whitespace = true;
            return;
        }
        if wrap_break.col_start > col_start {
            self.flush_complete_word_piece(PendingPiece {
                col_start,
                width: wrap_break.col_start - col_start,
                byte_start,
                byte_end: wrap_break.byte_start,
            });
            self.has_non_whitespace = true;
        } else if self.pending_width > 0 {
            self.finalize_pending();
        }

        // Leading indentation is content; only later separators may be elided.
        let preserve_leading = !self.has_non_whitespace;
        let limit = self.word_wrap_width();
        if !preserve_leading && self.cur.width_cols + wrap_break.width_cols > limit {
            if self.cur.width_cols > 0 {
                self.commit();
            }
            self.consume_dropped_whitespace(wrap_break.width_cols);
            return;
        }
        if !preserve_leading && self.cur.width_cols == 0 {
            self.consume_dropped_whitespace(wrap_break.width_cols);
            return;
        }
        self.place_complete_word_piece(PendingPiece {
            col_start: wrap_break.col_start,
            width: wrap_break.width_cols,
            byte_start: wrap_break.byte_start,
            byte_end: wrap_break.byte_end(),
        });
    }

    /// `processWordWrapBreakValue` (1980-2011) with
    /// `source_line_cjk_breaks` fixed true (single chunk per line).
    fn process_break(&mut self, wrap_break: LayoutWrapBreak) {
        let col_end = wrap_break.col_end().min(self.line.width_cols);
        let byte_end = wrap_break.byte_end().min(self.line.text.len() as u32);
        if col_end < self.word_col_start || byte_end < self.word_byte_start {
            return;
        }
        if wrap_break.kind == BreakKind::Whitespace {
            self.process_whitespace_break(wrap_break);
            self.word_col_start = col_end;
            self.word_byte_start = byte_end;
            return;
        }
        if col_end > self.word_col_start {
            self.flush_complete_word_piece(PendingPiece {
                col_start: self.word_col_start,
                width: col_end - self.word_col_start,
                byte_start: self.word_byte_start,
                byte_end,
            });
            self.has_non_whitespace = true;
        } else if byte_end > self.word_byte_start && self.pending_width > 0 {
            self.finalize_pending();
        }
        self.word_col_start = col_end;
        self.word_byte_start = byte_end;
    }
}

/// `processWordChunk` + `line_end_callback` (text-buffer-view.zig
/// 2018-2131 + 2287-2302 + 2304-2319) for the one-chunk-per-line shape
/// this port models.
fn word_mode_line(line: &LogicalLine, wrap_w: u32, out: &mut Vec<VirtualLine>) {
    let mut ctx = WordCtx {
        line,
        wrap_w,
        cur: CurrentLine::default(),
        source_col_start: 0,
        out,
        pending: Vec::new(),
        pending_width: 0,
        has_non_whitespace: false,
        word_col_start: 0,
        word_byte_start: 0,
    };

    let (breaks, _first_class, _last_class) = layout_wrap_breaks(&line.text);
    for b in breaks {
        ctx.process_break(b);
    }
    // trailing segment after the last break (2118-2129)
    if ctx.word_col_start < line.width_cols {
        ctx.queue(PendingPiece {
            col_start: ctx.word_col_start,
            width: line.width_cols - ctx.word_col_start,
            byte_start: ctx.word_byte_start,
            byte_end: line.text.len() as u32,
        });
        ctx.has_non_whitespace = true;
    }
    ctx.finalize_pending();
    ctx.clear_pending();

    // line_end tail (2304-2319)
    if !ctx.cur.slices.is_empty() || line.width_cols == 0 {
        let width_cols = ctx.cur.width_cols;
        let slices = std::mem::take(&mut ctx.cur.slices);
        let source_col_start = ctx.source_col_start;
        ctx.out.push(VirtualLine {
            source_line: 0,
            source_col_start,
            width_cols,
            slices,
        });
    }
}

// --- the draw loop (buffer.zig drawTextBufferInternal) ---------------------

/// Viewport — `Viewport` (text-buffer-view.zig:105-110). Scroll offsets
/// stay 0 in this lane; width/height come from the renderable's
/// laid-out size clamped ≥ 1 (`Math.max(layout.x, 1)` —
/// Renderable.ts:1114-1115 via onResize → setViewport).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// `setTabIndicator` (text-buffer-view.ts:204-207): a string indicator
/// is its first codepoint; a number is the codepoint itself.
#[must_use]
pub fn tab_indicator_codepoint(indicator: Option<&str>, numeric: Option<u32>) -> Option<u32> {
    if let Some(n) = numeric {
        return Some(n);
    }
    indicator.map(|s| s.chars().next().map_or(0, |c| c as u32))
}

/// The fixed draw state a text renderable passes down — the
/// `TextBufferView` fields the raster reads. `viewport: None` is the
/// pre-layout state: no alignment (getLineAlignmentPad returns 0 when
/// the viewport is null, text-buffer-view.zig:421-425), no horizontal
/// clip (`viewport_width = maxInt(u32)`, buffer.zig:1695-1696), no
/// vertical slice (getVirtualLines returns everything).
pub struct TextViewState {
    pub text_align: TextAlign,
    pub tab_indicator: Option<u32>,
    pub tab_indicator_color: Option<Rgba>,
    pub viewport: Option<Viewport>,
}

/// `drawTextBufferInternal` (buffer.zig:1652-2087) — the subset the
/// headless differential profile exercises: no selection, no truncate,
/// no scissor, opacity 1. `vlines` is the **full** virtual-line list;
/// the viewport slice is taken here (`getVirtualLines`,
/// text-buffer-view.zig:492-508).
pub fn draw_text_view(
    buffer: &mut OptimizedBuffer,
    doc: &TextDocument,
    view: &TextViewState,
    vlines: &[VirtualLine],
    x: i32,
    y: i32,
) {
    if vlines.is_empty() {
        return;
    }
    // getVirtualLines: [vp.y, vp.y + vp.height) clamped to the list
    let vl: &[VirtualLine] = match view.viewport {
        Some(vp) => {
            let start = (vp.y as usize).min(vlines.len());
            let end = (start + vp.height as usize).min(vlines.len());
            &vlines[start..end]
        }
        None => vlines,
    };
    // horizontal offset / viewport width (buffer.zig:1695-1696)
    let (h_off, vp_w) = match view.viewport {
        Some(vp) => (vp.x, vp.width),
        None => (0, u32::MAX),
    };
    let defaults = doc.defaults;

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

        // per-vline alignment (1716-1717): no viewport → no pad
        // (getLineAlignmentPad, text-buffer-view.zig:421-425)
        let align_pad = match view.viewport {
            Some(_) => alignment_pad_cols(view.text_align, vp_w, vline.width_cols) as i32,
            None => 0,
        };
        let mut current_x = x + align_pad;
        let mut rendered_col: u32 = 0;

        let line = &doc.lines[vline.source_line as usize];
        let spans = &line.spans;

        // initial span state (1725-1756): the span containing start_col
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

                // horizontal offset skip (1801-1811)
                if rendered_col < h_off {
                    rendered_col += w;
                    continue;
                }
                if rendered_col >= h_off + vp_w {
                    break; // past the viewport width
                }
                // glyph ends at/before column 0 → left of screen (1818-1824)
                if current_x + w as i32 <= 0 {
                    current_x += w as i32;
                    rendered_col += w;
                    continue;
                }
                if current_x >= buffer.width as i32 {
                    break;
                }
                // wide glyph must fit the viewport AND the screen (1840-1859)
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

                // zero-width clusters render nothing, advance nothing (1982-1984)
                if w == 0 {
                    continue;
                }

                let mut draw_fg = line_fg;
                let mut draw_bg = line_bg;
                let draw_attributes = line_attributes;
                // INVERSE swaps fg/bg at draw time (1990-1994)
                if draw_attributes & text_attributes::INVERSE_U32 != 0 {
                    core::mem::swap(&mut draw_fg, &mut draw_bg);
                }

                if is_tab {
                    // tab indicator cells (2007-2034)
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
                    // cluster encoding (2036-2048): single printable ASCII
                    // byte inline; everything else through the grapheme pool
                    let bytes =
                        &line.text[cluster.byte_start as usize..cluster.byte_end() as usize];
                    let encoded = if bytes.len() == 1 && w == 1 && bytes.as_bytes()[0] >= 32 {
                        u32::from(bytes.as_bytes()[0])
                    } else {
                        let gid = buffer.alloc_grapheme_pub(bytes);
                        pack_grapheme_start(gid, w)
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
