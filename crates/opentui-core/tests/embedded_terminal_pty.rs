//! PTY grid-parity tests for the embedded-terminal engine (lane D3-3a).
//!
//! oc-tui's `terminal_pane.rs` renders its VT grid from `vt100::Parser`;
//! this lane folds that surface into `opentui_core::embedded_terminal`
//! (the backend of `EmbeddedTerminalRenderable`). The split-footer /
//! terminal-backend suite already drives a private `posix_openpt` pair
//! (`tests/pty_terminal.rs`); this file reuses that paradigm to prove
//! the folded engine's grid and cursor are byte-for-byte identical to
//! the oc-tui oracle over a REAL PTY byte stream:
//!
//!   writer (slave, "the program") → PTY line discipline → master
//!     → `Engine::write` and `vt100::Parser::process`
//!     → per-cell text + cursor equality.
//!
//! The same bytes feed both parsers, so PTY output post-processing
//! (ONLCR and friends) is part of the parity input, not a confound.
//! Runs only with `--features terminal-backend` (the private-PTY
//! dependency); the default headless build is unaffected.

#![cfg(feature = "terminal-backend")]

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use opentui_core::embedded_terminal::Engine;

/// A private `posix_openpt` pair — the same constructor as
/// `tests/pty_terminal.rs`.
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

/// Read from `fd` until the line discipline goes quiet (`deadline` is
/// the hard cap; 20ms poll ticks — no long sleeps; the same helper as
/// `tests/pty_terminal.rs`).
fn read_available(fd: BorrowedFd<'_>, deadline: Instant) -> Vec<u8> {
    let flags = rustix::fs::fcntl_getfl(fd).expect("F_GETFL");
    let _ = rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match rustix::io::read(fd, &mut buf) {
            Ok(0) => break,
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

/// Write a "program output" script on the slave and read back the
/// master side — the byte stream a real child process would produce.
fn through_pty(bytes: &[u8]) -> Vec<u8> {
    let pty = open_pty();
    rustix::io::write(&pty.slave, bytes).expect("write slave");
    read_available(
        pty.master.as_fd(),
        Instant::now() + Duration::from_millis(300),
    )
}

/// The text grid of a `vt100` screen (skip wide-tail cells exactly as
/// the folded engine's `screen_lines()` does).
fn vt100_lines(parser: &vt100::Parser, cols: u16, rows: u16) -> Vec<String> {
    let screen = parser.screen();
    (0..rows)
        .map(|y| {
            let mut line = String::new();
            for x in 0..cols {
                let Some(cell) = screen.cell(y, x) else {
                    continue;
                };
                let contents = cell.contents();
                if contents.is_empty() {
                    continue; // wide-tail / blank continuation
                }
                line.push(contents.chars().next().unwrap_or(' '));
            }
            line.trim_end().to_owned()
        })
        .collect()
}

/// Feed the same PTY byte stream through both parsers and require an
/// identical trimmed text grid + cursor.
fn assert_pty_grid_parity(script: &[u8], cols: u16, rows: u16) -> Vec<String> {
    let bytes = through_pty(script);
    assert!(!bytes.is_empty(), "PTY delivered no bytes");

    let mut engine = Engine::new(cols, rows, 10_000);
    engine.write(&bytes);

    let mut oracle = vt100::Parser::new(rows, cols, 0);
    oracle.process(&bytes);

    let mut expected = vt100_lines(&oracle, cols, rows);
    while expected.last().is_some_and(|line| line.is_empty()) {
        expected.pop();
    }
    let actual = engine.screen_lines();
    assert_eq!(actual, expected, "grid parity over PTY bytes {bytes:?}");

    let cursor = engine.cursor();
    let (row, col) = oracle.screen().cursor_position();
    assert_eq!(
        (cursor.y, cursor.x),
        (row, col),
        "cursor parity over PTY bytes {bytes:?}"
    );
    actual
}

/// Text, SGR, wrap and cursor addressing through a real PTY.
#[test]
fn pty_grid_parity_text_sgr_wrap() {
    let lines = assert_pty_grid_parity(
        b"\x1b[2J\x1b[Hhello \x1b[1;32mworld\x1b[0m\r\nsecond line\r\n\x1b[3;1Hbottom",
        20,
        4,
    );
    assert_eq!(
        lines,
        vec!["hello world", "second line", "bottom"],
        "expected VT grid"
    );
}

/// Wide characters + pending-wrap at the right margin.
#[test]
fn pty_grid_parity_wide_and_wrap() {
    let lines = assert_pty_grid_parity(b"\x1b[2J\x1b[Hab\xe7\x95\x8ccd\xe7\x95\x8c", 6, 3);
    assert_eq!(lines, vec!["ab界cd", "界"], "wide-cell wrap grid");
}

/// Alternate screen enter/exit preserves the primary grid across the
/// PTY round trip.
#[test]
fn pty_grid_parity_alt_screen() {
    let lines = assert_pty_grid_parity(
        b"\x1b[2J\x1b[Hprimary\r\n\x1b[?1049h\x1b[2J\x1b[Halt\x1b[?1049l",
        12,
        3,
    );
    assert_eq!(lines, vec!["primary"], "primary restored after alt screen");
}

/// Scrollback + viewport scroll parity: `Engine::scroll_viewport` must
/// expose the same earlier rows the oc-tui pane would repaint.
#[test]
fn pty_grid_parity_scrollback_viewport() {
    let bytes = through_pty(b"\x1b[2J\x1b[Hone\r\ntwo\r\nthree\r\nfour\r\nfive");
    let mut engine = Engine::new(10, 2, 100);
    engine.write(&bytes);
    let mut oracle = vt100::Parser::new(2, 10, 100);
    oracle.process(&bytes);

    // Bottom-anchored viewport first.
    assert_eq!(engine.screen_lines(), vt100_lines(&oracle, 10, 2));

    // Scroll up two lines: both expose the same scrollback rows.
    engine.scroll_viewport(-2);
    oracle.set_scrollback(2);
    assert_eq!(
        engine.screen_lines(),
        vt100_lines(&oracle, 10, 2),
        "scrollback viewport parity"
    );
}
