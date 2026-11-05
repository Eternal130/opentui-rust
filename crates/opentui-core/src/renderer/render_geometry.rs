//! `render-geometry.ts` — the split-footer render-geometry contract
//! (upstream `packages/core/src/lib/render-geometry.ts`, whole file).
//!
//! Upstream: `calculateRenderGeometry` (render-geometry.ts:10-34) is
//! the single source of truth for how a screen mode maps terminal
//! space onto renderer surface space. Non-split modes are the
//! identity (effectiveFooterHeight 0, offset 0, full terminal
//! extents); `"split-footer"` clamps the footer band to the terminal
//! height and places it at the bottom:
//!
//! * `effectiveFooterHeight = min(footerHeight, terminalHeight)`
//! * `renderOffset = terminalHeight - effectiveFooterHeight`
//! * `renderWidth = terminalWidth`, `renderHeight = effectiveFooterHeight`
//!
//! The renderer applies this at construction (`initialGeometry`,
//! renderer.ts:1075) and on every mode/footer change
//! (`applyScreenMode`'s `nextGeometry`, renderer.ts:3015-3019 —
//! `this.width = nextGeometry.renderWidth; this.height =
//! nextGeometry.renderHeight`, renderer.ts:3160-3161).

use super::ScreenMode;

/// `RenderGeometry` (render-geometry.ts:3-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderGeometry {
    /// `effectiveFooterHeight` (render-geometry.ts:4).
    pub effective_footer_height: u32,
    /// `renderOffset` (render-geometry.ts:5) — how many terminal rows
    /// down the renderer surface starts (the footer band's top).
    pub render_offset: u32,
    /// `renderWidth` (render-geometry.ts:6).
    pub render_width: u32,
    /// `renderHeight` (render-geometry.ts:7).
    pub render_height: u32,
}

/// `calculateRenderGeometry` (render-geometry.ts:10-34) — verbatim.
#[must_use]
pub fn calculate_render_geometry(
    screen_mode: ScreenMode,
    terminal_width: u32,
    terminal_height: u32,
    footer_height: u32,
) -> RenderGeometry {
    // Math.max(v, 0) is identity on u32 (render-geometry.ts:15-16).
    let safe_terminal_width = terminal_width;
    let safe_terminal_height = terminal_height;

    if screen_mode != ScreenMode::SplitFooter {
        // render-geometry.ts:22-29
        return RenderGeometry {
            effective_footer_height: 0,
            render_offset: 0,
            render_width: safe_terminal_width,
            render_height: safe_terminal_height,
        };
    }

    // render-geometry.ts:31-33
    let effective_footer_height = footer_height.min(safe_terminal_height);
    RenderGeometry {
        effective_footer_height,
        render_offset: safe_terminal_height - effective_footer_height,
        render_width: safe_terminal_width,
        render_height: effective_footer_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The non-split identity (render-geometry.ts:22-29): every other
    /// mode renders the full terminal extents with zero offset.
    #[test]
    fn non_split_modes_are_identity() {
        for mode in [ScreenMode::AlternateScreen, ScreenMode::MainScreen] {
            assert_eq!(
                calculate_render_geometry(mode, 80, 24, 12),
                RenderGeometry {
                    effective_footer_height: 0,
                    render_offset: 0,
                    render_width: 80,
                    render_height: 24,
                }
            );
        }
    }

    /// The split-footer band (render-geometry.ts:31-39): footer clamped
    /// to the terminal height, offset = the rows above the band.
    #[test]
    fn split_footer_band_geometry() {
        assert_eq!(
            calculate_render_geometry(ScreenMode::SplitFooter, 100, 30, 8),
            RenderGeometry {
                effective_footer_height: 8,
                render_offset: 22,
                render_width: 100,
                render_height: 8,
            }
        );
        // footer taller than the terminal clamps to the full height
        // (render-geometry.ts:31: Math.min)
        assert_eq!(
            calculate_render_geometry(ScreenMode::SplitFooter, 100, 10, 40),
            RenderGeometry {
                effective_footer_height: 10,
                render_offset: 0,
                render_width: 100,
                render_height: 10,
            }
        );
    }
}
