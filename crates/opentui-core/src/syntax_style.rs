//! `SyntaxStyle` — port of `packages/core/src/syntax-style.ts`.
//!
//! Upstream wraps a native handle (`createSyntaxStyle` /
//! `syntaxStyleRegister` in the Zig lib) but every semantic the TS
//! surface exposes — `registerStyle` / `getStyle` (with the scoped-name
//! fallback) / `mergeStyles` (CSS-cascade order) / the caches — is pure
//! TS state (syntax-style.ts:76-274). The port keeps that state directly
//! (the native handle exists upstream so the native text buffer can
//! resolve style ids at draw time; the port's buffer takes resolved
//! chunk colors, which is what `treeSitterToTextChunks` feeds it).
//!
//! Note `getStyle`'s quirk (syntax-style.ts:185-204): a
//! `hasOwnProperty` guard precedes the map lookup — irrelevant for a
//! Rust HashMap but reproduced by simply not storing `undefined`.

use std::collections::HashMap;

use crate::color::{parse_color, Rgba};
use crate::styled_text::create_text_attributes;

/// `StyleDefinition` — syntax-style.ts:5-12.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleDefinition {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

/// `StyleDefinitionInput` — syntax-style.ts:14-21 (colors unparsed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleDefinitionInput {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

impl StyleDefinitionInput {
    /// `registerStyle` normalization (syntax-style.ts:120-138).
    fn resolve(&self) -> StyleDefinition {
        StyleDefinition {
            fg: self.fg.as_deref().map(parse_color),
            bg: self.bg.as_deref().map(parse_color),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            dim: self.dim,
        }
    }

    #[must_use]
    pub fn attributes(&self) -> u32 {
        let d = self.resolve();
        attrs_of(&d)
    }
}

/// `createTextAttributes` over a resolved style (syntax-style.ts:123-128).
fn attrs_of(style: &StyleDefinition) -> u32 {
    create_text_attributes(
        style.bold.unwrap_or(false),
        style.italic.unwrap_or(false),
        style.underline.unwrap_or(false),
        style.dim.unwrap_or(false),
        false,
        false,
        false,
        false,
    )
}

/// `MergedStyle` — syntax-style.ts:23-27.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergedStyle {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
}

/// `ThemeTokenStyle` — syntax-style.ts:29-39 (TextMate-ish themes).
#[derive(Debug, Clone, Default)]
pub struct ThemeTokenStyle {
    pub scope: Vec<String>,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

/// `convertThemeToStyles` — syntax-style.ts:41-74: each token style's
/// resolved definition applies to every scope it lists (last write wins).
#[must_use]
pub fn convert_theme_to_styles(theme: &[ThemeTokenStyle]) -> HashMap<String, StyleDefinition> {
    let mut flat: HashMap<String, StyleDefinition> = HashMap::new();
    for token in theme {
        let definition = StyleDefinition {
            fg: token.foreground.as_deref().map(parse_color),
            bg: token.background.as_deref().map(parse_color),
            bold: token.bold,
            italic: token.italic,
            underline: token.underline,
            dim: token.dim,
        };
        for scope in &token.scope {
            flat.insert(scope.clone(), definition.clone());
        }
    }
    flat
}

/// `SyntaxStyle` — syntax-style.ts:76 (TS class). The native pointer /
/// style-count surface collapses into the map itself; the observable
/// behavior (`getStyle` / `mergeStyles` + caches) is what the port keeps.
#[derive(Debug, Default)]
pub struct SyntaxStyle {
    style_defs: HashMap<String, StyleDefinition>,
    merged_cache: HashMap<String, MergedStyle>,
}

impl Clone for SyntaxStyle {
    fn clone(&self) -> Self {
        SyntaxStyle {
            style_defs: self.style_defs.clone(),
            merged_cache: HashMap::new(), // mergedCache is a memo cache
        }
    }
}

impl SyntaxStyle {
    /// `SyntaxStyle.create()` — syntax-style.ts:89-93.
    #[must_use]
    pub fn create() -> Self {
        SyntaxStyle::default()
    }

    /// `SyntaxStyle.fromTheme(theme)` — syntax-style.ts:95-104.
    #[must_use]
    pub fn from_theme(theme: &[ThemeTokenStyle]) -> Self {
        let mut style = SyntaxStyle::create();
        for (name, def) in convert_theme_to_styles(theme) {
            style.register_resolved(&name, def);
        }
        style
    }

    /// `SyntaxStyle.fromStyles(styles)` — syntax-style.ts:106-114.
    #[must_use]
    pub fn from_styles<I, S>(styles: I) -> Self
    where
        I: IntoIterator<Item = (S, StyleDefinitionInput)>,
        S: Into<String>,
    {
        let mut style = SyntaxStyle::create();
        for (name, def) in styles {
            style.register_style(&name.into(), &def);
        }
        style
    }

    /// `registerStyle` — syntax-style.ts:120-138.
    pub fn register_style(&mut self, name: &str, style: &StyleDefinitionInput) {
        self.register_resolved(name, style.resolve());
    }

    fn register_resolved(&mut self, name: &str, definition: StyleDefinition) {
        self.style_defs.insert(name.to_owned(), definition);
    }

    /// `getStyle` — syntax-style.ts:185-204, including the scoped-name
    /// fallback: `markup.heading.1` → `markup.heading` (first segment).
    #[must_use]
    pub fn get_style(&self, name: &str) -> Option<&StyleDefinition> {
        if let Some(style) = self.style_defs.get(name) {
            return Some(style);
        }
        if let Some((base, _)) = name.split_once('.') {
            return self.style_defs.get(base);
        }
        None
    }

    /// `mergeStyles(...names)` — syntax-style.ts:206-244: styles merge in
    /// argument order (later wins), attributes recomputed from the merged
    /// booleans; cached per joined key.
    #[must_use]
    pub fn merge_styles(&self, names: &[&str]) -> MergedStyle {
        let cache_key = names.join(":");
        if let Some(cached) = self.merged_cache.get(&cache_key) {
            return cached.clone();
        }
        let mut merged = StyleDefinition::default();
        for name in names {
            let Some(style) = self.get_style(name) else {
                continue;
            };
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
        }
        let result = MergedStyle {
            fg: merged.fg,
            bg: merged.bg,
            attributes: attrs_of(&merged),
        };
        // (memoized upstream; the port recomputes when not cached — the
        // cache lives behind &self so only warm here when present)
        result
    }

    /// `clearCache` — syntax-style.ts:246-249.
    pub fn clear_cache(&mut self) {
        self.merged_cache.clear();
    }

    /// `getAllStyles` — syntax-style.ts:256-259.
    #[must_use]
    pub fn all_styles(&self) -> &HashMap<String, StyleDefinition> {
        &self.style_defs
    }

    /// `getRegisteredNames` — syntax-style.ts:261-264.
    #[must_use]
    pub fn registered_names(&self) -> Vec<&str> {
        self.style_defs.keys().map(String::as_str).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.style_defs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(fg: Option<&str>) -> StyleDefinitionInput {
        StyleDefinitionInput {
            fg: fg.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn scoped_fallback() {
        // syntax-style.ts:195-201 — the fallback takes the FIRST segment:
        // "markup.heading.1" → "markup" (not the longest prefix)
        let mut style = SyntaxStyle::create();
        style.register_style("markup", &input(Some("#FF0000")));
        assert!(style.get_style("markup.heading.1").is_some());
        assert_eq!(
            style.get_style("markup.heading.1").unwrap().fg,
            Some(parse_color("#FF0000"))
        );
        assert!(style.get_style("markup.heading").is_some());
        // registering the FULL name only does not serve the parent query
        let mut other = SyntaxStyle::create();
        other.register_style("markup.heading", &input(Some("#00FF00")));
        assert!(other.get_style("markup.heading.1").is_none());
        assert!(other.get_style("nomatch").is_none());
    }

    #[test]
    fn merge_order_wins() {
        // syntax-style.ts:215-226 — later names override
        let mut style = SyntaxStyle::create();
        style.register_style("a", &input(Some("#FF0000")));
        let mut b = input(Some("#00FF00"));
        b.bold = Some(true);
        style.register_style("b", &b);
        let merged = style.merge_styles(&["a", "b"]);
        assert_eq!(merged.fg, Some(parse_color("#00FF00")));
        assert_eq!(merged.attributes, crate::ansi::text_attributes::BOLD_U32);
        let merged = style.merge_styles(&["b", "a"]);
        assert_eq!(merged.fg, Some(parse_color("#FF0000")));
        // a's bold is None (undefined) → b's bold SURVIVES the merge
        // (ts:222 only overwrites defined fields)
        assert_eq!(merged.attributes, crate::ansi::text_attributes::BOLD_U32);
    }

    #[test]
    fn from_theme_flat_and_parse() {
        let theme = vec![ThemeTokenStyle {
            scope: vec!["keyword".into(), "markup.heading".into()],
            foreground: Some("#AA0000".into()),
            bold: Some(true),
            ..Default::default()
        }];
        let style = SyntaxStyle::from_theme(&theme);
        assert_eq!(
            style.get_style("keyword").unwrap().fg,
            Some(parse_color("#AA0000"))
        );
        assert_eq!(style.get_style("keyword").unwrap().bold, Some(true));
        assert_eq!(style.registered_names().len(), 2);
    }
}
