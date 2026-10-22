//! `TimeToFirstDrawRenderable` — port of
//! `packages/core/src/renderables/TimeToFirstDraw.ts`.
//!
//! The renderable stamps `performance.now()` on its FIRST paint
//! (:98-100) and renders `` `${label}: ${ms.toFixed(precision)}ms` ``
//! centered, truncated to complete graphemes (:102-107). Upstream
//! draws at `this.x`/`this.y` — the PARENT-RELATIVE layout position,
//! not `_screenX` (a quirk of the 0.5.14 source; correct at root
//! level, which is how opencode uses it). The port reproduces this
//! bug-for-bug: [`TimeToFirstDrawState::paint`] receives the relative
//! x/y and treats them as buffer coordinates.
//!
//! The clock: upstream reads `performance.now()` inside `renderSelf`;
//! the port stamps in the `&mut` prepare pass
//! (`Renderable::prepare_paint`) with the renderer's
//! [`crate::renderer::CliRenderer::performance_now`] (a fixed override
//! is the test seam for upstream's `performance.now` monkeypatch,
//! TimeToFirstDraw.test.ts:19-20).

use crate::buffer::OptimizedBuffer;
use crate::color::{parse_color, Rgba};
use crate::layout::{Dimension, LayoutOptions};
use crate::text_buffer::cluster_width;

/// `measureCellWidth` (TimeToFirstDraw.ts:8-17) — the cluster's cell
/// width via the buffer's `encodeUnicode` glyph width: the native side
/// runs `findRenderClusterInfo` (lib.zig:3875), the port of which is
/// [`crate::text_buffer::cluster_width`] (utf8.zig:1892-2103 — the
/// grapheme-aware `.unicode` width rules: Devanagari virama conjuncts
/// add, ZWJ families keep the leading width). One glyph per grapheme,
/// so the sum over `encoded.data` (:13) is the single cluster width.
#[must_use]
pub fn measure_cell_width(text: &str) -> u32 {
    // `if (!encoded) return 0` (:10) — encodeUnicode fails only on the
    // native pool; a Rust &str always encodes.
    cluster_width(text)
}

/// `truncateToCellWidth` (TimeToFirstDraw.ts:19-32): greedy grapheme
/// fill — stop before the first grapheme whose inclusion would exceed
/// `max_width`. Returns `(visible_text, visible_width)`.
#[must_use]
pub fn truncate_to_cell_width(text: &str, max_width: u32) -> (String, u32) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut visible_text = String::new();
    let mut visible_width = 0u32;
    for segment in text.graphemes(true) {
        let segment_width = measure_cell_width(segment);
        if visible_width + segment_width > max_width {
            break;
        }
        visible_text.push_str(segment);
        visible_width += segment_width;
    }
    (visible_text, visible_width)
}

/// `Number.prototype.toFixed` (ECMA-262 §22.1.3.4) — exact rounding of
/// the f64's true decimal expansion, ties to the LARGER n (spec step
/// 8), `ToString(x)` for |x| ≥ 10^21 (step 4). The rendered `ms`
/// string is observable, so the port implements the full algorithm
/// instead of Rust's `{:.p}` (ties-to-even — `(0.125).toFixed(2)` is
/// `"0.13"` in JS, `"0.12"` with Rust's formatter).
///
/// # Panics
///
/// `precision > 100` — upstream `toFixed` throws `RangeError`
/// (`toFixed() digits argument must be between 0 and 100`), which
/// crashes the render; the port panics with the same message.
#[must_use]
pub fn js_to_fixed(x: f64, precision: u32) -> String {
    if precision > 100 {
        panic!("toFixed() digits argument must be between 0 and 100");
    }
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    // step 3: `If x < 0, s = "-", x = -x` — (-0) is not < 0, so
    // (-0).toFixed(2) === "0.00" (no sign)
    let sign = if x < 0.0 { "-" } else { "" };
    let a = x.abs();
    if a >= 1e21 {
        // step 4: m = ToString(x) — exponential notation ≥ 10^21
        return format!("{sign}{}", js_to_string_exponential(a));
    }
    // The exact decimal expansion of a = M · 2^-k terminates after k
    // fractional digits; formatting with q ≥ k digits is exact
    // (Rust's Display-with-precision is the exact flt2dec mode).
    let k = exact_fraction_digits(a);
    let q = (precision as usize).max(k);
    let s = format!("{a:.q$}");
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s.as_str(), ""));
    debug_assert_eq!(frac_part.len(), q);

    let p = precision as usize;
    if p >= frac_part.len() {
        // within the exact expansion — pad out to p digits
        let mut frac = frac_part.to_owned();
        frac.extend(std::iter::repeat_n('0', p - frac.len()));
        return format_fraction(sign, int_part, &frac, p);
    }
    // round at p: digit >= '5' rounds up (ties pick the larger n —
    // ECMA-262 toFixed step 8), digit < '5' truncates
    let up = frac_part.as_bytes()[p] >= b'5';
    let mut digits: Vec<u8> = Vec::with_capacity(int_part.len() + p + 1);
    digits.extend_from_slice(int_part.as_bytes());
    digits.extend_from_slice(&frac_part.as_bytes()[..p]);
    if up {
        increment_digits(&mut digits);
    }
    let digits = String::from_utf8(digits).expect("ascii digits");
    format_fraction(sign, &digits, "", p)
}

/// `ToString(x)` for x ≥ 1e21 — the shortest round-trip digits with a
/// JS `e+NN` exponent tail (String(1e21) === "1e+21").
fn js_to_string_exponential(a: f64) -> String {
    let rust_e = format!("{a:e}"); // "1.5e21" (shortest digits)
    match rust_e.split_once('e') {
        Some((mantissa, exp)) => {
            let exp: i32 = exp.parse().expect("exponent");
            if exp < 0 {
                format!("{mantissa}e{exp}")
            } else {
                format!("{mantissa}e+{exp}")
            }
        }
        None => rust_e,
    }
}

/// Number of fractional digits in `a`'s exact decimal expansion
/// (`a = M · 2^-k` terminates after k digits; integers → 0).
fn exact_fraction_digits(a: f64) -> usize {
    if a == 0.0 {
        return 0;
    }
    let bits = a.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let (mantissa, unbiased) = if raw_exp == 0 {
        (bits & 0xf_ffff_ffff_ffff, -1074i32)
    } else {
        (
            bits & 0xf_ffff_ffff_ffff | 0x10_0000_0000_0000,
            raw_exp - 1075,
        )
    };
    let _ = mantissa;
    if unbiased >= 0 {
        0
    } else {
        (-unbiased) as usize
    }
}

/// Decimal increment of an ASCII digit string ("099" → "100").
fn increment_digits(digits: &mut Vec<u8>) {
    let mut i = digits.len();
    loop {
        if i == 0 {
            digits.insert(0, b'1');
            return;
        }
        i -= 1;
        if digits[i] == b'9' {
            digits[i] = b'0';
        } else {
            digits[i] += 1;
            return;
        }
    }
}

/// Place the decimal point `p` digits from the right (toFixed step 9):
/// `format_fraction("", "10", "", 1)` → `"1.0"`.
fn format_fraction(sign: &str, digits: &str, tail: &str, p: usize) -> String {
    let mut all = String::from(digits);
    all.push_str(tail);
    if p == 0 {
        return format!("{sign}{all}");
    }
    while all.len() <= p {
        all.insert(0, '0');
    }
    let split = all.len() - p;
    format!("{sign}{}.{}", &all[..split], &all[split..])
}

/// `TimeToFirstDrawOptions` (TimeToFirstDraw.ts:34-38) over
/// `RenderableOptions` (width/height + yoga props ride
/// [`LayoutOptions`]).
#[derive(Debug, Clone, Default)]
pub struct TimeToFirstDrawOptions {
    pub layout: LayoutOptions,
    pub width: Option<Dimension>,
    pub height: Option<Dimension>,
    /// `fg` — pre-parsed `ColorInput` (default `#AAAAAA`, :55).
    pub fg: Option<Rgba>,
    /// `label` (default `"Time to first draw"`, :56).
    pub label: Option<String>,
    /// `precision` (default 2, normalized, :57).
    pub precision: Option<f64>,
}

impl TimeToFirstDrawOptions {
    /// The constructor's option merge (:47-53):
    /// `{ width: "100%", height: 1, flexShrink: 0, alignSelf: "center", ...options }`.
    #[must_use]
    pub fn resolved(&self) -> (LayoutOptions, Dimension, Dimension) {
        let mut layout = self.layout.clone();
        if layout.flex_shrink.is_none() {
            layout.flex_shrink = Some(0.0);
        }
        if layout.align_self.is_none() {
            layout.align_self = Some("center".into());
        }
        (
            layout,
            self.width.unwrap_or(Dimension::Percent(1.0)),
            self.height.unwrap_or(Dimension::Points(1.0)),
        )
    }
}

/// The renderable state — `TimeToFirstDrawRenderable`
/// (TimeToFirstDraw.ts:40-117).
#[derive(Debug, Clone)]
pub struct TimeToFirstDrawState {
    /// `_runtimeMs` (:41) — `None` until the first renderSelf.
    runtime_ms: Option<f64>,
    /// `textColor` (:42).
    pub text_color: Rgba,
    /// `label` (:43).
    pub label: String,
    /// `precision` (:44) — normalized by [`Self::normalize_precision`].
    pub precision: u32,
}

impl TimeToFirstDrawState {
    /// Constructor (:46-58).
    #[must_use]
    pub fn from_options(opts: &TimeToFirstDrawOptions) -> Self {
        TimeToFirstDrawState {
            runtime_ms: None,
            text_color: opts.fg.unwrap_or_else(|| parse_color("#AAAAAA")),
            label: opts
                .label
                .clone()
                .unwrap_or_else(|| "Time to first draw".to_owned()),
            precision: Self::normalize_precision(opts.precision.unwrap_or(2.0)),
        }
    }

    /// `get runtimeMs()` (:60-62).
    #[must_use]
    pub fn runtime_ms(&self) -> Option<f64> {
        self.runtime_ms
    }

    /// `set fg` / `set color` (:64-71): parse + `requestRender`.
    /// Returns true when a re-render is needed.
    pub fn set_fg(&mut self, value: Rgba) -> bool {
        self.text_color = value;
        true
    }

    /// `set textLabel` (:73-80): no-op when equal.
    pub fn set_text_label(&mut self, value: &str) -> bool {
        if value == self.label {
            return false;
        }
        self.label = value.to_owned();
        true
    }

    /// `set decimals` (:82-90): normalize, no-op when equal.
    pub fn set_decimals(&mut self, value: f64) -> bool {
        let next_precision = Self::normalize_precision(value);
        if next_precision == self.precision {
            return false;
        }
        self.precision = next_precision;
        true
    }

    /// `reset()` (:92-95).
    pub fn reset(&mut self) -> bool {
        self.runtime_ms = None;
        true
    }

    /// `normalizePrecision` (:110-116): non-finite → 2; else
    /// `max(0, floor(value))`.
    fn normalize_precision(value: f64) -> u32 {
        if !value.is_finite() {
            return 2;
        }
        value.floor().max(0.0) as u32
    }

    /// The first-paint stamp (:98-100): `if (_runtimeMs === null)
    /// _runtimeMs = performance.now()`.
    pub fn stamp(&mut self, now_ms: f64) {
        if self.runtime_ms.is_none() {
            self.runtime_ms = Some(now_ms);
        }
    }

    /// `renderSelf` (:97-108). `x`/`y` are the PARENT-RELATIVE layout
    /// position (upstream draws at `this.x`/`this.y` — see the module
    /// doc); `width` is `this.width` (already clamped ≥ 1 upstream).
    pub fn paint(&self, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: f32) {
        let runtime = self.runtime_ms.unwrap_or(0.0);
        let content = format!("{}: {}ms", self.label, js_to_fixed(runtime, self.precision));
        let max_width = (width.max(1.0)) as u32; // Math.max(this.width, 1) (:103)
        let (visible_text, visible_width) = truncate_to_cell_width(&content, max_width);
        let centered_x = x + (((max_width - visible_width) / 2) as i32).max(0); // :105
        if centered_x < 0 || y < 0 {
            return;
        }
        buffer.draw_text(
            &visible_text,
            centered_x as u32,
            y as u32,
            self.text_color,
            None,
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::create_cli_renderer;

    // --- js_to_fixed: ECMA-262 §22.1.3.4 (verified against node 22) ---------

    #[test]
    fn to_fixed_basics() {
        assert_eq!(js_to_fixed(0.0, 0), "0");
        assert_eq!(js_to_fixed(0.0, 2), "0.00");
        assert_eq!(js_to_fixed(123.456, 2), "123.46");
        assert_eq!(js_to_fixed(123.4, 5), "123.40000");
        assert_eq!(js_to_fixed(0.0000001, 7), "0.0000001");
    }

    #[test]
    fn to_fixed_exact_binary_rounds_down() {
        // the famous "1.005 is really 1.00499…" family — NOT ties
        assert_eq!(js_to_fixed(1.005, 2), "1.00");
        assert_eq!(js_to_fixed(2.675, 2), "2.67");
        assert_eq!(js_to_fixed(8.0055, 3), "8.005");
    }

    #[test]
    fn to_fixed_ties_pick_larger() {
        // exact .5 remainders round AWAY from zero (spec step 8);
        // Rust's {:.p} (ties-to-even) would give 0.12 / 2
        assert_eq!(js_to_fixed(0.125, 2), "0.13");
        assert_eq!(js_to_fixed(0.5, 0), "1");
        assert_eq!(js_to_fixed(1.5, 0), "2");
        assert_eq!(js_to_fixed(2.5, 0), "3");
        assert_eq!(js_to_fixed(-2.5, 0), "-3");
    }

    #[test]
    fn to_fixed_sign_and_zero() {
        // sign rides the ORIGINAL x even when the rounded value is 0
        assert_eq!(js_to_fixed(-0.004, 2), "-0.00");
        assert_eq!(js_to_fixed(-0.006, 2), "-0.01");
        // -0 is not < 0 → "0.00"
        assert_eq!(js_to_fixed(-0.0, 2), "0.00");
    }

    #[test]
    fn to_fixed_large_uses_to_string() {
        // |x| ≥ 10^21 → ToString(x) (exponential)
        assert_eq!(js_to_fixed(1e21, 2), "1e+21");
        assert_eq!(js_to_fixed(1e21, 0), "1e+21");
        assert_eq!(js_to_fixed(1.5e21, 0), "1.5e+21");
        // just under the boundary formats normally
        assert_eq!(js_to_fixed(1e20, 2), "100000000000000000000.00");
    }

    #[test]
    fn to_fixed_non_finite() {
        assert_eq!(js_to_fixed(f64::NAN, 2), "NaN");
        assert_eq!(js_to_fixed(f64::INFINITY, 2), "Infinity");
        assert_eq!(js_to_fixed(f64::NEG_INFINITY, 2), "-Infinity");
    }

    #[test]
    #[should_panic(expected = "toFixed() digits argument must be between 0 and 100")]
    fn to_fixed_precision_over_100_throws() {
        let _ = js_to_fixed(1.0, 101);
    }

    // --- truncation (TimeToFirstDraw.ts:19-32) ------------------------------

    #[test]
    fn truncation_stops_at_complete_grapheme() {
        // 😀 is 2 cells; a width-2 budget keeps exactly one
        let (text, width) = truncate_to_cell_width("😀😀", 2);
        assert_eq!((text.as_str(), width), ("😀", 2));
        // the ZWJ family is ONE grapheme of width 2 → fits; X dropped
        let (text, width) = truncate_to_cell_width("👨‍👩‍👧‍👦X", 2);
        assert_eq!((text.as_str(), width), ("👨‍👩‍👧‍👦", 2));
        // zero budget drops everything
        let (text, width) = truncate_to_cell_width("abc", 0);
        assert_eq!((text.as_str(), width), ("", 0));
    }

    // --- state (TimeToFirstDraw.ts:40-116) ----------------------------------

    #[test]
    fn defaults_match_constructor() {
        let state = TimeToFirstDrawState::from_options(&TimeToFirstDrawOptions::default());
        assert_eq!(state.text_color, parse_color("#AAAAAA"));
        assert_eq!(state.label, "Time to first draw");
        assert_eq!(state.precision, 2);
        assert_eq!(state.runtime_ms(), None);
    }

    #[test]
    fn setters_skip_equal_and_normalize_precision() {
        let mut state = TimeToFirstDrawState::from_options(&TimeToFirstDrawOptions::default());
        assert!(!state.set_text_label("Time to first draw"));
        assert!(state.set_text_label("boot"));
        assert!(!state.set_decimals(2.0));
        assert!(state.set_decimals(3.9)); // floor → 3
        assert_eq!(state.precision, 3);
        assert!(state.set_decimals(-4.0)); // clamp → 0
        assert_eq!(state.precision, 0);
        assert!(state.set_decimals(f64::NAN)); // non-finite → 2
        assert_eq!(state.precision, 2);
    }

    #[test]
    fn stamp_happens_once_and_reset_rearms() {
        let mut state = TimeToFirstDrawState::from_options(&TimeToFirstDrawOptions::default());
        state.stamp(12.5);
        state.stamp(99.0); // second render keeps the first draw
        assert_eq!(state.runtime_ms(), Some(12.5));
        state.reset();
        assert_eq!(state.runtime_ms(), None);
        state.stamp(20.0);
        assert_eq!(state.runtime_ms(), Some(20.0));
    }

    // --- paint (TimeToFirstDraw.ts:97-108) — the upstream test harness ------

    fn renderer_frame(opts: &TimeToFirstDrawOptions, now: f64) -> String {
        let mut renderer = create_cli_renderer(crate::renderer::CliRendererConfig {
            width: 10,
            height: 1,
            ..Default::default()
        });
        renderer.set_performance_now(Some(now));
        let node = renderer.make_time_to_first_draw("ttfd", opts);
        renderer.add(node);
        renderer.render();
        renderer.current_render_buffer.char_frame()
    }

    /// TimeToFirstDraw.test.ts:39-50 — truncation at a complete
    /// grapheme (frame CONTAINS the included cluster, NOT the excluded
    /// half/replacement char).
    #[test]
    fn truncates_at_complete_grapheme_in_frame() {
        for (label, included, excluded) in [("😀😀", "😀", "\u{FFFD}"), ("👨‍👩‍👧‍👦X", "👨‍👩‍👧‍👦", "X")]
        {
            let frame = renderer_frame(
                &TimeToFirstDrawOptions {
                    width: Some(Dimension::Points(2.0)),
                    label: Some(label.to_owned()),
                    precision: Some(0.0),
                    ..Default::default()
                },
                0.0,
            );
            assert!(frame.contains(included), "{label}: {frame:?}");
            assert!(!frame.contains(excluded), "{label}: {frame:?}");
        }
    }

    /// TimeToFirstDraw.test.ts:52-54 — "centers %s with
    /// native-compatible width": `${label}: 0ms` starts the row.
    #[test]
    fn centers_with_native_compatible_width() {
        for label in ["🫩", "क्त"] {
            let frame = renderer_frame(
                &TimeToFirstDrawOptions {
                    width: Some(Dimension::Points(8.0)),
                    layout: LayoutOptions {
                        align_self: Some("flex-start".into()),
                        ..Default::default()
                    },
                    label: Some(label.to_owned()),
                    precision: Some(0.0),
                    ..Default::default()
                },
                0.0,
            );
            let row = &frame.lines().next().unwrap();
            assert!(
                row.starts_with(&format!("{label}: 0ms")),
                "{label}: {row:?}"
            );
        }
    }

    /// The label + runtime format with a fractional clock (:102) and
    /// the first-draw stamp surviving re-renders (:98-100).
    #[test]
    fn renders_label_with_precision_and_keeps_first_stamp() {
        let opts = TimeToFirstDrawOptions {
            label: Some("boot".into()),
            precision: Some(1.0),
            ..Default::default()
        };
        let mut renderer = create_cli_renderer(crate::renderer::CliRendererConfig {
            width: 24,
            height: 1,
            ..Default::default()
        });
        renderer.set_performance_now(Some(12.34));
        let node = renderer.make_time_to_first_draw("ttfd", &opts);
        renderer.add(node);
        renderer.render();
        assert!(renderer
            .current_render_buffer
            .char_frame()
            .contains("boot: 12.3ms"));
        // second frame at a later clock: still the FIRST draw's value
        renderer.set_performance_now(Some(999.0));
        renderer.render();
        assert!(renderer
            .current_render_buffer
            .char_frame()
            .contains("boot: 12.3ms"));
    }
}
