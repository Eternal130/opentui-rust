//! vt-grid mirror — the SGR replay check.
//!
//! Adapted from opencode-rust `tools/parity/src/tui_diff.rs:106-243`
//! (vt100::Parser → cell grid with style digests): feed the Rust-side SGR
//! byte stream through a vt100 parser at scenario size and verify it
//! reconstructs the same text as the cell grid — i.e. the emitted frame
//! actually paints what the buffer says.

use crate::scenario::Capture;

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
            cells.push(cell.map_or(VtCell { ch: ' ', fg_rgb: [255, 255, 255] }, |c| VtCell {
                ch: c.contents().chars().next().unwrap_or(' '),
                fg_rgb: fg_of(&c),
            }));
        }
    }
    VtGrid { cols, rows, cells }
}

/// Check: replay the rust capture's SGR stream; text must equal the
/// capture's char frame (spaces normalized).
#[must_use]
pub fn replay_matches(cap: &Capture) -> Result<(), String> {
    let grid = parse_sgr(&cap.sgr_bytes, cap.renderer.width as usize, cap.renderer.height as usize);
    let expected: Vec<Vec<char>> = cap
        .char_frame
        .split('\n')
        .map(|r| r.chars().collect())
        .collect();
    for y in 0..grid.rows {
        for x in 0..grid.cols {
            let got = grid.cells[y * grid.cols + x].ch;
            let want = expected
                .get(y)
                .and_then(|r| r.get(x))
                .copied()
                .unwrap_or(' ');
            if got != want {
                return Err(format!(
                    "SGR replay mismatch at ({x},{y}): painted `{got:?}` expected `{want:?}`"
                ));
            }
        }
    }
    Ok(())
}
