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
//! directly. Tables construct `TextTableRenderable`s
//! (`createTableBlock` → `createTextTableRenderable`, Markdown.ts:1588
//! -1621 → TextTable.ts; the raw-markdown fallback survives only for
//! the no-content cache-miss shape, Markdown.ts:1595-1597).

use crate::renderables::code::{build_code_document, CodeOptions};
use crate::renderables::markdown_parser::{self, Token};
use crate::renderables::text_table::{
    ColumnFitter, ColumnWidthMode, TableConfig, TextTableOptions,
};
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
    /// `tableOptions` (Markdown.ts:112-114 → `MarkdownTableOptions`
    /// :32-84) — the consumed subset.
    pub table_options: Option<Box<MarkdownTableOptions>>,
}

/// `MarkdownTableOptions` (Markdown.ts:32-84) — the consumed subset.
#[derive(Debug, Clone, Default)]
pub struct MarkdownTableOptions {
    /// `style`: "grid" | "columns" (default grid in coalesced mode)
    pub style: Option<String>,
    /// `borders`
    pub borders: Option<bool>,
    /// `widthMode`: "content" | "full"
    pub width_mode: Option<String>,
    /// `columnFitter`: "proportional" | "balanced"
    pub column_fitter: Option<String>,
    /// `wrapMode`: "none" | "char" | "word"
    pub wrap_mode: Option<String>,
    pub cell_padding: Option<u32>,
    pub cell_padding_x: Option<u32>,
    pub cell_padding_y: Option<u32>,
    pub border_style: Option<String>,
    pub outer_border: Option<bool>,
    pub selectable: Option<bool>,
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
    /// `createTextTableRenderable` (Markdown.ts:1546-1586) — a
    /// `TextTableRenderable` over the chunk-grid content.
    Table {
        options: TextTableOptions,
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

// --- tables (TextTableRenderable wiring) -----------------------------------
//
// `getStyle` (Markdown.ts:536-545): group → base-segment fallback (the
// dotted-group half the table chunk builder needs).
fn md_get_style<'a>(
    style: &'a SyntaxStyle,
    group: &str,
) -> Option<&'a crate::syntax_style::StyleDefinition> {
    style
        .get_style(group)
        .or_else(|| style.get_style(group.split('.').next().unwrap_or(group)))
}

/// `createChunk` (Markdown.ts:547-563) with the base-segment fallback.
fn md_create_chunk(
    text: &str,
    group: &str,
    style: &SyntaxStyle,
    link: Option<String>,
) -> TextChunk {
    let def = md_get_style(style, group)
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

/// `isSupportedLinkTarget` (Markdown.ts:234-238) over
/// `MAX_LINK_URL_BYTES` (zig.ts:105).
fn is_supported_link_target(url: &str) -> bool {
    url.len() <= 512
}

/// `renderInlineContent` + `renderInlineToken` (Markdown.ts:586-699).
fn render_inline_tokens(
    tokens: &[Token],
    chunks: &mut Vec<TextChunk>,
    style: &SyntaxStyle,
    conceal: bool,
    hyperlinks_supported: bool,
    link: Option<String>,
) {
    for token in tokens {
        match token {
            Token::Text { text, .. } | Token::Escape { text, .. } => {
                chunks.push(md_create_chunk(text, "default", style, link.clone()));
            }
            Token::Tag { raw } => {
                // inline html — marked's html token carries text = raw
                chunks.push(md_create_chunk(raw, "default", style, link.clone()));
            }
            Token::Codespan { text, .. } => {
                if conceal {
                    chunks.push(md_create_chunk(text, "markup.raw", style, link.clone()));
                } else {
                    for piece in ["`", text.as_str(), "`"] {
                        chunks.push(md_create_chunk(piece, "markup.raw", style, link.clone()));
                    }
                }
            }
            Token::Strong { tokens, .. } => {
                if !conceal {
                    chunks.push(md_create_chunk("**", "markup.strong", style, link.clone()));
                }
                render_inline_with_style(
                    tokens,
                    chunks,
                    "markup.strong",
                    style,
                    conceal,
                    hyperlinks_supported,
                    link.clone(),
                );
                if !conceal {
                    chunks.push(md_create_chunk("**", "markup.strong", style, link.clone()));
                }
            }
            Token::Em { tokens, .. } => {
                if !conceal {
                    chunks.push(md_create_chunk("*", "markup.italic", style, link.clone()));
                }
                render_inline_with_style(
                    tokens,
                    chunks,
                    "markup.italic",
                    style,
                    conceal,
                    hyperlinks_supported,
                    link.clone(),
                );
                if !conceal {
                    chunks.push(md_create_chunk("*", "markup.italic", style, link.clone()));
                }
            }
            Token::Del { tokens, .. } => {
                if !conceal {
                    chunks.push(md_create_chunk(
                        "~~",
                        "markup.strikethrough",
                        style,
                        link.clone(),
                    ));
                }
                render_inline_with_style(
                    tokens,
                    chunks,
                    "markup.strikethrough",
                    style,
                    conceal,
                    hyperlinks_supported,
                    link.clone(),
                );
                if !conceal {
                    chunks.push(md_create_chunk(
                        "~~",
                        "markup.strikethrough",
                        style,
                        link.clone(),
                    ));
                }
            }
            Token::Link {
                raw,
                href,
                text,
                tokens,
                ..
            } => {
                let link_href = Some(href.clone());
                if conceal {
                    render_inline_with_style(
                        tokens,
                        chunks,
                        "markup.link.label",
                        style,
                        conceal,
                        hyperlinks_supported,
                        link_href.clone(),
                    );
                    if (!hyperlinks_supported || !is_supported_link_target(href))
                        && text != href
                        && raw != text
                        && *raw != format!("<{text}>")
                    {
                        chunks.push(md_create_chunk(
                            " (",
                            "markup.link",
                            style,
                            link_href.clone(),
                        ));
                        chunks.push(md_create_chunk(
                            href,
                            "markup.link.url",
                            style,
                            link_href.clone(),
                        ));
                        chunks.push(md_create_chunk(")", "markup.link", style, link_href));
                    }
                } else if raw == text || *raw == format!("<{text}>") {
                    if raw.starts_with('<') {
                        chunks.push(md_create_chunk(
                            "<",
                            "markup.link",
                            style,
                            link_href.clone(),
                        ));
                    }
                    render_inline_with_style(
                        tokens,
                        chunks,
                        "markup.link.label",
                        style,
                        conceal,
                        hyperlinks_supported,
                        link_href.clone(),
                    );
                    if raw.starts_with('<') {
                        chunks.push(md_create_chunk(">", "markup.link", style, link_href));
                    }
                } else {
                    chunks.push(md_create_chunk(
                        "[",
                        "markup.link",
                        style,
                        link_href.clone(),
                    ));
                    render_inline_with_style(
                        tokens,
                        chunks,
                        "markup.link.label",
                        style,
                        conceal,
                        hyperlinks_supported,
                        link_href.clone(),
                    );
                    chunks.push(md_create_chunk(
                        "](",
                        "markup.link",
                        style,
                        link_href.clone(),
                    ));
                    chunks.push(md_create_chunk(
                        href,
                        "markup.link.url",
                        style,
                        link_href.clone(),
                    ));
                    chunks.push(md_create_chunk(")", "markup.link", style, link_href));
                }
            }
            Token::Image { href, text, .. } => {
                let image_href = link.clone().or_else(|| Some(href.clone()));
                if conceal {
                    let label = if text.is_empty() { "image" } else { text };
                    chunks.push(md_create_chunk(
                        label,
                        "markup.link.label",
                        style,
                        image_href,
                    ));
                } else {
                    chunks.push(md_create_chunk(
                        "![",
                        "markup.link",
                        style,
                        image_href.clone(),
                    ));
                    chunks.push(md_create_chunk(
                        text,
                        "markup.link.label",
                        style,
                        image_href.clone(),
                    ));
                    chunks.push(md_create_chunk(
                        "](",
                        "markup.link",
                        style,
                        image_href.clone(),
                    ));
                    chunks.push(md_create_chunk(
                        href,
                        "markup.link.url",
                        style,
                        image_href.clone(),
                    ));
                    chunks.push(md_create_chunk(")", "markup.link", style, image_href));
                }
            }
            Token::Br { .. } => {
                chunks.push(md_create_chunk("\n", "default", style, link.clone()));
            }
            other => {
                // default branch (Markdown.ts:681-689): nested tokens; the
                // text fallback covers token shapes the table cells never
                // carry (block tokens), so containers recurse and the
                // rest contributes nothing
                if let Token::Heading { tokens, .. }
                | Token::Paragraph { tokens, .. }
                | Token::Blockquote { tokens, .. }
                | Token::ListItem { tokens, .. }
                | Token::Strong { tokens, .. }
                | Token::Em { tokens, .. }
                | Token::Del { tokens, .. } = other
                {
                    render_inline_tokens(
                        tokens,
                        chunks,
                        style,
                        conceal,
                        hyperlinks_supported,
                        link.clone(),
                    );
                }
            }
        }
    }
}

/// `renderInlineTokenWithStyle` (Markdown.ts:692-717).
fn render_inline_with_style(
    tokens: &[Token],
    chunks: &mut Vec<TextChunk>,
    style_group: &str,
    style: &SyntaxStyle,
    conceal: bool,
    hyperlinks_supported: bool,
    link: Option<String>,
) {
    for token in tokens {
        match token {
            Token::Text { text, .. } | Token::Escape { text, .. } => {
                chunks.push(md_create_chunk(text, style_group, style, link.clone()));
            }
            Token::Codespan { text, .. } => {
                if conceal {
                    chunks.push(md_create_chunk(text, "markup.raw", style, link.clone()));
                } else {
                    for piece in ["`", text.as_str(), "`"] {
                        chunks.push(md_create_chunk(piece, "markup.raw", style, link.clone()));
                    }
                }
            }
            other => render_inline_tokens(
                std::slice::from_ref(other),
                chunks,
                style,
                conceal,
                hyperlinks_supported,
                link.clone(),
            ),
        }
    }
}

/// `createTableDataCellChunks` (Markdown.ts:1382-1388): the cell's
/// inline chunks, or a single blank chunk.
fn table_data_cell_chunks(
    cell: Option<&markdown_parser::TableCell>,
    style: &SyntaxStyle,
    conceal: bool,
    hyperlinks_supported: bool,
) -> Vec<TextChunk> {
    let mut chunks = Vec::new();
    if let Some(cell) = cell {
        render_inline_tokens(
            &cell.tokens,
            &mut chunks,
            style,
            conceal,
            hyperlinks_supported,
            None,
        );
    }
    if chunks.is_empty() {
        chunks.push(md_create_chunk(" ", "default", style, None));
    }
    chunks
}

/// `createTableHeaderCellChunks` (Markdown.ts:1390-1410): the inline
/// chunks overlaid with the `markup.heading` style.
fn table_header_cell_chunks(
    cell: &markdown_parser::TableCell,
    style: &SyntaxStyle,
    conceal: bool,
    hyperlinks_supported: bool,
) -> Vec<TextChunk> {
    let mut chunks = Vec::new();
    render_inline_tokens(
        &cell.tokens,
        &mut chunks,
        style,
        conceal,
        hyperlinks_supported,
        None,
    );
    if chunks.is_empty() {
        chunks.push(md_create_chunk(" ", "default", style, None));
    }
    let Some(heading) = md_get_style(style, "markup.heading")
        .or_else(|| style.get_style("default"))
        .cloned()
    else {
        return chunks;
    };
    let heading_attributes = crate::styled_text::create_text_attributes(
        heading.bold.unwrap_or(false),
        heading.italic.unwrap_or(false),
        heading.underline.unwrap_or(false),
        heading.dim.unwrap_or(false),
        false,
        false,
        false,
        false,
    );
    chunks
        .into_iter()
        .map(|chunk| TextChunk {
            fg: heading.fg.or(chunk.fg),
            bg: heading.bg.or(chunk.bg),
            attributes: heading_attributes,
            ..chunk
        })
        .collect()
}

/// `resolveTableRenderableOptions` (Markdown.ts:1497-1523) for the
/// coalesced `internalBlockMode` the port pins: style "grid" unless
/// overridden; `usesBorderlessColumnSpacing` (:1490-1495) is false in
/// grid mode.
fn resolve_table_config(
    options: &MarkdownOptions,
    style: &SyntaxStyle,
) -> (TableConfig, crate::renderables::text::WrapMode) {
    let table_options = options.table_options.as_deref();
    let table_style = match table_options.and_then(|o| o.style.as_deref()) {
        Some("columns") => "columns",
        Some("grid") => "grid",
        // coalesced default (Markdown.ts:1484-1489)
        _ => "grid",
    };
    let borders = table_options
        .and_then(|o| o.borders)
        .unwrap_or(table_style == "grid");
    let borderless_columns = table_style == "columns" && !borders;
    let wrap_mode = crate::renderables::text::WrapMode::parse(
        table_options
            .and_then(|o| o.wrap_mode.as_deref())
            .unwrap_or("word"),
    );
    let column_width_mode = match table_options.and_then(|o| o.width_mode.as_deref()) {
        Some("content") => ColumnWidthMode::Content,
        Some("full") => ColumnWidthMode::Full,
        _ if table_style == "columns" => ColumnWidthMode::Content,
        _ => ColumnWidthMode::Full,
    };
    let column_fitter = match table_options.and_then(|o| o.column_fitter.as_deref()) {
        Some("balanced") => ColumnFitter::Balanced,
        _ => ColumnFitter::Proportional,
    };
    let cell_padding = table_options.and_then(|o| o.cell_padding).unwrap_or(0);
    let cell_padding_x = table_options
        .and_then(|o| o.cell_padding_x)
        .unwrap_or(cell_padding);
    let cell_padding_y = table_options
        .and_then(|o| o.cell_padding_y)
        .unwrap_or(cell_padding);
    let border_color = style
        .get_style("conceal")
        .and_then(|d| d.fg)
        .unwrap_or_else(|| crate::color::parse_color("#888888"));
    let defaults = TableConfig::default();
    let config = TableConfig {
        wrap_mode,
        column_width_mode,
        column_fitter,
        cell_padding_x,
        cell_padding_y,
        column_gap: if borderless_columns { 2 } else { 0 },
        show_borders: borders,
        border: borders,
        outer_border: table_options
            .and_then(|o| o.outer_border)
            .unwrap_or(borders),
        border_style: table_options
            .and_then(|o| o.border_style.clone())
            .unwrap_or_else(|| "single".into()),
        border_color,
        ..defaults
    };
    (config, wrap_mode)
}

/// `buildTableContentCache`'s content (:1415-1471) + `createTableBlock`
/// (:1588-1621): the chunk grid, or `None` for the empty cache shape
/// (no header/rows → the raw-markdown fallback, :1595-1597).
fn build_table_spec(
    token: &Token,
    style: &SyntaxStyle,
    options: &MarkdownOptions,
    margin_bottom: usize,
) -> BlockSpec {
    let Token::Table {
        raw, header, rows, ..
    } = token
    else {
        unreachable!("checked by caller");
    };
    let conceal = options.conceal.unwrap_or(true);
    if header.is_empty() || rows.is_empty() {
        // cache null → createMarkdownCodeRenderable(table.raw)
        return BlockSpec::MarkdownText {
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
        };
    }
    let mut content: Vec<Vec<Vec<TextChunk>>> = Vec::with_capacity(rows.len() + 1);
    // header row (rowIndex 0 is the header, :1436-1437)
    content.push(
        header
            .iter()
            .map(|cell| {
                table_header_cell_chunks(cell, style, conceal, options.hyperlinks_supported)
            })
            .collect(),
    );
    for row in rows {
        content.push(
            (0..header.len())
                .map(|col| {
                    table_data_cell_chunks(
                        row.get(col),
                        style,
                        conceal,
                        options.hyperlinks_supported,
                    )
                })
                .collect(),
        );
    }
    let (config, wrap_mode) = resolve_table_config(options, style);
    BlockSpec::Table {
        options: TextTableOptions {
            content,
            wrap_mode: Some(wrap_mode),
            column_width_mode: Some(config.column_width_mode),
            column_fitter: Some(config.column_fitter),
            cell_padding_x: Some(config.cell_padding_x),
            cell_padding_y: Some(config.cell_padding_y),
            column_gap: Some(config.column_gap),
            show_borders: Some(config.show_borders),
            border: Some(config.border),
            outer_border: Some(config.outer_border),
            border_style: Some(config.border_style.clone()),
            border_color: Some(config.border_color),
            fg: options.fg,
            ..Default::default()
        },
        margin_bottom,
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
            Token::Table { .. } => {
                // createTableBlock (Markdown.ts:1588-1621): the chunk-grid
                // content when the cache builds, else the raw fallback
                specs.push(build_table_spec(token, style, options, margin_bottom));
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
