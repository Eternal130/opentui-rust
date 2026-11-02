//! Post-probe theme reduction (A4) — the late-OSC11 → `ThemeMode`
//! event chain, ported from upstream's `RendererThemeMode`
//! (renderer-theme-mode.ts) plus the `themeModeHandler` wiring in
//! renderer.ts:1204-1216.
//!
//! Upstream routes EVERY stdin sequence through `themeModeHandler`;
//! it stores OSC 10/11 color answers (always), and — while a theme
//! query is pending and BOTH fg and bg have arrived — reduces the
//! background to a light/dark mode and emits `THEME_MODE` when the
//! mode CHANGED (renderer.ts:1214 emits the bare mode string; the
//! payload is upstream's `ThemeMode = "dark" | "light"`,
//! types.ts:32). A `?2031`/`?997` color-scheme-change notification
//! restarts the cycle: re-query `OSC 10;? / OSC 11;?`
//! (requestThemeOscColors → `queryThemeColors`, renderer-theme-mode.ts
//! :202-216 / terminal.zig:443-445), drop the stale fg/bg, wait for
//! the fresh pair.
//!
//! Bounded scope (the lane's ruling): only the late-OSC11 → event
//! chain. The `PALETTE` side of the upstream handler
//! (`clearPaletteCache` + `refreshPalette`, renderer.ts:1211-1213)
//! and the whole `TerminalPaletteDetector` family stay deferred —
//! gap-registered in `terminal/mod.rs`.
//!
//! Divergence: upstream completes a hung refresh cycle with a 250ms
//! timer (`QUERY_TIMEOUT_MS`, renderer-theme-mode.ts:18); the Rust
//! backend is the blocking, timer-less slice, so a lost reply leaves
//! the watch armed until the next `?997` restarts it (harmless: no
//! mode is derived from a partial pair either way).

use std::io;

use super::probe::{osc_rgb, osc_theme_front, Front};
use crate::renderer::{CliRenderEvent, CliRenderer, ThemeMode};

/// The `?997` color-scheme-change notifications that restart the
/// query cycle (`handleSequence`, renderer-theme-mode.ts:141-144).
const SCHEME_CHANGE_NOTIFICATIONS: [&[u8]; 2] = [b"\x1b[?997;1n", b"\x1b[?997;2n"];

/// The stdin prefixes the watch must recognize: the notifications
/// above and the OSC 10/11 answer prefixes (a strict prefix of any of
/// these at the front of the hold means "wait for more bytes").
const THEME_UNIT_PREFIXES: [&[u8]; 4] =
    [b"\x1b[?997;1n", b"\x1b[?997;2n", b"\x1b]10;", b"\x1b]11;"];

/// The re-query bytes (`queryThemeColors`, ansi.zig:397 — the same
/// pair the probe burst sends, probe.rs:67).
const THEME_QUERIES: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07";

/// `inferThemeModeFromBackgroundColor` (renderer-theme-mode.ts:33-37):
/// ITU-R BT.601 luminance over the OSC 11 background, threshold 128.
fn infer_theme_mode([r, g, b]: [u8; 3]) -> ThemeMode {
    let brightness = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
    if brightness > 128 {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    }
}

/// The post-probe theme state — upstream's `RendererThemeMode` fields
/// (renderer-theme-mode.ts:43-49: `_themeMode`, `themeQueryPending`,
/// `themeOscForeground`, `themeOscBackground`) plus the streaming
/// hold buffer for answers split across reads.
#[derive(Default)]
pub(crate) struct ThemeWatch {
    mode: Option<ThemeMode>,
    pending: bool,
    fg: Option<[u8; 3]>,
    bg: Option<[u8; 3]>,
    held: Vec<u8>,
}

impl ThemeWatch {
    /// Arm at probe settle: the burst's theme queries are in flight
    /// (`themeQueryPending` starts true upstream,
    /// renderer-theme-mode.ts:46), and any OSC 10/11 answers the
    /// probe already captured count toward the pair — both present
    /// already derives (and emits) the first mode, exactly like the
    /// probe's own answers flow through `themeModeHandler` upstream.
    pub(crate) fn arm(
        &mut self,
        fg: Option<[u8; 3]>,
        bg: Option<[u8; 3]>,
        renderer: &mut CliRenderer,
        events: &mut Vec<CliRenderEvent>,
    ) {
        self.pending = true;
        self.fg = fg;
        self.bg = bg;
        self.reduce(renderer, events);
    }

    /// Store one OSC 10/11 answer; when a pending query now has both
    /// halves, reduce the background to a mode and emit `ThemeMode`
    /// on change (the `applyThemeMode` changed-only contract,
    /// renderer-theme-mode.ts:223-235; emission shape
    /// renderer.ts:1214).
    fn reduce(&mut self, renderer: &mut CliRenderer, events: &mut Vec<CliRenderEvent>) {
        if !self.pending {
            return;
        }
        let (Some(_fg), Some(bg)) = (self.fg, self.bg) else {
            return; // renderer-theme-mode.ts:163-166 — wait for the pair
        };
        let next = infer_theme_mode(bg);
        if self.mode != Some(next) {
            self.mode = Some(next);
            let event = CliRenderEvent::ThemeMode { mode: next };
            renderer.emit_terminal_event(event.clone());
            events.push(event);
        }
        // completeThemeQuery (renderer-theme-mode.ts:190-194)
        self.pending = false;
    }

    /// Restart the query cycle on a `?997` notification
    /// (`requestThemeOscColors`, renderer-theme-mode.ts:202-216):
    /// drop the stale pair, re-arm, and write the OSC 10/11
    /// re-query to the terminal.
    fn restart(&mut self, out: &mut dyn io::Write) -> io::Result<()> {
        self.pending = true;
        self.fg = None;
        self.bg = None;
        out.write_all(THEME_QUERIES)?;
        out.flush()
    }

    /// Route post-probe stdin bytes: OSC 10/11 answers and `?997`
    /// notifications are consumed here (they must NOT reach the
    /// renderer's key stream — upstream hands them to
    /// `themeModeHandler`, which returns `handled: true`); everything
    /// else passes through to `renderer.feed_bytes` untouched.
    pub(crate) fn route(
        &mut self,
        out: &mut dyn io::Write,
        renderer: &mut CliRenderer,
        bytes: &[u8],
        events: &mut Vec<CliRenderEvent>,
    ) -> io::Result<()> {
        self.held.extend_from_slice(bytes);
        loop {
            if self.held.is_empty() {
                break;
            }
            // ?997 notifications first (handleSequence's first branch)
            let mut consumed = false;
            for notif in SCHEME_CHANGE_NOTIFICATIONS {
                if self.held == *notif {
                    self.held.clear();
                    self.restart(out)?;
                    consumed = true;
                    break;
                }
            }
            if consumed {
                continue;
            }
            // A strict prefix of any theme unit may still complete —
            // hold it for the next read (covers answers and
            // notifications split across reads).
            if THEME_UNIT_PREFIXES
                .iter()
                .any(|t| t.starts_with(&self.held[..]))
            {
                break;
            }
            // OSC 10/11 theme answers
            if self.held.starts_with(b"\x1b]10;") || self.held.starts_with(b"\x1b]11;") {
                match osc_theme_front(&self.held) {
                    Front::Complete { unit_len } => {
                        let unit: Vec<u8> = self.held.drain(..unit_len).collect();
                        let rgb = osc_rgb(&unit);
                        if unit.starts_with(b"\x1b]10;") {
                            self.fg = rgb;
                        } else {
                            self.bg = rgb;
                        }
                        self.reduce(renderer, events);
                        continue;
                    }
                    Front::Partial => break, // wait for the terminator
                    Front::NotOurs => {}
                }
            }
            // Not ours at the front. When the front is not an ESC,
            // everything up to the FIRST ESC can safely flush now
            // (no theme unit can start mid-run); when the front IS an
            // unrecognized escape, flush the whole hold — the
            // renderer's key/mouse parser owns its own incomplete-unit
            // pending, so a split key sequence is still reassembled
            // there.
            let split = if self.held[0] == 0x1b {
                self.held.len()
            } else {
                self.held
                    .iter()
                    .position(|&b| b == 0x1b)
                    .unwrap_or(self.held.len())
            };
            let passthrough: Vec<u8> = self.held.drain(..split).collect();
            renderer.feed_bytes(&passthrough);
            if split == 0 {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> CliRenderer {
        crate::renderer::create_cli_renderer(crate::renderer::CliRendererConfig::default())
    }

    /// The BT.601 threshold (renderer-theme-mode.ts:33-37): 128
    /// exactly is dark (strict `>`), 129 light; the canonical probe
    /// fixtures (16 / 255 gray) pin dark/light.
    #[test]
    fn luminance_threshold() {
        assert_eq!(infer_theme_mode([0x10, 0x10, 0x10]), ThemeMode::Dark);
        assert_eq!(infer_theme_mode([0xff, 0xff, 0xff]), ThemeMode::Light);
        assert_eq!(infer_theme_mode([128, 128, 128]), ThemeMode::Dark);
        assert_eq!(infer_theme_mode([129, 129, 129]), ThemeMode::Light);
    }

    /// arm() with the probe-captured pair derives the first mode and
    /// emits exactly once; a second arm with the same colors emits
    /// nothing (changed-only contract).
    #[test]
    fn arm_derives_first_mode_once() {
        let mut watch = ThemeWatch::default();
        let mut r = renderer();
        let mut events = Vec::new();
        watch.arm(
            Some([0xff, 0xff, 0xff]),
            Some([0x10, 0x10, 0x10]),
            &mut r,
            &mut events,
        );
        assert_eq!(
            events,
            vec![CliRenderEvent::ThemeMode {
                mode: ThemeMode::Dark
            }]
        );
        // same colors again -> no change, no event
        watch.arm(
            Some([0xff, 0xff, 0xff]),
            Some([0x10, 0x10, 0x10]),
            &mut r,
            &mut events,
        );
        assert_eq!(events.len(), 1);
    }

    /// Half a pair waits (renderer-theme-mode.ts:163-166); the late
    /// OSC 10 completing it triggers the reduction.
    #[test]
    fn late_pair_completes_on_second_half() {
        let mut watch = ThemeWatch::default();
        let mut r = renderer();
        let mut events = Vec::new();
        watch.arm(None, None, &mut r, &mut events);
        assert!(events.is_empty());
        let mut out = Vec::new();
        watch
            .route(
                &mut out,
                &mut r,
                b"\x1b]11;rgb:1010/1010/1010\x07",
                &mut events,
            )
            .unwrap();
        assert!(events.is_empty(), "bg alone derives nothing");
        watch
            .route(
                &mut out,
                &mut r,
                b"\x1b]10;rgb:ffff/ffff/ffff\x07",
                &mut events,
            )
            .unwrap();
        assert_eq!(
            events,
            vec![CliRenderEvent::ThemeMode {
                mode: ThemeMode::Dark
            }]
        );
    }

    /// A ?997 notification restarts the cycle and writes the OSC
    /// 10/11 re-query; the fresh light pair flips the mode
    /// (renderer-theme-mode.ts:141-144, 202-216).
    #[test]
    fn scheme_change_notification_requeries() {
        let mut watch = ThemeWatch::default();
        let mut r = renderer();
        let mut events = Vec::new();
        watch.arm(
            Some([0xff, 0xff, 0xff]),
            Some([0x10, 0x10, 0x10]),
            &mut r,
            &mut events,
        );
        events.clear();
        let mut out = Vec::new();
        watch
            .route(&mut out, &mut r, b"\x1b[?997;1n", &mut events)
            .unwrap();
        assert_eq!(out, THEME_QUERIES, "re-query written to the tty");
        assert!(events.is_empty(), "restart alone emits nothing");
        watch
            .route(
                &mut out,
                &mut r,
                b"\x1b]10;rgb:0000/0000/0000\x07\x1b]11;rgb:ffff/ffff/ffff\x07",
                &mut events,
            )
            .unwrap();
        assert_eq!(
            events,
            vec![CliRenderEvent::ThemeMode {
                mode: ThemeMode::Light
            }],
            "fresh light background flips the mode"
        );
    }

    /// Non-theme bytes route straight through to feed_bytes — observed
    /// via the focus sequence coming back as a TerminalFocus event —
    /// and answers split across reads still classify (streaming seam).
    #[test]
    fn passthrough_and_split_answers() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<CliRenderEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let mut r = renderer();
        {
            let sink = seen.clone();
            r.on_event(move |e| sink.lock().unwrap().push(e.clone()));
        }
        let mut watch = ThemeWatch::default();
        let mut events = Vec::new();
        let mut out = Vec::new();
        watch.arm(None, None, &mut r, &mut events);
        // split OSC 11 + interleaved focus-in
        watch
            .route(&mut out, &mut r, b"\x1b]11;rgb:10", &mut events)
            .unwrap();
        watch
            .route(&mut out, &mut r, b"10/1010\x07\x1b[I", &mut events)
            .unwrap();
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, CliRenderEvent::TerminalFocus)),
            "focus bytes reached feed_bytes"
        );
        assert!(events.is_empty(), "still no fg — no mode yet");
    }
}
