//! Byte-level DECSET/mode escape sequences for the terminal backend.
//!
//! Every constant cites the vendored upstream source it is ported from
//! (`third_party/opentui/packages/native/src/ansi.zig` line numbers in
//! parentheses) and, where the consumer repo self-manages the same
//! sequence (`oc-tui` mouse_modes.rs / terminal_proto.rs / suspend.rs),
//! the sequences were cross-checked byte-for-byte. Divergences are
//! recorded at each site — upstream zig wins.

use std::io;

// ---------------------------------------------------------------------------
// Screen + cursor (ANSI.saving / cursor section)
// ---------------------------------------------------------------------------

/// `saveCursorState` (ansi.zig:318). OC-TU cross-check: oc-tui relies on
/// crossterm's EnterAlternateScreen (which folds `?1049h` + cursor save);
/// upstream saves the cursor EXPLICITLY before switching screens
/// (renderer.zig:563 writes this before enterAltScreen) — upstream wins.
pub const SAVE_CURSOR: &[u8] = b"\x1b[s";

/// `restoreCursorState` (ansi.zig:319).
pub const RESTORE_CURSOR: &[u8] = b"\x1b[u";

/// `switchToAlternateScreen` (ansi.zig:325). Byte-identical to
/// crossterm's LeaveAlternateScreen counterpart used by oc-tui
/// (app.rs:7652) and suspend.rs:46 (`\x1b[?1049l`).
pub const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h";

/// `switchToMainScreen` (ansi.zig:326).
pub const EXIT_ALT_SCREEN: &[u8] = b"\x1b[?1049l";

/// `hideCursor` (ansi.zig:246).
pub const HIDE_CURSOR: &[u8] = b"\x1b[?25l";

/// `showCursor` (ansi.zig:247).
pub const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

// ---------------------------------------------------------------------------
// Mouse tracking (terminal.zig:946-993, ansi.zig:328-335)
// ---------------------------------------------------------------------------

/// `enableMouseTracking` (ansi.zig:328).
pub const MOUSE_TRACKING_ENABLE: &[u8] = b"\x1b[?1000h";
/// `disableMouseTracking` (ansi.zig:329).
pub const MOUSE_TRACKING_DISABLE: &[u8] = b"\x1b[?1000l";
/// `enableButtonEventTracking` (ansi.zig:330).
pub const BUTTON_EVENT_TRACKING_ENABLE: &[u8] = b"\x1b[?1002h";
/// `disableButtonEventTracking` (ansi.zig:331).
pub const BUTTON_EVENT_TRACKING_DISABLE: &[u8] = b"\x1b[?1002l";
/// `enableAnyEventTracking` (ansi.zig:332).
pub const ANY_EVENT_TRACKING_ENABLE: &[u8] = b"\x1b[?1003h";
/// `disableAnyEventTracking` (ansi.zig:333).
pub const ANY_EVENT_TRACKING_DISABLE: &[u8] = b"\x1b[?1003l";
/// `enableSGRMouseMode` (ansi.zig:334).
pub const SGR_MOUSE_MODE_ENABLE: &[u8] = b"\x1b[?1006h";
/// `disableSGRMouseMode` (ansi.zig:335).
pub const SGR_MOUSE_MODE_DISABLE: &[u8] = b"\x1b[?1006l";

/// `setMouseMode(enable=true)` burst (terminal.zig:962-979, one writeAll
/// per sequence in this order). With `movement` the set is
/// `?1000h ?1002h ?1003h ?1006h` — byte-identical to oc-tui
/// mouse_modes.rs:33 `V2_MOUSE_ENABLE` (order included). Without
/// movement, upstream FIRST resets any-event tracking
/// (terminal.zig:968-973 — some terminals let the last of
/// ?1000/?1002/?1003 win) and then enables click/drag only.
pub fn mouse_enable(out: &mut dyn io::Write, movement: bool) -> io::Result<()> {
    if !movement {
        out.write_all(ANY_EVENT_TRACKING_DISABLE)?; // terminal.zig:972
    }
    out.write_all(MOUSE_TRACKING_ENABLE)?; // terminal.zig:974
    out.write_all(BUTTON_EVENT_TRACKING_ENABLE)?; // terminal.zig:975
    if movement {
        out.write_all(ANY_EVENT_TRACKING_ENABLE)?; // terminal.zig:977
    }
    out.write_all(SGR_MOUSE_MODE_ENABLE) // terminal.zig:979
}

/// `writeMouseDisableSequences` (terminal.zig:946-951): the best-effort
/// shutdown order `?1003l ?1002l ?1000l ?1006l`.
///
/// DIVERGENCE vs oc-tui: mouse_modes.rs:37 `V2_MOUSE_DISABLE` pops in
/// reverse order (`?1006l ?1003l ?1002l ?1000l`). Upstream zig order
/// wins here (mode clears are order-independent on real terminals, but
/// the spec pins the zig bytes); recorded per lane A1 instructions.
pub const MOUSE_DISABLE_ALL: &[u8] = b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l";

// ---------------------------------------------------------------------------
// Paste / focus / sync / modifyOtherKeys / kitty keyboard
// ---------------------------------------------------------------------------

/// `bracketedPasteSet` (ansi.zig:391). Same bytes as oc-tui
/// terminal_proto.rs `PASTE_FOCUS_ON`'s paste half.
pub const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";
/// `bracketedPasteReset` (ansi.zig:392).
pub const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

/// `focusSet` (ansi.zig:379).
pub const FOCUS_ENABLE: &[u8] = b"\x1b[?1004h";
/// `focusReset` (ansi.zig:380).
pub const FOCUS_DISABLE: &[u8] = b"\x1b[?1004l";

/// `unicodeSet` (ansi.zig:387) — `?2027h`, the Unicode-overlap
/// mode `enableDetectedFeatures` arms when the probe settled on a
/// Unicode width method without explicit-width override
/// (terminal.zig:420-422).
pub const UNICODE_SET: &[u8] = b"\x1b[?2027h";

/// Synchronized-update begin. Upstream ansi.zig names it
/// `syncUpdateSet` (`\x1b[?2026h`) — see the zig sync wrapper the
/// output pump pairs with `?2026l` at frame end; A2's pump owns the
/// pairing, the raw bytes live here. Matches opentui-core ansi.rs:162
/// `SYNC_SET`.
pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// Synchronized-update end (`\x1b[?2026l`); ansi.rs:163 `SYNC_RESET`.
pub const SYNC_END: &[u8] = b"\x1b[?2026l";

/// `modifyOtherKeysSet` (ansi.zig:404).
pub const MODIFY_OTHER_KEYS_ENABLE: &[u8] = b"\x1b[>4;1m";
/// `modifyOtherKeysReset` (ansi.zig:405).
pub const MODIFY_OTHER_KEYS_DISABLE: &[u8] = b"\x1b[>4;0m";

/// `csiUPush` (ansi.zig:400) — `setKittyKeyboard(true, flags)`
/// (terminal.zig:1007-1013) formats it as `CSI > {flags} u`.
/// oc-tui cross-check: terminal_proto.rs:46 `KITTY_PUSH` is the fixed
/// `\x1b[>5u` (disambiguate|alternate keys — the same default flag pair
/// `build_kitty_keyboard_flags` yields, renderer.rs:250); identical
/// bytes for the default configuration.
pub fn kitty_push(out: &mut dyn io::Write, flags: u32) -> io::Result<()> {
    write!(out, "\x1b[>{}u", flags)
}

/// `csiUPop` (ansi.zig:401) — `setKittyKeyboard(false, _)`
/// (terminal.zig:1015-1017). Byte-identical to oc-tui
/// terminal_proto.rs:48 `KITTY_POP`.
pub const KITTY_POP: &[u8] = b"\x1b[<u";

// ---------------------------------------------------------------------------
// Shutdown tail (renderer.zig:642-645, ansi.zig:307-309/248)
// ---------------------------------------------------------------------------

/// `resetMousePointer` (ansi.zig:309) — written first by `resetState`
/// (terminal.zig:285).
pub const RESET_MOUSE_POINTER: &[u8] = b"\x1b]22;\x1b\\";
/// `resetCursorColorFallback` (ansi.zig:308; renderer.zig:642).
pub const RESET_CURSOR_COLOR_FALLBACK: &[u8] = b"\x1b]12;default\x07";
/// `resetCursorColor` (ansi.zig:307; renderer.zig:643).
pub const RESET_CURSOR_COLOR: &[u8] = b"\x1b]112\x07";
/// `defaultCursorStyle` (ansi.zig:248; renderer.zig:644). Byte-identical
/// to oc-tui terminal_proto.rs:53 `CURSOR_RESET`.
pub const DEFAULT_CURSOR_STYLE: &[u8] = b"\x1b[0 q";
/// `setTerminalTitle("")` (ansi.zig:412; terminal.zig:328 writes the
/// empty title during resetState).
pub const RESET_TERMINAL_TITLE: &[u8] = b"\x1b]0;\x07";

#[cfg(test)]
mod tests {
    #[cfg(feature = "terminal-backend")]
    use super::*;

    /// The enable set with movement is byte-identical to oc-tui's
    /// v2-parity `V2_MOUSE_ENABLE` (mouse_modes.rs:33).
    #[cfg(feature = "terminal-backend")]
    #[test]
    fn mouse_enable_movement_matches_v2_set() {
        let mut buf = Vec::new();
        mouse_enable(&mut buf, true).unwrap();
        assert_eq!(buf, b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
    }

    /// Without movement the any-event reset leads the burst
    /// (terminal.zig:968-973).
    #[cfg(feature = "terminal-backend")]
    #[test]
    fn mouse_enable_without_movement_leads_with_1003_reset() {
        let mut buf = Vec::new();
        mouse_enable(&mut buf, false).unwrap();
        assert_eq!(buf, b"\x1b[?1003l\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    }

    /// The disable order is upstream's terminal.zig:946-951 order — NOT
    /// oc-tui's reverse pop (divergence recorded above).
    #[cfg(feature = "terminal-backend")]
    #[test]
    fn mouse_disable_all_is_upstream_order() {
        assert_eq!(
            MOUSE_DISABLE_ALL,
            b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l"
        );
    }
}
