//! `treeSitterToTextChunks` / `treeSitterToStyledText` — port of
//! `packages/core/src/lib/tree-sitter-styled-text.ts`.
//!
//! The sweep runs in **UTF-16 index space** exactly like the JS (the
//! upstream code slices `content` with the highlight offsets and reads
//! single chars like `content[offset] === "\n"`); [`Utf16Map`] provides
//! the byte↔UTF-16 boundary for slicing.

use crate::styled_text::{StyledText, TextChunk};
use crate::syntax_style::SyntaxStyle;

use super::{SimpleHighlight, TreeSitterClient, Utf16Map};

/// `TextChunkOptions` — tree-sitter-styled-text.ts:11-15.
#[derive(Debug, Default)]
pub struct TextChunkOptions<'a> {
    /// concealment enabled (`options.enabled ?? true`)
    pub enabled: Option<bool>,
    pub base_highlight: Option<&'a str>,
    /// `ranges` — rendered-chunk source ranges (for `detectLinks`).
    pub ranges: Option<&'a mut Vec<(usize, usize)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundaryType {
    Start,
    End,
}

struct Boundary {
    offset: usize,
    kind: BoundaryType,
    highlight_index: usize,
}

/// `getSpecificity` — tree-sitter-styled-text.ts:23-25.
fn specificity(group: &str) -> usize {
    group.split('.').count()
}

/// `shouldSuppressInInjection` — tree-sitter-styled-text.ts:27-38.
fn should_suppress_in_injection(group: &str, meta_injection: bool) -> bool {
    if meta_injection {
        return false;
    }
    group == "markup.raw.block"
}

/// `treeSitterToTextChunks` — tree-sitter-styled-text.ts:40-286.
#[allow(clippy::too_many_lines)] // one faithful port of the upstream sweep
pub fn tree_sitter_to_text_chunks(
    content: &str,
    highlights: &[SimpleHighlight],
    syntax_style: &SyntaxStyle,
    mut options: TextChunkOptions<'_>,
) -> Vec<TextChunk> {
    let mut chunks: Vec<TextChunk> = Vec::new();
    let conceal_enabled = options.enabled.unwrap_or(true);
    let default_style = syntax_style.get_style("default").cloned();
    let base_style = options
        .base_highlight
        .and_then(|name| syntax_style.get_style(name).cloned());

    let map = Utf16Map::new(content);
    let char_at =
        |offset: usize| -> Option<char> { content[map.byte_of_utf16(offset)..].chars().next() };

    let mut injection_container_ranges: Vec<(usize, usize)> = Vec::new();
    let mut boundaries: Vec<Boundary> = Vec::new();
    for (i, highlight) in highlights.iter().enumerate() {
        if highlight.start == highlight.end {
            continue; // zero-length ranges skipped (ts:57)
        }
        if highlight.meta.contains_injection {
            injection_container_ranges.push((highlight.start, highlight.end));
        }
        boundaries.push(Boundary {
            offset: highlight.start,
            kind: BoundaryType::Start,
            highlight_index: i,
        });
        boundaries.push(Boundary {
            offset: highlight.end,
            kind: BoundaryType::End,
            highlight_index: i,
        });
    }

    // sort by offset, ENDs before STARTs at the same offset (ts:67-72);
    // ties keep insertion order (JS sort is stable)
    boundaries.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| match (a.kind, b.kind) {
                (BoundaryType::End, BoundaryType::Start) => std::cmp::Ordering::Less,
                (BoundaryType::Start, BoundaryType::End) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
    });

    // active highlight indices in insertion order (JS Set semantics)
    let mut active: Vec<usize> = Vec::new();
    let mut current_offset = 0usize;

    fn push_range(ranges: &mut Option<&mut Vec<(usize, usize)>>, start: usize, end: usize) {
        if let Some(r) = ranges {
            r.push((start, end));
        }
    }

    for boundary in &boundaries {
        if current_offset < boundary.offset && !active.is_empty() {
            let segment_text = map
                .slice(content, current_offset, boundary.offset)
                .to_owned();

            // active groups in Set iteration order (insertion, ts:83-87)
            let active_groups: Vec<(usize, &SimpleHighlight)> = active
                .iter()
                .map(|&idx| (idx, &highlights[idx]))
                .collect::<Vec<_>>();

            // conceal priority (ts:90-95): first active with meta.conceal,
            // group "conceal" or "conceal.*"
            let conceal_highlight = if conceal_enabled {
                active_groups.iter().find(|(_, h)| {
                    h.meta.conceal.is_some()
                        || h.group == "conceal"
                        || h.group.starts_with("conceal.")
                })
            } else {
                None
            };

            if let Some((_, highlight)) = conceal_highlight {
                let mut replacement_text = String::new();
                if let Some(conceal) = &highlight.meta.conceal {
                    replacement_text = conceal.clone();
                } else if highlight.group == "conceal.with.space" {
                    replacement_text = " ".to_owned();
                }

                if !replacement_text.is_empty() {
                    let (fg, bg, attributes) = style_fields(default_style.as_ref());
                    chunks.push(TextChunk {
                        text: replacement_text,
                        fg,
                        bg,
                        attributes,
                        link: None,
                    });
                    push_range(&mut options.ranges, current_offset, boundary.offset);
                }
            } else {
                let inside_injection_container = injection_container_ranges
                    .iter()
                    .any(|(start, end)| current_offset >= *start && current_offset < *end);

                let valid_groups: Vec<&(usize, &SimpleHighlight)> = active_groups
                    .iter()
                    .filter(|(_, h)| {
                        !(inside_injection_container
                            && should_suppress_in_injection(&h.group, h.meta.is_injection))
                    })
                    .collect();

                // sort by specificity asc, then index asc (ts:143-148)
                let mut sorted_groups = valid_groups;
                sorted_groups.sort_by_key(|(index, h)| (specificity(&h.group), *index));

                // CSS-cascade merge, base style seeds (ts:152-185)
                let mut merged = base_style.clone().unwrap_or_default();
                let mut merged_any = base_style.is_some();
                for (_, highlight) in &sorted_groups {
                    let style = syntax_style.get_style(&highlight.group).or_else(|| {
                        highlight
                            .group
                            .split_once('.')
                            .and_then(|(base, _)| syntax_style.get_style(base))
                    });
                    if let Some(style) = style {
                        if style.fg.is_some() {
                            merged.fg = style.fg;
                        }
                        if style.bg.is_some() {
                            merged.bg = style.bg;
                        }
                        if style.bold.is_some() {
                            merged.bold = style.bold;
                        }
                        if style.italic.is_some() {
                            merged.italic = style.italic;
                        }
                        if style.underline.is_some() {
                            merged.underline = style.underline;
                        }
                        if style.dim.is_some() {
                            merged.dim = style.dim;
                        }
                        merged_any = true;
                    }
                }

                // finalStyle: merged when non-empty else default (ts:188)
                let final_style = if merged_any && has_any_field(&merged) {
                    Some(merged)
                } else {
                    default_style.clone()
                };
                let (fg, bg, attributes) = style_fields(final_style.as_ref());
                chunks.push(TextChunk {
                    text: segment_text,
                    fg,
                    bg,
                    attributes,
                    link: None,
                });
                push_range(&mut options.ranges, current_offset, boundary.offset);
            }
        } else if current_offset < boundary.offset {
            // no active highlights: base ?? default styling (ts:206-224)
            let style = base_style.as_ref().or(default_style.as_ref());
            let (fg, bg, attributes) = style_fields(style);
            chunks.push(TextChunk {
                text: map
                    .slice(content, current_offset, boundary.offset)
                    .to_owned(),
                fg,
                bg,
                attributes,
                link: None,
            });
            push_range(&mut options.ranges, current_offset, boundary.offset);
        }

        match boundary.kind {
            BoundaryType::Start => {
                if !active.contains(&boundary.highlight_index) {
                    active.push(boundary.highlight_index);
                }
            }
            BoundaryType::End => {
                active.retain(|&idx| idx != boundary.highlight_index);
                if conceal_enabled {
                    let highlight = &highlights[boundary.highlight_index];
                    // concealLines: swallow the following newline (ts:233-238)
                    if highlight.meta.conceal_lines.is_some()
                        && char_at(boundary.offset) == Some('\n')
                    {
                        current_offset = boundary.offset + 1;
                        continue;
                    }
                    if let Some(conceal) = &highlight.meta.conceal {
                        // " " replacement: skip the next space (ts:243-248)
                        if conceal == " " {
                            if char_at(boundary.offset) == Some(' ') {
                                current_offset = boundary.offset + 1;
                                continue;
                            }
                        }
                        // heading markers: skip the trailing space (ts:252-257)
                        else if conceal.is_empty()
                            && highlight.group == "conceal"
                            && !highlight.meta.is_injection
                            && char_at(boundary.offset) == Some(' ')
                        {
                            current_offset = boundary.offset + 1;
                            continue;
                        }
                    }
                }
            }
        }

        current_offset = boundary.offset;
    }

    // tail (ts:265-283)
    if current_offset < map.total_utf16() {
        let style = base_style.as_ref().or(default_style.as_ref());
        let (fg, bg, attributes) = style_fields(style);
        chunks.push(TextChunk {
            text: map
                .slice(content, current_offset, map.total_utf16())
                .to_owned(),
            fg,
            bg,
            attributes,
            link: None,
        });
        push_range(&mut options.ranges, current_offset, map.total_utf16());
    }

    chunks
}

fn has_any_field(style: &crate::syntax_style::StyleDefinition) -> bool {
    style.fg.is_some()
        || style.bg.is_some()
        || style.bold.is_some()
        || style.italic.is_some()
        || style.underline.is_some()
        || style.dim.is_some()
}

/// chunk fields from a style (ts:108-122 / 190-203): `createTextAttributes`
/// over the style booleans; None style → attributes 0.
fn style_fields(
    style: Option<&crate::syntax_style::StyleDefinition>,
) -> (Option<crate::color::Rgba>, Option<crate::color::Rgba>, u32) {
    match style {
        Some(style) => {
            let attributes = crate::styled_text::create_text_attributes(
                style.bold.unwrap_or(false),
                style.italic.unwrap_or(false),
                style.underline.unwrap_or(false),
                style.dim.unwrap_or(false),
                false,
                false,
                false,
                false,
            );
            (style.fg, style.bg, attributes)
        }
        None => (None, None, 0),
    }
}

/// `TreeSitterToStyledTextOptions` — tree-sitter-styled-text.ts:288-291.
#[derive(Debug, Clone, Default)]
pub struct TreeSitterToStyledTextOptions {
    pub conceal_enabled: Option<bool>,
    pub base_highlight: Option<String>,
}

/// `treeSitterToStyledText` — tree-sitter-styled-text.ts:293-320 (the
/// async client round-trip collapses: the port highlights in-process).
pub fn tree_sitter_to_styled_text(
    content: &str,
    filetype: &str,
    syntax_style: &SyntaxStyle,
    client: &mut TreeSitterClient,
    options: TreeSitterToStyledTextOptions,
) -> StyledText {
    let result = client.highlight_once(content, filetype);
    let highlights = result.highlights.unwrap_or_default();
    if !highlights.is_empty() || options.base_highlight.is_some() {
        let chunks = tree_sitter_to_text_chunks(
            content,
            &highlights,
            syntax_style,
            TextChunkOptions {
                enabled: options.conceal_enabled,
                base_highlight: options.base_highlight.as_deref(),
                ranges: None,
            },
        );
        StyledText::new(chunks)
    } else {
        // default merged style for the whole content (ts:308-318)
        let default_style = syntax_style.merge_styles(&["default"]);
        StyledText::new(vec![TextChunk {
            text: content.to_owned(),
            fg: default_style.fg,
            bg: default_style.bg,
            attributes: default_style.attributes,
            link: None,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax_style::StyleDefinitionInput;

    fn style_with_default() -> SyntaxStyle {
        let mut style = SyntaxStyle::create();
        style.register_style(
            "default",
            &StyleDefinitionInput {
                fg: Some("#FFFFFF".into()),
                ..Default::default()
            },
        );
        style.register_style(
            "markup.strong",
            &StyleDefinitionInput {
                fg: Some("#FF0000".into()),
                bold: Some(true),
                ..Default::default()
            },
        );
        style
    }

    #[test]
    fn plain_text_without_highlights() {
        let style = style_with_default();
        let chunks = tree_sitter_to_text_chunks("hello", &[], &style, TextChunkOptions::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello");
        assert_eq!(chunks[0].fg, Some(crate::color::parse_color("#FFFFFF")));
    }

    #[test]
    fn highlight_segment_split_and_merge() {
        let style = style_with_default();
        // "ab" default + "cd" strong + "e" default
        let highlights = vec![SimpleHighlight {
            start: 2,
            end: 4,
            group: "markup.strong".into(),
            meta: super::super::HighlightMeta::default(),
        }];
        let chunks =
            tree_sitter_to_text_chunks("abcde", &highlights, &style, TextChunkOptions::default());
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "ab");
        assert_eq!(chunks[1].text, "cd");
        assert_eq!(chunks[1].fg, Some(crate::color::parse_color("#FF0000")));
        assert_eq!(chunks[2].text, "e");
    }

    #[test]
    fn empty_conceal_drops_text() {
        let style = style_with_default();
        let highlights = vec![SimpleHighlight {
            start: 0,
            end: 1,
            group: "conceal".into(),
            meta: super::super::HighlightMeta {
                conceal: Some(String::new()),
                ..Default::default()
            },
        }];
        let chunks =
            tree_sitter_to_text_chunks("# hi", &highlights, &style, TextChunkOptions::default());
        // the "#" is concealed to nothing; the trailing-space skip rule
        // fires on the END boundary (group "conceal", meta.conceal "")
        assert_eq!(chunks[0].text, "hi");
    }

    #[test]
    fn conceal_with_space_replaces() {
        let style = style_with_default();
        let highlights = vec![SimpleHighlight {
            start: 0,
            end: 1,
            group: "conceal".into(),
            meta: super::super::HighlightMeta {
                conceal: Some(" ".into()),
                ..Default::default()
            },
        }];
        let chunks =
            tree_sitter_to_text_chunks("x y", &highlights, &style, TextChunkOptions::default());
        assert_eq!(chunks[0].text, " ");
        assert_eq!(chunks[1].text, "y");
    }
}
