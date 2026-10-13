//! `TextRenderable` — `packages/core/src/renderables/Text.ts` over
//! `TextBufferRenderable` (`TextBufferRenderable.ts`).
//!
//! Defaults (TextBufferRenderable.ts:48-60): fg white `fromValues(1,1,1,1)`
//! (literal RGB intent), bg `fromValues(0,0,0,0)` (transparent black),
//! attributes 0, wrapMode `"word"`, textAlign left.
//!
//! The content model: a manual [`StyledText`] (`content` option —
//! `_hasManualStyledText`, Text.ts:32) or a [`TextNodeRenderable`]
//! tree whose gathered chunks own the text (Text.ts:85-97, ported in
//! [`crate::text_node`]). Either way the raster target is the
//! [`TextDocument`] + virtual-line view in [`crate::text_buffer`].

use crate::buffer::OptimizedBuffer;
use crate::color::Rgba;
use crate::layout::Dimension;
use crate::styled_text::{string_to_styled_text, StyledText};
use crate::text_buffer::{
    compute_virtual_lines, draw_text_view, TextAlign, TextDefaults, TextDocument, TextViewState,
    Viewport,
};
use crate::text_node::{NodeStyle, TextNodeRenderable};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    None,
    Char,
    #[default]
    Word,
}

impl WrapMode {
    #[must_use]
    pub fn parse(s: &str) -> WrapMode {
        match s {
            "none" => WrapMode::None,
            "char" => WrapMode::Char,
            _ => WrapMode::Word,
        }
    }
}

/// `content` — `TextOptions.content?: StyledText | string`. The port
/// keeps the string/object distinction because `_hasManualStyledText`
/// (Text.ts:32) is `content !== undefined && content !== ""`: a plain
/// **empty string** is NOT manual (the node tree owns the text), while
/// a `StyledText` object always is.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ContentInput {
    #[default]
    None,
    Str(String),
    Styled(StyledText),
}

impl ContentInput {
    /// `_hasManualStyledText` (Text.ts:32).
    #[must_use]
    pub fn is_manual(&self) -> bool {
        match self {
            ContentInput::None => false,
            ContentInput::Str(s) => !s.is_empty(),
            ContentInput::Styled(_) => true,
        }
    }

    /// The constructor's `styledText` (Text.ts:29-31): strings wrap via
    /// `stringToStyledText`.
    #[must_use]
    pub fn to_styled(&self) -> StyledText {
        match self {
            ContentInput::None | ContentInput::Str(_) => string_to_styled_text(match self {
                ContentInput::Str(s) => s.as_str(),
                _ => "",
            }),
            ContentInput::Styled(s) => s.clone(),
        }
    }
}

/// `TextOptions` — Text.ts:9-11 over `TextBufferOptions`
/// (TextBufferRenderable.ts:11-23).
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
    pub layout: crate::layout::LayoutOptions,
    pub width: Option<Dimension>,
    pub height: Option<Dimension>,
    /// `content` — a plain string or a multi-chunk `StyledText`.
    pub content: ContentInput,
    /// Node-tree content (`textNode.add` flows); used when `content`
    /// is not manual (Text.ts:85-97).
    pub node_tree: Option<TextNodeRenderable>,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
    pub wrap_mode: Option<WrapMode>,
    pub text_align: Option<TextAlign>,
    /// `tabIndicator?: string | number` — pre-resolved codepoint (see
    /// `tab_indicator_codepoint`).
    pub tab_indicator: Option<u32>,
    pub tab_indicator_color: Option<Rgba>,
    pub truncate: bool,
}

impl TextOptions {
    /// Plain-string convenience (the `content: string` shape).
    #[must_use]
    pub fn with_str_content(content: &str) -> Self {
        TextOptions {
            content: ContentInput::Str(content.to_owned()),
            ..Default::default()
        }
    }
}

/// The measured text the taffy leaf measures from — the native measure
/// target (`NativeMeasureTargetKind.TextBufferView` over
/// `measureForDimensions`, TextBufferRenderable.ts:402-431 +
/// native-renderable.zig:63-80).
#[derive(Debug, Clone, Default)]
pub struct MeasuredText {
    pub doc: TextDocument,
    pub wrap_mode: WrapMode,
}

/// The Text renderable's drawn state — the `TextBuffer` +
/// `TextBufferView` pair TextBufferRenderable owns.
pub struct TextRenderableState {
    pub doc: TextDocument,
    pub default_fg: Rgba,
    pub default_bg: Rgba,
    pub attributes: u32,
    pub wrap_mode: WrapMode,
    pub text_align: TextAlign,
    pub tab_indicator: Option<u32>,
    pub tab_indicator_color: Option<Rgba>,
    pub truncate: bool,
    /// `setViewport` state (TextBufferRenderable.ts:106-108 via
    /// onResize) — None until first layout.
    viewport: Option<Viewport>,
    /// virtual lines cached for (mode, wrap width)
    vlines: Vec<crate::text_buffer::VirtualLine>,
    vlines_wrap_width: Option<u32>,
    vlines_valid: bool,
}

impl TextRenderableState {
    #[must_use]
    pub fn from_options(opts: &TextOptions) -> Self {
        let default_fg = opts
            .fg
            .unwrap_or_else(|| Rgba::from_values(1.0, 1.0, 1.0, 1.0));
        let default_bg = opts
            .bg
            .unwrap_or_else(|| Rgba::from_values(0.0, 0.0, 0.0, 0.0));
        let defaults = TextDefaults {
            fg: default_fg,
            bg: default_bg,
            attributes: opts.attributes,
        };
        // effective content: manual styled content, else the node tree
        // (Text.ts:29-45 constructor + updateTextFromNodes 85-97)
        let chunks: Vec<crate::styled_text::TextChunk> = if opts.content.is_manual() {
            opts.content.to_styled().chunks
        } else {
            gather_tree(opts, &defaults)
        };
        let doc = TextDocument::from_styled(&chunks, defaults);
        TextRenderableState {
            doc,
            default_fg,
            default_bg,
            attributes: opts.attributes,
            wrap_mode: opts.wrap_mode.unwrap_or(WrapMode::Word),
            text_align: opts.text_align.unwrap_or(TextAlign::Left),
            tab_indicator: opts.tab_indicator,
            tab_indicator_color: opts.tab_indicator_color,
            truncate: opts.truncate,
            viewport: None,
            vlines: Vec::new(),
            vlines_wrap_width: None,
            vlines_valid: false,
        }
    }

    #[must_use]
    pub fn measured(&self) -> MeasuredText {
        MeasuredText {
            doc: self.doc.clone(),
            wrap_mode: self.wrap_mode,
        }
    }

    /// `onResize` → `setViewport(0, 0, width, height)`
    /// (TextBufferRenderable.ts:371-376): the viewport (clamped ≥ 1 —
    /// Renderable.ts:1114-1115) also sets the wrap width
    /// (text-buffer-view.zig:355-368).
    pub fn update_layout(&mut self, width: u32, height: u32) {
        let vp = Viewport {
            x: 0,
            y: 0,
            width: width.max(1),
            height: height.max(1),
        };
        if self.viewport != Some(vp) {
            self.viewport = Some(vp);
            self.vlines_valid = false;
        }
    }

    /// Virtual lines at the current wrap width (or unwrapped before
    /// the first layout).
    #[must_use]
    pub fn vlines(&mut self) -> &[crate::text_buffer::VirtualLine] {
        if !self.vlines_valid {
            let wrap_width = if self.wrap_mode == WrapMode::None {
                None
            } else {
                Some(self.viewport.map_or(0, |vp| vp.width))
            };
            // constructor semantics (TextBufferRenderable.ts:102-104):
            // wrap width only when > 0
            let wrap_width = wrap_width.filter(|w| *w > 0);
            self.vlines = compute_virtual_lines(&self.doc, self.wrap_mode, wrap_width);
            self.vlines_wrap_width = wrap_width;
            self.vlines_valid = true;
        }
        &self.vlines
    }

    /// Drop the vline cache (content changed through
    /// [`crate::renderer::CliRenderer::refresh_text_measure`] — the
    /// same seam that refreshes the taffy measure context).
    pub fn invalidate_vlines(&mut self) {
        self.vlines_valid = false;
    }

    /// Intrinsic size for the measure function — `measureForDimensions`
    /// (text-buffer-view.zig:1450-1510) via the native measure
    /// (native-renderable.zig:63-80): floor the available width,
    /// measure, clamp ≥ 1; an undefined/0 width measures intrinsic.
    #[must_use]
    pub fn measure(
        text: &MeasuredText,
        known_width: Option<f32>,
        available_width: f32,
    ) -> (f32, f32) {
        let avail = match known_width {
            Some(w) => w.max(0.0),
            None => {
                if available_width.is_finite() && available_width >= 0.0 {
                    available_width
                } else {
                    0.0
                }
            }
        };
        let measure_width = if avail.is_finite() && avail > 0.0 {
            avail.floor() as u32
        } else {
            0
        };
        let (w, h) = text.doc.measure(text.wrap_mode, measure_width);
        ((w.max(1)) as f32, (h.max(1)) as f32)
    }

    /// `renderSelf` (TextBufferRenderable.ts:504-511): draw the view at
    /// screen coords through `drawTextBuffer`.
    pub fn paint(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32) {
        let view = TextViewState {
            text_align: self.text_align,
            tab_indicator: self.tab_indicator,
            tab_indicator_color: self.tab_indicator_color,
            viewport: self.viewport,
        };
        // borrow the cached vlines (perf: the paint walk runs every
        // frame — cloning the line vec per text node per frame was the
        // dominant allocation in text-heavy trees); recompute into a
        // local only when the cache is cold
        let recomputed;
        let vlines: &[crate::text_buffer::VirtualLine] = if self.vlines_valid {
            &self.vlines
        } else {
            let wrap_width = if self.wrap_mode == WrapMode::None {
                None
            } else {
                Some(self.viewport.map_or(0, |vp| vp.width))
            };
            let wrap_width = wrap_width.filter(|w| *w > 0);
            recomputed = compute_virtual_lines(&self.doc, self.wrap_mode, wrap_width);
            &recomputed
        };
        // `truncate: true` (setTruncate + ensureTruncation,
        // text-buffer-view.zig:1290-1445) is a scrollbox-lane target;
        // vlines that fit the viewport need no truncation pass.
        draw_text_view(buffer, &self.doc, &view, vlines, screen_x, screen_y);
    }
}

/// `updateTextFromNodes` (Text.ts:85-97): the gathered tree chunks
/// inherit the text's defaults as the root style (Text.ts:34-43).
fn gather_tree(opts: &TextOptions, defaults: &TextDefaults) -> Vec<crate::styled_text::TextChunk> {
    match &opts.node_tree {
        Some(tree) => tree.gather_with_inherited_style(
            &NodeStyle {
                fg: Some(defaults.fg),
                bg: Some(defaults.bg),
                attributes: defaults.attributes,
            },
            None,
        ),
        None => Vec::new(),
    }
}
