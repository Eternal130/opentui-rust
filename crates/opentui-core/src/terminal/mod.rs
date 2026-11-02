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
//!   [`Terminal`], including the A4 probe-settle pass
//!   (`enable_detected_features`, terminal.zig:399-440);
//! * [`theme`] - the post-probe theme reduction (late OSC 11 -> the
//!   `ThemeMode` event, renderer-theme-mode.ts / renderer.ts:1204-1216).
//!
//! A4 gap register: `enableDetectedFeatures` sends the
//! ?2027h/?2004h/?1004h gated subset (terminal.zig:420-430) - the
//! modifyOtherKeys/kitty re-push (:409-418) stays with A1's
//! `SetupOptions` state machine and the 2031/theme re-query tail
//! (:432-439) is a no-op after the burst; auto-settle write errors
//! are best-effort (the pub seam surfaces them). The theme watch's
//! 250ms refresh-completion timer (renderer-theme-mode.ts:18) is not
//! ported (timer-less slice). The `PALETTE` event variant exists for
//! enum parity but its `TerminalPaletteDetector` family
//! (terminal-palette.ts) stays deferred.
//!
//! A3 gap register (remaining probe/teardown divergences, each also
//! cited at the site): the kitty graphics query IS sent (A3 —
//! unconditionally, where upstream gates `!skip_graphics_query &&
//! !isInTmux()` at terminal.zig:379 and recognizes the APC reply via
//! `parseKittyGraphicsResponse` :1103-1121; the `graphics_enabled`
//! env-override gate has no port — replies always classify). Still
//! deferred: explicit-width/scaled-text probe block
//! (terminal.zig:389-398, no consumer yet; the CPR seed upstream needs
//! from it is NOT captured — B must re-add it if it consumes
//! `caps.explicit_width`), notification queries (`OSC 99`/`1337`,
//! caps.notifications), the foot variant branch
//! (terminal.zig:377), the tmux DCS passthrough wrappers, the
//! LinkPool generation/refcount slot recycling (link.zig:7-10 — the
//! port's renderer-side pool interns URLs but never retires ids), and
//! the split-footer / inline-viewport / `ExternalOutputCommit` family
//! (renderer.ts:2339+ — `Terminal::teardown_scrollback` is the
//! alt-screen tail-jump only). The THEME_MODE / PALETTE events
//! (renderer.ts:761-762) ride the theme-probe consumer lane;
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
pub mod theme;
pub mod tty;

pub use probe::CapabilityProbe;
pub use session::{install_panic_hook, SetupOptions, Terminal};
pub use tty::{TermiosSave, TtyDevice, UnixTty};
