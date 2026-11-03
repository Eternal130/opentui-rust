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

mod config;
mod events;
mod frame;
mod lifecycle;
mod scrollback;

pub use config::*;
pub use events::*;
// `frame`/`lifecycle` hold only `CliRenderer` impl blocks (no new pub
// items), so they need no glob re-export.
pub use scrollback::*;

pub(crate) use crate::buffer::OptimizedBuffer;
pub(crate) use crate::color::parse_color;
pub(crate) use crate::color::Rgba;
pub(crate) use crate::editor::SelectionBehavior;
pub(crate) use crate::events::{KeyEvent, KeyHandler, MouseButton, MouseEvent, PasteEvent};
pub(crate) use crate::input::{InputOptions, InputRenderableState};
pub(crate) use crate::layout::Dimension;
pub(crate) use crate::parse_keypress::{parse_keypress, ParseKeypressOptions};
pub(crate) use crate::parse_mouse::{MouseParser, RawMouseEvent};
pub(crate) use crate::paste::BracketedPasteCollector;
pub(crate) use crate::renderable::{
    Content, MeasureCtx, MeasureKind, Renderable, RootRenderable, WalkEffects,
};
pub(crate) use crate::renderables::box_::{BoxOptions, BoxRenderableState};
pub(crate) use crate::renderables::diff;
pub(crate) use crate::renderables::scrollbox;
pub(crate) use crate::renderables::text::{TextOptions, TextRenderableState};
pub(crate) use crate::renderables::textarea::{TextareaOptions, TextareaState};
pub(crate) use std::sync::Arc;
pub(crate) use taffy::tree::TaffyTree;
pub(crate) use taffy::prelude as tp;

pub trait RendererTerminalSession {
    /// Write raw escape bytes to the session's output and flush
    /// (`Terminal::output()`) — the title/mouse-toggle writes.
    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    /// `Terminal::pump_output` (pumps.rs — the `?2026` frame drain).
    fn pump_output(&mut self, renderer: &mut CliRenderer) -> std::io::Result<()>;
    /// `Terminal::teardown` — `performShutdownSequence`
    /// (renderer.zig:600-654).
    fn teardown(&mut self) -> std::io::Result<()>;
    /// `Terminal::is_torn_down`.
    fn is_torn_down(&self) -> bool;
}

#[cfg(feature = "terminal-backend")]
impl<T: crate::terminal::TtyDevice, W: std::io::Write> RendererTerminalSession
    for crate::terminal::Terminal<T, W>
{
    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let out = self.output();
        out.write_all(bytes)?;
        out.flush()
    }

    fn pump_output(&mut self, renderer: &mut CliRenderer) -> std::io::Result<()> {
        crate::terminal::Terminal::pump_output(self, renderer)
    }

    fn teardown(&mut self) -> std::io::Result<()> {
        crate::terminal::Terminal::teardown(self)
    }

    fn is_torn_down(&self) -> bool {
        crate::terminal::Terminal::is_torn_down(self)
    }
}

/// `CliRenderer` — the render loop owner.
/// A live mouse cell-selection (anchor + focus in screen coordinates).
struct MouseSelection {
    target: String,
    anchor: (i32, i32),
    focus: (i32, i32),
}

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
    /// the embedded terminal's inline focus request (forwardMouse,
    /// EmbeddedTerminal.ts:339) — flushed by the dispatch tail
    pending_focus: Option<String>,
    /// the mouse-driven cell selection (renderer.ts's selection block:
    /// `startSelection` on left-down over a selectable renderable,
    /// `updateSelection` while dragging — the embedded arm)
    mouse_selection: Option<MouseSelection>,
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
    // ----- OSC 8 links (A3) -----
    /// The link-id → URL table — upstream is the GLOBAL `LinkPool`
    /// (buffer.zig:239 `initGlobalLinkPool`, id space spans the whole
    /// process lifetime, NOT per-frame; emit reads it via
    /// `nextRenderBuffer.link_pool`, renderer.zig:2760). The port keeps
    /// the pool on the renderer: ids are allocated monotonically and
    /// interned per URL (link.zig `interned_live_ids`); the
    /// generation/refcount slot recycling (link.zig:7-10 id layout) is
    /// deferred until a consumer can retire links.
    link_pool: HashMap<u32, String>,
    /// live-URL interning table (link.zig `interned_live_ids`) — the
    /// same URL keeps its id across frames.
    link_interned: HashMap<String, u32>,
    next_link_id: u32,
    // ----- lifecycle (D1a) -----
    /// `_isDestroyed` (renderer.ts:789; getter `isDestroyed`,
    /// renderer.ts:1365-1366). After it flips, every lifecycle call
    /// is a no-op (upstream's `if (this._isDestroyed) return` guards).
    destroyed: bool,
    /// The dirty bit — the port's requestRender coalescing marker.
    /// Upstream has no explicit flag: the scheduled timer IS the
    /// marker (`updateScheduled`/`renderTimeout`,
    /// renderer.ts:1591-1604); the flag mirrors it for
    /// [`CliRenderer::is_render_pending`].
    render_pending: bool,
    /// The scheduler half of the renderer (crate::scheduler — the
    /// exact port of requestRender/scheduleRenderTimer/loop,
    /// renderer.ts:1543-1619, 4660-4838). The virtual clock makes the
    /// render loop deterministic: a settle pumps at most the timers
    /// the invalidations armed.
    scheduler: crate::scheduler::RenderScheduler,
    /// The renderer's theme mode — upstream keeps it inside
    /// `RendererThemeMode._themeMode` (renderer-theme-mode.ts:44);
    /// the port mirrors the reduced mode, fed by THEME_MODE events
    /// (theme.rs emits, renderer.ts:1214).
    theme_mode: Option<ThemeMode>,
    /// `RendererThemeMode.waiters` (renderer-theme-mode.ts:54-56) —
    /// the pending `waitForThemeMode` registrations.
    theme_waiters: Vec<(u64, Option<f64>)>,
    theme_waiter_seq: u64,
    /// Resolved waiter results (`waiter.resolve(mode)`,
    /// renderer-theme-mode.ts:76-83, 89-100, 223-235) — polled by id.
    theme_wait_resolved: HashMap<u64, Option<ThemeMode>>,
    /// `_useConsole` (renderer.ts:1625-1632) — the consoleMode arm.
    use_console: bool,
    /// The last `setTerminalTitle` value (renderer.ts:4112-4114);
    /// teardown resets the title to "" (terminal.zig:328).
    terminal_title: String,
    /// The live terminal session (`terminal-backend` feature) —
    /// attached by the embedder; `destroy()` tears it down.
    #[cfg(feature = "terminal-backend")]
    pub session: Option<Box<dyn RendererTerminalSession>>,
    // ----- split-footer scrollback runtime base (D1b-core) -----
    /// Whether the split-footer capture path is armed — the runtime
    /// base of upstream's `screenMode === "split-footer" &&
    /// externalOutputMode === "capture-stdout"` gate (the mode
    /// resolution itself, `resolveModes` renderer.ts:365-386, rides
    /// the D1b-surface config enums; the toggle here is what they
    /// will drive).
    split_capture_active: bool,
    /// `splitTailColumn` (renderer.ts:901) — where the last flushed
    /// commit left the cursor inside the transcript's last row.
    split_tail_column: u32,
    /// `externalOutputQueue` — the FIFO between commit producers and
    /// the render-loop flush (renderer.ts:403-441, fed at :2609).
    external_output_queue: crate::scrollback::ExternalOutputQueue,
    /// The headless transcript model — the byte stream the flush
    /// adjudication has committed above the footer (the differential
    /// harness's view; the live path writes the same bytes through the
    /// session).
    split_transcript: String,
    // ----- split-footer surface family (D1b-surface) -----
    /// `_screenMode` (renderer.ts:882, resolved at :1074).
    screen_mode: ScreenMode,
    /// `_footerHeight` (renderer.ts:883, resolved at :1075).
    footer_height: u32,
    /// `_externalOutputMode` (renderer.ts:884, resolved at :1075).
    external_output_mode: ExternalOutputMode,
    /// `_capabilities` (renderer.ts:925) — the last probe snapshot;
    /// `renderer.widthMethod` reads `capabilities?.unicode` through it
    /// (renderer.ts:1468). Fed by the terminal probe lane via
    /// [`CliRenderer::set_capabilities`].
    capabilities: Option<TerminalCapabilities>,
}


pub(crate) use std::collections::HashMap;

impl Default for CliRenderer {
    fn default() -> Self {
        create_cli_renderer(CliRendererConfig::default())
    }
}

/// `createCliRenderer(config)` — renderer.ts:723-748. The upstream factory
/// awaits terminal setup; the headless port is synchronous and never
/// touches stdin/stdout.
///
/// The constructor resolves the mode triple (`resolveModes`,
/// renderer.ts:1074) and arms the split-footer capture path when
/// `externalOutputMode` is `"capture-stdout"` (the `stdout.write`
/// intercept at renderer.ts:1291 — `resolveModes` has already
/// validated it implies `split-footer`, which is exactly the gate
/// every scrollback write/flush checks, renderer.ts:2364-2366).
/// Upstream's constructor *throws* on an invalid config; the port's
/// factory is infallible, so the mode errors surface as a panic
/// carrying the upstream message (bounded divergence — the
/// `Result`-returning seam is [`resolve_modes`]).
#[must_use]
pub fn create_cli_renderer(config: CliRendererConfig) -> CliRenderer {
    let modes = resolve_modes(&config).unwrap_or_else(|e| panic!("{e}"));
    let split_capture_active = modes.external_output_mode == ExternalOutputMode::CaptureStdout;
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
        pending_focus: None,
        mouse_selection: None,
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
        link_pool: HashMap::new(),
        link_interned: HashMap::new(),
        next_link_id: 0,
        destroyed: false,
        render_pending: false,
        scheduler: crate::scheduler::RenderScheduler::new(config.target_fps, config.max_fps),
        theme_mode: None,
        theme_waiters: Vec::new(),
        theme_waiter_seq: 0,
        theme_wait_resolved: HashMap::new(),
        use_console: config.console_mode == ConsoleMode::ConsoleOverlay,
        terminal_title: String::new(),
        #[cfg(feature = "terminal-backend")]
        session: None,
        split_capture_active,
        split_tail_column: 0,
        external_output_queue: crate::scrollback::ExternalOutputQueue::default(),
        split_transcript: String::new(),
        screen_mode: modes.screen_mode,
        footer_height: modes.footer_height,
        external_output_mode: modes.external_output_mode,
        capabilities: None,
    }
}

impl CliRenderer {
    /// Allocate (or intern) the OSC 8 link id for `url` — the port of
    /// the `allocLink` export (lib.zig:1962-1963) over the global
    /// `LinkPool` (link.zig). Ids live for the renderer's lifetime and
    /// are stable across frames (same URL → same id); rasterizers pack
    /// them into cell attributes via [`crate::ansi::set_link_id`], and
    /// [`Self::emit_frame_sgr`]'s link branch resolves them back to
    /// `\x1b]8;id=…;URL\x1b\\` opens. Divergence vs link.zig: no
    /// generation/refcount slot recycling yet (no consumer retires
    /// links) — registered in terminal/mod.rs.
    pub fn alloc_link(&mut self, url: &str) -> u32 {
        if let Some(id) = self.link_interned.get(url) {
            return *id;
        }
        self.next_link_id += 1;
        let id = self.next_link_id;
        self.link_pool.insert(id, url.to_string());
        self.link_interned.insert(url.to_string(), id);
        id
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
            BlockSpec::Table {
                options,
                margin_bottom,
            } => self.make_text_table(&format!("{id}-block-{index}"), &options, margin_bottom),
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

    /// Construct a `TextTableRenderable` leaf (TextTable.ts:99) —
    /// `width: "100%"`, `flexShrink: 0` (the markdown table block shape,
    /// Markdown.ts:1564-1585).
    #[must_use]
    pub fn make_text_table(
        &mut self,
        id: &str,
        opts: &crate::renderables::text_table::TextTableOptions,
        margin_bottom: usize,
    ) -> Renderable {
        let state = crate::renderables::text_table::TextTableState::from_options(opts);
        let ctx = crate::renderable::MeasureCtx::table(state.core.clone());
        Renderable::new(
            &mut self.tree,
            id,
            crate::layout::LayoutOptions {
                flex_shrink: Some(0.0),
                margin_bottom: Some(Dimension::Points(margin_bottom as f32)),
                ..Default::default()
            },
            Dimension::Percent(1.0),
            Dimension::Auto,
            Content::TextTable(state),
            Some(ctx),
        )
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

    /// Construct an `EmbeddedTerminalRenderable` node
    /// (EmbeddedTerminal.ts:42-78): width/height default from cols/rows,
    /// `buffered: true`, focusable.
    #[must_use]
    pub fn make_embedded_terminal(
        &mut self,
        id: &str,
        opts: &crate::renderables::embedded_terminal::EmbeddedTerminalOptions,
    ) -> Renderable {
        let width = opts.width.unwrap_or(opts.cols.map_or(80, u32::from));
        let height = opts.height.unwrap_or(opts.rows.map_or(24, u32::from));
        let state = crate::renderables::embedded_terminal::EmbeddedTerminalState::new(opts);
        let mut r = Renderable::new(
            &mut self.tree,
            id,
            crate::layout::LayoutOptions::default(),
            crate::layout::Dimension::Points(width as f32),
            crate::layout::Dimension::Points(height as f32),
            Content::EmbeddedTerminal(Box::new(state)),
            None,
        );
        r.focusable = true;
        r
    }

    /// Mount convenience for the embedded terminal.
    pub fn add_embedded_terminal(
        &mut self,
        id: &str,
        opts: &crate::renderables::embedded_terminal::EmbeddedTerminalOptions,
    ) {
        let r = self.make_embedded_terminal(id, opts);
        self.add(r);
    }

    /// `terminal.write(data)` (EmbeddedTerminal.ts:142-150): feed +
    /// flush responses + requestRender.
    pub fn embedded_terminal_write(&mut self, id: &str, data: &[u8]) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        let Content::EmbeddedTerminal(state) = &mut node.content else {
            return false;
        };
        state.write(data);
        true
    }

    /// `terminal.screen()` (EmbeddedTerminal.ts:117-140) — None for
    /// unknown/non-terminal ids.
    pub fn embedded_terminal_screen(
        &self,
        id: &str,
    ) -> Option<crate::renderables::embedded_terminal::EmbeddedTerminalScreen> {
        let node = self.find_node(&self.root.inner, id)?;
        let Content::EmbeddedTerminal(state) = &node.content else {
            return None;
        };
        Some(state.screen(node.w.max(0.0) as u16, node.h.max(0.0) as u16))
    }

    /// `terminal.transparentBackground = value` (:104-115).
    pub fn embedded_terminal_set_transparent_background(&mut self, id: &str, value: bool) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        let Content::EmbeddedTerminal(state) = &mut node.content else {
            return false;
        };
        state.set_transparent_background(value)
    }

    /// Drain the `onData` records (`send`, EmbeddedTerminal.ts:374-376).
    pub fn embedded_terminal_take_data(
        &mut self,
        id: &str,
    ) -> Option<
        Vec<(
            Vec<u8>,
            crate::renderables::embedded_terminal::EmbeddedTerminalDataSource,
        )>,
    > {
        let node = Self::find_node_mut(&mut self.root.inner, id)?;
        let Content::EmbeddedTerminal(state) = &mut node.content else {
            return None;
        };
        Some(std::mem::take(&mut state.on_data))
    }

    /// `terminal.invalidate()` (EmbeddedTerminal.ts:152-156).
    pub fn embedded_terminal_invalidate(&mut self, id: &str) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        let Content::EmbeddedTerminal(state) = &mut node.content else {
            return false;
        };
        state.invalidate();
        true
    }

    /// `terminal.width = n` / `.height = n` — the Renderable setter
    /// path (style update → relayout → onResize).
    pub fn embedded_terminal_resize_prop(&mut self, id: &str, is_height: bool, value: f32) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        if !matches!(node.content, Content::EmbeddedTerminal(_)) {
            return false;
        }
        let dim = crate::layout::Dimension::Points(value.max(0.0));
        let mut style = self.tree.style(node.node).expect("taffy style").clone();
        if is_height {
            node.height = dim;
            style.size.height = taffy::style::Dimension::length(value.max(0.0));
        } else {
            node.width = dim;
            style.size.width = taffy::style::Dimension::length(value.max(0.0));
        }
        let _ = self.tree.set_style(node.node, style);
        let _ = self.tree.mark_dirty(node.node);
        true
    }

    /// `forwardMouse`'s scroll fallback (`embeddedTerminalScroll`,
    /// EmbeddedTerminal.ts:348-356) — the scenario/driver entry.
    pub fn embedded_terminal_scroll(&mut self, id: &str, delta: i32) -> bool {
        let Some(node) = Self::find_node_mut(&mut self.root.inner, id) else {
            return false;
        };
        let Content::EmbeddedTerminal(state) = &mut node.content else {
            return false;
        };
        state.engine.scroll_viewport(delta);
        true
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


// ---- split-footer scrollback runtime base (D1b-core) protocol tests ----

#[cfg(test)]
mod scrollback_runtime_tests {
    use super::*;
    use crate::scrollback::{ExternalOutputCommit, ScrollbackSnapshot};

    fn snapshot(lines: &[&str]) -> ScrollbackSnapshot {
        ScrollbackSnapshot {
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            row_columns: None,
            start_on_new_line: None,
            trailing_newline: None,
        }
    }

    /// The mode gate: `writeToScrollback` outside the split-footer
    /// capture mode errors instead of enqueueing (renderer.ts:2364-2366
    /// upstream throws).
    #[test]
    fn write_to_scrollback_requires_capture_mode() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        assert_eq!(
            r.write_to_scrollback(Box::new(|_| snapshot(&["hi"]))),
            Err(ScrollbackWriteError::CaptureNotActive)
        );
        assert_eq!(r.external_output_queue_size(), 0);
    }

    /// The writer context carries the PENDING tail column — queued
    /// commits replay before the writer sees its cursor
    /// (getPendingSplitTailColumn, renderer.ts:2572-2583).
    #[test]
    fn writer_context_sees_pending_tail_column() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        r.set_split_footer_capture(true);
        // first commit: mid-row content without trailingNewline
        r.write_to_scrollback(Box::new(|_ctx| ScrollbackSnapshot {
            lines: vec!["ab".to_owned()],
            trailing_newline: Some(false),
            ..snapshot(&[])
        }))
        .unwrap();
        // second writer must observe tail 2 (queued, not committed)
        let mut seen = None;
        r.write_to_scrollback(Box::new(|ctx| {
            seen = Some(ctx.tail_column);
            snapshot(&["cd"])
        }))
        .unwrap();
        assert_eq!(seen, Some(2));
    }

    /// The EXTERNAL_OUTPUT event shape: the payload IS the commit
    /// (CliRendererExternalOutputEvent, renderer.ts:252-257), emitted
    /// only when a listener is registered (renderer.ts:2610), exactly
    /// once per enqueue.
    #[test]
    fn external_output_event_shape_and_listener_gate() {
        use std::sync::{Arc, Mutex};
        let mut r = create_cli_renderer(CliRendererConfig::default());
        r.set_split_footer_capture(true);
        // no listener → no emission, but the commit still queues
        r.write_to_scrollback(Box::new(|_| {
            ScrollbackSnapshot {
                row_columns: Some(40),
                start_on_new_line: Some(false),
                trailing_newline: Some(true),
                ..snapshot(&["payload"])
            }
        }))
        .unwrap();
        assert_eq!(r.external_output_queue_size(), 1);

        let events: Arc<Mutex<Vec<CliRenderEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        r.on_event(move |e| {
            if matches!(e, CliRenderEvent::ExternalOutput(_)) {
                sink.lock().unwrap().push(e.clone());
            }
        });
        r.write_to_scrollback(Box::new(|_| snapshot(&["second"])))
            .unwrap();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            CliRenderEvent::ExternalOutput(commit) => {
                assert_eq!(commit.lines, vec!["second".to_owned()]);
                assert!(commit.start_on_new_line); // default true, renderer.ts:2612
                assert!(commit.trailing_newline); // default true, renderer.ts:2613
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    /// The flush adjudication decision table
    /// (flushPendingSplitCommits, renderer.ts:2729-2826):
    /// - one tick drains at most `maxSplitCommitsPerFrame` (8,
    ///   renderer.ts:909/2737);
    /// - the accepted prefix drops in FIFO order;
    /// - leftover commits re-request a render (:2817-2820);
    /// - drainAll flushes everything (:2737).
    #[test]
    fn flush_bounded_drain_decision_table() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        r.set_split_footer_capture(true);
        for i in 0..10 {
            let i = i.to_string();
            r.write_to_scrollback(Box::new(move |_| snapshot(&[&i])))
                .unwrap();
        }
        assert_eq!(r.external_output_queue_size(), 10);
        // bounded flush: 8 of 10
        assert_eq!(r.flush_pending_split_commits(false, false), SplitFlushStatus::Rendered);
        assert_eq!(r.external_output_queue_size(), 2);
        // transcript bytes landed in FIFO order with newline intent
        assert_eq!(r.split_transcript(), "0\n1\n2\n3\n4\n5\n6\n7\n");
        // the tail advanced row-by-row (8 rows flushed, each trailing)
        assert_eq!(r.split_tail_column(), 0);
        // the leftover re-armed the render request (:2819)
        assert!(r.is_render_pending());
        // the drain-all transition path flushes the rest (:2917)
        r.flush_pending_split_output_before_transition(false);
        assert_eq!(r.external_output_queue_size(), 0);
        assert_eq!(&r.split_transcript()[16..], "8\n9\n");
    }

    /// `set_split_footer_capture(false)` — the transition hygiene:
    /// pending commits flush first (renderer.ts:1805/1884), the queue
    /// clears, and the tail resets (:2968-2972).
    #[test]
    fn disarm_flushes_and_clears() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        r.set_split_footer_capture(true);
        r.write_to_scrollback(Box::new(|_| snapshot(&["tail"],)))
            .unwrap();
        assert_eq!(r.external_output_queue_size(), 1);
        r.set_split_footer_capture(false);
        assert_eq!(r.external_output_queue_size(), 0);
        assert!(!r.is_split_footer_capture_active());
        assert_eq!(r.split_transcript(), "tail\n"); // payload + trailing newline
        // after disarm, writes are rejected again
        assert_eq!(
            r.write_to_scrollback(Box::new(|_| snapshot(&["x"]))),
            Err(ScrollbackWriteError::CaptureNotActive)
        );
    }

    /// `destroy()` drains the queue before teardown — no captured
    /// output is lost (renderer.ts:4500, 4558).
    #[test]
    fn destroy_drains_pending_commits() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        r.set_split_footer_capture(true);
        r.write_to_scrollback(Box::new(|_| snapshot(&["last words"])))
            .unwrap();
        r.destroy();
        assert_eq!(r.external_output_queue_size(), 0);
        assert_eq!(r.split_transcript(), "last words\n");
    }

    /// The commit payload keeps the writer's rowColumns budget and
    /// newline flags verbatim (enqueueRenderedScrollbackCommit,
    /// renderer.ts:2583-2607).
    #[test]
    fn commit_flags_pass_through() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        r.set_split_footer_capture(true);
        let events: Arc<std::sync::Mutex<Vec<ExternalOutputCommit>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        r.on_event(move |e| {
            if let CliRenderEvent::ExternalOutput(c) = e {
                sink.lock().unwrap().push(c.clone());
            }
        });
        r.write_to_scrollback(Box::new(|_| ScrollbackSnapshot {
            lines: vec!["row".to_owned()],
            row_columns: Some(2),
            start_on_new_line: Some(false),
            trailing_newline: Some(false),
        }))
        .unwrap();
        let c = &events.lock().unwrap()[0];
        assert_eq!(c.row_columns, 2);
        assert!(!c.start_on_new_line);
        assert!(!c.trailing_newline);
    }
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

    /// OSC 8 emission (A3; renderer.zig:2750-2765, 2920-2925): a cell
    /// whose attributes carry a link id opens `\x1b]8;id=…;URL\x1b\\`
    /// before its text, a following unlinked cell closes with
    /// `\x1b]8;;\x1b\\`, and no-link frames emit no `]8;` at all (the
    /// zero-diff guarantee — every differential scenario is link-free).
    #[test]
    fn osc8_link_open_and_close() {
        let mut renderer = create_cli_renderer(CliRendererConfig {
            width: 6,
            height: 1,
            ..Default::default()
        });
        let id = renderer.alloc_link("https://example.com");
        assert_eq!(
            renderer.alloc_link("https://example.com"),
            id,
            "URL interning keeps the id (link.zig interned_live_ids)"
        );
        let template = renderer.next_render_buffer.get(0, 0).unwrap();
        let linked = crate::ansi::set_link_id(template.attributes, id);
        renderer
            .next_render_buffer
            .set_cell(0, 0, 'a', template.fg, template.bg, linked);
        renderer.next_render_buffer.set_cell(
            1,
            0,
            'b',
            template.fg,
            template.bg,
            template.attributes,
        );
        // drive the writer directly (render() would re-rasterize the
        // empty tree over the poked cells) — the doc-hidden bench seam
        let mut sgr = String::new();
        renderer.bench_emit_frame_sgr(&mut sgr);
        let open = "\x1b]8;id=1;https://example.com\x1b\\";
        assert!(sgr.contains(open), "open with id + URL: {sgr:?}");
        assert!(sgr.contains("\x1b]8;;\x1b\\"), "close at the link switch");
        let open_at = sgr.find(open).unwrap();
        let close_at = sgr.find("\x1b]8;;\x1b\\").unwrap();
        assert!(open_at < close_at, "open precedes close");
        assert!(
            sgr[open_at..close_at].contains('a'),
            "linked text inside the span"
        );
        // unknown link id: treated as no link (renderer.zig:2763-2764)
        let mut r2 = create_cli_renderer(CliRendererConfig {
            width: 6,
            height: 1,
            ..Default::default()
        });
        let t2 = r2.next_render_buffer.get(0, 0).unwrap();
        r2.next_render_buffer.set_cell(
            0,
            0,
            'x',
            t2.fg,
            t2.bg,
            crate::ansi::set_link_id(t2.attributes, 999),
        );
        let mut sgr2 = String::new();
        r2.bench_emit_frame_sgr(&mut sgr2);
        assert!(!sgr2.contains("]8;"), "unknown id emits no OSC 8");
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

    // =================================================================
    // Split-footer surface family (D1b-surface) — resolveModes
    // (renderer.ts:365-386), createScrollbackSurface (:1984-2338),
    // resetSplitFooterForReplay (:2446-2475)
    // =================================================================

    fn split_config() -> CliRendererConfig {
        CliRendererConfig {
            screen_mode: ScreenMode::SplitFooter,
            footer_height: Some(8),
            external_output_mode: Some(ExternalOutputMode::CaptureStdout),
            ..Default::default()
        }
    }

    /// Serializes the env-override arms — the process env is global.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The resolveModes decision table (renderer.ts:365-386): the
    /// defaults, footerHeight normalization, capture-stdout's
    /// split-footer dependency, and both env overrides.
    #[test]
    fn resolve_modes_decision_table() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("OTUI_USE_ALTERNATE_SCREEN");
        std::env::remove_var("OTUI_OVERRIDE_STDOUT");

        // defaults: alternate-screen, footer 12, passthrough
        // (renderer.ts:370, :341/:376, :379)
        let m = resolve_modes(&CliRendererConfig::default()).unwrap();
        assert_eq!(m.screen_mode, ScreenMode::AlternateScreen);
        assert_eq!(m.footer_height, DEFAULT_FOOTER_HEIGHT);
        assert_eq!(m.external_output_mode, ExternalOutputMode::Passthrough);

        // split-footer defaults capture-stdout (:379) and keeps the
        // requested footerHeight; None → DEFAULT (:376 via :349-351)
        let m = resolve_modes(&split_config()).unwrap();
        assert_eq!(m.screen_mode, ScreenMode::SplitFooter);
        assert_eq!(m.footer_height, 8);
        assert_eq!(m.external_output_mode, ExternalOutputMode::CaptureStdout);
        let mut c = split_config();
        c.footer_height = None;
        assert_eq!(resolve_modes(&c).unwrap().footer_height, DEFAULT_FOOTER_HEIGHT);

        // non-split modes keep DEFAULT_FOOTER_HEIGHT regardless
        // (:375-376)
        let mut c = split_config();
        c.screen_mode = ScreenMode::MainScreen;
        c.external_output_mode = Some(ExternalOutputMode::Passthrough);
        let m = resolve_modes(&c).unwrap();
        assert_eq!(m.footer_height, DEFAULT_FOOTER_HEIGHT);
        assert_eq!(m.screen_mode, ScreenMode::MainScreen);

        // footerHeight must be > 0 (:359-360)
        let mut c = split_config();
        c.footer_height = Some(0);
        assert_eq!(
            resolve_modes(&c),
            Err(RendererModeError::FooterHeightNotPositive)
        );

        // capture-stdout requires split-footer (:384-386)
        let mut c = CliRendererConfig::default();
        c.external_output_mode = Some(ExternalOutputMode::CaptureStdout);
        assert_eq!(
            resolve_modes(&c),
            Err(RendererModeError::CaptureRequiresSplitFooter)
        );

        // OTUI_USE_ALTERNATE_SCREEN forces the screen mode (:371-373)
        let mut c = split_config();
        c.external_output_mode = Some(ExternalOutputMode::Passthrough);
        std::env::set_var("OTUI_USE_ALTERNATE_SCREEN", "0");
        let m = resolve_modes(&c).unwrap();
        assert_eq!(m.screen_mode, ScreenMode::MainScreen);
        std::env::set_var("OTUI_USE_ALTERNATE_SCREEN", "1");
        let m = resolve_modes(&c).unwrap();
        assert_eq!(m.screen_mode, ScreenMode::AlternateScreen);
        std::env::remove_var("OTUI_USE_ALTERNATE_SCREEN");

        // OTUI_OVERRIDE_STDOUT: truthy + split-footer → capture,
        // otherwise passthrough (:380-382)
        let c = CliRendererConfig::default(); // alternate-screen
        std::env::set_var("OTUI_OVERRIDE_STDOUT", "1");
        assert_eq!(
            resolve_modes(&c).unwrap().external_output_mode,
            ExternalOutputMode::Passthrough
        );
        let m = resolve_modes(&split_config()).unwrap();
        assert_eq!(m.external_output_mode, ExternalOutputMode::CaptureStdout);
        std::env::set_var("OTUI_OVERRIDE_STDOUT", "0");
        let m = resolve_modes(&split_config()).unwrap();
        assert_eq!(m.external_output_mode, ExternalOutputMode::Passthrough);
        std::env::remove_var("OTUI_OVERRIDE_STDOUT");
    }

    /// The constructor arms the runtime from the resolved modes: the
    /// split-footer capture path (the D1b-core base) turns on with
    /// `externalOutputMode "capture-stdout"` (renderer.ts:1074,
    /// :1291) and stays off on the default config.
    #[test]
    fn constructor_arms_split_capture_from_modes() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        assert!(!r.is_split_footer_capture_active());
        assert_eq!(r.screen_mode(), ScreenMode::AlternateScreen);
        assert_eq!(r.footer_height(), DEFAULT_FOOTER_HEIGHT);
        assert_eq!(r.external_output_mode(), ExternalOutputMode::Passthrough);
        // the D1b-core manual arm still works (mode transitions)
        r.set_split_footer_capture(true);
        assert!(r.is_split_footer_capture_active());

        let r = create_cli_renderer(split_config());
        assert!(r.is_split_footer_capture_active());
        assert_eq!(r.screen_mode(), ScreenMode::SplitFooter);
        assert_eq!(r.footer_height(), 8);
        assert_eq!(r.external_output_mode(), ExternalOutputMode::CaptureStdout);
    }

    /// The surface mode gate (renderer.ts:1985-1989): constructing
    /// outside split-footer capture errors, and the surface path is
    /// usable exactly under it.
    #[test]
    fn scrollback_surface_requires_split_footer_capture() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        assert!(matches!(
            r.create_scrollback_surface(ScrollbackSurfaceOptions::default()),
            Err(ScrollbackSurfaceError::RequiresSplitFooterCapture)
        ));

        let mut r = create_cli_renderer(split_config());
        assert!(r
            .create_scrollback_surface(ScrollbackSurfaceOptions::default())
            .is_ok());
    }

    /// The surface lifecycle + snapshot correctness (renderer.ts:1990,
    /// :2150-2207, :2242-2272): mount a text entry under the root,
    /// render converges the auto height, commitRows enqueues the
    /// painted range with the rolling newline ownership, and the
    /// assert ladder (render-before-commit, geometry, row bounds,
    /// destroyed) rejects the misuse shapes.
    #[test]
    fn scrollback_surface_lifecycle_and_snapshot() {
        let mut r = create_cli_renderer(CliRendererConfig {
            width: 20,
            height: 24,
            ..split_config()
        });
        let mut surface = r
            .create_scrollback_surface(ScrollbackSurfaceOptions::default())
            .unwrap();
        assert_eq!(surface.width(), 20);
        assert_eq!(surface.height(), 1);
        assert!(!surface.is_destroyed());

        // commitRows requires render() first (renderer.ts:2068-2071)
        assert_eq!(
            surface.commit_rows(&mut r, 0, 1, ScrollbackSurfaceCommitOptions::default()),
            Err(ScrollbackSurfaceError::NotRendered)
        );

        // mount a two-line text entry (the ref consumer shape:
        // surface.root.add(renderable))
        let opts = TextOptions {
            content: crate::renderables::text::ContentInput::Str(
                "hello surface\nsecond row".to_owned(),
            ),
            ..Default::default()
        };
        let t = surface.render_context().make_text("entry", &opts);
        assert!(surface.add(t));

        surface.render();
        let h = surface.height();
        assert!(h >= 2, "auto height converged over the text rows, got {h}");

        // row-range ladder (renderer.ts:2087-2101)
        assert_eq!(
            surface.commit_rows(&mut r, h, 0, ScrollbackSurfaceCommitOptions::default()),
            Err(ScrollbackSurfaceError::InvalidRowRange)
        );
        assert_eq!(
            surface.commit_rows(&mut r, 0, h + 1, ScrollbackSurfaceCommitOptions::default()),
            Err(ScrollbackSurfaceError::InvalidRowRange)
        );
        // empty range is a no-op (:2265-2267)
        assert_eq!(
            surface.commit_rows(&mut r, 1, 1, ScrollbackSurfaceCommitOptions::default()),
            Ok(())
        );
        assert_eq!(r.external_output_queue_size(), 0);

        // the first commit carries the surface's startOnNewLine (the
        // option default true, renderer.ts:1992), the second flips it
        // (:2270)
        surface
            .commit_rows(&mut r, 0, h, ScrollbackSurfaceCommitOptions::default())
            .unwrap();
        assert_eq!(r.external_output_queue_size(), 1);
        let commit = r.external_output_queue().peek(1)[0].clone();
        assert!(commit.start_on_new_line);
        assert!(commit.trailing_newline);
        assert!(
            commit.lines.iter().any(|l| l.trim_end().starts_with("hello surface")),
            "painted rows carry the entry text: {:?}",
            commit.lines
        );
        assert_eq!(commit.lines.len(), h as usize);

        surface
            .commit_rows(&mut r, 0, h, ScrollbackSurfaceCommitOptions::default())
            .unwrap();
        let commit2 = r.external_output_queue().peek(2)[1].clone();
        assert!(!commit2.start_on_new_line, "nextCommitStartOnNewLine flips");

        // destroyed surfaces reject everything (renderer.ts:2062-2064)
        surface.destroy();
        surface.destroy(); // idempotent
        assert!(surface.is_destroyed());
        assert_eq!(
            surface.commit_rows(&mut r, 0, 1, ScrollbackSurfaceCommitOptions::default()),
            Err(ScrollbackSurfaceError::Destroyed)
        );
    }

    /// `resetSplitFooterForReplay` (renderer.ts:2446-2475): pending
    /// commits drain, the queue empties, the tail resets, and
    /// clearSavedLines empties the transcript model; outside the
    /// split-footer capture mode it errors (:2450-2454).
    #[test]
    fn reset_split_footer_for_replay_resets_state() {
        let mut r = create_cli_renderer(CliRendererConfig::default());
        assert_eq!(
            r.reset_split_footer_for_replay(false),
            Err(ScrollbackSurfaceError::RequiresSplitFooterCapture)
        );

        let mut r = create_cli_renderer(CliRendererConfig {
            width: 20,
            ..split_config()
        });
        r.write_to_scrollback(Box::new(|_| crate::scrollback::ScrollbackSnapshot {
            lines: vec!["queued tail".to_owned()],
            trailing_newline: Some(false),
            ..Default::default()
        }))
        .unwrap();
        r.flush_pending_split_commits(false, true);
        assert_eq!(r.split_tail_column(), "queued tail".len() as u32);

        r.reset_split_footer_for_replay(true).unwrap();
        assert_eq!(r.external_output_queue_size(), 0);
        assert_eq!(r.split_tail_column(), 0);
        assert!(r.split_transcript().is_empty());
        assert!(r.is_render_pending(), ":2474 requestRender");
    }

    /// `renderer.widthMethod` (renderer.ts:1468): the probe caps'
    /// unicode field once set, the zig default before — the D1b-core
    /// divergence-④ closure (the writer context now carries it).
    #[test]
    fn width_method_reads_capabilities() {
        let mut r = create_cli_renderer(split_config());
        assert_eq!(r.width_method(), WidthMethod::default());
        r.write_to_scrollback(Box::new(|ctx| {
            assert_eq!(ctx.width_method, WidthMethod::default());
            crate::scrollback::ScrollbackSnapshot {
                lines: vec!["x".to_owned()],
                ..Default::default()
            }
        }))
        .unwrap();
        r.flush_pending_split_commits(false, true);

        let mut caps = TerminalCapabilities::default();
        caps.unicode = WidthMethod::UnicodeWide;
        r.set_capabilities(caps);
        assert_eq!(r.width_method(), WidthMethod::UnicodeWide);
        r.write_to_scrollback(Box::new(|ctx| {
            assert_eq!(ctx.width_method, WidthMethod::UnicodeWide);
            crate::scrollback::ScrollbackSnapshot {
                lines: vec!["y".to_owned()],
                ..Default::default()
            }
        }))
        .unwrap();
    }
}

