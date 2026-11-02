//! The input/output pumps and resize watch — the runtime-agnostic,
//! blocking-API slice the Phase A design ruled on (no threads here;
//! consumers wrap).
//!
//! * `feed_input`/`pump_input` — stdin bytes split between the probe
//!   state machine and the renderer's key/mouse feed
//!   (upstream: the TS StdinParser capability-response branch,
//!   renderer.ts:3395-3416);
//! * `pump_output` — drain `renderer.sgr_stream()` into `W` wrapped in
//!   the `?2026` synchronized-update envelope, then flush;
//! * `check_resize` — TIOCGWINSZ reconciliation driving
//!   `renderer.resize` + the `Resize` event (upstream: the Node host's
//!   SIGWINCH → `processResize`, renderer.ts:3190-3198).

use std::io;

use super::probe::CapabilityProbe;
use super::session::Terminal;
use super::tty::TtyDevice;
use crate::renderer::{CliRenderEvent, CliRenderer, WidthMethod};

impl<T: TtyDevice, W: io::Write> Terminal<T, W> {
    /// Arm the capability probe and write the query burst
    /// (terminal.zig:347-399 — see `probe::write_probe_burst` for the
    /// gated-subset divergences). Responses arriving on
    /// [`Terminal::feed_input`] classify into
    /// [`crate::renderer::TerminalCapabilities`].
    pub fn probe_capabilities(&mut self) -> io::Result<()> {
        let timeout = super::probe::PROBE_TIMEOUT;
        let probe = CapabilityProbe::new(timeout);
        super::probe::write_probe_burst(&mut self.out)?;
        self.out.flush()?;
        self.probe = Some(probe);
        Ok(())
    }

    /// `enableDetectedFeatures` (terminal.zig:399-440) — the
    /// probe-settle feature burst. Runs AUTOMATICALLY when
    /// [`Terminal::feed_input`] finishes the probe (both the
    /// capability-complete and the timeout paths); this pub seam
    /// exists for consumers that want to re-run it manually
    /// (upstream's caller-owned `use_kitty_keyboard` decision
    /// surface).
    ///
    /// The gated subset (the three spans the Gate A oracle mandated;
    /// the rest is registered in `terminal/mod.rs`):
    ///
    /// * `\x1b[?2027h` when `caps.unicode` is `Unicode`/`UnicodeWide`
    ///   and `caps.explicit_width` is false (terminal.zig:420-422 —
    ///   the gate is copied verbatim, including the "unknown default
    ///   arms it" consequence: `WidthMethod::Unicode` is the zig
    ///   fresh-state default, terminal.zig:18);
    /// * `\x1b[?2004h` when `caps.bracketed_paste`
    ///   (terminal.zig:424-426 — `setBracketedPaste(true)` writes
    ///   unconditionally under the caps gate, and updates the state
    ///   flag the teardown epilogue reads);
    /// * `\x1b[?1004h` when `caps.focus_tracking`
    ///   (terminal.zig:428-430, same `setFocusTracking` shape).
    ///
    /// Skipped, gap-registered: the modifyOtherKeys/kitty re-push
    /// (terminal.zig:409-418 — A1's setup burst already owns that
    /// state machine via `SetupOptions`, and upstream's re-check is
    /// state-gated no-op for it) and the 2031/theme re-query tail
    /// (terminal.zig:432-439 — the probe burst already sent both, and
    /// this port tracks no state for them).
    ///
    /// Ordering divergence (A1 setup runs BEFORE the probe, upstream
    /// after): `SetupOptions` may already have enabled ?2004/?1004;
    /// this pass re-affirms per caps exactly like upstream and never
    /// disables — disable-on-unsupported is not an upstream behavior.
    pub fn enable_detected_features(&mut self) -> io::Result<()> {
        if (self.caps.unicode == WidthMethod::Unicode
            || self.caps.unicode == WidthMethod::UnicodeWide)
            && !self.caps.explicit_width
        {
            self.out.write_all(super::modes::UNICODE_SET)?; // terminal.zig:420-422
        }
        if self.caps.bracketed_paste {
            self.out.write_all(super::modes::BRACKETED_PASTE_ENABLE)?; // terminal.zig:424-426
            self.state.bracketed_paste = true;
        }
        if self.caps.focus_tracking {
            self.out.write_all(super::modes::FOCUS_ENABLE)?; // terminal.zig:428-430
            self.state.focus_tracking = true;
        }
        self.out.flush()
    }

    /// Probe settle: adopt the final snapshot, arm the theme watch
    /// from the probe's captured OSC 10/11 answers, and run
    /// [`Terminal::enable_detected_features`]. Write errors on the
    /// auto path are best-effort (the Vec-returning feed cannot
    /// propagate them); the pub seam surfaces them for manual calls.
    fn settle_probe(
        &mut self,
        probe: &CapabilityProbe,
        renderer: &mut CliRenderer,
        events: &mut Vec<CliRenderEvent>,
    ) {
        self.caps = probe.capabilities().clone();
        self.theme
            .arm(probe.last_osc10_fg, probe.last_osc11_bg, renderer, events);
        let _ = self.enable_detected_features();
    }

    /// Route raw stdin bytes: probe responses (DA1 `CSI ?..c`, DECRPM
    /// `CSI ?..$y`, kitty `CSI ?..u`, OSC 10/11, DCS xtversion /
    /// xtgettcap) feed the capability state machine; everything else
    /// passes to `renderer.feed_bytes` byte-for-byte, untouched.
    ///
    /// `Capabilities` events are returned AND emitted through the
    /// renderer's listener pipe (`emit_terminal_event`, the
    /// renderer.ts:3415 boundary). The 5s deadline (from probe arm)
    /// finishes the probe with unknown-field defaults; settling also
    /// runs `enable_detected_features` (terminal.zig:399) and arms
    /// the A4 theme watch. After settle, OSC 10/11 answers and ?997
    /// notifications route through the theme watch instead of the key
    /// stream (upstream: `themeModeHandler`, renderer.ts:1204-1216).
    pub fn feed_input(&mut self, renderer: &mut CliRenderer, bytes: &[u8]) -> Vec<CliRenderEvent> {
        let mut events = Vec::new();
        if let Some(mut probe) = self.probe.take() {
            probe.feed(renderer, bytes, &mut events);
            if probe.is_finished() {
                self.settle_probe(&probe, renderer, &mut events);
            } else {
                self.probe = Some(probe);
            }
        } else {
            // Best-effort write on the ?997 re-query path (same
            // reasoning as settle_probe).
            let _ = self
                .theme
                .route(&mut self.out, renderer, bytes, &mut events);
        }
        events
    }

    /// One input tick with REAL poll(2) semantics (the Gate A #4
    /// honesty fix; upstream has no direct counterpart — the Node
    /// host's reads are evented — so this is a library-side API
    /// correction, not a port): wait up to `timeout_ms` for input to
    /// arrive on the tty's input fd, then read and
    /// [`Terminal::feed_input`] whatever landed.
    ///
    /// * `timeout_ms == 0` — non-blocking (poll(2) with a zero
    ///   timeout: FIONREAD-then-read, the A2/A3 behavior);
    /// * `timeout_ms > 0` — blocks at most `timeout_ms`; a timeout
    ///   with no input returns an EMPTY event vec (the probe
    ///   deadline still gets its feed tick first).
    ///
    /// `POLLERR`/`POLLHUP` readiness with an empty queue takes the
    /// same no-input path.
    pub fn pump_input(
        &mut self,
        renderer: &mut CliRenderer,
        timeout_ms: u64,
    ) -> io::Result<Vec<CliRenderEvent>> {
        let fd = self.tty.input_fd();
        if timeout_ms > 0 {
            let mut fds = [rustix::event::PollFd::new(
                &fd,
                rustix::event::PollFlags::IN,
            )];
            let timeout = rustix::event::Timespec {
                tv_sec: (timeout_ms / 1000) as i64,
                tv_nsec: ((timeout_ms % 1000) * 1_000_000) as i64,
            };
            let ready = rustix::event::poll(&mut fds, Some(&timeout))?;
            if ready == 0 {
                return Ok(self.probe_deadline_tick(renderer));
            }
        }
        let available = rustix::io::ioctl_fionread(fd)? as usize;
        if available == 0 {
            return Ok(self.probe_deadline_tick(renderer));
        }
        let mut buf = vec![0u8; available];
        let n = rustix::io::read(fd, &mut buf)?;
        buf.truncate(n);
        Ok(self.feed_input(renderer, &buf))
    }

    /// The no-input feed tick: only the probe's wall-clock deadline
    /// can produce events here (its timeout-finish path).
    fn probe_deadline_tick(&mut self, renderer: &mut CliRenderer) -> Vec<CliRenderEvent> {
        if let Some(mut probe) = self.probe.take() {
            let mut events = Vec::new();
            probe.feed(renderer, &[], &mut events);
            if probe.is_finished() {
                self.settle_probe(&probe, renderer, &mut events);
            } else {
                self.probe = Some(probe);
            }
            return events;
        }
        Vec::new()
    }

    /// Drain the renderer's SGR stream into `W`: the bytes accumulated
    /// since the last pump are wrapped in the `?2026`
    /// synchronized-update envelope (`syncSet`/`syncReset`) and
    /// explicitly flushed.
    ///
    /// Upstream shape: `beginRenderFrame` writes `syncSet` +
    /// hideCursor unconditionally once a frame produces output
    /// (renderer.zig:1842-1845), and the frame tail closes with
    /// `syncReset` ONLY when the frame actually started — true no-op
    /// frames emit nothing (renderer.zig:3029-3032). `caps.sync`
    /// (DECRPM `2026;*$y`) is recorded but never gates the envelope
    /// upstream; this port matches that (ungated).
    pub fn pump_output(&mut self, renderer: &mut CliRenderer) -> io::Result<()> {
        let stream = renderer.sgr_stream();
        let drained_len = self.sgr_drained.min(stream.len());
        let chunk = &stream.as_bytes()[drained_len..];
        if chunk.is_empty() {
            return Ok(()); // no-op frame — no envelope (renderer.zig:3029)
        }
        self.out.write_all(super::modes::SYNC_BEGIN)?;
        self.out.write_all(chunk)?;
        self.out.write_all(super::modes::SYNC_END)?;
        self.out.flush()?;
        self.sgr_drained = stream.len();
        Ok(())
    }

    /// TIOCGWINSZ reconciliation (the SIGWINCH-poll stand-in the Phase
    /// A design ruled): a size change drives `renderer.resize(w, h)`
    /// (renderer.rs — the `processResize` port) and emits the `Resize`
    /// event (renderer.ts:3198), returning the new `(cols, rows)`.
    pub fn check_resize(&mut self, renderer: &mut CliRenderer) -> io::Result<Option<(u16, u16)>> {
        let (cols, rows) = self.tty.window_size()?;
        if self.size == Some((cols, rows)) {
            return Ok(None);
        }
        self.size = Some((cols, rows));
        renderer.resize(u32::from(cols), u32::from(rows));
        renderer.emit_terminal_event(CliRenderEvent::Resize {
            width: u32::from(cols),
            height: u32::from(rows),
        });
        Ok(Some((cols, rows)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{CliRenderEvent, CliRendererConfig};
    use crate::terminal::probe::write_probe_burst;
    use crate::terminal::tty::tests::MockTty;
    use std::cell::RefCell;
    use std::os::fd::AsFd;
    use std::rc::Rc;

    /// SharedBuf from session's tests, replicated (session's is private
    /// to its module).
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

    /// A writer that counts flushes (pump_output must flush).
    #[derive(Default)]
    struct CountingFlush {
        buf: Vec<u8>,
        flushes: u32,
    }

    /// Shareable wrapper (the Terminal owns its W).
    #[derive(Clone, Default)]
    struct CountingW(Rc<RefCell<CountingFlush>>);

    impl io::Write for CountingW {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().buf.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.borrow_mut().flushes += 1;
            Ok(())
        }
    }

    fn renderer() -> CliRenderer {
        crate::renderer::create_cli_renderer(CliRendererConfig::default())
    }

    /// probe_capabilities writes the burst, and responses classify +
    /// pass through cleanly.
    #[test]
    fn probe_burst_via_terminal() {
        let out = SharedBuf::default();
        let mut term = Terminal::new(MockTty::new(), out.clone(), &Default::default()).unwrap();
        out.clear(); // drop the setup burst
        term.probe_capabilities().unwrap();
        let mut expected = Vec::new();
        write_probe_burst(&mut expected).unwrap();
        assert_eq!(out.snapshot(), expected);
    }

    /// Real key/mouse/focus bytes flow through to renderer.feed_bytes
    /// untouched while the probe is live — observed via the renderer's
    /// event pipe (the focus sequence `\x1b[I` reaches the parser and
    /// comes back as a TerminalFocus event, renderer.rs feed lane).
    #[test]
    fn passthrough_bytes_reach_feed_bytes() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<CliRenderEvent>>> = Arc::new(Mutex::new(Vec::new()));

        let mut term = Terminal::new(MockTty::new(), Vec::new(), &Default::default()).unwrap();
        term.probe_capabilities().unwrap();
        let mut r = renderer();
        {
            let sink = seen.clone();
            r.on_event(move |e| sink.lock().unwrap().push(e.clone()));
        }
        // A canned probe response followed by the focus-in sequence.
        let events = term.feed_input(&mut r, b"\x1b[?2026;2$y\x1b[I");
        // capability event for the DECRPM, nothing else from the probe
        assert!(matches!(
            events.as_slice(),
            [CliRenderEvent::Capabilities(_)]
        ));
        assert!(term.probe.is_some(), "probe still live (no DA1 yet)");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, CliRenderEvent::TerminalFocus)),
            "focus bytes reached feed_bytes and emitted TerminalFocus"
        );
        assert!(term.probe.as_ref().unwrap().capabilities().sync);
    }

    /// A canned reply stream that finishes the probe AND arms every
    /// enable_detected_features gate (unicode DECRPM + 2004 + 1004).
    const FEATUREFUL_REPLIES: &[u8] =
        b"\x1b[?2027;2$y\x1b[?2004;1$y\x1b[?1004;1$y\x1b[?0u\x1b[?62;22c";
    /// Replies that keep every gate closed: tmux xtversion pins
    /// `unicode = Wcwidth` (terminal.zig:1380-1382) and no 2004/1004
    /// DECRPMs arrive.
    const FEATURELESS_REPLIES: &[u8] = b"\x1bP>|tmux 3.5a\x1b\\\x1b[?0u\x1b[?1;2c";

    /// Probe settle runs enable_detected_features: the ?2027h gate
    /// (terminal.zig:420-422) plus ?2004h/?1004h (terminal.zig:424-430)
    /// land on the wire in upstream order.
    #[test]
    fn enable_detected_features_gates_bytes() {
        let out = SharedBuf::default();
        let mut term = Terminal::new(MockTty::new(), out.clone(), &Default::default()).unwrap();
        let mut r = renderer();
        term.probe_capabilities().unwrap();
        out.clear(); // drop the probe burst
        let events = term.feed_input(&mut r, FEATUREFUL_REPLIES);
        assert_eq!(
            out.snapshot(),
            b"\x1b[?2027h\x1b[?2004h\x1b[?1004h",
            "gated enable burst in enableDetectedFeatures order"
        );
        // state updates drive the teardown epilogue's ?2004l/?1004l
        drop(term); // Drop writes the epilogue into the same SharedBuf
        let tail = out.snapshot();
        assert!(tail.windows(8).any(|w| w == b"\x1b[?2004l"));
        assert!(tail.windows(8).any(|w| w == b"\x1b[?1004l"));
        // the settle's Capabilities event still fires
        assert!(events
            .iter()
            .any(|e| matches!(e, CliRenderEvent::Capabilities(_))));
    }

    /// Caps that fail every gate emit nothing: Wcwidth unicode skips
    /// ?2027h, missing 2004/1004 DECRPMs skip the rest
    /// (terminal.zig:420-430 — all three gates are caps-conditional).
    #[test]
    fn enable_detected_features_silent_when_unsupported() {
        let out = SharedBuf::default();
        let mut term = Terminal::new(MockTty::new(), out.clone(), &Default::default()).unwrap();
        let mut r = renderer();
        term.probe_capabilities().unwrap();
        out.clear();
        term.feed_input(&mut r, FEATURELESS_REPLIES);
        assert!(
            out.snapshot().is_empty(),
            "no detected features to enable — zero bytes"
        );
    }

    /// Probe answers carrying both OSC 10/11 halves reduce the first
    /// theme mode AT settle (upstream's themeModeHandler sees the
    /// probe's own answers too, renderer.ts:1204-1216).
    #[test]
    fn theme_mode_from_probe_answers() {
        let mut term = Terminal::new(MockTty::new(), Vec::new(), &Default::default()).unwrap();
        let mut r = renderer();
        term.probe_capabilities().unwrap();
        let events = term.feed_input(
            &mut r,
            b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b]11;rgb:1010/1010/1010\x07\x1b[?0u\x1b[?1;2c",
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, CliRenderEvent::ThemeMode { .. }))
                .count(),
            1,
            "exactly one ThemeMode event at settle"
        );
        assert!(matches!(
            events.last(),
            Some(CliRenderEvent::ThemeMode {
                mode: crate::renderer::ThemeMode::Dark
            })
        ));
    }

    /// A late OSC 11 (the ?2031 change-notification cycle) never
    /// reaches the renderer's key stream, and the ?997 restart writes
    /// the OSC 10/11 re-query (renderer-theme-mode.ts:202-216). Key
    /// purity is observed via the renderer's key dispatch: only the
    /// real key byte arrives, no garbage from the theme units.
    #[test]
    fn late_osc11_rerouted_and_key_stream_clean() {
        use std::sync::{Arc, Mutex};
        let keys: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut term =
            Terminal::new(MockTty::new(), SharedBuf::default(), &Default::default()).unwrap();
        let mut r = renderer();
        let _sub = {
            let sink = keys.clone();
            r.key_input.on_key(
                crate::events::KeyHandlerEvent::Keypress,
                move |k: &mut crate::events::KeyEvent| sink.lock().unwrap().push(k.key.raw.clone()),
            )
        };
        term.probe_capabilities().unwrap();
        // settle without theme answers
        term.feed_input(&mut r, b"\x1b[?0u\x1b[?1;2c");
        // late pair + a real key in ONE stream: theme units consumed,
        // 'a' delivered, nothing else
        let events = term.feed_input(
            &mut r,
            b"\x1b]11;rgb:1010/1010/1010\x07\x1b]10;rgb:0000/0000/0000\x07a",
        );
        assert_eq!(*keys.lock().unwrap(), vec!["a".to_string()]);
        assert_eq!(
            events,
            vec![CliRenderEvent::ThemeMode {
                mode: crate::renderer::ThemeMode::Dark
            }],
            "the completed pair reduced to Dark"
        );
    }

    /// pump_output pairs the ?2026 envelope around fresh SGR bytes and
    /// flushes; a no-change pump emits nothing.
    #[test]
    fn pump_output_envelope_and_flush() {
        let out = CountingW::default();
        let mut term = Terminal::new(MockTty::new(), out.clone(), &Default::default()).unwrap();
        let mut r = renderer();
        // Drain whatever history the renderer arrived with.
        term.pump_output(&mut r).unwrap();
        out.0.borrow_mut().buf.clear();
        // No NEW frame bytes -> no envelope (renderer.zig:3029 no-op).
        term.pump_output(&mut r).unwrap();
        assert!(out.0.borrow().buf.is_empty());
        // Produce a frame: a resize forces a full render (resize -> render).
        r.resize(10, 2);
        term.pump_output(&mut r).unwrap();
        let buf = out.0.borrow();
        assert!(
            buf.buf.starts_with(b"\x1b[?2026h"),
            "syncSet leads (renderer.zig:1844)"
        );
        assert!(
            buf.buf.ends_with(b"\x1b[?2026l"),
            "syncReset closes (renderer.zig:3031)"
        );
        assert_eq!(buf.flushes, 2, "explicit flush per pumped frame");
        assert!(
            buf.buf.len() > b"\x1b[?2026h\x1b[?2026l".len(),
            "frame SGR bytes between the envelope"
        );
        // Second pump with no new frame -> nothing new emitted.
        drop(buf);
        let len_after_first = out.0.borrow().buf.len();
        term.pump_output(&mut r).unwrap();
        assert_eq!(out.0.borrow().buf.len(), len_after_first);
        assert_eq!(out.0.borrow().flushes, 2, "no-op pump must not flush again");
    }

    /// A mutable-size mock for the resize watch.
    struct ResizeTty {
        size: (u16, u16),
        file: std::fs::File,
    }

    impl TtyDevice for ResizeTty {
        fn raw_mode(&mut self) -> io::Result<crate::terminal::TermiosSave> {
            Ok(crate::terminal::TermiosSave::mock())
        }
        fn restore(&mut self, _: &crate::terminal::TermiosSave) -> io::Result<()> {
            Ok(())
        }
        fn input_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            self.file.as_fd()
        }
        fn window_size(&self) -> io::Result<(u16, u16)> {
            Ok(self.size)
        }
    }

    /// Resize changes drive renderer.resize + the Resize event; steady
    /// size reports None.
    #[test]
    fn check_resize_emits_event_on_change() {
        let tty = ResizeTty {
            size: (80, 24),
            file: std::fs::File::create("/dev/null").unwrap(),
        };
        let mut term = Terminal::new(tty, Vec::new(), &Default::default()).unwrap();
        let mut r = renderer();
        assert_eq!(term.check_resize(&mut r).unwrap(), Some((80, 24)));
        assert_eq!(r.config.width, 80);
        assert_eq!(term.check_resize(&mut r).unwrap(), None, "steady size");
        term.tty().size = (100, 30);
        assert_eq!(term.check_resize(&mut r).unwrap(), Some((100, 30)));
        assert_eq!(r.config.width, 100, "renderer.resize followed");
        assert_eq!(r.config.height, 30);
    }
}
