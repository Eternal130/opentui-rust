//! Lane D1a contract oracles — the three Gate-D0-ruled renderer
//! lifecycle contracts, each pinned to upstream
//! `@opentui/core 0.5.14` (`third_party/opentui/packages/core/src/
//! renderer.ts`, `renderer-theme-mode.ts`) with file:line citations.
//!
//! ① settle semantics — `requestRender` coalescing (renderer.ts:1565-1605)
//!   formalized as a property: N invalidations between settles flush
//!   EXACTLY one frame — no starvation (0 frames) and no over-merging
//!   (>1 frame per batch). The headless harness already models the
//!   0.5.14 settle sequence (renderer.rs `render`'s internal
//!   effects-guarded relayout loop); this pins the scheduling side.
//! ② dirty-tracking invariants — a clean tree + `requestRender`
//!   emits ZERO bytes (the diff loop finds no changes,
//!   renderer.zig:2833 sync makes current == next); a tree mutation
//!   without `requestRender` emits zero bytes (no render pass runs).
//! ③ lifecycle state machine — destroy/isDestroyed/
//!   set_background_color/set_terminal_title/wait_for_theme_mode/
//!   set_use_mouse/render: every transition enumerated, and after
//!   `destroy()` every call is a no-op, never a panic
//!   (renderer.ts:4408-4423 + the `_isDestroyed` guards).

use opentui_core::color::Rgba;
use opentui_core::create_cli_renderer;
use opentui_core::renderer::{
    CliRenderEvent, CliRenderer, CliRendererConfig, ThemeModeWait, ThemeWaitPoll,
};
use opentui_core::renderables::text::TextOptions;

fn renderer() -> CliRenderer {
    create_cli_renderer(CliRendererConfig::default())
}

// ---------------------------------------------------------------------------
// ① settle semantics (requestRender → loop, renderer.ts:1565-1605)
// ---------------------------------------------------------------------------

/// Flush one invalidation batch: fire `n` `requestRender` calls and
/// settle, returning the frames painted for the batch. Note the
/// `delay === 0` arm fires the frame synchronously inside
/// `requestRender` (the `process.nextTick` port, renderer.ts:1598-1600
/// and scheduler.rs's exact port), so frames are counted by frame-id
/// delta, not by the settle return alone.
fn flush_batch(r: &mut CliRenderer, n: u32) -> u64 {
    let before = r.frame_id();
    for _ in 0..n {
        r.request_render(); // renderer.ts:1565
    }
    r.settle(64); // `await renderer.idle()` (renderer.ts:1646-1664)
    r.frame_id() - before
}

/// The property: for every N in 1..=64, N `requestRender` calls
/// followed by a settle flush EXACTLY one frame — the coalescing
/// contract of `updateScheduled`/`renderTimeout`
/// (renderer.ts:1591-1604: one scheduled attempt at a time; repeated
/// invalidations while a frame is pending are dropped).
#[test]
fn settle_property_n_invalidations_flush_exactly_one_frame() {
    for n in 1..=64u32 {
        let mut r = renderer();
        r.add_text("t", &TextOptions::default());
        for _ in 0..n {
            r.request_render(); // renderer.ts:1565
        }
        assert!(r.is_render_pending(), "N={n}: batch marked dirty");
        let base = r.frame_id();
        r.settle(64);
        assert_eq!(r.frame_id() - base, 1, "N={n}: exactly one frame per batch");
        assert!(!r.is_render_pending(), "N={n}: dirty bit cleared");
        // No over-merge, no starvation: a settle with no new
        // invalidation paints nothing (idle scheduler).
        r.settle(64);
        assert_eq!(r.frame_id() - base, 1, "N={n}: idle settle adds no frame");
    }
}

/// Sequential batches each flush their own frame — M batches of N
/// invalidations → exactly M frames (the settle cadence the
/// differential harness relies on).
#[test]
fn settle_sequential_batches_exact_frame_count() {
    let mut r = renderer();
    r.add_text("t", &TextOptions::default());
    let base = r.frame_id();
    for batch in 1..=10u64 {
        assert_eq!(flush_batch(&mut r, 1 + (batch % 5) as u32), 1, "batch {batch}");
    }
    assert_eq!(r.frame_id(), base + 10);
}

/// A request after a settle flushes a fresh batch — the
/// `immediateRerenderRequested` / `rendering` arm
/// (renderer.ts:1584-1589) is exhaustively pinned at the scheduler
/// level (scheduler.rs unit tests: `request_render_coalesces`,
/// `request_render_while_running_arms_frame_timer`); the renderer
/// contract is that batches never bleed into each other.
#[test]
fn settle_re_request_after_settle_flushes_fresh_batch() {
    let mut r = renderer();
    r.add_text("t", &TextOptions::default());
    assert_eq!(flush_batch(&mut r, 1), 1);
    assert_eq!(flush_batch(&mut r, 1), 1, "the next batch flushes its own frame");
    let before = r.frame_id();
    r.settle(64);
    assert_eq!(r.frame_id(), before, "idle settle paints nothing");
}

// ---------------------------------------------------------------------------
// ② dirty-tracking invariants
// ---------------------------------------------------------------------------

/// Clean tree + `requestRender` → ZERO emitted bytes: the render pass
/// runs but the diff loop finds current == next (renderer.zig:2833),
/// so `emit_frame_sgr` writes nothing.
#[test]
fn clean_tree_request_render_emits_zero_bytes() {
    let mut r = renderer();
    r.add_text("t", &TextOptions::default());
    assert_eq!(flush_batch(&mut r, 1), 1); // paint once — the tree is now clean
    let before = r.sgr_stream().len();
    assert_eq!(flush_batch(&mut r, 1), 1, "the frame runs");
    assert_eq!(
        r.sgr_stream().len(),
        before,
        "clean-tree frame emits zero bytes"
    );
}

/// Tree mutation WITHOUT `requestRender` → zero bytes: no render pass
/// runs at all (the renderables own their invalidation calls
/// upstream; the harness's explicit `render` is the only other path).
#[test]
fn tree_mutation_without_request_render_emits_zero_bytes() {
    let mut r = renderer();
    r.add_text("t", &TextOptions::default());
    assert_eq!(flush_batch(&mut r, 1), 1);
    let before = r.sgr_stream().len();
    // mutate the tree (a second node) but never invalidate
    r.add_text("t2", &TextOptions::default());
    assert_eq!(r.sgr_stream().len(), before, "no requestRender → no bytes");
    // and the dirty bit is not set by the mutation itself
    assert!(!r.is_render_pending());
}

// ---------------------------------------------------------------------------
// ③ lifecycle state machine (renderer.ts:4408-4470, 1365-1366,
//    4078-4083, 4112-4114, 1963-1971, 1725-1737)
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_destroy_is_idempotent_and_flips_is_destroyed() {
    let mut r = renderer();
    assert!(!r.is_destroyed()); // renderer.ts:1365-1366
    r.destroy(); // renderer.ts:4408
    assert!(r.is_destroyed());
    r.destroy(); // renderer.ts:4410-4411 — second destroy is a no-op
    assert!(r.is_destroyed());
}

#[test]
fn lifecycle_every_call_after_destroy_is_noop_not_panic() {
    let mut r = renderer();
    r.add_text("t", &TextOptions::default());
    assert_eq!(flush_batch(&mut r, 1), 1);
    let frame = r.frame_id();
    let sgr_len = r.sgr_stream().len();
    r.destroy();

    // requestRender (renderer.ts:1544's destroyed scheduling guard)
    r.request_render();
    assert!(!r.is_render_pending());
    assert_eq!(r.settle(64), 0, "destroyed scheduler paints nothing");

    // render — the loop() destroyed guard
    let id = r.render();
    assert_eq!(id, frame, "no new frame after destroy");
    assert_eq!(r.sgr_stream().len(), sgr_len, "zero bytes after destroy");

    // setBackgroundColor (renderer.ts:4078-4083's `_isDestroyed` family)
    let bg = r.config.background_color;
    r.set_background_color(Rgba::rgb(1, 2, 3));
    assert_eq!(r.config.background_color, bg, "bg unchanged after destroy");

    // setTerminalTitle (renderer.ts:4112-4114)
    r.set_terminal_title("nope");
    assert_eq!(r.terminal_title(), "", "title write dropped after destroy");

    // set useMouse (renderer.ts:1725-1737)
    let mouse = r.config.use_mouse;
    r.set_use_mouse(!mouse);
    assert_eq!(r.config.use_mouse, mouse, "mouse unchanged after destroy");

    // resize / feed still must not panic
    r.resize(100, 30);
    r.feed_bytes(b"x");

    // wait_for_theme_mode resolves immediately with the current mode
    // (renderer-theme-mode.ts:62-66's isDestroyed arm)
    assert_eq!(
        r.wait_for_theme_mode(1000),
        ThemeModeWait::Resolved(None),
        "destroyed wait resolves with null mode"
    );
}

#[test]
fn lifecycle_set_background_color_invalidates_and_fills() {
    let mut r = renderer();
    r.add_text("t", &TextOptions::default());
    assert_eq!(flush_batch(&mut r, 1), 1);
    let sgr_before = r.sgr_stream().len();
    let before = r.frame_id();
    r.set_background_color(Rgba::rgb(9, 9, 9)); // renderer.ts:4078-4083
    r.settle(64);
    assert_eq!(r.frame_id() - before, 1, "exactly one frame scheduled");
    assert_eq!(r.config.background_color, Rgba::rgb(9, 9, 9));
    // the cleared `nextRenderBuffer` (renderer.ts:4082) changes every
    // cell's background — the diff loop emits bytes
    assert!(
        r.sgr_stream().len() > sgr_before,
        "background change is visible in the emission"
    );
}

#[test]
fn lifecycle_wait_for_theme_mode_state_machine() {
    // (a) unknown mode + timeout 0 → resolves immediately with null
    // (renderer-theme-mode.ts:62-66)
    let mut r = renderer();
    assert_eq!(r.wait_for_theme_mode(0), ThemeModeWait::Resolved(None));

    // (b) unknown mode + positive timeout → pending
    let wait = r.wait_for_theme_mode(1000);
    let ThemeModeWait::Pending { id } = wait else {
        panic!("expected pending wait, got {wait:?}");
    };

    // (c) THEME_MODE event resolves it with the mode (applyThemeMode,
    // renderer-theme-mode.ts:223-235; emission renderer.ts:1214)
    r.emit_terminal_event(CliRenderEvent::ThemeMode {
        mode: opentui_core::renderer::ThemeMode::Dark,
    });
    assert_eq!(r.theme_mode(), Some(opentui_core::renderer::ThemeMode::Dark));
    assert_eq!(
        r.poll_theme_mode_wait(id),
        Some(opentui_core::renderer::ThemeWaitPoll::Resolved(Some(
            opentui_core::renderer::ThemeMode::Dark
        )))
    );
    // resolved waits are consumed exactly once
    assert_eq!(r.poll_theme_mode_wait(id), None);

    // (d) mode now known → every wait resolves immediately
    assert_eq!(
        r.wait_for_theme_mode(1000),
        ThemeModeWait::Resolved(Some(opentui_core::renderer::ThemeMode::Dark))
    );

    // (e) pending wait + destroy → resolved with the current mode
    // (dispose, renderer-theme-mode.ts:89-100)
    let mut r2 = renderer();
    let ThemeModeWait::Pending { id } = r2.wait_for_theme_mode(1000) else {
        panic!("expected pending");
    };
    r2.destroy();
    assert_eq!(
        r2.poll_theme_mode_wait(id),
        Some(opentui_core::renderer::ThemeWaitPoll::Resolved(None))
    );
}

#[test]
fn lifecycle_ctrl_c_destroys_renderer_when_configured() {
    // exitOnCtrlC default true (renderer.ts:1182): Ctrl+C destroys
    // (armed renderer.ts:1252-1257; the sync port destroys
    // immediately — bounded divergence, no microtask queue).
    let mut r = renderer();
    r.feed_bytes(b"\x03"); // Ctrl+C
    assert!(r.is_destroyed());

    // opt-out: the key flows through untouched
    let mut r2 = create_cli_renderer(CliRendererConfig {
        exit_on_ctrl_c: false,
        ..CliRendererConfig::default()
    });
    r2.feed_bytes(b"\x03");
    assert!(!r2.is_destroyed());
}

#[test]
fn lifecycle_use_mouse_runtime_toggle() {
    // renderer.ts:1725-1737: enabling invalidates (a frame is
    // scheduled); disabling does not; unchanged toggles are no-ops.
    let mut r = renderer();
    assert!(r.config.use_mouse, "default useMouse=true (renderer.ts:1222)");
    r.set_use_mouse(true);
    assert!(!r.is_render_pending(), "unchanged toggle is a no-op");

    r.set_use_mouse(false);
    assert!(!r.config.use_mouse);
    let mid = r.frame_id();
    r.settle(64);
    assert_eq!(r.frame_id(), mid, "disable does not invalidate");

    let before = r.frame_id();
    r.set_use_mouse(true);
    r.settle(64);
    assert_eq!(r.frame_id() - before, 1, "enable invalidates (renderer.ts:1733)");
}

// ---------------------------------------------------------------------------
// terminal-backend: the live-session lifecycle (attach → destroy
// teardown; title/mouse writes flow through the session)
// ---------------------------------------------------------------------------

#[cfg(feature = "terminal-backend")]
mod session_lifecycle {
    use super::*;
    use std::io;
    use std::rc::Rc;

    #[test]
    fn destroy_tears_down_attached_session_once() {
        // the boxed session observes teardown through the recorder —
        // count teardown calls.
        use std::cell::Cell;
        struct TearCounter(Rc<Cell<u32>>);
        impl opentui_core::renderer::RendererTerminalSession for TearCounter {
            fn write_bytes(&mut self, _bytes: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn pump_output(&mut self, _r: &mut CliRenderer) -> io::Result<()> {
                Ok(())
            }
            fn teardown(&mut self) -> io::Result<()> {
                self.0.set(self.0.get() + 1);
                Ok(())
            }
            fn is_torn_down(&self) -> bool {
                self.0.get() > 0
            }
        }
        let count = Rc::new(Cell::new(0));
        let mut r = renderer();
        r.attach_terminal_session(Box::new(TearCounter(Rc::clone(&count))));
        r.destroy();
        assert!(r.is_destroyed());
        assert_eq!(count.get(), 1, "exactly one teardown");
        r.destroy(); // idempotent — no second teardown, no panic
        assert_eq!(count.get(), 1);
    }

    /// setTerminalTitle writes OSC 0 through the session
    /// (renderer.ts:4112-4114); after destroy the write is dropped.
    #[test]
    fn set_terminal_title_writes_osc0_through_session() {
        use std::cell::RefCell;
        use std::rc::Rc;

        struct Recorder(Rc<RefCell<Vec<u8>>>);
        impl opentui_core::renderer::RendererTerminalSession for Recorder {
            fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
                self.0.borrow_mut().extend_from_slice(bytes);
                Ok(())
            }
            fn pump_output(&mut self, _r: &mut CliRenderer) -> io::Result<()> {
                Ok(())
            }
            fn teardown(&mut self) -> io::Result<()> {
                Ok(())
            }
            fn is_torn_down(&self) -> bool {
                false
            }
        }

        let mut r = renderer();
        let shared = Rc::new(RefCell::new(Vec::new()));
        r.attach_terminal_session(Box::new(Recorder(Rc::clone(&shared))));
        r.set_terminal_title("hello");
        assert_eq!(r.terminal_title(), "hello");
        r.set_use_mouse(false); // pops the mouse family
        r.set_use_mouse(true); // re-arms + invalidates
        let bytes = shared.borrow().clone();
        assert!(
            bytes.starts_with(b"\x1b]0;hello\x07"),
            "OSC 0 title first, got {bytes:?}"
        );
        assert!(
            bytes.windows(8).any(|w| w == b"\x1b[?1003l"),
            "mouse disable burst present: {bytes:?}"
        );
        assert!(
            bytes.windows(8).any(|w| w == b"\x1b[?1003h"),
            "mouse re-enable burst present: {bytes:?}"
        );
        // after destroy, further writes are dropped
        r.destroy();
        r.set_terminal_title("nope");
        assert_eq!(r.terminal_title(), "hello", "dropped after destroy");
    }
}
