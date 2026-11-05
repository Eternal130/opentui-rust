//! PTY integration for the split-footer RENDER side (lane D4.1c-pre):
//! a split-footer renderer self-renders its footer frame and the
//! terminal backend's pump writes it out — wrapped in the `?2026`
//! synchronized-update envelope, with every row offset into the
//! bottom live region, leaving the transcript rows above untouched.
//!
//! Upstream evidence this pins:
//! * the footer frame is emitted through the SAME frame path —
//!   `beginRenderFrame` writes `syncSet` + hideCursor for every
//!   rendered frame (renderer.zig:1842-1845; there is no separate
//!   un-synced footer emission), and the frame tail closes with
//!   `syncReset` only when the frame started (renderer.zig:3029-3032);
//! * every emitted row carries the render offset
//!   (renderer.zig:2718 `moveToOutput(writer, x + 1, y + 1 +
//!   self.renderOffset)`), so the footer-band tree paints into the
//!   rows `makeRoomForRendererOutput` reserved (terminal.zig's
//!   scroll-down, renderer.zig:566-569).
//!
//! Private `posix_openpt` pair, same harness paradigm as
//! `pty_terminal.rs` (the in-repo PTY exception — no TUI binary).

#![cfg(feature = "terminal-backend")]

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use opentui_core::renderer::{
    create_cli_renderer, CliRendererConfig, ExternalOutputMode, ScreenMode,
};
use opentui_core::terminal::{SetupOptions, Terminal, UnixTty};

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

struct SlaveWriter {
    fd: OwnedFd,
}

impl io::Write for SlaveWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(rustix::io::write(self.fd.as_fd(), buf)?)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

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

fn split_renderer(width: u32, height: u32, footer: u32) -> opentui_core::renderer::CliRenderer {
    create_cli_renderer(CliRendererConfig {
        width,
        height,
        screen_mode: ScreenMode::SplitFooter,
        footer_height: Some(footer),
        external_output_mode: Some(ExternalOutputMode::Passthrough),
        ..Default::default()
    })
}

/// The live footer path: reserve the bottom band
/// (`makeRoomForRendererOutput`, renderer.zig:566-569), mount footer
/// content on the split renderer's root tree, self-render one frame,
/// and pump it out through the terminal backend — the bytes on the
/// master must be `?2026`-wrapped, address ONLY the reserved bottom
/// rows (renderOffset + 1 … terminalHeight, renderer.zig:2718), carry
/// the footer text, and never touch the transcript rows above (no
/// absolute CUP into rows 1..=renderOffset, no full-screen clear).
#[test]
fn pty_split_footer_frame_envelope_offset_and_transcript_intact() {
    let pty = open_pty();
    let (cols, rows, footer) = (40u32, 12u32, 4u32);
    let reserve = (rows - footer) as u32; // makeRoomForRendererOutput's freed lines

    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions {
            use_alt_screen: false,
            reserve_surface_height: reserve,
            ..Default::default()
        },
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let setup = read_available(master_fd, Instant::now() + Duration::from_millis(300));
    // the reservation scroll-down happened at setup (renderer.zig:566-569)
    assert!(
        setup.windows(6).any(|w| w.starts_with(b"\x1b[")),
        "setup burst wrote terminal bytes"
    );

    // Build the split renderer: render surface = the 4-row footer band
    // at the bottom (render-geometry.ts:31-39), offset = 8.
    let mut r = split_renderer(cols, rows, footer);
    assert_eq!((r.width(), r.height()), (cols, footer));
    let offset = r.render_offset();
    assert_eq!(offset, rows - footer);

    // Mount footer content onto the root tree (the render()-into-root
    // face's core half — upstream `render(() => …, renderer)` mounts
    // under renderer.root, index.ts:70-83).
    r.add_text(
        "footer-status",
        &opentui_core::renderables::text::TextOptions {
            content: opentui_core::renderables::text::ContentInput::Str("footer-live".to_owned()),
            ..Default::default()
        },
    );
    r.render();

    // Self-emit through the backend's pump: ?2026 envelope around the
    // fresh frame bytes (renderer.zig:1842-1845 / 3029-3032 — the
    // footer frame rides the same synchronized-update path).
    term.pump_output(&mut r).unwrap();

    let frame = read_available(master_fd, Instant::now() + Duration::from_millis(300));
    let frame = String::from_utf8_lossy(&frame).to_string();
    assert!(
        frame.starts_with("\x1b[?2026h"),
        "syncSet leads the footer frame (renderer.zig:1844): {frame:?}"
    );
    assert!(
        frame.ends_with("\x1b[?2026l"),
        "syncReset closes it (renderer.zig:3031): {frame:?}"
    );
    assert!(frame.contains("footer-live"), "footer text emitted");
    // the first band row lands at renderOffset+1 (renderer.zig:2718)
    assert!(
        frame.contains(&format!("\x1b[{};1H", offset + 1)),
        "first footer row at renderOffset+1: {frame:?}"
    );
    // transcript intact: no absolute addressing above the band, no
    // 2J full clear, no scroll
    for row in 1..=offset {
        assert!(
            !frame.contains(&format!("\x1b[{row};")),
            "row {row} above the band must not be addressed: {frame:?}"
        );
    }
    assert!(!frame.contains("2J"), "no full-screen clear");
}

/// A footer-height change re-geometries and re-emits into the new
/// band: `set footerHeight` (renderer.ts:1768-1772) → applyScreenMode
/// geometry (:3015-3019) → the next pumped frame addresses the NEW
/// renderOffset+1 row, still `?2026`-wrapped.
#[test]
fn pty_split_footer_height_setter_re_emits_at_new_offset() {
    let pty = open_pty();
    let (cols, rows) = (40u32, 12u32);
    let mut term = Terminal::new(
        UnixTty::from_fd(pty.slave.try_clone().unwrap()),
        SlaveWriter {
            fd: pty.slave.try_clone().unwrap(),
        },
        &SetupOptions {
            use_alt_screen: false,
            reserve_surface_height: rows - 4,
            ..Default::default()
        },
    )
    .unwrap();
    let master_fd = pty.master.as_fd();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(300));

    let mut r = split_renderer(cols, rows, 4);
    r.add_text(
        "footer-status",
        &opentui_core::renderables::text::TextOptions {
            content: opentui_core::renderables::text::ContentInput::Str("tall".to_owned()),
            ..Default::default()
        },
    );
    r.render();
    term.pump_output(&mut r).unwrap();
    let _ = read_available(master_fd, Instant::now() + Duration::from_millis(300));

    // Grow the footer 4 → 6: offset moves 8 → 6, render height 6.
    r.set_footer_height(6).unwrap();
    assert_eq!(r.render_offset(), 6);
    assert_eq!((r.width(), r.height()), (cols, 6));
    term.pump_output(&mut r).unwrap();
    let frame = read_available(master_fd, Instant::now() + Duration::from_millis(300));
    let frame = String::from_utf8_lossy(&frame).to_string();
    assert!(frame.starts_with("\x1b[?2026h") && frame.ends_with("\x1b[?2026l"));
    assert!(
        frame.contains("\x1b[7;1H"),
        "re-emitted at the new renderOffset+1 = 7: {frame:?}"
    );
    for row in 1..=6 {
        assert!(
            !frame.contains(&format!("\x1b[{row};")),
            "row {row} above the new band untouched: {frame:?}"
        );
    }
}
