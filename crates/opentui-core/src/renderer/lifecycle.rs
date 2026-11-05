//! D1a lifecycle family (moved verbatim from the former single-file
//! `renderer.rs`): dimension getters, the render scheduler request
//! pump, `settle`, `destroy`, theme plumbing and session attach.
use super::*;

impl CliRenderer {
    /// `renderer.width` (ctx.width).
    #[must_use]
    pub fn width(&self) -> u32 {
        self.config.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// `renderer.widthMethod` (renderer.ts:1468) —
    /// `capabilities?.unicode ?? nextRenderBuffer.widthMethod`; the
    /// port's buffers carry no method of their own, so the fallback is
    /// the zig default (`WidthMethod::default()`, terminal.zig:18).
    #[must_use]
    pub fn width_method(&self) -> WidthMethod {
        self.capabilities
            .as_ref()
            .map(|caps| caps.unicode)
            .unwrap_or_default()    }

    /// Store the probe's capability snapshot (`_capabilities`,
    /// renderer.ts:925) — feeds [`CliRenderer::width_method`] and the
    /// CAPABILITIES event consumers.
    pub fn set_capabilities(&mut self, caps: TerminalCapabilities) {
        self.capabilities = Some(caps);
    }

    /// `renderer.screenMode` (renderer.ts:1738-1740).
    #[must_use]
    pub fn screen_mode(&self) -> ScreenMode {
        self.screen_mode
    }

    /// The resolved footer height (`_footerHeight`, renderer.ts:883,
    /// resolved at :1075).
    #[must_use]
    pub fn footer_height(&self) -> u32 {
        self.footer_height
    }

    /// `renderer.externalOutputMode` — the resolved mode
    /// (`_externalOutputMode`, renderer.ts:884, resolved at :1075).
    #[must_use]
    pub fn external_output_mode(&self) -> ExternalOutputMode {
        self.external_output_mode
    }

    /// `_terminalWidth`/`_terminalHeight` (renderer.ts:898-899) — the
    /// raw terminal extents (config.width/height carry the RENDER
    /// extents; see [`render_geometry`]).
    #[must_use]
    pub fn terminal_size(&self) -> (u32, u32) {
        (self.terminal_width, self.terminal_height)
    }

    /// `renderOffset` (renderer.ts:900) — terminal rows above the
    /// renderer surface; emission adds it to every row
    /// (renderer.zig:2718).
    #[must_use]
    pub fn render_offset(&self) -> u32 {
        self.render_offset
    }

    /// `set footerHeight` (renderer.ts:1768-1772): normalize, no-op on
    /// an unchanged value, store, and — under split-footer — re-run
    /// the geometry (`applyScreenMode("split-footer")`, renderer.ts
    /// :1770-1772 → :3015-3019 `calculateRenderGeometry` → :3160-3161
    /// `this.width/height = renderWidth/renderHeight` +
    /// `root.resize` + requestRender). Errors mirror
    /// `normalizeFooterHeight`'s throw (renderer.ts:359-360).
    pub fn set_footer_height(&mut self, footer_height: u32) -> Result<(), RendererModeError> {
        let normalized = normalize_footer_height(Some(footer_height))?; // renderer.ts:1769
        if normalized == self.footer_height {
            return Ok(()); // renderer.ts:1770-1771
        }
        self.footer_height = normalized;
        if self.screen_mode == ScreenMode::SplitFooter {
            self.apply_screen_mode(); // renderer.ts:1770-1772
        }
        Ok(())
    }

    /// The `applyScreenMode` geometry re-run (renderer.ts:3015-3019 +
    /// :3160-3163) reduced to the port's render-surface slice:
    /// recompute [`RenderGeometry`] from the CURRENT terminal size and
    /// the (already-stored) new footer height, move the render
    /// extents/offset, rebuild the buffers, re-style the root, and
    /// render one frame (the requestRender tail, renderer.ts:3183).
    /// The terminal-transition half (scroll regions, stale-row clears,
    /// `writeOut` bursts) is terminal-backend territory and stays with
    /// the live embedder.
    fn apply_screen_mode(&mut self) {
        let geometry = render_geometry::calculate_render_geometry(
            self.screen_mode,
            self.terminal_width,
            self.terminal_height,
            self.footer_height,
        );
        let (width, height) = (geometry.render_width, geometry.render_height);
        if width == self.config.width && height == self.config.height {
            return; // applyScreenMode's same-geometry early return, renderer.ts:3023-3025
        }
        self.render_offset = geometry.render_offset; // :3057/:3136-3137
        self.config.width = width;  // renderer.ts:3160-3161
        self.config.height = height;
        self.next_render_buffer = OptimizedBuffer::new(width, height); // resizeRenderer
        self.current_render_buffer = OptimizedBuffer::new(width, height);
        let node = self.root.inner.node; // this.root.resize, renderer.ts:3163
        let mut style = self.tree.style(node).expect("root style").clone();
        style.size.width = tp::Dimension::length(width as f32);
        style.size.height = tp::Dimension::length(height as f32);
        let _ = self.tree.set_style(node, style);
        let _ = self.tree.mark_dirty(node);
        self.render(); // requestRender, renderer.ts:3183
    }

    // -----------------------------------------------------------------
    // Lifecycle + render scheduling (D1a) — renderer.ts:1365-1366,
    // 1543-1619, 1707-1740, 1963-1971, 4078-4114, 4408-4470;
    // renderer-theme-mode.ts:54-100, 223-235
    // -----------------------------------------------------------------

    /// `renderer.isDestroyed` (renderer.ts:1365-1366).
    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// `requestRender()` (renderer.ts:1565-1605) — coalesces
    /// invalidations: one scheduled attempt at a time, repeated calls
    /// while a frame is pending are dropped (the scheduler's
    /// `updateScheduled` guard, renderer.ts:1591-1604). A destroyed
    /// renderer drops the request (the scheduling guards,
    /// renderer.ts:1544).
    pub fn request_render(&mut self) {
        if self.destroyed {
            return;
        }
        self.render_pending = true;
        // The scheduler is taken out so the render closure can borrow
        // the renderer (the scheduler's contract: the render body is a
        // callback so the renderer stays the single owner of
        // painting — scheduler.rs).
        let mut scheduler = std::mem::replace(
            &mut self.scheduler,
            crate::scheduler::RenderScheduler::new(self.config.target_fps, self.config.max_fps),
        );
        scheduler.request_render(&mut || self.run_frame());
        self.scheduler = scheduler;
    }

    /// Whether an invalidation is pending (the dirty bit behind
    /// `requestRender` — the observable form of upstream's
    /// `hasScheduledRender`, renderer.ts:1671).
    #[must_use]
    pub fn is_render_pending(&self) -> bool {
        self.render_pending
    }

    /// One frame body — `loop()`'s render tail (renderer.ts:4761+).
    /// The dirty bit clears first (the batch this frame serves);
    /// a destroyed renderer renders nothing.
    fn run_frame(&mut self) {
        if self.destroyed {
            return;
        }
        self.render_pending = false;
        // The split-commit flush rides the frame head — upstream's
        // loop() drains the ExternalOutputQueue before painting
        // (renderer.ts:972-976), so one frame batches the queued
        // commits and repaints the footer together.
        if self.split_capture_active && self.external_output_queue.size() > 0 {
            self.flush_pending_split_commits(false, false);
        }
        let _ = self.render();
    }

    /// The deterministic `await renderer.idle()` (renderer.ts:1646-1664)
    /// — pump the scheduler's virtual timers until nothing is
    /// scheduled. Returns the number of frames painted; the settle
    /// contract is exactly one frame per invalidation batch (N
    /// `requestRender` calls between settles → 1 frame, never 0
    /// (starvation) and never >1 (over-merging)).
    pub fn settle(&mut self, max_frames: u64) -> u64 {
        let before = self.scheduler.frame_count();
        let mut scheduler = std::mem::replace(
            &mut self.scheduler,
            crate::scheduler::RenderScheduler::new(self.config.target_fps, self.config.max_fps),
        );
        scheduler.run_until_idle(max_frames, &mut || self.run_frame());
        self.scheduler = scheduler;
        self.scheduler.frame_count() - before
    }

    /// `destroy()` (renderer.ts:4408-4423) — idempotent; flips
    /// `_isDestroyed`, stops the scheduler, resolves the theme
    /// waiters with the current mode (the `dispose` ladder,
    /// renderer-theme-mode.ts:89-100), and tears the live terminal
    /// session down when one is attached. Every later lifecycle call
    /// is a no-op, never a panic.
    pub fn destroy(&mut self) {
        if self.destroyed {
            return; // renderer.ts:4410-4411
        }
        self.destroyed = true; // renderer.ts:4412
        self.scheduler.stop(); // `_isRunning = false` (cleanupBeforeDestroy, renderer.ts:4486)
        self.render_pending = false;
        // The destroy-path drain — upstream flushes pending split
        // output before the transition with suspend/unsetup allowed
        // (renderer.ts:4500, 4558), so no captured output is lost.
        if self.split_capture_active && self.external_output_queue.size() > 0 {
            self.flush_pending_split_commits(true, true);
        }
        // theme waiters resolve with the current mode
        // (renderer-theme-mode.ts:89-100 `dispose`)
        let mode = self.theme_mode;
        for (id, _) in std::mem::take(&mut self.theme_waiters) {
            self.theme_wait_resolved.insert(id, mode);
        }
        // Live session teardown. `clearOnShutdown` (renderer.ts:158-159)
        // gates upstream's region-clearing; the RAII port always runs
        // the full `performShutdownSequence` teardown — restoring the
        // host terminal (raw mode off, alt-screen exit) is not optional
        // — so the flag is recorded, not branched on (bounded
        // divergence, registered in the lane report).
        #[cfg(feature = "terminal-backend")]
        if let Some(mut session) = self.session.take() {
            let _ = session.teardown();
        }
    }

    /// `setBackgroundColor(color)` (renderer.ts:4078-4083) — set the
    /// fill color, clear `nextRenderBuffer` with it, and invalidate.
    /// No-op after destroy.
    pub fn set_background_color(&mut self, color: Rgba) {
        if self.destroyed {
            return;
        }
        self.config.background_color = color;
        self.next_render_buffer.clear(color);
        self.request_render();
    }

    /// `setTerminalTitle(title)` (renderer.ts:4112-4114) — OSC 0
    /// (`\x1b]0;…\x07`, the same title channel teardown resets,
    /// terminal.zig:328) written to the live session; the last value
    /// is kept for observability. No-op after destroy.
    pub fn set_terminal_title(&mut self, title: &str) {
        if self.destroyed {
            return;
        }
        self.terminal_title = title.to_owned();
        #[cfg(feature = "terminal-backend")]
        if let Some(session) = self.session.as_mut() {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"\x1b]0;");
            bytes.extend_from_slice(title.as_bytes());
            bytes.push(0x07);
            let _ = session.write_bytes(&bytes);
        }
    }

    /// The last `setTerminalTitle` value ("" until set).
    #[must_use]
    pub fn terminal_title(&self) -> &str {
        &self.terminal_title
    }

    /// `waitForThemeMode(timeoutMs)` (renderer.ts:1963-1971 →
    /// RendererThemeMode.waitForThemeMode, renderer-theme-mode.ts:62-83):
    /// resolves immediately when the mode is known, the renderer is
    /// destroyed, or the timeout is 0; otherwise registers a waiter
    /// that the next THEME_MODE event, the deadline, or `destroy()`
    /// resolves (`ThemeMode | null` — None = "not known"). The sync
    /// port hands back the wait state; poll with
    /// [`CliRenderer::poll_theme_mode_wait`].
    pub fn wait_for_theme_mode(&mut self, timeout_ms: u64) -> ThemeModeWait {
        if self.theme_mode.is_some() || self.destroyed || timeout_ms == 0 {
            return ThemeModeWait::Resolved(self.theme_mode);
        }
        self.theme_waiter_seq += 1;
        let id = self.theme_waiter_seq;
        let deadline = self.performance_now() + timeout_ms as f64;
        self.theme_waiters.push((id, Some(deadline)));
        ThemeModeWait::Pending { id }
    }

    /// Poll a registered theme wait — `waiter.resolve(mode)`'s call
    /// site (renderer-theme-mode.ts:76-83 timeout arm, :89-100
    /// dispose arm, :223-235 mode-change arm). `None` = unknown id
    /// (already resolved and taken, or never registered).
    pub fn poll_theme_mode_wait(&mut self, id: u64) -> Option<ThemeWaitPoll> {
        if let Some(mode) = self.theme_wait_resolved.remove(&id) {
            return Some(ThemeWaitPoll::Resolved(mode));
        }
        // Unknown id (already consumed, or never registered)?
        if !self.theme_waiters.iter().any(|(wid, _)| *wid == id) {
            return None;
        }
        let mut resolve_with = None;
        if self.theme_mode.is_some() || self.destroyed {
            resolve_with = Some(self.theme_mode);
        } else {
            let now = self.performance_now();
            if let Some((_, deadline)) = self.theme_waiters.iter().find(|(wid, _)| *wid == id) {
                if let Some(deadline) = *deadline {
                    if now >= deadline {
                        resolve_with = Some(self.theme_mode); // timeout → current (possibly null)
                    }
                }
            }
        }
        if let Some(mode) = resolve_with {
            self.theme_waiters.retain(|(wid, _)| *wid != id);
            return Some(ThemeWaitPoll::Resolved(mode));
        }
        Some(ThemeWaitPoll::Pending)
    }

    /// Resolve every waiter whose id is pending — the
    /// `applyThemeMode` arm (renderer-theme-mode.ts:223-235).
    pub(crate) fn resolve_theme_waiters(&mut self) {
        let mode = self.theme_mode;
        for (id, _) in std::mem::take(&mut self.theme_waiters) {
            self.theme_wait_resolved.insert(id, mode);
        }
    }

    /// `set useMouse` (renderer.ts:1725-1737) — runtime toggle: no-op
    /// when unchanged; enabling arms the `?1000/?1002/?1003/?1006`
    /// family and invalidates; disabling pops it (`MOUSE_DISABLE_ALL`,
    /// terminal.zig:946-951). No-op after destroy.
    pub fn set_use_mouse(&mut self, use_mouse: bool) {
        if self.destroyed || self.config.use_mouse == use_mouse {
            return; // renderer.ts:1727 — no change needed
        }
        self.config.use_mouse = use_mouse;
        #[cfg(feature = "terminal-backend")]
        if let Some(session) = self.session.as_mut() {
            let mut bytes = Vec::new();
            if use_mouse {
                // enableMouse with movement (terminal.zig:227 state
                // default true)
                let _ = crate::terminal::modes::mouse_enable(&mut bytes, true);
            } else {
                bytes.extend_from_slice(crate::terminal::modes::MOUSE_DISABLE_ALL);
            }
            let _ = session.write_bytes(&bytes);
        }
        if use_mouse {
            self.request_render(); // renderer.ts:1733
        }
    }

    /// `set consoleMode` (renderer.ts:1625-1632) — the "disabled" arm
    /// is fully implemented (nothing captures or paints); the
    /// "console-overlay" arm records intent (the overlay renderable
    /// family is deferred).
    pub fn set_console_mode(&mut self, mode: ConsoleMode) {
        self.use_console = mode == ConsoleMode::ConsoleOverlay;
        self.config.console_mode = mode;
    }

    /// `get consoleMode` (renderer.ts:1625).
    #[must_use]
    pub fn console_mode(&self) -> ConsoleMode {
        self.config.console_mode
    }

    /// `setGatherStats(enabled)` (renderer.ts:4924).
    pub fn set_gather_stats(&mut self, enabled: bool) {
        self.config.gather_stats = enabled;
    }

    /// `EventEmitter.setMaxListeners(n)` (node:events) counterpart —
    /// the listener-warning threshold. Node never rejects listeners
    /// past the cap (it only silences `MaxListenersExceededWarning`
    /// once raised); the port stores the cap the same way — the
    /// warning channel needs a logger seam and stays deferred.
    pub fn set_max_listeners(&mut self, max_listeners: u32) {
        self.config.max_listeners = max_listeners;
    }

    /// `EventEmitter.getMaxListeners()` (node:events) counterpart.
    #[must_use]
    pub fn max_listeners(&self) -> u32 {
        self.config.max_listeners
    }

    /// The renderer's current theme mode — `_themeMode`
    /// (renderer-theme-mode.ts:44), updated by THEME_MODE events.
    #[must_use]
    pub fn theme_mode(&self) -> Option<ThemeMode> {
        self.theme_mode
    }

    /// Attach the live terminal session (`terminal-backend`
    /// feature) — the renderer "holds" the Phase A session; `destroy`
    /// tears it down, the toggle/title writes flow through it.
    #[cfg(feature = "terminal-backend")]
    pub fn attach_terminal_session(&mut self, session: Box<dyn RendererTerminalSession>) {
        self.session = Some(session);
    }

    /// The attached live session, when one exists.
    #[cfg(feature = "terminal-backend")]
    #[must_use]
    pub fn terminal_session(&self) -> Option<&dyn RendererTerminalSession> {
        self.session.as_deref()
    }

    // -----------------------------------------------------------------
}
