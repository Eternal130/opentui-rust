//! Unit tests for the pure layout math in `text_buffer` — the wrap
//! break scan, virtual-line walkers, alignment pads and span model.
//! Each case cites the upstream behavior the differential scenarios
//! also pin byte-for-byte.

use crate::ansi::text_attributes;
use crate::color::Rgba;
use crate::renderables::text::WrapMode;
use crate::styled_text::{string_to_styled_text, StyledText, TextChunk};
use crate::text_buffer::{
    alignment_pad_cols, compute_virtual_lines, layout_wrap_breaks, text_clusters, TextAlign,
    TextDefaults, TextDocument,
};

fn defaults() -> TextDefaults {
    TextDefaults {
        fg: Rgba::rgb(255, 255, 255),
        bg: Rgba::from_ints(0, 0, 0, 0),
        attributes: 0,
    }
}

fn doc_from(text: &str) -> TextDocument {
    TextDocument::from_styled(&string_to_styled_text(text).chunks, defaults())
}

fn vline_texts(doc: &TextDocument, mode: WrapMode, width: u32) -> Vec<String> {
    compute_virtual_lines(doc, mode, Some(width))
        .iter()
        .map(|v| {
            v.slices
                .iter()
                .map(|s| {
                    doc.lines[v.source_line as usize].text
                        [s.byte_start as usize..(s.byte_start + s.byte_len) as usize]
                        .to_owned()
                })
                .collect::<String>()
        })
        .collect()
}

// --- alignmentPadCols (text-buffer-view.zig:224-231) ----------------------

#[test]
fn alignment_pad_math() {
    assert_eq!(alignment_pad_cols(TextAlign::Left, 20, 5), 0);
    assert_eq!(alignment_pad_cols(TextAlign::Center, 20, 5), 7); // (20-5)/2
    assert_eq!(alignment_pad_cols(TextAlign::Center, 21, 5), 8); // floor
    assert_eq!(alignment_pad_cols(TextAlign::Right, 20, 5), 15);
    // line at least as wide as the viewport → no pad
    assert_eq!(alignment_pad_cols(TextAlign::Right, 5, 5), 0);
    assert_eq!(alignment_pad_cols(TextAlign::Center, 4, 5), 0);
}

// --- the wrap-break scan (utf8.zig findChunkLayoutInfo) --------------------

#[test]
fn wrap_breaks_ascii() {
    let (breaks, first, last) = layout_wrap_breaks("hello world");
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].col_start, 5);
    assert_eq!(breaks[0].width_cols, 1);
    assert_eq!(breaks[0].byte_start, 5);
    assert_eq!(breaks[0].kind, crate::text_buffer::BreakKind::Whitespace);
    assert_eq!(first, crate::text_buffer::WordClass::AsciiWord);
    assert_eq!(last, crate::text_buffer::WordClass::AsciiWord);
}

#[test]
fn wrap_breaks_punctuation_and_cjk() {
    // punctuation: each break char is its own opportunity
    let (breaks, _, _) = layout_wrap_breaks("a-b.c");
    assert_eq!(breaks.len(), 2);
    assert_eq!(breaks[0].col_start, 1);
    assert_eq!(breaks[1].col_start, 3);

    // CJK run: inter-character opportunities (not word boundaries)
    let (breaks, _, _) = layout_wrap_breaks("中文测试");
    assert_eq!(breaks.len(), 3); // between each pair
    assert_eq!(breaks[0].width_cols, 2);
    assert!(breaks
        .iter()
        .all(|b| b.kind == crate::text_buffer::BreakKind::CjkIntercharacter));

    // CJK→ASCII transition is a break
    let (breaks, _, _) = layout_wrap_breaks("中文abc");
    assert_eq!(breaks.len(), 2);
    assert_eq!(
        breaks[0].kind,
        crate::text_buffer::BreakKind::CjkIntercharacter
    );
    assert_eq!(
        breaks[1].kind,
        crate::text_buffer::BreakKind::ScriptTransition
    );
}

#[test]
fn tab_is_a_two_col_whitespace_break() {
    let clusters = text_clusters("a\tb");
    assert_eq!(clusters[1].width, 2); // tab_width = 2 (text-buffer.zig:291)
    let (breaks, _, _) = layout_wrap_breaks("a\tb");
    assert_eq!(breaks[0].kind, crate::text_buffer::BreakKind::Whitespace);
    assert_eq!(breaks[0].col_start, 1);
    assert_eq!(breaks[0].width_cols, 2);
}

// --- virtual lines ---------------------------------------------------------

#[test]
fn none_mode_one_vline_per_line() {
    let doc = doc_from("a\nbb\n");
    let vlines = compute_virtual_lines(&doc, WrapMode::None, Some(3));
    assert_eq!(vlines.len(), 3); // trailing \n creates an empty line
    assert_eq!(vlines[0].width_cols, 1);
    assert_eq!(vlines[1].width_cols, 2);
    assert_eq!(vlines[2].width_cols, 0);
    assert!(vlines[2].slices.is_empty());
}

#[test]
fn empty_document_has_no_lines() {
    let doc = doc_from("");
    assert!(doc.lines.is_empty());
    let vlines = compute_virtual_lines(&doc, WrapMode::Word, Some(10));
    assert!(vlines.is_empty());
    // measure clamps to ≥1 at the caller (native-renderable.zig:72-73)
    let (w, h) = doc.measure(WrapMode::Word, 10);
    assert_eq!((w, h), (0, 0));
}

#[test]
fn word_mode_breaks_and_elides_separators() {
    let doc = doc_from("hello world foo");
    let texts = vline_texts(&doc, WrapMode::Word, 11);
    // "hello world" fits exactly; the separator before "foo" is dropped
    assert_eq!(texts, vec!["hello world", "foo"]);
}

#[test]
fn word_mode_hard_breaks_overlong_words() {
    let doc = doc_from("supercalifragilistic");
    let texts = vline_texts(&doc, WrapMode::Word, 5);
    assert_eq!(texts, vec!["super", "calif", "ragil", "istic"]);
}

#[test]
fn word_mode_preserves_leading_indentation() {
    // leading indentation is content (text-buffer-view.zig:1956-1957)
    let doc = doc_from("  hi");
    let texts = vline_texts(&doc, WrapMode::Word, 2);
    assert_eq!(texts, vec!["  ", "hi"]);
}

#[test]
fn char_mode_breaks_anywhere_and_snaps_wide() {
    let doc = doc_from("abcd");
    let texts = vline_texts(&doc, WrapMode::Char, 3);
    assert_eq!(texts, vec!["abc", "d"]);

    // wide glyphs never split: 世 fits exactly 2, line of width 3 → 世 + a
    let doc = doc_from("世界");
    let texts = vline_texts(&doc, WrapMode::Char, 3);
    assert_eq!(texts, vec!["世", "界"]);
    assert_eq!(
        compute_virtual_lines(&doc, WrapMode::Char, Some(3))[0].width_cols,
        2
    );
}

#[test]
fn char_mode_forces_overwide_grapheme_on_tiny_width() {
    // wrap width 1 < cluster width 2: the grapheme is force-placed
    // (processCharChunk force path, text-buffer-view.zig:2152-2160)
    let doc = doc_from("世界");
    let texts = vline_texts(&doc, WrapMode::Char, 1);
    assert_eq!(texts, vec!["世", "界"]);
}

#[test]
fn wrapped_lines_record_source_columns() {
    let doc = doc_from("hello world");
    let vlines = compute_virtual_lines(&doc, WrapMode::Word, Some(5));
    assert_eq!(vlines[0].source_col_start, 0);
    // the dropped separator advances the source offset (1738-1744)
    assert_eq!(vlines[1].source_col_start, 6);
}

// --- the span model (setStyledText) ----------------------------------------

#[test]
fn spans_partition_chunks_by_column() {
    let styled = StyledText::new(vec![
        TextChunk::new("red "),
        TextChunk {
            fg: Some(Rgba::rgb(255, 0, 0)),
            attributes: text_attributes::BOLD_U32,
            ..TextChunk::new("green ")
        },
        TextChunk::new("tail"),
    ]);
    let doc = TextDocument::from_styled(&styled.chunks, defaults());
    assert_eq!(doc.lines.len(), 1);
    let spans = &doc.lines[0].spans;
    assert_eq!(spans.len(), 3);
    assert_eq!((spans[0].col_start, spans[0].col_end), (0, 4));
    assert_eq!((spans[1].col_start, spans[1].col_end), (4, 10));
    assert_eq!(spans[1].fg, Some(Rgba::rgb(255, 0, 0)));
    assert_eq!(spans[1].attributes, text_attributes::BOLD_U32);
    assert_eq!((spans[2].col_start, spans[2].col_end), (10, 14));
}

#[test]
fn spans_split_chunks_across_lines_with_trailing_newline() {
    let styled = StyledText::new(vec![TextChunk::new("ab\ncd")]);
    let doc = TextDocument::from_styled(&styled.chunks, defaults());
    assert_eq!(doc.lines.len(), 2);
    assert_eq!(doc.lines[0].text, "ab");
    assert_eq!(doc.lines[1].text, "cd");
    assert_eq!(doc.lines[0].spans[0].col_end, 2);
    assert_eq!(doc.lines[1].spans[0].col_end, 2);
}

#[test]
fn tab_columns_count_two_in_spans() {
    let styled = StyledText::new(vec![TextChunk::new("a\tb"), TextChunk::new("!")]);
    let doc = TextDocument::from_styled(&styled.chunks, defaults());
    let spans = &doc.lines[0].spans;
    // "a\tb" = 1 + 2 + 1 = 4 columns
    assert_eq!((spans[0].col_start, spans[0].col_end), (0, 4));
    assert_eq!((spans[1].col_start, spans[1].col_end), (4, 5));
}

// --- measure ---------------------------------------------------------------

#[test]
fn measure_intrinsic_vs_wrapped() {
    let doc = doc_from("hello world\nsecond line");
    // width 0 → intrinsic (measureForDimensions 1467-1488)
    assert_eq!(doc.measure(WrapMode::Word, 0), (11, 2));
    // wrapped at 5: "hello"/"world" + "secon"/"d "/"line" (the separator
    // lands on the "d" line — kept because "line" then doesn't fit)
    assert_eq!(doc.measure(WrapMode::Word, 5), (5, 5));
    // none mode always intrinsic
    assert_eq!(doc.measure(WrapMode::None, 5), (11, 2));
}

// --- truncate: applyTruncation (text-buffer-view.zig:1280-1445) -----------

fn truncate_ellipsis_slices(
    doc: &TextDocument,
    vlines: &mut [crate::text_buffer::VirtualLine],
    vp_width: u32,
) {
    crate::text_buffer::apply_truncation(doc, vlines, vp_width);
}

#[test]
fn truncate_splits_prefix_ellipsis_suffix() {
    // applyTruncation 1368-1431: available = vp - 3, prefix = /2,
    // suffix = remainder anchored at the line end
    let doc = doc_from("abcdefghijklmnop"); // 16 cols
    let mut vlines = compute_virtual_lines(&doc, WrapMode::None, None);
    truncate_ellipsis_slices(&doc, &mut vlines, 10);
    assert_eq!(vlines.len(), 1);
    let v = &vlines[0];
    // available 7 → prefix 3 ("abc"), ellipsis, suffix 4 ("mnop")
    assert!(v.is_truncated);
    assert_eq!(v.width_cols, 10);
    assert_eq!(v.ellipsis_col, 3);
    assert_eq!(v.truncation_suffix_col_start, 12);
    assert_eq!(v.slices.len(), 3);
    assert!(!v.slices[0].is_ellipsis);
    assert_eq!(v.slices[0].width_cols, 3);
    assert!(v.slices[1].is_ellipsis);
    assert_eq!(v.slices[1].width_cols, crate::text_buffer::ELLIPSIS_WIDTH);
    assert_eq!(v.slices[2].width_cols, 4);
}

#[test]
fn truncate_narrow_viewport_keeps_only_the_mark() {
    // vp.width <= 3 (1363-1365): no chunks, zero width, mark set
    let doc = doc_from("abcdef");
    let mut vlines = compute_virtual_lines(&doc, WrapMode::None, None);
    truncate_ellipsis_slices(&doc, &mut vlines, 3);
    let v = &vlines[0];
    assert!(v.is_truncated);
    assert!(v.slices.is_empty());
    assert_eq!(v.width_cols, 0);
    assert_eq!(v.truncation_suffix_col_start, 6); // original width
}

#[test]
fn truncate_leaves_fitting_lines_untouched() {
    // overflow_count == 0 → early return (1344-1348)
    let doc = doc_from("abc\ndefghi");
    let mut vlines = compute_virtual_lines(&doc, WrapMode::None, None);
    let before = vlines.clone();
    truncate_ellipsis_slices(&doc, &mut vlines, 6);
    assert_eq!(vlines, before);
}

#[test]
fn truncate_cuts_snap_to_graphemes() {
    // keepChunkPrefix/dropChunkPrefix (1288-1334) are grapheme-safe:
    // a wide cluster that straddles the cut is dropped whole
    let doc = doc_from("a世bc"); // widths 1,2,1,1
    let mut vlines = compute_virtual_lines(&doc, WrapMode::None, None);
    truncate_ellipsis_slices(&doc, &mut vlines, 4);
    let v = &vlines[0];
    // available 1, prefix 0, suffix 1 → ellipsis then "c"
    assert!(v.is_truncated);
    assert_eq!(v.ellipsis_col, 0);
    assert_eq!(v.truncation_suffix_col_start, 4);
    assert_eq!(v.slices.len(), 2);
    assert_eq!(v.slices[1].width_cols, 1);
    // the suffix byte window is the trailing one-column cluster
    let line = &doc.lines[0];
    assert_eq!(
        &line.text[v.slices[1].byte_start as usize
            ..(v.slices[1].byte_start + v.slices[1].byte_len) as usize],
        "c"
    );
}

// --- the draw loop's three deferred semantics ------------------------------

use crate::buffer::OptimizedBuffer;
use crate::text_buffer::{draw_text_view, TextViewState, Viewport};

fn draw_doc(doc: &TextDocument, vp: Option<Viewport>, truncate_vp: Option<u32>) -> OptimizedBuffer {
    let mut buffer = OptimizedBuffer::new(20, 5);
    let mut vlines = compute_virtual_lines(doc, WrapMode::None, None);
    if let Some(w) = truncate_vp {
        crate::text_buffer::apply_truncation(doc, &mut vlines, w);
    }
    let view = TextViewState {
        text_align: TextAlign::Left,
        tab_indicator: None,
        tab_indicator_color: None,
        viewport: vp,
    };
    draw_text_view(&mut buffer, doc, &view, &vlines, 0, 0);
    buffer
}

fn row_text(buffer: &OptimizedBuffer, y: u32) -> String {
    (0..buffer.width)
        .map(|x| {
            buffer
                .get(x, y)
                .map(|c| char::from_u32(c.char).unwrap_or('?'))
                .unwrap_or(' ')
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn truncate_draws_prefix_ellipsis_suffix() {
    let doc = doc_from("abcdefghijklmnop");
    let buffer = draw_doc(
        &doc,
        Some(Viewport {
            x: 0,
            y: 0,
            width: 10,
            height: 5,
        }),
        Some(10),
    );
    // prefix 3 + "..." + suffix 4, exactly the 10-column viewport
    assert_eq!(row_text(&buffer, 0), "abc...mnop");
}

#[test]
fn scissor_clips_cells_and_drops_straddling_wide_glyphs() {
    // drawTextBufferInternal: per-glyph skip (1832-1835), wide-glyph
    // whole-drop when any cell is outside (1849-1861)
    let doc = doc_from("a世bc"); // cols: a=0, 世=1..2, b=3, c=4
    let mut buffer = OptimizedBuffer::new(20, 2);
    buffer.push_scissor_rect(2, 0, 2, 2); // columns 2..3 only
    let vlines = compute_virtual_lines(&doc, WrapMode::None, None);
    let view = TextViewState {
        text_align: TextAlign::Left,
        tab_indicator: None,
        tab_indicator_color: None,
        viewport: Some(Viewport {
            x: 0,
            y: 0,
            width: 20,
            height: 2,
        }),
    };
    draw_text_view(&mut buffer, &doc, &view, &vlines, 0, 0);
    buffer.pop_scissor_rect();
    // scissor covers cols 2..3: 'a' (col 0) skipped per-glyph, 世 spans
    // cols 1..2 with its first cell outside → the whole glyph drops
    // (upstream 1832-1835 + 1849-1861), 'b' (col 3) draws, 'c' (col 4)
    // is outside again
    assert_eq!(row_text(&buffer, 0), "   b");
    // the cells outside the scissor stay untouched
    assert_eq!(
        buffer.get(0, 0).unwrap().char,
        crate::buffer::DEFAULT_SPACE_CHAR
    );
    assert_eq!(
        buffer.get(4, 0).unwrap().char,
        crate::buffer::DEFAULT_SPACE_CHAR
    );
}

#[test]
fn scissor_nested_intersect_clips_draw() {
    // pushScissorRect intersects with the parent (buffer.zig:375-394):
    // [1..5] ∩ [3..8] = [3..4]
    let doc = doc_from("abcdef");
    let mut buffer = OptimizedBuffer::new(10, 2);
    buffer.push_scissor_rect(1, 0, 4, 2);
    buffer.push_scissor_rect(3, 0, 5, 2);
    let vlines = compute_virtual_lines(&doc, WrapMode::None, None);
    let view = TextViewState {
        text_align: TextAlign::Left,
        tab_indicator: None,
        tab_indicator_color: None,
        viewport: Some(Viewport {
            x: 0,
            y: 0,
            width: 10,
            height: 2,
        }),
    };
    draw_text_view(&mut buffer, &doc, &view, &vlines, 0, 0);
    buffer.pop_scissor_rect();
    buffer.pop_scissor_rect();
    // [1..5] ∩ [3..8] = cols 3..4: only 'd' and 'e' survive
    assert_eq!(row_text(&buffer, 0), "   de");
}

#[test]
fn scroll_offset_y_slices_the_vline_window() {
    // getVirtualLines (492-508): the viewport shows [y, y+height)
    let doc = doc_from("one\ntwo\nthree\nfour");
    let buffer = draw_doc(
        &doc,
        Some(Viewport {
            x: 0,
            y: 2,
            width: 20,
            height: 5,
        }),
        None,
    );
    assert_eq!(row_text(&buffer, 0), "three");
    assert_eq!(row_text(&buffer, 1), "four");
    assert_eq!(row_text(&buffer, 2), "");
}

#[test]
fn scroll_offset_x_skips_columns_and_drops_edge_wide_glyphs() {
    // horizontal offset (1801-1811) skips the first columns; a wide
    // glyph straddling the offset edge is dropped whole (1840-1859)
    let doc = doc_from("a世bc");
    // h_off 2: 'a' and the first cell of 世 are skipped; 世 would start
    // at screen col -1 → dropped (currentX + width <= 0 path is for
    // fully-left glyphs; here the skip consumes it via rendered_col)
    let buffer = draw_doc(
        &doc,
        Some(Viewport {
            x: 2,
            y: 0,
            width: 18,
            height: 5,
        }),
        None,
    );
    // cols 0..2 skipped ('a' 1 + 世 2 → 3 skipped actually: 'a'=1, 世=2
    // covers cols 1-2), so 'b' lands at col 0
    assert_eq!(row_text(&buffer, 0), "bc");
}
