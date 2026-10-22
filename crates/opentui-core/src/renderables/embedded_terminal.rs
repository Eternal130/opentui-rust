//! `EmbeddedTerminalRenderable` — port of
//! `packages/core/src/renderables/EmbeddedTerminal.ts:42-377`.
//!
//! Upstream wraps a native FFI handle (`RenderLib`/`EmbeddedTerminalHandle`)
//! backed by ghostty-vt; the port wraps the [`Engine`] (this crate's
//! re-implementation of the native module, `crate::embedded_terminal`).
//! Cited semantics:
//! * constructor — cols/rows default from width/height, `buffered: true`,
//!   focusable, `selectable ?? true` (EmbeddedTerminal.ts:54-78)
//! * `screen()` — framebuffer rows trimmed, trailing empty lines popped,
//!   `visible = hasValue && visible` (EmbeddedTerminal.ts:117-140)
//! * `write` — feed + flush responses + requestRender (:142-150)
//! * `encodeKey`/`encodePaste` — the ghostty encoders via the engine
//!   (:158-174), with upstream's `physicalKey`/`textualKey`/
//!   `unshiftedCodepoint` key-derivation helpers (:399-440)
//! * selection — cell-drag gate + clamp-to-bounds (:176-225)
//! * focus/blur — focus-event encoding + the cursor park on blur
//!   (:227-253); key dispatch returns `output.len() > 0` (:255-259)
//! * `renderSelf` — compose at (0,0) into its buffer + the focused
//!   cursor position/style/color (:283-298)
//! * `onResize` — clamp to (0, 0xffff], resize + invalidate + flush
//!   + the `onTerminalResize` callback (:272-281)
//! * mouse forwarding — left-press focuses, scroll-with-no-output maps
//!   to a ±3 viewport scroll (:337-361)
//!
//! Callbacks (`onData`, `onTerminalResize`, `onScreenChange`) surface as
//! drained records — the Rust renderer has no user-closure channel;
//! consumers read them via the renderer accessors.

use crate::embedded_terminal::{
    Engine, KeyAction, KeyInput, LogicalKey, MouseAction, MouseButton, MouseInput, TerminalCursor,
};
use crate::events::{KeyEvent, PasteEvent};

/// `EmbeddedTerminalDataSource` (EmbeddedTerminal.ts:10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedTerminalDataSource {
    Input,
    Response,
}

/// `EmbeddedTerminalScreen` (EmbeddedTerminal.ts:23-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedTerminalScreen {
    pub text: String,
    pub lines: Vec<String>,
    pub columns: u16,
    pub rows: u16,
    pub cursor: ScreenCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

/// `EmbeddedTerminalOptions` (EmbeddedTerminal.ts:12-21) — the subset
/// the port's scenario vocabulary and consumers drive.
#[derive(Debug, Clone)]
pub struct EmbeddedTerminalOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub max_scrollback: Option<usize>,
    pub transparent_background: bool,
    pub selectable: bool,
}

impl Default for EmbeddedTerminalOptions {
    fn default() -> Self {
        EmbeddedTerminalOptions {
            width: None,
            height: None,
            cols: None,
            rows: None,
            max_scrollback: None,
            transparent_background: false,
            selectable: true,
        }
    }
}

/// The renderable state — one per `EmbeddedTerminalRenderable`.
pub struct EmbeddedTerminalState {
    pub engine: Engine,
    pub selectable: bool,
    pub transparent_background: bool,
    /// `this.selection` (EmbeddedTerminal.ts:51) — a live cell selection.
    selection_active: bool,
    /// `hadRenderHooks` (:50) — the port has no render-hook vocabulary;
    /// the invalidate-on-hooks arm is structurally a no-op.
    had_render_hooks: bool,
    /// `onData` records — `(bytes, source)` pairs awaiting the consumer.
    pub on_data: Vec<(Vec<u8>, EmbeddedTerminalDataSource)>,
    /// `onTerminalResize` records.
    pub on_resize: Vec<(u16, u16)>,
    /// `onScreenChange` count (fires per compose).
    pub on_screen_change_count: usize,
}

impl EmbeddedTerminalState {
    /// The constructor (EmbeddedTerminal.ts:54-78): cols/rows default
    /// from width/height (else 80×24), `selectable ?? true`.
    pub fn new(options: &EmbeddedTerminalOptions) -> Self {
        let width = options.width.unwrap_or(80);
        let height = options.height.unwrap_or(24);
        let cols = options
            .cols
            .unwrap_or(width.clamp(1, u16::MAX as u32) as u16);
        let rows = options
            .rows
            .unwrap_or(height.clamp(1, u16::MAX as u32) as u16);
        EmbeddedTerminalState {
            engine: Engine::new(cols, rows, options.max_scrollback.unwrap_or(10_000)),
            selectable: options.selectable,
            transparent_background: options.transparent_background,
            selection_active: false,
            had_render_hooks: false,
            on_data: Vec::new(),
            on_resize: Vec::new(),
            on_screen_change_count: 0,
        }
    }

    /// `screen()` (EmbeddedTerminal.ts:117-140).
    pub fn screen(&self, columns: u16, rows: u16) -> EmbeddedTerminalScreen {
        let cursor = self.engine.cursor();
        let mut lines = self.engine.screen_lines();
        lines.truncate(rows as usize);
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        EmbeddedTerminalScreen {
            text: lines.join("\n"),
            lines,
            columns,
            rows,
            cursor: ScreenCursor {
                x: cursor.x,
                y: cursor.y,
                visible: cursor.has_value && cursor.visible,
            },
        }
    }

    /// `write` (EmbeddedTerminal.ts:142-150).
    pub fn write(&mut self, data: &[u8]) {
        self.engine.write(data);
        self.flush_responses();
    }

    /// `invalidate` (EmbeddedTerminal.ts:152-156).
    pub fn invalidate(&mut self) {
        self.engine.invalidate();
    }

    /// `set transparentBackground` (EmbeddedTerminal.ts:104-115).
    pub fn set_transparent_background(&mut self, value: bool) -> bool {
        if self.transparent_background == value {
            return false;
        }
        self.transparent_background = value;
        self.engine.set_transparent_background(value);
        true // → requestRender
    }

    /// `encodeKey` (EmbeddedTerminal.ts:158-169) — upstream's key
    /// derivation (physicalKey/textualKey/modifiers/unshiftedCodepoint)
    /// feeding the engine's ghostty-port encoder.
    pub fn encode_key(&self, key: &KeyEvent) -> Vec<u8> {
        let text = textual_key(key);
        let physical = physical_key(key);
        let input = KeyInput {
            action: if key.event_type() == crate::parse_keypress::KeyEventType::Release {
                KeyAction::Release
            } else if key.repeated() {
                KeyAction::Repeat
            } else {
                KeyAction::Press
            },
            key: LogicalKey::from_physical(&physical),
            mods: modifiers(key),
            text,
            unshifted_codepoint: unshifted_codepoint(key, &physical),
        };
        self.engine.encode_key(&input)
    }

    /// `encodePaste` (EmbeddedTerminal.ts:171-174).
    pub fn encode_paste(&self, bytes: &[u8]) -> Vec<u8> {
        self.engine.encode_paste(bytes)
    }

    /// `shouldStartSelection` (EmbeddedTerminal.ts:176-181).
    pub fn should_start_selection(
        &self,
        x: i32,
        y: i32,
        self_x: i32,
        self_y: i32,
        width: u32,
        height: u32,
    ) -> bool {
        if !self.selectable {
            return false;
        }
        let local_x = x - self_x;
        let local_y = y - self_y;
        local_x >= 0 && (local_x as u32) < width && local_y >= 0 && (local_y as u32) < height
    }

    /// `onSelectionChanged` (EmbeddedTerminal.ts:183-216): the
    /// cell-drag gate (a cell selection must leave its anchor cell
    /// once) + the clamp-to-bounds point mapping.
    pub fn on_selection_changed(&mut self, selection: Option<SelectionUpdate>) -> bool {
        let Some(sel) = selection else {
            if !self.selection_active {
                return false;
            }
            self.engine.clear_selection();
            self.selection_active = false;
            return false; // + requestRender upstream
        };
        let SelectionUpdate {
            behavior_cell: cell_behavior,
            anchor,
            focus,
            width,
            height,
        } = sel;
        if !self.selection_active && cell_behavior && anchor == focus {
            return false;
        }
        let point = |x: i32, y: i32| -> (u16, u16) {
            if y < 0 {
                (0, 0)
            } else if y >= height as i32 {
                (
                    width.saturating_sub(1) as u16,
                    height.saturating_sub(1) as u16,
                )
            } else {
                (
                    x.clamp(0, width as i32 - 1) as u16,
                    y.clamp(0, height as i32 - 1) as u16,
                )
            }
        };
        let a = point(anchor.0, anchor.1);
        let f = point(focus.0, focus.1);
        self.engine.set_selection(a, f);
        self.selection_active = true;
        true
    }

    /// `hasSelection` (EmbeddedTerminal.ts:218-220).
    pub fn has_selection(&self) -> bool {
        self.selection_active
    }

    /// `getSelectedText` (EmbeddedTerminal.ts:222-225).
    pub fn get_selected_text(&self) -> String {
        if !self.selection_active {
            return String::new();
        }
        self.engine.selected_text()
    }

    /// `handleKeyPress` (EmbeddedTerminal.ts:255-259) — encode + send as
    /// input; returns whether any bytes were produced.
    pub fn handle_key_press(&mut self, key: &KeyEvent) -> bool {
        let output = self.encode_key(key);
        self.send(output, EmbeddedTerminalDataSource::Input)
    }

    /// `handlePaste` (EmbeddedTerminal.ts:261-263).
    pub fn handle_paste(&mut self, event: &PasteEvent) {
        let output = self.encode_paste(&event.bytes);
        self.send(output, EmbeddedTerminalDataSource::Input);
    }

    /// The focus half of `focus()` (EmbeddedTerminal.ts:227-240): the
    /// focus-report bytes (mode 1004) go to the pty as input.
    pub fn focus(&mut self) {
        let out = self.engine.encode_focus(true);
        self.send(out, EmbeddedTerminalDataSource::Input);
    }

    /// `blur()` (EmbeddedTerminal.ts:242-253): the focus-loss report;
    /// user callbacks must not prevent native cleanup.
    pub fn blur(&mut self) {
        let out = self.engine.encode_focus(false);
        self.send(out, EmbeddedTerminalDataSource::Input);
    }

    /// `forwardMouse` (EmbeddedTerminal.ts:337-361): encode + send; a
    /// wheel event with no encoded output becomes a ±3 viewport scroll.
    /// Returns `(consumed, scroll_request)`.
    pub fn forward_mouse(&mut self, event: &MouseEventForTerminal) -> (bool, Option<i32>) {
        let input = MouseInput {
            action: event.action,
            button: event.button,
            mods: event.mods,
            x: event.x as f32,
            y: event.y as f32,
            any_button_pressed: event.any_button_pressed,
        };
        let output = self.engine.encode_mouse(&input);
        if event.is_scroll && output.is_empty() {
            let Some(direction) = event.scroll_direction else {
                return (false, None);
            };
            let delta = if direction == "up" { -3 } else { 3 };
            self.engine.scroll_viewport(delta);
            return (true, Some(delta));
        }
        if output.is_empty() {
            return (false, None);
        }
        self.send(output, EmbeddedTerminalDataSource::Input);
        (true, None)
    }

    /// `onResize` (EmbeddedTerminal.ts:272-281): floor + clamp to
    /// 0xffff, resize, invalidate, flush, `onTerminalResize` callback.
    pub fn update_layout(&mut self, width: u32, height: u32) {
        let (w, h) = (width as f64, height as f64);
        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
            return;
        }
        let cols = w.floor().min(0xffff as f64) as u16;
        let rows = h.floor().min(0xffff as f64) as u16;
        self.engine.resize(cols, rows);
        self.engine.invalidate();
        self.flush_responses();
        self.on_resize.push((cols, rows));
    }

    /// `flushResponses` (EmbeddedTerminal.ts:363-366).
    fn flush_responses(&mut self) {
        let drained = self.engine.drain_responses();
        self.send(drained, EmbeddedTerminalDataSource::Response);
    }

    /// `send` (EmbeddedTerminal.ts:374-376) — non-empty data only.
    fn send(&mut self, data: Vec<u8>, source: EmbeddedTerminalDataSource) -> bool {
        if data.is_empty() {
            return false;
        }
        self.on_data.push((data, source));
        true
    }

    /// `renderSelf` (EmbeddedTerminal.ts:283-298): compose + the
    /// screen-change callback; the cursor half is
    /// [`Self::render_cursor`].
    pub fn paint(&self, buffer: &mut crate::buffer::OptimizedBuffer, screen_x: i32, screen_y: i32) {
        self.engine
            .compose(buffer, screen_x, screen_y, self.transparent_background);
    }

    /// The focused-cursor half of `renderSelf` (:287-297) —
    /// `setCursorPosition(screenX + cursorX + 1, screenY + y + 1,
    /// visible)` with the wide-tail shift, plus style/color.
    pub fn render_cursor(&self, screen_x: i32, screen_y: i32) -> Option<CursorPositionOutput> {
        let cursor: TerminalCursor = self.engine.cursor();
        let visible = cursor.visible && cursor.has_value;
        let cursor_x = if cursor.wide_tail && cursor.x > 0 {
            cursor.x - 1
        } else {
            cursor.x
        };
        let pos = (
            (screen_x + cursor_x as i32 + 1).max(0) as u32,
            (screen_y + cursor.y as i32 + 1).max(0) as u32,
            visible,
        );
        if !visible {
            return Some(CursorPositionOutput {
                position: pos,
                style: None,
            });
        }
        // bar→line, underline→underline, else block (:293-296)
        let style = match cursor.style {
            0 => (
                1u8,
                cursor.blinking,
                (cursor.color.unwrap_or((255, 255, 255))),
            ), // line
            2 => (
                2u8,
                cursor.blinking,
                (cursor.color.unwrap_or((255, 255, 255))),
            ), // underline
            _ => (
                0u8,
                cursor.blinking,
                (cursor.color.unwrap_or((255, 255, 255))),
            ), // block
        };
        Some(CursorPositionOutput {
            position: pos,
            style: Some(style),
        })
    }

    pub fn had_render_hooks(&self) -> bool {
        self.had_render_hooks
    }
}

/// The focused-cursor output of `renderSelf` (:287-297) — the position
/// plus the optional style triple (style id, blinking, color).
pub struct CursorPositionOutput {
    pub position: (u32, u32, bool),
    pub style: Option<(u8, bool, (u8, u8, u8))>,
}

/// The renderer-level selection update handed to
/// [`EmbeddedTerminalState::on_selection_changed`] — global coords
/// already localized by the renderer.
#[derive(Debug, Clone, Copy)]
pub struct SelectionUpdate {
    /// behavior === "cell" (the drag gate only applies to cell drags)
    pub behavior_cell: bool,
    pub anchor: (i32, i32),
    pub focus: (i32, i32),
    pub width: u32,
    pub height: u32,
}

/// The mouse event shape `forwardMouse` consumes (rendererMouseEvent →
/// action/button/coords, EmbeddedTerminal.ts:337-347).
pub struct MouseEventForTerminal {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: u8,
    /// local coordinates (x - screenX, y - screenY)
    pub x: i32,
    pub y: i32,
    pub any_button_pressed: bool,
    pub is_scroll: bool,
    pub scroll_direction: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Key-derivation helpers — EmbeddedTerminal.ts:379-440
// ---------------------------------------------------------------------------

/// `modifiers` (EmbeddedTerminal.ts:379-397).
fn modifiers(key: &KeyEvent) -> u8 {
    let mut value = 0u8;
    if key.shift() {
        value |= crate::embedded_terminal::mods_bits::SHIFT;
    }
    if key.ctrl() {
        value |= crate::embedded_terminal::mods_bits::CTRL;
    }
    // `input.alt || input.option` — meta is NOT alt (the "Meta as
    // Super" test, EmbeddedTerminal.test.ts:259-274)
    if key.option() {
        value |= crate::embedded_terminal::mods_bits::ALT;
    }
    if key.super_key() || key.meta() {
        value |= crate::embedded_terminal::mods_bits::SUPER;
    }
    if key.caps_lock() {
        value |= crate::embedded_terminal::mods_bits::CAPS_LOCK;
    }
    if key.num_lock() {
        value |= crate::embedded_terminal::mods_bits::NUM_LOCK;
    }
    value
}

/// `physicalKey` (EmbeddedTerminal.ts:399-424) — note the baseCode
/// preference for kitty-parsed keys (the Dvorak/cyrillic alternate
/// path, EmbeddedTerminal.test.ts:295-314).
fn physical_key(key: &KeyEvent) -> String {
    if let Some(code) = key.code() {
        if !code.starts_with('[') {
            return code.to_owned();
        }
    }
    let name = match key.base_code() {
        Some(base) => char::from_u32(base).map(String::from).unwrap_or_default(),
        None => key.name().to_owned(),
    };
    if name.len() == 1 {
        let c = name.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return format!("Key{}", c.to_ascii_uppercase());
        }
        if c.is_ascii_digit() {
            return format!("Digit{c}");
        }
    }
    match name.to_lowercase().as_str() {
        "backspace" => "Backspace".to_owned(),
        "enter" | "return" => "Enter".to_owned(),
        "space" => "Space".to_owned(),
        "tab" => "Tab".to_owned(),
        "delete" => "Delete".to_owned(),
        "end" => "End".to_owned(),
        "home" => "Home".to_owned(),
        "insert" => "Insert".to_owned(),
        "pagedown" => "PageDown".to_owned(),
        "pageup" => "PageUp".to_owned(),
        "down" => "ArrowDown".to_owned(),
        "left" => "ArrowLeft".to_owned(),
        "right" => "ArrowRight".to_owned(),
        "up" => "ArrowUp".to_owned(),
        "escape" => "Escape".to_owned(),
        _ => String::new(),
    }
}

fn is_control_char(c: char) -> bool {
    (c as u32) < 0x20 || (c as u32) == 0x7f
}

/// `textualKey` (EmbeddedTerminal.ts:426-431).
fn textual_key(key: &KeyEvent) -> String {
    let seq = key.sequence();
    if !seq.is_empty() && !seq.chars().any(is_control_char) {
        return seq.to_owned();
    }
    if key.name() == "space" {
        return " ".to_owned();
    }
    let name = key.name();
    if name.is_empty() || name.chars().any(is_control_char) {
        return String::new();
    }
    let count = name.chars().count();
    if count == 1 || !name.is_ascii() {
        return name.to_owned();
    }
    String::new()
}

/// `unshiftedCodepoint` (EmbeddedTerminal.ts:433-440).
fn unshifted_codepoint(key: &KeyEvent, code: &str) -> u32 {
    // Kitty's baseCode is a physical-layout alternative, not the active
    // layout's character.
    if key.name() == "space" {
        return 32;
    }
    let name = key.name();
    if name.chars().count() == 1 {
        return name.chars().next().map_or(0, |c| c as u32);
    }
    if code.starts_with("Key") && code.len() == 4 {
        return u32::from(code.as_bytes()[2]) + 32;
    }
    if code.starts_with("Digit") && code.len() == 6 {
        return u32::from(code.as_bytes()[5]);
    }
    0
}

/// Map the renderer mouse event into the terminal-forwarding shape
/// (`forwardMouse`'s inputs, EmbeddedTerminal.ts:337-347) with global →
/// local coordinates (x - screenX, y - screenY).
pub fn mouse_event_for_terminal(
    event: &crate::events::MouseEvent,
    screen_x: i32,
    screen_y: i32,
) -> MouseEventForTerminal {
    use crate::parse_mouse::MouseEventType as T;
    let is_scroll = event.event_type == T::Scroll;
    let action = match event.event_type {
        T::Up => MouseAction::Release,
        T::Move | T::Drag | T::Over | T::Out | T::DragEnd | T::Drop => MouseAction::Motion,
        _ => MouseAction::Press,
    };
    let button = if event.event_type == T::Move && !event.is_dragging {
        None
    } else {
        mouse_button(event.button, is_scroll, &event.scroll)
    };
    let mut mods = 0u8;
    if event.modifiers.shift {
        mods |= crate::embedded_terminal::mods_bits::SHIFT;
    }
    if event.modifiers.ctrl {
        mods |= crate::embedded_terminal::mods_bits::CTRL;
    }
    if event.modifiers.alt {
        mods |= crate::embedded_terminal::mods_bits::ALT;
    }
    MouseEventForTerminal {
        action,
        button,
        mods,
        x: event.x - screen_x,
        y: event.y - screen_y,
        any_button_pressed: event.is_dragging || event.event_type == T::Down,
        is_scroll,
        scroll_direction: event.scroll.as_ref().map(|s| match s.direction {
            crate::parse_mouse::ScrollDirection::Up => "up",
            crate::parse_mouse::ScrollDirection::Down => "down",
            crate::parse_mouse::ScrollDirection::Left => "left",
            crate::parse_mouse::ScrollDirection::Right => "right",
        }),
    }
}

/// `mouseButton` (EmbeddedTerminal.ts:442-451).
fn mouse_button(
    button: i32,
    is_scroll: bool,
    scroll: &Option<crate::parse_mouse::ScrollInfo>,
) -> Option<MouseButton> {
    if is_scroll {
        return match scroll.as_ref().map(|s| s.direction) {
            Some(crate::parse_mouse::ScrollDirection::Up) => Some(MouseButton::Four),
            Some(crate::parse_mouse::ScrollDirection::Down) => Some(MouseButton::Five),
            Some(crate::parse_mouse::ScrollDirection::Left) => Some(MouseButton::Six),
            Some(crate::parse_mouse::ScrollDirection::Right) => Some(MouseButton::Seven),
            None => None,
        };
    }
    match button {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        4 => Some(MouseButton::Four),
        5 => Some(MouseButton::Five),
        _ => None,
    }
}
