//! ANSI escape emission and text-attribute packing.
//!
//! Spec: `packages/native/src/ansi.zig:240-482`. The renderer's SGR frame
//! writer (`renderer.zig:2768-2786` run logic + `emitColor` at
//! `renderer.zig:844-885`) consumes exactly these helpers; the Rust port
//! keeps them byte-identical so frames diff clean against the oracle.

use crate::color::{ColorIntent, Rgba};

/// Text attribute bits — ansi.zig:428-437. Packed in the low 8 bits of the
/// per-cell `attributes` u32; bits 8-31 carry the hyperlink id.
pub mod text_attributes {
    pub const NONE: u8 = 0;
    pub const BOLD: u8 = 1 << 0;
    pub const DIM: u8 = 1 << 1;
    pub const ITALIC: u8 = 1 << 2;
    pub const UNDERLINE: u8 = 1 << 3;
    pub const BLINK: u8 = 1 << 4;
    pub const INVERSE: u8 = 1 << 5;
    pub const HIDDEN: u8 = 1 << 6;
    pub const STRIKETHROUGH: u8 = 1 << 7;

    /// `TextAttributes` (types.ts:8-18) as u32 words — the attribute
    /// packing the text buffer stores (bits 8-31 carry the link id).
    pub const NONE_U32: u32 = 0;
    pub const BOLD_U32: u32 = 1;
    pub const DIM_U32: u32 = 1 << 1;
    pub const ITALIC_U32: u32 = 1 << 2;
    pub const UNDERLINE_U32: u32 = 1 << 3;
    pub const BLINK_U32: u32 = 1 << 4;
    pub const INVERSE_U32: u32 = 1 << 5;
    pub const HIDDEN_U32: u32 = 1 << 6;
    pub const STRIKETHROUGH_U32: u32 = 1 << 7;

    pub const ATTRIBUTE_BASE_BITS: u32 = 8;
    pub const ATTRIBUTE_BASE_MASK: u32 = 0xFF;
    pub const LINK_ID_BITS: u32 = 24;
    pub const LINK_ID_SHIFT: u32 = ATTRIBUTE_BASE_BITS;
    pub const LINK_ID_PAYLOAD_MASK: u32 = (1 << LINK_ID_BITS) - 1;
    pub const LINK_ID_MASK: u32 = LINK_ID_PAYLOAD_MASK << LINK_ID_SHIFT;
}

/// Base 8 attribute bits of a u32 attribute value — ansi.zig:450.
#[must_use]
pub fn get_base_attributes(attr: u32) -> u8 {
    (attr & text_attributes::ATTRIBUTE_BASE_MASK) as u8
}

/// Hyperlink id from bits 8-31 — ansi.zig:455.
#[must_use]
pub fn get_link_id(attr: u32) -> u32 {
    (attr & text_attributes::LINK_ID_MASK) >> text_attributes::LINK_ID_SHIFT
}

/// Set the hyperlink id preserving base attributes — ansi.zig:461.
#[must_use]
pub fn set_link_id(attr: u32, link_id: u32) -> u32 {
    let base = attr & text_attributes::ATTRIBUTE_BASE_MASK;
    let link_bits =
        (link_id & text_attributes::LINK_ID_PAYLOAD_MASK) << text_attributes::LINK_ID_SHIFT;
    base | link_bits
}

/// The attribute bit flags as a re-export-friendly wrapper (mirrors the
/// upstream `TextAttributes` struct namespace — ansi.zig:428).
pub struct TextAttributes;

impl TextAttributes {
    #[must_use]
    pub fn has_link(attr: u32) -> bool {
        get_link_id(attr) != 0
    }

    /// Emit each set attribute's SGR — ansi.zig:471-481. Order matters:
    /// bold, dim, italic, underline, blink, inverse, hidden, strikethrough.
    pub fn apply_attributes(buf: &mut String, attributes: u32) {
        use text_attributes::*;
        let base = get_base_attributes(attributes);
        if base & BOLD != 0 {
            buf.push_str("\x1b[1m");
        }
        if base & DIM != 0 {
            buf.push_str("\x1b[2m");
        }
        if base & ITALIC != 0 {
            buf.push_str("\x1b[3m");
        }
        if base & UNDERLINE != 0 {
            buf.push_str("\x1b[4m");
        }
        if base & BLINK != 0 {
            buf.push_str("\x1b[5m");
        }
        if base & INVERSE != 0 {
            buf.push_str("\x1b[7m");
        }
        if base & HIDDEN != 0 {
            buf.push_str("\x1b[8m");
        }
        if base & STRIKETHROUGH != 0 {
            buf.push_str("\x1b[9m");
        }
    }
}

/// SGR color emission — the `emitColor` decision tree at
/// renderer.zig:844-885 with a full-capability terminal (the headless
/// differential profile: truecolor + ansi256):
///
/// 1. `default` intent → `39`/`49` (terminal-configured color).
/// 2. background with alpha 0 → `49`.
/// 3. `indexed` intent → `38;5;n` / `48;5;n`.
/// 4. otherwise literal RGB → `38;2;r;g;b` / `48;2;r;g;b`.
pub fn emit_color(buf: &mut String, rgba: Rgba, is_background: bool) {
    let sgr_code = if is_background { 48 } else { 38 };
    match rgba.intent() {
        ColorIntent::Default => {
            buf.push_str(if is_background {
                "\x1b[49m"
            } else {
                "\x1b[39m"
            });
        }
        intent => {
            if is_background && rgba.alpha() == 0 {
                buf.push_str("\x1b[49m");
                return;
            }
            if intent == ColorIntent::Indexed {
                buf.push_str(&format!("\x1b[{};5;{}m", sgr_code, rgba.slot()));
                return;
            }
            buf.push_str(&format!(
                "\x1b[{};2;{};{};{}m",
                sgr_code,
                rgba.red(),
                rgba.green(),
                rgba.blue()
            ));
        }
    }
}

/// CUP (cursor position) — `moveToOutput`, 1-based — ansi.zig:253.
pub fn move_to(buf: &mut String, x: u32, y: u32) {
    buf.push_str(&format!("\x1b[{};{}H", y, x));
}

/// Terminal-mode escape sequences the renderer writes — ansi.zig:240-426
/// (subset the headless port emits; extended on demand by later lanes).
pub mod ansi_consts {
    pub const RESET: &str = "\x1b[0m";
    pub const CLEAR: &str = "\x1b[2J";
    pub const HOME: &str = "\x1b[H";
    pub const CLEAR_AND_HOME: &str = "\x1b[H\x1b[2J";
    pub const ERASE_TO_END_OF_LINE: &str = "\x1b[K";
    pub const HIDE_CURSOR: &str = "\x1b[?25l";
    pub const SHOW_CURSOR: &str = "\x1b[?25h";
    pub const NEXT_LINE: &str = "\x1b[E";
    pub const SWITCH_TO_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
    pub const SWITCH_TO_MAIN_SCREEN: &str = "\x1b[?1049l";
    pub const SYNC_SET: &str = "\x1b[?2026h";
    pub const SYNC_RESET: &str = "\x1b[?2026l";
    pub const BRACKETED_PASTE_SET: &str = "\x1b[?2004h";
    pub const BRACKETED_PASTE_RESET: &str = "\x1b[?2004l";
    pub const FOCUS_SET: &str = "\x1b[?1004h";
    pub const FOCUS_RESET: &str = "\x1b[?1004l";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgba;

    #[test]
    fn sgr_color_intents() {
        let mut s = String::new();
        emit_color(&mut s, Rgba::rgb(1, 2, 3), false);
        assert_eq!(s, "\x1b[38;2;1;2;3m");

        let mut s = String::new();
        emit_color(&mut s, Rgba::rgb(1, 2, 3), true);
        assert_eq!(s, "\x1b[48;2;1;2;3m");

        let mut s = String::new();
        emit_color(&mut s, Rgba::from_index(196), false);
        assert_eq!(s, "\x1b[38;5;196m");

        let mut s = String::new();
        emit_color(&mut s, Rgba::default_foreground(), false);
        assert_eq!(s, "\x1b[39m");

        let mut s = String::new();
        emit_color(&mut s, Rgba::default_background(), true);
        assert_eq!(s, "\x1b[49m");

        // transparent bg → 49 (renderer.zig:856-859)
        let mut s = String::new();
        emit_color(&mut s, Rgba::from_values(0.0, 0.0, 0.0, 0.0), true);
        assert_eq!(s, "\x1b[49m");
        // transparent fg stays literal RGB
        let mut s = String::new();
        emit_color(&mut s, Rgba::from_values(0.0, 0.0, 0.0, 0.0), false);
        assert_eq!(s, "\x1b[38;2;0;0;0m");
    }

    #[test]
    fn attribute_emission_order() {
        // bold|inverse → \e[1m\e[7m in that order (ansi.zig:473-479)
        let mut s = String::new();
        TextAttributes::apply_attributes(
            &mut s,
            u32::from(text_attributes::BOLD | text_attributes::INVERSE),
        );
        assert_eq!(s, "\x1b[1m\x1b[7m");
    }

    #[test]
    fn link_id_packing() {
        let attr = set_link_id(u32::from(text_attributes::UNDERLINE), 0xABCDEF);
        assert_eq!(get_base_attributes(attr), text_attributes::UNDERLINE);
        assert_eq!(get_link_id(attr), 0xABCDEF);
        assert!(TextAttributes::has_link(attr));
    }
}
