//! Behavior tests for the editor core — ports of upstream's own suites
//! (`packages/core/src/edit-buffer.test.ts`, `editor-view.test.ts`,
//! `lib/keybinding.internal.test.ts`) plus oracle-verified
//! characterization matrices (the published `@opentui/core@0.5.14`
//! driven through the same calls — see the lane report).

use crate::editor::{EditBuffer, EditorView, SelectionBehavior, SelectionOccupancy};
use crate::keybinding::{
    build_key_bindings_map, default_key_aliases, get_key_binding_action, get_key_binding_key,
    key_binding_to_string, merge_key_aliases, merge_key_bindings, KeyBinding, KeyBindingLike,
    KeyEvent,
};
use crate::renderables::text::WrapMode;
use crate::renderables::textarea::{
    default_textarea_key_bindings, TextareaOptions, TEXTAREA_ACTIONS,
};
use crate::renderer::{create_cli_renderer, CliRendererConfig};

fn buffer(text: &str) -> EditBuffer {
    let mut b = EditBuffer::new();
    b.set_text(text);
    b
}

// --- setText / getText (edit-buffer.test.ts:28-58) ---------------------------

#[test]
fn set_and_get_text() {
    let mut b = EditBuffer::new();
    b.set_text("Hello World");
    assert_eq!(b.get_text(), "Hello World");
    b.set_text("");
    assert_eq!(b.get_text(), "");
    let text = "Line 1\nLine 2\nLine 3";
    b.set_text(text);
    assert_eq!(b.get_text(), text);
    b.set_text("Hello 世界 🌟");
    assert_eq!(b.get_text(), "Hello 世界 🌟");
}

#[test]
fn empty_buffer_keeps_one_line() {
    // oracle: setText("") → getLineCount() == 1 (the rope's end invariant)
    let mut b = EditBuffer::new();
    b.set_text("");
    assert_eq!(b.line_count(), 1);
    assert_eq!(b.text_end_offset(), 0);
    b.set_text("x");
    b.delete_range(0, 0, 0, 1);
    assert_eq!(b.line_count(), 1);
    assert_eq!(b.get_text(), "");
    b.clear();
    assert_eq!(b.line_count(), 1);
}

#[test]
fn crlf_normalizes_to_lf() {
    // findLineBreaks: CR and CRLF are single breaks; getText joins with \n
    let mut b = EditBuffer::new();
    b.set_text("a\r\nb\rc");
    assert_eq!(b.get_text(), "a\nb\nc");
    assert_eq!(b.line_count(), 3);
    // offsets: the CRLF break costs 1
    assert_eq!(b.offset_to_coords(2), Some((1, 0)));
    assert_eq!(b.offset_to_coords(4), Some((2, 0)));
}

// --- cursor positions (edit-buffer.test.ts:60-138) ---------------------------

#[test]
fn cursor_starts_at_beginning_after_set_text() {
    let mut b = buffer("Hello World");
    assert_eq!(b.cursor_position().row, 0);
    assert_eq!(b.cursor_position().col, 0);
    b.move_right();
    assert_eq!(b.cursor_position().col, 1);
    assert_eq!(b.cursor_position().offset, 1);
}

#[test]
fn multi_line_cursor_positions() {
    let mut b = buffer("Line 1\nLine 2\nLine 3");
    b.move_down();
    assert_eq!(b.cursor_position().row, 1);
    b.move_down();
    assert_eq!(b.cursor_position().row, 2);
}

#[test]
fn set_cursor_clamps() {
    let mut b = buffer("ab\ncd");
    b.set_cursor(999, 999);
    assert_eq!(b.cursor_position(), (1, 2, 5).into_cursor());
    b.set_cursor_by_offset(3);
    assert_eq!(b.cursor_position(), (1, 0, 3).into_cursor());
    b.set_cursor(0, 99);
    assert_eq!(b.cursor_position(), (0, 2, 2).into_cursor());
}

trait CursorTuple {
    fn into_cursor(self) -> crate::editor::LogicalCursor;
}
impl CursorTuple for (u32, u32, u32) {
    fn into_cursor(self) -> crate::editor::LogicalCursor {
        crate::editor::LogicalCursor {
            row: self.0,
            col: self.1,
            offset: self.2,
        }
    }
}

// --- motion (edit-buffer.test.ts:140-202, 709-767) ---------------------------

#[test]
fn move_left_right() {
    let mut b = buffer("ABCDE");
    b.set_cursor_to_line_col(0, 5);
    assert_eq!(b.cursor_position().col, 5);
    b.move_left();
    assert_eq!(b.cursor_position().col, 4);
    b.move_left();
    assert_eq!(b.cursor_position().col, 3);
    // BOL wraps to previous line EOL
    let mut b = buffer("ab\ncd");
    b.set_cursor(1, 0);
    b.move_left();
    assert_eq!(b.cursor_position(), (0, 2, 2).into_cursor());
    // EOL wraps to next line BOL
    b.set_cursor(0, 2);
    b.move_right();
    assert_eq!(b.cursor_position(), (1, 0, 3).into_cursor());
}

#[test]
fn move_up_down_desired_col() {
    let mut b = buffer("Line 1\nLonger line 2\nL3");
    b.set_cursor(1, 11);
    b.move_up();
    assert_eq!(b.cursor_position(), (0, 6, 6).into_cursor());
    // desired col restored going down
    b.move_down();
    assert_eq!(b.cursor_position(), (1, 11, 18).into_cursor());
    b.move_down();
    assert_eq!(b.cursor_position(), (2, 2, 23).into_cursor());
}

#[test]
fn grapheme_aware_motion() {
    // "a🌟b": 🌟 is 2 cells, one cluster
    let mut b = buffer("a🌟b");
    b.set_cursor(0, 1);
    b.move_right();
    assert_eq!(b.cursor_position().col, 3);
    b.move_left();
    assert_eq!(b.cursor_position().col, 1);
    // wide CJK
    let mut b = buffer("你好");
    b.set_cursor(0, 2);
    b.move_left();
    assert_eq!(b.cursor_position().col, 0);
}

#[test]
fn goto_line_semantics() {
    let mut b = buffer("one\ntwo\nthree");
    b.goto_line(99);
    assert_eq!(b.cursor_position(), (2, 5, 13).into_cursor());
    b.goto_line(1);
    assert_eq!(b.cursor_position(), (1, 0, 4).into_cursor());
}

#[test]
fn get_eol() {
    let mut b = buffer("one\ntwo\nthree");
    b.set_cursor(0, 1);
    let eol = b.get_eol();
    assert_eq!(eol.row, 0);
    assert_eq!(eol.col, 3);
    assert_eq!(eol.offset, 3);
    // already at EOL stays
    b.set_cursor(2, 5);
    let eol = b.get_eol();
    assert_eq!(eol.col, 5);
}

// --- insertion (edit-buffer.test.ts:204-259) ----------------------------------

#[test]
fn insert_text_positions() {
    let mut b = buffer("hello");
    b.set_cursor(0, 5);
    b.insert_text(" world");
    assert_eq!(b.get_text(), "hello world");
    assert_eq!(b.cursor_position(), (0, 11, 11).into_cursor());
    // middle insert
    let mut b = buffer("hello world");
    b.set_cursor(0, 5);
    b.insert_text(",");
    assert_eq!(b.get_text(), "hello, world");
    assert_eq!(b.cursor_position().col, 6);
    // multi-line insert: cursor below the block
    let mut b = buffer("abcdef");
    b.set_cursor(0, 3);
    b.insert_text("X\nY\nZ");
    assert_eq!(b.get_text(), "abcX\nY\nZdef");
    assert_eq!(b.cursor_position(), (2, 1, 8).into_cursor());
    // insert into empty buffer
    let mut b = EditBuffer::new();
    b.insert_text("hi");
    assert_eq!(b.get_text(), "hi");
    assert_eq!(b.cursor_position(), (0, 2, 2).into_cursor());
    // newline at BOL / EOL
    let mut b = buffer("ab");
    b.set_cursor(0, 0);
    b.new_line();
    assert_eq!(b.get_text(), "\nab");
    let mut b = buffer("ab");
    b.set_cursor(0, 2);
    b.new_line();
    assert_eq!(b.get_text(), "ab\n");
    assert_eq!(b.line_count(), 2);
}

#[test]
fn insert_char_routes_to_insert_text() {
    let mut b = buffer("a");
    b.set_cursor(0, 1);
    b.insert_char("🌟");
    assert_eq!(b.get_text(), "a🌟");
    assert_eq!(b.cursor_position().col, 3);
}

// --- deletion (edit-buffer.test.ts:261-364, 709-767) --------------------------

#[test]
fn delete_char_and_backspace() {
    let mut b = buffer("abc");
    b.set_cursor(0, 1);
    b.delete_forward();
    assert_eq!(b.get_text(), "ac");
    // backspace mid-line
    let mut b = buffer("abc");
    b.set_cursor(0, 2);
    b.backspace();
    assert_eq!(b.get_text(), "ac");
    assert_eq!(b.cursor_position().col, 1);
    // backspace at BOL merges lines
    let mut b = buffer("hello\nworld");
    b.set_cursor(1, 0);
    b.backspace();
    assert_eq!(b.get_text(), "helloworld");
    assert_eq!(b.cursor_position(), (0, 5, 5).into_cursor());
    // delete at EOL joins
    let mut b = buffer("ab\ncd");
    b.set_cursor(0, 2);
    b.delete_forward();
    assert_eq!(b.get_text(), "abcd");
    assert_eq!(b.cursor_position(), (0, 2, 2).into_cursor());
    // (0,0) backspace no-op
    let mut b = buffer("abc");
    b.set_cursor(0, 0);
    b.backspace();
    assert_eq!(b.get_text(), "abc");
}

#[test]
fn delete_range_matrix() {
    // within one line
    let mut b = buffer("hello world");
    b.delete_range(0, 5, 0, 11);
    assert_eq!(b.get_text(), "hello");
    // across lines (exclusive end — upstream "AAAA\nBBBB\nCCCC"
    // deleteRange(0,2,2,2) → "AACC")
    let mut b = buffer("AAAA\nBBBB\nCCCC");
    b.delete_range(0, 2, 2, 2);
    assert_eq!(b.get_text(), "AACC");
    // start == end no-op
    let mut b = buffer("abc");
    b.delete_range(0, 1, 0, 1);
    assert_eq!(b.get_text(), "abc");
    // reversed order normalizes
    let mut b = buffer("abcdef");
    b.delete_range(0, 4, 0, 2);
    assert_eq!(b.get_text(), "abef");
    // entire content (single line, col == width — upstream :320)
    let mut b = buffer("Hello World");
    b.delete_range(0, 0, 0, 11);
    assert_eq!(b.get_text(), "");
    // full multi-line range
    let mut b = buffer("Line 1\nLine 2");
    b.delete_range(0, 0, 1, 6);
    assert_eq!(b.get_text(), "");
    assert_eq!(b.line_count(), 1);
    // unicode range: 你 (2 cells) + the space at col 4
    let mut b = buffer("a 你 b");
    b.delete_range(0, 2, 0, 5);
    assert_eq!(b.get_text(), "a b");
}

#[test]
fn delete_line_matrix() {
    // middle line
    let mut b = buffer("one\ntwo\nthree");
    b.set_cursor(1, 1);
    b.delete_line();
    assert_eq!(b.get_text(), "one\nthree");
    assert_eq!(b.cursor_position(), (1, 0, 4).into_cursor());
    // last line
    let mut b = buffer("one\ntwo\nthree");
    b.set_cursor(2, 2);
    b.delete_line();
    assert_eq!(b.get_text(), "one\ntwo");
    assert_eq!(b.cursor_position(), (1, 3, 7).into_cursor());
    // only line
    let mut b = buffer("only");
    b.delete_line();
    assert_eq!(b.get_text(), "");
    assert_eq!(b.line_count(), 1);
    assert_eq!(b.cursor_position().row, 0);
}

// --- offsets (edit-buffer.test.ts:567-631) ------------------------------------

#[test]
fn offset_position_round_trips() {
    let b = buffer("abc\nde\nf");
    // oracle matrix
    let expected = [
        (0u32, 0u32),
        (0, 1),
        (0, 2),
        (0, 3),
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
    ];
    for (offset, (row, col)) in expected.iter().enumerate() {
        let pos = b.offset_to_coords(offset as u32).expect("valid offset");
        assert_eq!((pos.0, pos.1), (*row, *col), "offset {offset}");
        assert_eq!(b.coords_to_offset(*row, *col), Some(offset as u32));
    }
    assert_eq!(b.offset_to_coords(9), None);
    assert_eq!(b.get_line_start_offset(1), 4);
    assert_eq!(b.get_line_start_offset(2), 7);
}

#[test]
fn empty_text_offset_conversions() {
    let mut b = EditBuffer::new();
    b.set_text("");
    assert_eq!(b.offset_to_coords(0), Some((0, 0)));
    assert_eq!(b.offset_to_coords(1), None);
}

// --- ranges (edit-buffer.test.ts:425-448 + oracle matrix) ----------------------

#[test]
fn get_text_range_snapping() {
    let b = buffer("ab 你 cd");
    // oracle matrix (你 spans cols 3-4)
    assert_eq!(b.get_text_range(0, 3), "ab ");
    assert_eq!(b.get_text_range(2, 5), " 你");
    assert_eq!(b.get_text_range(3, 8), "你 cd");
    assert_eq!(b.get_text_range(0, 8), "ab 你 cd");
    assert_eq!(b.get_text_range(5, 5), "");
    assert_eq!(b.get_text_range(6, 99), "cd");
    assert_eq!(b.get_text_range(9, 12), "");
}

#[test]
fn get_text_range_multiline() {
    let b = buffer("abc\nde\nf");
    assert_eq!(b.get_text_range(2, 6), "c\nde");
    assert_eq!(b.get_text_range(3, 4), "\n");
    assert_eq!(b.get_text_range(0, 9), "abc\nde\nf");
    assert_eq!(b.get_text_range_by_coords(0, 2, 1, 1), "c\nd");
}

// --- word boundaries (edit-buffer.test.ts:477-566 + oracle matrix) -------------

/// The oracle matrix: for every cursor column, (next, prev) word
/// boundary columns.
fn assert_word_matrix(text: &str, matrix: &[(u32, u32, u32)]) {
    let mut b = buffer(text);
    for (col, next, prev) in matrix {
        b.set_cursor(0, *col);
        assert_eq!(
            b.get_next_word_boundary().col,
            *next,
            "{text:?} next from col {col}"
        );
        assert_eq!(
            b.get_prev_word_boundary().col,
            *prev,
            "{text:?} prev from col {col}"
        );
    }
}

#[test]
fn word_boundaries_simple() {
    // oracle: "hello world foo" — cols 0..17 (clamped)
    assert_word_matrix(
        "hello world foo",
        &[
            (0, 6, 0),
            (5, 12, 0),
            (6, 12, 0),
            (7, 12, 6),
            (11, 15, 6),
            (12, 15, 6),
            (13, 15, 12),
            (15, 15, 12),
            (17, 15, 12),
        ],
    );
}

#[test]
fn word_boundaries_punctuation() {
    // oracle: "hello-world test" (the '-' is a Punctuation break)
    assert_word_matrix(
        "hello-world test",
        &[
            (0, 6, 0),
            (5, 12, 0),
            (6, 12, 0),
            (7, 12, 6),
            (11, 16, 6),
            (12, 16, 6),
            (13, 16, 12),
            (16, 16, 12),
            (18, 16, 12),
        ],
    );
    // oracle: "foo.bar baz" ('.' at col 3 is a break; on it → skip to next)
    assert_word_matrix(
        "foo.bar baz",
        &[
            (0, 4, 0),
            (3, 8, 0),
            (4, 8, 0),
            (5, 8, 4),
            (7, 11, 4),
            (8, 11, 4),
            (9, 11, 8),
            (11, 11, 8),
        ],
    );
}

#[test]
fn word_boundaries_cjk_and_emoji() {
    // oracle: "你 好" — 你=2 cells, space, 好=2 cells
    assert_word_matrix(
        "你 好",
        &[(0, 3, 0), (2, 5, 0), (3, 5, 0), (4, 5, 3), (5, 5, 3)],
    );
    // oracle: "🌟 ok"
    assert_word_matrix(
        "🌟 ok",
        &[(0, 3, 0), (2, 5, 0), (3, 5, 0), (4, 5, 3), (5, 5, 3)],
    );
    // oracle: "a日b 语" — script transitions are boundaries; CJK
    // codepoints count as word codepoints for the at-cursor check
    assert_word_matrix(
        "a日b 语",
        &[
            (0, 1, 0),
            (1, 3, 0),
            (2, 5, 1),
            (3, 5, 1),
            (4, 7, 3),
            (5, 7, 3),
            (6, 7, 5),
            (7, 7, 5),
        ],
    );
}

#[test]
fn word_boundaries_tabs() {
    // oracle: "Hello\tWorld" with tab = 2 cols (break at cols 5-7)
    assert_word_matrix(
        "Hello\tWorld",
        &[
            (0, 7, 0),
            (4, 7, 0),
            (5, 12, 0),
            (6, 12, 0),
            (7, 12, 0),
            (8, 12, 7),
            (12, 12, 7),
        ],
    );
}

#[test]
fn word_boundaries_across_lines() {
    let mut b = buffer("hello\nworld");
    b.set_cursor(0, 5);
    let next = b.get_next_word_boundary();
    assert_eq!(next.row, 1);
    assert_eq!(next.col, 0);
    b.set_cursor(1, 0);
    let prev = b.get_prev_word_boundary();
    assert_eq!(prev.row, 0);
    assert_eq!(prev.col, 5);
}

// --- history (edit-buffer.test.ts:1297-1535 + oracle matrix) -------------------

#[test]
fn undo_redo_after_insert() {
    let mut b = buffer("hello");
    b.set_cursor(0, 5);
    b.insert_text(" world");
    // oracle: undo returns the PRE-mutation cursor meta
    assert_eq!(b.undo().as_deref(), Some("cursor:0:5:5"));
    assert_eq!(b.get_text(), "hello");
    assert!(!b.can_undo());
    assert!(b.can_redo());
    // nothing further to undo
    assert_eq!(b.undo(), None);
    assert_eq!(b.redo().as_deref(), Some("cursor:0:11:11"));
    assert_eq!(b.get_text(), "hello world");
    assert_eq!(b.cursor_position().col, 11);
}

#[test]
fn redo_cleared_by_new_edit() {
    let mut b = buffer("hello");
    b.set_cursor(0, 5);
    b.insert_text(" world");
    b.undo();
    assert!(b.can_redo());
    assert_eq!(b.redo().as_deref(), Some("cursor:0:11:11"));
    assert_eq!(b.get_text(), "hello world");
    b.insert_text("!!");
    assert!(!b.can_redo(), "store_undo clears the redo stack");
    // oracle: undo chain — the current-state metas surface
    assert_eq!(b.undo().as_deref(), Some("cursor:0:11:11"));
    assert_eq!(b.get_text(), "hello world");
    assert_eq!(b.undo().as_deref(), Some("cursor:0:5:5"));
    assert_eq!(b.get_text(), "hello");
    assert_eq!(b.undo(), None);
}

#[test]
fn delete_char_double_stores_undo() {
    // upstream deleteForward calls autoStoreUndo AND deleteRange stores
    // again — two identical snapshots; the second undo is a same-state
    // restore (oracle-verified)
    let mut b = buffer("abc");
    b.set_cursor(0, 1);
    b.delete_forward();
    assert_eq!(b.get_text(), "ac");
    assert!(b.can_undo());
    assert_eq!(b.undo().as_deref(), Some("cursor:0:1:1"));
    assert_eq!(b.get_text(), "abc");
    assert!(b.can_undo(), "the second stored snapshot remains");
    assert_eq!(b.undo().as_deref(), Some("cursor:0:1:1"));
    assert_eq!(b.get_text(), "abc");
}

#[test]
fn set_text_clears_history_replace_text_keeps() {
    let mut b = buffer("hello");
    b.set_cursor(0, 5);
    b.insert_text(" world");
    b.set_text("fresh");
    assert!(!b.can_undo());
    assert!(!b.can_redo());
    let mut b = buffer("hello");
    b.replace_text("bye");
    assert!(b.can_undo(), "replaceText creates an undo point");
    assert_eq!(b.undo().as_deref(), Some("cursor:0:0:0"));
    assert_eq!(b.get_text(), "hello");
    assert_eq!(b.redo().as_deref(), Some("cursor:0:0:0"));
    assert_eq!(b.get_text(), "bye");
}

#[test]
fn undo_restores_cursor_mid_line() {
    let mut b = buffer("hello");
    b.set_cursor(0, 2);
    b.insert_text("XX");
    b.undo();
    assert_eq!(b.cursor_position(), (0, 2, 2).into_cursor());
    b.redo();
    assert_eq!(b.cursor_position(), (0, 4, 4).into_cursor());
}

#[test]
fn tab_width_remap_through_history() {
    // edit-buffer.test.ts:104-118
    let mut b = buffer("a\tb");
    b.set_cursor(0, 4);
    b.insert_text("x");
    b.set_tab_width(8);
    assert_eq!(b.tab_width(), 8);
    assert_eq!(b.cursor_position(), (0, 11, 11).into_cursor());
    assert_eq!(b.undo().as_deref(), Some("cursor:0:4:4"));
    assert_eq!(b.cursor_position(), (0, 10, 10).into_cursor());
    b.insert_text("y");
    assert_eq!(b.get_text(), "a\tby");
}

#[test]
fn clear_keeps_history() {
    let mut b = buffer("Hello World");
    b.clear();
    assert_eq!(b.get_text(), "");
    // rope.clear() keeps undo roots reachable
    let mut b = buffer("hello");
    b.set_cursor(0, 5);
    b.insert_text(" world");
    b.clear();
    assert_eq!(b.get_text(), "");
    // clear() stores no snapshot; undo pops the insert's snapshot
    b.undo();
    assert_eq!(b.get_text(), "hello");
}

// --- EditorView (editor-view.test.ts) ------------------------------------------

fn view(text: &str, w: u32, h: u32) -> EditorView {
    let buffer = buffer(text);
    EditorView::new(buffer, w, h)
}

#[test]
fn viewport_management() {
    let mut v = view("Line 1\nLine 2\nLine 3", 40, 10);
    let vp = v.get_viewport();
    assert_eq!(
        (vp.width, vp.height, vp.offset_x, vp.offset_y),
        (40, 10, 0, 0)
    );
    v.set_viewport_size(80, 20);
    let vp = v.get_viewport();
    assert_eq!((vp.width, vp.height), (80, 20));
    v.set_scroll_margin(0.2);
    // vline count for simple text (no wrap at width 80)
    v.set_wrap_mode(WrapMode::None);
    assert_eq!(v.total_virtual_line_count(), 3);
}

#[test]
fn wrapping_modes() {
    let mut v = view(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ABCDEFGHIJKLMNOPQRST",
        10,
        10,
    );
    v.set_wrap_mode(WrapMode::None);
    assert_eq!(v.total_virtual_line_count(), 1);
    // oracle: the 50-char text (three over-wide words) wraps to 6
    // vlines in BOTH char and word mode
    v.set_wrap_mode(WrapMode::Char);
    assert_eq!(v.total_virtual_line_count(), 6);
    v.set_wrap_mode(WrapMode::Word);
    assert_eq!(v.total_virtual_line_count(), 6);
}

#[test]
fn viewport_slicing() {
    let text = (0..20)
        .map(|i| format!("Line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut v = view(&text, 40, 5);
    v.set_wrap_mode(WrapMode::None);
    assert_eq!(v.total_virtual_line_count(), 20);
    assert_eq!(v.get_virtual_line_count(), 5);
    v.set_viewport(0, 10, 40, 5, false);
    assert_eq!(v.get_virtual_line_count(), 5);
}

#[test]
fn cursor_visibility_scrolling() {
    // oracle: 20 lines, viewport 40x5 (margin 0.2 → 1 line band)
    let text = (0..20)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut v = view(&text, 40, 5);
    v.set_scroll_margin(0.2);
    v.set_wrap_mode(WrapMode::None);
    v.buffer.set_cursor(10, 0);
    // the cursor-changed pump (onCursorChanged → ensureCursorVisible)
    // runs at the next view entry point
    let vc = v.get_visual_cursor();
    assert_eq!(
        v.get_viewport().offset_y,
        7,
        "cursor line 10 scrolls to y=7"
    );
    assert_eq!(vc.visual_row, 3);
    v.buffer.set_cursor(1, 0);
    v.get_visual_cursor();
    assert_eq!(v.get_viewport().offset_y, 0);
    v.buffer.set_cursor(3, 0);
    v.get_visual_cursor();
    assert_eq!(
        v.get_viewport().offset_y,
        0,
        "line 3 stays inside the margin band"
    );
}

#[test]
fn make_cursor_visible_moves_cursor() {
    // setViewport(moveCursor=true) pulls the CURSOR into view
    // (editor-view.zig:155-194); oracle: viewport y=2, cursor at row 0
    // → cursor moves to row 3 col 0
    let text = (0..6)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut v = view(&text, 24, 3);
    v.set_wrap_mode(WrapMode::None);
    v.set_viewport(0, 2, 24, 3, true);
    assert_eq!(v.buffer.primary_cursor().row, 3);
    assert_eq!(v.buffer.primary_cursor().offset, 9);
}

#[test]
fn visual_cursor_over_wrapped_lines() {
    // oracle: "the quick brown fox jumps over" at width 10 word wrap
    // vlines: "the quick " (10, trailing space) / "brown fox " (10) /
    // "jumps over"; source_col_start of vline 1 is 10
    let mut v = view("the quick brown fox jumps over", 10, 8);
    v.set_wrap_mode(WrapMode::Word);
    assert_eq!(v.total_virtual_line_count(), 3);
    v.buffer.set_cursor_by_offset(0);
    assert_eq!(v.get_visual_cursor().visual_row, 0);
    v.buffer.set_cursor_by_offset(6);
    assert_eq!(v.get_visual_cursor().visual_row, 0);
    assert_eq!(v.get_visual_cursor().visual_col, 6);
    v.buffer.set_cursor_by_offset(12);
    assert_eq!(v.get_visual_cursor().visual_row, 1);
    assert_eq!(v.get_visual_cursor().visual_col, 2);
    v.buffer.set_cursor_by_offset(18);
    assert_eq!(v.get_visual_cursor().visual_row, 1);
    assert_eq!(v.get_visual_cursor().visual_col, 8);
}

#[test]
fn visual_up_down_through_wrap() {
    let mut v = view("the quick brown fox jumps over", 10, 8);
    v.set_wrap_mode(WrapMode::Word);
    // oracle: moveUpVisual from (0,9) stays (row 0 is the first)
    v.buffer.set_cursor(0, 9);
    v.move_up_visual();
    assert_eq!(v.buffer.primary_cursor(), (0, 9, 9).into_cursor_tuple());
    // oracle: moveDownVisual from (0,11) → (0,21): desired col 11
    // clamps to vline 1's last cell (col 10 → 9), lands at logical 19?
    // oracle says col 21 — desired 11 on vline 2 ("jumps over" width 11)
    v.buffer.set_cursor(0, 11);
    v.move_down_visual();
    assert_eq!(v.buffer.primary_cursor(), (0, 21, 21).into_cursor_tuple());
}

trait CursorTuple2 {
    fn into_cursor_tuple(self) -> crate::editor::Cursor;
}
impl CursorTuple2 for (u32, u32, u32) {
    fn into_cursor_tuple(self) -> crate::editor::Cursor {
        crate::editor::Cursor {
            row: self.0,
            col: self.1,
            desired_col: self.1,
            offset: self.2,
        }
    }
}

#[test]
fn visual_line_end_and_sol() {
    let mut v = view("the quick brown fox jumps over", 10, 8);
    v.set_wrap_mode(WrapMode::Word);
    // oracle: gotoVisualLineEnd from (0,2) → col 9 (last cell of vline 0)
    v.buffer.set_cursor(0, 2);
    v.goto_visual_line_end();
    assert_eq!(v.buffer.primary_cursor().col, 9);
    // oracle: from (0,15) → col 19 (last cell of vline 1)
    v.buffer.set_cursor(0, 15);
    v.goto_visual_line_end();
    assert_eq!(v.buffer.primary_cursor().col, 19);
    // visual SOL of vline 1
    v.buffer.set_cursor(0, 15);
    let sol = v.get_visual_sol();
    assert_eq!((sol.logical_row, sol.logical_col, sol.offset), (0, 10, 10));
}

// --- selection (editor-view.test.ts:174-309 + oracle matrix) --------------------

#[test]
fn set_update_reset_selection() {
    let mut v = view("hello world foo\nsecond line here\nthird", 40, 10);
    v.set_wrap_mode(WrapMode::None);
    v.set_selection(2, 8, None, None);
    assert_eq!(v.get_selection(), Some((2, 8)));
    assert_eq!(v.get_selected_text(), "llo wo");
    v.update_selection(5, None, None);
    assert_eq!(v.get_selection(), Some((2, 5)));
    v.reset_selection();
    assert_eq!(v.get_selection(), None);
    assert_eq!(v.get_selected_text(), "");
}

#[test]
fn local_selection_and_updates() {
    // editor-view.test.ts:221-278 (the "Hello World" matrix)
    let mut v = view("Hello World", 40, 10);
    v.set_wrap_mode(WrapMode::None);
    // anchor (0,0) focus (5,0): cell occupancy includes the cell under
    // the focus → "Hello "
    assert!(v.set_local_selection(
        (0, 0),
        (5, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert_eq!(v.get_selected_text(), "Hello ");
    // extend to (11,0) — the EOL focus is exclusive-end
    assert!(v.update_local_selection(
        (0, 0),
        (11, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert_eq!(v.get_selected_text(), "Hello World");
    // shrink to (3,0) — "Hell"
    assert!(v.update_local_selection(
        (0, 0),
        (3, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert_eq!(v.get_selected_text(), "Hell");
    // backward selection: anchor (11,0) focus (6,0) → "World"
    assert!(v.set_local_selection(
        (11, 0),
        (11, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert!(v.update_local_selection(
        (11, 0),
        (6, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert_eq!(v.get_selected_text(), "World");
}

#[test]
fn local_selection_word_behavior() {
    let mut v = view("alpha beta gamma", 40, 5);
    v.set_wrap_mode(WrapMode::None);
    assert!(v.set_local_selection(
        (6, 0),
        (6, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Word
    ));
    assert_eq!(v.get_selected_text(), "beta");
}

#[test]
fn local_selection_wrapped_lines() {
    // oracle: "the quick brown fox jumps over" width 10 word wrap;
    // anchor (3,0) focus (2,2): vline 2 is below → textEnd
    let mut v = view("the quick brown fox jumps over", 10, 8);
    v.set_wrap_mode(WrapMode::Word);
    assert!(v.set_local_selection(
        (3, 0),
        (2, 2),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert_eq!(v.get_selection(), Some((3, 23)));
    assert_eq!(v.get_selected_text(), " quick brown fox jum");
    // focus back to (0,1) — vline 1's first cell
    assert!(v.update_local_selection(
        (3, 0),
        (0, 1),
        (None, None),
        false,
        false,
        SelectionBehavior::Cell
    ));
    assert_eq!(v.get_selection(), Some((3, 11)));
    assert_eq!(v.get_selected_text(), " quick b");
    // word behavior on the same cell
    v.set_local_selection(
        (6, 0),
        (6, 0),
        (None, None),
        false,
        false,
        SelectionBehavior::Word,
    );
    assert_eq!(v.get_selected_text(), "quick");
    // line behavior
    v.set_local_selection(
        (3, 0),
        (3, 1),
        (None, None),
        false,
        false,
        SelectionBehavior::Line,
    );
    assert_eq!(v.get_selected_text(), "the quick brown fox jumps over");
}

#[test]
fn selection_deletion() {
    let mut v = view("the quick brown fox jumps over", 10, 8);
    v.set_wrap_mode(WrapMode::Word);
    v.set_selection(4, 12, None, None);
    assert_eq!(v.get_selected_text(), "quick br");
    v.delete_selected_text();
    assert_eq!(v.buffer.get_text(), "the own fox jumps over");
    assert_eq!(v.buffer.primary_cursor().col, 4);
    assert_eq!(v.get_selection(), None);
}

#[test]
fn selection_inclusive_and_occupancy() {
    let mut v = view("abcd", 10, 4);
    v.set_wrap_mode(WrapMode::None);
    // cell occupancy extends the end over the grapheme at it
    v.set_selection_inclusive(1, 3, None, None);
    assert_eq!(v.get_selection(), Some((1, 4)));
    assert_eq!(v.get_selected_text(), "bcd");
    // boundary occupancy writes [start, end) as-is
    v.set_selection_occupancy(SelectionOccupancy::Boundary);
    v.set_selection_inclusive(1, 3, None, None);
    assert_eq!(v.get_selection(), Some((1, 3)));
    v.set_selection_occupancy(SelectionOccupancy::Cell);
}

// --- keybinding (keybinding.internal.test.ts) -----------------------------------

#[test]
fn key_binding_keys_and_lookup() {
    let a = KeyBindingLike::with_modifiers("y", true, true, false, false);
    assert_eq!(get_key_binding_key(&a), "y:1:1:0:0");
    let b = KeyBindingLike::with_modifiers("y", false, true, false, false);
    assert_ne!(get_key_binding_key(&a), get_key_binding_key(&b));
    // meta modifier key
    let m = KeyBindingLike::with_modifiers("y", false, false, true, false);
    assert_eq!(get_key_binding_key(&m), "y:0:0:1:0");
    let s = KeyBindingLike::with_modifiers("y", false, false, false, true);
    assert_eq!(get_key_binding_key(&s), "y:0:0:0:1");

    // baseCode fallback (kitty base layout): ㅊ (U+CE8A) with baseCode 99
    let mut map = std::collections::BTreeMap::new();
    map.insert("c:1:0:0:0".to_owned(), "copy".to_owned());
    let key = KeyEvent {
        name: "ㅊ".into(),
        ctrl: true,
        base_code: Some(99),
        ..Default::default()
    };
    assert_eq!(get_key_binding_action(&map, &key).as_deref(), Some("copy"));
    // direct name match wins over baseCode
    let mut map = std::collections::BTreeMap::new();
    map.insert("ㅊ:1:0:0:0".to_owned(), "direct".to_owned());
    map.insert("c:1:0:0:0".to_owned(), "basecode".to_owned());
    assert_eq!(
        get_key_binding_action(&map, &key).as_deref(),
        Some("direct")
    );
}

#[test]
fn merge_bindings_override() {
    let defaults = vec![
        KeyBinding::new("a", true, false, false, false, "default-a"),
        KeyBinding::new("b", false, false, false, false, "default-b"),
    ];
    let custom = vec![KeyBinding::new("a", true, false, false, false, "custom-a")];
    let merged = merge_key_bindings(&defaults, &custom);
    let map = build_key_bindings_map(&merged, None);
    assert_eq!(map.get("a:1:0:0:0").map(String::as_str), Some("custom-a"));
    assert_eq!(map.get("b:0:0:0:0").map(String::as_str), Some("default-b"));
}

#[test]
fn alias_map() {
    let aliases = default_key_aliases();
    assert_eq!(aliases.get("enter").map(String::as_str), Some("return"));
    assert_eq!(aliases.get("esc").map(String::as_str), Some("escape"));
    assert_eq!(aliases.get("kpenter").map(String::as_str), Some("enter"));
    // custom aliases override; build map adds aliased entries
    let mut custom = std::collections::BTreeMap::new();
    custom.insert("ret".to_owned(), "return".to_owned());
    let merged_aliases = merge_key_aliases(aliases, custom);
    assert_eq!(
        merged_aliases.get("ret").map(String::as_str),
        Some("return")
    );
    // a binding named with the canonical name does NOT gain alias
    // entries (keybinding.internal.test.ts:124-131)
    let bindings = vec![KeyBinding::new(
        "return", false, false, false, false, "submit",
    )];
    let map = build_key_bindings_map(&bindings, Some(&merged_aliases));
    assert_eq!(
        map.get("return:0:0:0:0").map(String::as_str),
        Some("submit")
    );
    assert_eq!(map.get("enter:0:0:0:0"), None);
    // a binding NAMED with an alias gains the canonical entry (:133-142)
    let bindings = vec![KeyBinding::new(
        "enter", false, false, false, false, "submit",
    )];
    let map = build_key_bindings_map(&bindings, Some(&merged_aliases));
    assert_eq!(map.get("enter:0:0:0:0").map(String::as_str), Some("submit"));
    assert_eq!(
        map.get("return:0:0:0:0").map(String::as_str),
        Some("submit")
    );
    // the default textarea table's "kpenter" binding → "enter" resolves
    let map = build_key_bindings_map(&default_textarea_key_bindings(), Some(&merged_aliases));
    assert_eq!(
        map.get("enter:0:0:0:0").map(String::as_str),
        Some("newline")
    );
    assert_eq!(
        map.get("kpenter:0:0:0:0").map(String::as_str),
        Some("newline")
    );
}

#[test]
fn key_binding_stringifies() {
    assert_eq!(
        key_binding_to_string(&KeyBinding::new("y", true, true, false, false, "x")),
        "ctrl+shift+y"
    );
    assert_eq!(
        key_binding_to_string(&KeyBinding::new("escape", false, false, false, false, "x")),
        "escape"
    );
}

// --- the 36-action executor + textarea integration ------------------------------

fn textarea_renderer(initial: &str, w: u32, h: u32) -> crate::renderer::CliRenderer {
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: w,
        height: h,
        ..Default::default()
    });
    let opts = TextareaOptions {
        initial_value: Some(initial.to_owned()),
        width: Some(crate::layout::Dimension::Points(w as f32)),
        height: Some(crate::layout::Dimension::Points(h as f32)),
        ..Default::default()
    };
    let r = renderer.make_textarea("ta", &opts);
    renderer.add(r);
    renderer
}

#[test]
fn all_36_actions_execute() {
    // every edit-buffer action resolves and returns true on a live
    // textarea (the keymap command set, edit-buffer-bindings.ts:35-72)
    for action in TEXTAREA_ACTIONS {
        let mut renderer = textarea_renderer("hello world foo\nsecond line\nthird", 40, 10);
        let handled = renderer.textarea_action("ta", action);
        assert!(handled, "action {action} must be handled");
    }
}

#[test]
fn action_matrix_state_changes() {
    // motion actions
    let mut renderer = textarea_renderer("hello world\nsecond", 40, 6);
    for _ in 0..3 {
        renderer.textarea_action("ta", "move-right");
    }
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().col, 3);
    });
    renderer.textarea_action("ta", "line-end");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().col, 11);
    });
    // line-end twice at non-final line steps to next line BOL
    renderer.textarea_action("ta", "line-end");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().row, 1);
        assert_eq!(ta.logical_cursor().col, 0);
    });
    // line-home at col 0 goes to previous line EOL
    renderer.textarea_action("ta", "line-home");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().row, 0);
        assert_eq!(ta.logical_cursor().col, 11);
    });
    // buffer home/end
    renderer.textarea_action("ta", "buffer-end");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().offset, 18);
    });
    renderer.textarea_action("ta", "buffer-home");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().offset, 0);
    });
    // word motions
    renderer.textarea_action("ta", "word-forward");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().col, 6);
    });
    renderer.textarea_action("ta", "word-backward");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.logical_cursor().col, 0);
    });

    // delete actions
    let mut renderer = textarea_renderer("hello world", 40, 4);
    renderer.textarea_action("ta", "move-right");
    renderer.textarea_action("ta", "move-right");
    renderer.textarea_action("ta", "move-right");
    renderer.textarea_action("ta", "move-right");
    renderer.textarea_action("ta", "move-right");
    renderer.textarea_action("ta", "delete-to-line-end");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "hello");
    });
    renderer.textarea_action("ta", "backspace");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "hell");
    });
    renderer.textarea_action("ta", "delete");
    renderer.textarea_state_mut("ta", |ta| {
        // delete at EOL of the last line is a no-op
        assert_eq!(ta.plain_text(), "hell");
    });
    renderer.textarea_action("ta", "newline");
    renderer.textarea_action("ta", "insert-me-not");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "hell\n");
    });

    // word-wise deletes
    let mut renderer = textarea_renderer("one two three four", 40, 4);
    renderer.textarea_action("ta", "buffer-end");
    renderer.textarea_action("ta", "delete-word-backward");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "one two three ");
    });
    renderer.textarea_action("ta", "delete-word-backward");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "one two ");
    });
    renderer.textarea_action("ta", "buffer-home");
    renderer.textarea_action("ta", "delete-word-forward");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "two ");
    });

    // undo/redo actions
    renderer.textarea_action("ta", "undo");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "one two ");
    });
    renderer.textarea_action("ta", "redo");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "two ");
    });
    // delete-line
    renderer.textarea_action("ta", "delete-line");
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "");
    });
}

#[test]
fn select_actions_via_key_presses() {
    // the keyboard-selection path: shift-arrows through handleKeyPress
    // (oracle: 3× shift-right from (0,0) → selection [0,4) "hell",
    // cursor (0,3))
    let mut renderer = textarea_renderer("hello world foo\nsecond line", 10, 8);
    for _ in 0..3 {
        let key = KeyEvent {
            name: "right".into(),
            shift: true,
            ..Default::default()
        };
        assert!(renderer.textarea_key_press("ta", &key));
    }
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.get_selection(), Some((0, 4)));
        assert_eq!(ta.get_selected_text(), "hell");
        assert_eq!(ta.logical_cursor().col, 3);
    });
    // shift-down extends across the line end (oracle: [0,10) "hello worl")
    let key = KeyEvent {
        name: "down".into(),
        shift: true,
        ..Default::default()
    };
    assert!(renderer.textarea_key_press("ta", &key));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.get_selection(), Some((0, 10)));
        assert_eq!(ta.get_selected_text(), "hello worl");
    });
    // select-all (oracle: [0,27) whole buffer)
    let key = KeyEvent {
        name: "a".into(),
        super_: true,
        ..Default::default()
    };
    assert!(renderer.textarea_key_press("ta", &key));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.get_selection(), Some((0, 27)));
        assert_eq!(ta.get_selected_text(), "hello world foo\nsecond line");
    });
    // plain left collapses to the selection start without moving
    let key = KeyEvent::named("left");
    assert!(renderer.textarea_key_press("ta", &key));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.get_selection(), None);
        assert_eq!(ta.logical_cursor().offset, 0);
    });
}

#[test]
fn printable_key_insertion_path() {
    let mut renderer = textarea_renderer("", 40, 4);
    for ch in ["h", "i", " ", "x"] {
        let key = KeyEvent {
            name: ch.to_owned(),
            sequence: Some(ch.to_owned()),
            ..Default::default()
        };
        assert!(renderer.textarea_key_press("ta", &key));
    }
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "hi x");
    });
    // space key inserts via the name check
    let key = KeyEvent::named("space");
    assert!(renderer.textarea_key_press("ta", &key));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.plain_text(), "hi x ");
    });
    // control keys without bindings fall through (no insert)
    let key = KeyEvent {
        name: "c".into(),
        ctrl: true,
        ..Default::default()
    };
    assert!(!renderer.textarea_key_press("ta", &key));
    // control codes never insert
    let key = KeyEvent {
        name: "x".into(),
        sequence: Some("\x1b".into()),
        ..Default::default()
    };
    assert!(!renderer.textarea_key_press("ta", &key));
    // del (127) never inserts
    let key = KeyEvent {
        name: "x".into(),
        sequence: Some("\x7f".into()),
        ..Default::default()
    };
    assert!(!renderer.textarea_key_press("ta", &key));
}

#[test]
fn default_bindings_table_shape() {
    // spot-check the merged default table (Textarea.ts:58-128)
    let map = build_key_bindings_map(&default_textarea_key_bindings(), None);
    assert_eq!(
        map.get("left:0:0:0:0").map(String::as_str),
        Some("move-left")
    );
    assert_eq!(
        map.get("left:0:1:0:0").map(String::as_str),
        Some("select-left")
    );
    assert_eq!(map.get("a:1:0:0:0").map(String::as_str), Some("line-home"));
    assert_eq!(map.get("e:1:0:0:0").map(String::as_str), Some("line-end"));
    assert_eq!(
        map.get("a:0:0:1:0").map(String::as_str),
        Some("visual-line-home")
    );
    assert_eq!(
        map.get("return:0:0:0:0").map(String::as_str),
        Some("newline")
    );
    assert_eq!(
        map.get("return:0:0:1:0").map(String::as_str),
        Some("submit")
    );
    assert_eq!(map.get("z:0:0:0:1").map(String::as_str), Some("undo"));
    assert_eq!(map.get("z:0:1:0:1").map(String::as_str), Some("redo"));
    assert_eq!(
        map.get("w:1:0:0:0").map(String::as_str),
        Some("delete-word-backward")
    );
    assert_eq!(map.get("a:0:0:0:1").map(String::as_str), Some("select-all"));
    // enter alias resolves newline via the alias map
    let aliases = default_key_aliases();
    let map = build_key_bindings_map(&default_textarea_key_bindings(), Some(&aliases));
    assert_eq!(
        map.get("enter:0:0:0:0").map(String::as_str),
        Some("newline")
    );
}

// --- placeholder + scrollY + cursor emission -----------------------------------

#[test]
fn placeholder_lifecycle() {
    let mut renderer = textarea_renderer("", 24, 4);
    renderer.textarea_state_mut("ta", |ta| {
        ta.set_placeholder(Some(vec![crate::styled_text::TextChunk {
            text: "Type here".into(),
            ..Default::default()
        }]));
    });
    renderer.render();
    renderer.textarea_state_mut("ta", |ta| {
        assert!(ta.view.placeholder_active());
        assert_eq!(ta.virtual_line_count(), 1);
    });
    // typing replaces the placeholder
    renderer.textarea_state_mut("ta", |ta| ta.insert_text("hi"));
    renderer.render();
    renderer.textarea_state_mut("ta", |ta| {
        assert!(!ta.view.placeholder_active());
        assert_eq!(ta.plain_text(), "hi");
    });
    // clearing brings it back
    renderer.textarea_state_mut("ta", |ta| ta.clear());
    renderer.render();
    renderer.textarea_state_mut("ta", |ta| {
        assert!(ta.view.placeholder_active());
    });
}

#[test]
fn scroll_y_and_follow() {
    let mut renderer = textarea_renderer("", 24, 4);
    let text = (0..10)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    renderer.textarea_state_mut("ta", |ta| ta.set_text(&text));
    renderer.refresh_textarea_measure("ta");
    renderer.render();
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.scroll_y(), 0);
        assert_eq!(ta.virtual_line_count(), 4);
    });
    // cursor to line 8 scrolls (oracle: scrollY=6 with margin 0.2)
    renderer.textarea_state_mut("ta", |ta| ta.set_cursor(8, 3));
    renderer.render();
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.scroll_y(), 6);
        let vc = ta.visual_cursor();
        assert_eq!(vc.logical_row, 8);
    });
    // handleScroll semantics: down clamps at total-height
    renderer.textarea_state_mut("ta", |ta| ta.scroll_by("down", 99));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.scroll_y(), 6);
    });
    renderer.textarea_state_mut("ta", |ta| ta.scroll_by("up", 2));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.scroll_y(), 4);
    });
    renderer.textarea_state_mut("ta", |ta| ta.scroll_by("up", 99));
    renderer.textarea_state_mut("ta", |ta| {
        assert_eq!(ta.scroll_y(), 0);
    });
}

#[test]
fn cursor_sgr_emission() {
    // focused textarea → the frame stream positions the terminal cursor
    // (renderer.zig:2932-3016): OSC12 color, DECSCUSR, CUP, show
    let mut renderer = textarea_renderer("hi", 20, 4);
    renderer.textarea_state_mut("ta", |ta| {
        ta.focus();
        ta.set_cursor(0, 1);
    });
    renderer.render();
    let sgr = renderer.sgr_stream().to_owned();
    assert!(sgr.contains("\x1b]12;#ffffff\x07"), "cursor color OSC12");
    assert!(sgr.contains("\x1b[1 q"), "block-blink DECSCUSR");
    assert!(sgr.contains("\x1b[1;2H"), "CUP at row 1 col 2");
    assert!(sgr.ends_with("\x1b[?25h"), "cursor shown");
    // second frame with no changes emits nothing new for the cursor
    renderer.render();
    assert_eq!(renderer.last_frame_sgr(), "");
    // blur hides it (setCursorPosition(0,0,false), EditBufferRenderable.ts:1030-1034)
    renderer.textarea_state_mut("ta", |ta| {
        ta.blur();
    });
    renderer.render();
    assert!(renderer.last_frame_sgr().contains("\x1b[?25l"));
}

// --- cursor meta round trip (edit-buffer.zig:54-94) ------------------------------

#[test]
fn cursor_meta_decode_round_trip() {
    // public form: cursor:row:col:desired (tab width optional tail)
    assert_eq!(
        crate::editor::testutil::decode_cursor_meta("cursor:0:4:4"),
        Some((0, 4, 4, None))
    );
    assert_eq!(
        crate::editor::testutil::decode_cursor_meta("cursor:2:10:7:8"),
        Some((2, 10, 7, Some(8)))
    );
    assert_eq!(
        crate::editor::testutil::decode_cursor_meta("cursor:0:4:4:8:2"),
        None
    );
    assert_eq!(
        crate::editor::testutil::decode_cursor_meta("nonsense"),
        None
    );
}

// --- keymap glue (edit-buffer-bindings.ts command set ↔ executor) --------------

#[test]
fn keymap_actions_match_executor_surface() {
    // The 36 edit-buffer action ids the keymap commands emit
    // (`opentui_keymap::textarea::EDIT_BUFFER_ACTIONS`, ported from
    // edit-buffer-bindings.ts:35-72) are exactly the ids
    // `TextareaState::execute_action` dispatches — the host applies a
    // sink-recorded action straight to its focused textarea.
    let keymap: Vec<&str> = opentui_keymap::textarea::EDIT_BUFFER_ACTIONS.to_vec();
    assert_eq!(keymap.len(), 36);
    assert_eq!(keymap.as_slice(), TEXTAREA_ACTIONS);
    // and the executor implements each one end-to-end (all_36_actions)
}

#[test]
fn keymap_action_applies_to_textarea() {
    // end-to-end glue: register the keymap's managed textarea commands,
    // dispatch a binding, take the recorded action, execute it
    let host = Box::new(opentui_keymap::testing::TestHost::new());
    let mut keymap = opentui_keymap::Keymap::new(host);
    let (_reg, sink) = opentui_keymap::textarea::register_managed_textarea_layer(
        &mut keymap,
        opentui_keymap::textarea::ManagedTextareaLayer {
            bindings: Vec::new(),
            priority: 0,
            mode: None,
        },
        None,
    );
    let mut event = opentui_keymap::types::KeymapEvent::new("left");
    let outcome = keymap.handle_key_event(&mut event);
    assert!(outcome.handled, "the managed layer handles plain left");
    let recorded = sink.lock().unwrap().clone();
    assert_eq!(recorded, vec!["move-left".to_owned()]);

    // the recorded action applies to a live textarea
    let mut renderer = textarea_renderer("hello world", 40, 4);
    renderer.textarea_state_mut("ta", |ta| {
        // park the cursor mid-line first
        ta.set_cursor(0, 5);
    });
    renderer.textarea_action("ta", &recorded[0]);
    renderer.textarea_state_mut("ta", |ta| {
        // move-left is one grapheme step: 5 → 4
        assert_eq!(ta.logical_cursor().col, 4);
    });
}
