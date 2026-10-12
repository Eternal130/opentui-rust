//! `TextNodeRenderable` — port of
//! `packages/core/src/renderables/TextNode.ts:34-361`.
//!
//! A rich-text tree: children are plain strings or nested nodes; each
//! node carries optional fg/bg/attributes/link that **merge down** the
//! tree (`mergeStyles`, TextNode.ts:226-238: child value wins for
//! colors/link, attributes OR). `gatherWithInheritedStyle`
//! (TextNode.ts:240-270) flattens the tree into [`TextChunk`]s in
//! document order — the content model `TextRenderable` rasterizes when
//! the tree (not a manual `content`) owns the text (Text.ts:85-97).
//!
//! Ownership note: upstream nodes have `parent` backlinks and
//! reparent on insert. The Rust port keeps the same observable
//! semantics with owned `Box` children: `insert_before`/`remove` take
//! unique child ids (the harness address), and moving a subtree
//! between parents is an ownership move rather than a pointer splice.

use crate::color::{parse_color, Rgba};
use crate::styled_text::TextChunk;

/// The merged style a node inherits + its own — `TextNodeOptions`
/// (TextNode.ts:8-13) and the gather style state (TextNode.ts:226).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeStyle {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
}

/// A child entry — string or node (TextNode.ts:41).
#[derive(Debug, Clone)]
pub enum TextChild {
    Text(String),
    Node(TextNodeRenderable),
}

/// `TextNodeRenderable` (TextNode.ts:34). `id` is the user-facing
/// identity used by `insertBefore`/`remove` (upstream uses object
/// identity; the port uses ids because scenarios address nodes by id).
#[derive(Debug, Clone)]
pub struct TextNodeRenderable {
    pub id: String,
    fg: Option<Rgba>,
    bg: Option<Rgba>,
    attributes: u32,
    link: Option<String>,
    children: Vec<TextChild>,
}

impl TextNodeRenderable {
    /// Constructor (TextNode.ts:44-51): options parsed, attributes
    /// default 0.
    pub fn new(id: impl Into<String>, options: TextNodeOptions) -> Self {
        TextNodeRenderable {
            id: id.into(),
            fg: options.fg.as_deref().map(parse_color),
            bg: options.bg.as_deref().map(parse_color),
            attributes: options.attributes.unwrap_or(0),
            link: options.link,
            children: Vec::new(),
        }
    }

    /// `fromString` (TextNode.ts:272-276).
    pub fn from_string(id: impl Into<String>, text: &str, options: TextNodeOptions) -> Self {
        let mut node = Self::new(id, options);
        node.add_text(text);
        node
    }

    /// `children` getter (TextNode.ts:53-55).
    #[must_use]
    pub fn children(&self) -> &[TextChild] {
        &self.children
    }

    /// `getChildrenCount` (TextNode.ts:300-302).
    #[must_use]
    pub fn children_count(&self) -> usize {
        self.children.len()
    }

    /// `add(string)` (TextNode.ts:82-94): appends, returns the index.
    pub fn add_text(&mut self, text: &str) -> usize {
        let index = self.children.len();
        self.children.push(TextChild::Text(text.to_owned()));
        index
    }

    /// `add(TextNodeRenderable)` (TextNode.ts:96-102): reparents the
    /// child (removing it from its current parent first — the caller
    /// drives the move, matching `prepareChildInsert` 123-137).
    pub fn add_node(&mut self, node: TextNodeRenderable) -> usize {
        let index = self.children.len();
        self.children.push(TextChild::Node(node));
        index
    }

    /// `insertBefore` (TextNode.ts:162-196): insert `child` before the
    /// anchor node (by id). Returns the insertion index.
    pub fn insert_before(&mut self, child: TextChild, anchor_id: &str) -> usize {
        let Some(anchor_index) = self.children.iter().position(|c| match c {
            TextChild::Node(n) => n.id == anchor_id,
            _ => false,
        }) else {
            panic!("Anchor node not found in children: {anchor_id}");
        };
        self.children.insert(anchor_index, child);
        anchor_index
    }

    /// `remove(child)` (TextNode.ts:198-214): removes the child node
    /// with the given id; unknown ids are a no-op (upstream warns).
    pub fn remove(&mut self, child_id: &str) -> bool {
        let Some(index) = self.children.iter().position(|c| match c {
            TextChild::Node(n) => n.id == child_id,
            _ => false,
        }) else {
            return false;
        };
        self.children.remove(index);
        true
    }

    /// `clear` (TextNode.ts:216-224).
    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// `mergeStyles` (TextNode.ts:226-238): own style wins over the
    /// parent's for fg/bg/link; attributes OR.
    #[must_use]
    pub fn merge_styles(
        &self,
        parent: &NodeStyle,
        parent_link: Option<&str>,
    ) -> (NodeStyle, Option<String>) {
        (
            NodeStyle {
                fg: self.fg.or(parent.fg),
                bg: self.bg.or(parent.bg),
                attributes: self.attributes | parent.attributes,
            },
            self.link
                .clone()
                .or_else(|| parent_link.map(std::borrow::ToOwned::to_owned)),
        )
    }

    /// `gatherWithInheritedStyle` (TextNode.ts:240-270): depth-first
    /// flatten into chunks, strings carrying the merged style.
    #[must_use]
    pub fn gather_with_inherited_style(
        &self,
        parent: &NodeStyle,
        parent_link: Option<&str>,
    ) -> Vec<TextChunk> {
        let (style, link) = self.merge_styles(parent, parent_link);
        let mut chunks = Vec::new();
        for child in &self.children {
            match child {
                TextChild::Text(text) => chunks.push(TextChunk {
                    text: text.clone(),
                    fg: style.fg,
                    bg: style.bg,
                    attributes: style.attributes,
                    link: link.clone(),
                }),
                TextChild::Node(node) => {
                    chunks.extend(node.gather_with_inherited_style(&style, link.as_deref()));
                }
            }
        }
        chunks
    }

    /// `toChunks` (TextNode.ts:286-294) with the zero style root call.
    #[must_use]
    pub fn to_chunks(&self) -> Vec<TextChunk> {
        self.gather_with_inherited_style(&NodeStyle::default(), None)
    }

    /// `findDescendantById` (TextNode.ts:358-360) — upstream scans
    /// direct children (`getRenderable`, 304-306); the port matches
    /// that surface (direct children only).
    #[must_use]
    pub fn get_renderable(&self, id: &str) -> Option<&TextNodeRenderable> {
        self.children.iter().find_map(|c| match c {
            TextChild::Node(n) if n.id == id => Some(n),
            _ => None,
        })
    }

    // -- setters (fg/bg/attributes/link, TextNode.ts:312-356) ----------

    pub fn set_fg(&mut self, fg: Option<&str>) {
        self.fg = fg.map(parse_color);
    }

    pub fn set_bg(&mut self, bg: Option<&str>) {
        self.bg = bg.map(parse_color);
    }

    pub fn set_attributes(&mut self, attributes: u32) {
        self.attributes = attributes;
    }

    #[must_use]
    pub fn attributes(&self) -> u32 {
        self.attributes
    }

    #[must_use]
    pub fn fg(&self) -> Option<Rgba> {
        self.fg
    }

    #[must_use]
    pub fn bg(&self) -> Option<Rgba> {
        self.bg
    }
}

/// `TextNodeOptions` (TextNode.ts:8-13) — colors as parseable strings
/// (the port's `parse_color` covers the TS `RGBA | string` inputs).
#[derive(Debug, Clone, Default)]
pub struct TextNodeOptions {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub attributes: Option<u32>,
    pub link: Option<String>,
}

/// `RootTextNodeRenderable` (TextNode.ts:363-379): the root a
/// `TextRenderable` owns — same tree, its style is the text's
/// defaults (Text.ts:34-43).
pub type RootTextNodeRenderable = TextNodeRenderable;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::text_attributes;

    #[test]
    fn gather_merges_styles_down_tree() {
        let mut root = TextNodeRenderable::new(
            "root",
            TextNodeOptions {
                attributes: Some(text_attributes::BOLD_U32),
                ..Default::default()
            },
        );
        root.add_text("plain ");
        let mut inner = TextNodeRenderable::new(
            "inner",
            TextNodeOptions {
                fg: Some("red".into()),
                attributes: Some(text_attributes::UNDERLINE_U32),
                ..Default::default()
            },
        );
        inner.add_text("hot");
        let mut leaf = TextNodeRenderable::new("leaf", TextNodeOptions::default());
        leaf.add_text("!");
        inner.add_node(leaf);
        root.add_node(inner);
        root.add_text(" tail");

        let chunks = root.to_chunks();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["plain ", "hot", "!", " tail"]);
        // root default: bold only
        assert_eq!(chunks[0].attributes, text_attributes::BOLD_U32);
        // inner adds underline + red fg on top of inherited bold
        assert_eq!(
            chunks[1].attributes,
            text_attributes::BOLD_U32 | text_attributes::UNDERLINE_U32
        );
        assert_eq!(chunks[1].fg, Some(parse_color("red")));
        // leaf inherits bold|underline + red
        assert_eq!(
            chunks[2].attributes,
            text_attributes::BOLD_U32 | text_attributes::UNDERLINE_U32
        );
        assert_eq!(chunks[2].fg, Some(parse_color("red")));
    }

    #[test]
    fn insert_before_and_remove() {
        let mut root = TextNodeRenderable::new("root", TextNodeOptions::default());
        root.add_text("a");
        let b = TextNodeRenderable::from_string("b", "b", TextNodeOptions::default());
        root.add_node(b);
        root.add_text("c");
        let d = TextNodeRenderable::from_string("d", "d", TextNodeOptions::default());
        root.insert_before(TextChild::Node(d), "b");
        assert_eq!(root.children_count(), 4);
        assert!(root.remove("b"));
        assert!(!root.remove("missing"));
        let chunks = root.to_chunks();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "d", "c"]);
    }
}
