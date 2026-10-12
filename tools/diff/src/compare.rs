//! Cell-grid comparison + report.
//!
//! Primary oracle: expand both captures' spans to per-cell grids
//! (char, fg u16x4, bg u16x4, attributes) and compare byte-level. The
//! vt-grid mirror (`vt.rs`) provides the secondary SGR-replay check.

use crate::scenario::Capture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub fg: [u16; 4],
    pub bg: [u16; 4],
    pub attributes: u32,
}

/// Expand span lines to a cell grid (spans are runs; continuation cells
/// carry "" text with width — buffer.ts:172-234 semantics).
#[must_use]
pub fn capture_cells(cap: &Capture) -> Vec<Vec<Cell>> {
    cap.lines
        .iter()
        .map(|line| {
            let mut cells: Vec<Cell> = Vec::new();
            for span in &line.spans {
                let fg = [span.fg[0], span.fg[1], span.fg[2], span.fg[3]];
                let bg = [span.bg[0], span.bg[1], span.bg[2], span.bg[3]];
                let graphemes: Vec<String> = span.text.chars().map(|c| c.to_string()).collect();
                for i in 0..span.width as usize {
                    // continuation cells: grapheme count < width
                    let text = graphemes.get(i).cloned().unwrap_or_default();
                    cells.push(Cell {
                        text,
                        fg,
                        bg,
                        attributes: span.attributes,
                    });
                }
            }
            cells
        })
        .collect()
}

#[derive(Debug)]
pub struct CellDiff {
    pub x: usize,
    pub y: usize,
    pub field: &'static str,
    pub oracle: String,
    pub rust: String,
}

pub struct Report {
    pub identical: bool,
    pub total_cells: usize,
    pub mismatched_cells: usize,
    pub diffs: Vec<CellDiff>,
    pub oracle_frame: String,
    pub rust_frame: String,
}

fn rgba_str(w: &[u16; 4]) -> String {
    let bytes = [w[0] as u8, w[1] as u8, w[2] as u8, w[3] as u8];
    format!(
        "rgba({:#04x?}) intent={:?} slot={}",
        bytes,
        intent_of(w),
        w[0] >> 8
    )
}

fn intent_of(w: &[u16; 4]) -> &'static str {
    // meta byte0 = bits 8-15 of the metadata word = high byte of word 0
    match (w[0] >> 8) & 0xff {
        1 => "indexed",
        2 => "default",
        _ => "rgb",
    }
}

#[must_use]
pub fn compare(oracle: &Capture, rust: &Capture) -> Report {
    let oc = capture_cells(oracle);
    let rc = capture_cells(rust);
    let mut diffs = Vec::new();
    let height = oc.len().max(rc.len());
    for y in 0..height {
        let orow = oc.get(y).map(Vec::as_slice).unwrap_or(&[]);
        let rrow = rc.get(y).map(Vec::as_slice).unwrap_or(&[]);
        let width = orow.len().max(rrow.len());
        for x in 0..width {
            let o = orow.get(x);
            let r = rrow.get(x);
            if o == r {
                continue;
            }
            if let (Some(o), Some(r)) = (o, r) {
                if o.text != r.text {
                    diffs.push(CellDiff {
                        x,
                        y,
                        field: "char",
                        oracle: o.text.clone(),
                        rust: r.text.clone(),
                    });
                }
                if o.fg != r.fg {
                    diffs.push(CellDiff {
                        x,
                        y,
                        field: "fg",
                        oracle: rgba_str(&o.fg),
                        rust: rgba_str(&r.fg),
                    });
                }
                if o.bg != r.bg {
                    diffs.push(CellDiff {
                        x,
                        y,
                        field: "bg",
                        oracle: rgba_str(&o.bg),
                        rust: rgba_str(&r.bg),
                    });
                }
                if o.attributes != r.attributes {
                    diffs.push(CellDiff {
                        x,
                        y,
                        field: "attributes",
                        oracle: o.attributes.to_string(),
                        rust: r.attributes.to_string(),
                    });
                }
            } else {
                diffs.push(CellDiff {
                    x,
                    y,
                    field: "presence",
                    oracle: o.map_or_else(|| "<none>".into(), |c| c.text.clone()),
                    rust: r.map_or_else(|| "<none>".into(), |c| c.text.clone()),
                });
            }
        }
    }
    let total_cells: usize = oc.iter().map(Vec::len).sum();
    let mismatched = diffs
        .iter()
        .map(|d| (d.y, d.x))
        .collect::<std::collections::HashSet<_>>()
        .len();
    Report {
        identical: diffs.is_empty(),
        total_cells,
        mismatched_cells: mismatched,
        diffs,
        oracle_frame: oracle.char_frame.clone(),
        rust_frame: rust.char_frame.clone(),
    }
}

impl Report {
    #[must_use]
    pub fn to_markdown(&self, name: &str) -> String {
        let mut md = String::new();
        md.push_str(&format!(
            "# diff report: {name}\n\n**{}** — {}/{} cells differ ({} field diffs)\n\n",
            if self.identical {
                "IDENTICAL"
            } else {
                "MISMATCH"
            },
            self.mismatched_cells,
            self.total_cells,
            self.diffs.len()
        ));
        let show = self.diffs.len().min(40);
        for d in &self.diffs[..show] {
            md.push_str(&format!(
                "- ({},{}) {}: oracle `{}` vs rust `{}`\n",
                d.x, d.y, d.field, d.oracle, d.rust
            ));
        }
        if self.diffs.len() > show {
            md.push_str(&format!("… {} more\n", self.diffs.len() - show));
        }
        md.push_str("\n## oracle frame\n\n```\n");
        md.push_str(&self.oracle_frame.replace('\n', "⏎\n"));
        md.push_str("\n```\n\n## rust frame\n\n```\n");
        md.push_str(&self.rust_frame.replace('\n', "⏎\n"));
        md.push_str("\n```\n");
        md
    }
}
