//! The terminal backend (Phase A1, `terminal-backend` feature).
//!
//! Upstream `@opentui` splits terminal ownership between the TS facade
//! (`packages/core/src/renderer.ts` — `createCliRenderer` assembly,
//! renderer.ts:723-748) and the zig native library
//! (`packages/native/src/terminal.zig` — modes/state, and
//! `renderer.zig` — setup/shutdown orchestration), leaning on the Node
//! host for termios, fds, and resize. The Rust port owns that layer
//! itself:
//!
//! * [`tty`] — the `TtyDevice` seam + the rustix-backed `UnixTty`
//!   (raw mode, restore, TIOCGWINSZ);
//! * [`modes`] — the byte-level mode sequences, each citing its
//!   upstream `ansi.zig` line and cross-checked against oc-tui's
//!   self-managed equivalents;
//! * [`session`] — `SetupOptions` + the RAII `Terminal` lifecycle
//!   (setup burst per renderer.zig:558-576, teardown epilogue per
//!   renderer.zig:600-654 / terminal.zig:282-335) and the optional
//!   chained panic hook;
//! * [`probe`] — the capability query burst (terminal.zig:347-399)
//!   and the response state machine accumulating
//!   `TerminalCapabilities` (terminal.zig:1247-1446);
//! * [`pumps`] — the input/output pumps and the resize watch on
//!   `Terminal`.
//!
//! A2 gap register (probe burst divergences vs terminal.zig:347-399,
//! each also cited at the probe site): kitty graphics query (A3,
//! image lanes), explicit-width/scaled-text probe block
//! (terminal.zig:389-398, no consumer yet), notification queries
//! (`OSC 99`/`1337`, caps.notifications), the foot variant branch,
//! and the tmux DCS passthrough wrappers. The THEME_MODE / PALETTE
//! events (renderer.ts:761-762) ride the theme-probe consumer lane;
//! OSC 10/11 replies are captured (`CapabilityProbe::last_osc11_bg`)
//! but not yet reduced to a theme mode.
//!
//! Scope guard (Phase A ruling): alt-screen ONLY. The inline viewport
//! (`makeRoomForRendererOutput`), split-footer surface, and the
//! `ExternalOutputCommit` family (renderer.ts:2339+) are deferred to
//! Phase B4 — GAPS-registered, not silently approximated.
pub mod modes;
pub mod probe;
pub mod pumps;
pub mod session;
pub mod tty;

pub use probe::CapabilityProbe;
pub use session::{install_panic_hook, SetupOptions, Terminal};
pub use tty::{TermiosSave, TtyDevice, UnixTty};
