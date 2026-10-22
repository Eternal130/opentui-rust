//! The renderer — `createCliRenderer` / `CliRenderer`
//! (`packages/core/src/renderer.ts:723-748`, native `renderer.zig`).
//!
//! The slice implements the **headless render target**: explicit width/
//! height (the harness profile — `createTestRenderer` passes fake streams
//! with `columns`/`rows`, renderer.ts:733-734), no terminal I/O. The frame
//! lifecycle mirrors upstream:
//!
//! 1. clear `nextRenderBuffer` with `backgroundColor` (default transparent
//!    black `fromInts(0,0,0,0)` — renderer.ts:819; cleared per-frame at the
//!    end of native `prepareRenderFrame`, renderer.zig:3052),
//! 2. `root.render(nextRenderBuffer)` — layout pass + paint
//!    (renderer.ts:4726, RootRenderable.render:1789+),
//! 3. swap current/next (the diff loop syncs current=next,
//!    renderer.zig:2833),
//! 4. [`CliRenderer::render_frame_sgr`] emits the diff-based SGR byte
//!    stream (`prepareRenderFrameWithWriter`, renderer.zig:2584-2838).
//!
//! The events lane adds the interactive half: the render scheduler
//! ([`crate::scheduler`]), focus tracking
//! (`focusRenderable`/`blurRenderable`, renderer.ts:1407-1439), the
//! hit grid + mouse dispatch (`processSingleMouseEvent`,
//! renderer.ts:3632-3817), and the input feed (bytes → parsed keys →
//! the key handler → the focused renderable, the slice of
//! `StdinParser` + `handleStdinEvent`, stdin-parser.ts:1833-1849,
//! renderer.ts:3498-3545).

use crate::buffer::OptimizedBuffer;
use crate::color::parse_color;
use crate::color::Rgba;
use crate::editor::SelectionBehavior;
use crate::events::{KeyEvent, KeyHandler, MouseButton, MouseEvent, PasteEvent};
use crate::input::{InputOptions, InputRenderableState};
use crate::layout::Dimension;
use crate::parse_keypress::{parse_keypress, ParseKeypressOptions};
use crate::parse_mouse::{MouseParser, RawMouseEvent};
use crate::paste::BracketedPasteCollector;
use crate::renderable::{
    Content, MeasureCtx, MeasureKind, Renderable, RootRenderable, WalkEffects,
};
use crate::renderables::box_::{BoxOptions, BoxRenderableState};
use crate::renderables::diff;
use crate::renderables::scrollbox;
use crate::renderables::text::{TextOptions, TextRenderableState};
use crate::renderables::textarea::{TextareaOptions, TextareaState};
use std::sync::Arc;
use taffy::prelude as tp;
use taffy::tree::TaffyTree;

/// `CliRendererConfig` (renderer.ts:122-243) — the slice subset.
#[derive(Debug, Clone)]
pub struct CliRendererConfig {
    pub width: u32,
    pub height: u32,
    /// renderer.ts:217 `backgroundColor`
    pub background_color: Rgba,
    /// `targetFps` (renderer.ts:169-172; `|| 30` at 1196)
    pub target_fps: f64,
    /// `maxFps` (renderer.ts:174-177; `|| 60` at 1197)
    pub max_fps: f64,
    /// `autoFocus` (renderer.ts:198, 1223) — focus the nearest
    /// focusable renderable on left click.
    pub auto_focus: bool,
    /// `useMouse` (renderer.ts:205-206, 1222).
    pub use_mouse: bool,
    /// `useKittyKeyboard` (renderer.ts:214) — `None` disables the
    /// kitty protocol; `Some(options)` enables it (the parser's
    /// `useKittyForParsing = kittyConfig !== null`, renderer.ts:1247).
    pub kitty_keyboard: Option<KittyKeyboardOptions>,
}

impl Default for CliRendererConfig {
    fn default() -> Self {
        CliRendererConfig {
            width: 80,
            height: 24,
            background_color: Rgba::from_ints(0, 0, 0, 0),
            target_fps: 30.0,
            max_fps: 60.0,
            auto_focus: true,
            use_mouse: true,
            kitty_keyboard: None,
        }
    }
}

/// The renderer's global selection — `Selection`
/// (lib/selection.ts:27) reduced to the keyboard-selection state: an
/// anchor pinned relative to its renderable + a focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererSelection {
    pub anchor_renderable: String,
    /// anchor relative to the renderable's layout position
    /// (`SelectionAnchor`, lib/selection.ts:5-25)
    pub anchor_relative: (i32, i32),
    /// focus in the anchor's coordinate space
    pub focus: (i32, i32),
    pub behavior: SelectionBehavior,
    pub is_start: bool,
    pub is_dragging: bool,
}

/// Terminal cursor state — `Terminal.state.cursor` (terminal.zig) as
/// the renderer sees it: position (1-based), visibility, style, color,
/// plus the emission diff cache (`lastCursor*`, renderer.zig:325, 2968+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorOutputState {
    pub x: u32,
    pub y: u32,
    pub visible: bool,
    pub style: crate::renderables::textarea::CursorStyle,
    pub blinking: bool,
    pub color: Rgba,
    // diff cache (None = never emitted)
    last_x: Option<u32>,
    last_y: Option<u32>,
    last_visible: Option<bool>,
    last_style_tag: Option<u8>,
    last_blinking: Option<bool>,
    last_color: Option<[u8; 3]>,
}

impl Default for CursorOutputState {
    fn default() -> Self {
        CursorOutputState {
            x: 1,
            y: 1,
            visible: false,
            style: crate::renderables::textarea::CursorStyle::Block,
            blinking: true,
            color: Rgba::rgb(255, 255, 255),
            last_x: None,
            last_y: None,
            last_visible: None,
            last_style_tag: None,
            last_blinking: None,
            last_color: None,
        }
    }
}

impl CursorOutputState {
    /// `Terminal.setCursorPosition` (terminal.zig:1458-1468).
    pub fn set_position(&mut self, x: u32, y: u32, visible: bool) {
        self.x = x.max(1);
        self.y = y.max(1);
        self.visible = visible;
    }

    /// `Terminal.setCursorStyle` + `setCursorColor`.
    pub fn set_style(
        &mut self,
        style: crate::renderables::textarea::CursorStyle,
        blinking: bool,
        color: Rgba,
    ) {
        self.style = style;
        self.blinking = blinking;
        self.color = color;
    }

    /// The cursor emission tail (renderer.zig:2932-3016) — appended to
    /// the frame's SGR stream. `frame_started` mirrors the cell-run
    /// emission state.
    fn emit(&mut self, out: &mut String, frame_started: bool) {
        let style_tag: u8 = match self.style {
            crate::renderables::textarea::CursorStyle::Block => 1,
            crate::renderables::textarea::CursorStyle::Line => 3,
            crate::renderables::textarea::CursorStyle::Underline => 5,
        };
        if self.visible {
            let (cr, cg, cb) = (self.color.red(), self.color.green(), self.color.blue());
            let style_changed =
                self.last_style_tag != Some(style_tag) || self.last_blinking != Some(self.blinking);
            let color_changed = self.last_color != Some([cr, cg, cb]);
            let position_changed = self.last_x != Some(self.x) || self.last_y != Some(self.y);
            let visibility_changed = self.last_visible != Some(true);
            // If any visual output was produced this frame, cursor state
            // must be restored (renderer.zig:2975-2978)
            let needs_restore = frame_started
                || style_changed
                || color_changed
                || position_changed
                || visibility_changed;
            if needs_restore {
                if color_changed {
                    out.push_str(&format!("\x1b]12;#{cr:02x}{cg:02x}{cb:02x}\x07"));
                    self.last_color = Some([cr, cg, cb]);
                }
                if style_changed {
                    // DECSCUSR: 1/2 block, 3/4 line, 5/6 underline (blink/steady)
                    let code = style_tag + u8::from(!self.blinking);
                    out.push_str(&format!("\x1b[{code} q"));
                    self.last_style_tag = Some(style_tag);
                    self.last_blinking = Some(self.blinking);
                }
                out.push_str(&format!("\x1b[{};{}H", self.y, self.x));
                out.push_str("\x1b[?25h");
            }
            self.last_x = Some(self.x);
            self.last_y = Some(self.y);
            self.last_visible = Some(true);
        } else {
            if !frame_started && self.last_visible.is_none_or(|v| v) {
                out.push_str("\x1b[?25l");
            }
            self.last_style_tag = None;
            self.last_blinking = None;
            self.last_color = None;
            self.last_x = None;
            self.last_y = None;
            self.last_visible = Some(false);
        }
    }
}

/// Where output goes — the headless slice has two targets: the cell grid
/// (differential harness) and an in-memory SGR byte stream.
pub enum RenderTarget {
    Headless,
}

/// `KittyKeyboardOptions` (renderer.ts:597-608).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyKeyboardOptions {
    /// Disambiguate escape codes. Default: true.
    pub disambiguate: Option<bool>,
    /// Report alternate keys. Default: true.
    pub alternate_keys: Option<bool>,
    /// Report event types (press/repeat/release). Default: false.
    pub events: Option<bool>,
    /// Report all keys as escape codes. Default: false.
    pub all_keys_as_escapes: Option<bool>,
    /// Report text associated with key events. Default: false.
    pub report_text: Option<bool>,
}

// The kitty protocol's progressive-enhancement bits (renderer.ts:584-588)
const KITTY_FLAG_DISAMBIGUATE: u32 = 0b1;
const KITTY_FLAG_EVENT_TYPES: u32 = 0b10;
const KITTY_FLAG_ALTERNATE_KEYS: u32 = 0b100;
const KITTY_FLAG_ALL_KEYS_AS_ESCAPES: u32 = 0b1000;
const KITTY_FLAG_REPORT_TEXT: u32 = 0b10000;

/// `buildKittyKeyboardFlags` (renderer.ts:616-651): disambiguate and
/// alternateKeys default ON (disabled only when explicitly `false`);
/// the rest default OFF (enabled only when explicitly `true`).
/// Returns 0 for `None` (protocol disabled).
#[must_use]
pub fn build_kitty_keyboard_flags(config: Option<&KittyKeyboardOptions>) -> u32 {
    let Some(config) = config else {
        return 0;
    };
    let mut flags = 0u32;
    if config.disambiguate != Some(false) {
        flags |= KITTY_FLAG_DISAMBIGUATE;
    }
    if config.alternate_keys != Some(false) {
        flags |= KITTY_FLAG_ALTERNATE_KEYS;
    }
    if config.events == Some(true) {
        flags |= KITTY_FLAG_EVENT_TYPES;
    }
    if config.all_keys_as_escapes == Some(true) {
        flags |= KITTY_FLAG_ALL_KEYS_AS_ESCAPES;
    }
    if config.report_text == Some(true) {
        flags |= KITTY_FLAG_REPORT_TEXT;
    }
    flags
}

/// `CliRenderEvents` (renderer.ts:750-767) — the renderer's own event
/// stream (the slice the input lane emits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliRenderEvent {
    /// `FRAME` — `{ frameId }` (renderer.ts:4761-4765).
    Frame {
        frame_id: u64,
    },
    /// `FOCUSED_RENDERABLE` — (current, previous) by renderable id
    /// (renderer.ts:1423).
    FocusedRenderable {
        current: Option<String>,
        previous: Option<String>,
    },
    /// `FOCUSED_EDITOR` — (current, previous) by editor id
    /// (renderer.ts:1419-1421); editors are the EditBuffer family
    /// (Input counts).
    FocusedEditor {
        current: Option<String>,
        previous: Option<String>,
    },
    /// `FOCUS` / `BLUR` — terminal focus sequences `[I` / `[O`
    /// (renderer.ts:3446-3471).
    TerminalFocus,
    TerminalBlur,
}

/// One hit-grid rect, in paint order (later rects win).
#[derive(Debug, Clone)]
struct HitRect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    id: String,
}

/// A per-renderable mouse handler (the `onMouseDown`/`processMouseEvent`
/// side of the tree — Renderable.ts mouse event plumbing).
pub type MouseEventHandler = Box<dyn FnMut(&mut MouseEvent) + Send>;

type RendererEventListener = Box<dyn FnMut(&CliRenderEvent) + Send>;

/// `CliRenderer` — the render loop owner.
pub struct CliRenderer {
    pub config: CliRendererConfig,
    pub tree: TaffyTree<MeasureCtx>,
    pub root: RootRenderable,
    pub next_render_buffer: OptimizedBuffer,
    pub current_render_buffer: OptimizedBuffer,
    /// the global selection (renderer.ts:471 `currentSelection`)
    pub selection: Option<RendererSelection>,
    /// terminal cursor state + emission diff cache
    pub cursor_state: CursorOutputState,
    /// SGR bytes emitted by the last [`CliRenderer::render`]
    last_frame_sgr: String,
    /// every frame's SGR concatenated — the full terminal emission
    /// (upstream's stdout stream; the differential harness replays it)
    sgr_history: String,
    frame_id: u64,
    /// scrollbox root id → content node id (`ScrollBox.add` routes user
    /// children into the content viewport, ScrollBox.ts:514-516)
    content_of: std::collections::HashMap<String, String>,
    // ----- events lane -----
    /// `_keyHandler` (renderer.ts:861, 1248) — the priority key
    /// dispatcher.
    pub key_input: Arc<KeyHandler>,
    /// `currentFocusedRenderable` id (renderer.ts:935, 1382).
    focused: Option<String>,
    /// hit grid in paint order (renderer.ts:1448-1453 + native)
    hit_grid: Vec<HitRect>,
    /// mouse handlers per renderable id
    mouse_handlers: HashMap<String, MouseEventHandler>,
    /// renderer event listeners (CliRenderEvents)
    event_listeners: Vec<RendererEventListener>,
    /// `capturedRenderable` id (renderer.ts:3765-3792)
    captured: Option<String>,
    /// `lastOverRenderableNum` (renderer.ts:3678-3679) — the
    /// same-element check for over/out
    last_over: Option<String>,
    /// `lastOverRenderable` (renderer.ts:3754) — the renderable that
    /// last received `over` (the `out` target)
    last_over_hover: Option<String>,
    /// pointer tracking for `recheckHoverState` (renderer.ts:3824-3863)
    latest_pointer: (i32, i32),
    has_pointer: bool,
    /// the mouse parser half of StdinParser
    mouse_parser: MouseParser,
    /// incomplete-unit bytes carried across feeds (the StdinParser's
    /// single pending unit: lone ESC, split UTF-8, unterminated OSC)
    feed_pending: Vec<u8>,
    /// bracketed paste collector (stdin-parser.ts:552+)
    paste_collector: Option<BracketedPasteCollector>,
    // ----- clock -----
    /// `performance.now()`'s timeOrigin analog — the renderer's
    /// creation instant (ms since then is the wall value).
    time_origin: std::time::Instant,
    /// fixed `performance.now` override — the test seam for upstream's
    /// monkeypatch (TimeToFirstDraw.test.ts:19-20).
    performance_now_override: Option<f64>,
}

use std::collections::HashMap;

impl Default for CliRenderer {
    fn default() -> Self {
        create_cli_renderer(CliRendererConfig::default())
    }
}

/// `createCliRenderer(config)` — renderer.ts:723-748. The upstream factory
/// awaits terminal setup; the headless port is synchronous and never
/// touches stdin/stdout.
#[must_use]
pub fn create_cli_renderer(config: CliRendererConfig) -> CliRenderer {
    let mut tree: TaffyTree<MeasureCtx> = TaffyTree::new();
    // rounding enabled by default in taffy — yoga's point-scale-1 behavior
    let root = RootRenderable::new(&mut tree, config.width, config.height);
    CliRenderer {
        config: config.clone(),
        tree,
        root,
        next_render_buffer: OptimizedBuffer::new(config.width, config.height),
        current_render_buffer: OptimizedBuffer::new(config.width, config.height),
        selection: None,
        cursor_state: CursorOutputState::default(),
        last_frame_sgr: String::new(),
        sgr_history: String::new(),
        frame_id: 0,
        content_of: std::collections::HashMap::new(),
        key_input: Arc::new(KeyHandler::new()),
        focused: None,
        hit_grid: Vec::new(),
        mouse_handlers: HashMap::new(),
        event_listeners: Vec::new(),
        captured: None,
        last_over: None,
        last_over_hover: None,
        latest_pointer: (0, 0),
        has_pointer: false,
        mouse_parser: MouseParser::new(),
        paste_collector: None,
        feed_pending: Vec::new(),
        time_origin: std::time::Instant::now(),
        performance_now_override: None,
    }
}

impl CliRenderer {
    /// `renderer.width` (ctx.width).
    #[must_use]
    pub fn width(&self) -> u32 {
        self.config.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// `renderer.root.add(child)` — mount a renderable under the root.
    pub fn add(&mut self, child: Renderable) {
        self.root.add(&mut self.tree, child);
    }

    /// Mount convenience (the reconciler path): construct + mount a text
    /// renderable in one step.
    pub fn add_text(&mut self, id: &str, opts: &TextOptions) {
        let r = self.make_text(id, opts);
        self.add(r);
    }

    /// Mount convenience: construct + mount a box renderable (borders
    /// feed the taffy node).
    pub fn add_box(&mut self, id: &str, opts: &BoxOptions) {
        let r = self.make_box(id, opts);
        self.add(r);
    }

    /// Construct a text renderable without mounting (nested mounting
    /// pairs this with [`CliRenderer::mount_child`]).
    #[must_use]
    pub fn make_text(&mut self, id: &str, opts: &TextOptions) -> Renderable {
        let state = TextRenderableState::from_options(opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
            ..Default::default()
        };
        Renderable::new(
            &mut self.tree,
            id,
            opts.layout.clone(),
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Text(state),
            Some(ctx),
        )
    }

    /// The layout options mirror for text leaves built directly from
    /// state (width 100%, flexShrink 0 — the Code/Markdown block options).
    fn state_layout(_state: &TextRenderableState) -> crate::layout::LayoutOptions {
        crate::layout::LayoutOptions {
            flex_shrink: Some(0.0),
            ..Default::default()
        }
    }

    /// Construct a Code renderable (Code.ts:48) — a text leaf whose
    /// document is the settled highlight pipeline output
    /// ([`crate::renderables::code::build_code_document`]).
    #[must_use]
    pub fn make_code(
        &mut self,
        id: &str,
        opts: &crate::renderables::code::CodeOptions,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Renderable {
        let doc = crate::renderables::code::build_code_document(opts, syntax_style);
        let state = TextRenderableState::from_options(&crate::renderables::text::TextOptions {
            layout: crate::layout::LayoutOptions {
                flex_shrink: Some(0.0),
                margin_bottom: Some(Dimension::Points(0.0)),
                ..Default::default()
            },
            width: Some(Dimension::Percent(1.0)),
            content: crate::renderables::text::ContentInput::Styled(doc.to_styled()),
            fg: opts.fg,
            bg: opts.bg,
            ..Default::default()
        });
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
            ..Default::default()
        };
        Renderable::new(
            &mut self.tree,
            id,
            Self::state_layout(&state),
            Dimension::Percent(1.0),
            Dimension::Auto,
            Content::Text(state),
            Some(ctx),
        )
    }

    /// Construct a MarkdownRenderable's settled block tree
    /// (Markdown.ts:270 — column, flexShrink 0; blocks per
    /// [`crate::renderables::markdown::build_markdown_blocks`]).
    #[must_use]
    pub fn make_markdown(
        &mut self,
        id: &str,
        opts: &crate::renderables::markdown::MarkdownOptions,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Renderable {
        let mut root = Renderable::new(
            &mut self.tree,
            id,
            crate::layout::LayoutOptions {
                flex_shrink: Some(0.0),
                ..Default::default()
            },
            Dimension::Auto,
            Dimension::Auto,
            Content::Group,
            None,
        );
        let blocks = crate::renderables::markdown::build_markdown_blocks(opts, syntax_style);
        for (index, block) in blocks.into_iter().enumerate() {
            let child = self.make_block_spec(id, index, block, syntax_style);
            root.add(&mut self.tree, child);
        }
        root
    }

    fn make_block_spec(
        &mut self,
        id: &str,
        index: usize,
        block: crate::renderables::markdown::BlockSpec,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Renderable {
        use crate::renderables::markdown::BlockSpec;
        match block {
            BlockSpec::MarkdownText {
                options,
                margin_bottom,
            }
            | BlockSpec::CodeBlock {
                options,
                margin_bottom,
            } => {
                let layout = crate::layout::LayoutOptions {
                    flex_shrink: Some(0.0),
                    margin_bottom: Some(Dimension::Points(margin_bottom as f32)),
                    ..Default::default()
                };
                let state =
                    TextRenderableState::from_options(&crate::renderables::text::TextOptions {
                        layout: layout.clone(),
                        width: Some(Dimension::Percent(1.0)),
                        content: crate::renderables::text::ContentInput::Styled(
                            crate::renderables::code::build_code_document(&options, syntax_style)
                                .to_styled(),
                        ),
                        fg: options.fg,
                        bg: options.bg,
                        ..Default::default()
                    });
                let ctx = MeasureCtx {
                    kind: MeasureKind::Text,
                    text: Some(state.measured()),
                    ..Default::default()
                };
                Renderable::new(
                    &mut self.tree,
                    &format!("{id}-block-{index}"),
                    layout,
                    Dimension::Percent(1.0),
                    Dimension::Auto,
                    Content::Text(state),
                    Some(ctx),
                )
            }
            BlockSpec::Blockquote {
                content,
                border_color,
                margin_bottom,
            } => {
                let mut box_opts = BoxOptions {
                    border: Some(crate::renderables::box_::BorderSpec::Sides(vec![
                        "left".to_owned()
                    ])),
                    border_color: border_color.map(crate::color::rgb_to_hex_string),
                    width: Some(Dimension::Percent(1.0)),
                    ..Default::default()
                };
                box_opts.layout.flex_shrink = Some(0.0);
                box_opts.layout.padding_left = Some(Dimension::Points(1.0));
                box_opts.layout.margin_bottom = Some(Dimension::Points(margin_bottom as f32));
                let mut quote = self.make_box(&format!("{id}-block-{index}"), &box_opts);
                let inner =
                    self.make_block_spec(&format!("{id}-block-{index}"), 0, *content, syntax_style);
                quote.add(&mut self.tree, inner);
                quote
            }
            BlockSpec::HorizontalRule {
                border_color,
                margin_bottom,
            } => {
                let mut box_opts = BoxOptions {
                    border: Some(crate::renderables::box_::BorderSpec::Sides(vec![
                        "top".to_owned()
                    ])),
                    border_color: border_color.map(crate::color::rgb_to_hex_string),
                    width: Some(Dimension::Percent(1.0)),
                    height: Some(Dimension::Points(1.0)),
                    ..Default::default()
                };
                box_opts.layout.flex_shrink = Some(0.0);
                box_opts.layout.margin_bottom = Some(Dimension::Points(margin_bottom as f32));
                self.make_box(&format!("{id}-block-{index}"), &box_opts)
            }
            BlockSpec::List {
                items,
                margin_bottom,
            } => {
                let mut list_opts = BoxOptions {
                    width: Some(Dimension::Percent(1.0)),
                    ..Default::default()
                };
                list_opts.layout.flex_direction = Some("column".into());
                list_opts.layout.flex_shrink = Some(0.0);
                list_opts.layout.margin_bottom = Some(Dimension::Points(margin_bottom as f32));
                let mut list = self.make_box(&format!("{id}-block-{index}"), &list_opts);
                for (item_index, item) in items.into_iter().enumerate() {
                    let row = self.make_list_item(
                        &format!("{id}-block-{index}"),
                        item_index,
                        item,
                        syntax_style,
                    );
                    list.add(&mut self.tree, row);
                }
                list
            }
        }
    }

    fn make_list_item(
        &mut self,
        id: &str,
        item_index: usize,
        item: crate::renderables::markdown::ListItemSpec,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Renderable {
        let item_id = format!("{id}-item-{item_index}");
        // row: width 100%, row direction, flexShrink 0
        let mut row_opts = BoxOptions {
            width: Some(Dimension::Percent(1.0)),
            ..Default::default()
        };
        row_opts.layout.flex_direction = Some("row".into());
        row_opts.layout.flex_shrink = Some(0.0);
        row_opts.layout.margin_bottom = Some(Dimension::Points(item.margin_bottom as f32));
        let mut row = self.make_box(&item_id, &row_opts);
        // marker text: width markerWidth+1 points, flexShrink 0
        let marker_opts = crate::renderables::text::TextOptions {
            layout: crate::layout::LayoutOptions {
                flex_shrink: Some(0.0),
                ..Default::default()
            },
            width: Some(Dimension::Points(item.marker_width as f32)),
            content: crate::renderables::text::ContentInput::Styled(
                crate::styled_text::StyledText::new(vec![item.marker_chunk]),
            ),
            ..Default::default()
        };
        let marker = self.make_text(&format!("{item_id}-marker"), &marker_opts);
        row.add(&mut self.tree, marker);
        // content box: column, flexGrow 1, flexShrink 1
        let mut content_opts = BoxOptions::default();
        content_opts.layout.flex_direction = Some("column".into());
        content_opts.layout.flex_grow = Some(1.0);
        content_opts.layout.flex_shrink = Some(1.0);
        let mut content = self.make_box(&format!("{item_id}-content"), &content_opts);
        for (child_index, child) in item.children.into_iter().enumerate() {
            let margin_top = item
                .child_margins_top
                .get(child_index)
                .copied()
                .unwrap_or(0);
            let child_id = format!("{item_id}-child-{child_index}");
            let rendered = match child {
                crate::renderables::markdown::BlockSpec::MarkdownText {
                    options,
                    margin_bottom,
                }
                | crate::renderables::markdown::BlockSpec::CodeBlock {
                    options,
                    margin_bottom,
                } => {
                    let layout = crate::layout::LayoutOptions {
                        flex_shrink: Some(0.0),
                        margin_top: Some(Dimension::Points(margin_top as f32)),
                        margin_bottom: Some(Dimension::Points(margin_bottom as f32)),
                        ..Default::default()
                    };
                    let state =
                        TextRenderableState::from_options(&crate::renderables::text::TextOptions {
                            layout: layout.clone(),
                            width: Some(Dimension::Percent(1.0)),
                            content: crate::renderables::text::ContentInput::Styled(
                                crate::renderables::code::build_code_document(
                                    &options,
                                    syntax_style,
                                )
                                .to_styled(),
                            ),
                            fg: options.fg,
                            bg: options.bg,
                            ..Default::default()
                        });
                    let ctx = MeasureCtx {
                        kind: MeasureKind::Text,
                        text: Some(state.measured()),
                        ..Default::default()
                    };
                    Renderable::new(
                        &mut self.tree,
                        &child_id,
                        layout,
                        Dimension::Percent(1.0),
                        Dimension::Auto,
                        Content::Text(state),
                        Some(ctx),
                    )
                }
                other => self.make_block_spec(&child_id, child_index, other, syntax_style),
            };
            content.add(&mut self.tree, rendered);
        }
        row.add(&mut self.tree, content);
        row
    }

    /// Construct a box renderable without mounting.
    #[must_use]
    pub fn make_box(&mut self, id: &str, opts: &BoxOptions) -> Renderable {
        let state = BoxRenderableState::from_options(opts);
        let mut layout = opts.layout.clone();
        state.apply_yoga_borders(&mut layout);
        Renderable::new(
            &mut self.tree,
            id,
            layout,
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Box(state),
            None,
        )
    }

    /// `performance.now()` — ms since the renderer's timeOrigin (its
    /// creation). Renderables that need the clock read it through the
    /// renderer (TimeToFirstDraw.ts:99 stamps inside `renderSelf`).
    #[must_use]
    pub fn performance_now(&self) -> f64 {
        self.performance_now_override
            .unwrap_or_else(|| self.time_origin.elapsed().as_secs_f64() * 1000.0)
    }

    /// Fix `performance.now` (the port's seam for upstream tests
    /// monkeypatching it, TimeToFirstDraw.test.ts:19-20). `None`
    /// restores the wall clock.
    pub fn set_performance_now(&mut self, fixed: Option<f64>) {
        self.performance_now_override = fixed;
    }

    /// Construct a `TimeToFirstDrawRenderable` (TimeToFirstDraw.ts:40)
    /// without mounting. The constructor's option merge
    /// (TimeToFirstDraw.ts:47-53): `width: "100%"`, `height: 1`,
    /// `flexShrink: 0`, `alignSelf: "center"`, caller options winning.
    #[must_use]
    pub fn make_time_to_first_draw(
        &mut self,
        id: &str,
        opts: &crate::renderables::time_to_first_draw::TimeToFirstDrawOptions,
    ) -> Renderable {
        let state =
            crate::renderables::time_to_first_draw::TimeToFirstDrawState::from_options(opts);
        let (layout, width, height) = opts.resolved();
        Renderable::new(
            &mut self.tree,
            id,
            layout,
            width,
            height,
            Content::TimeToFirstDraw(state),
            None,
        )
    }

    /// Construct an input renderable without mounting.
    #[must_use]
    pub fn make_input(&mut self, id: &str, opts: &InputOptions) -> Renderable {
        let state = InputRenderableState::new(opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Input,
            input_width_cols: state.edit_width_cols(),
            ..Default::default()
        };
        let width = opts
            .width
            .map(|w| Dimension::Points(w as f32))
            .unwrap_or(Dimension::Auto);
        Renderable::new(
            &mut self.tree,
            id,
            crate::layout::LayoutOptions::default(),
            width,
            Dimension::Points(1.0), // single-line constraint (Input.ts:78)
            Content::Input(state),
            Some(ctx),
        )
    }

    /// Construct a textarea renderable without mounting
    /// (EditBufferRenderable.ts:157 — `EditorView.create(editBuffer,
    /// this.width || 80, this.height || 24)`; the explicit width/height
    /// options are the renderable's numeric size).
    #[must_use]
    pub fn make_textarea(&mut self, id: &str, opts: &TextareaOptions) -> Renderable {
        let vp_w = match opts.width {
            Some(Dimension::Points(w)) => w.max(0.0) as u32,
            _ => 80,
        };
        let vp_h = match opts.height {
            Some(Dimension::Points(h)) => h.max(0.0) as u32,
            _ => 24,
        };
        let state = TextareaState::from_options(id, opts, vp_w, vp_h);
        let ctx = MeasureCtx {
            kind: MeasureKind::Editor,
            editor: Some(state.measured()),
            ..Default::default()
        };
        Renderable::new(
            &mut self.tree,
            id,
            opts.layout.clone(),
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Textarea(state),
            Some(ctx),
        )
    }

    /// Mount convenience: construct + mount an input renderable.
    pub fn add_input(&mut self, id: &str, opts: &InputOptions) {
        let r = self.make_input(id, opts);
        self.add(r);
    }

    /// Construct a `DiffRenderable`'s settled subtree (Diff.ts:110-151
    /// constructor + `buildView` Diff.ts:177-195): the column/row root
    /// carrying the rebuild state, with per-side
    /// [`crate::renderables::line_number`] wrappers (unified: one at
    /// 100%; split: two at 50% — `createOrUpdateSide` Diff.ts:427-484)
    /// or the error view (`buildErrorView` Diff.ts:310-365).
    #[must_use]
    pub fn make_diff(
        &mut self,
        id: &str,
        opts: &diff::DiffOptions,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Renderable {
        let state = crate::renderable::DiffState {
            options: opts.clone(),
            view: opts.view,
            aligned: false,
            last_code_widths: [0.0, 0.0],
            hunk_start_lines: Vec::new(),
        };
        let mut root = Renderable::new(
            &mut self.tree,
            id,
            crate::layout::LayoutOptions {
                flex_direction: Some(
                    if opts.view == diff::DiffView::Split {
                        "row"
                    } else {
                        "column"
                    }
                    .to_owned(),
                ),
                ..Default::default()
            },
            opts.width,
            opts.height,
            Content::Diff(state),
            None,
        );
        let build = diff::build_diff_view(opts, None);
        for (index, side) in build.sides.iter().enumerate() {
            let child = self.make_diff_side(
                &format!("{id}-{}", if index == 0 { "left" } else { "right" }),
                side,
                opts,
                syntax_style,
            );
            root.add(&mut self.tree, child);
        }
        if build.error.is_some() {
            for child in self.make_diff_error_children(id, opts, syntax_style) {
                root.add(&mut self.tree, child);
            }
        }
        if let Content::Diff(state) = &mut root.content {
            state.hunk_start_lines = build.hunk_start_lines;
        }
        root
    }

    /// The error view — Diff.ts:310-365: the red message text
    /// (flexShrink 0, width 100%) over the raw diff through a
    /// `filetype: "diff"` code block (flexGrow 1, flexShrink 1).
    /// Returns the children unattached (the caller adds them under the
    /// diff root in order).
    fn make_diff_error_children(
        &mut self,
        id: &str,
        opts: &diff::DiffOptions,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Vec<Renderable> {
        let message = diff::build_diff_view(opts, None)
            .error
            .unwrap_or_else(|| "Unknown error".to_owned());
        let error_message = format!("Error parsing diff: {message}\n");
        let text_state = crate::renderables::text::TextRenderableState::from_options(
            &crate::renderables::text::TextOptions {
                layout: crate::layout::LayoutOptions {
                    flex_shrink: Some(0.0),
                    ..Default::default()
                },
                width: Some(Dimension::Percent(1.0)),
                content: crate::renderables::text::ContentInput::Str(error_message),
                fg: Some(parse_color("#ef4444")),
                ..Default::default()
            },
        );
        let text_ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(text_state.measured()),
            ..Default::default()
        };
        let error_text = Renderable::new(
            &mut self.tree,
            &format!("{id}-error-text"),
            crate::layout::LayoutOptions {
                flex_shrink: Some(0.0),
                ..Default::default()
            },
            Dimension::Percent(1.0),
            Dimension::Auto,
            Content::Text(text_state),
            Some(text_ctx),
        );

        // the raw diff through a code block — create-branch wrapMode is
        // `this._wrapMode` (undefined → the TextBuffer "word" default;
        // the update branch sets "none", Diff.ts:356 — quirk kept)
        let code_opts = crate::renderables::code::CodeOptions {
            content: opts.diff.clone(),
            filetype: Some("diff".to_owned()),
            conceal: Some(opts.conceal),
            fg: opts.fg,
            ..Default::default()
        };
        let doc = crate::renderables::code::build_code_document(&code_opts, syntax_style);
        let code_state = crate::renderables::text::TextRenderableState::from_options(
            &crate::renderables::text::TextOptions {
                layout: crate::layout::LayoutOptions {
                    flex_grow: Some(1.0),
                    flex_shrink: Some(1.0),
                    ..Default::default()
                },
                width: Some(Dimension::Percent(1.0)),
                content: crate::renderables::text::ContentInput::Styled(doc.to_styled()),
                fg: opts.fg,
                wrap_mode: opts.wrap_mode,
                ..Default::default()
            },
        );
        let code_ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(code_state.measured()),
            ..Default::default()
        };
        let error_code = Renderable::new(
            &mut self.tree,
            &format!("{id}-error-code"),
            crate::layout::LayoutOptions {
                flex_grow: Some(1.0),
                flex_shrink: Some(1.0),
                ..Default::default()
            },
            Dimension::Percent(1.0),
            Dimension::Auto,
            Content::Text(code_state),
            Some(code_ctx),
        );
        vec![error_text, error_code]
    }

    /// One diff side — the `LineNumberRenderable` wrapper
    /// (LineNumberRenderable.ts:406-442: row, height forced auto) with
    /// its `[gutter, code]` children (:444-479).
    fn make_diff_side(
        &mut self,
        id: &str,
        spec: &diff::SideSpec,
        opts: &diff::DiffOptions,
        syntax_style: &crate::syntax_style::SyntaxStyle,
    ) -> Renderable {
        use crate::renderables::line_number as ln;

        // the wrapper's content colors + the code concealment source map
        let mut line_colors_content = std::collections::HashMap::new();
        for (line, color) in &spec.line_colors {
            ln::parse_line_color(
                color,
                &mut std::collections::HashMap::new(),
                &mut line_colors_content,
                *line,
            );
        }
        let code_opts = crate::renderables::code::CodeOptions {
            content: spec.content.clone(),
            filetype: opts.filetype.clone(),
            conceal: Some(opts.conceal),
            fg: opts.fg,
            ..Default::default()
        };
        let (doc, highlights) =
            crate::renderables::code::build_code_document_with_highlights(&code_opts, syntax_style);
        let source_map = crate::renderables::code::conceal_lines_source_map(
            &spec.content,
            &highlights,
            opts.conceal,
        );

        // the target CodeRenderable — width/height 100% (Diff.ts:383-384)
        let code_state = crate::renderables::text::TextRenderableState::from_options(
            &crate::renderables::text::TextOptions {
                width: Some(Dimension::Percent(1.0)),
                height: Some(Dimension::Percent(1.0)),
                content: crate::renderables::text::ContentInput::Styled(doc.to_styled()),
                fg: opts.fg,
                // CodeRenderable's wrapMode: the Diff's option, else the
                // TextBuffer "word" default (TextBufferRenderable.ts:55)
                wrap_mode: Some(opts.wrap_mode.unwrap_or_default()),
                ..Default::default()
            },
        );
        let initial_total = code_state.doc.lines.len() as u32;
        let code_ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(code_state.measured()),
            ..Default::default()
        };
        let target = Renderable::new(
            &mut self.tree,
            &format!("{id}-code"),
            crate::layout::LayoutOptions::default(),
            Dimension::Percent(1.0),
            Dimension::Percent(1.0),
            Content::Text(code_state),
            Some(code_ctx),
        );

        // the gutter — auto/auto, flexGrow 0, flexShrink 0, measured
        // (LineNumberRenderable.ts:72-79); `showLineNumbers` maps to
        // `gutter.visible` (Diff.ts:453, LineNumberRenderable.ts:567-571)
        let mut line_options = ln::LineNumberOptions {
            fg: opts.line_number_fg,
            bg: opts.line_number_bg,
            line_colors: spec.line_colors.clone(),
            line_signs: spec.line_signs.clone(),
            line_numbers: spec.line_numbers.clone(),
            hide_line_numbers: spec.hide_line_numbers.clone(),
            width: spec.width,
            ..Default::default()
        };
        let _ = &mut line_options;
        let gutter_state = ln::GutterState::new(&line_options);
        let gutter_measure = gutter_state.measure_ctx(initial_total);
        let gutter_ctx = MeasureCtx {
            kind: MeasureKind::Gutter,
            gutter: Some(gutter_measure),
            ..Default::default()
        };
        let mut gutter = Renderable::new(
            &mut self.tree,
            &format!("{id}-gutter"),
            crate::layout::LayoutOptions {
                flex_grow: Some(0.0),
                flex_shrink: Some(0.0),
                ..Default::default()
            },
            Dimension::Auto,
            Dimension::Auto,
            Content::Gutter(gutter_state),
            Some(gutter_ctx),
        );
        gutter.visible = opts.show_line_numbers;
        gutter.apply_initial_display(&mut self.tree);

        let mut wrapper = Renderable::new(
            &mut self.tree,
            id,
            crate::layout::LayoutOptions {
                flex_direction: Some("row".to_owned()),
                ..Default::default()
            },
            spec.width,
            // height forced "auto" (LineNumberRenderable.ts:412)
            Dimension::Auto,
            Content::LineNumber(ln::LineNumberState {
                line_colors_content,
                source_map,
            }),
            None,
        );
        wrapper.add(&mut self.tree, gutter);
        wrapper.add(&mut self.tree, target);
        wrapper
    }

    /// Rebuild a diff's side subtrees at the current width — the
    /// `requestRebuild` microtask's payload (Diff.ts:221-234) applied:
    /// `buildSplitView`'s alignment pass (Diff.ts:755-821) over the
    /// measured per-logical-line visual counts, then fresh sides.
    fn apply_diff_realign(&mut self, diff_id: &str, effects: &mut WalkEffects) {
        // measure first: the counts + code widths the rebuild reads
        // options snapshot first (a short borrow), then the measurement
        let options = {
            let Some(node) = self.find_node(&self.root.inner, diff_id) else {
                return;
            };
            let Content::Diff(state) = &node.content else {
                return;
            };
            state.options.clone()
        };
        let mut counts = diff::SideVisualCounts::default();
        let mut code_widths = [0.0f32, 0.0];
        {
            let Some(node) = self.find_node(&self.root.inner, diff_id) else {
                return;
            };
            let Content::Diff(state) = &node.content else {
                return;
            };
            let _ = state;
            for (side, wrapper) in node.children.iter().take(2).enumerate() {
                let Some(target) = wrapper
                    .children
                    .iter()
                    .skip(1)
                    .find(|c| matches!(c.content, Content::Text(_)))
                else {
                    continue;
                };
                let Content::Text(text) = &target.content else {
                    continue;
                };
                code_widths[side] = target.w;
                let wrap_width = if text.wrap_mode == crate::renderables::text::WrapMode::None {
                    None
                } else {
                    Some(text.viewport.map_or(0, |vp| vp.width))
                }
                .filter(|w| *w > 0);
                let vlines = crate::text_buffer::compute_virtual_lines(
                    &text.doc,
                    text.wrap_mode,
                    wrap_width,
                );
                let source_map = wrapper_source_map(wrapper);
                let per_side = if side == 0 {
                    &mut counts.left
                } else {
                    &mut counts.right
                };
                per_side.resize(text.doc.lines.len().max(vlines.len()), 0);
                for vline in &vlines {
                    // mapped through the concealment source map
                    // (Code.ts:157 — lineInfo remaps rendered rows)
                    let logical = source_map
                        .as_ref()
                        .and_then(|map| map.get(vline.source_line as usize).copied())
                        .unwrap_or(i64::from(vline.source_line));
                    if logical >= 0 && (logical as usize) < per_side.len() {
                        per_side[logical as usize] += 1;
                    }
                }
            }
        }
        let build = diff::build_diff_view(&options, Some(&counts));
        let style = crate::syntax_style::SyntaxStyle::create();
        // fresh side subtrees (createOrUpdateSide's maps + the final
        // content, Diff.ts:908-925), then swap them in
        let mut children: Vec<Renderable> = Vec::new();
        for (index, side) in build.sides.iter().enumerate() {
            children.push(self.make_diff_side(
                &format!("{diff_id}-{}", if index == 0 { "left" } else { "right" }),
                side,
                &options,
                &style,
            ));
        }
        {
            let Self { tree, root, .. } = self;
            let Some(node) = root.inner.find_mut(diff_id) else {
                return;
            };
            let diff_node = node.node;
            while !node.children.is_empty() {
                let child_id = node.children[0].id.0.clone();
                if let Some(child) = node.remove_child_by_id(tree, &child_id) {
                    let _ = tree.remove(child.node);
                }
            }
            for child in children {
                node.add(tree, child);
            }
            if let Content::Diff(state) = &mut node.content {
                state.aligned = true;
                state.last_code_widths = code_widths;
                state.hunk_start_lines = build.hunk_start_lines;
            }
            let _ = tree.mark_dirty(diff_node);
        }
        effects.size_changed = true;
    }

    /// Refresh a gutter's measure to the target's current virtual line
    /// count — upstream `line-info-change` → `gutter.remeasure()`
    /// (LineNumberRenderable.ts:96-103, 379-383).
    fn apply_gutter_remeasure(&mut self, wrapper_id: &str, effects: &mut WalkEffects) {
        let mut refresh: Option<(tp::NodeId, u32)> = None;
        {
            let Some(node) = Self::find_node_mut(&mut self.root.inner, wrapper_id) else {
                return;
            };
            let Some(target) = node
                .children
                .iter()
                .skip(1)
                .find(|c| matches!(c.content, Content::Text(_)))
            else {
                return;
            };
            let Content::Text(text) = &target.content else {
                return;
            };
            let wrap_width = if text.wrap_mode == crate::renderables::text::WrapMode::None {
                None
            } else {
                Some(text.viewport.map_or(0, |vp| vp.width))
            }
            .filter(|w| *w > 0);
            let vlines =
                crate::text_buffer::compute_virtual_lines(&text.doc, text.wrap_mode, wrap_width);
            if let Some(gutter) = node.children.first() {
                refresh = Some((gutter.node, vlines.len() as u32));
            }
        }
        let Some((gutter_node, total)) = refresh else {
            return;
        };
        if let Some(ctx) = self.tree.get_node_context_mut(gutter_node) {
            if let Some(measure) = &mut ctx.gutter {
                measure.total_lines = total;
            }
        }
        let _ = self.tree.mark_dirty(gutter_node);
        effects.size_changed = true;
    }

    /// Deferred tree repairs collected by the update walk
    /// ([`crate::renderable::RepairEvent`]) — applied between settle
    /// passes, each driving another layout like upstream's
    /// `requestRender`.
    fn apply_repairs(&mut self, effects: &mut WalkEffects) {
        let repairs = std::mem::take(&mut effects.repairs);
        for repair in repairs {
            match repair {
                crate::renderable::RepairEvent::GutterRemeasure { wrapper } => {
                    self.apply_gutter_remeasure(&wrapper, effects);
                }
                crate::renderable::RepairEvent::DiffRealign { diff } => {
                    self.apply_diff_realign(&diff, effects);
                }
            }
        }
    }

    /// `diffRenderable.diff = value` / `.view = value` — the two
    /// content setters (Diff.ts:928-961): re-parse + rebuild the sides
    /// (the split path's microtask rebuild settles synchronously here).
    pub fn diff_set(&mut self, id: &str, diff: Option<String>, view: Option<diff::DiffView>) {
        let mut changed = false;
        {
            let Self { tree, root, .. } = self;
            let Some(node) = root.inner.find_mut(id) else {
                return;
            };
            let Content::Diff(state) = &mut node.content else {
                return;
            };
            if let Some(value) = diff {
                if state.options.diff != value {
                    state.options.diff = value;
                    changed = true;
                }
            }
            if let Some(value) = view {
                if state.view != value {
                    state.view = value;
                    state.options.view = value;
                    node.layout.flex_direction = Some(
                        if value == diff::DiffView::Split {
                            "row"
                        } else {
                            "column"
                        }
                        .to_owned(),
                    );
                    let style = crate::layout::taffy_style(&node.layout, node.width, node.height);
                    let _ = tree.set_style(node.node, style);
                    changed = true;
                }
            }
            if changed {
                state.aligned = false;
                state.last_code_widths = [0.0, 0.0];
            }
        }
        if !changed {
            return;
        }
        self.rebuild_diff(id);
        self.render();
    }

    /// Rebuild a diff's subtrees from its current options (the
    /// `buildView` payload; unified rebuilds immediately, split defers
    /// to the realign pass — Diff.ts:240-246).
    fn rebuild_diff(&mut self, id: &str) {
        let options = {
            let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
                return;
            };
            let Content::Diff(state) = &node.content else {
                return;
            };
            state.options.clone()
        };
        let style = crate::syntax_style::SyntaxStyle::create();
        let build = diff::build_diff_view(&options, None);
        let mut children: Vec<Renderable> = Vec::new();
        for (index, side) in build.sides.iter().enumerate() {
            children.push(self.make_diff_side(
                &format!("{id}-{}", if index == 0 { "left" } else { "right" }),
                side,
                &options,
                &style,
            ));
        }
        if build.error.is_some() {
            children.extend(self.make_diff_error_children(id, &options, &style));
        }
        {
            let Self { tree, root, .. } = self;
            let Some(node) = root.inner.find_mut(id) else {
                return;
            };
            let diff_node = node.node;
            while !node.children.is_empty() {
                let child_id = node.children[0].id.0.clone();
                if let Some(child) = node.remove_child_by_id(tree, &child_id) {
                    let _ = tree.remove(child.node);
                }
            }
            for child in children {
                node.add(tree, child);
            }
            if let Content::Diff(state) = &mut node.content {
                state.hunk_start_lines = build.hunk_start_lines;
            }
            let _ = tree.mark_dirty(diff_node);
        }
    }

    /// `getHunkRowOffsets()` (Diff.ts:1254-1259) — the hunk-start visual
    /// rows resolved against the left code's current line sources.
    #[must_use]
    pub fn diff_hunk_row_offsets(&self, id: &str) -> Vec<usize> {
        let Some(node) = self.find_node(&self.root.inner, id) else {
            return Vec::new();
        };
        let Content::Diff(state) = &node.content else {
            return Vec::new();
        };
        // computeHunkRowOffsets reads leftCodeRenderable.lineInfo
        // (Diff.ts:1266) — the left wrapper's target, remapped through
        // its concealment source map
        let Some(wrapper) = node.children.first() else {
            return Vec::new();
        };
        let sources = wrapper_text_sources(wrapper);
        diff::compute_hunk_row_offsets(&state.hunk_start_lines, &sources)
    }

    /// Update an input leaf's measure context after its value changed
    /// (`content-changed` → `yogaNode.markDirty`,
    /// EditBufferRenderable.ts:203-210).
    pub fn refresh_input_measure(&mut self, id: &str) {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return;
        };
        if let Content::Input(state) = &node.content {
            let w = state.edit_width_cols();
            let n = node.node;
            if let Some(ctx) = self.tree.get_node_context_mut(n) {
                ctx.input_width_cols = w;
            }
            let _ = self.tree.mark_dirty(n);
        }
    }

    /// `requestRender` → `loop()` tick: one deterministic frame.
    pub fn render(&mut self) -> u64 {
        // 1. clear next with the background color
        self.next_render_buffer.clear(self.config.background_color);

        // 2. layout: root calculateLayout(width, height) —
        //    RootRenderable.render pass 1 (Renderable.ts:1798-1802, 1865-67)
        self.layout_pass();

        // pass 2: update layout + screen coords + inline size hooks
        // (effects drive re-layout frames exactly like upstream's
        // requestRender scheduling — the observed 0.5.14 settle sequence:
        // frame A lays out with both bars visible, the recalc hides the
        // horizontal bar, frame B re-lays the viewport one row taller)
        let mut effects = WalkEffects::default();
        self.root.inner.update_layout(
            &self.tree,
            (0.0, 0.0),
            (f32::INFINITY, f32::INFINITY),
            &mut effects,
        );
        self.apply_recalcs(&mut effects);
        self.apply_repairs(&mut effects);
        let mut guard = 0;
        while effects.any() && guard < 16 {
            self.layout_pass();
            effects = WalkEffects::default();
            self.root.inner.update_layout(
                &self.tree,
                (0.0, 0.0),
                (f32::INFINITY, f32::INFINITY),
                &mut effects,
            );
            if std::env::var("OT_DBG").is_ok() {
                let vp = self
                    .root
                    .inner
                    .find("scroll-box-viewport-0")
                    .map(|n| format!("({},{})", n.w, n.h))
                    .unwrap_or_default();
                eprintln!("[walk{}] vp={}", guard + 2, vp);
            }
            self.apply_recalcs(&mut effects);
            self.apply_repairs(&mut effects);
            guard += 1;
        }

        // pass 2.5: textarea prepare (renderSelf snapshot + renderCursor,
        // EditBufferRenderable.ts:991-1004) — the cursor is renderer
        // state, so it is collected before the paint walk
        let mut cursor_positions: Vec<(String, u32, u32)> = Vec::new();
        let now_ms = self.performance_now();
        self.root.inner.prepare_paint(&mut cursor_positions, now_ms);

        // pass 3: paint into next buffer; the hit grid rebuilds in
        // paint order (addToHitGrid, Renderable.ts:1493)
        self.hit_grid.clear();
        let captured = self.captured.clone();
        Self::collect_hit_rects(&self.root.inner, captured.as_deref(), &mut self.hit_grid);
        self.root.inner.paint(&mut self.next_render_buffer);

        // renderCursor: each focused editor sets the terminal cursor;
        // the last one wins (upstream sets it during render order,
        // EditBufferRenderable.ts:1010-1022). A focused editor that
        // blurred/hidden set `setCursorPosition(0,0,false)` itself
        // (1030-1034); with no focused editor at all the port's only
        // cursor writers are editors (the textareas above, the input
        // in update_terminal_cursor below), so the cursor hides.
        for (id, x, y) in &cursor_positions {
            if let Some(node) = self.find_node(&self.root.inner, id) {
                if let Content::Textarea(state) = &node.content {
                    let (style, blinking, color) = state.cursor_style();
                    self.cursor_state.set_style(style, blinking, color);
                }
            }
            self.cursor_state.set_position(*x, *y, true);
        }
        if cursor_positions.is_empty() {
            self.cursor_state.visible = false;
        }
        // the Input family's cursor rides the same channel
        // (setCursorPosition, renderer.ts:4149-4155)
        self.update_terminal_cursor();

        // 3. SGR emission (diff current vs next) — before the swap
        let mut sgr = String::new();
        self.emit_frame_sgr(&mut sgr);
        self.sgr_history.push_str(&sgr);
        self.last_frame_sgr = sgr;

        // swap: current := next (renderer.zig syncs cell-by-cell; a frame
        // ends with current == next)
        core::mem::swap(
            &mut self.current_render_buffer,
            &mut self.next_render_buffer,
        );

        self.frame_id += 1;
        // FRAME event (renderer.ts:4761-4765)
        self.emit_event(CliRenderEvent::Frame {
            frame_id: self.frame_id,
        });
        self.frame_id
    }

    fn layout_pass(&mut self) {
        let root_node = self.root.inner.node;
        let available = tp::Size {
            width: tp::AvailableSpace::Definite(self.config.width as f32),
            height: tp::AvailableSpace::Definite(self.config.height as f32),
        };
        self.tree
            .compute_layout_with_measure(root_node, available, Self::measure_text)
            .expect("taffy layout");
    }

    /// Apply size-change hook events in fire order — each is a
    /// `recalculateBarProps` run with the at-fire-time sizes
    /// (ScrollBox.ts:749-805).
    fn apply_recalcs(&mut self, effects: &mut WalkEffects) {
        let Self { root, tree, .. } = self;
        for event in std::mem::take(&mut effects.events) {
            scrollbox::apply_recalc(
                &mut root.inner,
                tree,
                &event.owner,
                event.stale_content_size,
                effects,
            );
        }
    }

    /// Mount a renderable under an arbitrary node (nested `add`).
    /// Returns false when `parent_id` is not in the tree. ScrollBox ids
    /// route into the content viewport (`ScrollBox.add`, ScrollBox.ts:514-516).
    pub fn mount_child(&mut self, parent_id: &str, child: Renderable) -> bool {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        crate::renderable::mount_under(&mut root.inner, &target, tree, child).is_none()
    }

    /// `parent.add(child, index)` for the reconciler's anchored inserts
    /// (upstream `_insertNode` → `parent.add(node, anchorIndex)`,
    /// reconciler.ts:98-111). ScrollBox ids route into the content
    /// viewport like [`CliRenderer::mount_child`]. Returns false when
    /// `parent_id` is not in the tree.
    pub fn insert_child_at(&mut self, parent_id: &str, index: usize, child: Renderable) -> bool {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        match root.inner.find_mut(&target) {
            Some(parent) => {
                parent.insert_at(tree, index, child);
                true
            }
            None => false,
        }
    }

    /// `parent.remove(child)` WITHOUT destruction — detach the subtree
    /// for a keyed MOVE (upstream `Renderable.add`'s same-parent
    /// branch unlinks first, Renderable.ts:1225-1228; the taffy subtree
    /// stays intact for re-attachment via
    /// [`CliRenderer::insert_child_at`]).
    pub fn detach_child(&mut self, parent_id: &str, child_id: &str) -> Option<Renderable> {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        let Self { tree, root, .. } = self;
        let parent = root.inner.find_mut(&target)?;
        let pos = parent.children.iter().position(|c| c.id.0 == child_id)?;
        let _ = tree.remove_child_at_index(parent.node, pos);
        Some(parent.children.remove(pos))
    }

    /// `parent.remove(child)` + `destroyRecursively` (upstream
    /// `_removeNode`, reconciler.ts:113-141: detach, then destroy the
    /// detached subtree on the next tick — here synchronously, dropping
    /// the owned subtree and freeing its taffy nodes). ScrollBox ids
    /// route into the content viewport. Returns false when either id is
    /// missing.
    pub fn unmount_child(&mut self, parent_id: &str, child_id: &str) -> bool {
        let Some(node) = self.detach_child(parent_id, child_id) else {
            return false;
        };
        // destroyRecursively: free the subtree's taffy nodes (the node
        // is unlinked now, so `remove` takes the whole subtree)
        let _ = self.tree.remove(node.node);
        true
    }

    /// Live child ids under `parent_id` (ScrollBox ids route into the
    /// content viewport) — reconciler/test observability of child order.
    #[must_use]
    pub fn has_node(&self, id: &str) -> bool {
        self.root.inner.find(id).is_some()
    }

    #[must_use]
    pub fn child_ids(&self, parent_id: &str) -> Option<Vec<String>> {
        let target = self
            .content_of
            .get(parent_id)
            .cloned()
            .unwrap_or_else(|| parent_id.to_owned());
        self.root
            .inner
            .find(&target)
            .map(|p| p.children.iter().map(|c| c.id.0.clone()).collect())
    }

    /// Rename a node's live id (`node.id = value`, reconciler.ts:260-263).
    /// Keeps ScrollBox content routing coherent.
    pub fn rename_node(&mut self, from: &str, to: &str) -> bool {
        if let Some(node) = self.root.inner.find_mut(from) {
            node.id.0 = to.to_owned();
        } else {
            return false;
        }
        if let Some(content) = self.content_of.get(from).cloned() {
            self.content_of.remove(from);
            self.content_of.insert(to.to_owned(), content);
        }
        true
    }

    /// Re-apply a node's style from the full layout options (upstream:
    /// each style prop setter writes the yoga node, e.g. Renderable.ts
    /// padding/flex setters; a wholesale `setStyle` is equivalent since
    /// [`crate::layout::taffy_style`] derives from the same options).
    /// Returns false when `id` is not in the tree.
    pub fn restyle_node(
        &mut self,
        id: &str,
        layout: crate::layout::LayoutOptions,
        width: Option<Dimension>,
        height: Option<Dimension>,
    ) -> bool {
        let Self { tree, root, .. } = self;
        let Some(node) = root.inner.find_mut(id) else {
            return false;
        };
        node.layout = layout;
        node.width = width.unwrap_or(Dimension::Auto);
        node.height = height.unwrap_or(Dimension::Auto);
        let style = crate::layout::taffy_style(&node.layout, node.width, node.height);
        let _ = tree.set_style(node.node, style);
        let _ = tree.mark_dirty(node.node);
        true
    }

    /// Construct a ScrollBox subtree without mounting (children added via
    /// [`CliRenderer::mount_child`] land in its content).
    #[must_use]
    pub fn make_scrollbox(
        &mut self,
        id: &str,
        opts: &mut scrollbox::ScrollBoxOptions,
    ) -> Renderable {
        let r = scrollbox::build_scrollbox(&mut self.tree, id, opts);
        let content_id = match &r.content {
            Content::ScrollBox(_, state) => state.content_id.clone(),
            _ => unreachable!("build_scrollbox yields ScrollBox root"),
        };
        self.content_of.insert(id.to_owned(), content_id);
        r
    }

    /// `scrollBox.scrollTop = value` (ScrollBox.ts:168-171) followed by a
    /// settle render — the harness's scripted-mutation step.
    pub fn set_scroll_top(&mut self, owner: &str, value: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::set_scroll_top(&mut root.inner, tree, owner, value, &mut effects);
        let _ = effects;
        self.render();
    }

    pub fn set_scroll_left(&mut self, owner: &str, value: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::set_scroll_left(&mut root.inner, tree, owner, value, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollBy(delta, unit)` (ScrollBox.ts:404-413).
    pub fn scroll_by(
        &mut self,
        owner: &str,
        dx: f32,
        dy: f32,
        unit: crate::renderables::scrollbar::ScrollUnit,
    ) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_by(&mut root.inner, tree, owner, dx, dy, unit, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollTo({x, y})` (ScrollBox.ts:464-473).
    pub fn scroll_to(&mut self, owner: &str, x: f32, y: f32) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_to(&mut root.inner, tree, owner, x, y, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `scrollBox.scrollChildIntoView(childId)` (ScrollBox.ts:415-462).
    pub fn scroll_child_into_view(&mut self, owner: &str, child_id: &str) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::scroll_child_into_view(&mut root.inner, tree, owner, child_id, &mut effects);
        let _ = effects;
        self.render();
    }

    /// Wheel-event seam (`onMouseEvent` scroll branch, ScrollBox.ts:542-584).
    pub fn scroll_event(&mut self, owner: &str, event: &scrollbox::ScrollEvent) {
        let mut effects = WalkEffects::default();
        let Self { root, tree, .. } = self;
        scrollbox::on_scroll_event(&mut root.inner, tree, owner, event, &mut effects);
        let _ = effects;
        self.render();
    }

    /// `renderer.resize(w, h)` (renderer.ts:4073-4076 → processResize):
    /// new buffers, root re-sized; cached node sizes PERSIST (upstream
    /// `_widthValue` keeps the last size across resizes, Renderable.ts:1096-1119)
    /// so only genuinely-resized nodes fire their size hooks.
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.next_render_buffer = OptimizedBuffer::new(width, height);
        self.current_render_buffer = OptimizedBuffer::new(width, height);
        // RootRenderable size is fixed at construction (Renderable.ts:1743+)
        let node = self.root.inner.node;
        let mut style = self.tree.style(node).expect("root style").clone();
        style.size.width = tp::Dimension::length(width as f32);
        style.size.height = tp::Dimension::length(height as f32);
        let _ = self.tree.set_style(node, style);
        let _ = self.tree.mark_dirty(node);
        self.render();
    }

    /// The taffy measure function: Text leaves measure their wrapped
    /// content; Editor leaves measure through the EditorView target
    /// (native-renderable.zig:63-88); everything else taffy defaults.
    fn measure_text(
        known_dimensions: tp::Size<Option<f32>>,
        available_space: tp::Size<tp::AvailableSpace>,
        _node_id: tp::NodeId,
        ctx: Option<&mut MeasureCtx>,
        _style: &tp::Style,
    ) -> tp::Size<f32> {
        let Some(ctx) = ctx else {
            return tp::Size::ZERO;
        };
        if ctx.kind == MeasureKind::Text {
            // perf lane: borrow the measure target — cloning the whole
            // TextDocument per taffy measure call dominated layout cost
            // on text-heavy trees (measure only reads it)
            let default_text;
            let text = match &ctx.text {
                Some(t) => t,
                None => {
                    default_text = crate::renderables::text::MeasuredText::default();
                    &default_text
                }
            };
            let avail_w = match available_space.width {
                tp::AvailableSpace::Definite(w) => w,
                tp::AvailableSpace::MaxContent => f32::INFINITY,
                tp::AvailableSpace::MinContent => 0.0,
            };
            let (w, h) = crate::renderables::text::TextRenderableState::measure(
                text,
                known_dimensions.width,
                avail_w,
            );
            return tp::Size {
                width: w,
                height: h,
            };
        }
        if ctx.kind == MeasureKind::Editor {
            let default_editor;
            let editor = match &ctx.editor {
                Some(e) => e,
                None => {
                    default_editor = crate::renderables::textarea::MeasuredEditor::default();
                    &default_editor
                }
            };
            let avail_w = match available_space.width {
                tp::AvailableSpace::Definite(w) => w,
                tp::AvailableSpace::MaxContent => f32::INFINITY,
                tp::AvailableSpace::MinContent => 0.0,
            };
            let avail_h = match available_space.height {
                tp::AvailableSpace::Definite(h) => h,
                tp::AvailableSpace::MaxContent => f32::INFINITY,
                tp::AvailableSpace::MinContent => 0.0,
            };
            let (w, h) = TextareaState::measure(
                &editor.doc,
                editor.wrap_mode,
                known_dimensions.width,
                avail_w,
                avail_h,
            );
            return tp::Size {
                width: w,
                height: h,
            };
        }
        if ctx.kind == MeasureKind::Gutter {
            // GutterRenderable.setupMeasureFunc (LineNumberRenderable.ts:106-125):
            // content-derived dimensions — calculateWidth × the target's
            // virtual line count — ignoring the incoming constraints.
            let empty = crate::renderables::line_number::GutterMeasure::default();
            let measure = ctx.gutter.as_ref().unwrap_or(&empty);
            let (w, h) = measure.measure();
            return tp::Size {
                width: w,
                height: h,
            };
        }
        tp::Size::ZERO
    }

    /// The SGR frame writer — `prepareRenderFrameWithWriter` simplified to
    /// the full-repaint first frame + diffed subsequent frames with
    /// row-equality fast path (renderer.zig:2650-2838).
    fn emit_frame_sgr(&mut self, out: &mut String) {
        let width = self.config.width;
        let height = self.config.height;
        let force = self.frame_id == 0;
        let mut current_fg: Option<Rgba> = None;
        let mut current_bg: Option<Rgba> = None;
        let mut current_attrs: Option<u32> = None;
        let mut run_start: i64 = -1;
        let mut run_length: u32 = 0;

        for y in 0..height {
            // row-equality fast path (renderer.zig:2655-2662)
            if !force {
                let mut equal = true;
                for x in 0..width {
                    match (
                        self.current_render_buffer.get(x, y),
                        self.next_render_buffer.get(x, y),
                    ) {
                        (Some(a), Some(b)) if a == b => {}
                        _ => {
                            equal = false;
                            break;
                        }
                    }
                }
                if equal {
                    continue;
                }
            }
            for x in 0..width {
                let (Some(cur), Some(next)) = (
                    self.current_render_buffer.get(x, y),
                    self.next_render_buffer.get(x, y),
                ) else {
                    continue;
                };
                if !force && cur == next {
                    if run_length > 0 {
                        out.push_str(crate::ansi::ansi_consts::RESET);
                        run_start = -1;
                        run_length = 0;
                    }
                    continue;
                }
                let fg_match = current_fg.is_some_and(|c| c == next.fg);
                let bg_match = current_bg.is_some_and(|c| c == next.bg);
                let same_attrs = fg_match && bg_match && current_attrs == Some(next.attributes);
                if !same_attrs || run_start == -1 {
                    if run_length > 0 {
                        out.push_str(crate::ansi::ansi_consts::RESET);
                    }
                    run_start = i64::from(x);
                    run_length = 0;
                    current_fg = Some(next.fg);
                    current_bg = Some(next.bg);
                    current_attrs = Some(next.attributes);
                    crate::ansi::move_to(out, x + 1, y + 1);
                    crate::ansi::emit_color(out, next.fg, false);
                    crate::ansi::emit_color(out, next.bg, true);
                    crate::ansi::TextAttributes::apply_attributes(out, next.attributes);
                }
                // print the cell's text
                if crate::buffer::is_continuation_char(next.char) {
                    // no bytes; cursor advances implicitly
                } else if crate::buffer::is_grapheme_char(next.char) {
                    out.push_str(
                        self.next_render_buffer
                            .grapheme_bytes(crate::buffer::grapheme_id_from_char(next.char)),
                    );
                } else if let Some(c) = char::from_u32(next.char) {
                    out.push(c);
                }
                run_length += 1;
            }
        }
        if run_length > 0 {
            out.push_str(crate::ansi::ansi_consts::RESET);
        }
        // the cursor emission tail (renderer.zig:2932-3016) — `frame_
        // started` mirrors whether any cell bytes were written
        let frame_started = run_length > 0;
        self.cursor_state.emit(out, frame_started);
    }

    /// SGR bytes of the last rendered frame (harness observability).
    #[must_use]
    pub fn last_frame_sgr(&self) -> &str {
        &self.last_frame_sgr
    }

    // -- bench-rig hooks (perf lane) ---------------------------------------
    //
    // The perf rig (`tools/bench`) measures the SGR frame writer in
    // isolation. These expose the internal writer + its forced-full-
    // repaint path WITHOUT adding public API surface beyond the rig:
    // doc-hidden, zero behavior change for every other caller.

    /// [`Self::emit_frame_sgr`] exposed for the perf rig (doc-hidden —
    /// not part of the ported API surface).
    #[doc(hidden)]
    pub fn bench_emit_frame_sgr(&mut self, out: &mut String) {
        self.emit_frame_sgr(out);
    }

    /// Reset the frame counter so the NEXT emission takes the forced
    /// full-repaint path (`frame_id == 0`, renderer.zig first-frame).
    /// Perf-rig only (doc-hidden): a real render() after this would
    /// simply emit one full frame then resume diffing.
    #[doc(hidden)]
    pub fn bench_force_full_frame(&mut self) {
        self.frame_id = 0;
    }

    /// The FULL SGR emission across all rendered frames — what a terminal
    /// consumed (the harness's vt-replay validates this end-to-end).
    #[must_use]
    pub fn full_sgr_stream(&self) -> &str {
        &self.sgr_history
    }

    /// Alias of [`Self::full_sgr_stream`] (the textarea lane's original
    /// name; kept for the lane's callers).
    #[must_use]
    pub fn sgr_stream(&self) -> &str {
        &self.sgr_history
    }

    #[must_use]
    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    /// Update a text leaf's measure context after content changes
    /// (`content` setter → updateTextBuffer, Text.ts:75-83).
    pub fn refresh_text_measure(&mut self, id: &str) {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return;
        };
        if let Content::Text(state) = &mut node.content {
            state.invalidate_vlines();
            let m = state.measured();
            let n = node.node;
            if let Some(ctx) = self.tree.get_node_context_mut(n) {
                ctx.text = Some(m);
            }
            let _ = self.tree.mark_dirty(n);
        }
    }

    /// Update a textarea leaf's measure context after content changes
    /// (`content-changed` → `yogaNode.markDirty`, EditBufferRenderable.ts:203-210).
    pub fn refresh_textarea_measure(&mut self, id: &str) {
        let Some(node) = self.find_node(&self.root.inner, id) else {
            return;
        };
        if let Content::Textarea(state) = &node.content {
            let m = state.measured();
            let n = node.node;
            if let Some(ctx) = self.tree.get_node_context_mut(n) {
                ctx.editor = Some(m);
            }
            let _ = self.tree.mark_dirty(n);
        }
    }

    /// The terminal cursor state (upstream `renderer.cursor` /
    /// `setCursorPosition` on the RenderContext).
    pub fn set_cursor_position(&mut self, x: u32, y: u32, visible: bool) {
        self.cursor_state.set_position(x, y, visible);
    }

    pub fn set_cursor_style(
        &mut self,
        style: crate::renderables::textarea::CursorStyle,
        blinking: bool,
        color: Rgba,
    ) {
        self.cursor_state.set_style(style, blinking, color);
    }

    pub fn cursor_position(&self) -> (u32, u32, bool) {
        (
            self.cursor_state.x,
            self.cursor_state.y,
            self.cursor_state.visible,
        )
    }

    // -- textarea actions + selection delivery ---------------------------------

    /// Execute one of the 36 edit-buffer actions on a mounted textarea
    /// (the `withFocusedEditor` host path, edit-buffer-bindings.ts:433-440).
    /// Delivers the pending global-selection update to the anchor
    /// renderable afterwards (upstream notifies inside updateSelection;
    /// deferred here — see `TextareaState::update_selection_for_movement`).
    pub fn textarea_action(&mut self, id: &str, action: &str) -> bool {
        let mut dirty = false;
        let result = self.with_textarea_mut(id, |state, pos, selection| {
            let mut ctx = crate::renderables::textarea::ActionCtx {
                selection,
                node_x: pos.0,
                node_y: pos.1,
                selection_dirty: false,
            };
            let result = state.execute_action(action, &mut ctx);
            dirty = ctx.selection_dirty;
            result
        });
        if result.is_some_and(|_| dirty) {
            self.deliver_selection();
        }
        result.unwrap_or(false)
    }

    /// `handleKeyPress` on a mounted textarea.
    pub fn textarea_key_press(&mut self, id: &str, key: &crate::events::KeyEvent) -> bool {
        let mut dirty = false;
        let result = self.with_textarea_mut(id, |state, pos, selection| {
            let mut ctx = crate::renderables::textarea::ActionCtx {
                selection,
                node_x: pos.0,
                node_y: pos.1,
                selection_dirty: false,
            };
            let result = state.handle_key_press(key, &mut ctx);
            dirty = ctx.selection_dirty;
            result
        });
        if result.is_some_and(|_| dirty) {
            self.deliver_selection();
        }
        result.unwrap_or(false)
    }

    /// Borrow a textarea's state with the selection moved out (the
    /// tree and the selection are both renderer-owned; moving the
    /// selection out keeps the borrows disjoint).
    fn with_textarea_mut(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut TextareaState, (i32, i32), &mut Option<RendererSelection>) -> bool,
    ) -> Option<bool> {
        let mut selection = std::mem::take(&mut self.selection);
        let Self { root, .. } = self;
        let node = find_node_mut(&mut root.inner, id)?;
        let pos = (node.x as i32, node.y as i32);
        let Content::Textarea(state) = &mut node.content else {
            return None;
        };
        let result = f(state, pos, &mut selection);
        self.selection = selection;
        Some(result)
    }

    /// Mutate a mounted textarea's state (scenario/harness surface).
    pub fn textarea_state_mut(&mut self, id: &str, f: impl FnOnce(&mut TextareaState)) -> bool {
        self.with_textarea_mut(id, |state, _pos, _selection| {
            f(state);
            true
        })
        .unwrap_or(false)
    }

    /// Read a mounted textarea's state (scenario/harness surface).
    pub fn textarea_state<R>(
        &mut self,
        id: &str,
        f: impl FnOnce(&TextareaState) -> R,
    ) -> Option<R> {
        let mut out = None;
        self.with_textarea_mut(id, |state, _pos, _selection| {
            out = Some(f(state));
            true
        });
        out
    }

    /// `notifySelectablesOfSelectionChange` (renderer.ts:5054-5077) —
    /// deliver the global selection (or its absence) to the anchor
    /// renderable: convert to local coordinates
    /// (`convertGlobalToLocalSelection`, lib/selection.ts:167-184) and
    /// run `onSelectionChanged` (EditBufferRenderable.ts:519-580).
    fn deliver_selection(&mut self) {
        let sel = self.selection.clone();
        let Some(sel) = sel else {
            return;
        };
        let anchor_id = sel.anchor_renderable.clone();
        let is_start = sel.is_start;
        let is_dragging = sel.is_dragging;
        // absolute anchor = anchor renderable's current position + the
        // stored relative offset (SelectionAnchor.x, lib/selection.ts:18-24)
        let Some(node) = self.find_node(&self.root.inner, &anchor_id) else {
            return;
        };
        let (nx, ny) = (node.x as i32, node.y as i32);
        let anchor_abs = (nx + sel.anchor_relative.0, ny + sel.anchor_relative.1);
        let local = Some((
            sel.anchor_relative.0,
            sel.anchor_relative.1,
            sel.focus.0 - nx,
            sel.focus.1 - ny,
            sel.behavior,
        ));
        let _ = anchor_abs;
        self.with_textarea_mut(&anchor_id, move |state, _pos, _selection| {
            state.on_selection_changed(local, is_start, is_dragging);
            true
        });
    }

    /// `clearSelection` (renderer.ts:4943-4946): null-notify the touched
    /// renderable, then drop the state.
    pub fn clear_selection(&mut self) {
        if let Some(sel) = self.selection.take() {
            let anchor_id = sel.anchor_renderable.clone();
            self.with_textarea_mut(&anchor_id, |state, _pos, _selection| {
                state.on_selection_changed(None, false, false);
                true
            });
        }
    }

    /// `hasSelection` (renderer.ts:4935).
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    fn find_node<'a>(&self, root: &'a Renderable, id: &str) -> Option<&'a Renderable> {
        if root.id.0 == id {
            return Some(root);
        }
        root.children.iter().find_map(|c| self.find_node(c, id))
    }

    fn find_node_mut<'a>(root: &'a mut Renderable, id: &str) -> Option<&'a mut Renderable> {
        if root.id.0 == id {
            return Some(root);
        }
        root.children
            .iter_mut()
            .find_map(|c| Self::find_node_mut(c, id))
    }

    // -----------------------------------------------------------------
    // Hit grid + mouse dispatch — renderer.ts:1448-1453, 3602-3817
    // -----------------------------------------------------------------

    /// `addToHitGrid` walk: every VISIBLE renderable adds its rect in
    /// paint order (Renderable.ts:1481-1493). The captured renderable
    /// is skipped (renderer.ts:1450-1452).
    fn collect_hit_rects(node: &Renderable, captured: Option<&str>, grid: &mut Vec<HitRect>) {
        if !node.visible {
            return;
        }
        if captured != Some(node.id.0.as_str()) {
            grid.push(HitRect {
                x: node.screen_x,
                y: node.screen_y,
                w: node.w.max(0.0) as u32,
                h: node.h.max(0.0) as u32,
                id: node.id.0.clone(),
            });
        }
        for child in &node.children {
            Self::collect_hit_rects(child, captured, grid);
        }
    }

    /// `hitTest` (renderer.ts:3869-3871): the top-most rect containing
    /// the point (later paint order wins).
    #[must_use]
    pub fn hit_test(&self, x: i32, y: i32) -> Option<String> {
        self.hit_grid
            .iter()
            .rev()
            .find(|r| x >= r.x && x < r.x + r.w as i32 && y >= r.y && y < r.y + r.h as i32)
            .map(|r| r.id.clone())
    }

    /// `onMouseDown` and friends — register a handler for a
    /// renderable id (`Renderable.processMouseEvent` +
    /// `renderable.on("mouse-down")`).
    pub fn on_mouse<F>(&mut self, id: &str, handler: F)
    where
        F: FnMut(&mut MouseEvent) + Send + 'static,
    {
        self.mouse_handlers.insert(id.to_owned(), Box::new(handler));
    }

    /// `sendMouseEvent` (renderer.ts:3623-3630): deliver to the
    /// target's handler.
    fn send_mouse_event(&mut self, target: &str, event: &mut MouseEvent) {
        if let Some(handler) = self.mouse_handlers.get_mut(target) {
            handler(event);
        }
    }

    /// `dispatchMouseEvent` (renderer.ts:3602-3621): deliver, then the
    /// autoFocus walk — the nearest focusable ancestor of the target
    /// receives focus on an un-prevented left down.
    fn dispatch_mouse_event(&mut self, target: &str, attributes: &RawMouseEvent) {
        let source = self.captured.clone();
        let mut event = MouseEvent::new(Some(target), attributes, source.as_deref(), false);
        self.send_mouse_event(target, &mut event);

        if self.config.auto_focus
            && event.event_type == crate::parse_mouse::MouseEventType::Down
            && event.button == MouseButton::Left as i32
            && !event.default_prevented()
        {
            // walk target → parents for the first focusable
            // (renderer.ts:3609-3618)
            if let Some(path) = self.ancestor_path(target) {
                for id in path {
                    if let Some(node) = self.find_node(&self.root.inner, &id) {
                        if node.focusable {
                            self.focus_renderable(&id);
                            break;
                        }
                    }
                }
            }
        }
    }

    /// The target id followed by its ancestors (target first, root last).
    fn ancestor_path(&self, target: &str) -> Option<Vec<String>> {
        fn walk(node: &Renderable, target: &str, path: &mut Vec<String>) -> bool {
            if node.id.0 == target {
                return true;
            }
            for child in &node.children {
                if walk(child, target, path) {
                    path.insert(0, node.id.0.clone());
                    return true;
                }
            }
            false
        }
        let mut path = vec![target.to_owned()];
        if walk(&self.root.inner, target, &mut path) {
            Some(path)
        } else {
            None
        }
    }

    /// `processSingleMouseEvent` (renderer.ts:3632-3817) — the
    /// selection/console/split-footer branches are the textarea lane;
    /// the core routing (scroll fallback, over/out, drag capture,
    /// drop) is ported order-exact.
    pub fn process_mouse_event(&mut self, event: RawMouseEvent) -> bool {
        // pointer tracking (renderer.ts:3640-3643)
        self.latest_pointer = (event.x, event.y);
        self.has_pointer = true;

        if event.event_type == crate::parse_mouse::MouseEventType::Scroll {
            // scroll → hit target, falling back to the focused
            // renderable (renderer.ts:3659-3674)
            let maybe = self.hit_test(event.x, event.y);
            let fallback = self
                .focused
                .clone()
                .filter(|id| self.find_node(&self.root.inner, id).is_some());
            let scroll_target = maybe.or(fallback);
            if let Some(target) = scroll_target {
                let mut evt = MouseEvent::new(Some(&target), &event, None, false);
                self.send_mouse_event(&target, &mut evt);
            }
            return true;
        }

        let maybe_id = self.hit_test(event.x, event.y);
        let same_element = maybe_id == self.last_over;
        // hit tracking updates even when nothing is hit (None == None)
        self.last_over = maybe_id.clone();
        let maybe_id = maybe_id.filter(|id| self.find_node(&self.root.inner, id).is_some());

        // over/out on target change for move/drag (renderer.ts:3742-3763)
        if !same_element
            && (event.event_type == crate::parse_mouse::MouseEventType::Drag
                || event.event_type == crate::parse_mouse::MouseEventType::Move)
        {
            let prev = self.last_over_hover.take();
            if let Some(prev_id) = prev {
                if Some(&prev_id) != self.captured.as_ref() {
                    let mut out = MouseEvent::new(Some(&prev_id), &event, None, false)
                        .with_type(crate::parse_mouse::MouseEventType::Out);
                    self.send_mouse_event(&prev_id, &mut out);
                }
            }
            if let Some(id) = &maybe_id {
                let mut over = MouseEvent::new(Some(id), &event, self.captured.as_deref(), false)
                    .with_type(crate::parse_mouse::MouseEventType::Over);
                self.send_mouse_event(id, &mut over);
            }
            self.last_over_hover = maybe_id.clone();
        }

        // captured renderable routing (renderer.ts:3765-3792)
        if let Some(captured) = self.captured.clone() {
            if event.event_type != crate::parse_mouse::MouseEventType::Up {
                let mut evt = MouseEvent::new(Some(&captured), &event, None, false);
                self.send_mouse_event(&captured, &mut evt);
                return true;
            }
            // drag-end + up + drop (renderer.ts:3771-3792)
            let mut drag_end = MouseEvent::new(Some(&captured), &event, None, false)
                .with_type(crate::parse_mouse::MouseEventType::DragEnd);
            self.send_mouse_event(&captured, &mut drag_end);
            let mut up = MouseEvent::new(Some(&captured), &event, None, false);
            self.send_mouse_event(&captured, &mut up);
            if let Some(id) = &maybe_id {
                let mut drop = MouseEvent::new(Some(id), &event, Some(&captured), false)
                    .with_type(crate::parse_mouse::MouseEventType::Drop);
                self.send_mouse_event(id, &mut drop);
            }
            self.last_over = Some(captured.clone());
            self.last_over_hover = Some(captured);
            self.captured = None;
            return true;
        }

        if let Some(id) = maybe_id {
            // drag with the left button captures the target
            // (renderer.ts:3795-3800)
            if event.event_type == crate::parse_mouse::MouseEventType::Drag
                && event.button == MouseButton::Left as i32
            {
                self.captured = Some(id.clone());
            }
            self.dispatch_mouse_event(&id, &event);
        }
        true
    }

    // -----------------------------------------------------------------
    // Focus — renderer.ts:1407-1439
    // -----------------------------------------------------------------

    /// `focusRenderable(renderable)` (renderer.ts:1407-1424).
    pub fn focus_renderable(&mut self, id: &str) {
        if self.focused.as_deref() == Some(id) {
            return;
        }
        let previous = self.focused.clone();
        let previous_editor = self.current_focused_editor();
        self.focused = Some(id.to_owned());

        // blur the previous renderable's state (Renderable.blur,
        // Renderable.ts:445-460)
        if let Some(prev_id) = &previous {
            self.apply_focus_state(prev_id, false);
        }
        // focus the new one (Renderable.focus, Renderable.ts:399-430)
        self.apply_focus_state(id, true);

        let current_editor = self.current_focused_editor();
        if previous_editor != current_editor {
            self.emit_event(CliRenderEvent::FocusedEditor {
                current: current_editor,
                previous: previous_editor,
            });
        }
        self.emit_event(CliRenderEvent::FocusedRenderable {
            current: Some(id.to_owned()),
            previous,
        });
    }

    /// `blurRenderable(renderable)` (renderer.ts:1426-1439).
    pub fn blur_renderable(&mut self, id: &str) {
        if self.focused.as_deref() != Some(id) {
            return;
        }
        let previous_editor = self.current_focused_editor();
        self.focused = None;
        self.apply_focus_state(id, false);
        if previous_editor.is_some() {
            self.emit_event(CliRenderEvent::FocusedEditor {
                current: None,
                previous: previous_editor,
            });
        }
        self.emit_event(CliRenderEvent::FocusedRenderable {
            current: None,
            previous: Some(id.to_owned()),
        });
    }

    fn apply_focus_state(&mut self, id: &str, focused: bool) {
        if let Some(node) = Self::find_node_mut(&mut self.root.inner, id) {
            node.focused = focused;
            match &mut node.content {
                Content::Input(state) => {
                    if focused {
                        state.focus();
                    } else {
                        state.blur();
                    }
                }
                Content::Textarea(state) => {
                    // `Textarea.focus`/`blur` (Textarea.ts:310-320) — the
                    // effective-colors flip
                    if focused {
                        state.focus();
                    } else {
                        state.blur();
                    }
                }
                _ => {}
            }
        }
    }

    /// `currentFocusedRenderable` (renderer.ts:1381-1382).
    #[must_use]
    pub fn current_focused_renderable(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Observe an input renderable's value (`input.value`).
    #[must_use]
    pub fn input_value(&self, id: &str) -> Option<String> {
        let node = self.find_node(&self.root.inner, id)?;
        match &node.content {
            Content::Input(state) => Some(state.value().to_owned()),
            _ => None,
        }
    }

    /// Run an edit action on an input renderable — the host-side
    /// application of keymap edit commands (`withFocusedEditor`,
    /// edit-buffer-bindings.ts:433-440).
    pub fn run_input_action(&mut self, id: &str, action: crate::input::InputAction) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        match &mut node.content {
            Content::Input(state) => state.run_action(action),
            _ => false,
        }
    }

    /// Toggle the textarea mapping suspension on an input — the
    /// renderer side of `registerTextareaMappingSuspension`
    /// (edit-buffer-bindings.ts:374-431): with the keymap owning the
    /// keys, the input's own binding map stands down.
    pub fn set_input_mapping_suspension(&mut self, id: &str, suspended: bool) {
        if let Some(node) = Self::find_node_mut(&mut self.root.inner, id) {
            if let Content::Input(state) = &mut node.content {
                state.suspend_mappings = suspended;
            }
        }
    }

    /// `currentFocusedEditor` (renderer.ts:1384-1388) — the focused
    /// renderable when it is an EditBuffer family member (Input or
    /// Textarea; `isEditBufferRenderable`, EditBufferRenderable.ts:51).
    #[must_use]
    pub fn current_focused_editor(&self) -> Option<String> {
        let id = self.focused.as_ref()?;
        let node = self.find_node(&self.root.inner, id)?;
        match node.content {
            // `isEditBufferRenderable` (EditBufferRenderable.ts:51-53):
            // both the Input and the Textarea are EditBuffer family
            Content::Input(_) | Content::Textarea(_) => Some(id.clone()),
            _ => None,
        }
    }

    /// Focus tree snapshot — the queries `@opentui/keymap`'s host
    /// needs (`getParentTarget`, `getFocusedTarget`,
    /// keymap opentui.ts:60-71): node → parent links plus the focused
    /// id. Feeds [`opentui_keymap`]'s `OpenTuiHost::add`/`set_focused`.
    #[must_use]
    pub fn focus_tree(&self) -> (HashMap<String, Option<String>>, Option<String>) {
        let mut parents = HashMap::new();
        fn walk(
            node: &Renderable,
            parent: Option<&str>,
            parents: &mut HashMap<String, Option<String>>,
        ) {
            parents.insert(node.id.0.clone(), parent.map(str::to_owned));
            for child in &node.children {
                walk(child, Some(node.id.0.as_str()), parents);
            }
        }
        walk(&self.root.inner, None, &mut parents);
        (parents, self.focused.clone())
    }

    // -----------------------------------------------------------------
    // Renderer events — CliRenderEvents (renderer.ts:750-767)
    // -----------------------------------------------------------------

    /// `renderer.on(CliRenderEvents.X, listener)`.
    pub fn on_event<F>(&mut self, listener: F)
    where
        F: FnMut(&CliRenderEvent) + Send + 'static,
    {
        self.event_listeners.push(Box::new(listener));
    }

    fn emit_event(&mut self, event: CliRenderEvent) {
        for listener in &mut self.event_listeners {
            listener(&event);
        }
    }

    // -----------------------------------------------------------------
    // Terminal cursor — setCursorPosition (renderer.ts:4149-4155)
    // -----------------------------------------------------------------

    /// The Input-family cursor, folded into the one
    /// [`CursorOutputState`] the frame emits (the textarea cursors
    /// arrive via `prepare_paint` earlier in `render`; one coherent
    /// renderCursor pipeline — EditBufferRenderable.ts:1010-1022).
    fn update_terminal_cursor(&mut self) {
        // only the focused editor shows the cursor; +1 for 1-based
        // terminal coordinates (line 1017-1018)
        if let Some(id) = self.focused.clone() {
            if let Some(node) = self.find_node(&self.root.inner, &id) {
                if let Content::Input(state) = &node.content {
                    let x = node.screen_x + state.visual_cursor_col() as i32;
                    let y = node.screen_y;
                    self.cursor_state.set_position(
                        (x + 1).max(1) as u32,
                        (y + 1).max(1) as u32,
                        true,
                    );
                }
            }
        }
    }

    /// The terminal cursor position (1-based, upstream coordinates)
    /// and visibility — `renderCursor` + `setCursorPosition`. Sourced
    /// from the same [`CursorOutputState`] the frame emission writes
    /// (the oracle greps the emitted CUP + show sequence).
    #[must_use]
    pub fn terminal_cursor(&self) -> (u32, u32, bool) {
        if self.cursor_state.visible {
            (self.cursor_state.x, self.cursor_state.y, true)
        } else {
            (0, 0, false)
        }
    }

    // -----------------------------------------------------------------
    // Input feed — the StdinParser slice (stdin-parser.ts) +
    // handleStdinEvent (renderer.ts:3498-3545)
    // -----------------------------------------------------------------

    /// Feed raw stdin bytes. The events lane's parser handles the unit
    /// classes the input family produces: mouse sequences (SGR + X10,
    /// runs included), bracketed paste with partial-chunk reassembly,
    /// terminal focus sequences, and complete key units. Incomplete
    /// units (a lone ESC awaiting timeout disambiguation, split UTF-8
    /// codepoints, lone high bytes) stay pending across feeds —
    /// matching the oracle's immediate-event behavior. The full
    /// protocol-context state machine (CPR/DA/pixel-resolution
    /// deferral) is the tier-2 `StdinParser` follow-up port.
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            // Preserve the existing empty-chunk → empty-keypress
            // behavior (stdin-parser.ts:735-740)
            let parsed = parse_keypress(
                "",
                ParseKeypressOptions {
                    use_kitty_keyboard: self.use_kitty_keyboard(),
                },
            );
            if let Some(key) = parsed {
                self.dispatch_key(key);
            }
            return;
        }
        // Active paste: everything flows through the collector
        // (stdin-parser.ts:744-747)
        if let Some(mut collector) = self.paste_collector.take() {
            match collector.consume(bytes) {
                Some((paste, remainder)) => {
                    self.dispatch_paste(paste);
                    if !remainder.is_empty() {
                        self.feed_bytes(&remainder);
                    }
                }
                None => {
                    self.paste_collector = Some(collector);
                }
            }
            return;
        }
        self.feed_pending.extend_from_slice(bytes);
        self.drain_feed_units();
    }

    /// `renderer.useKittyKeyboard` (renderer.ts:1975-1983): the parser
    /// flag — true when the protocol is configured at all.
    #[must_use]
    pub fn use_kitty_keyboard(&self) -> bool {
        self.config.kitty_keyboard.is_some()
    }

    /// The configured kitty flags (`buildKittyKeyboardFlags`,
    /// renderer.ts:1131-1133).
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> u32 {
        build_kitty_keyboard_flags(self.config.kitty_keyboard.as_ref())
    }

    /// Process complete units off the pending buffer (the
    /// `scanPending` loop's unit classes, stdin-parser.ts:905+).
    fn drain_feed_units(&mut self) {
        loop {
            // Paste start at a unit boundary (stdin-parser.ts:753-756)
            if self
                .feed_pending
                .starts_with(crate::paste::BRACKETED_PASTE_START)
            {
                let start_len = crate::paste::BRACKETED_PASTE_START.len();
                self.feed_pending.drain(..start_len);
                self.paste_collector = Some(BracketedPasteCollector::new());
                let rest = std::mem::take(&mut self.feed_pending);
                if !rest.is_empty() {
                    self.feed_bytes(&rest);
                }
                return;
            }

            // Mouse sequences route before keys (handleStdinEvent,
            // renderer.ts:3521-3527)
            if let Some(consumed) = self
                .mouse_parser
                .parse_mouse_sequence_len(&self.feed_pending, 0)
            {
                let events = self
                    .mouse_parser
                    .parse_all_mouse_events(&self.feed_pending[..consumed]);
                if !events.is_empty() {
                    if self.config.use_mouse {
                        for event in events {
                            self.process_mouse_event(event);
                        }
                    }
                    self.feed_pending.drain(..consumed);
                    continue;
                }
            }

            let buf = self.feed_pending.clone();
            if buf.is_empty() {
                return;
            }

            // Terminal focus sequences (renderer.ts:3446-3471)
            if buf == b"\x1b[I" || buf == b"\x1b[O" {
                let focus = buf == b"\x1b[I";
                self.emit_event(if focus {
                    CliRenderEvent::TerminalFocus
                } else {
                    CliRenderEvent::TerminalBlur
                });
                self.feed_pending.drain(..buf.len());
                continue;
            }

            let Some(unit_len) = complete_unit_len(&buf) else {
                return; // incomplete unit stays pending
            };
            if unit_len == 0 {
                return;
            }
            let unit: Vec<u8> = self.feed_pending.drain(..unit_len).collect();
            // Valid UTF-8 decodes as such (the parser's KEY_DECODER,
            // stdin-parser.ts:548-550); non-UTF-8 bytes take the
            // latin1 path (decodeLatin1, stdin-parser.ts:544-546)
            let raw = match String::from_utf8(unit.clone()) {
                Ok(text) => text,
                Err(_) => unit
                    .iter()
                    .map(|b| char::from_u32(u32::from(*b)).unwrap_or('\u{fffd}'))
                    .collect(),
            };
            let parsed = parse_keypress(
                &raw,
                ParseKeypressOptions {
                    use_kitty_keyboard: self.use_kitty_keyboard(),
                },
            );
            if let Some(key) = parsed {
                self.dispatch_key(key);
            }
            // response/unknown sequences produce no key event (the
            // sequence handler chain is a follow-up port)
        }
    }

    /// `handleStdinEvent` key branch (renderer.ts:3514-3520): global
    /// key listeners first (KeyHandler), then the focused
    /// renderable's `handleKeyPress` (the internal-handler
    /// registration of Renderable.focus, Renderable.ts:406-427).
    fn dispatch_key(&mut self, key: crate::parse_keypress::ParsedKey) {
        let mut event = KeyEvent::new(key);
        // globals (KeyHandler global listeners)
        self.key_input.process_parsed_key(event.key.clone());
        // the focused renderable only when the globals did not
        // prevent or stop the event (KeyHandler.ts:179-181)
        if event.default_prevented() || event.propagation_stopped() {
            return;
        }
        self.deliver_key_to_focused(&mut event);
    }

    fn deliver_key_to_focused(&mut self, event: &mut KeyEvent) {
        let Some(id) = self.focused.clone() else {
            return;
        };
        let is_textarea = self
            .find_node(&self.root.inner, &id)
            .is_some_and(|n| matches!(n.content, Content::Textarea(_)));
        if is_textarea {
            // the focused textarea's handleKeyPress (Renderable.focus
            // internal-handler registration, Renderable.ts:406-427)
            self.textarea_key_press(&id, event);
            return;
        }
        let Some(node) = Self::find_node_mut(&mut self.root.inner, &id) else {
            return;
        };
        if let Content::Input(state) = &mut node.content {
            state.handle_key(event);
        }
    }

    /// The paste branch (renderer.ts:3528-3530).
    fn dispatch_paste(&mut self, bytes: Vec<u8>) {
        // global paste listeners
        self.key_input.process_paste(bytes.clone(), None);
        // focused renderable
        let Some(id) = self.focused.clone() else {
            return;
        };
        let is_textarea = self
            .find_node(&self.root.inner, &id)
            .is_some_and(|n| matches!(n.content, Content::Textarea(_)));
        if is_textarea {
            // `handlePaste` (Textarea.ts:259-261)
            self.with_textarea_mut(&id, |state, _pos, _selection| {
                state.handle_paste(&bytes);
                true
            });
            return;
        }
        let Some(node) = Self::find_node_mut(&mut self.root.inner, &id) else {
            return;
        };
        if let Content::Input(state) = &mut node.content {
            let mut event = PasteEvent::new(bytes, None);
            state.handle_paste(&mut event);
        }
    }
}

/// The length of the first complete protocol unit in `data`, or
/// `None` while the unit is incomplete (the StdinParser keeps at most
/// one incomplete unit pending — stdin-parser.ts:579-586). Unit
/// classes: single ASCII bytes, `ESC` + final / CSI / SS3 / OSC / DCS
/// sequences, and complete UTF-8 codepoints. Lone ESC, unterminated
/// OSC/DCS, and bytes that cannot start a unit (continuation bytes,
/// invalid leads) stay pending — upstream resolves them on timeout.
fn complete_unit_len(data: &[u8]) -> Option<usize> {
    let first = *data.first()?;
    if first != 0x1b {
        // UTF-8 lead byte: complete when all continuation bytes exist
        let len = match first {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            // continuation bytes without a lead / invalid leads wait
            // for the timeout (emitLegacyHighByte, stdin-parser.ts:1870)
            _ => return None,
        };
        if data.len() < len {
            return None;
        }
        if len > 1 && !data[1..len].iter().all(|b| (0x80..=0xbf).contains(b)) {
            // broken sequence: the maximal subpart is its own unit
            return Some(1);
        }
        return Some(len);
    }

    // The esc state consumes EVERY leading ESC (ESC ESC stays in esc —
    // terminals encode Alt+ESC as ESC ESC [...], stdin-parser.ts:1043-1047)
    let mut i = 0;
    while i < data.len() && data[i] == 0x1b {
        i += 1;
    }
    if i == data.len() {
        return None; // lone ESC(s): timeout resolution (follow-up port)
    }
    // The byte after the ESCs determines the sub-protocol
    // (stdin-parser.ts:1020-1042)
    match data[i] {
        b'[' => {
            // CSI: any bytes then a final byte 0x40-0x7e. A second `[`
            // right after ESC[ (absolute index 2) is PAYLOAD, not a
            // final byte — the cygwin `ESC [[ A` variants
            // (stdin-parser.ts:1186-1193).
            let mut j = i + 1;
            if j == 2 && data.get(2) == Some(&b'[') {
                j = 3;
            }
            while j < data.len() {
                let b = data[j];
                if (0x40..=0x7e).contains(&b) {
                    return Some(j + 1);
                }
                j += 1;
            }
            None
        }
        b'O' | b'N' => {
            if data.len() >= i + 2 {
                Some(i + 2)
            } else {
                None
            }
        }
        b']' | b'P' | b'X' | b'^' | b'_' => {
            // OSC / DCS / SOS / PM / APC: to BEL (OSC only) or ST
            let is_osc = data[i] == b']';
            let mut j = i + 1;
            while j < data.len() {
                if is_osc && data[j] == 0x07 {
                    return Some(j + 1);
                }
                if data[j] == 0x1b && data.get(j + 1) == Some(&b'\\') {
                    return Some(j + 2);
                }
                j += 1;
            }
            None
        }
        _ => Some(i + 1), // ESC + byte: a complete two-byte escape
    }
}

/// Mutable node lookup by id (recursive).
fn find_node_mut<'a>(root: &'a mut Renderable, id: &str) -> Option<&'a mut Renderable> {
    if root.id.0 == id {
        return Some(root);
    }
    root.children.iter_mut().find_map(|c| find_node_mut(c, id))
}

/// A diff-side wrapper's Code concealment source map (the
/// `LineNumberState` payload — Code.ts `_renderedLineSources`).
fn wrapper_source_map(wrapper: &Renderable) -> Option<Vec<i64>> {
    match &wrapper.content {
        Content::LineNumber(state) => state.source_map.clone(),
        _ => None,
    }
}

/// A diff-side wrapper's target line sources — the wrapped virtual
/// lines' source rows remapped through the concealment source map
/// (`CodeRenderable.lineInfo`, Code.ts:147-160).
fn wrapper_text_sources(wrapper: &Renderable) -> Vec<u32> {
    let Some(target) = wrapper
        .children
        .iter()
        .skip(1)
        .find(|c| matches!(c.content, Content::Text(_)))
    else {
        return Vec::new();
    };
    let Content::Text(text) = &target.content else {
        return Vec::new();
    };
    let wrap_width = if text.wrap_mode == crate::renderables::text::WrapMode::None {
        None
    } else {
        Some(text.viewport.map_or(0, |vp| vp.width))
    }
    .filter(|w| *w > 0);
    let vlines = crate::text_buffer::compute_virtual_lines(&text.doc, text.wrap_mode, wrap_width);
    let source_map = wrapper_source_map(wrapper);
    vlines
        .iter()
        .map(|v| {
            source_map
                .as_ref()
                .and_then(|map| map.get(v.source_line as usize).copied())
                .filter(|s| *s >= 0)
                .map_or(v.source_line, |s| s as u32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dimension, LayoutOptions};
    use crate::renderable::{Content, Renderable};
    use crate::renderables::box_::BoxOptions;
    use crate::renderables::text::TextOptions;

    fn text_renderable(id: &str, content: &str) -> Renderable {
        let opts = TextOptions {
            content: crate::renderables::text::ContentInput::Str(content.to_owned()),
            ..Default::default()
        };
        let state = crate::renderables::text::TextRenderableState::from_options(&opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
            ..Default::default()
        };
        Renderable::new(
            &mut TaffyTree::new(),
            id,
            LayoutOptions::default(),
            Dimension::Auto,
            Dimension::Auto,
            Content::Text(state),
            Some(ctx),
        )
    }

    // helper: build with a real renderer-owned tree
    fn add_text(renderer: &mut CliRenderer, id: &str, content: &str) {
        let opts = TextOptions {
            content: crate::renderables::text::ContentInput::Str(content.to_owned()),
            ..Default::default()
        };
        let state = crate::renderables::text::TextRenderableState::from_options(&opts);
        let ctx = MeasureCtx {
            kind: MeasureKind::Text,
            text: Some(state.measured()),
            ..Default::default()
        };
        let r = Renderable::new(
            &mut renderer.tree,
            id,
            LayoutOptions::default(),
            Dimension::Auto,
            Dimension::Auto,
            Content::Text(state),
            Some(ctx),
        );
        renderer.add(r);
    }

    fn add_box(renderer: &mut CliRenderer, id: &str, opts: BoxOptions) {
        let state = crate::renderables::box_::BoxRenderableState::from_options(&opts);
        let mut layout = opts.layout.clone();
        state.apply_yoga_borders(&mut layout);
        let r = Renderable::new(
            &mut renderer.tree,
            id,
            layout,
            opts.width.unwrap_or(Dimension::Auto),
            opts.height.unwrap_or(Dimension::Auto),
            Content::Box(state),
            None,
        );
        renderer.add(r);
    }

    #[test]
    fn hello_smoke_renders_text_at_origin() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 5,
            ..Default::default()
        });
        add_text(&mut renderer, "t1", "hello");
        renderer.render();
        let frame = renderer.current_render_buffer.char_frame();
        let rows: Vec<&str> = frame.split('\n').collect();
        assert_eq!(rows[0].trim_end(), "hello");
        // fg = literal white, bg transparent black (TextBufferRenderable defaults)
        let spans = renderer.current_render_buffer.get_span_lines();
        let s0 = &spans[0].spans[0];
        assert_eq!(s0.text.trim_end(), "hello");
        assert_eq!(s0.fg, Rgba::from_values(1.0, 1.0, 1.0, 1.0));
        assert_eq!(s0.bg.alpha(), 0);
    }

    #[test]
    fn sgr_stream_contains_text_and_colors() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 10,
            height: 3,
            ..Default::default()
        });
        add_text(&mut renderer, "t1", "hi");
        renderer.render();
        let sgr = renderer.last_frame_sgr().to_owned();
        // first frame is a full repaint: contains CUP + fg SGR + text + reset
        assert!(sgr.contains("\x1b[1;1H"));
        assert!(sgr.contains("hi"));
        assert!(sgr.contains("\x1b[38;2;255;255;255m"));
        assert!(sgr.ends_with("\x1b[0m"));
        // second frame with no changes emits nothing (no-op suppression)
        renderer.render();
        assert_eq!(renderer.last_frame_sgr(), "");
    }

    #[test]
    fn box_lays_out_children_in_column() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 10,
            ..Default::default()
        });
        add_box(
            &mut renderer,
            "b1",
            BoxOptions {
                border: Some(crate::renderables::box_::BorderSpec::All),
                width: Some(Dimension::Points(10.0)),
                height: Some(Dimension::Points(5.0)),
                ..Default::default()
            },
        );
        renderer.render();
        let frame = renderer.current_render_buffer.char_frame();
        let rows: Vec<&str> = frame.split('\n').collect();
        assert_eq!(rows[0].trim_end(), "┌────────┐");
        assert_eq!(rows[1].trim_end(), "│        │");
        assert_eq!(rows[4].trim_end(), "└────────┘");
    }

    #[test]
    fn flex_grow_splits_space() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 3,
            ..Default::default()
        });
        let layout = LayoutOptions {
            flex_direction: Some("row".into()),
            ..Default::default()
        };
        let b1_layout = LayoutOptions {
            flex_grow: Some(1.0),
            ..Default::default()
        };
        let b1 = Renderable::new(
            &mut renderer.tree,
            "b1",
            b1_layout,
            Dimension::Auto,
            Dimension::Points(1.0),
            Content::Box(crate::renderables::box_::BoxRenderableState::from_options(
                &BoxOptions {
                    background_color: Some("#FF0000".into()),
                    should_fill: Some(true),
                    ..Default::default()
                },
            )),
            None,
        );
        renderer.root.inner.layout = layout;
        let mut root_style = tp::Style {
            flex_direction: tp::FlexDirection::Row,
            ..Default::default()
        };
        // keep the root's terminal size (RootRenderable sets it; a bare
        // default Style would wipe width/height to AUTO)
        root_style.size.width = tp::Dimension::length(20.0);
        root_style.size.height = tp::Dimension::length(3.0);
        let _ = renderer
            .tree
            .set_style(renderer.root.inner.node, root_style);
        renderer.add(b1);
        renderer.render();
        // flexGrow 1 in a 20-wide row → fills the row with red
        let cell = renderer.current_render_buffer.get(19, 0).unwrap();
        assert_eq!(cell.bg, Rgba::rgb(255, 0, 0));
    }

    #[test]
    fn kitty_keyboard_flags_defaults() {
        // renderer.ts:616-651 — None → 0 (disabled)
        assert_eq!(build_kitty_keyboard_flags(None), 0);
        // empty options: disambiguate + alternate keys default ON
        let flags = build_kitty_keyboard_flags(Some(&KittyKeyboardOptions::default()));
        assert_eq!(flags, 0b101);
        // explicit false disables a default flag
        let flags = build_kitty_keyboard_flags(Some(&KittyKeyboardOptions {
            disambiguate: Some(false),
            ..Default::default()
        }));
        assert_eq!(flags, 0b100);
        // optional flags only when explicitly true
        let flags = build_kitty_keyboard_flags(Some(&KittyKeyboardOptions {
            events: Some(true),
            report_text: Some(true),
            ..Default::default()
        }));
        assert_eq!(flags, 0b10000 | 0b10 | 0b101);
        // allKeysAsEscapes alone
        let flags = build_kitty_keyboard_flags(Some(&KittyKeyboardOptions {
            all_keys_as_escapes: Some(true),
            ..Default::default()
        }));
        assert_eq!(flags, 0b1000 | 0b101);
    }

    #[test]
    fn standalone_text_renderable_helper_works() {
        let r = text_renderable("t", "x");
        assert_eq!(r.id.0, "t");
    }

    // --- reconciler-facing mutations (solid lane) ---------------------------

    mod mutations {
        use super::*;

        fn box_node(renderer: &mut CliRenderer, id: &str, w: f32) -> Renderable {
            let opts = BoxOptions {
                width: Some(Dimension::Points(w)),
                height: Some(Dimension::Points(1.0)),
                ..Default::default()
            };
            renderer.make_box(id, &opts)
        }

        fn setup() -> CliRenderer {
            create_cli_renderer(CliRendererConfig {
                width: 40,
                height: 3,
                ..Default::default()
            })
        }

        #[test]
        fn insert_at_reorders_children() {
            let mut r = setup();
            let a = box_node(&mut r, "a", 5.0);
            r.add(a);
            let b = box_node(&mut r, "b", 5.0);
            r.add(b);
            assert_eq!(r.child_ids("__root__").unwrap(), vec!["a", "b"]);
            // insert c before a (index 0)
            let c = box_node(&mut r, "c", 5.0);
            r.insert_child_at("__root__", 0, c);
            assert_eq!(r.child_ids("__root__").unwrap(), vec!["c", "a", "b"]);
            // clamp: index past the end appends
            let d = box_node(&mut r, "d", 5.0);
            r.insert_child_at("__root__", 99, d);
            assert_eq!(r.child_ids("__root__").unwrap(), vec!["c", "a", "b", "d"]);
            r.render();
            let frame = r.current_render_buffer.char_frame();
            assert!(!frame.is_empty());
        }

        #[test]
        fn move_is_remove_then_insert() {
            let mut r = setup();
            for id in ["a", "b", "c"] {
                let n = box_node(&mut r, id, 5.0);
                r.add(n);
            }
            // move a to the end: remove + insert_at(len)
            assert!(r.unmount_child("__root__", "a"));
            let a = box_node(&mut r, "a", 5.0);
            r.insert_child_at("__root__", 2, a);
            assert_eq!(r.child_ids("__root__").unwrap(), vec!["b", "c", "a"]);
        }

        #[test]
        fn unmount_removes_subtree() {
            let mut r = setup();
            let mut parent = box_node(&mut r, "p", 10.0);
            let child = box_node(&mut r, "child", 4.0);
            parent.add(&mut r.tree, child);
            r.add(parent);
            assert_eq!(r.child_ids("p").unwrap(), vec!["child"]);
            assert!(r.unmount_child("__root__", "p"));
            assert!(r.child_ids("__root__").unwrap().is_empty());
            assert!(r.root.inner.find("child").is_none());
            assert!(r.root.inner.find("p").is_none());
            // unknown ids report false, no panic
            assert!(!r.unmount_child("__root__", "nope"));
            assert!(!r.unmount_child("nope", "p"));
        }

        #[test]
        fn restyle_updates_layout_and_rerenders() {
            let mut r = setup();
            let a = box_node(&mut r, "a", 10.0);
            r.add(a);
            r.render();
            let a = r.root.inner.find("a").unwrap();
            assert_eq!(a.w, 10.0);
            let layout = crate::layout::LayoutOptions {
                flex_direction: Some("row".into()),
                ..Default::default()
            };
            assert!(r.restyle_node(
                "a",
                layout,
                Some(Dimension::Points(20.0)),
                Some(Dimension::Points(2.0))
            ));
            let layout2 = crate::layout::LayoutOptions::default();
            assert!(!r.restyle_node("missing", layout2, None, None));
            r.render();
            let a = r.root.inner.find("a").unwrap();
            assert_eq!((a.w, a.h), (20.0, 2.0));
        }
    }

    // --- scroll family -----------------------------------------------------

    mod scroll {
        use super::*;
        use crate::renderables::scrollbar::ScrollUnit;
        use crate::renderables::scrollbox::{
            self, ScrollBoxOptions, ScrollDirection, ScrollEvent, StickyStart,
        };
        use crate::scroll_acceleration::MacOSScrollAccel;

        fn mounted_scrollbox() -> CliRenderer {
            let mut renderer = create_cli_renderer(CliRendererConfig {
                width: 20,
                height: 8,
                ..Default::default()
            });
            let mut opts = ScrollBoxOptions {
                scroll_y: true,
                ..Default::default()
            };
            opts.box_options.width = Some(Dimension::Points(20.0));
            opts.box_options.height = Some(Dimension::Points(8.0));
            let sb = renderer.make_scrollbox("sb", &mut opts);
            renderer.add(sb);
            for i in 0..10 {
                let t =
                    text_renderable_node(&mut renderer, &format!("t{i}"), &format!("line-{i:02}"));
                renderer.mount_child("sb", t);
            }
            renderer.render();
            renderer
        }

        fn text_renderable_node(renderer: &mut CliRenderer, id: &str, content: &str) -> Renderable {
            let opts = TextOptions {
                content: crate::renderables::text::ContentInput::Str(content.to_owned()),
                ..Default::default()
            };
            renderer.make_text(id, &opts)
        }

        fn scroll_top_of(renderer: &CliRenderer) -> f32 {
            scrollbox::scroll_top(&renderer.root.inner, "sb").unwrap()
        }

        /// Internal scrollbox node ids carry the process-global
        /// `ScrollBoxRenderable.idCounter` suffix (ScrollBox.ts:110 —
        /// upstream is a class static, so the number depends on how many
        /// scrollboxes the test process already created). Resolve the
        /// actual ids through the renderer's root→content routing table
        /// instead of hard-coding `-0`.
        fn scroll_node<'a>(renderer: &'a CliRenderer, kind: &str) -> &'a Renderable {
            let content_id = renderer.content_of.get("sb").expect("sb content id");
            let suffix = content_id.rsplit('-').next().expect("counter suffix");
            renderer
                .root
                .inner
                .find(&format!("scroll-box-{kind}-{suffix}"))
                .unwrap_or_else(|| panic!("scroll-box-{kind}-{suffix}"))
        }

        fn translate_y_of(renderer: &CliRenderer) -> f32 {
            let content_id = renderer.content_of.get("sb").expect("sb content id");
            renderer
                .root
                .inner
                .find(content_id)
                .expect("content node")
                .translate_y
        }

        #[test]
        fn settles_with_visible_vertical_bar() {
            // the oracle-pinned settled state: scrollSize 10, viewportSize
            // 8, slider max 2 / viewPortSize 3 (path-dependent transient)
            let r = mounted_scrollbox();
            assert_eq!(scroll_top_of(&r), 0.0);
            assert_eq!(translate_y_of(&r), 0.0);
            let vp = scroll_node(&r, "viewport");
            assert_eq!((vp.w, vp.h), (19.0, 8.0));
            let bar = scroll_node(&r, "vertical-scrollbar");
            let crate::renderable::Content::ScrollBar(b) = &bar.content else {
                panic!("bar state")
            };
            assert_eq!((b.scroll_size, b.viewport_size), (10.0, 8.0));
            assert!(bar.visible);
            let hbar = scroll_node(&r, "horizontal-scrollbar");
            assert!(!hbar.visible);
        }

        #[test]
        fn scroll_top_clamps_and_translates() {
            let mut r = mounted_scrollbox();
            r.set_scroll_top("sb", 3.0);
            // clamp to max = 10 - 8 = 2 (ScrollBar.ts:72-74)
            assert_eq!(scroll_top_of(&r), 2.0);
            assert_eq!(translate_y_of(&r), -2.0);
            r.set_scroll_top("sb", f32::MIN);
            assert_eq!(scroll_top_of(&r), 0.0);
            assert_eq!(translate_y_of(&r), 0.0);
        }

        #[test]
        fn scroll_by_viewport_units() {
            // ScrollBar.ts:246-258 — viewport unit multiplies by viewportSize
            let mut r = mounted_scrollbox();
            r.scroll_by("sb", 0.0, 0.25, ScrollUnit::Viewport);
            assert_eq!(scroll_top_of(&r), 2.0); // 0.25 × 8 = 2, clamped
            r.scroll_by("sb", 0.0, -0.25, ScrollUnit::Viewport);
            assert_eq!(scroll_top_of(&r), 0.0);
        }

        #[test]
        fn sticky_bottom_applies_on_first_layout() {
            let mut renderer = create_cli_renderer(CliRendererConfig {
                width: 20,
                height: 8,
                ..Default::default()
            });
            let mut opts = ScrollBoxOptions {
                scroll_y: true,
                sticky_scroll: true,
                sticky_start: Some(StickyStart::Bottom),
                ..Default::default()
            };
            opts.box_options.width = Some(Dimension::Points(20.0));
            opts.box_options.height = Some(Dimension::Points(8.0));
            let sb = renderer.make_scrollbox("sb", &mut opts);
            renderer.add(sb);
            for i in 0..10 {
                let t =
                    text_renderable_node(&mut renderer, &format!("t{i}"), &format!("sline-{i:02}"));
                renderer.mount_child("sb", t);
            }
            renderer.render();
            // applyStickyStart → max(0, 10-8) = 2 (ScrollBox.ts:257)
            assert_eq!(scroll_top_of(&renderer), 2.0);
            assert_eq!(translate_y_of(&renderer), -2.0);
        }

        #[test]
        fn wheel_seam_accumulates_and_uses_accel_curve() {
            // ScrollBox.ts:542-584 via MacOSScrollAccel: fractional deltas
            // accumulate; only integer parts scroll
            let mut r = mounted_scrollbox();
            r.scroll_event(
                "sb",
                &ScrollEvent {
                    direction: ScrollDirection::Down,
                    delta: 0.6, // LinearAccel ⇒ multiplier 1 (first tick)
                    shift: false,
                    now_ms: Some(1000.0),
                },
            );
            // 0.6 truncates to 0 → no scroll, accumulator keeps 0.6
            assert_eq!(scroll_top_of(&r), 0.0);
            r.scroll_event(
                "sb",
                &ScrollEvent {
                    direction: ScrollDirection::Down,
                    delta: 0.6,
                    shift: false,
                    now_ms: Some(1100.0),
                },
            );
            // 1.2 → trunc 1 → scrollTop 1
            assert_eq!(scroll_top_of(&r), 1.0);
            // shift remaps down → right (ScrollBox.ts:545-546)
            r.scroll_event(
                "sb",
                &ScrollEvent {
                    direction: ScrollDirection::Down,
                    delta: 5.0,
                    shift: true,
                    now_ms: Some(1200.0),
                },
            );
            let left = scrollbox::scroll_left(&r.root.inner, "sb").unwrap();
            assert_eq!(left, 0.0); // horizontal scroll clamps (content fits)
        }

        #[test]
        fn macos_accel_scales_wheel_deltas() {
            // burst ticks: multiplier > 1 → each delta pushes several cells
            // (here it saturates the 2-row scroll range and leaves residue
            // in the accumulator — the linear curve would leave < 1)
            let mut r = mounted_scrollbox();
            if let Some(sb) = r.root.inner.find_mut("sb") {
                if let crate::renderable::Content::ScrollBox(_, st) = &mut sb.content {
                    st.scroll_accel = std::sync::Mutex::new(Box::new(MacOSScrollAccel::default()));
                }
            }
            for t in [1000.0, 1020.0, 1040.0] {
                r.scroll_event(
                    "sb",
                    &ScrollEvent {
                        direction: ScrollDirection::Down,
                        delta: 1.0,
                        shift: false,
                        now_ms: Some(t),
                    },
                );
            }
            // dt=20ms ⇒ velocity 5 ⇒ multiplier 1 + 0.8·(e^{5/3} − 1) ≈ 4.4;
            // the range (2) saturates and the accumulator holds the excess
            assert_eq!(scroll_top_of(&r), 2.0);
            let residue = {
                let sb = r.root.inner.find("sb").unwrap();
                let crate::renderable::Content::ScrollBox(_, st) = &sb.content else {
                    unreachable!()
                };
                st.scroll_accumulator_y
            };
            assert!(
                residue > 0.5,
                "accelerated residue: {residue} (linear would be 0)"
            );
        }
    }
    // --- Diff / LineNumber lane -------------------------------------------------

    const UPSTREAM_SIMPLE_DIFF: &str = r#"--- a/test.js
+++ b/test.js
@@ -1,3 +1,3 @@
 function hello() {
-  console.log("Hello");
+  console.log("Hello, World!");
 }"#;

    /// Diff.test.ts:127-150 (unified view renders correctly) — the pinned
    /// snapshot (Diff.test.ts.snap:3-25): the 5-wide gutter, the minus /
    /// plus signs, and the shared line number 2 on the remove/add pair.
    #[test]
    fn diff_unified_renders_the_upstream_snapshot_frame() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 80,
            height: 20,
            ..Default::default()
        });
        let opts = diff::DiffOptions {
            diff: UPSTREAM_SIMPLE_DIFF.to_owned(),
            view: diff::DiffView::Unified,
            ..Default::default()
        };
        let d = renderer.make_diff("d", &opts, &crate::syntax_style::SyntaxStyle::create());
        renderer.add(d);
        renderer.render();
        let frame = renderer.current_render_buffer.char_frame();
        let rows: Vec<String> = frame.lines().map(|r| format!("{r:<80}")).collect();
        assert_eq!(
            rows[0],
            format!("{:<80}", " 1   function hello() {"),
            "snap line 4"
        );
        assert_eq!(
            rows[1],
            format!("{:<80}", r#" 2 -   console.log("Hello");"#),
            "snap line 5"
        );
        assert_eq!(
            rows[2],
            format!("{:<80}", r#" 2 +   console.log("Hello, World!");"#),
            "snap line 6"
        );
        assert_eq!(rows[3], format!("{:<80}", " 3   }"), "snap line 7");
        // rows 4+ blank (snap lines 8-23)
        assert!(rows[4..].iter().all(|r| r.trim().is_empty()));
    }

    /// Diff.test.ts:152-176 (split view renders correctly) — the pinned
    /// snapshot (Diff.test.ts.snap:27-49): both 50% panes with their own
    /// gutters, the change paired across the middle.
    #[test]
    fn diff_split_renders_the_upstream_snapshot_frame() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 80,
            height: 20,
            ..Default::default()
        });
        let opts = diff::DiffOptions {
            diff: UPSTREAM_SIMPLE_DIFF.to_owned(),
            view: diff::DiffView::Split,
            ..Default::default()
        };
        let d = renderer.make_diff("d", &opts, &crate::syntax_style::SyntaxStyle::create());
        renderer.add(d);
        renderer.render();
        let frame = renderer.current_render_buffer.char_frame();
        let rows: Vec<String> = frame.lines().map(|r| format!("{r:<80}")).collect();
        assert_eq!(
            rows[0],
            format!(
                "{:<80}",
                " 1   function hello() {                  1   function hello() {"
            ),
            "snap line 28"
        );
        assert_eq!(
            rows[1],
            format!(
                "{:<80}",
                r#" 2 -   console.log("Hello");             2 +   console.log("Hello, World!");"#
            ),
            "snap line 29"
        );
        assert_eq!(
            rows[2],
            format!("{:<80}", " 3   }                                   3   }"),
            "snap line 30"
        );
    }

    /// getHunkRowOffsets (Diff.ts:1254-1281) — the hunk-start visual
    /// rows against the left code line sources (Diff.test.ts:3182).
    #[test]
    fn diff_hunk_row_offsets_resolve_through_wrapping() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 80,
            height: 20,
            ..Default::default()
        });
        let opts = diff::DiffOptions {
            diff: UPSTREAM_SIMPLE_DIFF.to_owned(),
            ..Default::default()
        };
        let d = renderer.make_diff("d", &opts, &crate::syntax_style::SyntaxStyle::create());
        renderer.add(d);
        renderer.render();
        // one hunk starting at logical line 0, unwrapped → visual row 0
        assert_eq!(renderer.diff_hunk_row_offsets("d"), vec![0]);
    }
}
