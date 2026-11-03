//! The split-footer scrollback runtime base — the
//! `ExternalOutputCommit` family (renderer.ts:2339+).
//!
//! This is lane D1b-core: the write path
//! ([`crate::renderer::CliRenderer::write_to_scrollback`]), the FIFO
//! commit queue, the tail-column (newline-ownership) arithmetic, and
//! the flush adjudication ("when may external output be flushed into
//! the transcript region"). The *surface* family
//! (`createScrollbackSurface`, the `screenMode`/`footerHeight`/
//! `externalOutputMode` config enums, `resetSplitFooterForReplay`)
//! stays with lane D1b-surface.
//!
//! Upstream shapes ported here:
//!
//! * [`ScrollbackRenderContext`] / [`ScrollbackSnapshot`] /
//!   [`ScrollbackWriter`] — renderer.ts:295-314;
//! * [`ExternalOutputCommit`] — renderer.ts:395-400 (the payload is
//!   painted text rows instead of an `OptimizedBuffer`: the port's
//!   snapshot producers paint through a scratch renderer — the same
//!   contract `opentui-solid`'s `createScrollbackWriter` already
//!   consumes via `ScrollbackSnapshot::paint_lines`);
//! * [`ExternalOutputQueue`] — renderer.ts:403-441;
//! * the tail-column family — `getSnapshotRowWidths`
//!   (renderer.ts:2497-2519), `advanceSplitTailColumn` (:2521-2532),
//!   `getSplitTailColumnAfterCommit` (:2546-2566).

use crate::text_buffer::cluster_width;

/// Display width of a painted row (grapheme clusters summed) — the
/// char-cell count of [`ExternalOutputCommit`] rows.
#[must_use]
pub fn row_display_width(line: &str) -> u32 {
    use unicode_segmentation::UnicodeSegmentation;
    line.graphemes(true).map(cluster_width).sum()
}

/// `ScrollbackRenderContext` (renderer.ts:295-300) — the slice the
/// writer consumes: the renderer's live width, its width method, and
/// the pending tail column (where the last flushed commit left the
/// cursor inside the transcript's last row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackRenderContext {
    pub width: u32,
    pub width_method: crate::renderer::WidthMethod,
    pub tail_column: u32,
}

/// `ScrollbackSnapshot` (renderer.ts:304-313) — the writer's return
/// value. Upstream carries a `root: Renderable` the renderer renders
/// into an off-screen `OptimizedBuffer`; the port's producers paint
/// through a scratch renderer (opentui-solid's
/// `createScrollbackWriter` contract), so the payload arrives as
/// painted rows plus the snapshot knobs. `teardown` upstream is the
/// scratch renderer's drop — nothing to call here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrollbackSnapshot {
    /// The painted rows — the snapshot's payload (`snapshot` on the
    /// commit shape, renderer.ts:396).
    pub lines: Vec<String>,
    /// `rowColumns` (renderer.ts:308) — the payload column budget per
    /// row; `None` = the full snapshot width (the natural width of
    /// the painted rows).
    pub row_columns: Option<u32>,
    /// `startOnNewLine` (renderer.ts:309) — add a newline before this
    /// commit if the previous one ended mid-row. Default `true`
    /// (renderer.ts:2612).
    pub start_on_new_line: Option<bool>,
    /// `trailingNewline` (renderer.ts:310) — add a newline after this
    /// commit's final row. Default `true` (renderer.ts:2613).
    pub trailing_newline: Option<bool>,
}

/// `ScrollbackWriter = (ctx) => ScrollbackSnapshot`
/// (renderer.ts:314).
pub type ScrollbackWriter<'a> =
    Box<dyn FnOnce(&ScrollbackRenderContext) -> ScrollbackSnapshot + 'a>;

/// `ExternalOutputCommit` (renderer.ts:395-400) — one scrollback
/// append step, shared by `writeToScrollback` and captured stdout
/// (renderer.ts:2615-2618: one commit shape keeps split-append
/// behavior consistent across both sources). The `snapshot`
/// `OptimizedBuffer` is the port's painted `lines` (bounded
/// divergence — no native buffer; rows carry the same payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalOutputCommit {
    /// The painted payload rows.
    pub lines: Vec<String>,
    /// `rowColumns` (renderer.ts:397) — the column budget the
    /// tail-column math reads (trailing blanks past it do not count).
    pub row_columns: u32,
    /// `startOnNewLine` (renderer.ts:398).
    pub start_on_new_line: bool,
    /// `trailingNewline` (renderer.ts:399).
    pub trailing_newline: bool,
}

/// `CliRendererExternalOutputEvent` (renderer.ts:252-257) — the
/// `EXTERNAL_OUTPUT` event payload; upstream is exactly the commit
/// (`this.emit(CliRenderEvents.EXTERNAL_OUTPUT, commit)`,
/// renderer.ts:2611-2612), so the port's payload is the commit too.
pub type CliRendererExternalOutputEvent = ExternalOutputCommit;

impl ExternalOutputCommit {
    /// `getSnapshotRowWidths` (renderer.ts:2497-2519) — each row's
    /// occupied width: start at `min(rowColumns, snapshot width)` and
    /// skip trailing blank cells (upstream's `cp === 0` walk-back;
    /// the port's rows are text, so trailing spaces/blanks trim the
    /// same way — a row's width is what it occupies, not its budget).
    #[must_use]
    pub fn row_widths(&self) -> Vec<u32> {
        let limit = self.row_columns;
        self.lines
            .iter()
            .map(|line| {
                // walk back over trailing clusters while the row's
                // occupied width exceeds nothing — i.e. trim trailing
                // spaces, then clamp to the rowColumns budget.
                let trimmed = line.trim_end_matches(' ');
                row_display_width(trimmed).min(limit)
            })
            .collect()
    }

    /// The append bytes one commit contributes to the transcript —
    /// the port's `commitSplitFooterSnapshot` payload half: newline
    /// intent around the rows (`startOnNewLine` terminates a previous
    /// mid-row cursor with `\n`, rows are newline-separated,
    /// `trailingNewline` closes the final row). The native
    /// split-append invariants live upstream in renderer.zig; the
    /// port writes the same byte stream through
    /// [`crate::renderer::CliRenderer`]'s flush — no glued rows
    /// (missing newline) and no double-advance gaps (extra newline),
    /// exactly the contract renderer.ts:2349-2353 describes.
    #[must_use]
    pub fn transcript_bytes(&self, tail_column: u32) -> String {
        let mut out = String::new();
        // newline before the payload only when the cursor sits
        // mid-row (renderer.ts:2349-2351)
        if self.start_on_new_line && tail_column > 0 {
            out.push('\n');
        }
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n'); // between-row advance
            }
            out.push_str(line.trim_end_matches(' '));
        }
        if self.trailing_newline && !self.lines.is_empty() {
            out.push('\n'); // renderer.ts:2351-2353
        }
        out
    }
}

/// `ExternalOutputQueue` (renderer.ts:403-441) — the FIFO between
/// commit producers (`writeToScrollback`, captured stdout) and the
/// render-loop flush. Ordering is deterministic even when both
/// sources interleave (renderer.ts:2341-2346).
#[derive(Debug, Clone, Default)]
pub struct ExternalOutputQueue {
    commits: Vec<ExternalOutputCommit>,
}

impl ExternalOutputQueue {
    /// `get size()` (renderer.ts:407).
    #[must_use]
    pub fn size(&self) -> usize {
        self.commits.len()
    }

    /// `writeSnapshot(commit)` (renderer.ts:410-412).
    pub fn write_snapshot(&mut self, commit: ExternalOutputCommit) {
        self.commits.push(commit);
    }

    /// `peek(limit)` (renderer.ts:415-419) — the first `limit`
    /// commits, unclaimed.
    #[must_use]
    pub fn peek(&self, limit: usize) -> &[ExternalOutputCommit] {
        let limit = limit.max(1);
        &self.commits[..limit.min(self.commits.len())]
    }

    /// `claim(limit)` (renderer.ts:422-436) — take and remove the
    /// first `limit` commits. `claim()` with `usize::MAX` drains
    /// everything.
    pub fn claim(&mut self, limit: usize) -> Vec<ExternalOutputCommit> {
        if self.commits.is_empty() {
            return Vec::new();
        }
        // The upstream bound keeps one render tick's frame modest
        // (renderer.ts:429-432); the caller chooses the limit.
        let take = limit.min(self.commits.len());
        self.commits.drain(..take).collect()
    }

    /// `drop(count)` (renderer.ts:438-441) — remove the first
    /// `count` commits without flushing (upstream destroys their
    /// snapshot buffers; the port's rows need no destruction).
    pub fn drop_commits(&mut self, count: usize) {
        let count = count.min(self.commits.len());
        self.commits.drain(..count);
    }

    /// `clear()` (renderer.ts:443-445 via `drop(size)`).
    pub fn clear(&mut self) {
        self.commits.clear();
    }
}

/// `advanceSplitTailColumn` (renderer.ts:2521-2532) — walk a cursor
/// `columns` forward over `width`-wide rows, wrapping to column 0 at
/// each row boundary.
#[must_use]
pub fn advance_split_tail_column(tail_column: u32, columns: u32, width: u32) -> u32 {
    if width == 0 || columns == 0 {
        return tail_column;
    }
    let mut tail = tail_column;
    let mut remaining = columns;
    while remaining > 0 {
        if tail >= width {
            tail = 0;
        }
        let step = remaining.min(width - tail);
        tail += step;
        remaining -= step;
        if remaining > 0 && tail >= width {
            tail = 0;
        }
    }
    tail
}

/// `getSplitTailColumnAfterCommit` (renderer.ts:2546-2566) — where
/// the cursor lands in the last transcript row once `commit` is
/// appended from `initial_tail_column`: `startOnNewLine` resets a
/// mid-row cursor first, each row advances the tail and resets it
/// (a newline follows every row but the last), and
/// `trailingNewline` resets after the final row too.
#[must_use]
pub fn tail_column_after_commit(
    commit: &ExternalOutputCommit,
    initial_tail_column: u32,
    width: u32,
) -> u32 {
    let width = width.max(1);
    let mut tail_column = initial_tail_column;

    if commit.start_on_new_line && tail_column > 0 {
        tail_column = 0; // renderer.ts:2553-2555
    }

    let row_widths = commit.row_widths();
    for (index, row_width) in row_widths.iter().enumerate() {
        tail_column = advance_split_tail_column(tail_column, *row_width, width);
        if index < row_widths.len() - 1 || commit.trailing_newline {
            tail_column = 0; // renderer.ts:2559-2561
        }
    }

    // An empty payload with trailingNewline still owns its newline
    // (upstream iterates zero rows and skips the reset — renderer.ts:
    // 2556-2562 — so mirror exactly: no rows → no reset).
    tail_column
}

/// `getSnapshotWidth` (renderer.ts:2477-2487) — clamp to `[1,
/// renderer width]`, erroring on non-finite input upstream. The port
/// takes already-finite `u32`s, so this is the pure clamp.
#[must_use]
pub fn snapshot_width_clamp(value: u32, renderer_width: u32) -> u32 {
    value.max(1).min(renderer_width.max(1))
}

/// `getSnapshotHeight` (renderer.ts:2489-2499) — clamp to ≥ 1.
#[must_use]
pub fn snapshot_height_clamp(value: u32) -> u32 {
    value.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(lines: &[&str], row_columns: u32, start: bool, trailing: bool) -> ExternalOutputCommit {
        ExternalOutputCommit {
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            row_columns,
            start_on_new_line: start,
            trailing_newline: trailing,
        }
    }

    // ---- ExternalOutputQueue (renderer.ts:403-441) ----

    /// FIFO order, bounded peek/claim, drop-prefix — the exact
    /// upstream queue contract (renderer.ts:410-445).
    #[test]
    fn queue_fifo_peek_claim_drop() {
        let mut q = ExternalOutputQueue::default();
        assert_eq!(q.size(), 0);
        for i in 0..3 {
            q.write_snapshot(commit(&[&i.to_string()], 1, true, true));
        }
        assert_eq!(q.size(), 3);
        // peek is bounded and non-claiming
        assert_eq!(q.peek(2).len(), 2);
        assert_eq!(q.peek(usize::MAX).len(), 3);
        assert_eq!(q.size(), 3);
        // claim drains a bounded prefix in FIFO order
        let claimed = q.claim(2);
        assert_eq!(claimed.len(), 2);
        assert_eq!(claimed[0].lines, vec!["0".to_owned()]);
        assert_eq!(claimed[1].lines, vec!["1".to_owned()]);
        assert_eq!(q.size(), 1);
        // drop removes the prefix without returning it
        q.write_snapshot(commit(&["x"], 1, true, true));
        q.drop_commits(1);
        assert_eq!(q.size(), 1);
        assert_eq!(q.peek(usize::MAX)[0].lines, vec!["x".to_owned()]);
        // clear (renderer.ts:443-445)
        q.clear();
        assert_eq!(q.size(), 0);
        // claim on empty → empty (renderer.ts:423-425)
        assert!(q.claim(usize::MAX).is_empty());
    }

    // ---- advanceSplitTailColumn (renderer.ts:2521-2532) ----

    /// The wrap decision table: mid-row advance, exact row fill, and
    /// multi-row wrap — every branch of the while loop.
    #[test]
    fn advance_tail_column_decision_table() {
        // columns == 0 → unchanged (renderer.ts:2527-2529)
        assert_eq!(advance_split_tail_column(5, 0, 80), 5);
        // mid-row advance, no wrap
        assert_eq!(advance_split_tail_column(5, 3, 80), 8);
        // exact fill: tail lands AT width (a full row, mid-row cursor
        // for the NEXT writer)
        assert_eq!(advance_split_tail_column(78, 2, 80), 80);
        // wrap mid-payload: advancing 3 from 79 wraps to column 2
        assert_eq!(advance_split_tail_column(79, 3, 80), 2);
        // advance past several full rows
        assert_eq!(advance_split_tail_column(0, 160, 80), 80);
        assert_eq!(advance_split_tail_column(0, 161, 80), 1);
        // tail >= width normalizes to 0 first (renderer.ts:2530-2532)
        assert_eq!(advance_split_tail_column(80, 1, 80), 1);
        // zero width is degenerate — unchanged
        assert_eq!(advance_split_tail_column(4, 3, 0), 4);
    }

    // ---- getSplitTailColumnAfterCommit (renderer.ts:2546-2566) ----

    /// Newline ownership: startOnNewLine resets, between-rows reset,
    /// trailingNewline resets, and the no-reset final row keeps the
    /// advanced tail.
    #[test]
    fn tail_column_after_commit_newline_ownership() {
        // single row, trailingNewline → 0 (renderer.ts:2560-2561)
        assert_eq!(tail_column_after_commit(&commit(&["hello"], 80, true, true), 4, 80), 0);
        // single row, NO trailingNewline → tail advanced by row width
        // (startOnNewLine reset the mid-row cursor first)
        assert_eq!(tail_column_after_commit(&commit(&["hello"], 80, true, false), 4, 80), 5);
        // startOnNewLine resets a mid-row cursor (renderer.ts:2553-2555)
        assert_eq!(tail_column_after_commit(&commit(&["hi"], 80, true, false), 40, 80), 2);
        // startOnNewLine=false on a mid-row cursor keeps appending
        assert_eq!(tail_column_after_commit(&commit(&["hi"], 80, false, false), 40, 80), 42);
        // two rows: between-row reset (renderer.ts:2559-2560)
        assert_eq!(tail_column_after_commit(&commit(&["ab", "cd"], 80, true, false), 0, 80), 2);
        assert_eq!(tail_column_after_commit(&commit(&["ab", "cd"], 80, true, true), 0, 80), 0);
        // empty payload: zero rows iterate → no advance, but the
        // startOnNewLine reset still applies (renderer.ts:2553-2555
        // runs before the row loop)
        assert_eq!(tail_column_after_commit(&commit(&[], 80, true, true), 7, 80), 0);
        assert_eq!(tail_column_after_commit(&commit(&[], 80, false, true), 7, 80), 7);
        // rowColumns budget trims trailing blanks (renderer.ts:2497-2519):
        // "ab   " under a budget of 3 occupies 2 columns
        assert_eq!(tail_column_after_commit(&commit(&["ab   "], 3, true, false), 0, 80), 2);
    }

    // ---- row widths + transcript rows ----

    /// row_widths trims trailing blanks and honors the rowColumns
    /// budget (getSnapshotRowWidths, renderer.ts:2497-2519).
    #[test]
    fn row_widths_trim_and_budget() {
        let c = commit(&["ab  ", "  ", "abcd"], 3, true, true);
        assert_eq!(c.row_widths(), vec![2, 0, 3]);
    }

    /// The emitted byte stream carries the newline intent: no glued
    /// rows (`startOnNewLine` on a mid-row tail), no double-advance
    /// gaps (`trailingNewline` closes the final row exactly once).
    #[test]
    fn transcript_bytes_newline_intent() {
        // mid-row tail + startOnNewLine → leading newline
        let c = commit(&["ab"], 80, true, true);
        assert_eq!(c.transcript_bytes(5), "\nab\n");
        // row-aligned tail → no leading newline (nothing glued)
        let c = commit(&["ab"], 80, true, true);
        assert_eq!(c.transcript_bytes(0), "ab\n");
        // mid-row tail, no startOnNewLine → glued continuation
        let c = commit(&["ab"], 80, false, true);
        assert_eq!(c.transcript_bytes(5), "ab\n");
        // no trailingNewline → cursor stays mid-row
        let c = commit(&["ab"], 80, true, false);
        assert_eq!(c.transcript_bytes(0), "ab");
        // two rows + trailingNewline — consecutive rows, one separator
        let c = commit(&["ab", "cd"], 80, true, true);
        assert_eq!(c.transcript_bytes(0), "ab\ncd\n");
        // empty-line write still advances (createStdoutSnapshotCommits
        // keeps empty rows so scrollback state advances,
        // renderer.ts:2720-2724)
        let c = commit(&[""], 80, true, true);
        assert_eq!(c.transcript_bytes(0), "\n");
        // trailing blanks trim (the snapshot payload's occupied cells)
        let c = commit(&["ab   "], 80, true, true);
        assert_eq!(c.transcript_bytes(0), "ab\n");
    }

    // ---- snapshot clamps (renderer.ts:2477-2499) ----

    #[test]
    fn snapshot_clamps() {
        assert_eq!(snapshot_width_clamp(0, 80), 1);
        assert_eq!(snapshot_width_clamp(999, 80), 80);
        assert_eq!(snapshot_height_clamp(0), 1);
        assert_eq!(snapshot_height_clamp(7), 7);
    }
}
