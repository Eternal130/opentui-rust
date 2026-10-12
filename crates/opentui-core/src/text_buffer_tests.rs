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
