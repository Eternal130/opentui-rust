//! vt-grid mirror — the SGR replay check.
//!
//! Adapted from opencode-rust `tools/parity/src/tui_diff.rs:106-243`
//! (vt100::Parser → cell grid with style digests): feed the Rust-side SGR
//! byte stream through a vt100 parser at scenario size and verify it
//! reconstructs the same text as the cell grid — i.e. the emitted frame
//! actually paints what the buffer says.

use crate::scenario::Capture;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtCell {
    pub ch: char,
    pub fg_rgb: [u8; 3],
}

pub struct VtGrid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<VtCell>,
}

fn fg_of(cell: &vt100::Cell) -> [u8; 3] {
    match cell.fgcolor() {
        vt100::Color::Rgb(r, g, b) => [r, g, b],
        vt100::Color::Idx(i) => ansi256(i),
        vt100::Color::Default => [255, 255, 255],
    }
}

fn ansi256(i: u8) -> [u8; 3] {
    opentui_core::color::ansi256_index_to_rgb(i)
}

/// Parse an SGR byte stream into a grid.
#[must_use]
pub fn parse_sgr(bytes: &[u8], cols: usize, rows: usize) -> VtGrid {
    let mut parser = vt100::Parser::new(rows as u16, cols as u16, 0);
    parser.process(bytes);
    let screen = parser.screen();
    let mut cells = Vec::with_capacity(rows * cols);
    for y in 0..rows {
        for x in 0..cols {
            let cell = screen.cell(y as u16, x as u16);
            cells.push(cell.map_or(
                VtCell {
                    ch: ' ',
                    fg_rgb: [255, 255, 255],
                },
                |c| VtCell {
                    ch: c.contents().chars().next().unwrap_or(' '),
                    fg_rgb: fg_of(c),
                },
            ));
        }
    }
    VtGrid { cols, rows, cells }
}

/// Check: replay the rust capture's SGR stream; text must equal the
/// capture's char frame (spaces normalized). Wide characters occupy two
/// columns in the vt grid with a `wide_continuation` cell after them —
/// the char frame drops continuations (`getSpanLines`' char walk), so
/// both sides compare as per-row *visible cell* sequences.
///
/// Multi-codepoint grapheme clusters (ZWJ families, VS16 emoji, flag
/// pairs) are beyond the `vt100` parser — it splits them into separate
/// cells, so the replay drifts even though the emission is what a real
/// terminal joins atomically. Frames containing them skip the strict
/// check (the oracle byte-identity compare remains the acceptance
/// gate).
pub fn replay_matches(cap: &Capture) -> Result<(), String> {
    let has_multi_cp_clusters = cap
        .char_frame
        .lines()
        .any(|row| row.graphemes(true).any(|g| g.chars().count() > 1));
    if has_multi_cp_clusters {
        return Err("SKIP: multi-codepoint grapheme clusters exceed the vt100 parser (ZWJ/VS16/flags join atomically in real terminals)".into());
    }

    let grid = parse_sgr(
        &cap.sgr_bytes,
        cap.renderer.width as usize,
        cap.renderer.height as usize,
    );
    // rebuild the vt grid without continuation cells, pairing each
    // visible cell with its column
    let mut parser = vt100::Parser::new(cap.renderer.height as u16, cap.renderer.width as u16, 0);
    parser.process(&cap.sgr_bytes);
    let screen = parser.screen();

    for y in 0..grid.rows {
        let row_text = cap.char_frame.split('\n').nth(y).unwrap_or("");
        let mut expected = row_text.chars().peekable();
        for x in 0..grid.cols {
            let cell = screen.cell(y as u16, x as u16);
            let is_continuation = cell.is_some_and(|c| c.is_wide_continuation());
            if is_continuation {
                continue; // occupies a column, paints no char
            }
            let got = grid.cells[y * grid.cols + x].ch;
            let want = expected.next().unwrap_or(' ');
            if got != want {
                return Err(format!(
                    "SGR replay mismatch at ({x},{y}): painted `{got:?}` expected `{want:?}`"
                ));
            }
        }
        if expected.next().is_some() {
            return Err(format!("SGR replay mismatch at row {y}: expected frame has more text than the replay painted"));
        }
    }
    Ok(())
}
