//! `CodeRenderable` — port of `packages/core/src/renderables/Code.ts`
//! over `TextBufferRenderable`.
//!
//! Upstream highlights asynchronously (worker round-trip →
//! `setStyledText`); the port highlights **synchronously at
//! construction** — the differential oracle captures the settled frame
//! (after `flush()`, where the async result has landed), so the port
//! builds the settled state directly. The pipeline itself is the
//! upstream one: `highlightOnce` → `onHighlight` hook →
//! `treeSitterToTextChunks` → `onChunks` hook (detectLinks) →
//! `setStyledText` (Code.ts:348-455).
//!
//! Also ports the two markdown hooks: `detectLinks`
//! (lib/detect-links.ts) and `addMarkdownLinkHighlights`
//! (Markdown.ts:724-790) — both operate in UTF-16 index space like the
//! JS.

use crate::styled_text::{StyledText, TextChunk};
use crate::syntax_style::SyntaxStyle;
use crate::text_buffer::TextDefaults;
use crate::tree_sitter::chunks::TextChunkOptions;
use crate::tree_sitter::{
    chunks::tree_sitter_to_text_chunks, get_tree_sitter_client, HighlightMeta, SimpleHighlight,
};

/// `CodeOptions` — Code.ts:32-44 (the subset the pipeline consumes).
#[derive(Debug, Clone, Default)]
pub struct CodeOptions {
    pub content: String,
    pub filetype: Option<String>,
    /// `conceal` (default true)
    pub conceal: Option<bool>,
    /// `baseHighlight` (blockquote → "markup.quote")
    pub base_highlight: Option<String>,
    /// `fg`/`bg` overrides (None → the text-buffer defaults)
    pub fg: Option<crate::color::Rgba>,
    pub bg: Option<crate::color::Rgba>,
    /// `onChunks = detectLinks` (markdown text blocks)
    pub detect_links: bool,
    /// `onHighlight = addMarkdownLinkHighlights` (markdown text blocks)
    pub markdown_link_highlights: bool,
    /// `ctx.capabilities.hyperlinks === true` — false in the headless
    /// profile
    pub hyperlinks_supported: bool,
}

/// The settled document of a CodeRenderable: the styled-text chunks
/// after the full pipeline, plus the plain fallback.
pub struct CodeDocument {
    pub chunks: Vec<TextChunk>,
    pub plain: bool,
}

impl CodeDocument {
    pub fn to_styled(&self) -> StyledText {
        StyledText::new(self.chunks.clone())
    }

    pub fn defaults(&self) -> TextDefaults {
        TextDefaults {
            fg: crate::color::Rgba::from_values(1.0, 1.0, 1.0, 1.0),
            bg: crate::color::Rgba::from_values(0.0, 0.0, 0.0, 0.0),
            attributes: 0,
        }
    }
}

/// The settled `renderSelf` outcome (Code.ts:348-455 with the streaming
/// branches collapsed — `drawUnstyledText` settles to the highlighted
/// document; no filetype or no parser → plain text).
pub fn build_code_document(options: &CodeOptions, syntax_style: &SyntaxStyle) -> CodeDocument {
    let conceal = options.conceal.unwrap_or(true);
    let defaults = CodeDocument::defaults(&CodeDocument {
        chunks: Vec::new(),
        plain: true,
    });
    let _ = defaults;

    let Some(filetype) = &options.filetype else {
        // renderSelf: no filetype → setText(content) (Code.ts:593-601)
        return CodeDocument {
            chunks: vec![plain_chunk(&options.content)],
            plain: true,
        };
    };

    let result = get_tree_sitter_client()
        .lock()
        .expect("tree-sitter client")
        .highlight_once(&options.content, filetype);
    let mut highlights: Vec<SimpleHighlight> = result.highlights.unwrap_or_default();

    // onHighlight = addMarkdownLinkHighlights (Markdown.ts:724-790)
    if options.markdown_link_highlights {
        highlights = add_markdown_link_highlights(
            highlights,
            &options.content,
            conceal,
            options.hyperlinks_supported,
        );
    }

    // Code.ts:399: `highlights.length > 0 || onChunks || baseHighlight`
    if !highlights.is_empty() || options.detect_links || options.base_highlight.is_some() {
        let mut ranges = if options.detect_links {
            Some(Vec::new())
        } else {
            None
        };
        let mut chunks = tree_sitter_to_text_chunks(
            &options.content,
            &highlights,
            syntax_style,
            TextChunkOptions {
                enabled: Some(conceal),
                base_highlight: options.base_highlight.as_deref(),
                ranges: ranges.as_mut(),
            },
        );
        if options.detect_links {
            chunks = detect_links(chunks, &options.content, &highlights, ranges.as_deref());
        }
        CodeDocument {
            chunks,
            plain: false,
        }
    } else {
        CodeDocument {
            chunks: vec![plain_chunk(&options.content)],
            plain: true,
        }
    }
}

fn plain_chunk(text: &str) -> TextChunk {
    TextChunk {
        text: text.to_owned(),
        fg: None,
        bg: None,
        attributes: 0,
        link: None,
    }
}

/// `normalizeMarkdownLinkTarget` — detect-links.ts:5-7: unescape
/// backslash-escaped punctuation in link destinations.
pub fn normalize_markdown_link_target(destination: &str) -> String {
    let mut out = String::new();
    let mut chars = destination.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if is_ascii_punct(next) {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

const ESCAPABLE: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~-";

fn is_ascii_punct(c: char) -> bool {
    ESCAPABLE.contains(c)
}

/// `detectLinks` — detect-links.ts:12-167. Rewrites chunks so URL ranges
/// carry `link: {url}` (the label side of markdown links too).
pub fn detect_links(
    chunks: Vec<TextChunk>,
    content: &str,
    highlights: &[SimpleHighlight],
    source_ranges: Option<&[(usize, usize)]>,
) -> Vec<TextChunk> {
    let has_bare_url = content.contains("http://")
        || content.contains("https://")
        || content.contains("HTTP://")
        || content.contains("HTTPS://");
    let url_scopes = ["markup.link.url", "string.special.url"];
    let has_url_scope = highlights
        .iter()
        .any(|h| url_scopes.contains(&h.group.as_str()));
    if !has_bare_url && !has_url_scope {
        return chunks;
    }

    #[derive(Debug, Clone)]
    struct Range {
        start: usize,
        end: usize,
        url: String,
        label: bool,
        /// `markup.link.url` normalizes the destination;
        /// `string.special.url` doesn't (detect-links.ts:68)
        markdown_group: bool,
    }

    let mut ranges: Vec<Range> = Vec::new();
    let mut raw_ranges: Vec<(usize, usize)> = Vec::new();
    let map = crate::tree_sitter::Utf16Map::new(content);
    let char_at = |i: usize| content[map.byte_of_utf16(i)..].chars().next();
    let slice = |a: usize, b: usize| map.slice(content, a, b);

    let mut labels: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, insertion)
    let mut concealed_delimiters: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();

    for highlight in highlights {
        let (start, end, group, meta) = (
            &highlight.start,
            &highlight.end,
            &highlight.group,
            &highlight.meta,
        );
        if group == "conceal"
            && meta.conceal.as_deref() == Some("")
            && slice(*start, *end)
                .chars()
                .all(|c| c == '*' || c == '_' || c == '~')
        {
            concealed_delimiters.insert(*end, *start);
        }
        if group == "markup.raw" || group.starts_with("markup.raw.") {
            raw_ranges.push((*start, *end));
        }
        if group == "markup.link.label" {
            // keep the EARLIEST label ending at each position
            if let Some(existing) = labels.iter().find(|(_, e, _)| e == end) {
                if *start < existing.0 {
                    let idx = labels.iter().position(|(_, e, _)| e == end).unwrap();
                    labels[idx] = (*start, *end, idx);
                }
            } else {
                labels.push((*start, *end, labels.len()));
            }
        }
        if !url_scopes.contains(&group.as_str()) {
            continue;
        }
        let url = slice(*start, *end).to_owned();
        if url.is_empty() {
            continue;
        }
        ranges.push(Range {
            start: *start,
            end: *end,
            url,
            label: false,
            markdown_group: group == "markup.link.url",
        });
    }

    let highlighted_count = ranges.len();
    for index in 0..highlighted_count {
        let range = ranges[index].clone();
        let mut label_end = range.start;
        if char_at(label_end.wrapping_sub(1)) == Some('<') && label_end > 0 {
            label_end -= 1;
        }
        while label_end > 0
            && char_at(label_end - 1)
                .map(|c| c.is_whitespace())
                .unwrap_or(false)
        {
            label_end -= 1;
        }
        if char_at(label_end.wrapping_sub(1)) != Some('(') || label_end < 2 {
            continue;
        }
        if char_at(label_end - 2) != Some(']') {
            continue;
        }
        let Some(&(_, _, _)) = labels.iter().find(|(_, e, _)| *e == label_end - 2) else {
            continue;
        };
        let label = labels.iter().find(|(_, e, _)| *e == label_end - 2).unwrap();
        let url = if ranges[index].markdown_group {
            normalize_markdown_link_target(&range.url)
        } else {
            range.url.clone()
        };
        ranges.push(Range {
            start: label.0,
            end: label.1,
            url,
            label: true,
            markdown_group: ranges[index].markdown_group,
        });
    }

    if has_bare_url {
        raw_ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        ranges.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut raw_index = 0usize;
        let mut highlighted_index = 0usize;
        // content.matchAll(BARE_URL) — https?://[^\s<>"`]+
        let bytes: Vec<char> = content.chars().collect();
        let mut matches: Vec<(usize, usize)> = Vec::new(); // u16 ranges
        {
            let mut scan = 0usize; // char index
            while scan < bytes.len() {
                let lower: String = bytes[scan..(scan + 8).min(bytes.len())]
                    .iter()
                    .collect::<String>()
                    .to_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") {
                    let mut j = scan;
                    while j < bytes.len()
                        && !bytes[j].is_whitespace()
                        && bytes[j] != '<'
                        && bytes[j] != '>'
                        && bytes[j] != '"'
                        && bytes[j] != '`'
                    {
                        j += 1;
                    }
                    matches.push((
                        char_index_to_u16(&bytes, scan),
                        char_index_to_u16(&bytes, j),
                    ));
                    scan = j;
                } else {
                    scan += 1;
                }
            }
        }
        for (start, end) in matches {
            let mut url = slice(start, end).to_owned();
            let mut open_parens = 0usize;
            let mut close_parens = 0usize;
            for c in url.chars() {
                if c == '(' {
                    open_parens += 1;
                }
                if c == ')' {
                    close_parens += 1;
                }
            }
            while !url.is_empty() {
                let len_u16: usize = url.chars().map(char::len_utf16).sum();
                if let Some(&delimiter_start) = concealed_delimiters.get(&(start + len_u16)) {
                    if delimiter_start >= start {
                        url = slice(start, delimiter_start).to_owned();
                        continue;
                    }
                }
                let last = url.chars().last();
                let trimmed = match last {
                    Some(')') if close_parens > open_parens => {
                        close_parens -= 1;
                        true
                    }
                    Some(c) if ".,!?;:]}'".contains(c) => true,
                    _ => false,
                };
                if !trimmed {
                    break;
                }
                url.pop();
            }
            if url.is_empty() {
                continue;
            }
            let url_len: usize = url.chars().map(char::len_utf16).sum();
            let end = start + url_len;
            while raw_index < raw_ranges.len() && raw_ranges[raw_index].1 <= start {
                raw_index += 1;
            }
            if raw_index < raw_ranges.len() && raw_ranges[raw_index].0 < end {
                continue;
            }
            while highlighted_index < ranges.len() && ranges[highlighted_index].end <= start {
                highlighted_index += 1;
            }
            if highlighted_index < ranges.len() && ranges[highlighted_index].start < end {
                continue;
            }
            ranges.push(Range {
                start,
                end,
                url,
                label: false,
                markdown_group: true,
            });
        }
    }

    if ranges.is_empty() {
        return chunks;
    }
    ranges.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(b.label.cmp(&a.label))
    });

    let mut linked_chunks: Vec<TextChunk> = Vec::new();
    let mut content_pos = 0usize;
    let mut range_index = 0usize;
    for (index, chunk) in chunks.iter().enumerate() {
        if chunk.text.is_empty() || chunk.link.is_some() {
            linked_chunks.push(chunk.clone());
            continue;
        }
        let provided_range = source_ranges.and_then(|r| r.get(index).copied());
        let start = match provided_range {
            Some((s, _)) => s,
            None => match find_subrange(content, &chunk.text, content_pos) {
                Some(s) => s,
                None => {
                    linked_chunks.push(chunk.clone());
                    continue;
                }
            },
        };
        let end = match provided_range {
            Some((_, e)) => e,
            None => start + chunk.text.chars().map(char::len_utf16).sum::<usize>(),
        };
        content_pos = end;
        while range_index < ranges.len() && ranges[range_index].end <= start {
            range_index += 1;
        }
        if slice(start, end) != chunk.text {
            if let Some(range) = ranges.get(range_index) {
                if range.label && range.start < end {
                    let mut c = chunk.clone();
                    c.link = Some(range.url.clone());
                    linked_chunks.push(c);
                    continue;
                }
            }
            linked_chunks.push(chunk.clone());
            continue;
        }

        let mut position = start;
        let mut consumed_any = false;
        while range_index < ranges.len() {
            let range = &ranges[range_index];
            if range.end <= position {
                range_index += 1;
                continue;
            }
            if range.start >= end {
                break;
            }
            let link_start = position.max(range.start);
            let link_end = end.min(range.end);
            if link_start > position {
                let mut prefix = chunk.clone();
                prefix.text = substring_u16(&chunk.text, position - start, link_start - start);
                linked_chunks.push(prefix);
            }
            let mut linked = chunk.clone();
            linked.text = substring_u16(&chunk.text, link_start - start, link_end - start);
            linked.link = Some(range.url.clone());
            linked_chunks.push(linked);
            position = link_end;
            consumed_any = true;
            if position == end {
                break;
            }
            range_index += 1;
        }
        if !consumed_any {
            linked_chunks.push(chunk.clone());
        } else if position < end {
            let mut suffix = chunk.clone();
            suffix.text = substring_u16(&chunk.text, position - start, end - start);
            linked_chunks.push(suffix);
        }
    }
    linked_chunks
}

fn char_index_to_u16(chars: &[char], char_index: usize) -> usize {
    chars[..char_index.min(chars.len())]
        .iter()
        .map(|c| c.len_utf16())
        .sum()
}

fn substring_u16(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut u16_pos = 0usize;
    for &c in &chars {
        let len = c.len_utf16();
        if u16_pos >= start && u16_pos + len <= end {
            out.push(c);
        }
        u16_pos += len;
    }
    out
}

/// `content.indexOf(chunk.text, contentPos)` in UTF-16 space.
fn find_subrange(content: &str, needle: &str, from_u16: usize) -> Option<usize> {
    let content_chars: Vec<char> = content.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || needle_chars.len() > content_chars.len() {
        return None;
    }
    let mut start_char = 0usize;
    let mut u16_pos = 0usize;
    // find the char index of from_u16
    while start_char < content_chars.len() && u16_pos < from_u16 {
        u16_pos += content_chars[start_char].len_utf16();
        start_char += 1;
    }
    let mut i = start_char;
    while i + needle_chars.len() <= content_chars.len() {
        if content_chars[i..i + needle_chars.len()] == needle_chars[..] {
            let mut u = 0usize;
            for &c in &content_chars[..i] {
                u += c.len_utf16();
            }
            return Some(u);
        }
        i += 1;
    }
    None
}

/// `addMarkdownLinkHighlights` — Markdown.ts:724-790: rewrites the
/// highlight list so markdown link syntax conceals when hyperlinks are
/// unavailable (the `](url)` tail collapses onto the label).
pub fn add_markdown_link_highlights(
    highlights: Vec<SimpleHighlight>,
    content: &str,
    conceal: bool,
    hyperlinks_supported: bool,
) -> Vec<SimpleHighlight> {
    let has_url = highlights
        .iter()
        .any(|h| h.group == "markup.link.url" || h.group == "string.special.url");
    if !has_url {
        return highlights;
    }

    let map = crate::tree_sitter::Utf16Map::new(content);
    let char_at = |i: usize| content[map.byte_of_utf16(i)..].chars().next();
    let slice = |a: usize, b: usize| map.slice(content, a, b).to_owned();

    let mut modified = highlights.clone();
    let mut labels: std::collections::HashMap<usize, (usize, usize)> =
        std::collections::HashMap::new(); // end -> (start, index)
    let mut bracket_conceals: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new(); // start -> index
    let mut closings: Vec<usize> = Vec::new(); // indexes of ")" markup.link

    for (index, highlight) in highlights.iter().enumerate() {
        if highlight.group == "markup.link.label"
            && highlight.start < highlight.end
            && !labels.contains_key(&highlight.end)
        {
            labels.insert(highlight.end, (highlight.start, index));
        }
        if highlight.end == highlight.start + 1
            && highlight.meta.conceal.as_deref() == Some(" ")
            && !bracket_conceals.contains_key(&highlight.start)
        {
            bracket_conceals.insert(highlight.start, index);
        }
        if highlight.group == "markup.link" && slice(highlight.start, highlight.end) == ")" {
            closings.push(index);
        }
    }

    let max_link_bytes = 512; // MAX_LINK_URL_BYTES (zig.ts:105)
    let is_supported_target = |url: &str| url.len() <= max_link_bytes;

    let mut closing_index = 0usize;
    for index in 0..highlights.len() {
        let (start, end, group) = (
            highlights[index].start,
            highlights[index].end,
            highlights[index].group.clone(),
        );
        if group != "markup.link.url" && group != "string.special.url" {
            continue;
        }
        let mut url_start = start;
        let mut url_end = end;
        if char_at(start) == Some('<') && char_at(end.wrapping_sub(1)) == Some('>') {
            url_start += 1;
            url_end -= 1;
            modified[index] = SimpleHighlight {
                start: url_start,
                end: url_end,
                group: group.clone(),
                meta: highlights[index].meta.clone(),
            };
            if conceal {
                modified.push(SimpleHighlight {
                    start,
                    end: start + 1,
                    group: "conceal".into(),
                    meta: HighlightMeta {
                        conceal: Some(String::new()),
                        is_injection: true,
                        ..Default::default()
                    },
                });
                modified.push(SimpleHighlight {
                    start: end - 1,
                    end,
                    group: "conceal".into(),
                    meta: HighlightMeta {
                        conceal: Some(String::new()),
                        is_injection: true,
                        ..Default::default()
                    },
                });
            }
        }

        let mut destination_start = start;
        while destination_start > 0
            && char_at(destination_start - 1)
                .map(|c| c.is_whitespace())
                .unwrap_or(false)
        {
            destination_start -= 1;
        }
        if !conceal
            || char_at(destination_start.wrapping_sub(1)) != Some('(')
            || destination_start < 2
            || char_at(destination_start - 2) != Some(']')
        {
            continue;
        }
        let Some(&(label_start, label_end)) = labels.get(&(destination_start - 2)) else {
            continue;
        };
        let destination = slice(url_start, url_end);
        let url = if group == "markup.link.url" {
            normalize_markdown_link_target(&destination)
        } else {
            destination.clone()
        };
        if (!hyperlinks_supported || !is_supported_target(&url))
            && slice(label_start, label_end) != url
        {
            continue;
        }
        while closing_index < closings.len() && highlights[closings[closing_index]].end < end {
            closing_index += 1;
        }
        let Some(&close) = closings.get(closing_index) else {
            continue;
        };
        if let Some(&bracket_index) = bracket_conceals.get(&(destination_start - 2)) {
            let meta = modified[bracket_index].meta.clone();
            modified[bracket_index] = SimpleHighlight {
                start: highlights[bracket_index].start,
                end: highlights[bracket_index].end,
                group: highlights[bracket_index].group.clone(),
                meta: HighlightMeta {
                    conceal: Some(String::new()),
                    ..meta
                },
            };
        }
        modified.push(SimpleHighlight {
            start: destination_start - 1,
            end: highlights[close].end,
            group: "conceal".into(),
            meta: HighlightMeta {
                conceal: Some(String::new()),
                is_injection: true,
                ..Default::default()
            },
        });
    }
    modified
}
