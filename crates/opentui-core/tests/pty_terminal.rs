//! PTY integration tests for the terminal backend (lane A3) — every
//! test drives a PRIVATE `posix_openpt` pair (master/slave), never a
//! real terminal. The slave side goes through the real `UnixTty`
//! (`TtyDevice`), the master side is the "terminal emulator" the test
//! speaks: it reads the setup/probe/teardown bursts the backend writes
//! and injects canned responses / key bytes / TIOCSWINSZ changes.
//!
//! All reads use short wall-clock deadlines (poll(2) + `Instant`,
//! never long sleeps); the suite is CI-safe without a controlling TTY.

#![cfg(feature = "terminal-backend")]

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use opentui_core::renderer::{CliRenderEvent, CliRenderer, CliRendererConfig};
use opentui_core::terminal::probe::write_probe_burst;
use opentui_core::terminal::{SetupOptions, Terminal, UnixTty};

/// A `posix_openpt` pair: master (the test-as-emulator side) + the
/// slave fd handed to `UnixTty::from_fd`.
struct PtyPair {
    master: OwnedFd,
    slave: OwnedFd,
}

fn open_pty() -> PtyPair {
    use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
    let master = openpt(
        OpenptFlags::RDWR
            .union(OpenptFlags::NOCTTY)
            .union(OpenptFlags::CLOEXEC),
    )
    .expect("posix_openpt");
    grantpt(&master).expect("grantpt");
    unlockpt(&master).expect("unlockpt");
    let name = ptsname(&master, Vec::new()).expect("ptsname");
    let slave = rustix::fs::open(
        name.to_bytes(),
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .expect("open slave pty");
    PtyPair { master, slave }
}

/// The Terminal's `W`: every write lands on the PTY SLAVE — the
/// program side of the pair, exactly like a real process's stdout —
/// and becomes readable at the master ("the terminal emulator").
struct SlaveWriter {
    fd: OwnedFd,
}

impl SlaveWriter {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl io::Write for SlaveWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(rustix::io::write(self.as_fd(), buf)?)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Read from `fd` until `deadline`, accumulating everything that
/// arrives (poll(2) with 20ms ticks — no long sleeps; a silent fd
/// just returns what landed so far).
fn read_available(fd: BorrowedFd<'_>, deadline: Instant) -> Vec<u8> {
    // Non-blocking: EAGAIN means "nothing yet", poll decides the wait.
    let flags = rustix::fs::fcntl_getfl(fd).expect("F_GETFL");
    let _ = rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match rustix::io::read(fd, &mut buf) {
            Ok(0) => break, // EOF (shouldn't happen — slave fd still open)
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e == rustix::io::Errno::AGAIN => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        let mut poll = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        let timeout =
            rustix::event::Timespec::try_from(Duration::from_millis(20)).expect("timespec");
        let _ = rustix::event::poll(&mut poll, Some(&timeout));
    }
    out
}

/// Write bytes "into the terminal": emulate terminal OUTPUT by writing
/// to master (which becomes the slave's input).
fn write_to_master(fd: BorrowedFd<'_>, bytes: &[u8]) {
    rustix::io::write(fd, bytes).expect("write to master");
}

/// Pump `pump_input` a few times so queued slave input drains through
/// the probe/passthrough router. Master→slave delivery can be
/// asynchronous, so the first pump waits (bounded, 5ms ticks) until
/// bytes actually land in the slave's input queue.
fn pump_all<T: opentui_core::terminal::TtyDevice>(
    term: &mut Terminal<T, SlaveWriter>,
    renderer: &mut CliRenderer,
) -> Vec<CliRenderEvent> {
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let avail = rustix::io::ioctl_fionread(term.tty().input_fd()).unwrap_or(0);
        if avail > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut events = Vec::new();
    for _ in 0..4 {
        match term.pump_input(renderer, 0) {
            Ok(mut ev) => {
                let drained = ev.is_empty();
                events.append(&mut ev);
                if drained {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    events
}

fn full_options() -> SetupOptions {
    SetupOptions {
        use_mouse: true,
        kitty_keyboard_flags: 5,
        ..SetupOptions::default()
    }
}

fn renderer() -> CliRenderer {
    opentui_core::renderer::create_cli_renderer(CliRendererConfig::default())
}

/// The A1 setup burst arrives on the master byte-exactly.
#[test]
fn pty_setup_burst_exact_bytes() {
    let pty = open_pty();
    let opts = full_options();
    {
        let master_fd = pty.master.as_fd();
        let _term = Terminal::new(
            UnixTty::from_fd(pty.slave.try_clone().unwrap()),
            SlaveWriter {
                fd: pty.slave.try_clone().unwrap(),
            },
            &opts,
        )
        .unwrap();
        let bytes = read_available(master_fd, Instant::now() + Duration::from_secs(2));
        assert_eq!(
            bytes,
            b"\x1b[s\x1b[?1049h\x1b[>4;1m\x1b[>4;0m\x1b[>5u\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?1004h",
            "renderer.zig:563-575 + terminal.zig:409-430 order over a real PTY"
        );
    } // Drop -> teardown epilogue (asserted in its own test)
}

/// A1's full setup→teardown lifecycle over one pair: teardown emits
/// the complete epilogue, and the idempotent second teardown (Drop
/// after explicit) writes nothing more.
#[test]
fn pty_teardown_bytes_and_idempotent_second_teardown() {
    let pty = open_pty();
    let opts = full_options();
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &opts,
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(300)); // drop setup

    term.teardown().unwrap();
    let teardown = read_available(master_fd, Instant::now() + Duration::from_secs(1));
    assert_eq!(
        teardown,
        b"\x1b[?25h\x1b[0m\x1b]22;\x1b\\\x1b[<u\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b]0;\x07\x1b]12;default\x07\x1b]112\x07\x1b[0 q\x1b[?25h",
        "terminal.zig:282-335 + renderer.zig:642-645 epilogue",
    );

    term.teardown().unwrap(); // idempotent
    drop(term); // Drop must stay silent too
    let extra = read_available(master_fd, Instant::now() + Duration::from_millis(200));
    assert!(
        extra.is_empty(),
        "second teardown/Drop must emit zero bytes"
    );
}

/// probe_capabilities writes the burst (incl. A3's kitty graphics
/// query) on the master; a canned response set fed back through
/// master → pump_input classifies into Capabilities events.
#[test]
fn pty_probe_burst_and_capability_classification() {
    let pty = open_pty();
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions::default(),
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(200));

    term.probe_capabilities().unwrap();
    let burst = read_available(master_fd, Instant::now() + Duration::from_secs(1));
    let mut expected = Vec::new();
    write_probe_burst(&mut expected).unwrap();
    assert_eq!(burst, expected, "probe burst byte-exact over PTY");

    // Canned ghostty-shaped replies (terminal.zig:1097): DECRPM sync +
    // kitty `?u` + DA1 — completes the probe (DA1 + kitty answered).
    write_to_master(master_fd, b"\x1b[?2026;2$y\x1b[?0u\x1b[?62;22c");
    let mut r = renderer();
    let events = pump_all(&mut term, &mut r);
    let caps = events
        .iter()
        .filter_map(|e| match e {
            CliRenderEvent::Capabilities(c) => Some(c),
            _ => None,
        })
        .next_back()
        .expect("Capabilities event fired");
    assert!(caps.sync, "2026;2$y classified");
    assert!(caps.kitty_keyboard, "?0u classified");
    // pump again — key passthrough (below) needs the probe finished
    write_to_master(master_fd, b"\x1b[I");
    let events = pump_all(&mut term, &mut r);
    assert!(
        events.is_empty(),
        "focus bytes are not terminal events (renderer-side only)"
    );
}

/// Real key bytes written to master pass through the (live or
/// finished) probe untouched and reach the renderer's key dispatch.
#[test]
fn pty_key_bytes_pass_through_to_renderer() {
    let pty = open_pty();
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions::default(),
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(200));

    term.probe_capabilities().unwrap();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(200));

    // Finish the probe with a minimal reply pair, then a real key.
    write_to_master(master_fd, b"\x1b[?0u\x1b[?1;2c");
    let mut r = renderer();
    let _ = pump_all(&mut term, &mut r);

    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // Hold the Subscription — it deregisters on drop.
    let _sub = {
        let sink = seen.clone();
        r.key_input.on_key(
            opentui_core::events::KeyHandlerEvent::Keypress,
            move |k: &mut opentui_core::events::KeyEvent| {
                sink.lock().unwrap().push(k.key.raw.clone());
            },
        )
    };
    write_to_master(master_fd, b"a");
    let _ = pump_all(&mut term, &mut r);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["a".to_string()],
        "key byte reached the renderer's key dispatch"
    );
}

/// pump_output wraps fresh frame bytes in the ?2026 envelope on the
/// master side; a no-change pump emits nothing.
#[test]
fn pty_pump_output_envelope_on_master() {
    let pty = open_pty();
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions::default(),
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(200));

    let mut r = renderer();
    r.resize(10, 2); // force a full frame
    term.pump_output(&mut r).unwrap();
    let frame = read_available(master_fd, Instant::now() + Duration::from_secs(1));
    assert!(
        frame.starts_with(b"\x1b[?2026h"),
        "syncSet leads on the wire (renderer.zig:1844)"
    );
    assert!(
        frame.ends_with(b"\x1b[?2026l"),
        "syncReset closes (renderer.zig:3031)"
    );
    assert!(
        frame.len() > b"\x1b[?2026h\x1b[?2026l".len(),
        "SGR frame bytes between the envelope"
    );

    // No new frame -> nothing on the wire.
    term.pump_output(&mut r).unwrap();
    let extra = read_available(master_fd, Instant::now() + Duration::from_millis(200));
    assert!(extra.is_empty(), "no-op pump emits zero bytes");
}

/// TIOCSWINSZ on the master → check_resize → renderer.resize + the
/// Resize event; a steady size reports None.
#[test]
fn pty_resize_via_tiocswinsz() {
    let pty = open_pty();
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions::default(),
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(200));

    rustix::termios::tcsetwinsize(
        master_fd,
        rustix::termios::Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();

    let mut r = renderer();
    let sink: std::sync::Arc<std::sync::Mutex<Vec<CliRenderEvent>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let s = sink.clone();
        r.on_event(move |e| s.lock().unwrap().push(e.clone()));
    }
    assert_eq!(term.check_resize(&mut r).unwrap(), Some((100, 30)));
    assert_eq!(term.check_resize(&mut r).unwrap(), None, "steady size");
    assert_eq!(r.config.width, 100, "renderer.resize followed");
    assert_eq!(r.config.height, 30);
    // renderer.resize() itself renders (emitting a Frame event through
    // the same listeners); filter to the Resize event the pump added.
    let events = sink.lock().unwrap().clone();
    let resizes: Vec<CliRenderEvent> = events
        .iter()
        .filter(|e| matches!(e, CliRenderEvent::Resize { .. }))
        .cloned()
        .collect();
    assert!(
        matches!(
            resizes.as_slice(),
            [CliRenderEvent::Resize {
                width: 100,
                height: 30
            }]
        ),
        "exactly one Resize event, got: {events:?}"
    );
}

/// Raw mode round-trip: the slave's termios leaves cooked mode at
/// `new` and is restored by teardown.
#[test]
fn pty_raw_mode_and_restore() {
    use rustix::termios::{LocalModes, Termios};
    let pty = open_pty();
    let saved: Termios = rustix::termios::tcgetattr(pty.slave.as_fd()).expect("slave tcgetattr");
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions::default(),
    )
    .unwrap();
    let raw: Termios = rustix::termios::tcgetattr(pty.slave.as_fd()).expect("raw tcgetattr");
    assert!(
        !raw.local_modes
            .contains(LocalModes::ICANON | LocalModes::ECHO),
        "raw mode cleared ICANON/ECHO"
    );
    term.teardown().unwrap();
    let restored: Termios =
        rustix::termios::tcgetattr(pty.slave.as_fd()).expect("restored tcgetattr");
    assert_eq!(
        (
            restored.input_modes,
            restored.output_modes,
            restored.local_modes
        ),
        (saved.input_modes, saved.output_modes, saved.local_modes),
        "teardown restored the saved termios"
    );
    let _ = AsRawFd::as_raw_fd(&pty.master); // ownership lint silence
}

/// pump_input's timeout_ms is REAL poll(2) semantics (Gate A #4): with
/// no input queued, a 50ms-timeout call blocks ~50ms and returns an
/// empty event vec — bounded wall-clock, neither instant (dead
/// parameter) nor unbounded.
#[test]
fn pty_pump_input_timeout_is_bounded() {
    let pty = open_pty();
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions::default(),
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(200));

    let mut r = renderer();
    // timeout_ms = 0 stays non-blocking: returns immediately.
    let t0 = Instant::now();
    let events = term.pump_input(&mut r, 0).unwrap();
    assert!(events.is_empty());
    assert!(
        t0.elapsed() < Duration::from_millis(200),
        "zero timeout must not wait"
    );

    // 50ms timeout with no input: waits ~50ms, returns empty.
    let t1 = Instant::now();
    let events = term.pump_input(&mut r, 50).unwrap();
    let elapsed = t1.elapsed();
    assert!(events.is_empty(), "timeout yields no events");
    assert!(
        elapsed >= Duration::from_millis(45),
        "must actually wait the timeout, waited {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2000),
        "wait must be bounded at the timeout, waited {elapsed:?}"
    );
}
