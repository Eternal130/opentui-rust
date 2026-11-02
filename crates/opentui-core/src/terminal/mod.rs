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
//!   chained panic hook.
//!
//! Scope guard (Phase A ruling): alt-screen ONLY. The inline viewport
//! (`makeRoomForRendererOutput`), split-footer surface, and the
//! `ExternalOutputCommit` family (renderer.ts:2339+) are deferred to
//! Phase B4 — GAPS-registered, not silently approximated. Capability
//! probing, the input/output pumps, and resize watching are lane A2.
pub mod modes;
pub mod session;
pub mod tty;

pub use session::{install_panic_hook, SetupOptions, Terminal};
pub use tty::{TermiosSave, TtyDevice, UnixTty};
