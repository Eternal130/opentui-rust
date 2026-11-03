//! The terminal session lifecycle: raw mode, alt-screen setup burst,
//! RAII teardown, and the panic-hook helper.
//!
//! Port of `CliRenderer.setupTerminal`'s non-detection half
//! (renderer.zig:558-576 — `setupTerminalWithoutDetection`) and
//! `performShutdownSequence` (renderer.zig:600-654, whose body is
//! `Terminal.resetState` terminal.zig:282-335 plus the cursor-color /
//! DECSCUSR tail). The capability-probe burst (renderer.zig:544-551,
//! terminal.zig:347-397) is lane A2 — this lane has no probing, so
//! `SetupOptions` states feature switches directly (the consumer's
//! config decides, exactly like oc-tui's config-gated mouse modes).
//!
//! Screen mode is alt-screen ONLY by design: the inline viewport and
//! the whole split-footer / ExternalOutputCommit family
//! (renderer.ts:2339+, `makeRoomForRendererOutput` ansi.zig) are
//! deferred to Phase B4 per the Phase A ruling — GAPS-registered.

use std::io;

use super::modes;
use super::tty::{TermiosSave, TtyDevice};

/// What `Terminal::new` arms — the feature subset of upstream's
/// detected-feature burst (terminal.zig:399-439) that A1 can decide
/// without probing.
#[derive(Clone, Debug)]
pub struct SetupOptions {
    /// Enable the mouse tracking family (terminal.zig:955-985; the
    /// consumer-side equivalent of `renderer.enableMouse`,
    /// renderer.zig:3367-3371 — oc-tui pushes this config-gated right
    /// after renderer setup, app.rs:7636-7643).
    pub use_mouse: bool,
    /// `enable_movement` — upstream state default is true
    /// (terminal.zig:227) and oc-tui always runs the full
    /// ?1000/?1002/?1003/?1006 set (mouse_modes.rs:33).
    pub mouse_movement: bool,
    /// Kitty progressive-enhancement flags; 0 disables the protocol —
    /// upstream gates exactly this way (`useKitty =
    /// opts.kitty_keyboard_flags > 0`, renderer.zig:572). Flags come
    /// from `build_kitty_keyboard_flags` (renderer.rs:250).
    pub kitty_keyboard_flags: u32,
    /// `?2004` bracketed paste (terminal.zig:995-999). Upstream arms it
    /// when `caps.bracketed_paste`; A1 has no caps, so the consumer
    /// states it (oc-tui always enables, app.rs:7635).
    pub bracketed_paste: bool,
    /// `?1004` focus reporting (terminal.zig:1001-1005). Same story as
    /// `bracketed_paste` (oc-tui always enables, app.rs:7631).
    pub auto_focus: bool,
    /// `useAlternateScreen` (renderer.zig:560/566-569) — default
    /// `true` (the Phase A alt-screen ruling). When `false`, the
    /// setup skips `?1049h` and instead reserves the main-screen
    /// surface via `makeRoomForRendererOutput` (ansi.zig:418-425)
    /// when `reserve_surface_height > 0` — the split-footer runtime
    /// base (D1b-core): the renderer's rows sit at the bottom of the
    /// main screen with the shell's scrollback above.
    pub use_alt_screen: bool,
    /// The `makeRoomForRendererOutput` height (renderer.zig:568
    /// passes `@max(self.height, 1)`) — only read when
    /// `use_alt_screen` is false; 0 = no reservation (the bare
    /// non-alt arm).
    pub reserve_surface_height: u32,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            use_mouse: false,
            mouse_movement: true,
            kitty_keyboard_flags: 0,
            bracketed_paste: true,
            auto_focus: true,
            use_alt_screen: true,
            reserve_surface_height: 0,
        }
    }
}

/// Mirrors `Terminal.state` (terminal.zig:221-245) — the subset this
/// lane tracks, driving teardown conditionals exactly like
/// `resetState` reads them.
#[derive(Clone, Debug, Default)]
pub(super) struct TerminalState {
    alt_screen: bool,
    kitty_keyboard: bool,
    modify_other_keys: bool,
    mouse_was_enabled: bool,
    pub(super) bracketed_paste: bool,
    pub(super) focus_tracking: bool,
    /// The image-session marker (the C3 graphics teardown gate): set
    /// by [`Terminal::note_kitty_graphics`] once the session emitted
    /// kitty graphics — `teardown` then pops the graphics family.
    kitty_graphics_session: bool,
}

/// A live terminal session: raw mode + alt screen armed on `new`,
/// fully restored on `teardown`/`Drop` (RAII, idempotent).
pub struct Terminal<T: TtyDevice, W: io::Write> {
    pub(super) tty: T,
    pub(super) out: W,
    pub(super) state: TerminalState,
    termios_save: Option<TermiosSave>,
    torn_down: bool,
    /// Registered panic-hook fd (deregistered on teardown/drop); None
    /// for mock devices that never join the global registry.
    panic_fd: Option<i32>,
    /// The live capability probe (A2's `probe_capabilities` arms it;
    /// `feed_input` consumes it).
    pub(super) probe: Option<super::probe::CapabilityProbe>,
    /// Last-known capability snapshot — the probe's final state once
    /// settled (A4's `enable_detected_features` reads it; the
    /// all-unknown default before any probe, like upstream's fresh
    /// `caps` at terminal.zig:13).
    pub(super) caps: crate::renderer::TerminalCapabilities,
    /// The post-probe theme watch (A4 — late-OSC11 reduction).
    pub(super) theme: super::theme::ThemeWatch,
    /// How much of `renderer.sgr_stream()` `pump_output` already
    /// drained (A2's output pump).
    pub(super) sgr_drained: usize,
    /// Last-seen TIOCGWINSZ (A2's resize watch).
    pub(super) size: Option<(u16, u16)>,
}

impl<T: TtyDevice, W: io::Write> Terminal<T, W> {
    /// Acquire the terminal: raw mode (Node's `setRawMode(true)`), then
    /// the setup burst in `setupTerminalWithoutDetection` order
    /// (renderer.zig:563-575 + `enableDetectedFeatures`
    /// terminal.zig:409-430):
    ///
    /// 1. `\x1b[s`          saveCursorState        (renderer.zig:563)
    /// 2. `\x1b[?1049h`     enterAltScreen         (renderer.zig:566)
    ///    — or, when `use_alt_screen` is false with a reserved
    ///    surface, `height-1` newlines (makeRoomForRendererOutput,
    ///    ansi.zig:418-425 via renderer.zig:566-569 — the D1b-core
    ///    non-alt arm)
    /// 3. `\x1b[>4;1m`      modifyOtherKeys on     (terminal.zig:409-411 —
    ///    state is fresh, so upstream ALWAYS writes this first)
    /// 4. if kitty flags > 0: `\x1b[>4;0m` then `\x1b[>{flags}u`
    ///    (terminal.zig:413-418)
    /// 5. if use_mouse: the setMouseMode burst  (terminal.zig:962-979)
    /// 6. if bracketed_paste: `\x1b[?2004h`     (terminal.zig:424-426)
    /// 7. if auto_focus: `\x1b[?1004h`          (terminal.zig:428-430)
    ///
    /// Upstream's `setCursorPosition(1, 1, false)` between 2 and 3
    /// (renderer.zig:571) is state-only — it emits no bytes
    /// (terminal.zig:1458-1466).
    ///
    /// DIVERGENCE vs oc-tui setup (app.rs:7627-7643): crossterm's
    /// `EnterAlternateScreen` folds cursor-save INTO `?1049h` and the
    /// focus/paste enables lead the burst with NO modifyOtherKeys /
    /// kitty push at setup (kitty is pushed by terminal_proto later).
    /// Upstream zig order wins; differences recorded per-sequence in
    /// `modes`.
    /// The teardown byte-stream for the given live state — shared by
    /// [`Terminal::teardown`] and the panic-hook epilogue snapshot.
    fn write_epilogue(st: &TerminalState, out: &mut dyn io::Write) -> io::Result<()> {
        // The kitty graphics pop (the A1-registered gap, closed by the
        // image lane C3): an IMAGE session (kitty graphics were
        // emitted) deletes them before the mode resets — upstream's
        // `performShutdownSequence` walks its committed kitty
        // placements and writes a per-id `a=d,d=I,i=<id>` delete for
        // each BEFORE `resetState` (renderer.zig:602-618, the delete
        // shape at terminal-image.zig:234). The port emits the
        // delete-ALL spelling (`\x1b_Ga=d,d=a\x1b\\`) the consumer
        // standardized (oc-tui terminal_proto.rs:53, auditC P13 —
        // app.rs:9878-9881 gates on a non-None graphics protocol);
        // per-placement id tracking arrives with the buffer
        // rasterization lane. Never sent by default (no image
        // session → byte-identical epilogue).
        if st.kitty_graphics_session {
            out.write_all(b"\x1b_Ga=d,d=a\x1b\\")?;
        }
        out.write_all(modes::SHOW_CURSOR)?; // terminal.zig:283
        out.write_all(b"\x1b[0m")?; // terminal.zig:284 (ansi.ANSI.reset)
        out.write_all(modes::RESET_MOUSE_POINTER)?; // terminal.zig:285
        if st.kitty_keyboard {
            out.write_all(modes::KITTY_POP)?; // terminal.zig:289
        }
        if st.modify_other_keys {
            out.write_all(modes::MODIFY_OTHER_KEYS_DISABLE)?; // terminal.zig:293
        }
        if st.mouse_was_enabled {
            // Best-effort even if tracked state drifted
            // (terminal.zig:296-298 -> 946-951).
            out.write_all(modes::MOUSE_DISABLE_ALL)?;
        }
        if st.bracketed_paste {
            out.write_all(modes::BRACKETED_PASTE_DISABLE)?; // terminal.zig:301
        }
        if st.focus_tracking {
            out.write_all(modes::FOCUS_DISABLE)?; // terminal.zig:305
        }
        if st.alt_screen {
            out.write_all(modes::EXIT_ALT_SCREEN)?; // terminal.zig:309
        }
        out.write_all(modes::RESET_TERMINAL_TITLE)?; // terminal.zig:328
        out.write_all(modes::RESET_CURSOR_COLOR_FALLBACK)?; // renderer.zig:642
        out.write_all(modes::RESET_CURSOR_COLOR)?; // renderer.zig:643
        out.write_all(modes::DEFAULT_CURSOR_STYLE)?; // renderer.zig:644
        out.write_all(modes::SHOW_CURSOR)?; // renderer.zig:645
        out.flush()
    }

    /// Acquire the terminal: raw mode (Node's `setRawMode(true)`), then
    /// the setup burst in `setupTerminalWithoutDetection` order
    pub fn new(mut tty: T, mut out: W, opts: &SetupOptions) -> io::Result<Self> {
        let termios_save = tty.raw_mode()?;
        let mut state = TerminalState {
            alt_screen: opts.use_alt_screen,
            bracketed_paste: opts.bracketed_paste,
            focus_tracking: opts.auto_focus,
            ..TerminalState::default()
        };

        let result = (|| -> io::Result<()> {
            out.write_all(modes::SAVE_CURSOR)?; // renderer.zig:563
            if opts.use_alt_screen {
                out.write_all(modes::ENTER_ALT_SCREEN)?; // renderer.zig:566
            } else if opts.reserve_surface_height > 0 {
                // The non-alt reservation arm (renderer.zig:566-569):
                // `makeRoomForRendererOutput(@max(height, 1))` — the
                // split-footer runtime base (D1b-core). Upstream's
                // `setCursorPosition(1, 1, false)` right after
                // (renderer.zig:571) is state-only, no bytes.
                modes::make_room_for_renderer_output(
                    &mut out,
                    opts.reserve_surface_height.max(1),
                )?;
            }
            out.write_all(modes::MODIFY_OTHER_KEYS_ENABLE)?; // terminal.zig:410
            state.modify_other_keys = true;

            if opts.kitty_keyboard_flags > 0 {
                // terminal.zig:413-418: MOK off before the kitty push.
                out.write_all(modes::MODIFY_OTHER_KEYS_DISABLE)?;
                state.modify_other_keys = false;
                modes::kitty_push(&mut out, opts.kitty_keyboard_flags)?;
                state.kitty_keyboard = true;
            }

            if opts.use_mouse {
                // renderer.zig:3367-3371's enableMouse — the consumer
                // pushes right after setup, as oc-tui does.
                modes::mouse_enable(&mut out, opts.mouse_movement)?;
                // mouse_was_enabled arms the shutdown path even if a
                // later best-effort disable fails (terminal.zig:965-967).
                state.mouse_was_enabled = true;
            }

            if opts.bracketed_paste {
                out.write_all(modes::BRACKETED_PASTE_ENABLE)?; // terminal.zig:425
            }
            if opts.auto_focus {
                out.write_all(modes::FOCUS_ENABLE)?; // terminal.zig:429
            }
            out.flush()
        })();

        match result {
            Ok(()) => {
                // Panic-hook registration (Phase A design: RAII +
                // panic-safe raw mode). Only real devices expose an fd
                // snapshot; mocks stay out of the global registry.
                let panic_fd = tty.panic_restore_fd();
                if let Some(fd) = panic_fd {
                    let mut epilogue = Vec::new();
                    let _ = Self::write_epilogue(&state, &mut epilogue);
                    panic_teardown::register(panic_teardown::Entry {
                        epilogue,
                        fd,
                        attrs: termios_save.attrs.clone(),
                    });
                }
                Ok(Self {
                    tty,
                    out,
                    state,
                    termios_save: Some(termios_save),
                    torn_down: false,
                    panic_fd,
                    probe: None,
                    caps: crate::renderer::TerminalCapabilities::default(),
                    theme: super::theme::ThemeWatch::default(),
                    sgr_drained: 0,
                    size: None,
                })
            }
            Err(e) => {
                // Best-effort: undo raw mode before propagating.
                let _ = tty.restore(&termios_save);
                Err(e)
            }
        }
    }

    /// The full epilogue — `performShutdownSequence` (renderer.zig:600)
    /// whose body is `resetState` (terminal.zig:282-335) plus the
    /// shutdown tail (renderer.zig:642-645):
    ///
    /// 1. `\x1b[?25h`        showCursor            (terminal.zig:283)
    /// 2. `\x1b[0m`          SGR reset             (terminal.zig:284)
    /// 3. `\x1b]22;\x1b\\`   resetMousePointer     (terminal.zig:285)
    /// 4. if kitty: `\x1b[<u`                      (terminal.zig:288-290)
    /// 5. if modifyOtherKeys: `\x1b[>4;0m`         (terminal.zig:292-294)
    /// 6. if mouse was ever enabled:
    ///    `\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l`
    ///    (terminal.zig:296-298, 946-951)
    /// 7. if paste: `\x1b[?2004l`                  (terminal.zig:300-302)
    /// 8. if focus: `\x1b[?1004l`                  (terminal.zig:304-306)
    /// 9. `\x1b[?1049l`      exitAltScreen         (terminal.zig:308-309)
    /// 10. `\x1b]0;\x07`     setTerminalTitle("")  (terminal.zig:328)
    /// 11. `\x1b]12;default\x07` `\x1b]112\x07` `\x1b[0 q`
    ///     cursor color + DECSCUSR reset (renderer.zig:642-644)
    /// 12. `\x1b[?25h`      the Ghostty-cursor showCursor retry
    ///     (renderer.zig:645,652)
    ///
    /// then flush and restore cooked mode (`setRawMode(false)`).
    ///
    /// Upstream's `?2031l` (color-scheme-updates off, terminal.zig:324)
    /// never fires here: A1's setup doesn't enable `?2031` (that rides
    /// the A2 theme-probe family). The inline-viewport / clear-screen
    /// branches (renderer.zig:631-637, alt-screen false only) are out
    /// of scope — alt-screen only (see module docs).
    ///
    /// CROSS-CHECK vs oc-tui teardown (app.rs:9646-9672): same net mode
    /// set (alt off, mouse pops under the same gate, ?2004l ?1004l,
    /// kitty pop, DECSCUSR reset) — oc-tui additionally pops graphics
    /// (`a=d,d=a`, image lanes) and writes no mouse-pointer/SGR/
    /// cursor-color resets; the graphics delete-all they emit is now the
    /// port's own (`note_kitty_graphics` arms it, image lane C3).
    pub fn teardown(&mut self) -> io::Result<()> {
        if self.torn_down {
            return Ok(()); // idempotent — Drop after explicit teardown
        }
        self.torn_down = true;
        if let Some(fd) = self.panic_fd.take() {
            // Deregister BEFORE the fd could close (Drop ordering).
            panic_teardown::clear_fd(fd);
        }

        let result = Self::write_epilogue(&self.state, &mut self.out);

        if let Some(save) = self.termios_save.take() {
            let _ = self.tty.restore(&save);
        }
        result
    }

    /// The scrollback tail-jump teardown (A3): leave the alt screen
    /// FIRST, write `lines` to the main screen (each followed by
    /// `\r\n`, flushed once after the last line), then run the regular
    /// mode/cursor epilogue — so the lines land in the host scrollback
    /// instead of vanishing with the alt buffer. Idempotent with
    /// [`Terminal::teardown`] / `Drop` exactly like the plain path.
    ///
    /// Upstream alignment (renderer.ts:4520-4609, `finalizeDestroy`):
    /// the upstream exit commits its shutdown bytes through
    /// `lib.destroyRenderer` and then drains the feed — there is NO
    /// alt-screen line dump upstream in 0.5.14 (the scrollback write
    /// family is split-footer only, renderer.ts:2339+, deferred to
    /// Phase B4 — GAPS-registered). This API is the Rust-side
    /// equivalent of that family's tail jump: alt exit → payload →
    /// epilogue, sharing [`Terminal::write_epilogue`] for the mode
    /// reset tail (the alt-screen exit inside it is skipped — already
    /// emitted so the payload could land on the main screen).
    ///
    /// CROSS-CHECK vs oc-tui (app.rs:9646-9672): oc-tui runs the FULL
    /// teardown (leave-alt, mode pops, DECSCUSR reset) first and only
    /// then writes its exit epilogue text (`store.exit_epilogue()` +
    /// `\n`); this path writes the payload between the alt exit and
    /// the mode pops, keeping cursor position semantics of the
    /// pre-teardown state for the payload. Byte-visible difference,
    /// deliberate: upstream zig's leave-alt ordering
    /// (terminal.zig:308-309) stays authoritative for everything
    /// except the payload insertion point.
    pub fn teardown_scrollback(&mut self, lines: &[&[u8]]) -> io::Result<()> {
        if self.torn_down {
            return Ok(()); // idempotent — teardown/Drop already ran
        }
        self.torn_down = true;
        if let Some(fd) = self.panic_fd.take() {
            panic_teardown::clear_fd(fd);
        }

        let mut state = self.state.clone();
        let result = (|| -> io::Result<()> {
            self.out.write_all(modes::EXIT_ALT_SCREEN)?; // terminal.zig:309
            state.alt_screen = false; // write_epilogue must not repeat it
            for line in lines {
                self.out.write_all(line)?;
                self.out.write_all(b"\r\n")?;
            }
            self.out.flush()?; // the tail flush — payload visible before resets
            Self::write_epilogue(&state, &mut self.out)
        })();

        if let Some(save) = self.termios_save.take() {
            let _ = self.tty.restore(&save);
        }
        result
    }

    /// The output seam (Phase A design ruling #8): the W the renderer's
    /// frame bytes will be pumped through (A2's `pump_output`).
    pub fn output(&mut self) -> &mut W {
        &mut self.out
    }

    /// The tty device (resize checks in A2 go through here).
    pub fn tty(&mut self) -> &mut T {
        &mut self.tty
    }

    /// Mark this as an image session (the C3 graphics teardown
    /// gate): the consumer calls this once it has EMITTED kitty
    /// graphics on this terminal — `teardown`/`Drop` then leads its
    /// epilogue with the graphics delete-all (`a=d,d=a`). The
    /// upstream analog is the renderer's committed-protocol ledger
    /// (`hasCommittedProtocol(.kitty)` + `currentImages`,
    /// renderer.zig:602-618); the oc-tui consumer gates on its
    /// graphics-protocol config (app.rs:9878-9881). No-op after
    /// teardown.
    pub fn note_kitty_graphics(&mut self) {
        if self.torn_down {
            return;
        }
        self.state.kitty_graphics_session = true;
    }

    /// Whether [`Terminal::teardown`] already ran.
    #[must_use]
    pub fn is_torn_down(&self) -> bool {
        self.torn_down
    }
}

impl<T: TtyDevice, W: io::Write> Drop for Terminal<T, W> {
    /// RAII guard: the teardown epilogue runs even on early returns and
    /// unwinding. Idempotent with an explicit [`Terminal::teardown`].
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// Install a chained panic hook that runs the teardown epilogue for
/// every LIVE terminal session (raw-mode restore + mode resets) before
/// deferring to the previous hook — the panic-safe raw-mode guard the
/// Phase A design mandates. Consumers decide whether to install (the
/// library never touches the global hook on its own).
///
/// The hook writes epilogue bytes to process stdout and restores
/// termios on each live session's fd (best-effort, lock-intolerant
/// contexts included); consumers driving a non-stdout `W` should keep
/// their own hook instead.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        panic_teardown::run_all();
        previous(info);
    }));
}

/// The panic-path teardown registry: terminals register the epilogue
/// snapshot + raw-mode restore info at `new` and deregister on
/// teardown/drop, so the hook only ever touches LIVE sessions.
pub(crate) mod panic_teardown {
    use std::io::Write;
    use std::sync::Mutex;

    #[derive(Clone)]
    pub(crate) struct Entry {
        pub epilogue: Vec<u8>,
        /// (fd, saved attrs) — fd stays valid while the owning
        /// terminal (and its UnixTty) is alive; entry deregistration
        /// happens before the fd closes.
        pub fd: i32,
        pub attrs: rustix::termios::Termios,
    }

    fn registry() -> &'static Mutex<Vec<Entry>> {
        static REGISTRY: std::sync::OnceLock<Mutex<Vec<Entry>>> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub(crate) fn register(entry: Entry) {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entry);
    }

    pub(crate) fn clear_fd(fd: i32) {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|e| e.fd != fd);
    }

    /// Best-effort: flush every live epilogue to stdout, then restore
    /// cooked mode on every live fd. Never panics.
    pub(crate) fn run_all() {
        let entries = registry().lock().unwrap_or_else(|e| e.into_inner()).clone();
        for entry in &entries {
            let _ = std::io::stdout().write_all(&entry.epilogue);
            let _ = std::io::stdout().flush();
            // SAFETY: fd is valid — the owning terminal deregistered on
            // drop BEFORE its fd could close.
            let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(entry.fd) };
            let _ =
                rustix::termios::tcsetattr(fd, rustix::termios::OptionalActions::Now, &entry.attrs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::tty::tests::MockTty;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A shareable writer so tests can snapshot the byte stream while
    /// the Terminal (which owns its W) is still live.
    #[derive(Clone, Default)]
    struct SharedBuf(Rc<RefCell<Vec<u8>>>);

    impl SharedBuf {
        fn snapshot(&self) -> Vec<u8> {
            self.0.borrow().clone()
        }
        fn clear(&self) {
            self.0.borrow_mut().clear();
        }
    }

    impl io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn full_options() -> SetupOptions {
        SetupOptions {
            use_mouse: true,
            kitty_keyboard_flags: 5,
            ..SetupOptions::default()
        }
    }

    /// renderer.zig:563-575 + terminal.zig:409-430 order, byte-exact.
    #[test]
    fn setup_burst_exact_bytes() {
        let out = SharedBuf::default();
        let opts = full_options();
        {
            let _t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        }
        // Drop appended the teardown epilogue; assert the setup PREFIX
        // byte-exactly (the epilogue itself is pinned below).
        let stream = out.snapshot();
        assert!(stream.starts_with(
            &b"\x1b[s\x1b[?1049h\x1b[>4;1m\x1b[>4;0m\x1b[>5u\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?1004h"[..]
        ));
        assert_eq!(
            stream,
            b"\x1b[s" // saveCursorState         renderer.zig:563
                .iter()
                .cloned()
                .chain(b"\x1b[?1049h".iter().cloned()) // enterAlt  renderer.zig:566
                .chain(b"\x1b[>4;1m".iter().cloned()) // MOK on     terminal.zig:410
                .chain(b"\x1b[>4;0m".iter().cloned()) // MOK off    terminal.zig:415
                .chain(b"\x1b[>5u".iter().cloned()) // kitty push  terminal.zig:1010
                .chain(
                    b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h"
                        .iter()
                        .cloned()
                ) // terminal.zig:974-979
                .chain(b"\x1b[?2004h".iter().cloned()) // paste     terminal.zig:425
                .chain(b"\x1b[?1004h".iter().cloned()) // focus     terminal.zig:429
                // then the Drop-guard epilogue (pinned byte-exactly in
                // teardown_burst_exact_bytes — same bytes here):
                .chain(b"\x1b[?25h\x1b[0m\x1b]22;\x1b\\".iter().cloned())
                .chain(b"\x1b[<u".iter().cloned())
                .chain(
                    b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l"
                        .iter()
                        .cloned()
                )
                .chain(b"\x1b[?2004l\x1b[?1004l\x1b[?1049l".iter().cloned())
                .chain(
                    b"\x1b]0;\x07\x1b]12;default\x07\x1b]112\x07\x1b[0 q\x1b[?25h"
                        .iter()
                        .cloned()
                )
                .collect::<Vec<u8>>()[..],
            "setup burst must match upstream byte-for-byte"
        );
    }

    /// terminal.zig:282-335 + renderer.zig:642-645 order, byte-exact.
    #[test]
    fn teardown_burst_exact_bytes() {
        let out = SharedBuf::default();
        let opts = full_options();
        let mut t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        out.clear();
        t.teardown().unwrap();
        assert_eq!(
            out.snapshot(),
            b"\x1b[?25h" // showCursor            terminal.zig:283
                .iter()
                .cloned()
                .chain(b"\x1b[0m".iter().cloned()) // SGR reset      terminal.zig:284
                .chain(b"\x1b]22;\x1b\\".iter().cloned()) // mousePtr terminal.zig:285
                .chain(b"\x1b[<u".iter().cloned()) // kitty pop      terminal.zig:289
                // no MOK disable here: the kitty path turned MOK off
                // at setup (terminal.zig:413-418), mirroring zig state.
                .chain(
                    b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l"
                        .iter()
                        .cloned()
                ) // terminal.zig:946-951
                .chain(b"\x1b[?2004l".iter().cloned()) // paste off   terminal.zig:301
                .chain(b"\x1b[?1004l".iter().cloned()) // focus off   terminal.zig:305
                .chain(b"\x1b[?1049l".iter().cloned()) // exit alt    terminal.zig:309
                .chain(b"\x1b]0;\x07".iter().cloned()) // title reset terminal.zig:328
                .chain(b"\x1b]12;default\x07".iter().cloned()) // renderer.zig:642
                .chain(b"\x1b]112\x07".iter().cloned()) // renderer.zig:643
                .chain(b"\x1b[0 q".iter().cloned()) // renderer.zig:644
                .chain(b"\x1b[?25h".iter().cloned()) // renderer.zig:645
                .collect::<Vec<u8>>(),
        );
    }

    /// Without kitty the MOK on/off pair collapses to just on
    /// (terminal.zig:409-418), and teardown then pops MOK
    /// (terminal.zig:292-294) before the mouse disables.
    #[test]
    fn no_kitty_keeps_modify_other_keys_on() {
        let out = SharedBuf::default();
        let opts = SetupOptions {
            use_mouse: true,
            ..SetupOptions::default()
        };
        let mut t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        assert_eq!(
            out.snapshot(),
            b"\x1b[s\x1b[?1049h\x1b[>4;1m\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?1004h"
        );
        out.clear();
        t.teardown().unwrap();
        // kitty pop absent, MOK disable present (terminal.zig:292-294).
        let bytes = out.snapshot();
        assert!(bytes.starts_with(b"\x1b[?25h\x1b[0m\x1b]22;\x1b\\\x1b[>4;0m"));
        assert!(!bytes.windows(4).any(|w| w == b"\x1b[<u"));
    }

    /// `use_mouse: false` clips the whole mouse family from setup AND
    /// teardown (oc-tui's opt-out semantics — mouse_modes.rs:95-102).
    #[test]
    fn mouse_false_clips_mouse_sequences() {
        let out = SharedBuf::default();
        let opts = SetupOptions {
            use_mouse: false,
            ..SetupOptions::default()
        };
        let mut t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        assert!(!out.snapshot().windows(7).any(|w| w == b"\x1b[?1000h"));
        out.clear();
        t.teardown().unwrap();
        let bytes = out.snapshot();
        assert!(!bytes.windows(7).any(|w| w == b"\x1b[?1000l"));
        assert!(!bytes.windows(7).any(|w| w == b"\x1b[?1006l"));
    }

    /// teardown is idempotent: a second call writes nothing and
    /// restores nothing; Drop after explicit teardown stays silent too.
    #[test]
    fn teardown_is_idempotent() {
        let out = SharedBuf::default();
        let opts = SetupOptions::default();
        let mut t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        t.teardown().unwrap();
        out.clear();
        t.teardown().unwrap();
        assert!(
            out.snapshot().is_empty(),
            "second teardown must emit nothing"
        );
        let restores = t.tty().restore_calls;
        drop(t); // Drop guard must not double-run
        assert!(out.snapshot().is_empty());
        assert_eq!(restores, 1);
    }

    /// The Drop guard fires the full epilogue when the consumer never
    /// calls teardown explicitly.
    #[test]
    fn drop_guard_runs_teardown() {
        let out = SharedBuf::default();
        let opts = full_options();
        {
            let t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
            assert!(!t.is_torn_down());
        }
        let bytes = out.snapshot();
        assert!(bytes.windows(8).any(|w| w == b"\x1b[?1049l"));
        assert!(bytes.windows(4).any(|w| w == b"\x1b[<u"));
    }

    /// Raw mode restore happens exactly once per session (the mock
    /// counts calls), whether teardown is explicit, Drop, or both.
    #[test]
    fn raw_mode_restored_once() {
        let out = SharedBuf::default();
        let opts = SetupOptions::default();
        let mut t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        assert_eq!(t.tty().raw_calls, 1);
        t.teardown().unwrap();
        let restores = t.tty().restore_calls;
        assert_eq!(restores, 1);
        drop(t);
        assert_eq!(restores, 1, "Drop must not restore twice");
    }

    /// teardown_scrollback: alt exit leads, each line lands verbatim
    /// (+ `\r\n`), then the regular epilogue WITHOUT a second alt exit.
    /// Escapes and UTF-8 in lines pass through byte-for-byte.
    #[test]
    fn teardown_scrollback_byte_order() {
        let out = SharedBuf::default();
        let opts = full_options();
        let mut t = Terminal::new(MockTty::new(), out.clone(), &opts).unwrap();
        out.clear();
        t.teardown_scrollback(&[b"line one \xe2\x9c\x93", b"\x1b[31mred tail\x1b[0m"])
            .unwrap();
        let bytes = out.snapshot();
        let expected: Vec<u8> = b"\x1b[?1049l"
            .iter()
            .cloned()
            .chain(b"line one \xe2\x9c\x93\r\n".iter().cloned())
            .chain(b"\x1b[31mred tail\x1b[0m\r\n".iter().cloned())
            // then the epilogue — same bytes as teardown_burst_exact_bytes
            // MINUS the `\x1b[?1049l` (already emitted above)
            .chain(b"\x1b[?25h".iter().cloned())
            .chain(b"\x1b[0m".iter().cloned())
            .chain(b"\x1b]22;\x1b\\".iter().cloned())
            .chain(b"\x1b[<u".iter().cloned())
            .chain(
                b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l"
                    .iter()
                    .cloned(),
            )
            .chain(b"\x1b[?2004l".iter().cloned())
            .chain(b"\x1b[?1004l".iter().cloned())
            .chain(b"\x1b]0;\x07".iter().cloned())
            .chain(b"\x1b]12;default\x07".iter().cloned())
            .chain(b"\x1b]112\x07".iter().cloned())
            .chain(b"\x1b[0 q".iter().cloned())
            .chain(b"\x1b[?25h".iter().cloned())
            .collect();
        assert_eq!(bytes, expected);
        // exactly ONE alt-screen exit in the whole stream
        assert_eq!(bytes.windows(8).filter(|w| *w == b"\x1b[?1049l").count(), 1);
        // idempotent: a follow-up teardown (or Drop) is silent
        t.teardown().unwrap();
        drop(t);
        assert_eq!(out.snapshot(), expected);
    }

    /// The panic hook chains: the previous hook still runs after ours.
    /// (Entries are only registered by real devices; mock sessions
    /// never join the registry, so this exercises chain order only.)
    #[test]
    fn panic_hook_chains_previous() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static RAN: AtomicBool = AtomicBool::new(false);
        RAN.store(false, Ordering::SeqCst);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            RAN.store(true, Ordering::SeqCst);
        }));
        install_panic_hook();
        let result = std::panic::catch_unwind(|| panic!("hook chain probe"));
        assert!(result.is_err());
        assert!(RAN.load(Ordering::SeqCst), "chained hook must run");
        std::panic::set_hook(prev);
    }
}
