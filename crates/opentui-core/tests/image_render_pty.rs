//! PTY integration for the native image lane (D3-4a): probe bytes →
//! capability/resolution capture → protocol resolution → emitted
//! kitty/sixel bytes, over a private `posix_openpt` pair.
//!
//! The terminal-emulator side writes canned probe replies to the
//! master; the program side reads them from the slave into
//! [`CapabilityProbe`]. The emitter then writes to the slave (program
//! stdout) and the test reads the escape stream from the master,
//! exactly the direction the A3 PTY suite uses.
//!
//! Runs only with `--features terminal-backend` (the private-PTY
//! dependency); the default headless build is unaffected.

#![cfg(feature = "terminal-backend")]

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use opentui_core::buffer::OptimizedBuffer;
use opentui_core::image::NativeImage;
use opentui_core::image_render::{
    write_pixel_resolution_query, EmitProtocol, ImageFrameEmitter, PIXEL_RESOLUTION_QUERY,
};
use opentui_core::renderables::image::{ImageRenderCtx, ImageRenderProtocol};
use opentui_core::renderer::CliRendererConfig;
use opentui_core::terminal::probe::CapabilityProbe;
use opentui_core::terminal::{TtyDevice, UnixTty};

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

#[test]
fn pty_probe_resolution_selects_sixel_and_emits_dcs() {
    let pair = open_pty();
    let mut tty = UnixTty::from_fd(pair.slave);
    let _raw = tty.raw_mode().expect("slave raw mode");

    // The "terminal" answers DA1 with sixel attribute 4, the kitty
    // keyboard query, and the pixel-size query (80×24 grid at 800×480).
    let terminal_reply = b"\x1b[?1;4c\x1b[?1u\x1b[4;480;800t";
    rustix::io::write(pair.master.as_fd(), terminal_reply).expect("write probe reply");
    let probe_bytes = read_available(tty.input_fd(), Instant::now() + Duration::from_millis(500));
    assert!(
        probe_bytes.windows(3).any(|w| w == b"[4;"),
        "the reply crossed the PTY line discipline"
    );

    let mut renderer = opentui_core::renderer::create_cli_renderer(CliRendererConfig::default());
    let mut probe = CapabilityProbe::new(Duration::from_secs(5));
    let mut events = Vec::new();
    probe.feed(&mut renderer, &probe_bytes, &mut events);
    probe.finish(&mut renderer, &mut events);
    assert!(probe.capabilities().sixel, "DA1 attribute 4 → sixel");
    assert_eq!(
        probe.capabilities().pixel_resolution,
        Some((800, 480)),
        "CSI 4;height;width t captured"
    );

    let ctx = ImageRenderCtx::from_capabilities(probe.capabilities(), 80, 24);
    assert_eq!(ctx.pixel_resolution(), Some((800, 480)));
    let cell = ctx.cell_px().expect("cell px from resolution");
    assert!((cell.width - 10.0).abs() < f64::EPSILON);
    assert!((cell.height - 20.0).abs() < f64::EPSILON);

    // `protocol="auto"` now resolves to sixel (kitty graphics absent).
    let mut buffer = OptimizedBuffer::new(2, 1);
    let image = NativeImage::from_rgba(&[255, 0, 0, 255], 1, 1).expect("raw image");
    assert!(buffer
        .draw_image(
            &image,
            0,
            0,
            1,
            1,
            20,
            20,
            0,
            0,
            1,
            1,
            ImageRenderProtocol::Auto,
        )
        .expect("drawImage contract"));
    let mut emitter = ImageFrameEmitter::new();
    let mut emitted = Vec::new();
    let report = emitter
        .render_frame(&mut emitted, &mut buffer, &ctx)
        .expect("render frame");
    assert_eq!(report.protocol, Some(EmitProtocol::Sixel));
    assert_eq!(report.drawn, vec![1]);

    // Program stdout → PTY → terminal-emulator reads the DCS payload.
    rustix::io::write(tty.input_fd(), &emitted).expect("write frame");
    let observed = read_available(
        pair.master.as_fd(),
        Instant::now() + Duration::from_millis(500),
    );
    let text = String::from_utf8_lossy(&observed);
    assert!(
        text.contains("\x1b[1;1H\x1bP0;1;0q\"1;1;20;20"),
        "CUP + DCS: {text:?}"
    );
    assert!(text.ends_with("\x1b\\"), "ST terminator");
}

#[test]
fn pty_query_round_trips_and_oversized_reply_stays_blocks() {
    let pair = open_pty();
    let mut tty = UnixTty::from_fd(pair.slave);
    let _raw = tty.raw_mode().expect("slave raw mode");

    // Program asks for the pixel size; the emulator sees the exact CSI.
    let mut query = Vec::new();
    write_pixel_resolution_query(&mut query).expect("query write");
    assert_eq!(query, PIXEL_RESOLUTION_QUERY);
    rustix::io::write(tty.input_fd(), &query).expect("write query");
    let observed = read_available(
        pair.master.as_fd(),
        Instant::now() + Duration::from_millis(500),
    );
    assert_eq!(observed, b"\x1b[14t");

    // An oversized reply is consumed as a probe response but leaves the
    // resolution absent, so auto stays on blocks (renderer.ts:3579-3582
    // + the upstream "oversized resolution" test).
    rustix::io::write(pair.master.as_fd(), b"\x1b[4;4294967295;4294967295t")
        .expect("write oversized reply");
    let probe_bytes = read_available(tty.input_fd(), Instant::now() + Duration::from_millis(500));
    let mut renderer = opentui_core::renderer::create_cli_renderer(CliRendererConfig::default());
    let mut probe = CapabilityProbe::new(Duration::from_secs(5));
    let mut events = Vec::new();
    probe.feed(&mut renderer, &probe_bytes, &mut events);
    probe.finish(&mut renderer, &mut events);
    assert_eq!(probe.capabilities().pixel_resolution, None);
    let ctx = ImageRenderCtx::from_capabilities(probe.capabilities(), 80, 24);

    let mut buffer = OptimizedBuffer::new(2, 1);
    let image = NativeImage::from_rgba(&[0, 255, 0, 255], 1, 1).expect("raw image");
    assert!(buffer
        .draw_image(
            &image,
            0,
            0,
            1,
            1,
            20,
            20,
            0,
            0,
            1,
            1,
            ImageRenderProtocol::Auto,
        )
        .expect("drawImage contract"));
    let mut emitter = ImageFrameEmitter::new();
    let mut emitted = Vec::new();
    let report = emitter
        .render_frame(&mut emitted, &mut buffer, &ctx)
        .expect("render frame");
    assert_eq!(report.protocol, Some(EmitProtocol::Blocks));
    assert!(report.materialized);
    assert!(emitted.is_empty(), "blocks fallback emits no escape bytes");
}
