//! `MarkdownRenderable` block pipeline — port of
//! `packages/core/src/renderables/Markdown.ts` (the coalesced
//! `internalBlockMode` path markdown-parser.ts feeds).
//!
//! The port builds the settled block tree: `updateBlocks` →
//! `buildRenderableTokens` (coalesce non-separate blocks into synthetic
//! paragraphs) → per-token renderables (code/blockquote/list/hr/table/
//! markdown-text). Streaming, `renderNode` hooks and incremental
//! reconciliation are upstream liveness machinery — the differential
//! oracle observes the settled tree, so the port constructs it
//! directly. Tables render through the fallback
//! `createMarkdownCodeRenderable(table.raw)` until the TextTable
//! renderable lands (documented delta; table scenarios pin the target).

use crate::renderables::code::{build_code_document, CodeOptions};
use crate::renderables::markdown_parser::{self, Token};
use crate::styled_text::TextChunk;
use crate::syntax_style::SyntaxStyle;

/// `MarkdownOptions` — Markdown.ts:87-126 (the consumed subset).
#[derive(Debug, Clone, Default)]
pub struct MarkdownOptions {
    pub content: String,
    pub fg: Option<crate::color::Rgba>,
    pub bg: Option<crate::color::Rgba>,
    /// `conceal` (default true)
    pub conceal: Option<bool>,
    /// `concealCode` (default false)
    pub conceal_code: Option<bool>,
    /// capabilities.hyperlinks in the profile (false headless)
    pub hyperlinks_supported: bool,
}

/// One block's renderable spec — what `updateBlocks` produces per token
/// (the Rust tree assembles these through the renderer's own
/// box/text/code constructors).
pub enum BlockSpec {
    /// `createMarkdownCodeRenderable` — a markdown-text CodeRenderable
    /// (filetype "markdown", detectLinks onChunks, conceal on) or the
    /// blockquote content with `baseHighlight: "markup.quote"`.
    MarkdownText {
        options: CodeOptions,
        margin_bottom: usize,
    },
    /// `createCodeRenderable` — fenced/indented code (Code.ts via
    /// Markdown.ts:1123-1138)
    CodeBlock {
        options: CodeOptions,
        margin_bottom: usize,
    },
    /// `createBlockquoteRenderable` — border-left box (Markdown.ts:833-855)
    Blockquote {
        content: Box<BlockSpec>,
        border_color: Option<crate::color::Rgba>,
        margin_bottom: usize,
    },
    /// `createHorizontalRuleRenderable` (Markdown.ts:1111-1121)
    HorizontalRule {
        border_color: Option<crate::color::Rgba>,
        margin_bottom: usize,
    },
    /// `createListRenderable` (Markdown.ts:857-872)
    List {
        items: Vec<ListItemSpec>,
        margin_bottom: usize,
    },
}

pub struct ListItemSpec {
    /// `marker.padStart(markerWidth) + " "` styled with "markup.list"
    pub marker_chunk: TextChunk,
    pub marker_width: usize,
    /// trailing blank line inside the item raw → extra bottom margin
    pub margin_bottom: usize,
    pub children: Vec<BlockSpec>,
    /// per-child margin-top (list "space" tokens → 1)
    pub child_margins_top: Vec<usize>,
}

/// `TRAILING_MARKDOWN_BLOCK_BREAKS_RE` (Markdown.ts:230).
fn normalize_markdown_block_raw(raw: &str) -> String {
    // (?:\r?\n){2,}$ → "\n"
    let trimmed = raw.trim_end_matches('\n');
    let trailing = raw.len() - trimmed.len();
    if trailing >= 2 {
        format!("{trimmed}\n")
    } else {
        raw.to_owned()
    }
}

/// `shouldRenderSeparately` (Markdown.ts:1205-1207).
fn should_render_separately(token: &Token) -> bool {
    matches!(
        token,
        Token::Code { .. } | Token::Table { .. } | Token::Blockquote { .. } | Token::Hr { .. }
    )
}

/// `getInterBlockMargin` (Markdown.ts:1209-1214).
fn inter_block_margin(token: &Token, next: Option<&Token>) -> usize {
    let Some(next) = next else { return 0 };
    if should_render_separately(token) {
        return 1;
    }
    if !should_render_separately(next) {
        return 0;
    }
    // TRAILING_MARKDOWN_BLOCK_NEWLINES_RE on raw
    let raw = token.raw();
    let trimmed = raw.trim_end_matches('\n');
    if raw.len() > trimmed.len() {
        0
    } else {
        1
    }
}

/// The style-lookup helper Markdown.ts:518-529 needs (`getStyle` +
/// first-segment fallback), producing a chunk.
pub fn markdown_chunk(
    text: &str,
    group: &str,
    style: &SyntaxStyle,
    link: Option<String>,
) -> TextChunk {
    let def = style
        .get_style(group)
        .or_else(|| style.get_style("default"))
        .cloned();
    let (fg, bg, attributes) = match &def {
        Some(d) => (
            d.fg,
            d.bg,
            crate::styled_text::create_text_attributes(
                d.bold.unwrap_or(false),
                d.italic.unwrap_or(false),
                d.underline.unwrap_or(false),
                d.dim.unwrap_or(false),
                false,
                false,
                false,
                false,
            ),
        ),
        None => (None, None, 0),
    };
    TextChunk {
        text: text.to_owned(),
        fg,
        bg,
        attributes,
        link,
    }
}

/// `getBlockquoteBorderColor` (Markdown.ts:829-831) — the RGBA object
/// passes straight through upstream (ColorInput accepts RGBA).
fn blockquote_border_color(
    style: &SyntaxStyle,
    fg: &Option<crate::color::Rgba>,
) -> Option<crate::color::Rgba> {
    style
        .get_style("conceal")
        .and_then(|d| d.fg)
        .or_else(|| style.get_style("default").and_then(|d| d.fg))
        .or(*fg)
        .or(Some(crate::color::Rgba::from_ints(0xFF, 0xFF, 0xFF, 0xFF)))
}

fn hr_border_color(
    style: &SyntaxStyle,
    fg: &Option<crate::color::Rgba>,
) -> Option<crate::color::Rgba> {
    style
        .get_style("conceal")
        .and_then(|d| d.fg)
        .or(*fg)
        .or(Some(crate::color::Rgba::from_ints(0x88, 0x88, 0x88, 0xFF)))
}

/// `buildRenderableTokens` (Markdown.ts:1242-1291) — coalesce the
/// non-separate blocks into synthetic paragraph tokens.
fn build_renderable_tokens(tokens: Vec<Token>) -> Vec<Token> {
    let mut render_tokens: Vec<Token> = Vec::new();
    let mut markdown_raw = String::new();

    let flush = |markdown_raw: &mut String, render_tokens: &mut Vec<Token>| {
        if markdown_raw.is_empty() {
            return;
        }
        let normalized = normalize_markdown_block_raw(markdown_raw);
        if !normalized.is_empty() {
            // createMarkdownBlockToken (Markdown.ts:1221-1228)
            render_tokens.push(Token::Paragraph {
                raw: normalized.clone(),
                text: normalized,
                tokens: Vec::new(),
            });
        }
        markdown_raw.clear();
    };

    let mut i = 0usize;
    while i < tokens.len() {
        let token = &tokens[i];
        match token {
            Token::Space { raw } => {
                if markdown_raw.is_empty() {
                    i += 1;
                    continue;
                }
                let mut next_index = i + 1;
                while next_index < tokens.len() && matches!(tokens[next_index], Token::Space { .. })
                {
                    next_index += 1;
                }
                let next_token = tokens.get(next_index);
                if next_token.is_some_and(|t| !should_render_separately(t)) {
                    markdown_raw.push_str(raw);
                }
            }
            t if should_render_separately(t) => {
                flush(&mut markdown_raw, &mut render_tokens);
                render_tokens.push(t.clone());
            }
            t => {
                markdown_raw.push_str(t.raw());
            }
        }
        i += 1;
    }
    flush(&mut markdown_raw, &mut render_tokens);
    render_tokens
}

/// The list marker inputs (Markdown.ts:874-885).
fn list_marker_inputs(token: &Token) -> Vec<(String, usize)> {
    let Token::List {
        items,
        ordered,
        start,
        ..
    } = token
    else {
        return Vec::new();
    };
    let start_num = match start {
        markdown_parser::ListStart::Number(n) => *n,
        markdown_parser::ListStart::Empty => 1,
    };
    let marker_width = items
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if *ordered {
                format!("{}.", start_num + index as u32).len()
            } else {
                1
            }
        })
        .max()
        .unwrap_or(1)
        .max(1);
    items
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let marker = if *ordered {
                format!("{}.", start_num + index as u32)
            } else {
                "-".to_owned()
            };
            (marker, marker_width)
        })
        .collect()
}

/// `createListChildRenderable` (Markdown.ts:1083-1109) for the item
/// token stream.
fn list_item_child_specs(
    item: &Token,
    style: &SyntaxStyle,
    options: &MarkdownOptions,
) -> (Vec<BlockSpec>, Vec<usize>) {
    let Token::ListItem { tokens, raw, .. } = item else {
        return (Vec::new(), Vec::new());
    };
    let conceal = options.conceal.unwrap_or(true);
    let mut specs = Vec::new();
    let mut margins = Vec::new();
    let mut pending_margin_top = 0usize;
    // checkbox + space tokens skip; others render
    let mut child_index = 0usize;
    for child in tokens {
        match child {
            Token::Checkbox { .. } => continue,
            Token::Space { .. } => {
                pending_margin_top = pending_margin_top.max(1);
                continue;
            }
            _ => {}
        }
        let spec = match child {
            Token::Text { raw, .. } | Token::Paragraph { raw, .. } => {
                // normalizeScrollbackMarkdownBlockRaw (Markdown.ts:1234)
                let content = raw.trim_end_matches('\n').to_owned();
                Some(BlockSpec::MarkdownText {
                    options: CodeOptions {
                        content,
                        filetype: Some("markdown".into()),
                        conceal: Some(conceal),
                        fg: options.fg,
                        bg: options.bg,
                        detect_links: true,
                        markdown_link_highlights: true,
                        hyperlinks_supported: options.hyperlinks_supported,
                        ..Default::default()
                    },
                    margin_bottom: 0,
                })
            }
            Token::List { .. } => Some(build_list_spec(child, style, options, 0)),
            Token::Code { text, lang, .. } => Some(BlockSpec::CodeBlock {
                options: CodeOptions {
                    content: text.clone(),
                    filetype: lang
                        .as_ref()
                        .and_then(|l| crate::tree_sitter::info_string_to_filetype(l)),
                    conceal: options.conceal_code,
                    fg: options.fg,
                    bg: options.bg,
                    ..Default::default()
                },
                margin_bottom: 0,
            }),
            Token::Blockquote { text, .. } => Some(BlockSpec::Blockquote {
                content: Box::new(BlockSpec::MarkdownText {
                    options: CodeOptions {
                        content: if text.is_empty() {
                            " ".to_owned()
                        } else {
                            text.clone()
                        },
                        filetype: Some("markdown".into()),
                        conceal: Some(conceal),
                        base_highlight: Some("markup.quote".into()),
                        fg: options.fg,
                        bg: options.bg,
                        detect_links: true,
                        hyperlinks_supported: options.hyperlinks_supported,
                        ..Default::default()
                    },
                    margin_bottom: 0,
                }),
                border_color: blockquote_border_color(style, &options.fg),
                margin_bottom: 0,
            }),
            Token::Hr { .. } => Some(BlockSpec::HorizontalRule {
                border_color: hr_border_color(style, &options.fg),
                margin_bottom: 0,
            }),
            _ => {
                let _ = raw;
                None
            }
        };
        if let Some(spec) = spec {
            let is_list = matches!(child, Token::List { .. });
            let margin_top = if is_list { 0 } else { pending_margin_top };
            pending_margin_top = 0;
            specs.push(spec);
            margins.push(margin_top);
            child_index += 1;
        }
    }
    let _ = child_index;
    (specs, margins)
}

/// `createListItemRenderable` rows (Markdown.ts:921-963).
fn build_list_spec(
    token: &Token,
    style: &SyntaxStyle,
    options: &MarkdownOptions,
    margin_bottom: usize,
) -> BlockSpec {
    let inputs = list_marker_inputs(token);
    let Token::List { items, .. } = token else {
        unreachable!("checked by caller");
    };
    let mut item_specs = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some((marker, marker_width)) = inputs.get(index) else {
            continue;
        };
        let marker_text = format!("{} ", pad_start(marker, *marker_width));
        let marker_chunk = markdown_chunk(&marker_text, "markup.list", style, None);
        // marginBottom: /\n[ \t]*\n$/ on the item raw (Markdown.ts:927)
        let item_margin_bottom = if ends_with_blank_line(item.raw()) {
            1
        } else {
            0
        };
        let (children, child_margins_top) = list_item_child_specs(item, style, options);
        item_specs.push(ListItemSpec {
            marker_chunk,
            marker_width: marker_width + 1,
            margin_bottom: item_margin_bottom,
            children,
            child_margins_top,
        });
    }
    BlockSpec::List {
        items: item_specs,
        margin_bottom,
    }
}

fn ends_with_blank_line(raw: &str) -> bool {
    // /\n[ \t]*\n[ \t]*$/
    let t = raw.trim_end_matches([' ', '\t']);
    match t.strip_suffix('\n') {
        Some(t) => {
            let t = t.trim_end_matches([' ', '\t']);
            t.ends_with('\n') || t.is_empty()
        }
        None => false,
    }
}

fn pad_start(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len < width {
        format!("{}{}", " ".repeat(width - len), s)
    } else {
        s.to_owned()
    }
}

/// `updateBlocks` → `createDefaultRenderable` for each block token
/// (Markdown.ts:1724-1764) — the coalesced mode the port pins.
pub fn build_markdown_blocks(options: &MarkdownOptions, style: &SyntaxStyle) -> Vec<BlockSpec> {
    let tokens = markdown_parser::lex(&options.content);
    let block_tokens = build_renderable_tokens(tokens);
    let conceal = options.conceal.unwrap_or(true);

    let mut specs = Vec::new();
    for (index, token) in block_tokens.iter().enumerate() {
        let next = block_tokens.get(index + 1);
        let margin_bottom = inter_block_margin(token, next);
        match token {
            Token::Code { text, lang, .. } => {
                specs.push(BlockSpec::CodeBlock {
                    options: CodeOptions {
                        content: text.clone(),
                        filetype: lang
                            .as_ref()
                            .and_then(|l| crate::tree_sitter::info_string_to_filetype(l)),
                        conceal: options.conceal_code,
                        fg: options.fg,
                        bg: options.bg,
                        ..Default::default()
                    },
                    margin_bottom,
                });
            }
            Token::Blockquote { text, .. } => {
                specs.push(BlockSpec::Blockquote {
                    content: Box::new(BlockSpec::MarkdownText {
                        options: CodeOptions {
                            content: if text.is_empty() {
                                " ".to_owned()
                            } else {
                                text.clone()
                            },
                            filetype: Some("markdown".into()),
                            conceal: Some(conceal),
                            base_highlight: Some("markup.quote".into()),
                            fg: options.fg,
                            bg: options.bg,
                            detect_links: true,
                            hyperlinks_supported: options.hyperlinks_supported,
                            ..Default::default()
                        },
                        margin_bottom: 0,
                    }),
                    border_color: blockquote_border_color(style, &options.fg),
                    margin_bottom,
                });
            }
            Token::List { .. } => {
                specs.push(build_list_spec(token, style, options, margin_bottom));
            }
            Token::Hr { .. } => {
                specs.push(BlockSpec::HorizontalRule {
                    border_color: hr_border_color(style, &options.fg),
                    margin_bottom,
                });
            }
            Token::Table { raw, .. } => {
                // TextTableRenderable is not yet ported — the fallback
                // path (createTableBlock with no cache, Markdown.ts:1597)
                // renders the raw markdown
                specs.push(BlockSpec::MarkdownText {
                    options: CodeOptions {
                        content: raw.clone(),
                        filetype: Some("markdown".into()),
                        conceal: Some(conceal),
                        fg: options.fg,
                        bg: options.bg,
                        detect_links: true,
                        hyperlinks_supported: options.hyperlinks_supported,
                        ..Default::default()
                    },
                    margin_bottom,
                });
            }
            _ => {
                // heading/paragraph/text/html/def — the markdown-text
                // CodeRenderable over token.raw
                let raw = token.raw().to_owned();
                if raw.is_empty() {
                    continue;
                }
                specs.push(BlockSpec::MarkdownText {
                    options: CodeOptions {
                        content: raw,
                        filetype: Some("markdown".into()),
                        conceal: Some(conceal),
                        fg: options.fg,
                        bg: options.bg,
                        detect_links: true,
                        markdown_link_highlights: true,
                        hyperlinks_supported: options.hyperlinks_supported,
                        ..Default::default()
                    },
                    margin_bottom,
                });
            }
        }
    }
    specs
}

/// Build a Code renderable's settled styled text (the make_code entry).
pub fn markdown_code_styled(
    options: &CodeOptions,
    style: &SyntaxStyle,
) -> crate::styled_text::StyledText {
    build_code_document(options, style).to_styled()
}
