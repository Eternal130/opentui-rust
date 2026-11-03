//! Split-footer scrollback surface family + renderer-side split
//! commit/flush runtime (the `renderer.rs` slice of the D1b-core
//! lane). Distinct from the crate-level `crate::scrollback` queue
//! module: this file holds the renderer-owned surface + flush
//! methods moved verbatim from the former single-file `renderer.rs`.
use super::*;

/// `maxSplitCommitsPerFrame` (renderer.ts:909) — the flush bound: one
/// render tick drains at most this many commits (a "modest frame";
/// leftover work re-requests a render, renderer.ts:2817-2820).
const MAX_SPLIT_COMMITS_PER_FRAME: usize = 8;

/// The `flushPendingSplitCommits` outcome (renderer.ts:2730-2734's
/// `"rendered" | "backpressured" | "failed"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitFlushStatus {
    /// Commits were flushed (or nothing was queued).
    Rendered,
    /// Native backpressure — upstream's `SKIPPED` status reschedules
    /// (renderer.ts:2758-2761). The port has no native split-append
    /// gate, so this arm is unreachable for now and exists for the
    /// decision-table parity.
    Backpressured,
    /// The live-session write failed (upstream's `FAILED` status,
    /// renderer.ts:2763-2766).
    Failed,
}

/// The `writeToScrollback` mode-gate failure (upstream throws,
/// renderer.ts:2364-2366 — the port returns the error instead of
/// unwinding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbackWriteError {
    /// `screenMode "split-footer"` + `externalOutputMode
    /// "capture-stdout"` not armed (renderer.ts:2364-2366).
    CaptureNotActive,
    /// The writer returned no snapshot rows (the `must return a
    /// snapshot root renderable` guard, renderer.ts:2383-2385).
    EmptySnapshot,
}

/// The `ScrollbackSurface` assertion failures — upstream throws at
/// each site; the port returns the error (renderer.ts:1986-1988,
/// :2062-2102).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbackSurfaceError {
    /// `createScrollbackSurface requires screenMode "split-footer"
    /// and externalOutputMode "capture-stdout"` (renderer.ts:1986-1988).
    RequiresSplitFooterCapture,
    /// `ScrollbackSurface is destroyed` (renderer.ts:2062-2064).
    Destroyed,
    /// `ScrollbackSurface.commitRows requires render() before
    /// commitRows()` (renderer.ts:2068-2071).
    NotRendered,
    /// `ScrollbackSurface.commitRows requires render() after renderer
    /// geometry changes` (renderer.ts:2078-2083).
    GeometryChanged,
    /// `ScrollbackSurface.commitRows requires ...` row-bound family
    /// (renderer.ts:2087-2101).
    InvalidRowRange,
}

impl std::fmt::Display for ScrollbackSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiresSplitFooterCapture => write!(
                f,
                "createScrollbackSurface requires screenMode \"split-footer\" and externalOutputMode \"capture-stdout\""
            ),
            Self::Destroyed => write!(f, "ScrollbackSurface is destroyed"),
            Self::NotRendered => write!(
                f,
                "ScrollbackSurface.commitRows requires render() before commitRows()"
            ),
            Self::GeometryChanged => write!(
                f,
                "ScrollbackSurface.commitRows requires render() after renderer geometry changes"
            ),
            Self::InvalidRowRange => write!(
                f,
                "ScrollbackSurface.commitRows requires 0 <= startRow <= endRowExclusive <= height"
            ),
        }
    }
}

impl std::error::Error for ScrollbackSurfaceError {}

/// `ScrollbackSurfaceOptions` (renderer.ts:316-318).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollbackSurfaceOptions {
    /// `startOnNewLine` (default true, renderer.ts:1992).
    pub start_on_new_line: Option<bool>,
}

/// `ScrollbackSurfaceCommitOptions` (renderer.ts:320-323).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollbackSurfaceCommitOptions {
    /// `rowColumns` (clamped in `enqueueRenderedScrollbackCommit`,
    /// renderer.ts:2583-2614).
    pub row_columns: Option<u32>,
    /// `trailingNewline` (default true, renderer.ts:2260).
    pub trailing_newline: Option<bool>,
}

/// `ScrollbackSurface` (renderer.ts:325-338) — a retained surface
/// with its own render tree and backing buffer; the host renders
/// streaming entries onto it and commits painted row ranges into the
/// parent renderer's scrollback FIFO.
///
/// Port shape (bounded divergence, same contract as
/// `opentui-solid`'s scrollback writer): the render tree lives on a
/// SCRATCH [`CliRenderer`] (core renderables bind to their taffy
/// tree), the backing buffer is the scratch renderer's frame buffer,
/// and the `renderContext` surface is the scratch renderer itself
/// (the port constructs renderables through `make_*`). `commit_rows`
/// takes the parent renderer explicitly — the enqueue target
/// upstream reaches through `this.renderer`
/// (renderer.ts:2261-2264). `settle` is synchronous: the port's
/// code-highlighting pipeline is synchronous
/// (`build_code_document`), so there is nothing to await
/// (renderer.ts:2196-2226 collapses to a render).
pub struct ScrollbackSurface {
    /// The scratch renderer owning the surface's tree + buffers.
    scratch: CliRenderer,
    /// The public root's live id (the `scrollback-surface-root-N`
    /// BoxRenderable, renderer.ts:2016-2029).
    root_id: String,
    surface_width: u32,
    surface_height: u32,
    /// The geometry the last `render()` snapshotted — the
    /// `assertGeometryStillCurrent` inputs (renderer.ts:2078-2083).
    geo_width: u32,
    geo_width_method: WidthMethod,
    /// The parent geometry at snapshot time.
    destroyed: bool,
    has_rendered: bool,
    /// `nextCommitStartOnNewLine` (renderer.ts:2049) — true until the
    /// first commit claims the surface's leading newline ownership.
    next_commit_start_on_new_line: bool,
}

impl ScrollbackSurface {
    /// The construction half of `createScrollbackSurface`
    /// (renderer.ts:1990-2050): snapshot the pending tail column,
    /// build the scratch renderer at width×1, and mount the public
    /// box root (position absolute at 0,0, full width, auto height,
    /// no border/fill, column flow — renderer.ts:2016-2029).
    fn new(parent: &CliRenderer, options: ScrollbackSurfaceOptions) -> Result<Self, ScrollbackSurfaceError> {
        static SURFACE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let surface_id = SURFACE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let start_on_new_line = options.start_on_new_line.unwrap_or(true); // :1992
        // firstLineOffset (:1993-1994) — the port's paint is
        // full-width (the tail-column math owns the splice), so the
        // offset only feeds the first commit's startOnNewLine default.
        let tail_column = parent.get_pending_split_tail_column();
        let _first_line_offset =
            !start_on_new_line && tail_column > 0 && tail_column < parent.width().max(1);

        let width = parent.width().max(1);
        let mut scratch = create_cli_renderer(CliRendererConfig {
            width,
            height: 1, // renderer.ts:2007-2010 (snapshotContext height 1)
            ..CliRendererConfig::default()
        });
        let root_id = format!("scrollback-surface-root-{surface_id}");
        let root_opts = crate::renderables::box_::BoxOptions {
            layout: crate::layout::LayoutOptions {
                position: Some("absolute".to_owned()),
                top: Some(Dimension::Points(0.0)),
                left: Some(Dimension::Points(0.0)),
                flex_direction: Some("column".to_owned()),
                ..Default::default()
            },
            width: Some(Dimension::Points(width as f32)),
            height: None, // "auto"
            background_color: Some("transparent".to_owned()),
            border: Some(crate::renderables::box_::BorderSpec::Off),
            should_fill: Some(false),
            ..Default::default()
        };
        let root_renderable = scratch.make_box(&root_id, &root_opts);
        scratch.add(root_renderable);

        Ok(ScrollbackSurface {
            scratch,
            root_id,
            surface_width: width,
            surface_height: 1,
            geo_width: width,
            geo_width_method: parent.width_method(),
            destroyed: false,
            has_rendered: false,
            next_commit_start_on_new_line: start_on_new_line,
        })
    }

    /// The surface's `renderContext` face — upstream hands the
    /// context to `new *Renderable(surface.renderContext, …)`
    /// (renderer.ts:307's `ScrollbackSnapshotRenderContext`); in the
    /// port renderables are constructed through the renderer, so this
    /// IS the context.
    pub fn render_context(&mut self) -> &mut CliRenderer {
        &mut self.scratch
    }

    /// `surface.root` (renderer.ts:2030-2032) — the public box root's
    /// live id; mount children with
    /// [`CliRenderer::mount_child`] under it.
    #[must_use]
    pub fn root_id(&self) -> &str {
        &self.root_id
    }

    /// Mount a renderable under the surface root (`surface.root.add`,
    /// renderer.ts:2030-2032's consumer face).
    pub fn add(&mut self, child: Renderable) -> bool {
        let root = self.root_id.clone();
        self.scratch.mount_child(&root, child)
    }

    /// `surface.width` (renderer.ts:333-ish surface getters).
    #[must_use]
    pub fn width(&self) -> u32 {
        self.surface_width
    }

    /// `surface.height`.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.surface_height
    }

    /// `surface.isDestroyed`.
    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// The public root's measured layout height, clamped ≥ 1
    /// (`measureSnapshotHeight`, scrollback.ts:108-122 — the same
    /// measure the solid writer uses).
    fn measure_root_height(&self) -> u32 {
        self.scratch
            .root
            .inner
            .find(&self.root_id)
            .map_or(1, |n| n.h.max(1.0) as u32)
    }

    fn render_pass(&mut self, width: u32, height: u32) {
        self.scratch.resize(width, height);
        self.scratch.render();
    }

    /// `surface.render()` (renderer.ts:2150-2207) — re-render the
    /// surface at the CURRENT renderer geometry, converging the
    /// auto-height root over at most
    /// [`MAX_SCROLLBACK_SURFACE_HEIGHT_PASSES`] measure/render passes
    /// (:2165-2207), then snapshot the geometry for the next
    /// `commitRows`' `assertGeometryStillCurrent`.
    pub fn render(&mut self) {
        if self.destroyed {
            return; // assertNotDestroyed, renderer.ts:2152-2154
        }
        let width = self.scratch.config.width.max(1);
        let mut target_height = self.surface_height.max(1); // :2188
        let mut converged = false;

        for _pass in 0..MAX_SCROLLBACK_SURFACE_HEIGHT_PASSES {
            self.render_pass(width, target_height);
            let measured = self.measure_root_height().max(1);
            if measured == target_height {
                converged = true; // :2190-2206
                break;
            }
            target_height = measured;
        }
        if !converged {
            // The post-loop renderPass (:2209-2210) — the budget ran
            // out at a freshly measured height.
            self.render_pass(width, target_height);
        }

        self.surface_width = width;
        self.surface_height = target_height;
        self.geo_width = width;
        self.has_rendered = true; // :2201
    }

    /// `surface.settle(timeoutMs)` (renderer.ts:2212-2226) — render,
    /// then wait for pending `CodeRenderable` highlighting. The
    /// port's highlight pipeline is synchronous, so settle IS render.
    pub fn settle(&mut self) {
        self.render();
    }

    /// `surface.commitRows(startRow, endRowExclusive, options)`
    /// (renderer.ts:2242-2272) — copy the painted row range out of
    /// the backing buffer and enqueue it into `renderer`'s scrollback
    /// FIFO with the surface's rolling newline ownership
    /// (`nextCommitStartOnNewLine` → false after the first commit,
    /// :2270). The parent renderer is the explicit enqueue target
    /// (upstream closes over `renderer`, :2243).
    pub fn commit_rows(
        &mut self,
        renderer: &mut CliRenderer,
        start_row: u32,
        end_row_exclusive: u32,
        options: ScrollbackSurfaceCommitOptions,
    ) -> Result<(), ScrollbackSurfaceError> {
        if self.destroyed {
            return Err(ScrollbackSurfaceError::Destroyed); // :2062-2064
        }
        if !self.has_rendered {
            return Err(ScrollbackSurfaceError::NotRendered); // :2068-2071
        }
        // assertGeometryStillCurrent (:2078-2083): the surface's
        // width/widthMethod must still match the parent's.
        if renderer.width().max(1) != self.geo_width
            || renderer.width_method() != self.geo_width_method
        {
            return Err(ScrollbackSurfaceError::GeometryChanged);
        }
        // assertRowRange (:2087-2101) — u32 bounds make the
        // integer/negativity arms structurally unreachable.
        if end_row_exclusive < start_row || end_row_exclusive > self.surface_height {
            return Err(ScrollbackSurfaceError::InvalidRowRange);
        }
        if start_row == end_row_exclusive {
            return Ok(()); // :2265-2267
        }

        // drawFrameBuffer (:2250-2253) — the port's payload is the
        // painted text rows of the backing buffer.
        let frame = self.scratch.current_render_buffer.char_frame();
        let lines: Vec<String> = frame
            .split('\n')
            .skip(start_row as usize)
            .take((end_row_exclusive - start_row) as usize)
            .map(str::to_owned)
            .collect();

        let start_on_new_line = self.next_commit_start_on_new_line;
        renderer.enqueue_rendered_scrollback_commit(
            lines,
            options.row_columns,
            Some(start_on_new_line),
            options.trailing_newline,
        );
        self.next_commit_start_on_new_line = false; // :2270
        Ok(())
    }

    /// `surface.destroy()` (renderer.ts:2274-2306) — the surface's
    /// tree + buffers drop with the scratch renderer; idempotent.
    pub fn destroy(&mut self) {
        self.destroyed = true;
    }
}

impl CliRenderer {
    // renderer.ts:2339-2826, 2894-2918
    // -----------------------------------------------------------------

    /// Arm / disarm the split-footer capture path — the runtime base
    /// of the `screenMode "split-footer"` + `externalOutputMode
    /// "capture-stdout"` gate every scrollback write/flush checks
    /// (renderer.ts:2364-2366, 2603-2607). The config-enum resolution
    /// (`resolveModes`, renderer.ts:365-386) rides D1b-surface; this
    /// toggle is what it drives. Disarming mirrors upstream's
    /// transition hygiene: pending commits flush first (the
    /// `flushPendingSplitOutputBeforeTransition(true)` tail,
    /// renderer.ts:1805/1884) and the queue clears.
    pub fn set_split_footer_capture(&mut self, active: bool) {
        if self.destroyed {
            return;
        }
        if !active && self.split_capture_active {
            self.flush_pending_split_output_before_transition(true);
            self.external_output_queue.clear();
        }
        self.split_capture_active = active;
        if !active {
            // syncSplitScrollback's passthrough arm resets the tail
            // (renderer.ts:2968-2972)
            self.split_tail_column = 0;
        }
    }

    /// Whether the split-footer capture path is armed.
    #[must_use]
    pub fn is_split_footer_capture_active(&self) -> bool {
        self.split_capture_active
    }

    /// `writeToScrollback(write)` (renderer.ts:2363-2440) — the
    /// "render to scrollback commit" API: the writer paints a
    /// snapshot against the pending tail column, and the result is
    /// enqueued as ONE [`crate::scrollback::ExternalOutputCommit`] in
    /// the same FIFO as captured stdout (deterministic ordering,
    /// renderer.ts:2341-2346), then a render is requested so the
    /// flush can batch commits into one frame (renderer.ts:2614).
    ///
    /// Upstream hands the writer a `root: Renderable` and renders it
    /// through an off-screen `OptimizedBuffer` (:2397-2402); the
    /// port's producers paint through a scratch renderer (the
    /// contract `opentui-solid`'s `createScrollbackWriter` already
    /// feeds), so the snapshot arrives as painted rows and the
    /// width/height clamps (`getSnapshotWidth`/`getSnapshotHeight`,
    /// :2477-2499) apply to the row set. `teardown` is the scratch
    /// renderer's drop — nothing to invoke.
    pub fn write_to_scrollback(
        &mut self,
        writer: crate::scrollback::ScrollbackWriter<'_>,
    ) -> Result<(), ScrollbackWriteError> {
        if !self.split_capture_active {
            return Err(ScrollbackWriteError::CaptureNotActive); // renderer.ts:2364-2366
        }

        // The writer context — width/widthMethod/tailColumn
        // (renderer.ts:2368-2375); the tail column is the PENDING one
        // (queued commits included, renderer.ts:2572-2583), and the
        // width method is the live one — `renderer.widthMethod`
        // (renderer.ts:1468: `capabilities?.unicode ?? buffer`), wired
        // since D1b-surface (the D1b-core base reported the zig
        // default).
        let ctx = crate::scrollback::ScrollbackRenderContext {
            width: self.config.width.max(1),
            width_method: self.width_method(),
            tail_column: self.get_pending_split_tail_column(),
        };
        let snapshot = writer(&ctx);
        if snapshot.lines.is_empty() {
            return Err(ScrollbackWriteError::EmptySnapshot); // renderer.ts:2383-2385
        }

        self.enqueue_rendered_scrollback_commit(
            snapshot.lines,
            snapshot.row_columns,
            snapshot.start_on_new_line,
            snapshot.trailing_newline,
        );
        Ok(())
    }

    /// `enqueueRenderedScrollbackCommit` (renderer.ts:2583-2614) —
    /// clamp the rowColumns budget, default the newline flags,
    /// enqueue ONE commit in the FIFO, and request a render so the
    /// flush can batch commits into one frame (:2614). Shared by
    /// `writeToScrollback` and the scrollback *surface* commit path
    /// (`ScrollbackSurface.commitRows`, :2245-2272).
    pub fn enqueue_rendered_scrollback_commit(
        &mut self,
        lines: Vec<String>,
        row_columns: Option<u32>,
        start_on_new_line: Option<bool>,
        trailing_newline: Option<bool>,
    ) {
        let width = self.config.width.max(1);
        let clamped =
            crate::scrollback::snapshot_width_clamp(row_columns.unwrap_or(width), width);
        let row_columns = if row_columns.is_some() {
            clamped
        } else {
            width
        };
        let commit = crate::scrollback::ExternalOutputCommit {
            lines,
            row_columns,
            start_on_new_line: start_on_new_line.unwrap_or(true), // :2612
            trailing_newline: trailing_newline.unwrap_or(true),   // :2613
        };
        self.enqueue_split_commit(commit);
        self.request_render(); // renderer.ts:2614
    }

    /// `enqueueSplitCommit` (renderer.ts:2608-2613) — queue the
    /// commit, then emit `EXTERNAL_OUTPUT` only when a listener is
    /// registered (the `listenerCount > 0` gate, :2610).
    fn enqueue_split_commit(&mut self, commit: crate::scrollback::ExternalOutputCommit) {
        self.external_output_queue.write_snapshot(commit.clone());
        if !self.event_listeners.is_empty() {
            self.emit_event(CliRenderEvent::ExternalOutput(commit));
        }
    }

    /// `getPendingSplitTailColumn` (renderer.ts:2572-2583) — the tail
    /// column after every QUEUED commit replays: what the next writer
    /// sees.
    #[must_use]
    pub fn get_pending_split_tail_column(&self) -> u32 {
        let width = self.config.width.max(1);
        let mut tail_column = self.split_tail_column;
        for commit in self.external_output_queue.peek(usize::MAX) {
            tail_column =
                crate::scrollback::tail_column_after_commit(commit, tail_column, width);
        }
        tail_column
    }

    /// `flushPendingSplitCommits` (renderer.ts:2729-2826) — the flush
    /// adjudication: when may external output be committed into the
    /// transcript region. One render tick drains at most
    /// [`MAX_SPLIT_COMMITS_PER_FRAME`] commits (the bounded peek,
    /// :2737 — one modest frame per tick); leftover commits stay
    /// queued and re-request a render (:2817-2820). Each accepted
    /// commit appends its rows (newline intent per the flags) — to
    /// the live session's output when one is attached, else to the
    /// headless transcript model — then the accepted prefix drops and
    /// its tail columns record (:2795-2797).
    pub fn flush_pending_split_commits(
        &mut self,
        _force_footer_repaint: bool,
        drain_all: bool,
    ) -> SplitFlushStatus {
        let limit = if drain_all {
            usize::MAX
        } else {
            MAX_SPLIT_COMMITS_PER_FRAME
        };
        let commits: Vec<_> = self
            .external_output_queue
            .peek(limit)
            .to_vec();
        let mut accepted = 0usize;
        let mut failed = false;
        let mut pending_bytes = String::new();
        let mut tail_column = self.split_tail_column;

        for commit in &commits {
            // the payload byte stream (newline intent per the flags —
            // the commitSplitFooterSnapshot payload half,
            // renderer.ts:2752-2760)
            pending_bytes.push_str(&commit.transcript_bytes(tail_column));
            // record the tail AFTER this commit (recordSplitCommit,
            // renderer.ts:2568-2570 via :2796)
            tail_column =
                crate::scrollback::tail_column_after_commit(commit, tail_column, self.config.width.max(1));
            accepted += 1;
        }

        if accepted > 0 {
            #[cfg(feature = "terminal-backend")]
            let write_result = self.write_split_bytes_live(&pending_bytes);
            #[cfg(not(feature = "terminal-backend"))]
            let write_result: Result<(), std::io::Error> = Ok(());
            match write_result {
                Ok(()) => {
                    self.split_transcript.push_str(&pending_bytes);
                }
                Err(_) => failed = true,
            }
        }

        if failed {
            return SplitFlushStatus::Failed;
        }

        if accepted > 0 {
            self.split_tail_column = tail_column;
            self.external_output_queue.drop_commits(accepted);
        }

        if self.external_output_queue.size() > 0 {
            // Preserve FIFO ordering without unbounded work in one
            // tick (renderer.ts:2817-2820).
            self.request_render();
        }

        SplitFlushStatus::Rendered
    }

    /// Write the flushed transcript bytes through the live session —
    /// one `write_bytes` batch (the native frame envelope upstream is
    /// begin/finalize around exactly this payload,
    /// renderer.ts:2752-2760).
    #[cfg(feature = "terminal-backend")]
    fn write_split_bytes_live(&mut self, bytes: &str) -> std::io::Result<()> {
        let Some(session) = self.session.as_mut() else {
            return Ok(()); // headless: the transcript model absorbs it
        };
        session.write_bytes(bytes.as_bytes())
    }

    /// `flushPendingSplitOutputBeforeTransition` (renderer.ts:2894-2918)
    /// — the pre-transition drain: before a mode/geometry transition
    /// (or destroy), flush the whole queue (drainAll) so no captured
    /// output is lost. Guards mirror upstream: inactive capture or an
    /// empty queue without a forced repaint is a no-op (:2900-2917).
    pub fn flush_pending_split_output_before_transition(&mut self, force_footer_repaint: bool) {
        if !self.split_capture_active {
            return; // renderer.ts:2900-2905
        }
        if self.external_output_queue.size() == 0 && !force_footer_repaint {
            return; // renderer.ts:2907-2909
        }
        self.flush_pending_split_commits(force_footer_repaint, true); // drainAll, :2917
    }

    /// The headless transcript model — the bytes the flush
    /// adjudication has committed above the footer (the differential
    /// harness's view; the live path writes the same bytes through the
    /// session).
    #[must_use]
    pub fn split_transcript(&self) -> &str {
        &self.split_transcript
    }

    /// `externalOutputQueue.size` — the pending commit count.
    #[must_use]
    pub fn external_output_queue_size(&self) -> usize {
        self.external_output_queue.size()
    }

    /// The live queue (`externalOutputQueue`) — read-only peek access
    /// for hosts asserting pending commits.
    #[must_use]
    pub fn external_output_queue(&self) -> &crate::scrollback::ExternalOutputQueue {
        &self.external_output_queue
    }

    /// The committed tail column (`splitTailColumn`, renderer.ts:901).
    #[must_use]
    pub fn split_tail_column(&self) -> u32 {
        self.split_tail_column
    }

    // -----------------------------------------------------------------
    // Split-footer surface family (D1b-surface) —
    // renderer.ts:1984-2338 (createScrollbackSurface), 2446-2475
    // (resetSplitFooterForReplay)
    // -----------------------------------------------------------------

    /// `createScrollbackSurface(options)` (renderer.ts:1984-2338) —
    /// a retained surface the host renders streaming entries onto and
    /// commits row ranges from (ref mini `RunScrollbackStream`:
    /// mount renderables under `surface.root`, `render()`/`settle()`,
    /// `commitRows(committed, target)` per flush).
    ///
    /// Requires the split-footer capture mode (renderer.ts:1985-1989:
    /// upstream throws; the port returns the error). Upstream's
    /// surface keeps its render tree on the SAME renderer through a
    /// snapshot `RenderContext` + off-screen `OptimizedBuffer`; the
    /// port's renderables are bound to their taffy tree, so the
    /// surface owns a SCRATCH renderer (the same shape
    /// `opentui-solid`'s scrollback writer uses) and
    /// [`ScrollbackSurface::commit_rows`] hands the painted rows back
    /// to this renderer's commit FIFO — bounded divergence, noted in
    /// the surface docs.
    pub fn create_scrollback_surface(
        &mut self,
        options: ScrollbackSurfaceOptions,
    ) -> Result<ScrollbackSurface, ScrollbackSurfaceError> {
        if self.screen_mode != ScreenMode::SplitFooter
            || self.external_output_mode != ExternalOutputMode::CaptureStdout
        {
            // renderer.ts:1985-1989
            return Err(ScrollbackSurfaceError::RequiresSplitFooterCapture);
        }
        ScrollbackSurface::new(self, options)
    }

    /// `resetSplitFooterForReplay(options)` (renderer.ts:2446-2475):
    /// drain the pending commits, drop the queue, reset the split
    /// scrollback state, and (live) emit the replay-reset control
    /// sequence (`resetScrollRegion + reset + home + clearScreen
    /// [+ clearSavedLines] + home`, :2466-2473).
    ///
    /// Requires the split-footer capture mode (:2450-2454) and an
    /// "active terminal" (:2455-2457) — upstream's
    /// `_terminalIsSetup`; the port treats a headless renderer (no
    /// attached session) as active too, since the headless transcript
    /// model IS its terminal (bounded divergence).
    pub fn reset_split_footer_for_replay(&mut self, clear_saved_lines: bool) -> Result<(), ScrollbackSurfaceError> {
        if self.destroyed {
            return Ok(()); // renderer.ts:2448
        }
        if self.screen_mode != ScreenMode::SplitFooter
            || self.external_output_mode != ExternalOutputMode::CaptureStdout
        {
            return Err(ScrollbackSurfaceError::RequiresSplitFooterCapture); // :2450-2454
        }
        self.flush_pending_split_output_before_transition(true); // :2460
        self.external_output_queue.clear(); // :2461
        // resetSplitScrollback (:2462 → :2921-2924): the native
        // transcript cursor resets; the port's tracked tail resets
        // with it.
        self.split_tail_column = 0;
        // clearSavedLines (:2471) — the transcript model is the port's
        // saved-lines view.
        if clear_saved_lines {
            self.split_transcript.clear();
        }
        // The control-sequence burst (:2466-2473) reaches the real
        // terminal only (writeOut); the headless model absorbs
        // nothing — the next repaint is full (`forceFullRepaint`,
        // :2465 → the port's request below).
        #[cfg(feature = "terminal-backend")]
        {
            let seq = format!(
                "{}{}{}{}{}{}",
                crate::ansi::ansi_consts::RESET_SCROLL_REGION,
                crate::ansi::ansi_consts::RESET,
                crate::ansi::ansi_consts::HOME,
                crate::ansi::ansi_consts::CLEAR,
                if clear_saved_lines {
                    crate::ansi::ansi_consts::CLEAR_SAVED_LINES
                } else {
                    ""
                },
                crate::ansi::ansi_consts::HOME,
            );
            let _ = self.write_split_bytes_live(&seq);
        }
        self.request_render(); // :2474
        Ok(())
    }

}
