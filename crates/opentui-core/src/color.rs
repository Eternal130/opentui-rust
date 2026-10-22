//! Packed RGBA color with embedded color-intent metadata.
//!
//! Spec: `packages/native/src/ansi.zig:5-199` (native layout, the authority)
//! and `packages/core/src/lib/RGBA.ts:88-218` (the TS mirror the differential
//! harness observes). The two agree byte-for-byte on the packed form:
//!
//! * `RGBA = [4]u16` — each component stores the 8-bit channel in its low
//!   byte and one byte of a 32-bit metadata word in its high byte.
//! * metadata word: bits 0-7 = ANSI palette slot, bits 8-9 = intent
//!   (`rgb = 0 | indexed = 1 | default = 2`).
//!
//! Keeping color + intent in one 64-bit value is upstream's design: the
//! renderer compares frames with exact integer equality (`renderer.zig:2658`
//! row-diff uses `std.mem.eql(RGBA, ...)`), and palette intent crosses the
//! TS/native boundary without a sidecar field.

/// Records the original color intent so the renderer emits the right ANSI
/// sequence: literal RGB (`38;2;r;g;b`), indexed palette (`38;5;n`), or
/// terminal default (`39`/`49`). — ansi.zig:26-30, RGBA.ts:5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorIntent {
    Rgb,
    Indexed,
    Default,
}

/// Packed color; see module docs. Invariant: the four u16 words are exactly
/// what upstream would store in the cell buffers, so `Rgba` comparisons are
/// byte-exact against the oracle dumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba(pub [u16; 4]);

/// Standard ANSI 16-color palette RGB values (colors 0-15). — ansi.zig:38-55.
pub const ANSI16_RGB: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x80, 0x00, 0x00],
    [0x00, 0x80, 0x00],
    [0x80, 0x80, 0x00],
    [0x00, 0x00, 0x80],
    [0x80, 0x00, 0x80],
    [0x00, 0x80, 0x80],
    [0xc0, 0xc0, 0xc0],
    [0x80, 0x80, 0x80],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x00, 0x00, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

/// The six intensity levels of the ANSI 256-color cube (indices 16-231). —
/// ansi.zig:58.
pub const ANSI_256_CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// `DEFAULT_FOREGROUND_RGB` — RGBA.ts:7.
pub const DEFAULT_FOREGROUND_RGB: [u8; 3] = [255, 255, 255];
/// `DEFAULT_BACKGROUND_RGB` — RGBA.ts:8.
pub const DEFAULT_BACKGROUND_RGB: [u8; 3] = [0, 0, 0];

/// Quantize a 0.0-1.0 float channel to a 0-255 byte.
///
/// TS uses `Math.round(clamp(x))` (RGBA.ts:70 `toU8`); Zig's x86 `roundByte`
/// (ansi.zig:73-78) reproduces `@round` = half-away-from-zero. `f32::round`
/// in Rust is the same half-away-from-zero, so this matches both.
fn component_to_u8(component: f32) -> u8 {
    if !component.is_finite() {
        return 0;
    }
    let clamped = component.clamp(0.0, 1.0);
    (clamped * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Pack intent + slot into the metadata word (bits 0-7 slot, 8-9 intent). —
/// ansi.zig:102-104, RGBA.ts:60.
fn pack_meta(intent: ColorIntent, slot: u8) -> u32 {
    let intent_bits = match intent {
        ColorIntent::Rgb => 0u32,
        ColorIntent::Indexed => 1,
        ColorIntent::Default => 2,
    };
    (u32::from(slot) & 0xff) | ((intent_bits & 0xff) << 8)
}

fn pack_rgba8(r: u8, g: u8, b: u8, a: u8, meta: u32) -> Rgba {
    Rgba([
        u16::from(r) | ((meta as u16 & 0xff) << 8),
        u16::from(g) | (((meta >> 8) as u16 & 0xff) << 8),
        u16::from(b) | (((meta >> 16) as u16 & 0xff) << 8),
        u16::from(a) | (((meta >> 24) as u16 & 0xff) << 8),
    ])
}

impl Rgba {
    /// `RGBA.fromInts` — literal RGB intent. — RGBA.ts:110.
    pub fn from_ints(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        pack_rgba8(r, g, b, a, pack_meta(ColorIntent::Rgb, 0))
    }

    /// `RGBA.fromValues` (floats 0.0-1.0) — literal RGB intent. — RGBA.ts:102.
    pub fn from_values(r: f32, g: f32, b: f32, a: f32) -> Rgba {
        Rgba::from_ints(
            component_to_u8(r),
            component_to_u8(g),
            component_to_u8(b),
            component_to_u8(a),
        )
    }

    /// Opaque RGB. — ansi.zig:202 `u8RgbToRgba`.
    pub fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba::from_ints(r, g, b, 255)
    }

    /// `rgbaFromFloats`. — ansi.zig:184.
    pub fn rgba_from_floats(r: f32, g: f32, b: f32, a: f32) -> Rgba {
        Rgba::from_values(r, g, b, a)
    }

    /// `ansi.defaultColor` — ansi.zig:197: terminal-default intent with
    /// an RGB snapshot (the embedded terminal's transparent background,
    /// compositor.zig:19-21).
    pub fn default_color(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        pack_rgba8(r, g, b, a, pack_meta(ColorIntent::Default, 0))
    }

    /// Indexed ANSI color with an RGB snapshot for blending fallbacks. —
    /// ansi.zig:191, RGBA.ts:118 `fromIndex`.
    pub fn from_index(index: u8) -> Rgba {
        let [r, g, b] = ansi256_index_to_rgb(index);
        pack_rgba8(r, g, b, 255, pack_meta(ColorIntent::Indexed, index))
    }

    /// `RGBA.defaultForeground` — default intent, white snapshot. — RGBA.ts:132.
    pub fn default_foreground() -> Rgba {
        pack_rgba8(
            DEFAULT_FOREGROUND_RGB[0],
            DEFAULT_FOREGROUND_RGB[1],
            DEFAULT_FOREGROUND_RGB[2],
            255,
            pack_meta(ColorIntent::Default, 0),
        )
    }

    /// `RGBA.defaultBackground` — default intent, black snapshot. — RGBA.ts:140.
    pub fn default_background() -> Rgba {
        pack_rgba8(
            DEFAULT_BACKGROUND_RGB[0],
            DEFAULT_BACKGROUND_RGB[1],
            DEFAULT_BACKGROUND_RGB[2],
            255,
            pack_meta(ColorIntent::Default, 0),
        )
    }

    /// `RGBA.fromArray` — raw packed words (differential-harness boundary).
    pub fn from_packed(words: [u16; 4]) -> Rgba {
        Rgba(words)
    }

    pub fn to_ints(self) -> [u8; 4] {
        [
            self.0[0] as u8,
            self.0[1] as u8,
            self.0[2] as u8,
            self.0[3] as u8,
        ]
    }

    pub fn red(self) -> u8 {
        (self.0[0] & 0xff) as u8
    }
    pub fn green(self) -> u8 {
        (self.0[1] & 0xff) as u8
    }
    pub fn blue(self) -> u8 {
        (self.0[2] & 0xff) as u8
    }
    pub fn alpha(self) -> u8 {
        (self.0[3] & 0xff) as u8
    }

    /// Reassemble the metadata word from the high bytes. — ansi.zig:151-156,
    /// RGBA.ts:159 `meta` getter.
    pub fn meta(self) -> u32 {
        (u32::from(self.0[0] >> 8))
            | (u32::from(self.0[1] >> 8) << 8)
            | (u32::from(self.0[2] >> 8) << 16)
            | (u32::from(self.0[3] >> 8) << 24)
    }

    /// The color intent. — ansi.zig:159-165, RGBA.ts:177 `intent` getter.
    pub fn intent(self) -> ColorIntent {
        match (self.meta() >> 8) & 0xff {
            1 => ColorIntent::Indexed,
            2 => ColorIntent::Default,
            _ => ColorIntent::Rgb,
        }
    }

    /// ANSI palette slot; meaningful only for [`ColorIntent::Indexed`]. —
    /// ansi.zig:168.
    pub fn slot(self) -> u8 {
        self.meta() as u8
    }

    /// Channel-wise float view (r/g/b/a as 0.0-1.0). — RGBA.ts:128 getters.
    pub fn floats(self) -> [f32; 4] {
        let [r, g, b, a] = self.to_ints();
        [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            f32::from(a) / 255.0,
        ]
    }

    /// `rgbToHex`. — RGBA.ts:241.
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.to_ints();
        if a == 255 {
            format!("#{:02x}{:02x}{:02x}", r, g, b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a)
        }
    }

    /// Alpha-blend `self` over `bg` (used by cell drawing with alpha). —
    /// mirrors `buffer.zig` `rgbaEqual`/blend sites; kept exact-integer.
    pub fn blend_over(self, bg: Rgba) -> Rgba {
        let a = f32::from(self.alpha()) / 255.0;
        if a >= 1.0 {
            return self;
        }
        if a <= 0.0 {
            return bg;
        }
        let s = self.floats();
        let b = bg.floats();
        // Result keeps RGB intent: blending never preserves palette slots.
        Rgba::from_values(
            s[0] * a + b[0] * (1.0 - a),
            s[1] * a + b[1] * (1.0 - a),
            s[2] * a + b[2] * (1.0 - a),
            1.0,
        )
    }
}

/// Convert an ANSI 256-color index to RGB: base 16 + 6×6×6 cube + grayscale
/// ramp. — ansi.zig:208-223, RGBA.ts:74-88.
pub fn ansi256_index_to_rgb(index: u8) -> [u8; 3] {
    let index = index as usize;
    if index < ANSI16_RGB.len() {
        return ANSI16_RGB[index];
    }
    if index < 232 {
        let cube = index - 16;
        return [
            ANSI_256_CUBE_LEVELS[(cube / 36) % 6],
            ANSI_256_CUBE_LEVELS[(cube / 6) % 6],
            ANSI_256_CUBE_LEVELS[cube % 6],
        ];
    }
    let gray = (8 + (index - 232) * 10) as u8;
    [gray, gray, gray]
}

/// CSS color-name lookup — RGBA.ts:283-311 (exact values).
fn css_name_hex(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "black" => "#000000",
        "white" => "#FFFFFF",
        "red" => "#FF0000",
        "green" => "#008000",
        "blue" => "#0000FF",
        "yellow" => "#FFFF00",
        "cyan" => "#00FFFF",
        "magenta" => "#FF00FF",
        "silver" => "#C0C0C0",
        "gray" | "grey" => "#808080",
        "maroon" => "#800000",
        "olive" => "#808000",
        "lime" => "#00FF00",
        "aqua" => "#00FFFF",
        "teal" => "#008080",
        "navy" => "#000080",
        "fuchsia" => "#FF00FF",
        "purple" => "#800080",
        "orange" => "#FFA500",
        "brightblack" => "#666666",
        "brightred" => "#FF6666",
        "brightgreen" => "#66FF66",
        "brightblue" => "#6666FF",
        "brightyellow" => "#FFFF66",
        "brightcyan" => "#66FFFF",
        "brightmagenta" => "#FF66FF",
        "brightwhite" => "#FFFFFF",
        _ => return None,
    })
}

/// `hexToRgb` — RGBA.ts:225-244. Accepts 3/4/6/8 hex digits; invalid input
/// warns and falls back to magenta (upstream semantics, kept).
pub fn hex_to_rgb(hex: &str) -> Rgba {
    let hex = hex.trim_start_matches('#');
    let expanded: String;
    let hex = if hex.len() == 3 || hex.len() == 4 {
        expanded = hex.chars().flat_map(|c| [c, c]).collect();
        expanded.as_str()
    } else {
        hex
    };
    let valid = (hex.len() == 6 || hex.len() == 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        // upstream: console.warn + magenta fallback (RGBA.ts:231-233)
        return Rgba::from_values(1.0, 0.0, 1.0, 1.0);
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(255);
    let a = if hex.len() == 8 { byte(6) } else { 255 };
    Rgba::from_ints(byte(0), byte(2), byte(4), a)
}

/// `parseColor` — RGBA.ts:326-340: "transparent" → transparent black,
/// CSS names, then hex.
pub fn parse_color(color: &str) -> Rgba {
    let lower = color.to_ascii_lowercase();
    if lower == "transparent" {
        return Rgba::from_values(0.0, 0.0, 0.0, 0.0);
    }
    if let Some(hex) = css_name_hex(&lower) {
        return hex_to_rgb(hex);
    }
    hex_to_rgb(color)
}

/// A `#RRGGBB` spelling of a packed color (theme border colors re-enter
/// the option parsers this way upstream — `getBlockquoteBorderColor`
/// feeds `borderColor` which re-parses; Markdown.ts:829-831).
#[must_use]
pub fn rgb_to_hex_string(color: Rgba) -> String {
    fn channel(word: u16) -> u8 {
        (word & 0xFF) as u8
    }
    let [r, g, b, _a] = color.0;
    format!("#{:02X}{:02X}{:02X}", channel(r), channel(g), channel(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_layout_matches_ansi_zig() {
        // packMeta(.rgb,0)=0 → meta bytes 00 00 00 00 in the high bytes
        // (ansi.zig:109-116: meta byte0 rides in R's high byte, ...).
        let c = Rgba::from_ints(0x12, 0x34, 0x56, 0x78);
        assert_eq!(c.0, [0x0012, 0x0034, 0x0056, 0x0078]);
        assert_eq!(c.to_ints(), [0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn intent_round_trip() {
        let idx = Rgba::from_index(196);
        assert_eq!(idx.intent(), ColorIntent::Indexed);
        assert_eq!(idx.slot(), 196);
        let [r, g, b, _] = idx.to_ints();
        assert_eq!([r, g, b], ansi256_index_to_rgb(196));

        assert_eq!(Rgba::default_foreground().intent(), ColorIntent::Default);
        assert_eq!(Rgba::rgb(1, 2, 3).intent(), ColorIntent::Rgb);
    }

    #[test]
    fn ansi256_ramp() {
        assert_eq!(ansi256_index_to_rgb(0), [0x00, 0x00, 0x00]);
        assert_eq!(ansi256_index_to_rgb(16), [0, 0, 0]);
        assert_eq!(ansi256_index_to_rgb(21), [0, 0, 255]);
        assert_eq!(ansi256_index_to_rgb(232), [8, 8, 8]);
        assert_eq!(ansi256_index_to_rgb(255), [238, 238, 238]);
    }

    #[test]
    fn parse_color_paths() {
        assert_eq!(
            parse_color("transparent"),
            Rgba::from_values(0.0, 0.0, 0.0, 0.0)
        );
        assert_eq!(parse_color("#FFFFFF"), Rgba::rgb(255, 255, 255));
        assert_eq!(parse_color("#fff"), Rgba::rgb(255, 255, 255));
        assert_eq!(parse_color("red"), Rgba::rgb(255, 0, 0));
        // upstream falls back to magenta on invalid hex
        assert_eq!(
            parse_color("#zzzzzz"),
            Rgba::from_values(1.0, 0.0, 1.0, 1.0)
        );
    }

    #[test]
    fn float_quantization_is_js_math_round() {
        // 0.5*255 = 127.5 → Math.round → 128 (half away from zero).
        assert_eq!(component_to_u8(0.5), 128);
        assert_eq!(component_to_u8(-1.0), 0);
        assert_eq!(component_to_u8(2.0), 255);
        assert_eq!(component_to_u8(f32::NAN), 0);
    }
}
