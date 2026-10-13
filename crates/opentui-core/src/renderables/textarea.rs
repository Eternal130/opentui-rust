//! `TextareaRenderable` — packages/core/src/renderables/Textarea.ts over
//! `EditBufferRenderable.ts` — the v2 composer's editor.
//!
//! Composition note: upstream is a class chain (Renderable →
//! EditBufferRenderable → TextareaRenderable) sharing a native
//! EditBuffer/EditorView pair. The port composes one
//! [`TextareaState`] holding the [`EditorView`] (which owns the
//! [`crate::editor::EditBuffer`]) plus the renderable's colors, key
//! bindings, and the 36-action executor (`buildActionHandlers`,
//! Textarea.ts:220-259).

use crate::color::{parse_color, Rgba};
use crate::editor::{
    draw_editor_view, EditorDrawState, EditorView, EditorViewport, SelectionBehavior,
    SelectionOccupancy, VisualCursor,
};
use crate::events;
use crate::keybinding::{
    build_key_bindings_map, default_key_aliases, get_key_binding_action, merge_key_aliases,
    merge_key_bindings, KeyBinding, KeyBindingLookup,
};
use crate::layout::Dimension;
use crate::styled_text::TextChunk;
use crate::text_buffer::{TextDocument, Viewport};

/// `TextareaAction` (Textarea.ts:17-53) — the 36 edit-buffer actions
/// (order matches `opentui-keymap::textarea::EDIT_BUFFER_ACTIONS`).
pub const TEXTAREA_ACTIONS: &[&str] = &[
    "move-left",
    "move-right",
    "move-up",
    "move-down",
    "select-left",
    "select-right",
    "select-up",
    "select-down",
    "line-home",
    "line-end",
    "select-line-home",
    "select-line-end",
    "visual-line-home",
    "visual-line-end",
    "select-visual-line-home",
    "select-visual-line-end",
    "buffer-home",
    "buffer-end",
    "select-buffer-home",
    "select-buffer-end",
    "delete-line",
    "delete-to-line-end",
    "delete-to-line-start",
    "backspace",
    "delete",
    "newline",
    "undo",
    "redo",
    "word-forward",
    "word-backward",
    "select-word-forward",
    "select-word-backward",
    "delete-word-forward",
    "delete-word-backward",
    "select-all",
    "submit",
];

/// `defaultTextareaKeyBindings` (Textarea.ts:58-128) — verbatim.
pub fn default_textarea_key_bindings() -> Vec<KeyBinding> {
    let t: &[(&str, bool, bool, bool, bool, &str)] = &[
        ("left", false, false, false, false, "move-left"),
        ("right", false, false, false, false, "move-right"),
        ("up", false, false, false, false, "move-up"),
        ("down", false, false, false, false, "move-down"),
        ("left", false, true, false, false, "select-left"),
        ("right", false, true, false, false, "select-right"),
        ("up", false, true, false, false, "select-up"),
        ("down", false, true, false, false, "select-down"),
        ("home", false, false, false, false, "buffer-home"),
        ("end", false, false, false, false, "buffer-end"),
        ("home", false, true, false, false, "select-buffer-home"),
        ("end", false, true, false, false, "select-buffer-end"),
        ("a", true, false, false, false, "line-home"),
        ("e", true, false, false, false, "line-end"),
        ("a", true, true, false, false, "select-line-home"),
        ("e", true, true, false, false, "select-line-end"),
        ("a", false, false, true, false, "visual-line-home"),
        ("e", false, false, true, false, "visual-line-end"),
        ("a", false, true, true, false, "select-visual-line-home"),
        ("e", false, true, true, false, "select-visual-line-end"),
        ("f", true, false, false, false, "move-right"),
        ("b", true, false, false, false, "move-left"),
        ("w", true, false, false, false, "delete-word-backward"),
        (
            "backspace",
            true,
            false,
            false,
            false,
            "delete-word-backward",
        ),
        ("d", false, false, true, false, "delete-word-forward"),
        ("delete", false, false, true, false, "delete-word-forward"),
        ("delete", true, false, false, false, "delete-word-forward"),
        ("d", true, true, false, false, "delete-line"),
        ("k", true, false, false, false, "delete-to-line-end"),
        ("u", true, false, false, false, "delete-to-line-start"),
        ("backspace", false, false, false, false, "backspace"),
        ("backspace", false, true, false, false, "backspace"),
        ("d", true, false, false, false, "delete"),
        ("delete", false, false, false, false, "delete"),
        ("delete", false, true, false, false, "delete"),
        ("return", false, false, false, false, "newline"),
        ("kpenter", false, false, false, false, "newline"),
        ("linefeed", false, false, false, false, "newline"),
        ("return", false, false, true, false, "submit"),
        ("kpenter", false, false, true, false, "submit"),
        // undo/redo
        ("-", true, false, false, false, "undo"),
        (".", true, false, false, false, "redo"),
        ("z", false, false, false, true, "undo"),
        ("z", false, true, false, true, "redo"),
        ("f", false, false, true, false, "word-forward"),
        ("b", false, false, true, false, "word-backward"),
        ("right", false, false, true, false, "word-forward"),
        ("left", false, false, true, false, "word-backward"),
        ("right", true, false, false, false, "word-forward"),
        ("left", true, false, false, false, "word-backward"),
        ("f", false, true, true, false, "select-word-forward"),
        ("b", false, true, true, false, "select-word-backward"),
        ("right", false, true, true, false, "select-word-forward"),
        ("left", false, true, true, false, "select-word-backward"),
        (
            "backspace",
            false,
            false,
            true,
            false,
            "delete-word-backward",
        ),
        // super (cmd/win) + arrows for Kitty Keyboard mode
        ("left", false, false, false, true, "visual-line-home"),
        ("right", false, false, false, true, "visual-line-end"),
        ("up", false, false, false, true, "buffer-home"),
        ("down", false, false, false, true, "buffer-end"),
        ("left", false, true, false, true, "select-visual-line-home"),
        ("right", false, true, false, true, "select-visual-line-end"),
        ("up", false, true, false, true, "select-buffer-home"),
        ("down", false, true, false, true, "select-buffer-end"),
        ("a", false, false, false, true, "select-all"),
    ];
    t.iter()
        .map(|(name, ctrl, shift, meta, sup, action)| {
            KeyBinding::new(name, *ctrl, *shift, *meta, *sup, action)
        })
        .collect()
}

/// `TextareaRenderable.defaults` (Textarea.ts:159-166).
fn default_text_color() -> Rgba {
    parse_color("#FFFFFF")
}
fn default_placeholder_color() -> Rgba {
    parse_color("#666666")
}

/// `CursorStyleOptions` — the terminal cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Line,
    Underline,
}

/// `SubmitEvent` (Textarea.ts:130).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubmitEvent;

/// `TextareaOptions` (Textarea.ts:132-143 over EditBufferOptions:64-83).
/// Colors arrive pre-parsed (the TS layer accepts `ColorInput`; the port
/// resolves through [`parse_color`] at the builder boundary).
#[derive(Debug, Clone, Default)]
pub struct TextareaOptions {
    pub layout: crate::layout::LayoutOptions,
    pub width: Option<Dimension>,
    pub height: Option<Dimension>,
    pub initial_value: Option<String>,
    pub text_color: Option<Rgba>,
    pub background_color: Option<Rgba>,
    pub focused_text_color: Option<Rgba>,
    pub focused_background_color: Option<Rgba>,
    /// `placeholder?: StyledText | string | null`
    pub placeholder: Option<Vec<TextChunk>>,
    pub placeholder_color: Option<Rgba>,
    pub selection_bg: Option<Rgba>,
    pub selection_fg: Option<Rgba>,
    pub wrap_mode: Option<crate::renderables::text::WrapMode>,
    pub scroll_margin: Option<f32>,
    pub show_cursor: Option<bool>,
    pub cursor_color: Option<Rgba>,
    pub cursor_style: Option<(CursorStyle, bool)>,
    pub attributes: u32,
    pub tab_indicator: Option<u32>,
    pub tab_indicator_color: Option<Rgba>,
    /// `keyBindings` — merged OVER the defaults (constructor path,
    /// Textarea.ts:190-192).
    pub custom_bindings: Vec<KeyBinding>,
}

/// The `placeholder` option's effective styled chunks — a plain string
/// becomes one chunk with the placeholder color (`applyPlaceholder`,
/// Textarea.ts:205-218).
fn placeholder_chunks(placeholder: Option<&Vec<TextChunk>>, color: Rgba) -> Vec<TextChunk> {
    match placeholder {
        None => Vec::new(),
        Some(chunks) => chunks
            .iter()
            .map(|c| TextChunk {
                text: c.text.clone(),
                fg: c.fg.or(Some(color)),
                bg: c.bg,
                attributes: c.attributes,
                link: c.link.clone(),
            })
            .collect(),
    }
}

/// The textarea renderable state — `EditBufferRenderable`'s fields +
/// Textarea's.
pub struct TextareaState {
    pub view: EditorView,
    // colors (EditBufferRenderable.ts:91-99 + Textarea.ts:146-157)
    unfocused_bg: Rgba,
    unfocused_fg: Rgba,
    focused_bg: Rgba,
    focused_fg: Rgba,
    placeholder_color: Rgba,
    selection_bg: Option<Rgba>,
    selection_fg: Option<Rgba>,
    default_attributes: u32,
    // editor options
    show_cursor: bool,
    cursor_color: Rgba,
    cursor_style: (CursorStyle, bool),
    pub selectable: bool,
    /// `EditorTraits.suspend` — suspends the textarea's own key handling
    /// (`registerTextareaMappingSuspension`'s contract).
    pub suspended: bool,
    focused: bool,
    // key bindings (Textarea.ts:152-155)
    custom_bindings: Vec<KeyBinding>,
    key_alias_map: std::collections::HashMap<String, String>,
    key_bindings_map: std::collections::HashMap<String, String>,
    // tab indicator
    tab_indicator: Option<u32>,
    tab_indicator_color: Option<Rgba>,
    // keyboard-selection state (EditBufferRenderable.ts:111)
    keyboard_selection_active: bool,
    initial_value_set: bool,
    /// `lastLocalSelection` (EditBufferRenderable.ts:101)
    last_local_selection: Option<(i32, i32, i32, i32, SelectionBehavior)>,
    /// `onSubmit` listener
    submit_listener: Option<Box<dyn FnMut(SubmitEvent)>>,
    /// cursor id — the renderer's hit/selection identity for this node
    pub renderable_id: String,
    /// the draw snapshot stashed by `prepare_render` (the port's
    /// two-phase render; upstream reads the view live inside render())
    draw_cache: Option<(TextDocument, crate::editor::EditorVlines, Viewport)>,
}

impl TextareaState {
    /// The constructor chain (EditBufferRenderable.ts:137-183 +
    /// Textarea.ts:168-203). `viewport_width/height` stand in for
    /// upstream's `this.width || 80, this.height || 24`.
    pub fn from_options(
        id: &str,
        opts: &TextareaOptions,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Self {
        let buffer = crate::editor::EditBuffer::new();
        let mut view = EditorView::new(buffer, viewport_width, viewport_height);

        let unfocused_fg = opts.text_color.unwrap_or_else(default_text_color);
        let unfocused_bg = opts
            .background_color
            .unwrap_or_else(|| parse_color("transparent"));
        let focused_fg = opts
            .focused_text_color
            .or(opts.text_color)
            .unwrap_or_else(default_text_color);
        let focused_bg = opts
            .focused_background_color
            .or(opts.background_color)
            .unwrap_or_else(|| parse_color("transparent"));
        let placeholder_color = opts
            .placeholder_color
            .unwrap_or_else(default_placeholder_color);

        // EditBufferRenderable constructor: wrapMode word, scrollMargin
        // 0.2, defaults onto the buffer (166-168)
        view.set_wrap_mode(
            opts.wrap_mode
                .unwrap_or(crate::renderables::text::WrapMode::Word),
        );
        view.set_scroll_margin(opts.scroll_margin.unwrap_or(0.2));

        let key_alias_map = default_key_aliases();
        let custom_bindings = opts.custom_bindings.clone();
        let merged = merge_key_bindings(&default_textarea_key_bindings(), &custom_bindings);
        let key_bindings_map = build_key_bindings_map(&merged, Some(&key_alias_map));

        let mut state = TextareaState {
            view,
            unfocused_bg,
            unfocused_fg,
            focused_bg,
            focused_fg,
            placeholder_color,
            selection_bg: opts.selection_bg,
            selection_fg: opts.selection_fg,
            default_attributes: opts.attributes,
            show_cursor: opts.show_cursor.unwrap_or(true),
            cursor_color: opts
                .cursor_color
                .unwrap_or_else(|| Rgba::from_values(1.0, 1.0, 1.0, 1.0)),
            cursor_style: opts.cursor_style.unwrap_or((CursorStyle::Block, true)),
            selectable: true,
            suspended: false,
            focused: false,
            custom_bindings,
            key_alias_map,
            key_bindings_map,
            tab_indicator: opts.tab_indicator,
            tab_indicator_color: opts.tab_indicator_color,
            keyboard_selection_active: false,
            initial_value_set: false,
            last_local_selection: None,
            submit_listener: None,
            renderable_id: id.to_owned(),
            draw_cache: None,
        };

        // setDefaultFg/Bg/Attributes (EditBufferRenderable.ts:166-168)
        state.apply_effective_colors();

        if let Some(value) = &opts.initial_value {
            state.set_text(value);
            state.initial_value_set = true;
        }

        // applyPlaceholder (Textarea.ts:202)
        state.apply_placeholder(opts.placeholder.as_ref());
        state
    }

    /// `updateColors` (Textarea.ts:302-308) + the setters that route
    /// into the buffer defaults.
    fn apply_effective_colors(&mut self) {
        let bg = if self.focused {
            self.focused_bg
        } else {
            self.unfocused_bg
        };
        let fg = if self.focused {
            self.focused_fg
        } else {
            self.unfocused_fg
        };
        self.view.buffer.set_default_bg(bg);
        self.view.buffer.set_default_fg(fg);
        self.view
            .buffer
            .set_default_attributes(self.default_attributes);
    }

    /// `applyPlaceholder` (Textarea.ts:205-218).
    fn apply_placeholder(&mut self, placeholder: Option<&Vec<TextChunk>>) {
        let chunks = placeholder_chunks(placeholder, self.placeholder_color);
        self.view.set_placeholder_styled_text(&chunks);
    }

    // -- text APIs (EditBufferRenderable.ts 1126-1161) ------------------------

    /// `setText` (1126-1130).
    pub fn set_text(&mut self, text: &str) {
        self.view.buffer.set_text(text);
    }

    /// `replaceText` (1136-1140) — undoable.
    pub fn replace_text(&mut self, text: &str) {
        self.view.buffer.replace_text(text);
    }

    /// `clear` (1142-1147).
    pub fn clear(&mut self) {
        self.view.buffer.clear();
    }

    /// `plainText` (225-227).
    pub fn plain_text(&self) -> String {
        self.view.buffer.get_text()
    }

    /// `insertText` (680-687): delete the selection first.
    pub fn insert_text(&mut self, text: &str) {
        if self.has_selection() {
            self.delete_selected_text();
        }
        self.view.buffer.insert_text(text);
    }

    /// `insertChar` (671-678).
    pub fn insert_char(&mut self, ch: &str) {
        if self.has_selection() {
            self.delete_selected_text();
        }
        self.view.buffer.insert_char(ch);
    }

    /// `deleteRange` (1149-1153).
    pub fn delete_range(&mut self, start_row: u32, start_col: u32, end_row: u32, end_col: u32) {
        self.view
            .buffer
            .delete_range(start_row, start_col, end_row, end_col);
    }

    /// `getTextRange` (1155-1157).
    pub fn get_text_range(&self, start: u32, end: u32) -> String {
        self.view.buffer.get_text_range(start, end)
    }

    pub fn get_text_range_by_coords(
        &self,
        start_row: u32,
        start_col: u32,
        end_row: u32,
        end_col: u32,
    ) -> String {
        self.view
            .buffer
            .get_text_range_by_coords(start_row, start_col, end_row, end_col)
    }

    // -- cursor ---------------------------------------------------------------

    /// `setCursor` (665-669).
    pub fn set_cursor(&mut self, row: u32, col: u32) {
        self.clear_selection();
        self.view.buffer.set_cursor(row, col);
    }

    /// `logicalCursor` (229-231).
    pub fn logical_cursor(&self) -> crate::editor::LogicalCursor {
        self.view.buffer.cursor_position()
    }

    /// `visualCursor` (233-236).
    pub fn visual_cursor(&mut self) -> VisualCursor {
        self.view.get_visual_cursor()
    }

    /// `cursorOffset` (238-246).
    pub fn cursor_offset(&mut self) -> u32 {
        self.visual_cursor().offset
    }

    pub fn set_cursor_offset(&mut self, offset: u32) {
        self.clear_selection();
        self.view.set_cursor_by_offset(offset);
    }

    /// `lineCount` / `virtualLineCount` (213-219).
    pub fn line_count(&self) -> u32 {
        self.view.buffer.line_count()
    }

    pub fn virtual_line_count(&mut self) -> u32 {
        self.view.get_virtual_line_count()
    }

    /// `scrollY` (221-223) — scrollTop.
    pub fn scroll_y(&self) -> u32 {
        self.view.get_viewport().offset_y
    }

    /// `handleScroll` (EditBufferRenderable.ts:450-479): wheel deltas
    /// clamped to the content range; wrap-none also scrolls x.
    pub fn scroll_by(&mut self, direction: &str, delta: u32) {
        let viewport = self.view.get_viewport();
        match direction {
            "up" => {
                let new_y = viewport.offset_y.saturating_sub(delta);
                self.view.set_viewport(
                    viewport.offset_x,
                    new_y,
                    viewport.width,
                    viewport.height,
                    true,
                );
            }
            "down" => {
                let total = self.view.total_virtual_line_count();
                let max_y = total.saturating_sub(viewport.height);
                let new_y = (viewport.offset_y + delta).min(max_y);
                self.view.set_viewport(
                    viewport.offset_x,
                    new_y,
                    viewport.width,
                    viewport.height,
                    true,
                );
            }
            "left" | "right"
                if self.view.wrap_mode() == crate::renderables::text::WrapMode::None =>
            {
                let new_x = if direction == "left" {
                    viewport.offset_x.saturating_sub(delta)
                } else {
                    viewport.offset_x + delta
                };
                self.view.set_viewport(
                    new_x,
                    viewport.offset_y,
                    viewport.width,
                    viewport.height,
                    true,
                );
            }
            _ => {}
        }
    }

    /// Explicit viewport set (the transcript-scroll v2 path:
    /// `editorView.setViewport(x, y, w, h, moveCursor)`).
    pub fn set_viewport(&mut self, x: u32, y: u32, width: u32, height: u32, move_cursor: bool) {
        self.view.set_viewport(x, y, width, height, move_cursor);
    }

    pub fn get_viewport(&self) -> EditorViewport {
        self.view.get_viewport()
    }

    /// `onResize` → `setViewportSize` (EditBufferRenderable.ts:481-483).
    pub fn update_layout(&mut self, width: u32, height: u32) {
        self.view.set_viewport_size(width.max(1), height.max(1));
    }

    // -- selection (EditBufferRenderable.ts 608-663) ---------------------------

    pub fn get_selected_text(&self) -> String {
        self.view.get_selected_text()
    }

    pub fn has_selection(&self) -> bool {
        self.view.has_selection()
    }

    pub fn get_selection(&self) -> Option<(u32, u32)> {
        self.view.get_selection()
    }

    /// `deleteSelectedText` (624-628) — no global-selection clear (the
    /// renderer-level Selection is the events lane's plumbing).
    fn delete_selected_text(&mut self) {
        self.view.delete_selected_text();
    }

    /// `setSelection` (630-636).
    pub fn set_selection(&mut self, start: u32, end: u32) {
        self.last_local_selection = None;
        self.view.reset_selection();
        self.view
            .set_selection(start, end, self.selection_bg, self.selection_fg);
    }

    /// `setSelectionInclusive` (638-644).
    pub fn set_selection_inclusive(&mut self, start: u32, end: u32) {
        self.last_local_selection = None;
        self.view.reset_selection();
        self.view
            .set_selection_inclusive(start, end, self.selection_bg, self.selection_fg);
    }

    /// `clearSelection` (646-655).
    pub fn clear_selection(&mut self) -> bool {
        let had = self.has_selection();
        self.last_local_selection = None;
        self.view.reset_selection();
        had
    }

    /// `deleteSelection` (657-663).
    pub fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        self.last_local_selection = None;
        self.delete_selected_text();
        true
    }

    /// `selectionOccupancy` (396-405).
    pub fn set_selection_occupancy(&mut self, occupancy: SelectionOccupancy) {
        self.view.set_selection_occupancy(occupancy);
    }

    pub fn selection_occupancy(&self) -> SelectionOccupancy {
        self.view.selection_occupancy()
    }

    /// `onSelectionChanged` (519-580) — the keyboard-selection re-entry
    /// point (renderer global selection → local). `global` is the
    /// selection converted to this renderable's local coordinates.
    #[allow(clippy::too_many_arguments)]
    pub fn on_selection_changed(
        &mut self,
        local: Option<(i32, i32, i32, i32, SelectionBehavior)>,
        is_start: bool,
        is_dragging: bool,
    ) -> bool {
        self.last_local_selection = local;
        let follow_cursor = self.keyboard_selection_active;

        let changed = match local {
            None => {
                self.keyboard_selection_active = false;
                self.view.reset_selection();
                true
            }
            Some((anchor_x, anchor_y, focus_x, focus_y, behavior)) => {
                let colors = (self.selection_bg, self.selection_fg);
                if is_start {
                    self.view.set_local_selection(
                        (anchor_x, anchor_y),
                        (focus_x, focus_y),
                        colors,
                        true,
                        follow_cursor,
                        behavior,
                    )
                } else {
                    self.view.update_local_selection(
                        (anchor_x, anchor_y),
                        (focus_x, focus_y),
                        colors,
                        true,
                        follow_cursor,
                        behavior,
                    )
                }
            }
        };
        let _ = is_dragging; // auto-scroll velocity is the animation loop's
                             // concern (onUpdate, 582-606)
        changed && self.has_selection()
    }

    /// `updateSelectionForMovement` (1163-1191) — the shift-motion
    /// selection path against the renderer's global selection
    /// (`ctx.startSelection` / `ctx.updateSelection`,
    /// renderer.ts:4964-5024). Port delta: upstream notifies the
    /// selected renderables synchronously inside updateSelection; the
    /// port defers delivery to the end of the action (see
    /// `CliRenderer::textarea_action`) — observationally equivalent
    /// for the keyboard path (the start-notify's zero-width selection
    /// is immediately superseded by the focus update).
    fn update_selection_for_movement(
        &mut self,
        shift_pressed: bool,
        is_before_movement: bool,
        ctx: &mut ActionCtx,
    ) {
        if !shift_pressed {
            self.keyboard_selection_active = false;
            self.clear_selection();
            ctx.start_clear_selection();
            return;
        }
        if !self.selectable {
            return;
        }
        self.keyboard_selection_active = true;
        let visual_cursor = self.view.get_visual_cursor();
        // cursorX = this.x + visualCol (1174-1176) — the node's layout
        // position relative to its parent comes in through the ctx
        let cursor_x = ctx.node_x + visual_cursor.visual_col as i32;
        let cursor_y = ctx.node_y + visual_cursor.visual_row as i32;

        if is_before_movement {
            if !ctx.has_selection() || !self.has_selection() {
                ctx.start_selection(&self.renderable_id, cursor_x, cursor_y);
            } else if ctx.selection_behavior() != Some(SelectionBehavior::Cell)
                && self.view.convert_selection_to_cell()
            {
                ctx.set_selection_behavior(SelectionBehavior::Cell);
            }
            return;
        }
        ctx.update_selection(cursor_x, cursor_y, true);
    }

    // -- motion (EditBufferRenderable.ts 727-933) ------------------------------

    /// `collapseSelectionToEdge` (727-734): horizontal motions collapse
    /// to an edge without taking another step.
    fn collapse_selection_to_edge(&mut self, edge: &str) -> bool {
        let Some((start, end)) = self.get_selection() else {
            return false;
        };
        let offset = if edge == "start" { start } else { end };
        self.view.buffer.set_cursor_by_offset(offset);
        self.clear_selection();
        true
    }

    pub fn move_cursor_left(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("start") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        self.view.buffer.move_left();
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    pub fn move_cursor_right(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("end") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        self.view.buffer.move_right();
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    pub fn move_cursor_up(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        self.update_selection_for_movement(select, true, renderer);
        self.view.move_up_visual();
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    pub fn move_cursor_down(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        self.update_selection_for_movement(select, true, renderer);
        self.view.move_down_visual();
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoLineHome` (791-807).
    pub fn goto_line_home(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("start") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        let cursor = self.view.buffer.primary_cursor();
        if cursor.col == 0 && cursor.row > 0 {
            self.view.buffer.set_cursor(cursor.row - 1, 0);
            let eol = self.view.buffer.get_eol();
            self.view.buffer.set_cursor(eol.row, eol.col);
        } else {
            self.view.buffer.set_cursor(cursor.row, 0);
        }
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoLineEnd` (809-825).
    pub fn goto_line_end(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("end") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        let cursor = self.view.buffer.primary_cursor();
        let eol = self.view.buffer.get_eol();
        let line_count = self.view.buffer.line_count();
        if cursor.col == eol.col && cursor.row + 1 < line_count {
            self.view.buffer.set_cursor(cursor.row + 1, 0);
        } else {
            self.view.buffer.set_cursor(eol.row, eol.col);
        }
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoVisualLineHome` (827-836).
    pub fn goto_visual_line_home(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("start") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        let sol = self.view.get_visual_sol();
        self.view
            .buffer
            .set_cursor(sol.logical_row, sol.logical_col);
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoVisualLineEnd` (838-846).
    pub fn goto_visual_line_end(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("end") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        self.view.goto_visual_line_end();
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoBufferHome` (848-856).
    pub fn goto_buffer_home(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("start") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        self.view.buffer.set_cursor(0, 0);
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoBufferEnd` (858-866).
    pub fn goto_buffer_end(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("end") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        self.view.buffer.goto_line(999_999);
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `gotoLine` (776-780).
    pub fn goto_line(&mut self, line: u32) {
        self.clear_selection();
        self.view.buffer.goto_line(line);
    }

    /// `selectAll` (868-872).
    pub fn select_all(&mut self, renderer: &mut ActionCtx) -> bool {
        self.update_selection_for_movement(false, true, renderer);
        self.view.buffer.set_cursor(0, 0);
        self.goto_buffer_end(true, renderer)
    }

    /// `deleteToLineEnd` (874-884).
    pub fn delete_to_line_end(&mut self) -> bool {
        let cursor = self.view.buffer.primary_cursor();
        let eol = self.view.buffer.get_eol();
        if eol.col > cursor.col {
            self.view
                .buffer
                .delete_range(cursor.row, cursor.col, eol.row, eol.col);
        }
        true
    }

    /// `deleteToLineStart` (886-897).
    pub fn delete_to_line_start(&mut self) -> bool {
        let cursor = self.view.buffer.primary_cursor();
        if cursor.col > 0 {
            self.view
                .buffer
                .delete_range(cursor.row, 0, cursor.row, cursor.col);
        } else if cursor.row > 0 {
            self.view.buffer.backspace();
        }
        true
    }

    /// `deleteLine` (720-725).
    pub fn delete_line(&mut self) -> bool {
        self.view.buffer.delete_line();
        true
    }

    /// `newLine` (713-718).
    pub fn new_line(&mut self) -> bool {
        self.view.buffer.new_line();
        true
    }

    /// `deleteChar` (689-699).
    pub fn delete_char(&mut self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        self.view.buffer.delete_forward();
        true
    }

    /// `deleteCharBackward` (701-711).
    pub fn delete_char_backward(&mut self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        self.view.buffer.backspace();
        true
    }

    /// `undo` / `redo` (899-911).
    pub fn undo(&mut self) -> bool {
        self.view.buffer.undo();
        true
    }

    pub fn redo(&mut self) -> bool {
        self.view.buffer.redo();
        true
    }

    /// `moveWordForward` (913-922).
    pub fn move_word_forward(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("end") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        let next_word = self.view.buffer.get_next_word_boundary();
        self.view.buffer.set_cursor_by_offset(next_word.offset);
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `moveWordBackward` (924-933).
    pub fn move_word_backward(&mut self, select: bool, renderer: &mut ActionCtx) -> bool {
        if !select && self.collapse_selection_to_edge("start") {
            return true;
        }
        self.update_selection_for_movement(select, true, renderer);
        let prev_word = self.view.buffer.get_prev_word_boundary();
        self.view.buffer.set_cursor_by_offset(prev_word.offset);
        self.update_selection_for_movement(select, false, renderer);
        true
    }

    /// `deleteWordForward` (935-951).
    pub fn delete_word_forward(&mut self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        let cursor = self.view.buffer.cursor_position();
        let next_word = self.view.buffer.get_next_word_boundary();
        if next_word.offset > cursor.offset {
            self.view
                .buffer
                .delete_range(cursor.row, cursor.col, next_word.row, next_word.col);
        }
        true
    }

    /// `deleteWordBackward` (953-969).
    pub fn delete_word_backward(&mut self) -> bool {
        if self.has_selection() {
            self.delete_selected_text();
            return true;
        }
        let cursor = self.view.buffer.cursor_position();
        let prev_word = self.view.buffer.get_prev_word_boundary();
        if prev_word.offset < cursor.offset {
            self.view
                .buffer
                .delete_range(prev_word.row, prev_word.col, cursor.row, cursor.col);
        }
        true
    }

    // -- the action executor (Textarea.ts:220-259) ------------------------------

    /// `buildActionHandlers` + `handleKeyPress`'s dispatch — the 36
    /// edit-buffer actions (`opentui-keymap::textarea`'s command set).
    /// `ctx` is the renderer's interactive state (selection + node
    /// position), borrowed for the keyboard-selection path.
    pub fn execute_action(&mut self, action: &str, ctx: &mut ActionCtx) -> bool {
        match action {
            "move-left" => self.move_cursor_left(false, ctx),
            "move-right" => self.move_cursor_right(false, ctx),
            "move-up" => self.move_cursor_up(false, ctx),
            "move-down" => self.move_cursor_down(false, ctx),
            "select-left" => self.move_cursor_left(true, ctx),
            "select-right" => self.move_cursor_right(true, ctx),
            "select-up" => self.move_cursor_up(true, ctx),
            "select-down" => self.move_cursor_down(true, ctx),
            "line-home" => self.goto_line_home(false, ctx),
            "line-end" => self.goto_line_end(false, ctx),
            "select-line-home" => self.goto_line_home(true, ctx),
            "select-line-end" => self.goto_line_end(true, ctx),
            "visual-line-home" => self.goto_visual_line_home(false, ctx),
            "visual-line-end" => self.goto_visual_line_end(false, ctx),
            "select-visual-line-home" => self.goto_visual_line_home(true, ctx),
            "select-visual-line-end" => self.goto_visual_line_end(true, ctx),
            "buffer-home" => self.goto_buffer_home(false, ctx),
            "buffer-end" => self.goto_buffer_end(false, ctx),
            "select-buffer-home" => self.goto_buffer_home(true, ctx),
            "select-buffer-end" => self.goto_buffer_end(true, ctx),
            "delete-line" => self.delete_line(),
            "delete-to-line-end" => self.delete_to_line_end(),
            "delete-to-line-start" => self.delete_to_line_start(),
            "backspace" => self.delete_char_backward(),
            "delete" => self.delete_char(),
            "newline" => self.new_line(),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "word-forward" => self.move_word_forward(false, ctx),
            "word-backward" => self.move_word_backward(false, ctx),
            "select-word-forward" => self.move_word_forward(true, ctx),
            "select-word-backward" => self.move_word_backward(true, ctx),
            "delete-word-forward" => self.delete_word_forward(),
            "delete-word-backward" => self.delete_word_backward(),
            "select-all" => self.select_all(ctx),
            "submit" => self.submit(),
            _ => false,
        }
    }

    /// `handleKeyPress` (Textarea.ts:265-300): binding dispatch, then
    /// the printable-text path. Takes the KeyHandler `KeyEvent`
    /// (KeyHandler.ts:5-62) — a parsed key plus prevention flags.
    pub fn handle_key_press(&mut self, key: &events::KeyEvent, ctx: &mut ActionCtx) -> bool {
        if !self.suspended {
            let lookup = KeyBindingLookup::from_parsed_key(&key.key);
            if let Some(action) = get_key_binding_action(&self.key_bindings_map, &lookup) {
                return self.execute_action(&action, ctx);
            }
        }
        if !key.ctrl() && !key.meta() && !key.super_key() && !key.hyper() {
            if key.name() == "space" {
                self.insert_text(" ");
                return true;
            }
            let sequence = key.sequence();
            if !sequence.is_empty() {
                let Some(first) = sequence.chars().next() else {
                    return false;
                };
                let first_code = first as u32;
                if first_code < 32 {
                    return false;
                }
                if first_code == 127 {
                    return false;
                }
                self.insert_text(sequence);
                return true;
            }
        }
        false
    }

    /// `handlePaste` (Textarea.ts:259-261): decode the bytes, strip
    /// stray ANSI, insert.
    pub fn handle_paste(&mut self, bytes: &[u8]) {
        let text = crate::paste::decode_paste_bytes(bytes);
        let stripped = crate::paste::strip_ansi_sequences(&text);
        self.insert_text(&stripped);
    }

    // -- submit (Textarea.ts:395-408) -------------------------------------------

    pub fn submit(&mut self) -> bool {
        if let Some(listener) = &mut self.submit_listener {
            listener(SubmitEvent);
        }
        true
    }

    pub fn set_on_submit(&mut self, listener: Option<Box<dyn FnMut(SubmitEvent)>>) {
        self.submit_listener = listener;
    }

    // -- focus (Textarea.ts:310-320 + EditBufferRenderable.ts:1024-1034) --------

    pub fn focus(&mut self) {
        self.focused = true;
        self.apply_effective_colors();
    }

    pub fn blur(&mut self) {
        self.focused = false;
        self.apply_effective_colors();
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    // -- properties ---------------------------------------------------------------

    /// `placeholder` setter (Textarea.ts:326-333).
    pub fn set_placeholder(&mut self, placeholder: Option<Vec<TextChunk>>) {
        self.apply_placeholder(placeholder.as_ref());
    }

    /// `placeholderColor` setter (339-346).
    pub fn set_placeholder_color(&mut self, color: Rgba) {
        self.placeholder_color = color;
        // re-apply: the string-placeholder chunks carry the color
        // (placeholder is re-derived by the caller through set_placeholder;
        // styled chunks keep their own colors)
    }

    /// `backgroundColor` / `textColor` setters route into the buffer
    /// defaults (EditBufferRenderable.ts:270-277, 320-327).
    pub fn set_background_color(&mut self, color: Rgba) {
        self.unfocused_bg = color;
        self.apply_effective_colors();
    }

    pub fn set_text_color(&mut self, color: Rgba) {
        self.unfocused_fg = color;
        self.apply_effective_colors();
    }

    pub fn set_focused_background_color(&mut self, color: Rgba) {
        self.focused_bg = color;
        self.apply_effective_colors();
    }

    pub fn set_focused_text_color(&mut self, color: Rgba) {
        self.focused_fg = color;
        self.apply_effective_colors();
    }

    pub fn background_color(&self) -> Rgba {
        self.unfocused_bg
    }

    pub fn text_color(&self) -> Rgba {
        self.unfocused_fg
    }

    /// `selectionBg`/`selectionFg` setters (EditBufferRenderable.ts:294-314).
    pub fn set_selection_bg(&mut self, color: Option<Rgba>) {
        self.selection_bg = color;
        self.view
            .set_selection_colors(self.selection_bg, self.selection_fg);
    }

    pub fn set_selection_fg(&mut self, color: Option<Rgba>) {
        self.selection_fg = color;
        self.view
            .set_selection_colors(self.selection_bg, self.selection_fg);
    }

    /// `keyBindings` setter (Textarea.ts:410-414).
    pub fn set_key_bindings(&mut self, bindings: Vec<KeyBinding>) {
        self.custom_bindings = bindings;
        let merged = merge_key_bindings(&default_textarea_key_bindings(), &self.custom_bindings);
        self.key_bindings_map = build_key_bindings_map(&merged, Some(&self.key_alias_map));
    }

    /// `keyAliasMap` setter (416-420).
    pub fn set_key_alias_map(&mut self, aliases: std::collections::HashMap<String, String>) {
        self.key_alias_map = merge_key_aliases(&default_key_aliases(), &aliases);
        let merged = merge_key_bindings(&default_textarea_key_bindings(), &self.custom_bindings);
        self.key_bindings_map = build_key_bindings_map(&merged, Some(&self.key_alias_map));
    }

    /// `initialValue` setter (388-393): one-shot.
    pub fn set_initial_value(&mut self, value: &str) {
        if !self.initial_value_set {
            self.set_text(value);
            self.initial_value_set = true;
        }
    }

    /// `wrapMode` setter (EditBufferRenderable.ts:341-352).
    pub fn set_wrap_mode(&mut self, mode: crate::renderables::text::WrapMode) {
        self.view.set_wrap_mode(mode);
    }

    pub fn wrap_mode(&self) -> crate::renderables::text::WrapMode {
        self.view.wrap_mode()
    }

    // -- rendering (EditBufferRenderable.ts:991-1022) ------------------------------

    /// `render()` phase 1: stash the draw snapshot
    /// (`updateBeforeRender` + virtual lines) and compute the terminal
    /// cursor output (`renderCursor`, 1010-1022) —
    /// `(screenX + visualCol + 1, screenY + visualRow + 1)`, 1-based.
    pub fn prepare_render(&mut self, screen_x: i32, screen_y: i32) -> Option<(u32, u32)> {
        let (doc, vlines) = self.view.draw_state();
        let viewport = self.view.draw_viewport();
        self.draw_cache = Some((doc, vlines, viewport));
        if !self.show_cursor || !self.focused {
            return None;
        }
        let vc = self.view.get_visual_cursor();
        let x = (screen_x + vc.visual_col as i32 + 1).max(1) as u32;
        let y = (screen_y + vc.visual_row as i32 + 1).max(1) as u32;
        Some((x, y))
    }

    /// `renderSelf` → `drawEditorView` (1006-1008) + the viewport
    /// prefill input (buffer.zig:1671-1679).
    pub fn paint(&self, buffer: &mut crate::buffer::OptimizedBuffer, screen_x: i32, screen_y: i32) {
        let Some((doc, vlines, viewport)) = &self.draw_cache else {
            return;
        };
        let draw = EditorDrawState {
            doc,
            vlines: &vlines.vlines,
            cell_offsets: &vlines.cell_offsets,
            viewport: *viewport,
            selection: self.view.selection(),
            defaults: doc.defaults,
            prefill_bg: Some(self.view.buffer.defaults().bg),
            tab_indicator: self.tab_indicator,
            tab_indicator_color: self.tab_indicator_color,
        };
        draw_editor_view(buffer, &draw, screen_x, screen_y);
    }

    /// The last prepared draw snapshot (harness observability: spans,
    /// viewport, placeholder state).
    pub fn draw_cache(&self) -> Option<&(TextDocument, crate::editor::EditorVlines, Viewport)> {
        self.draw_cache.as_ref()
    }

    pub fn cursor_style(&self) -> (CursorStyle, bool, Rgba) {
        (self.cursor_style.0, self.cursor_style.1, self.cursor_color)
    }

    /// The taffy measure — `NativeMeasureTargetKind.EditorView` over
    /// `measureForDimensions` (native-renderable.zig:63-88): intrinsic
    /// (max line width, vline count), clamped by the at-most
    /// constraint, minimum 1×1.
    pub fn measure(
        doc: &TextDocument,
        wrap_mode: crate::renderables::text::WrapMode,
        known_width: Option<f32>,
        available_width: f32,
        available_height: f32,
    ) -> (f32, f32) {
        let measure_width = match known_width {
            Some(w) if w.is_finite() && w > 0.0 => w.floor() as u32,
            _ => {
                if available_width.is_finite() && available_width > 0.0 {
                    available_width.floor() as u32
                } else {
                    0
                }
            }
        };
        let (w, h) = doc.measure(wrap_mode, measure_width);
        let mut measured_w = (w.max(1)) as f32;
        let mut measured_h = (h.max(1)) as f32;
        // at-most clamp (native-renderable.zig:77-80) — taffy's
        // Definite available space is yoga's at-most mode
        if known_width.is_none() && available_width.is_finite() {
            measured_w = measured_w.min(available_width);
            measured_h = measured_h.min(available_height.max(0.0));
        }
        (measured_w, measured_h)
    }

    /// The measured snapshot for the taffy leaf context.
    pub fn measured(&self) -> MeasuredEditor {
        MeasuredEditor {
            doc: self.view.buffer.document(),
            wrap_mode: self.view.wrap_mode(),
        }
    }
}

/// The measured editor content — the taffy measure context
/// (`MeasureCtx` for `MeasureKind::Editor`).
#[derive(Debug, Clone, Default)]
pub struct MeasuredEditor {
    pub doc: TextDocument,
    pub wrap_mode: crate::renderables::text::WrapMode,
}

/// The renderer context textarea actions run against — the port's
/// binding of `_ctx` (the renderer's global selection + this node's
/// layout position). Delivered by [`crate::renderer::CliRenderer`].
pub struct ActionCtx<'a> {
    pub(crate) selection: &'a mut Option<crate::renderer::RendererSelection>,
    pub(crate) node_x: i32,
    pub(crate) node_y: i32,
    /// whether this action touched the global selection (the deferred
    /// delivery gate — upstream notifies inside updateSelection only)
    pub(crate) selection_dirty: bool,
}

impl ActionCtx<'_> {
    /// `ctx.hasSelection` (renderer.ts:4935).
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// `ctx.getSelection()?.behavior`.
    pub fn selection_behavior(&self) -> Option<SelectionBehavior> {
        self.selection.as_ref().map(|s| s.behavior)
    }

    /// `ctx.startSelection(renderable, x, y)` (renderer.ts:4964-4972) —
    /// clears any previous selection, anchors at (x, y), `isStart`.
    pub fn start_selection(&mut self, renderable_id: &str, x: i32, y: i32) {
        self.selection_dirty = true;
        *self.selection = Some(crate::renderer::RendererSelection {
            anchor_renderable: renderable_id.to_owned(),
            anchor_relative: (x - self.node_x, y - self.node_y),
            focus: (x, y),
            behavior: SelectionBehavior::Cell,
            is_start: true,
            is_dragging: true,
        });
    }

    /// `ctx.clearSelection` + the null-notify (renderer.ts:4943-4962).
    /// The notify delivery is deferred to the action boundary.
    pub fn start_clear_selection(&mut self) {
        if self.selection.is_some() {
            self.selection_dirty = true;
        }
        *self.selection = None;
    }

    /// `ctx.updateSelection(renderable, x, y, {finishDragging})`
    /// (renderer.ts:4988-5024): focus move + `isStart` clear.
    pub fn update_selection(&mut self, x: i32, y: i32, finish_dragging: bool) {
        if let Some(sel) = self.selection.as_mut() {
            self.selection_dirty = true;
            sel.is_start = false;
            sel.focus = (x, y);
            if finish_dragging {
                sel.is_dragging = false;
            }
        }
    }

    /// `selection.behavior = "cell"` (EditBufferRenderable.ts:1183-1185).
    pub fn set_selection_behavior(&mut self, behavior: SelectionBehavior) {
        if let Some(sel) = self.selection.as_mut() {
            sel.behavior = behavior;
        }
    }
}
