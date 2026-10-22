//! Embedded terminal unit tests — ports of upstream
//! `packages/core/src/renderables/EmbeddedTerminal.test.ts` (the
//! expectations are byte-exact oracle pins of the real ghostty-vt
//! native library) plus the probe-frozen resize/scrollback semantics.
//!
//! Upstream lines cited per test. The upstream suite drives the REAL
//! `libopentui.so` through `resolveRenderLib()`; these tests drive the
//! port's [`opentui_core::embedded_terminal::Engine`] against the same
//! pinned bytes.

use opentui_core::embedded_terminal::{
    mods_bits, Engine, KeyAction, KeyInput, LogicalKey, MouseAction, MouseButton, MouseInput,
};
use opentui_core::events::KeyEvent;
use opentui_core::parse_keypress::{parse_keypress, ParseKeypressOptions};
use opentui_core::renderables::embedded_terminal::ScreenCursor as Cursor;

fn key_event(mutations: impl FnOnce(&mut opentui_core::parse_keypress::ParsedKey)) -> KeyEvent {
    let mut key = opentui_core::parse_keypress::ParsedKey {
        name: String::new(),
        ctrl: false,
        meta: false,
        shift: false,
        option: false,
        sequence: String::new(),
        number: false,
        raw: String::new(),
        event_type: opentui_core::parse_keypress::KeyEventType::Press,
        source: opentui_core::parse_keypress::KeySource::Raw,
        code: None,
        super_key: None,
        hyper: None,
        caps_lock: None,
        num_lock: None,
        base_code: None,
        repeated: None,
    };
    mutations(&mut key);
    KeyEvent::new(key)
}

/// upstream `keyEvent(...)` helper (EmbeddedTerminal.test.ts:451-466).
fn key_event_ts(
    name: &str,
    sequence: &str,
    extra: impl FnOnce(&mut opentui_core::parse_keypress::ParsedKey),
) -> KeyEvent {
    key_event(|k| {
        k.name = name.to_owned();
        k.sequence = sequence.to_owned();
        k.raw = sequence.to_owned();
        extra(k);
    })
}

fn encode(term: &mut EmbeddedFixture, key: &KeyEvent) -> String {
    let input = term.state.encode_key(key);
    String::from_utf8_lossy(&input).into_owned()
}

/// A terminal fixture mirroring the upstream suite's
/// `new EmbeddedTerminalRenderable(setup.renderer, { width: 20, height: 4 })`.
struct EmbeddedFixture {
    state: opentui_core::renderables::embedded_terminal::EmbeddedTerminalState,
}

impl EmbeddedFixture {
    fn new() -> Self {
        Self::with_options(20, 4)
    }
    fn with_options(w: u32, h: u32) -> Self {
        let opts = opentui_core::renderables::embedded_terminal::EmbeddedTerminalOptions {
            width: Some(w),
            height: Some(h),
            ..Default::default()
        };
        EmbeddedFixture {
            state: opentui_core::renderables::embedded_terminal::EmbeddedTerminalState::new(&opts),
        }
    }
    fn write(&mut self, data: &str) {
        self.state.write(data.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Grid semantics (probe-frozen, oracle-pinned)
// ---------------------------------------------------------------------------

/// upstream "renders VT output and preserves it across clean frames"
/// (EmbeddedTerminal.test.ts:39-51) — the screen text after a styled
/// write.
#[test]
fn renders_vt_output() {
    let mut t = EmbeddedFixture::new();
    t.write("hello \x1b[1;32mworld\x1b[0m\r\nwide: 界");
    let screen = t.state.screen(20, 4);
    assert_eq!(screen.lines, vec!["hello world", "wide: 界"]);
    // cursor after 界: a(1) + :(1) + space(1) + 界(2) = col 8… "wide: " is
    // 6 cells + 界 (2) → x=8, y=1
    assert_eq!(screen.cursor.x, 8);
    assert_eq!(screen.cursor.y, 1);
    assert!(screen.cursor.visible);
}

/// Wrap-pending: the reported cursor sits on the last written cell
/// (probe 3: "abcd" in a 4-col terminal → x=3).
#[test]
fn wrap_pending_cursor() {
    let mut t = EmbeddedFixture::with_options(4, 2);
    t.write("abcd");
    assert_eq!(
        t.state.screen(4, 2).cursor,
        Cursor {
            x: 3,
            y: 0,
            visible: true
        }
    );
    t.write("e");
    assert_eq!(
        t.state.screen(4, 2).cursor,
        Cursor {
            x: 1,
            y: 1,
            visible: true
        }
    );
    t.write("\x1b[?25l");
    assert!(!t.state.screen(4, 2).cursor.visible);
    t.write("\x1b[?25h");
    assert!(t.state.screen(4, 2).cursor.visible);
}

/// upstream "toggles the pane background…" (:53-66) — the transparent
/// default-intent bg vs the opaque rgb (compositor.zig:19-22).
#[test]
fn transparent_background_default_intent() {
    use opentui_core::buffer::OptimizedBuffer;
    let mut t = EmbeddedFixture::with_options(6, 2);
    t.write("ab\x1b[42mcd\x1b[0m");
    let mut buf = OptimizedBuffer::new(6, 2);
    t.state.engine.compose(&mut buf, 0, 0, false);
    let opaque_bg = buf.get(5, 0).expect("cell").bg;
    assert_eq!(opaque_bg.intent(), opentui_core::color::ColorIntent::Rgb);

    let mut buf = OptimizedBuffer::new(6, 2);
    t.state.engine.compose(&mut buf, 0, 0, true);
    let transparent_bg = buf.get(5, 0).expect("cell").bg;
    assert_eq!(
        transparent_bg.intent(),
        opentui_core::color::ColorIntent::Default
    );
    // the explicit SGR bg keeps rgb intent in both modes
    let styled = buf.get(2, 0).expect("cell").bg;
    assert_eq!(styled.intent(), opentui_core::color::ColorIntent::Rgb);
}

/// LF is pure — no implicit CR (probe 1 "scroll": `l1\nl2\n…` indents).
#[test]
fn pure_lf_keeps_column() {
    let mut t = EmbeddedFixture::new();
    t.write("l1\nl2\nl3");
    assert_eq!(t.state.screen(20, 4).lines, vec!["l1", "  l2", "    l3"]);
}

/// Scroll on overflow pushes into scrollback (probe 3 SB0/SB1/SB2).
#[test]
fn scrollback_viewport() {
    let mut t = EmbeddedFixture::with_options(10, 4);
    for i in 1..=9 {
        t.write(&format!("L{i}\r\n"));
    }
    t.write("bot");
    assert_eq!(t.state.screen(10, 4).lines, vec!["L7", "L8", "L9", "bot"]);
    t.state.engine.scroll_viewport(-3);
    assert_eq!(t.state.screen(10, 4).lines, vec!["L4", "L5", "L6", "L7"]);
    t.state.engine.scroll_viewport(-3);
    assert_eq!(t.state.screen(10, 4).lines, vec!["L1", "L2", "L3", "L4"]);
    t.state.engine.scroll_viewport(3);
    assert_eq!(t.state.screen(10, 4).lines, vec!["L4", "L5", "L6", "L7"]);
    // a write while scrolled does NOT jump to the bottom (probe 3)
    t.write("zz");
    assert_eq!(t.state.screen(10, 4).lines, vec!["L4", "L5", "L6", "L7"]);
}

/// Reflow on resize (probe 3 R0/R4/R8): soft-wrapped lines re-join and
/// re-split at each width; the cursor follows its cell.
#[test]
fn resize_reflow() {
    let mut t = EmbeddedFixture::with_options(6, 2);
    t.write("abcdef");
    assert_eq!(t.state.screen(6, 2).lines, vec!["abcdef"]);
    assert_eq!(t.state.screen(6, 2).cursor.x, 5);
    t.state.engine.resize(4, 2);
    assert_eq!(t.state.screen(4, 2).lines, vec!["abcd", "ef"]);
    assert_eq!(
        (t.state.screen(4, 2).cursor.x, t.state.screen(4, 2).cursor.y),
        (1, 1)
    );
    t.state.engine.resize(8, 2);
    assert_eq!(t.state.screen(8, 2).lines, vec!["abcdef"]);
    assert_eq!(t.state.screen(8, 2).cursor.x, 5);
}

/// Reflow of a wrapped long line (probe 1 resize battery: the
/// "aaaa bbbb …" line re-splits at 8/14).
#[test]
fn resize_reflow_spaces() {
    let mut t = EmbeddedFixture::with_options(10, 4);
    t.write("aaaa bbbb cccc dddd eeee ffff");
    assert_eq!(
        t.state.screen(10, 4).lines,
        vec!["aaaa bbbb", "cccc dddd", "eeee ffff"]
    );
    t.state.engine.resize(8, 4);
    assert_eq!(
        t.state.screen(8, 4).lines,
        vec!["aaaa bbb", "b cccc d", "ddd eeee", " ffff"]
    );
    t.state.engine.resize(14, 4);
    assert_eq!(
        t.state.screen(14, 4).lines,
        vec!["aaaa bbbb cccc", " dddd eeee fff", "f"]
    );
}

/// The palette is pinned: 0-15 Tomorrow Night, 16+ the xterm cube
/// (probe 2).
#[test]
fn palette_pinning() {
    assert_eq!(
        opentui_core::embedded_terminal::palette_rgb(1),
        (204, 102, 102)
    );
    assert_eq!(
        opentui_core::embedded_terminal::palette_rgb(4),
        (129, 162, 190)
    );
    assert_eq!(
        opentui_core::embedded_terminal::palette_rgb(9),
        (213, 78, 83)
    );
    assert_eq!(
        opentui_core::embedded_terminal::palette_rgb(208),
        (255, 135, 0)
    );
    assert_eq!(opentui_core::embedded_terminal::palette_rgb(17), (0, 0, 95));
    assert_eq!(opentui_core::embedded_terminal::palette_rgb(232), (8, 8, 8));
    assert_eq!(
        opentui_core::embedded_terminal::palette_rgb(255),
        (238, 238, 238)
    );
}

/// BCE — erase takes the current SGR bg (probe 3).
#[test]
fn erase_uses_current_bg() {
    use opentui_core::buffer::OptimizedBuffer;
    let mut t = EmbeddedFixture::with_options(6, 3);
    t.write("\x1b[41mab\x1b[3G\x1b[K");
    let mut buf = OptimizedBuffer::new(6, 3);
    t.state.engine.compose(&mut buf, 0, 0, false);
    let red = opentui_core::embedded_terminal::palette_rgb(1);
    for x in 0..6 {
        let cell = buf.get(x, 0).expect("cell");
        let [r, g, b, _] = cell.bg.to_ints();
        assert_eq!((r, g, b), red, "cell {x}");
    }
}

/// OSC 10/11 recolor the pane defaults retroactively (probe 2).
#[test]
fn osc_default_colors() {
    use opentui_core::buffer::OptimizedBuffer;
    let mut t = EmbeddedFixture::with_options(6, 2);
    t.write("\x1b]10;#123456\x07x\x1b]11;#654321\x07y");
    let mut buf = OptimizedBuffer::new(6, 2);
    t.state.engine.compose(&mut buf, 0, 0, false);
    let cell = buf.get(5, 1).expect("cell");
    let [fr, fg, fb, _] = cell.fg.to_ints();
    let [br, bg_, bb, _] = cell.bg.to_ints();
    assert_eq!((fr, fg, fb), (0x12, 0x34, 0x56));
    assert_eq!((br, bg_, bb), (0x65, 0x43, 0x21));
}

// ---------------------------------------------------------------------------
// Responses (EmbeddedTerminal.test.ts:216-232, 334-346)
// ---------------------------------------------------------------------------

/// upstream "distinguishes user input from terminal responses"
/// (:216-232): `\x1b[5n` → response `\x1b[0n`; Enter → input `\r`.
#[test]
fn distinguishes_input_from_responses() {
    use opentui_core::renderables::embedded_terminal::EmbeddedTerminalDataSource as Src;
    let mut t = EmbeddedFixture::new();
    t.write("\x1b[5n");
    t.state
        .handle_key_press(&key_event_ts("enter", "\r", |_| {}));
    let data = &t.state.on_data;
    assert_eq!(data.len(), 2);
    assert_eq!(
        (data[0].0.as_slice(), data[0].1),
        (b"\x1b[0n".as_slice(), Src::Response)
    );
    assert_eq!(
        (data[1].0.as_slice(), data[1].1),
        (b"\r".as_slice(), Src::Input)
    );
}

/// CPR + DA replies (probe 2: `\x1b[1;3R` + `\x1b[0n` + the XTVERSION
/// DCS for DA1).
#[test]
fn dsr_and_da_responses() {
    let mut t = EmbeddedFixture::new();
    t.write("ab\x1b[6n\x1b[5n\x1b[c");
    // write() flushes responses into on_data (EmbeddedTerminal.ts:142-150)
    let drained: Vec<u8> = t
        .state
        .on_data
        .iter()
        .flat_map(|(d, _)| d.iter().copied())
        .collect();
    assert_eq!(
        drained.as_slice(),
        b"\x1b[1;3R\x1b[0n\x1bP>|libghostty\x1b\\"
    );
}

/// upstream "drains the preserved response prefix after overflow"
/// (:334-346): the response queue caps at 1 MiB.
#[test]
fn response_overflow_prefix() {
    let mut engine = Engine::new(20, 4, 10_000);
    let query = b"\x1b[5n";
    let repeats = 1024 * 1024 / query.len() + 1;
    let mut batch = Vec::new();
    for _ in 0..repeats {
        batch.extend_from_slice(query);
    }
    engine.write(&batch);
    let drained = engine.drain_responses();
    assert_eq!(drained.len(), 1024 * 1024);
    assert_eq!(&drained[..4], b"\x1b[0n");
}

// ---------------------------------------------------------------------------
// Key encoding (EmbeddedTerminal.test.ts:188-332)
// ---------------------------------------------------------------------------

/// upstream "encodes keys and bracketed paste" (:188-214).
#[test]
fn encodes_keys_and_bracketed_paste() {
    let mut t = EmbeddedFixture::new();
    t.write("\x1b[?2004h");

    assert_eq!(encode(&mut t, &key_event_ts("enter", "\r", |_| {})), "\r");
    assert_eq!(
        String::from_utf8_lossy(&t.state.encode_paste(b"one\ntwo")),
        "\x1b[200~one\ntwo\x1b[201~"
    );
    assert_eq!(encode(&mut t, &key_event_ts("😀", "😀", |_| {})), "😀");
    assert_eq!(encode(&mut t, &key_event_ts("space", " ", |_| {})), " ");
    assert_eq!(
        encode(
            &mut t,
            &key_event_ts("up", "\x1b[A", |k| k.code = Some("[A".into()))
        ),
        "\x1b[A"
    );
    assert_eq!(
        encode(
            &mut t,
            &key_event_ts("down", "\x1b[B", |k| k.code = Some("[B".into()))
        ),
        "\x1b[B"
    );
    assert_eq!(
        encode(&mut t, &key_event_ts("a", "A", |k| k.shift = true)),
        "A"
    );

    // `\x1b[>19u` — report-all passthrough of long text (:209-213)
    t.write("\x1b[>19u");
    let long = "x".repeat(2048);
    assert_eq!(encode(&mut t, &key_event_ts(&long, &long, |_| {})), long);
}

/// upstream "encodes Meta as Super rather than Alt" (:259-274).
#[test]
fn meta_encodes_as_super() {
    let mut t = EmbeddedFixture::new();
    t.write("\x1b[>3u");
    let encoded = encode(
        &mut t,
        &key_event_ts("a", "a", |k| {
            k.code = Some("KeyA".into());
            k.base_code = Some(u32::from(b'a'));
            k.meta = true;
        }),
    );
    assert_eq!(encoded, "\x1b[97;9u");
}

/// upstream "re-encodes Kitty character and escape keys" (:276-293).
#[test]
fn reencodes_kitty_keys() {
    let mut t = EmbeddedFixture::new();
    t.write("\x1b[>3u");
    assert_eq!(
        encode(
            &mut t,
            &key_event_ts("c", "", |k| {
                k.raw = "\x1b[99;5u".into();
                k.source = opentui_core::parse_keypress::KeySource::Kitty;
                k.ctrl = true;
            })
        ),
        "\x1b[99;5u"
    );
    assert_eq!(
        encode(
            &mut t,
            &key_event_ts("escape", "\x1b[27u", |k| {
                k.raw = "\x1b[27u".into();
                k.source = opentui_core::parse_keypress::KeySource::Kitty;
                k.code = Some("[27u".into());
            })
        ),
        "\x1b[27u"
    );
}

/// upstream `test.each` Dvorak/legacy matrix (:295-314) — the
/// base-layout alternate handling.
#[test]
fn dvorak_matrix() {
    let cases: &[(&str, u8, &str, &str)] = &[
        ("plain Dvorak u", 1, "\x1b[117::102;1u", "u"),
        ("plain Dvorak d", 1, "\x1b[100::104;1u", "d"),
        ("Dvorak Ctrl+U", 1, "\x1b[117::102;5u", "\x1b[117;5u"),
        ("Dvorak Ctrl+D", 1, "\x1b[100::104;5u", "\x1b[100;5u"),
        (
            "Dvorak Ctrl+Shift+U",
            1,
            "\x1b[117:85:102;6u",
            "\x1b[117;6u",
        ),
        ("Cyrillic Ctrl+ф", 1, "\x1b[1092::97;5u", "\x1b[1092;5u"),
        ("QWERTY Ctrl+U", 1, "\x1b[117;5u", "\x1b[117;5u"),
        (
            "Dvorak Ctrl+U in legacy mode",
            0,
            "\x1b[117::102;5u",
            "\x15",
        ),
        (
            "Dvorak Ctrl+D in legacy mode",
            0,
            "\x1b[100::104;5u",
            "\x04",
        ),
        (
            "Dvorak Ctrl+U with its base-layout alternative",
            5,
            "\x1b[117::102;5u",
            "\x1b[117::102;5u",
        ),
        (
            "Dvorak Ctrl+D with its base-layout alternative",
            5,
            "\x1b[100::104;5u",
            "\x1b[100::104;5u",
        ),
    ];
    for (_label, flags, raw, expected) in cases {
        let mut t = EmbeddedFixture::new();
        t.write(&format!("\x1b[>{flags}u"));
        let parsed = parse_keypress(
            raw,
            ParseKeypressOptions {
                use_kitty_keyboard: true,
            },
        )
        .expect("parse");
        let encoded = encode(&mut t, &KeyEvent::new(parsed));
        assert_eq!(&encoded, expected, "case {raw} flags {flags}");
    }
}

/// upstream "forwards Dvorak press, repeat, and release" (:316-332).
#[test]
fn dvorak_event_types() {
    let mut t = EmbeddedFixture::new();
    t.write("\x1b[>3u");
    for raw in [
        "\x1b[117::102;5u",
        "\x1b[117::102;5:2u",
        "\x1b[117::102;5:3u",
    ] {
        let parsed = parse_keypress(
            raw,
            ParseKeypressOptions {
                use_kitty_keyboard: true,
            },
        )
        .expect("parse");
        let out = t.state.handle_key_press(&KeyEvent::new(parsed));
        assert!(out);
    }
    let outs: Vec<String> = t
        .state
        .on_data
        .iter()
        .map(|(d, _)| String::from_utf8_lossy(d).into_owned())
        .collect();
    assert_eq!(outs, vec!["\x1b[117;5u", "\x1b[117;5:2u", "\x1b[117;5:3u"]);
}

/// upstream "forwards Kitty key releases while focused" (:422-448).
#[test]
fn kitty_release_while_focused() {
    let mut t = EmbeddedFixture::new();
    t.write("\x1b[>3u");
    let key = key_event(|k| {
        k.name = "a".into();
        k.sequence = String::new();
        k.raw = String::new();
        k.event_type = opentui_core::parse_keypress::KeyEventType::Release;
        k.source = opentui_core::parse_keypress::KeySource::Kitty;
        k.code = Some("KeyA".into());
    });
    t.state.handle_key_press(&key);
    let last = t.state.on_data.last().expect("data");
    assert_eq!(String::from_utf8_lossy(&last.0), "\x1b[97;1:3u");
}

// ---------------------------------------------------------------------------
// Mouse encoding (EmbeddedTerminal.test.ts:234-257)
// ---------------------------------------------------------------------------

/// upstream "encodes no-button motion and suppresses unavailable pixel
/// coordinates" (:234-257).
#[test]
fn encodes_no_button_motion() {
    let mut engine = Engine::new(20, 4, 10_000);
    engine.write(b"\x1b[?1003h\x1b[?1006h");
    let motion = engine.encode_mouse(&MouseInput {
        action: MouseAction::Motion,
        x: 1.0,
        y: 1.0,
        ..Default::default()
    });
    assert_eq!(String::from_utf8_lossy(&motion), "\x1b[<35;2;2M");

    engine.write(b"\x1b[?1016h");
    let suppressed = engine.encode_mouse(&MouseInput {
        action: MouseAction::Motion,
        x: 1.0,
        y: 1.0,
        ..Default::default()
    });
    assert!(suppressed.is_empty());
}

/// SGR wheel mappings (ghostty mouse_encode tests: four→64, five→65).
#[test]
fn sgr_wheel_buttons() {
    let mut engine = Engine::new(20, 4, 10_000);
    engine.write(b"\x1b[?1000h\x1b[?1006h");
    for (button, code) in [(MouseButton::Four, 64u8), (MouseButton::Five, 65)] {
        let out = engine.encode_mouse(&MouseInput {
            action: MouseAction::Press,
            button: Some(button),
            x: 0.0,
            y: 0.0,
            ..Default::default()
        });
        assert_eq!(String::from_utf8_lossy(&out), format!("\x1b[<{code};1;1M"));
    }
}

/// Legacy modifiers add to the button code (ghostty mouse_encode
/// "urxvt with modifiers").
#[test]
fn urxvt_modifiers() {
    let mut engine = Engine::new(20, 4, 10_000);
    engine.write(b"\x1b[?1003h\x1b[?1015h");
    let out = engine.encode_mouse(&MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        mods: mods_bits::SHIFT | mods_bits::ALT | mods_bits::CTRL,
        x: 2.0,
        y: 3.0,
        ..Default::default()
    });
    assert_eq!(String::from_utf8_lossy(&out), "\x1b[60;3;4M");
}

// ---------------------------------------------------------------------------
// Focus reporting (EmbeddedTerminal.ts:227-253; mode 1004)
// ---------------------------------------------------------------------------

#[test]
fn focus_reporting() {
    let mut t = EmbeddedFixture::new();
    assert!(t.state.engine.encode_focus(true).is_empty());
    t.write("\x1b[?1004h");
    assert_eq!(t.state.engine.encode_focus(true), b"\x1b[I");
    assert_eq!(t.state.engine.encode_focus(false), b"\x1b[O");
}

// ---------------------------------------------------------------------------
// Paste encoding (input/paste.zig)
// ---------------------------------------------------------------------------

#[test]
fn paste_unbracketed_newline_and_strip() {
    let t = EmbeddedFixture::new();
    assert_eq!(t.state.encode_paste(b"hello\nworld"), b"hello\rworld");
    assert_eq!(t.state.encode_paste(b"hel\x1blo"), b"hel lo");
    // \r\n → \r\r (xterm parity, paste.zig tests)
    assert_eq!(t.state.encode_paste(b"a\r\nb"), b"a\r\rb");
}

// ---------------------------------------------------------------------------
// Selection (EmbeddedTerminal.test.ts:88-186)
// ---------------------------------------------------------------------------

/// upstream "participates in renderer selection and returns terminal
/// text" (:88-104) — getSelectedText slices the row.
#[test]
fn selection_text() {
    let mut t = EmbeddedFixture::new();
    t.write("hello\r\nwide: 界");
    t.state
        .on_selection_changed(Some(sel_update(1, 0, 3, 0, 20, 4, true)));
    assert!(t.state.has_selection());
    assert_eq!(t.state.get_selected_text(), "ell");
    t.state.on_selection_changed(None);
    assert!(!t.state.has_selection());
    assert_eq!(t.state.get_selected_text(), "");
}

/// upstream "waits for a drag to leave its starting cell" (:106-150):
/// a cell selection on its anchor is ignored until it moves.
#[test]
fn selection_drag_gate() {
    let mut t = EmbeddedFixture::new();
    t.write("hello");
    // anchor == focus, cell behavior → not started
    assert!(!t
        .state
        .on_selection_changed(Some(sel_update(1, 0, 1, 0, 20, 4, true))));
    assert!(!t.state.has_selection());
    // moved → active
    assert!(t
        .state
        .on_selection_changed(Some(sel_update(1, 0, 2, 0, 20, 4, true))));
    assert!(t.state.has_selection());
    assert_eq!(t.state.get_selected_text(), "el");
    // back to the anchor still selects (threshold crossed once)
    assert!(t
        .state
        .on_selection_changed(Some(sel_update(1, 0, 1, 0, 20, 4, true))));
    assert_eq!(t.state.get_selected_text(), "e");
}

/// upstream "does not delay word or line selection gestures" (:175-186).
#[test]
fn word_line_selection_immediate() {
    let mut t = EmbeddedFixture::new();
    t.write("hello");
    assert!(t
        .state
        .on_selection_changed(Some(sel_update(0, 0, 0, 0, 20, 4, false))));
    assert_eq!(t.state.get_selected_text(), "h");
}

/// upstream "starts selection on horizontal or vertical movement even
/// when only one text cell is selected" (:152-173).
#[test]
fn selection_movement_starts() {
    let mut t = EmbeddedFixture::new();
    t.write("hello");
    for (x, y) in [(5, 0), (4, 1)] {
        t.state.on_selection_changed(None);
        assert!(t
            .state
            .on_selection_changed(Some(sel_update(4, 0, x, y, 20, 4, true))));
        assert!(t.state.has_selection());
        assert_eq!(t.state.get_selected_text(), "o");
    }
}

fn sel_update(
    ax: i32,
    ay: i32,
    fx: i32,
    fy: i32,
    w: u32,
    h: u32,
    cell: bool,
) -> opentui_core::renderables::embedded_terminal::SelectionUpdate {
    opentui_core::renderables::embedded_terminal::SelectionUpdate {
        behavior_cell: cell,
        anchor: (ax, ay),
        focus: (fx, fy),
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------------------
// Constructor / resize callback parity (upstream :348-378)
// ---------------------------------------------------------------------------

/// upstream "resizes and destroys native state idempotently"
/// (:348-369) — the onTerminalResize callback records (5, 3).
#[test]
fn resize_callback() {
    let mut t = EmbeddedFixture::with_options(10, 2);
    t.write("abcdefghij");
    t.state.update_layout(5, 3);
    assert!(t.state.on_resize.iter().any(|&(c, r)| c == 5 && r == 3));
}

/// upstream "rejects dimensions that cannot cross the native ABI"
/// (:371-378): 1..=65535 validation. The port clamps via the renderable
/// layer; the engine itself no-ops zero dims (resize guard).
#[test]
fn zero_resize_is_noop() {
    let mut engine = Engine::new(10, 4, 10_000);
    engine.resize(0, 24);
    assert_eq!((engine.cols, engine.rows), (10, 4));
}

// ---------------------------------------------------------------------------
// Low-level encode inputs (engine-level pins for the ghostty tables)
// ---------------------------------------------------------------------------

#[test]
fn engine_key_table_pins() {
    let engine = Engine::new(10, 4, 10_000);
    // legacy arrows
    let up = KeyInput {
        action: KeyAction::Press,
        key: LogicalKey::ArrowUp,
        mods: 0,
        text: String::new(),
        unshifted_codepoint: 0,
    };
    assert_eq!(engine.encode_key(&up), b"\x1b[A");
    // kitty disambiguate: shift+backspace → CSI u (ghostty test)
    let mut e2 = Engine::new(10, 4, 10_000);
    e2.write(b"\x1b[>1u");
    let shift_bs = KeyInput {
        action: KeyAction::Press,
        key: LogicalKey::Backspace,
        mods: mods_bits::SHIFT,
        text: String::new(),
        unshifted_codepoint: 0,
    };
    assert_eq!(e2.encode_key(&shift_bs), b"\x1b[127;2u");
    // disambiguate + report-events + report-all: enter release
    let mut e3 = Engine::new(10, 4, 10_000);
    e3.write(b"\x1b[>15u");
    let enter_release = KeyInput {
        action: KeyAction::Release,
        key: LogicalKey::Enter,
        mods: 0,
        text: String::new(),
        unshifted_codepoint: 0,
    };
    assert_eq!(e3.encode_key(&enter_release), b"\x1b[13;1:3u");
}
