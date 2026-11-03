//! `TerminalColors` — the terminal-palette data contract the ref
//! theme layer consumes (`context/theme.tsx:1`,
//! `mini/theme.ts:1,273,297,381`). Upstream真身:
//! `core/src/lib/terminal-palette.ts`.
//!
//! Placed at the crate root (not under `terminal/`) because the
//! palette is a pure data contract with no backend — the `terminal`
//! module is `terminal-backend`-feature-gated and the ref theme layer
//! consumes this type unconditionally.
//!
//! Lane D1c item e ports the **type + construction** half only:
//! * `TerminalColors` (terminal-palette.ts:24-35) — the detected
//!   palette as `#rrggbb` hex (or null per slot);
//! * `normalizeTerminalPalette` (:415-429) — hex → `RGBA` with the
//!   fallback 256-palette / default fg/bg for missing slots.
//!
//! The **detector** half (`TerminalPalette` class, :93+, OSC 4/10/11
//! queries with idle timeout) stays deferred — the PALETTE-side of
//! upstream's renderer remains gap-registered in `terminal/mod.rs`
//! and the PORT-MATRIX row is unconsumed by the ref TUI runtime.

use crate::color::Rgba;

/// A `#rrggbb` hex color or `null` (undetected slot).
pub type Hex = Option<String>;

/// `TerminalColors` (terminal-palette.ts:24-35).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalColors {
    /// The 16 ANSI palette slots (OSC 4 answers; `null` = unanswered).
    pub palette: Vec<Hex>,
    pub default_foreground: Hex,
    pub default_background: Hex,
    pub cursor_color: Hex,
    pub mouse_foreground: Hex,
    pub mouse_background: Hex,
    pub tek_foreground: Hex,
    pub tek_background: Hex,
    pub highlight_background: Hex,
    pub highlight_foreground: Hex,
}

impl TerminalColors {
    /// An all-null palette — what an empty detection yields.
    #[must_use]
    pub fn undetected() -> Self {
        Self {
            palette: vec![None; 16],
            ..Self::default()
        }
    }
}

/// `NormalizedTerminalPalette` (terminal-palette.ts:36-41).
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedTerminalPalette {
    /// 256 entries — detected slots over the fallback ANSI-256 table.
    pub palette: Vec<Rgba>,
    pub default_foreground: Rgba,
    pub default_background: Rgba,
}

/// `RGBA.fromHex` (RGBA.ts) — the `#rrggbb` (or `#rgb`) form the OSC
/// answers use; returns the shared white fallback on malformed input.
#[must_use]
pub fn rgba_from_hex(hex: &str) -> Rgba {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    let (r, g, b) = match digits.as_bytes() {
        [r, g, b] => (expand_short(*r), expand_short(*g), expand_short(*b)),
        [a, b, c, d, e, f] => (
            hex_pair(*a, *b).unwrap_or(255),
            hex_pair(*c, *d).unwrap_or(255),
            hex_pair(*e, *f).unwrap_or(255),
        ),
        _ => (255, 255, 255),
    };
    Rgba::rgb(r, g, b)
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
    Some(hex_val(hi)? * 16 + hex_val(lo)?)
}

fn expand_short(byte: u8) -> u8 {
    hex_val(byte).map_or(255, |v| v * 17)
}

/// `normalizeTerminalPalette` (terminal-palette.ts:415-429): every
/// missing slot falls back — palette entries to the ANSI-256 table
/// (`RGBA.fromIndex`), fg/bg to the renderer defaults.
#[must_use]
pub fn normalize_terminal_palette(colors: Option<&TerminalColors>) -> NormalizedTerminalPalette {
    let fallback = |index: usize| Rgba::from_index((index.min(255)) as u8);
    NormalizedTerminalPalette {
        palette: (0..256)
            .map(|index| {
                colors
                    .and_then(|c| c.palette.get(index).cloned().flatten())
                    .map(|hex| rgba_from_hex(&hex))
                    .unwrap_or_else(|| fallback(index))
            })
            .collect(),
        default_foreground: colors
            .and_then(|c| c.default_foreground.clone())
            .map(|hex| rgba_from_hex(&hex))
            .unwrap_or_else(Rgba::default_foreground),
        default_background: colors
            .and_then(|c| c.default_background.clone())
            .map(|hex| rgba_from_hex(&hex))
            .unwrap_or_else(Rgba::default_background),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_colors_shape() {
        let colors = TerminalColors::undetected();
        assert_eq!(colors.palette.len(), 16);
        assert!(colors.palette.iter().all(|slot| slot.is_none()));
        assert!(colors.default_foreground.is_none());
    }

    #[test]
    fn normalize_fills_missing_slots_with_fallbacks() {
        let normalized = normalize_terminal_palette(None);
        assert_eq!(normalized.palette.len(), 256);
        assert_eq!(normalized.palette[0], Rgba::from_index(0));
        assert_eq!(normalized.palette[255], Rgba::from_index(255));
        assert_eq!(normalized.default_foreground, Rgba::default_foreground());
        assert_eq!(normalized.default_background, Rgba::default_background());
    }

    #[test]
    fn normalize_uses_detected_hex() {
        let mut colors = TerminalColors::undetected();
        colors.palette[0] = Some("#010203".to_string());
        colors.palette[7] = Some("#abc".to_string());
        colors.default_foreground = Some("#eeeeee".to_string());
        let normalized = normalize_terminal_palette(Some(&colors));
        assert_eq!(normalized.palette[0], Rgba::rgb(1, 2, 3));
        // #abc expands to #aabbcc
        assert_eq!(normalized.palette[7], Rgba::rgb(0xAA, 0xBB, 0xCC));
        assert_eq!(normalized.default_foreground, Rgba::rgb(0xEE, 0xEE, 0xEE));
        // undetected slots keep the fallback
        assert_eq!(normalized.palette[1], Rgba::from_index(1));
    }

    #[test]
    fn from_hex_malformed_falls_back() {
        assert_eq!(rgba_from_hex("nope"), Rgba::rgb(255, 255, 255));
        assert_eq!(rgba_from_hex("#12"), Rgba::rgb(255, 255, 255));
    }
}
