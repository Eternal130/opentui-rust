//! Rig sanity: the synthetic tree builders must actually PAINT (a
//! builder regression that mounts nothing — or paints only spaces —
//! would make every render benchmark measure an empty tree and rot the
//! frame-budget numbers silently).

use opentui_bench::{build_culling_tree, build_mixed_tree, build_wordwrap_tree};

fn painted_rows(r: &opentui_core::renderer::CliRenderer) -> usize {
    let buf = &r.current_render_buffer;
    let cleared = opentui_core::color::Rgba::from_ints(0, 0, 0, 0);
    (0..buf.height)
        .filter(|&y| {
            (0..buf.width).any(|x| {
                buf.get(x, y).is_some_and(|c| {
                    c.char != opentui_core::buffer::DEFAULT_SPACE_CHAR || c.bg != cleared
                })
            })
        })
        .count()
}

#[test]
fn synthetic_trees_paint_content() {
    for (w, h) in [(80u32, 24u32), (120, 30), (200, 50)] {
        // mixed: visible panels + text leaves
        let r = build_mixed_tree(w, h);
        let rows = painted_rows(&r);
        assert!(
            rows >= (h / 2) as usize,
            "mixed {w}x{h}: only {rows} painted rows"
        );

        // wordwrap: paragraphs fill the viewport
        let r = build_wordwrap_tree(w, h);
        let rows = painted_rows(&r);
        assert!(
            rows >= (h / 2) as usize,
            "wordwrap {w}x{h}: only {rows} painted rows"
        );

        // culling scrollbox: rows visible mid-scroll
        let r = build_culling_tree(w, h, 120.max(h * 8) as usize);
        let rows = painted_rows(&r);
        assert!(
            rows >= (h / 2) as usize,
            "culling {w}x{h}: only {rows} painted rows"
        );
    }
}
