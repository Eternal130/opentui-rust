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
use crate::renderer::{CliRenderEvent, CliRenderer};

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

    /// Route raw stdin bytes: probe responses (DA1 `CSI ?..c`, DECRPM
    /// `CSI ?..$y`, kitty `CSI ?..u`, OSC 10/11, DCS xtversion /
    /// xtgettcap) feed the capability state machine; everything else
    /// passes to `renderer.feed_bytes` byte-for-byte, untouched.
    ///
    /// `Capabilities` events are returned AND emitted through the
    /// renderer's listener pipe (`emit_terminal_event`, the
    /// renderer.ts:3415 boundary). The 5s deadline (from probe arm)
    /// finishes the probe with unknown-field defaults.
    pub fn feed_input(
        &mut self,
        renderer: &mut CliRenderer,
        bytes: &[u8],
    ) -> Vec<CliRenderEvent> {
        let mut events = Vec::new();
        if let Some(mut probe) = self.probe.take() {
            probe.feed(renderer, bytes, &mut events);
            if !probe.is_finished() {
                self.probe = Some(probe);
            }
        } else {
            renderer.feed_bytes(bytes);
        }
        events
    }

    /// One non-blocking input tick: FIONREAD the tty's input fd, read
    /// whatever is buffered, and [`Terminal::feed_input`] it. Timeout
    /// (ms) is accepted for API-shape parity with the consumer's poll
    /// loop — this implementation never blocks (FIONREAD-then-read), so
    /// the value is only consulted for the probe deadline, which is
    /// wall-clock.
    pub fn pump_input(
        &mut self,
        renderer: &mut CliRenderer,
        _timeout_ms: u64,
    ) -> io::Result<Vec<CliRenderEvent>> {
        let fd = self.tty.input_fd();
        let available = rustix::io::ioctl_fionread(fd)? as usize;
        if available == 0 {
            // Deadline check still needs a feed tick.
            if let Some(mut probe) = self.probe.take() {
                let mut events = Vec::new();
                probe.feed(renderer, &[], &mut events);
                if !probe.is_finished() {
                    self.probe = Some(probe);
                }
                return Ok(events);
            }
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; available];
        let n = rustix::io::read(fd, &mut buf)?;
        buf.truncate(n);
        Ok(self.feed_input(renderer, &buf))
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
    pub fn check_resize(
        &mut self,
        renderer: &mut CliRenderer,
    ) -> io::Result<Option<(u16, u16)>> {
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
        let mut term =
            Terminal::new(MockTty::new(), out.clone(), &Default::default()).unwrap();
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
        // commit 1: classifiers are stubbed, so no Capabilities event
        // yet — the DECRPM is merely consumed by the state machine.
        assert!(events.is_empty());
        assert!(term.probe.is_some(), "probe still live (no DA1 yet)");
        assert!(
            seen.lock().unwrap().iter().any(|e| matches!(e, CliRenderEvent::TerminalFocus)),
            "focus bytes reached feed_bytes and emitted TerminalFocus"
        );
        // (classification assertions land with commit 2's classifiers)
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
        assert!(buf.buf.starts_with(b"\x1b[?2026h"), "syncSet leads (renderer.zig:1844)");
        assert!(buf.buf.ends_with(b"\x1b[?2026l"), "syncReset closes (renderer.zig:3031)");
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
