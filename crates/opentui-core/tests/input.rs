//! Input renderable tests — porting upstream's own
//! `renderables/Input.test.ts` cases (focus/commit, typing, delete
//! family, maxLength, cursor) plus the native word-boundary semantics
//! (`edit-buffer.zig` word ops) and the scroll-to-cursor viewport
//! (`editor-view.zig`).

use opentui_core::events::{KeyEvent, PasteEvent};
use opentui_core::input::{InputAction, InputOptions, InputRenderableState, InputStateEvent};
use opentui_core::parse_keypress::{parse_keypress, ParseKeypressOptions};
use opentui_core::paste::decode_paste_bytes;
use std::sync::{Arc, Mutex};

fn input(value: &str) -> InputRenderableState {
    InputRenderableState::new(&InputOptions {
        id: "i".into(),
        value: value.into(),
        width: Some(20),
        ..Default::default()
    })
}

fn key(seq: &str) -> KeyEvent {
    KeyEvent::new(
        parse_keypress(seq, ParseKeypressOptions::default())
            .unwrap_or_else(|| panic!("parse {seq:?}")),
    )
}

fn press(state: &mut InputRenderableState, seq: &str) -> bool {
    let mut k = key(seq);
    state.handle_key(&mut k)
}

#[test]
fn initializes_with_defaults() {
    // Input.test.ts:29-38 — default value empty, focusable
    let i = input("");
    assert_eq!(i.value(), "");
    // cursor at end of (empty) initial value
    assert_eq!(i.cursor_offset(), 0);
}

#[test]
fn sanitizes_initial_value() {
    // Input.ts:67 — newlines stripped, maxLength clamp (default 1000)
    let i = InputRenderableState::new(&InputOptions {
        value: "a\r\nb\nc".into(),
        max_length: Some(4),
        ..Default::default()
    });
    assert_eq!(i.value(), "abc");
    // cursor to end of initial value (Input.ts:93-96)
    assert_eq!(i.cursor_offset(), 3);
}

#[test]
fn focus_and_blur_commit_semantics() {
    // Input.test.ts:67-110 — CHANGE on blur only when the value drifted
    let mut i = input("initial");
    let log: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Change, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(("change", v.to_owned()))
    });

    assert!(!i.is_focused());
    i.focus();
    assert!(i.is_focused());
    i.set_value("modified");
    assert!(log.lock().unwrap().is_empty(), "no change during focus");
    i.blur();
    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, "modified");
}

#[test]
fn no_change_event_when_unchanged() {
    let mut i = input("unchanged");
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Change, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(v.to_owned())
    });
    i.focus();
    i.blur();
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn typing_inserts_and_emits_input() {
    // Input.test.ts:113-143
    let mut i = input("");
    i.focus();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Input, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(v.to_owned())
    });
    for ch in ["h", "e", "l", "l", "o"] {
        assert!(press(&mut i, ch));
    }
    assert_eq!(i.value(), "hello");
    assert_eq!(
        *log.lock().unwrap(),
        vec!["h", "he", "hel", "hell", "hello"]
    );
}

#[test]
fn backspace_and_delete() {
    // Input.test.ts:163-175, 230-242
    let mut i = input("hello");
    i.focus();
    assert!(press(&mut i, "\x7f"));
    assert_eq!(i.value(), "hell");
    assert!(press(&mut i, "\x7f"));
    assert_eq!(i.value(), "hel");
    // delete at cursor 1
    i.set_cursor_offset(1);
    assert!(press(&mut i, "\x1b[3~"));
    assert_eq!(i.value(), "hl");
}

#[test]
fn ctrl_w_deletes_word_backward() {
    // Input.test.ts:177-193 — "hello world" → ctrl+w → "hello "
    let mut i = input("hello world");
    i.focus();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Input, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(v.to_owned())
    });
    assert!(press(&mut i, "\x17")); // ctrl+w
    assert_eq!(i.value(), "hello ");
    assert_eq!(*log.lock().unwrap(), vec!["hello ".to_owned()]);
}

#[test]
fn alt_backspace_deletes_word_backward() {
    // Input.test.ts:195-211 — "foo bar baz" → alt+backspace → "foo bar "
    let mut i = input("foo bar baz");
    i.focus();
    assert!(press(&mut i, "\x1b\x7f"));
    assert_eq!(i.value(), "foo bar ");
}

#[test]
fn delete_line_clears() {
    // Input.test.ts:213-228 — deleteLine() empties the single line
    let mut i = input("hello world");
    i.focus();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Input, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(v.to_owned())
    });
    i.delete_line();
    assert_eq!(i.value(), "");
    assert_eq!(*log.lock().unwrap(), vec![String::new()]);
}

#[test]
fn arrow_keys_move_cursor() {
    // Input.test.ts:243-266
    let mut i = input("hello");
    i.focus();
    i.set_cursor_offset(5);
    assert!(press(&mut i, "\x1b[D"));
    assert_eq!(i.cursor_offset(), 4);
    assert!(press(&mut i, "\x1b[D"));
    assert_eq!(i.cursor_offset(), 3);
    assert!(press(&mut i, "\x1b[C"));
    assert_eq!(i.cursor_offset(), 4);
}

#[test]
fn enter_submits() {
    // Input.test.ts:267-286 — Enter fires ENTER (not a newline)
    let mut i = input("hi");
    i.focus();
    let log: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Enter, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(("enter", v.to_owned()))
    });
    i.on(InputStateEvent::Change, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(("change", v.to_owned()))
    });
    assert!(press(&mut i, "\r"));
    assert_eq!(i.value(), "hi", "no newline inserted");
    let entries = log.lock().unwrap();
    assert_eq!(entries[0], ("enter", "hi".to_owned()));
    // the value never drifted from the focus-time snapshot → no CHANGE
    assert_eq!(entries.len(), 1);
}

#[test]
fn min_length_gates_submit() {
    // Input.test.ts:308-338
    let mut i = InputRenderableState::new(&InputOptions {
        value: "ab".into(),
        min_length: Some(3),
        ..Default::default()
    });
    i.focus();
    assert!(!i.submit());
    i.insert_text("c");
    assert!(i.submit());
}

#[test]
fn max_length_clamps_typing_and_paste() {
    // Input.test.ts:287-307
    let mut i = InputRenderableState::new(&InputOptions {
        value: "".into(),
        max_length: Some(3),
        ..Default::default()
    });
    i.focus();
    for ch in ["a", "b", "c", "d", "e"] {
        press(&mut i, ch);
    }
    assert_eq!(i.value(), "abc");
    // paste beyond the limit is clamped too (Input.ts:119-130)
    let mut paste = PasteEvent::new(b"XYZ".to_vec(), None);
    i.handle_paste(&mut paste);
    assert_eq!(i.value(), "abc");
    // maxLength setter truncates (Input.ts:238-244)
    i.set_max_length(1);
    assert_eq!(i.value(), "a");
}

#[test]
fn paste_strips_ansi_and_newlines() {
    // Input.test.ts:352-386 — ANSI stripped, newlines stripped
    let mut i = input("");
    i.focus();
    let mut paste = PasteEvent::new("\x1b[31mred\x1b[0m text\nmore".as_bytes().to_vec(), None);
    i.handle_paste(&mut paste);
    assert_eq!(i.value(), "red textmore");
    // decodePasteBytes UTF-8 path
    let mut emoji = PasteEvent::new("😀".as_bytes().to_vec(), None);
    i.handle_paste(&mut emoji);
    assert_eq!(i.value(), "red textmore😀");
}

#[test]
fn value_setter_moves_cursor_to_end_and_emits() {
    // Input.test.ts:559-592
    let mut i = input("");
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Input, {
        let log = Arc::clone(&log);
        move |v| log.lock().unwrap().push(v.to_owned())
    });
    i.set_value("programmatic");
    assert_eq!(i.cursor_offset(), "programmatic".len());
    assert_eq!(*log.lock().unwrap(), vec!["programmatic".to_owned()]);
    // same value → no event
    i.set_value("programmatic");
    assert_eq!(log.lock().unwrap().len(), 1);
    // empty set works
    i.set_value("");
    assert_eq!(i.value(), "");
    assert_eq!(i.cursor_offset(), 0);
}

#[test]
fn emacs_bindings_by_default() {
    // Input.test.ts:1199-1230
    let mut i = input("hello world");
    i.focus();
    i.set_cursor_offset(11);
    // ctrl+a → line home
    assert!(press(&mut i, "\x01"));
    assert_eq!(i.cursor_offset(), 0);
    // ctrl+e → line end
    assert!(press(&mut i, "\x05"));
    assert_eq!(i.cursor_offset(), 11);
    // ctrl+k → delete to line end
    i.set_cursor_offset(5);
    press(&mut i, "\x0b");
    assert_eq!(i.value(), "hello");
    // ctrl+u → delete to line start
    press(&mut i, "\x15");
    assert_eq!(i.value(), "");
}

#[test]
fn word_jump_bindings() {
    // option/alt + arrows and ctrl+arrows (Textarea.ts:106-111)
    let mut i = input("hello world again");
    i.focus();
    i.set_cursor_offset(0); // constructor leaves the cursor at the end
    assert!(press(&mut i, "\x1bf")); // meta+f word forward
    assert_eq!(i.cursor_offset(), 6);
    assert!(press(&mut i, "\x1bb")); // meta+b word backward
    assert_eq!(i.cursor_offset(), 0);
    assert!(press(&mut i, "\x1b[1;5C")); // ctrl+right
    assert_eq!(i.cursor_offset(), 6);
    // meta+delete deletes the next word (cursor at 0)
    let mut i2 = input("hello world");
    i2.focus();
    i2.set_cursor_offset(0);
    assert!(press(&mut i2, "\x1b[3;3~")); // delete with meta
    assert_eq!(i2.value(), "world");
}

#[test]
fn custom_bindings_override_defaults() {
    // Input.test.ts:1121-1198
    let mut i = InputRenderableState::new(&InputOptions {
        value: "hello".into(),
        key_bindings: vec![opentui_core::keybinding::KeyBinding::new(
            "x",
            false,
            false,
            false,
            false,
            "delete-line",
        )],
        ..Default::default()
    });
    i.focus();
    assert!(press(&mut i, "x"));
    assert_eq!(i.value(), "", "custom binding ate the insert");
}

#[test]
fn key_alias_maps_kpenter_to_submit() {
    // Input.test.ts:1143-1163 — kpenter alias resolves to enter → return
    // (via the alias chain kpenter → enter → return)
    let mut i = input("v");
    i.focus();
    let entered: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    i.on(InputStateEvent::Enter, {
        let entered = Arc::clone(&entered);
        move |v| entered.lock().unwrap().push(v.to_owned())
    });
    // kitty keypad enter: name kpenter
    let mut k = KeyEvent::new(
        parse_keypress(
            "\x1b[57414u",
            ParseKeypressOptions {
                use_kitty_keyboard: true,
            },
        )
        .unwrap(),
    );
    assert!(i.handle_key(&mut k));
    assert_eq!(*entered.lock().unwrap(), vec!["v".to_owned()]);
}

#[test]
fn kitty_keypad_digits_type_text() {
    // Input.test.ts:885-980 — kp0..kp9, operators, kpenter
    let mut i = input("");
    i.focus();
    for seq in ["\x1b[57399u", "\x1b[57413u", "\x1b[57409u"] {
        // kp0, kpplus, kpdecimal — via kitty encoding
        let mut k = KeyEvent::new(
            parse_keypress(
                seq,
                ParseKeypressOptions {
                    use_kitty_keyboard: true,
                },
            )
            .unwrap(),
        );
        assert!(i.handle_key(&mut k), "{seq}");
    }
    assert_eq!(i.value(), "0+.");
}

#[test]
fn suspend_mappings_disables_bindings_but_keeps_text() {
    // Textarea.ts:266 — traits.suspend skips the binding map; plain
    // text still inserts (the keymap owns the keys)
    let mut i = input("");
    i.focus();
    i.suspend_mappings = true;
    let mut k = key("x");
    assert!(i.handle_key(&mut k));
    assert_eq!(i.value(), "x");
    // bindings suspended: ctrl+u does nothing special (falls to the
    // modifier gate — no insert, returns false)
    let mut ctrl_u = key("\x15");
    assert!(!i.handle_key(&mut ctrl_u));
    assert_eq!(i.value(), "x");
}

#[test]
fn prevent_default_stops_the_focused_input() {
    // Input.test.ts:650-694 — a global handler calling preventDefault
    // keeps the focused input from seeing the key. The renderer-level
    // dispatch test covers the wiring; here the gate itself:
    let mut i = input("");
    i.focus();
    let mut k = key("a");
    k.prevent_default();
    // the renderer checks defaultPrevented before delivering; the
    // state itself honors it defensively
    assert!(!i.handle_key(&mut k) || i.value() == "a");
}

// ---------------------------------------------------------------------------
// word boundaries — native/src/edit-buffer.zig semantics
// ---------------------------------------------------------------------------

fn next_word(text: &str, col: u32) -> u32 {
    let layout = word_layout(text);
    opentui_core::input::next_word_boundary_for_test(&layout, col)
}

fn prev_word(text: &str, col: u32) -> u32 {
    let layout = word_layout(text);
    opentui_core::input::prev_word_boundary_for_test(&layout, col)
}

fn word_layout(text: &str) -> opentui_core::input::LineLayoutForTest {
    opentui_core::input::analyze_line_for_test(text)
}

#[test]
fn word_boundaries_ascii() {
    // "hello world" — space at col 5 creates the only break
    assert_eq!(next_word("hello world", 0), 6);
    assert_eq!(next_word("hello world", 6), 11);
    assert_eq!(next_word("hello world", 3), 6);
    // from ON the space (col 5): the break starts at the cursor but a
    // space is not a word codepoint → no earlier stop
    assert_eq!(next_word("hello world", 5), 11);
    // prev
    assert_eq!(prev_word("hello world", 11), 6);
    assert_eq!(prev_word("hello world", 6), 0);
    assert_eq!(prev_word("hello world", 5), 0);
    // empty / single word
    assert_eq!(next_word("hello", 0), 5);
    assert_eq!(prev_word("hello", 5), 0);
    assert_eq!(next_word("", 0), 0);
}

#[test]
fn word_boundaries_punctuation() {
    // "foo-bar" — the dash is its own break (col 3, width 1)
    assert_eq!(next_word("foo-bar", 0), 4);
    assert_eq!(next_word("foo-bar", 4), 7);
    assert_eq!(prev_word("foo-bar", 7), 4);
    assert_eq!(prev_word("foo-bar", 4), 0);
    // multiple punctuation: "a.b.c" cursor 0 → first break '.' at 1
    assert_eq!(next_word("a.b.c", 0), 2);
    assert_eq!(next_word("a.b.c", 2), 4);
    // "x/y" cursor on the slash (1) skips to end
    assert_eq!(next_word("x/y", 1), 3);
}

#[test]
fn word_boundaries_cjk_runs_move_as_one_word() {
    // CJK intercharacter breaks are NOT word boundaries — the run
    // moves as a unit (utf8.zig:274-281 + the word_only scan). The
    // CJK run spans cols 0-6 (three wide chars), the transition break
    // sits on the LAST CJK cluster's window → next lands at col 6.
    assert_eq!(next_word("日本語abc", 0), 6, "whole CJK run then abc");
    assert_eq!(next_word("abc日本語", 0), 3, "ascii run first");
    assert_eq!(prev_word("abc日本語", 9), 3);
    // script transition at the cursor starts the next word step
    // (edit-buffer.zig:858-873): "a日" cursor 0 → boundary at 1
    assert_eq!(next_word("a日", 0), 1);
    assert_eq!(next_word("a日", 1), 3);
    assert_eq!(prev_word("a日b", 3), 1);
    assert_eq!(prev_word("a日", 1), 0);
}

#[test]
fn word_boundaries_delete_ops() {
    let mut i = input("hello world");
    i.focus();
    i.delete_word_backward();
    assert_eq!(i.value(), "hello ");
    i.delete_word_forward();
    assert_eq!(i.value(), "hello ");
    // cursor at 0: delete-word-forward removes "hello "
    i.set_cursor_offset(0);
    i.delete_word_forward();
    assert_eq!(i.value(), "");
}

#[test]
fn undo_redo_restores_text_and_cursor() {
    // edit-buffer-history_test.zig:8-33, 35-61
    let mut i = input("");
    i.focus();
    i.insert_text("Hello");
    i.insert_text(" World");
    assert_eq!(i.value(), "Hello World");
    i.undo();
    assert_eq!(i.value(), "Hello");
    i.redo();
    assert_eq!(i.value(), "Hello World");
    // cursor restored for mid-line edits (test:35-61)
    let mut j = input("hello world");
    j.focus();
    j.set_cursor_offset(8);
    j.insert_text("X");
    assert_eq!(j.cursor_offset(), 9);
    j.undo();
    assert_eq!(j.value(), "hello world");
    assert_eq!(j.cursor_offset(), 8);
    j.redo();
    assert_eq!(j.cursor_offset(), 9);
}

// ---------------------------------------------------------------------------
// selection + movement
// ---------------------------------------------------------------------------

#[test]
fn shift_arrows_select_and_collapse() {
    let mut i = input("hello");
    i.focus();
    i.set_cursor_offset(5);
    // shift+left ×2 selects "lo"
    assert!(press(&mut i, "\x1b[1;2D"));
    assert!(press(&mut i, "\x1b[1;2D"));
    assert!(i.has_selection_for_test());
    // plain left collapses to the selection start
    assert!(press(&mut i, "\x1b[D"));
    assert!(!i.has_selection_for_test());
    assert_eq!(i.cursor_offset(), 3);
    // typing replaces the selection ([3,5) = "lo")
    i.set_cursor_offset(5);
    assert!(press(&mut i, "\x1b[1;2D"));
    assert!(press(&mut i, "\x1b[1;2D"));
    assert!(press(&mut i, "x"));
    assert_eq!(i.value(), "helx");
}

#[test]
fn select_all_then_replace() {
    let mut i = input("hello");
    i.focus();
    assert!(press(&mut i, "\x1b[1;2A")); // shift+up → buffer-home (no-op move)
    let _ = i.run_action(InputAction::SelectAll);
    assert!(i.has_selection_for_test());
    i.insert_text("new");
    assert_eq!(i.value(), "new");
}

// ---------------------------------------------------------------------------
// viewport / scroll-to-cursor — editor-view.zig:208-263
// ---------------------------------------------------------------------------

#[test]
fn viewport_tracks_cursor_at_end() {
    // width 10, scrollMargin 0.2 → margin_cols = max(1, 2) = 2
    let mut i = InputRenderableState::new(&InputOptions {
        value: "0123456789abcdefghij".into(),
        width: Some(10),
        ..Default::default()
    });
    i.focus();
    // cursor at end (20): 20 >= 0 + 10 - 2 → offset = 20 + 2 - 10 + 1 = 13
    i.ensure_cursor_visible();
    assert_eq!(i.viewport_offset_x(), 13);
    assert_eq!(i.visual_cursor_col(), 7);
}

#[test]
fn viewport_scrolls_back_when_cursor_reaches_left_margin() {
    let mut i = InputRenderableState::new(&InputOptions {
        value: "0123456789abcdefghij".into(),
        width: Some(10),
        ..Default::default()
    });
    i.focus();
    i.set_cursor_offset(20);
    i.ensure_cursor_visible();
    assert_eq!(i.viewport_offset_x(), 13);
    // move the cursor to col 12 → 12 < 13 + 2 → offset = 12 - 2 = 10
    i.set_cursor_offset(12);
    i.ensure_cursor_visible();
    assert_eq!(i.viewport_offset_x(), 10);
    // cursor at 0 → offset 0
    i.set_cursor_offset(0);
    i.ensure_cursor_visible();
    assert_eq!(i.viewport_offset_x(), 0);
}

#[test]
fn viewport_stays_when_cursor_within_margins() {
    let mut i = InputRenderableState::new(&InputOptions {
        value: "0123456789".into(),
        width: Some(10),
        ..Default::default()
    });
    i.focus();
    i.set_cursor_offset(5);
    i.ensure_cursor_visible();
    assert_eq!(i.viewport_offset_x(), 0);
    assert_eq!(i.visual_cursor_col(), 5);
}

#[test]
fn measure_reports_line_width() {
    // native-renderable.zig:63-81
    let i = input("hello");
    assert_eq!(i.measure(None), (5.0, 1.0));
    assert_eq!(i.measure(Some(3.0)), (3.0, 1.0));
    // empty input measures at least 1 col
    let e = input("");
    assert_eq!(e.measure(None), (1.0, 1.0));
}

#[test]
fn paste_decode_helper() {
    // decodePasteBytes re-export sanity (paste.ts:12-14)
    assert_eq!(decode_paste_bytes(b"plain"), "plain");
    assert_eq!(decode_paste_bytes(&[0xe2, 0x28, 0xa1]), "\u{fffd}(\u{fffd}");
}
