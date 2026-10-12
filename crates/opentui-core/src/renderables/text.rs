//! `TextRenderable` — `packages/core/src/renderables/Text.ts` over
//! `TextBufferRenderable` (`TextBufferRenderable.ts`).
//!
//! Defaults (TextBufferRenderable.ts:41-54): fg white `fromValues(1,1,1,1)`
//! (literal RGB intent), bg `fromValues(0,0,0,0)` (transparent black),
//! attributes 0, wrapMode `"word"`, textAlign left.
//!
//! The slice models one styled chunk (the plain-string `content` path);
//! multi-chunk `StyledText` spans are a tier-2 port target.

use crate::buffer::OptimizedBuffer;
use crate::color::Rgba;
use crate::layout::Dimension;
use unicode_segmentation::UnicodeSegmentation;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    None,
    Char,
    #[default]
    Word,
}

impl WrapMode {
    pub fn from_str(s: &str) -> WrapMode {
        match s {
            "none" => WrapMode::None,
            "char" => WrapMode::Char,
            _ => WrapMode::Word,
        }
    }
}

/// `TextOptions` — Text.ts:9-11 + TextBufferOptions.
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
    pub layout: crate::layout::LayoutOptions,
    pub width: Option<Dimension>,
    pub height: Option<Dimension>,
    pub content: String,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
    pub wrap_mode: Option<WrapMode>,
    pub text_align: Option<String>,
}

fn grapheme_width(g: &str) -> u32 {
    g.chars()
        .map(|c| c.width().map(|w| w as u32).unwrap_or(0))
        .max()
        .unwrap_or(1)
        .max(1)
}

fn display_width(s: &str) -> u32 {
    s.graphemes(true).map(grapheme_width).sum()
}

/// Word wrap (greedy, whitespace-split) — the `wrapMode: "word"` default.
/// "char" wraps per grapheme; "none" only splits on `\n`.
pub fn wrap_text(text: &str, mode: WrapMode, width: u32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        if width == 0 || mode == WrapMode::None {
            lines.push(para.to_owned());
            continue;
        }
        match mode {
            WrapMode::Char => {
                let mut line = String::new();
                let mut w = 0;
                for g in para.graphemes(true) {
                    let gw = grapheme_width(g);
                    if w + gw > width && w > 0 {
                        lines.push(core::mem::take(&mut line));
                        w = 0;
                    }
                    line.push_str(g);
                    w += gw;
                }
                lines.push(line);
            }
            _ => {
                let mut line = String::new();
                let mut w = 0;
                for word in para.split_whitespace() {
                    let ww = display_width(word);
                    // words longer than the width hard-break
                    if ww > width {
                        if w > 0 {
                            lines.push(core::mem::take(&mut line));
                        }
                        let mut piece = String::new();
                        let mut pw = 0;
                        for g in word.graphemes(true) {
                            let gw = grapheme_width(g);
                            if pw + gw > width && pw > 0 {
                                lines.push(core::mem::take(&mut piece));
                                pw = 0;
                            }
                            piece.push_str(g);
                            pw += gw;
                        }
                        if !piece.is_empty() {
                            line = piece;
                            w = pw;
                        }
                        continue;
                    }
                    let sep = if line.is_empty() { 0 } else { 1 };
                    if w + sep + ww > width {
                        lines.push(core::mem::take(&mut line));
                    }
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(word);
                    w = display_width(&line);
                }
                lines.push(line);
            }
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The measured text the taffy leaf measures from (the native measure
/// target — TextBufferRenderable.ts:402-431).
#[derive(Debug, Clone, Default)]
pub struct MeasuredText {
    pub content: String,
    pub wrap_mode: WrapMode,
}

/// The Text renderable's drawn state.
pub struct TextRenderableState {
    pub content: String,
    pub default_fg: Rgba,
    pub default_bg: Rgba,
    pub attributes: u32,
    pub wrap_mode: WrapMode,
    pub text_align: String,
    /// wrapped lines cache — set during `updateLayout` once width is known
    pub lines: Option<Vec<String>>,
}

impl TextRenderableState {
    pub fn from_options(opts: &TextOptions) -> Self {
        TextRenderableState {
            content: opts.content.clone(),
            default_fg: opts.fg.unwrap_or_else(|| Rgba::from_values(1.0, 1.0, 1.0, 1.0)),
            default_bg: opts.bg.unwrap_or_else(|| Rgba::from_values(0.0, 0.0, 0.0, 0.0)),
            attributes: opts.attributes,
            wrap_mode: opts.wrap_mode.unwrap_or(WrapMode::Word),
            text_align: opts.text_align.clone().unwrap_or_else(|| "left".into()),
            lines: None,
        }
    }

    pub fn measured(&self) -> MeasuredText {
        MeasuredText { content: self.content.clone(), wrap_mode: self.wrap_mode }
    }

    /// Re-wrap at the laid-out width.
    pub fn wrap_at(&mut self, width: u32) {
        self.lines = Some(wrap_text(&self.content, self.wrap_mode, width));
    }

    /// Intrinsic size for the measure function: wrap at available width
    /// (definite) or natural width (max-content), return
    /// (widest line, line count) — the text buffer measure contract.
    #[must_use]
    pub fn measure(text: &MeasuredText, known_width: Option<f32>, available_width: f32) -> (f32, f32) {
        let wrap_width = match known_width {
            Some(w) => w.max(0.0) as u32,
            None => {
                if available_width.is_finite() && available_width >= 0.0 {
                    available_width as u32
                } else {
                    u32::MAX / 4
                }
            }
        };
        let lines = wrap_text(&text.content, text.wrap_mode, wrap_width);
        let w = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
        (w as f32, lines.len() as f32)
    }

    /// `renderSelf` (TextBufferRenderable.ts:504-511): draw the view at
    /// screen coords, line by line.
    pub fn paint(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32) {
        let effective: Vec<String> = match &self.lines {
            Some(l) => l.clone(),
            // not laid out yet: draw unwrapped
            None => wrap_text(&self.content, self.wrap_mode, u32::MAX / 4),
        };
        for (i, line) in effective.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            buffer.draw_text(
                line,
                screen_x.max(0) as u32,
                (screen_y + i as i32).max(0) as u32,
                self.default_fg,
                Some(self.default_bg),
                self.attributes,
            );
        }
    }
}

use unicode_width::UnicodeWidthChar;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_textbuffer() {
        let s = TextRenderableState::from_options(&TextOptions { content: "hi".into(), ..Default::default() });
        assert_eq!(s.default_fg, Rgba::from_values(1.0, 1.0, 1.0, 1.0));
        assert_eq!(s.default_bg, Rgba::from_values(0.0, 0.0, 0.0, 0.0));
        assert_eq!(s.wrap_mode, WrapMode::Word);
    }

    #[test]
    fn word_wrap_greedy() {
        assert_eq!(wrap_text("hello world", WrapMode::Word, 5), vec!["hello", "world"]);
        assert_eq!(wrap_text("hello world", WrapMode::Word, 11), vec!["hello world"]);
        assert_eq!(wrap_text("hello world", WrapMode::Word, 6), vec!["hello", "world"]);
        // overlong word hard-breaks
        assert_eq!(wrap_text("abcdefgh", WrapMode::Word, 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap_text("a\nb", WrapMode::Word, 10), vec!["a", "b"]);
    }

    #[test]
    fn measure_natural_and_wrapped() {
        let m = MeasuredText { content: "hello world".into(), wrap_mode: WrapMode::Word };
        // max-content: no wrap
        assert_eq!(TextRenderableState::measure(&m, None, f32::INFINITY), (11.0, 1.0));
        // definite 5 → two lines
        assert_eq!(TextRenderableState::measure(&m, Some(5.0), 5.0), (5.0, 2.0));
    }
}
