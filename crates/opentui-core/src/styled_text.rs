//! `StyledText` + `TextChunk` — port of
//! `packages/core/src/lib/styled-text.ts` (the styled spans) and the
//! `TextChunk` shape from `packages/core/src/text-buffer.ts:7-14`
//! (the data the text buffer rasterizes).
//!
//! Upstream model: a `StyledText` is an ordered list of chunks; each
//! chunk carries optional `fg`/`bg` (parsed [`Rgba`]), an `attributes`
//! bit set (see [`crate::ansi::text_attributes`]) and an optional
//! hyperlink. Chunks concatenate into the document text; the style
//! applies to the chunk's columns (text-buffer.zig `setStyledText`,
//! 1157-1269: per-chunk style definitions + column-range highlights).

use crate::ansi::text_attributes;
use crate::color::{parse_color, Rgba};

/// One styled span — `TextChunk` (text-buffer.ts:7-14). `fg`/`bg` are
/// already-parsed packed colors (the TS side stores `RGBA` objects).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextChunk {
    pub text: String,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
    /// Hyperlink URL (chunk `link: { url }`). Rasterizing links needs the
    /// renderer's link-id tracker (upstream `LinkPool`); the id bits live
    /// in cell attributes 8-31. The port keeps the field for API parity;
    /// differential link scenarios arrive with the events lane.
    pub link: Option<String>,
}

impl TextChunk {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        TextChunk {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// `StyledText` — styled-text.ts:26-34.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyledText {
    pub chunks: Vec<TextChunk>,
}

impl StyledText {
    #[must_use]
    pub fn new(chunks: Vec<TextChunk>) -> Self {
        StyledText { chunks }
    }

    /// Concatenated plain text (what the buffer rasterizes).
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.chunks.iter().map(|c| c.text.as_str()).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.iter().all(|c| c.text.is_empty())
    }
}

/// `stringToStyledText` — styled-text.ts:36-42.
#[must_use]
pub fn string_to_styled_text(content: &str) -> StyledText {
    StyledText::new(vec![TextChunk::new(content)])
}

/// `StyleAttrs` — styled-text.ts:10-20: the style vocabulary the
/// helper functions apply.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleAttrs {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub dim: bool,
    pub reverse: bool,
    pub blink: bool,
}

/// `createTextAttributes` — utils.ts:4-29: booleans → attribute bits.
/// `inverse` and `reverse` are synonyms for the same bit.
#[must_use]
#[allow(clippy::too_many_arguments)] // the upstream signature is one boolean per attribute
pub fn create_text_attributes(
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
) -> u32 {
    let mut attributes = text_attributes::NONE_U32;
    if bold {
        attributes |= text_attributes::BOLD_U32;
    }
    if italic {
        attributes |= text_attributes::ITALIC_U32;
    }
    if underline {
        attributes |= text_attributes::UNDERLINE_U32;
    }
    if dim {
        attributes |= text_attributes::DIM_U32;
    }
    if blink {
        attributes |= text_attributes::BLINK_U32;
    }
    if inverse {
        attributes |= text_attributes::INVERSE_U32;
    }
    if hidden {
        attributes |= text_attributes::HIDDEN_U32;
    }
    if strikethrough {
        attributes |= text_attributes::STRIKETHROUGH_U32;
    }
    attributes
}

impl StyleAttrs {
    fn attributes(&self) -> u32 {
        create_text_attributes(
            self.bold,
            self.italic,
            self.underline,
            self.dim,
            self.blink,
            self.reverse,
            false,
            self.strikethrough,
        )
    }
}

/// Input to a style helper — string, number, or an existing chunk
/// (`StylableInput`, styled-text.ts:44).
#[derive(Debug, Clone)]
pub enum StylableInput {
    Str(String),
    Chunk(TextChunk),
}

impl From<&str> for StylableInput {
    fn from(s: &str) -> Self {
        StylableInput::Str(s.to_owned())
    }
}

impl From<String> for StylableInput {
    fn from(s: String) -> Self {
        StylableInput::Str(s)
    }
}

impl From<TextChunk> for StylableInput {
    fn from(c: TextChunk) -> Self {
        StylableInput::Chunk(c)
    }
}

/// `applyStyle` — styled-text.ts:46-78. Chunks merge: explicit style
/// colors win, attributes OR; plain inputs take the style wholesale.
#[must_use]
pub fn apply_style(input: StylableInput, style: &StyleAttrs) -> TextChunk {
    match input {
        StylableInput::Chunk(existing) => {
            let fg = style.fg.as_deref().map(parse_color).or(existing.fg);
            let bg = style.bg.as_deref().map(parse_color).or(existing.bg);
            let new_attrs = style.attributes();
            let attributes = existing.attributes | new_attrs;
            TextChunk {
                text: existing.text,
                fg,
                bg,
                attributes,
                link: existing.link,
            }
        }
        StylableInput::Str(text) => TextChunk {
            fg: style.fg.as_deref().map(parse_color),
            bg: style.bg.as_deref().map(parse_color),
            attributes: style.attributes(),
            link: None,
            text,
        },
    }
}

/// Style helpers — styled-text.ts:81-127. Each wraps [`apply_style`]
/// with one attribute/color, exactly like the upstream chainable
/// functions (`bold(input)`, `fg(color)(input)`, …).
pub mod styles {
    use super::*;

    macro_rules! attr_fn {
        ($name:ident, $field:ident, $doc:expr) => {
            #[doc = $doc]
            pub fn $name(input: impl Into<StylableInput>) -> TextChunk {
                apply_style(
                    input.into(),
                    &StyleAttrs {
                        $field: true,
                        ..Default::default()
                    },
                )
            }
        };
    }

    attr_fn!(bold, bold, "`bold` — styled-text.ts:111.");
    attr_fn!(italic, italic, "`italic` — styled-text.ts:112.");
    attr_fn!(underline, underline, "`underline` — styled-text.ts:113.");
    attr_fn!(
        strikethrough,
        strikethrough,
        "`strikethrough` — styled-text.ts:114."
    );
    attr_fn!(dim, dim, "`dim` — styled-text.ts:115.");
    attr_fn!(reverse, reverse, "`reverse` — styled-text.ts:116.");
    attr_fn!(blink, blink, "`blink` — styled-text.ts:117.");

    macro_rules! color_fn {
        ($name:ident, $field:ident, $color:literal, $doc:expr) => {
            #[doc = $doc]
            pub fn $name(input: impl Into<StylableInput>) -> TextChunk {
                apply_style(
                    input.into(),
                    &StyleAttrs {
                        $field: Some($color.to_owned()),
                        ..Default::default()
                    },
                )
            }
        };
    }

    color_fn!(black, fg, "black", "`black` — styled-text.ts:81.");
    color_fn!(red, fg, "red", "`red` — styled-text.ts:82.");
    color_fn!(green, fg, "green", "`green` — styled-text.ts:83.");
    color_fn!(yellow, fg, "yellow", "`yellow` — styled-text.ts:84.");
    color_fn!(blue, fg, "blue", "`blue` — styled-text.ts:85.");
    color_fn!(magenta, fg, "magenta", "`magenta` — styled-text.ts:86.");
    color_fn!(cyan, fg, "cyan", "`cyan` — styled-text.ts:87.");
    color_fn!(white, fg, "white", "`white` — styled-text.ts:88.");
    color_fn!(
        bright_black,
        fg,
        "brightBlack",
        "`brightBlack` — styled-text.ts:91."
    );
    color_fn!(
        bright_red,
        fg,
        "brightRed",
        "`brightRed` — styled-text.ts:92."
    );
    color_fn!(
        bright_green,
        fg,
        "brightGreen",
        "`brightGreen` — styled-text.ts:93."
    );
    color_fn!(
        bright_yellow,
        fg,
        "brightYellow",
        "`brightYellow` — styled-text.ts:94."
    );
    color_fn!(
        bright_blue,
        fg,
        "brightBlue",
        "`brightBlue` — styled-text.ts:95."
    );
    color_fn!(
        bright_magenta,
        fg,
        "brightMagenta",
        "`brightMagenta` — styled-text.ts:96."
    );
    color_fn!(
        bright_cyan,
        fg,
        "brightCyan",
        "`brightCyan` — styled-text.ts:97."
    );
    color_fn!(
        bright_white,
        fg,
        "brightWhite",
        "`brightWhite` — styled-text.ts:98."
    );
    color_fn!(bg_black, bg, "black", "`bgBlack` — styled-text.ts:101.");
    color_fn!(bg_red, bg, "red", "`bgRed` — styled-text.ts:102.");
    color_fn!(bg_green, bg, "green", "`bgGreen` — styled-text.ts:103.");
    color_fn!(bg_yellow, bg, "yellow", "`bgYellow` — styled-text.ts:104.");
    color_fn!(bg_blue, bg, "blue", "`bgBlue` — styled-text.ts:105.");
    color_fn!(
        bg_magenta,
        bg,
        "magenta",
        "`bgMagenta` — styled-text.ts:106."
    );
    color_fn!(bg_cyan, bg, "cyan", "`bgCyan` — styled-text.ts:107.");
    color_fn!(bg_white, bg, "white", "`bgWhite` — styled-text.ts:108.");

    /// `fg(color)(input)` — styled-text.ts:120-123. Accepts any
    /// parseable color (CSS name, hex, …).
    pub fn fg(color: &str, input: impl Into<StylableInput>) -> TextChunk {
        apply_style(
            input.into(),
            &StyleAttrs {
                fg: Some(color.to_owned()),
                ..Default::default()
            },
        )
    }

    /// `bg(color)(input)` — styled-text.ts:124-127.
    pub fn bg(color: &str, input: impl Into<StylableInput>) -> TextChunk {
        apply_style(
            input.into(),
            &StyleAttrs {
                bg: Some(color.to_owned()),
                ..Default::default()
            },
        )
    }
}

/// `t` — the template-literal chunk assembler (styled-text.ts:150-178):
/// literal strings become plain chunks (attributes 0), interpolated
/// chunks pass through, interpolated plain values stringify.
#[must_use]
pub fn t(parts: &[&str], values: &[StylableInput]) -> StyledText {
    let mut chunks = Vec::new();
    for (i, raw) in parts.iter().enumerate() {
        if !raw.is_empty() {
            chunks.push(TextChunk {
                attributes: 0,
                ..TextChunk::new(*raw)
            });
        }
        if let Some(val) = values.get(i) {
            match val {
                StylableInput::Chunk(c) => chunks.push(c.clone()),
                StylableInput::Str(s) => chunks.push(TextChunk {
                    attributes: 0,
                    ..TextChunk::new(s.clone())
                }),
            }
        }
    }
    StyledText::new(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::parse_color;

    #[test]
    fn string_to_styled() {
        let s = string_to_styled_text("hi");
        assert_eq!(s.chunks.len(), 1);
        assert_eq!(s.chunks[0].text, "hi");
        assert_eq!(s.chunks[0].attributes, 0);
        assert!(s.chunks[0].fg.is_none());
    }

    #[test]
    fn style_helpers_match_upstream_values() {
        let c = styles::bold("x");
        assert_eq!(c.attributes, text_attributes::BOLD_U32);
        assert_eq!(c.text, "x");

        let c = styles::red("err");
        assert_eq!(c.fg, Some(parse_color("red")));

        let c = styles::bg_blue(styles::bold("both"));
        // merged chunk: attrs OR, new bg wins, fg kept
        assert_eq!(c.attributes, text_attributes::BOLD_U32);
        assert_eq!(c.bg, Some(parse_color("blue")));
        assert_eq!(c.text, "both");
    }

    #[test]
    fn create_text_attributes_bits() {
        assert_eq!(
            create_text_attributes(true, false, false, false, false, false, false, false),
            1
        );
        assert_eq!(
            create_text_attributes(false, false, false, false, false, true, false, false),
            1 << 5
        );
        assert_eq!(
            create_text_attributes(false, false, false, false, false, false, true, false),
            1 << 6
        );
        assert_eq!(
            create_text_attributes(false, false, false, false, false, false, false, true),
            1 << 7
        );
    }

    #[test]
    fn template_assembler() {
        let s = t(&["a ", " c"], &[StylableInput::Chunk(styles::bold("b"))]);
        assert_eq!(s.plain_text(), "a b c");
        assert_eq!(s.chunks.len(), 3);
        assert_eq!(s.chunks[1].attributes, 1);
    }
}
