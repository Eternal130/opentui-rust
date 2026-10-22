//! The embedded terminal engine — the Rust stand-in for upstream's
//! native `embedded-terminal` module (`packages/native/src/embedded-terminal/`),
//! which embeds Ghostty's `libghostty-vt` VT engine. The renderable
//! (`renderables/embedded_terminal.rs`) ports `EmbeddedTerminal.ts` 1:1
//! against this engine the same way upstream talks to the FFI lib.
//!
//! Provenance and parity discipline:
//! * the observable contract (grid cells, colors, attributes, cursor,
//!   responses, input encodings) was frozen by probing the REAL pinned
//!   `@opentui/core@0.5.14` native library (`libopentui.so`, ghostty-vt
//!   enabled): the palette/attr/response constants live here, the grid
//!   semantics are pinned by the `et-*` differential scenarios
//!   (`tools/diff/scenarios/`), and the input encoders by the ported
//!   upstream suite (`crates/opentui-core/tests/embedded_terminal.rs`).
//! * constants probed once, pinned here:
//!   - default fg `#FFFFFF` / bg `#000000` (opaque; `transparentBackground`
//!     writes the bg as a `default`-intent color, compositor.zig:19-22).
//!   - palette 0-15 = the Tomorrow Night theme; 16-255 = the standard
//!     xterm cube/ramp (probe: `38;5;208` → #FF8700, `48;5;17` → #00005F).
//!   - attribute bits = the port's `text_attributes` (blink→16, hidden→64
//!     via SGR 8, strikethrough→128).
//! * input encoding ports Ghostty's `src/input/{key_encode,mouse_encode,
//!   paste}.zig` (MIT) — the exact encoder the vendored native library
//!   calls — pinned by the upstream test suite
//!   (`packages/core/src/renderables/EmbeddedTerminal.test.ts`) and the
//!   frozen corpus.
//!
//! Known narrowing vs Ghostty (kept honest in PORT-MATRIX): grapheme
//! clustering is single-codepoint + width, tab stops are fixed at 8, and
//! the kitty key table covers the functional keys upstream's
//! `physicalKey` can produce.

use crate::ansi::text_attributes;
use crate::buffer::OptimizedBuffer;
use crate::color::Rgba;
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

// ---------------------------------------------------------------------------
// Constants — frozen oracle observations
// ---------------------------------------------------------------------------

/// `state.colors.foreground` default (probe: empty pane fg = white).
pub const DEFAULT_FG: (u8, u8, u8) = (255, 255, 255);
/// `state.colors.background` default (probe: empty pane bg = black).
pub const DEFAULT_BG: (u8, u8, u8) = (0, 0, 0);

/// Ghostty's default 16-color palette — the Tomorrow Night theme
/// (probe: SGR 31 → #CC6666, 34 → #81A2BE, 90 → #D54E53, …).
const PALETTE_16: [(u8, u8, u8); 16] = [
    (0x1D, 0x1F, 0x21),
    (0xCC, 0x66, 0x66),
    (0xB5, 0xBD, 0x68),
    (0xF0, 0xC6, 0x74),
    (0x81, 0xA2, 0xBE),
    (0xB2, 0x94, 0xBB),
    (0x8A, 0xBE, 0xB7),
    (0xC5, 0xC8, 0xC6),
    (0x66, 0x66, 0x66),
    (0xD5, 0x4E, 0x53),
    (0xB9, 0xCA, 0x4A),
    (0xE7, 0xC5, 0x47),
    (0x7A, 0xA6, 0xDA),
    (0xC3, 0x97, 0xD8),
    (0x70, 0xC0, 0xB1),
    (0xEA, 0xEA, 0xEA),
];

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// `38;5;n` resolution — the probed values match the standard xterm
/// cube/grayscale ramp exactly (ansi.zig `fallbackAnsi256Color`).
#[must_use]
pub fn palette_rgb(index: u8) -> (u8, u8, u8) {
    let i = index as usize;
    if i < 16 {
        return PALETTE_16[i];
    }
    if i < 232 {
        let c = i - 16;
        return (
            CUBE_LEVELS[(c / 36) % 6],
            CUBE_LEVELS[(c / 6) % 6],
            CUBE_LEVELS[c % 6],
        );
    }
    let gray = (8 + (i - 232) * 10) as u8;
    (gray, gray, gray)
}

/// `response_limit` (main.zig:29) — the preserved response prefix cap.
pub const RESPONSE_LIMIT: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Cell model
// ---------------------------------------------------------------------------

/// A cell color: terminal-default, palette index, or direct RGB —
/// ghostty's style model as the compositor resolves it
/// (compositor.zig:100-101).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellColor {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg: CellColor,
    pub bg: CellColor,
    pub attrs: u8,
}

/// A grid cell. `cp == '\0'` marks an untouched cell (compositor's
/// clear value); `' '` marks explicit blank (BCE erase keeps its bg).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub cp: char,
    pub wide_tail: bool,
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            cp: '\0',
            wide_tail: false,
            style: CellStyle::default(),
        }
    }
}

impl Cell {
    fn blank(style: CellStyle) -> Self {
        Cell {
            cp: ' ',
            wide_tail: false,
            style,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Row {
    cells: Vec<Cell>,
    /// soft-wrap flag — wrapped lines re-flow on resize (probe: the
    /// "aaaa bbbb …" line re-splits at every width).
    wrapped: bool,
}

impl Row {
    fn new(cols: u16) -> Self {
        Row {
            cells: vec![Cell::default(); cols as usize],
            wrapped: false,
        }
    }
    fn resize(&mut self, cols: u16) {
        self.cells.resize(cols as usize, Cell::default());
    }
}

fn char_width(c: char) -> usize {
    // combining marks report None — narrowed to zero width
    UnicodeWidthChar::width(c).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Cursor / encoders' shared types
// ---------------------------------------------------------------------------

/// `Cursor` (main.zig:12-21) — what `embeddedTerminalCursor` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalCursor {
    pub x: u16,
    pub y: u16,
    pub has_value: bool,
    pub visible: bool,
    pub blinking: bool,
    pub wide_tail: bool,
    /// 0=bar 1=block 2=underline 3=block-hollow (main.zig:154-159)
    pub style: u8,
    pub color: Option<(u8, u8, u8)>,
}

/// Modifier bits — the wire format `EmbeddedTerminal.ts:35-40` packs
/// (`MOD_SHIFT=1, MOD_CTRL=2, MOD_ALT=4, MOD_SUPER=8, CAPS=16, NUM=32`).
pub mod mods_bits {
    pub const SHIFT: u8 = 1 << 0;
    pub const CTRL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const SUPER: u8 = 1 << 3;
    pub const CAPS_LOCK: u8 = 1 << 4;
    pub const NUM_LOCK: u8 = 1 << 5;
}

/// Ghostty `key.Mods` (input/key_mods.zig) — the encoder's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_: bool,
    pub hyper: bool,
    pub meta: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl Mods {
    /// The ghostty key.Mods bit layout the FFI mods byte uses.
    fn from_bits(bits: u8) -> Self {
        Mods {
            shift: bits & mods_bits::SHIFT != 0,
            ctrl: bits & mods_bits::CTRL != 0,
            alt: bits & mods_bits::ALT != 0,
            super_: bits & mods_bits::SUPER != 0,
            hyper: false,
            meta: false,
            caps_lock: bits & mods_bits::CAPS_LOCK != 0,
            num_lock: bits & mods_bits::NUM_LOCK != 0,
        }
    }
    /// `mods.binding()` — the binding-sensitive subset (locks stripped).
    fn binding(self) -> Self {
        Mods {
            shift: self.shift,
            alt: self.alt,
            ctrl: self.ctrl,
            super_: self.super_,
            ..Mods::default()
        }
    }
    fn is_empty(self) -> bool {
        self == Mods::default()
    }
    fn int(self) -> u8 {
        (self.shift as u8)
            | ((self.alt as u8) << 1)
            | ((self.ctrl as u8) << 2)
            | ((self.super_ as u8) << 3)
            | ((self.hyper as u8) << 4)
            | ((self.meta as u8) << 5)
            | ((self.caps_lock as u8) << 6)
            | ((self.num_lock as u8) << 7)
    }
}

/// `EmbeddedTerminalKey` (ghostty.zig:9-29) — the encode input; the
/// renderable builds it from a `KeyEvent` (EmbeddedTerminal.ts:158-169).
#[derive(Debug, Clone, Default)]
pub struct KeyInput {
    pub action: KeyAction,
    pub key: LogicalKey,
    pub mods: u8,
    pub text: String,
    pub unshifted_codepoint: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

/// The subset of ghostty's `input.Key` that upstream's `physicalKey`
/// (EmbeddedTerminal.ts:399-424) can produce; text keys carry their
/// codepoint (`Key.codepoint()`, used for the base-layout alternate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogicalKey {
    #[default]
    Unidentified,
    Escape,
    Enter,
    Tab,
    Backspace,
    Insert,
    Delete,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    PageUp,
    PageDown,
    Home,
    End,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    F(u8), // f1..=f25
    Codepoint(char),
}

impl LogicalKey {
    fn codepoint(self) -> Option<char> {
        match self {
            LogicalKey::Codepoint(c) => Some(c),
            _ => None,
        }
    }

    /// Map the physical-key string (`physicalKey`, EmbeddedTerminal.ts:399-424).
    #[must_use]
    pub fn from_physical(name: &str) -> LogicalKey {
        if let Some(rest) = name.strip_prefix("Key") {
            if let [c] = rest.as_bytes() {
                if c.is_ascii_uppercase() {
                    return LogicalKey::Codepoint((c + 32) as char);
                }
            }
            return LogicalKey::Unidentified;
        }
        if let Some(rest) = name.strip_prefix("Digit") {
            if let [c] = rest.as_bytes() {
                if c.is_ascii_digit() {
                    return LogicalKey::Codepoint(*c as char);
                }
            }
            return LogicalKey::Unidentified;
        }
        match name {
            "Escape" => LogicalKey::Escape,
            "Enter" => LogicalKey::Enter,
            "Tab" => LogicalKey::Tab,
            "Backspace" => LogicalKey::Backspace,
            "Insert" => LogicalKey::Insert,
            "Delete" => LogicalKey::Delete,
            "ArrowLeft" => LogicalKey::ArrowLeft,
            "ArrowRight" => LogicalKey::ArrowRight,
            "ArrowUp" => LogicalKey::ArrowUp,
            "ArrowDown" => LogicalKey::ArrowDown,
            "PageUp" => LogicalKey::PageUp,
            "PageDown" => LogicalKey::PageDown,
            "Home" => LogicalKey::Home,
            "End" => LogicalKey::End,
            "CapsLock" => LogicalKey::CapsLock,
            "ScrollLock" => LogicalKey::ScrollLock,
            "NumLock" => LogicalKey::NumLock,
            "PrintScreen" => LogicalKey::PrintScreen,
            "Pause" => LogicalKey::Pause,
            other => {
                if let Some(num) = other.strip_prefix('F') {
                    if let Ok(n) = num.parse::<u8>() {
                        if (1..=25).contains(&n) {
                            return LogicalKey::F(n);
                        }
                    }
                }
                LogicalKey::Unidentified
            }
        }
    }
}

/// `EmbeddedTerminalMouse` (EmbeddedTerminal.ts:337-347).
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: u8,
    pub x: f32,
    pub y: f32,
    pub any_button_pressed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseAction {
    #[default]
    Press,
    Release,
    Motion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Four,
    Five,
    Six,
    Seven,
}

pub mod kitty_flags {
    pub const DISAMBIGUATE: u8 = 1 << 0;
    pub const REPORT_EVENTS: u8 = 1 << 1;
    pub const REPORT_ALTERNATES: u8 = 1 << 2;
    pub const REPORT_ALL: u8 = 1 << 3;
    pub const REPORT_ASSOCIATED: u8 = 1 << 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseEventMode {
    None,
    Normal, // 1000
    Button, // 1002
    Any,    // 1003
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseFormat {
    X10,
    Utf8,      // 1005
    Sgr,       // 1006
    Urxvt,     // 1015
    SgrPixels, // 1016
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct Pen {
    fg: CellColor,
    bg: CellColor,
    attrs: u8,
}

#[derive(Debug, Clone, Copy)]
struct CursorState {
    x: u16,
    y: u16,
    pending_wrap: bool,
    visible: bool,
    /// main.zig:19 `style: u8 = 1` — 0=bar 1=block 2=underline 3=hollow.
    style: u8,
    blinking: bool,
}

impl Default for CursorState {
    fn default() -> Self {
        CursorState {
            x: 0,
            y: 0,
            pending_wrap: false,
            visible: true,
            style: 1,
            blinking: false,
        }
    }
}

/// The embedded terminal — `EmbeddedTerminal` of main.zig (state + ops),
/// driven by the `vte` state machine (the crate already in the lockfile
/// via the vt100 replay dependency — no new dependency).
pub struct Engine {
    pub cols: u16,
    pub rows: u16,
    max_scrollback: usize,

    grid: Vec<Row>,
    scrollback: VecDeque<Row>,
    /// primary state saved across an alternate-screen switch
    saved_primary: Option<(Vec<Row>, CursorState, Pen, usize)>,

    cursor: CursorState,
    pen: Pen,
    saved_cursor: (CursorState, Pen),

    scroll_top: usize,
    scroll_bottom: usize,
    origin_mode: bool,
    autowrap: bool,
    in_alt: bool,

    bracketed_paste: bool,
    focus_event: bool,
    mouse_event: MouseEventMode,
    mouse_format: MouseFormat,
    cursor_keys_app: bool,

    kitty_stack: Vec<u8>,
    kitty_flags: u8,

    /// viewport scroll offset in lines (0 = bottom; probe: writes while
    /// scrolled do not move it).
    viewport_offset: usize,

    responses: VecDeque<u8>,
    response_overflow: bool,

    mouse_last_cell: Option<(i32, i32)>,

    pub(crate) default_fg: (u8, u8, u8),
    pub(crate) default_bg: (u8, u8, u8),

    /// cell selection in viewport coordinates (setSelection)
    selection: Option<((u16, u16), (u16, u16))>,

    parser: vte::Parser,
}

impl Engine {
    /// `EmbeddedTerminal.init` (main.zig:44-67). Dimensions are
    /// validated by the FFI layer (`embeddedTerminalDimension`,
    /// zig.ts:386-391: integers in 1..=65535).
    #[must_use]
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Self {
        Engine {
            cols,
            rows,
            max_scrollback,
            grid: (0..rows).map(|_| Row::new(cols)).collect(),
            scrollback: VecDeque::new(),
            saved_primary: None,
            cursor: CursorState::default(),
            pen: Pen::default(),
            saved_cursor: (CursorState::default(), Pen::default()),
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1) as usize,
            origin_mode: false,
            autowrap: true,
            in_alt: false,
            bracketed_paste: false,
            focus_event: false,
            mouse_event: MouseEventMode::None,
            mouse_format: MouseFormat::X10,
            cursor_keys_app: false,
            kitty_stack: Vec::new(),
            kitty_flags: 0,
            viewport_offset: 0,
            responses: VecDeque::new(),
            response_overflow: false,
            mouse_last_cell: None,
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            selection: None,
            parser: vte::Parser::new(),
        }
    }

    /// `write` (main.zig:78-82).
    pub fn write(&mut self, bytes: &[u8]) {
        // split-borrow: the parser is taken out so `self` can be the
        // Perform target (vte's advance takes them as distinct borrows)
        let mut parser = std::mem::replace(&mut self.parser, vte::Parser::new());
        for &b in bytes {
            parser.advance(self, b);
        }
        self.parser = parser;
    }

    /// `resize` (main.zig:84-93). Width changes re-flow wrapped lines
    /// (ghostty semantics, probe-pinned); height changes keep the
    /// viewport bottom-anchored, pulling from scrollback.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 || (cols == self.cols && rows == self.rows) {
            return;
        }

        if self.in_alt {
            for row in &mut self.grid {
                row.resize(cols);
                row.wrapped = false;
            }
            self.grid.resize(rows as usize, Row::new(cols));
            self.grid.truncate(rows as usize);
            self.cols = cols;
            self.rows = rows;
            self.scroll_top = 0;
            self.scroll_bottom = rows.saturating_sub(1) as usize;
            self.clamp_cursor();
            self.cursor.pending_wrap = false;
            self.mouse_last_cell = None;
            self.viewport_offset = 0;
            return;
        }

        // Cursor as an absolute offset over the joined stream so it
        // survives the re-wrap (probe: "abcdef" 6→4 keeps the pen at the
        // end of the text on the new last row).
        let cursor_abs = self.cursor_abs_offset();

        let mut lines: Vec<Row> = if cols != self.cols {
            self.join_lines()
        } else {
            self.scrollback
                .drain(..)
                .chain(std::mem::take(&mut self.grid))
                .collect()
        };

        if cols != self.cols {
            lines = resplit(lines, cols);
        }

        // Trailing entirely-untouched rows are grid padding, not logical
        // lines — ghostty's reflow keeps them out of the redistribution
        // (probe: the "aaaa bbbb…" shrink shows all 4 content rows).
        while lines
            .last()
            .is_some_and(|r| r.cells.iter().all(|c| c.cp == '\0'))
        {
            lines.pop();
        }

        // Redistribute: the grid keeps the last `rows` lines; the rest
        // stays scrollback (capped). Growth pulls lines back in.
        let mut grid: Vec<Row> = Vec::new();
        let mut scrollback: VecDeque<Row> = VecDeque::new();
        for row in lines.into_iter() {
            grid.push(row);
            while grid.len() > rows as usize {
                let top = grid.remove(0);
                scrollback.push_back(top);
            }
        }
        while scrollback.len() > self.max_scrollback {
            scrollback.pop_front();
        }
        // height growth with empty scrollback: blank rows pad the BOTTOM
        // (the content stays top-anchored, probe R8)
        while grid.len() < rows as usize {
            grid.push(Row::new(cols));
        }

        self.grid = grid;
        self.scrollback = scrollback;
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1) as usize;

        // Re-derive the cursor by walking the re-flowed rows.
        self.place_cursor_by_offset(cursor_abs);
        self.viewport_offset = 0;
        self.mouse_last_cell = None;
        self.selection = None;
        self.cursor.pending_wrap = false;
    }

    /// Join soft-wrapped lines into single logical lines (reflow step 1).
    fn join_lines(&mut self) -> Vec<Row> {
        let source = self
            .scrollback
            .drain(..)
            .chain(std::mem::take(&mut self.grid));
        let mut lines: Vec<Row> = Vec::new();
        let mut pending: Option<Row> = None;
        for row in source {
            // a pending segment was wrapped by construction — the
            // incoming row continues it; the merged line continues
            // further iff the incoming row is itself wrapped
            if let Some(mut joined) = pending.take() {
                joined.cells.extend(row.cells);
                joined.wrapped = row.wrapped;
                if row.wrapped {
                    pending = Some(joined);
                } else {
                    lines.push(joined);
                }
            } else if row.wrapped {
                pending = Some(row);
            } else {
                lines.push(row);
            }
        }
        if let Some(mut tail) = pending {
            tail.wrapped = false;
            lines.push(tail);
        }
        lines
    }

    /// Cursor offset as an absolute cell index over the joined stream.
    /// `x` references the cell AT the cursor (pending-wrap reports the
    /// last written cell — probe R0/R4: the pin follows that cell
    /// through reflow).
    fn cursor_abs_offset(&self) -> usize {
        let mut abs = 0usize;
        for (i, row) in self.grid.iter().enumerate() {
            if i == self.cursor.y as usize {
                return abs + self.cursor.x as usize;
            }
            abs += row.cells.len();
        }
        abs
    }

    /// Re-place the cursor at an absolute cell offset over the
    /// re-flowed rows (probe: "abcdef" 6→4 leaves the pen at the end of
    /// the text on the new last row; grow-back returns it to (5,0)).
    fn place_cursor_by_offset(&mut self, mut abs: usize) {
        for (y, row) in self.grid.iter().enumerate() {
            if abs < row.cells.len() {
                self.cursor.y = y as u16;
                self.cursor.x = abs as u16;
                return;
            }
            abs -= row.cells.len();
        }
        let last = self.grid.len().saturating_sub(1);
        self.cursor.y = last as u16;
        self.cursor.x = self.grid[last].cells.len().saturating_sub(1) as u16;
    }

    /// `scroll` (main.zig:95-97) — viewport scroll by `delta` lines
    /// (negative = up into scrollback).
    pub fn scroll_viewport(&mut self, delta: i32) {
        let max = self.scrollback.len();
        if delta < 0 {
            self.viewport_offset = (self.viewport_offset + (-delta) as usize).min(max);
        } else {
            self.viewport_offset = self.viewport_offset.saturating_sub(delta as usize);
        }
    }

    /// `setSelection` (main.zig:99-104) — viewport coordinates.
    pub fn set_selection(&mut self, start: (u16, u16), end: (u16, u16)) {
        self.selection = Some((start, end));
    }

    /// `clearSelection` (main.zig:106-108).
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// `selectedText` (main.zib:110-114).
    pub fn selected_text(&self) -> String {
        let Some(((ax, ay), (fx, fy))) = self.selection else {
            return String::new();
        };
        let ((sx, sy), (ex, ey)) = if (ay, ax) <= (fy, fx) {
            ((ax, ay), (fx, fy))
        } else {
            ((fx, fy), (ax, ay))
        };
        let mut row_texts: Vec<String> = Vec::new();
        for y in sy..=ey.min(self.rows.saturating_sub(1)) {
            let Some(row) = self.viewport_row(y) else {
                continue;
            };
            let (lo, hi) = if sy == ey {
                (sx as usize, ex as usize)
            } else if y == sy {
                (sx as usize, row.cells.len().saturating_sub(1))
            } else if y == ey {
                (0, ex as usize)
            } else {
                (0, row.cells.len().saturating_sub(1))
            };
            let mut text = String::new();
            for x in lo..=hi.min(row.cells.len().saturating_sub(1)) {
                let cell = &row.cells[x];
                if cell.wide_tail || cell.cp == '\0' {
                    continue;
                }
                text.push(cell.cp);
            }
            row_texts.push(text);
        }
        // ghostty's selection string drops trailing empty rows
        while row_texts.last().is_some_and(String::is_empty) {
            row_texts.pop();
        }
        row_texts.join("\n")
    }

    /// `invalidate` (main.zig:120-122) — the port repaints fully every
    /// frame; kept for API parity.
    pub fn invalidate(&mut self) {}

    /// `setTransparentBackground` (main.zig:124-127) — render-side only.
    pub fn set_transparent_background(&mut self, _transparent: bool) {}

    /// `cursor` (main.zig:143-162). `has_value` is false only before the
    /// first compose upstream; the port composes eagerly.
    pub fn cursor(&self) -> TerminalCursor {
        TerminalCursor {
            x: self.cursor.x.min(self.cols.saturating_sub(1)),
            y: self.cursor.y.min(self.rows.saturating_sub(1)),
            has_value: true,
            visible: self.cursor.visible,
            blinking: self.cursor.blinking,
            wide_tail: self.cursor_wide_tail(),
            style: self.cursor.style,
            color: Some(self.default_fg),
        }
    }

    fn cursor_wide_tail(&self) -> bool {
        self.grid
            .get(self.cursor.y as usize)
            .and_then(|r| r.cells.get(self.cursor.x as usize))
            .is_some_and(|c| c.wide_tail)
    }

    /// `drainResponses` (main.zig:214-236): the preserved prefix is what
    /// callers observe; overflow surfaces once and resets.
    pub fn drain_responses(&mut self) -> Vec<u8> {
        self.response_overflow = false;
        self.responses.drain(..).collect()
    }

    fn queue_response(&mut self, data: &[u8]) {
        if self.responses.len() + data.len() > RESPONSE_LIMIT {
            self.response_overflow = true;
            let room = RESPONSE_LIMIT.saturating_sub(self.responses.len());
            let clamped = &data[..room.min(data.len())];
            self.responses.extend(clamped.iter().copied());
            return;
        }
        self.responses.extend(data.iter().copied());
    }

    /// Whether a response overflow is latched (the error FFI status).
    pub fn response_overflowed(&self) -> bool {
        self.response_overflow
    }

    // -----------------------------------------------------------------
    // Compose — compositor.zig
    // -----------------------------------------------------------------

    fn viewport_rows(&self) -> Vec<&Row> {
        let sb_len = self.scrollback.len();
        let offset = self.viewport_offset.min(sb_len);
        let total = sb_len + self.grid.len();
        // bottom-anchored viewport shifted up by `offset` lines: the
        // top line of the viewport in the sb+grid stream
        let top = sb_len
            .saturating_sub(offset)
            .min(total.saturating_sub(self.rows as usize));
        let bottom = (top + self.rows as usize).min(total);
        let mut rows: Vec<&Row> = Vec::with_capacity(self.rows as usize);
        if top < sb_len {
            rows.extend(self.scrollback.iter().skip(top).take(bottom - top));
        }
        if bottom > sb_len {
            let grid_from = top.saturating_sub(sb_len);
            rows.extend(
                self.grid
                    .iter()
                    .skip(grid_from)
                    .take(bottom.saturating_sub(sb_len) - grid_from),
            );
        }
        rows
    }

    fn viewport_row(&self, y: u16) -> Option<&Row> {
        self.viewport_rows().get(y as usize).copied()
    }

    /// The visible-screen text (`screen()`'s line source): viewport
    /// rows, trailing blanks trimmed, trailing empty lines dropped.
    pub fn screen_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .viewport_rows()
            .iter()
            .map(|row| {
                let mut s = String::new();
                for cell in &row.cells {
                    if cell.wide_tail {
                        continue;
                    }
                    s.push(if cell.cp == '\0' { ' ' } else { cell.cp });
                }
                s.trim_end().to_owned()
            })
            .collect();
        lines.truncate(self.rows as usize);
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines
    }

    /// `compose` (compositor.zig:8-50): clear each row with the default
    /// fg/bg, then paint styled cells; inverse and selection swap
    /// fg/bg (compositor.zig:100-105); wide tails are skipped
    /// (compositor.zig:96) and painted as same-style spaces.
    pub fn compose(
        &self,
        target: &mut OptimizedBuffer,
        origin_x: i32,
        origin_y: i32,
        transparent: bool,
    ) {
        let default_fg = Rgba::rgb(self.default_fg.0, self.default_fg.1, self.default_fg.2);
        let default_bg = if transparent {
            Rgba::default_color(self.default_bg.0, self.default_bg.1, self.default_bg.2, 255)
        } else {
            Rgba::rgb(self.default_bg.0, self.default_bg.1, self.default_bg.2)
        };
        let height = target.height as i32;
        let width = target.width as i32;
        let selection = self.selection.map(|((ax, ay), (fx, fy))| {
            if (ay, ax) <= (fy, fx) {
                ((ax, ay), (fx, fy))
            } else {
                ((fx, fy), (ax, ay))
            }
        });

        for (y, row) in self.viewport_rows().iter().enumerate() {
            let dest_y = origin_y + y as i32;
            if dest_y < 0 || dest_y >= height {
                continue;
            }
            // clearRow (compositor.zig:52-66)
            for x in 0..self.cols as i32 {
                let dest_x = origin_x + x;
                if dest_x < 0 || dest_x >= width {
                    continue;
                }
                target.set_cell(dest_x as u32, dest_y as u32, ' ', default_fg, default_bg, 0);
            }
            // text_end (compositor.zig:81-91)
            let text_end = row_text_end(row);
            // composeRow (compositor.zig:68-120)
            for (x, cell) in row.cells.iter().enumerate() {
                let dest_x = origin_x + x as i32;
                if dest_x < 0 || dest_x >= width {
                    continue;
                }
                if cell.wide_tail {
                    continue;
                }
                let fg = self.resolve_fg(cell.style.fg);
                let bg = self.resolve_bg(cell.style.bg, default_bg);
                let (mut fg_p, mut bg_p) = (fg, bg);
                if cell.style.attrs & text_attributes::INVERSE != 0 {
                    std::mem::swap(&mut fg_p, &mut bg_p);
                }
                if let Some(((sx, sy), (ex, ey))) = selection {
                    let y16 = y as u16;
                    let w = char_width(cell.cp).max(1) as u16;
                    let in_rows = y16 >= sy && y16 <= ey;
                    // compositor.zig:104 — `x < text_end && x + width > start && x <= end`
                    let col_hit = x < text_end
                        && (x as u16) + w > sx
                        && (x as u16) <= ex
                        && (y16 != sy || (x as u16) + w > sx)
                        && (y16 != ey || (x as u16) <= ex);
                    if in_rows && col_hit {
                        std::mem::swap(&mut fg_p, &mut bg_p);
                    }
                }
                if cell.cp == '\0' {
                    continue; // untouched tail keeps the clear value
                }
                // compositor.zig:149-156 — invisible cells paint a blank
                // with their attributes; INVERSE is consumed by the swap
                // (not an attribute bit, compositor.zig:102)
                let paint_char = if cell.style.attrs & text_attributes::HIDDEN != 0 {
                    ' '
                } else {
                    cell.cp
                };
                let attrs = (cell.style.attrs & !text_attributes::INVERSE) as u32;
                // `drawGrapheme` (compositor.zig:158) — the port's
                // grapheme-aware text path packs wide clusters + their
                // continuation cells (the capture's span shape)
                let mut s = [0u8; 4];
                let text = paint_char.encode_utf8(&mut s);
                target.draw_text(text, dest_x as u32, dest_y as u32, fg_p, Some(bg_p), attrs);
            }
        }
    }

    fn resolve_fg(&self, color: CellColor) -> Rgba {
        match color {
            CellColor::Default => {
                Rgba::rgb(self.default_fg.0, self.default_fg.1, self.default_fg.2)
            }
            CellColor::Idx(i) => {
                let (r, g, b) = palette_rgb(i);
                Rgba::rgb(r, g, b)
            }
            CellColor::Rgb(r, g, b) => Rgba::rgb(r, g, b),
        }
    }

    fn resolve_bg(&self, color: CellColor, default_bg: Rgba) -> Rgba {
        match color {
            CellColor::Default => default_bg,
            CellColor::Idx(i) => {
                let (r, g, b) = palette_rgb(i);
                Rgba::rgb(r, g, b)
            }
            CellColor::Rgb(r, g, b) => Rgba::rgb(r, g, b),
        }
    }

    // -----------------------------------------------------------------
    // Screen primitives
    // -----------------------------------------------------------------

    fn blank_style(&self) -> CellStyle {
        CellStyle {
            fg: self.pen.fg,
            bg: self.pen.bg,
            attrs: self.pen.attrs,
        }
    }

    fn scroll_up(&mut self, count: usize) {
        // region scroll (top > 0 or alternate screen): the top line is
        // discarded, not pushed to scrollback
        for _ in 0..count {
            self.grid.remove(self.scroll_top);
            self.grid.insert(self.scroll_bottom, Row::new(self.cols));
        }
    }

    fn scroll_up_pushback(&mut self, count: usize) {
        for _ in 0..count {
            let top = self.grid.remove(self.scroll_top);
            if !self.in_alt && self.scroll_top == 0 {
                self.scrollback.push_back(top);
                while self.scrollback.len() > self.max_scrollback {
                    self.scrollback.pop_front();
                }
            }
            self.grid.insert(self.scroll_bottom, Row::new(self.cols));
        }
    }

    fn scroll_down(&mut self, count: usize) {
        for _ in 0..count {
            self.grid.remove(self.scroll_bottom);
            self.grid.insert(self.scroll_top, Row::new(self.cols));
        }
    }

    fn print(&mut self, c: char) {
        let width = char_width(c);
        if width == 0 {
            return; // combining marks: narrowed to identity
        }
        if self.cursor.pending_wrap {
            self.cursor.pending_wrap = false;
            self.linefeed_raw();
            self.carriage_return();
        }
        if width == 2 && self.autowrap && self.cursor.x as usize + 1 >= self.cols as usize {
            self.linefeed_raw();
            self.carriage_return();
        }
        if self.cursor.x as usize >= self.cols as usize {
            self.cursor.x = self.cols.saturating_sub(1);
        }
        let y = self.cursor.y as usize;
        let x = self.cursor.x as usize;
        let style = self.blank_style();
        if let Some(row) = self.grid.get_mut(y) {
            if x < row.cells.len() {
                row.cells[x] = Cell {
                    cp: c,
                    wide_tail: false,
                    style,
                };
                for i in 1..width {
                    if x + i < row.cells.len() {
                        row.cells[x + i] = Cell {
                            cp: '\0',
                            wide_tail: true,
                            style,
                        };
                    }
                }
            }
        }
        let new_x = x + width;
        if new_x >= self.cols as usize {
            self.cursor.x = self.cols.saturating_sub(1);
            self.cursor.pending_wrap = self.autowrap;
            // the row soft-wraps into the next (reflow joins on resize)
            if self.autowrap {
                if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                    row.wrapped = true;
                }
            }
        } else {
            self.cursor.x = new_x as u16;
            self.cursor.pending_wrap = false;
        }
    }

    /// LF: down one row, same column (probe: pure `\n` never CRs).
    /// At the scroll-region bottom the region scrolls; a full-screen
    /// scroll pushes the top line into scrollback (primary only).
    fn linefeed_raw(&mut self) {
        if self.cursor.y as usize >= self.scroll_bottom {
            if self.in_alt || self.scroll_top != 0 {
                self.scroll_up(1);
            } else {
                self.scroll_up_pushback(1);
            }
        } else if self.cursor.y as usize + 1 < self.rows as usize {
            self.cursor.y += 1;
        } else {
            if self.in_alt || self.scroll_top != 0 {
                self.scroll_up(1);
            } else {
                self.scroll_up_pushback(1);
            }
        }
    }

    fn carriage_return(&mut self) {
        self.cursor.x = 0;
        self.cursor.pending_wrap = false;
    }

    fn backspace(&mut self) {
        if self.cursor.pending_wrap {
            self.cursor.pending_wrap = false;
            return;
        }
        if self.cursor.x > 0 {
            self.cursor.x -= 1;
        }
    }

    fn tab(&mut self) {
        let mut x = self.cursor.x as usize;
        while x + 1 < self.cols as usize && !(x + 1).is_multiple_of(8) {
            x += 1;
        }
        x = (x + 1).min(self.cols as usize - 1);
        self.cursor.x = x as u16;
        self.cursor.pending_wrap = false;
    }

    fn erase_cells(&mut self, y: usize, from: usize, to: usize) {
        let style = self.blank_style();
        if let Some(row) = self.grid.get_mut(y) {
            for x in from..to.min(row.cells.len()) {
                row.cells[x] = Cell::blank(style);
            }
        }
    }

    fn move_to(&mut self, col: usize, row: usize) {
        let row = if self.origin_mode {
            row.saturating_add(self.scroll_top)
        } else {
            row
        };
        self.cursor.x = (col as u16).min(self.cols.saturating_sub(1));
        self.cursor.y = (row as u16).min(self.rows.saturating_sub(1));
        self.cursor.pending_wrap = false;
    }

    fn clamp_cursor(&mut self) {
        self.cursor.x = self.cursor.x.min(self.cols.saturating_sub(1));
        self.cursor.y = self.cursor.y.min(self.rows.saturating_sub(1));
    }

    // -----------------------------------------------------------------
    // Input encoders — ghostty src/input/*.zig ports
    // -----------------------------------------------------------------

    /// `encodeKey` (main.zig:164-169 → key_encode.zig `encode`).
    pub fn encode_key(&self, input: &KeyInput) -> Vec<u8> {
        let mut out = Vec::new();
        let event = KeyEventEnc {
            action: input.action,
            key: input.key,
            mods: Mods::from_bits(input.mods),
            utf8: input.text.clone(),
            unshifted_codepoint: input.unshifted_codepoint,
        };
        let opts = self.encoder_options();
        encode_key(&mut out, &event, &opts);
        out
    }

    fn encoder_options(&self) -> EncoderOptions {
        EncoderOptions {
            cursor_key_application: self.cursor_keys_app,
            backarrow_key_mode: false,
            alt_esc_prefix: false,
            kitty_flags: self.kitty_flags,
        }
    }

    /// `encodeMouse` (main.zig:171-188): cell coordinates (1×1 cells, no
    /// padding) and pixel formats suppressed (main.zig:179-180).
    pub fn encode_mouse(&mut self, input: &MouseInput) -> Vec<u8> {
        let mut out = Vec::new();
        if self.mouse_format == MouseFormat::SgrPixels {
            return out;
        }
        let cx = (input.x.floor() as i32).clamp(0, self.cols as i32 - 1);
        let cy = (input.y.floor() as i32).clamp(0, self.rows as i32 - 1);
        let event = MouseEncEvent {
            action: input.action,
            button: input.button,
            mods: Mods::from_bits(input.mods),
            pos: (input.x, input.y),
            cell: (cx, cy),
            screen: (self.cols as i32, self.rows as i32),
        };
        let opts = MouseEncOptions {
            event: self.mouse_event,
            format: self.mouse_format,
            any_button_pressed: input.any_button_pressed,
            last_cell: &mut self.mouse_last_cell,
        };
        encode_mouse(&mut out, &event, opts);
        out
    }

    /// `encodePaste` (main.zig:190-199 → paste.zig).
    pub fn encode_paste(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if self.bracketed_paste {
            out.extend_from_slice(b"\x1b[200~");
            out.extend(strip_unsafe(data));
            out.extend_from_slice(b"\x1b[201~");
        } else {
            for b in strip_unsafe(data) {
                out.push(if b == b'\n' { b'\r' } else { b });
            }
        }
        out
    }

    /// `encodeFocus` (main.zig:201-208) — only in mode 1004.
    pub fn encode_focus(&self, focused: bool) -> Vec<u8> {
        if !self.focus_event {
            return Vec::new();
        }
        if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        }
    }
}

/// xterm's unsafe-paste bytes (input/paste.zig `strip`).
const PASTE_STRIP: [u8; 16] = [
    0x00, 0x08, 0x05, 0x04, 0x1b, 0x7f, 0x03, 0x1c, 0x15, 0x1a, 0x11, 0x13, 0x17, 0x16, 0x12, 0x0f,
];

fn strip_unsafe(data: &[u8]) -> Vec<u8> {
    data.iter()
        .map(|&b| if PASTE_STRIP.contains(&b) { b' ' } else { b })
        .collect()
}

/// `text_end` (compositor.zig:81-91): the used-cell extent (wide tails
/// kept, unused row tails not).
fn row_text_end(row: &Row) -> usize {
    let mut x = row.cells.len();
    while x > 0 {
        x -= 1;
        let cell = &row.cells[x];
        if cell.wide_tail {
            continue;
        }
        if cell.cp != '\0' {
            return x + char_width(cell.cp).max(1);
        }
    }
    0
}

/// Split joined lines at the new width, keeping wide pairs intact.
/// Chunks are padded back to full width (untouched tails).
fn resplit(lines: Vec<Row>, cols: u16) -> Vec<Row> {
    let limit = cols as usize;
    let mut out = Vec::new();
    for line in lines {
        let cells = line.cells;
        let mut chunk: Vec<Cell> = Vec::with_capacity(limit);
        let mut width_used = 0usize;
        for cell in cells {
            let w = if cell.wide_tail {
                0
            } else {
                char_width(cell.cp)
            };
            if width_used + w > limit && !chunk.is_empty() {
                out.push(Row {
                    cells: chunk,
                    wrapped: true,
                });
                chunk = Vec::with_capacity(limit);
                width_used = 0;
            }
            chunk.push(cell);
            width_used += w;
        }
        chunk.resize(limit, Cell::default());
        out.push(Row {
            cells: chunk,
            wrapped: false,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// vte Perform — the VT stream half of ghostty's TerminalStream
// ---------------------------------------------------------------------------

impl vte::Perform for Engine {
    fn print(&mut self, c: char) {
        self.print(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.carriage_return(),
            b'\n' | 0x0b | 0x0c => self.linefeed_raw(),
            0x08 => self.backspace(),
            b'\t' => self.tab(),
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let byte = action as u8;
        // DEC private modes arrive with a '?' intermediate; kitty flag
        // control with '>' / '<' / '='.
        match intermediates {
            [b'?'] => {
                let set = byte == b'h';
                if byte == b'h' || byte == b'l' {
                    for param in params.iter() {
                        if let Some(&m) = param.first() {
                            self.set_dec_mode(m, set);
                        }
                    }
                }
                return;
            }
            [b'>'] => {
                if byte == b'u' {
                    if let Some(p) = params.iter().next().and_then(|s| s.first()) {
                        let flags = *p as u8;
                        self.kitty_stack.push(flags);
                        self.kitty_flags = flags;
                    } else {
                        self.kitty_stack.push(0);
                        self.kitty_flags = 0;
                    }
                }
                return;
            }
            [b'<'] => {
                if byte == b'u' {
                    self.kitty_stack.pop();
                    self.kitty_flags = self.kitty_stack.last().copied().unwrap_or(0);
                }
                return;
            }
            [b'='] => {
                if byte == b'u' {
                    if let Some(p) = params.iter().next().and_then(|s| s.first()) {
                        self.kitty_flags = *p as u8;
                    }
                }
                return;
            }
            _ if !intermediates.is_empty() => return,
            _ => {}
        }

        let p: Vec<u16> = params.iter().filter_map(|s| s.first().copied()).collect();
        match byte {
            b'm' => self.sgr(&p),
            b'A' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor.y = (self.cursor.y as usize)
                    .saturating_sub(n)
                    .max(self.scroll_top) as u16;
                self.cursor.pending_wrap = false;
            }
            b'B' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor.y = ((self.cursor.y as usize) + n).min(
                    self.scroll_bottom
                        .min((self.rows as usize).saturating_sub(1)),
                ) as u16;
                self.cursor.pending_wrap = false;
            }
            b'C' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor.x = ((self.cursor.x as usize) + n).min(self.cols as usize - 1) as u16;
                self.cursor.pending_wrap = false;
            }
            b'D' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor.x = (self.cursor.x as usize).saturating_sub(n) as u16;
                self.cursor.pending_wrap = false;
            }
            b'E' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor.y = ((self.cursor.y as usize) + n).min(
                    self.scroll_bottom
                        .min((self.rows as usize).saturating_sub(1)),
                ) as u16;
                self.cursor.x = 0;
                self.cursor.pending_wrap = false;
            }
            b'F' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor.y = (self.cursor.y as usize)
                    .saturating_sub(n)
                    .max(self.scroll_top) as u16;
                self.cursor.x = 0;
                self.cursor.pending_wrap = false;
            }
            b'G' => {
                let col = p.first().copied().unwrap_or(1).max(1) as usize - 1;
                self.cursor.x = col.min(self.cols as usize - 1) as u16;
                self.cursor.pending_wrap = false;
            }
            b'H' | b'f' => {
                let row = p.first().copied().unwrap_or(1).max(1) as usize - 1;
                let col = p.get(1).copied().unwrap_or(1).max(1) as usize - 1;
                self.move_to(col, row);
            }
            b'd' => {
                let row = p.first().copied().unwrap_or(1).max(1) as usize - 1;
                self.move_to(self.cursor.x as usize, row);
            }
            b'J' => {
                let mode = p.first().copied().unwrap_or(0);
                let y = self.cursor.y as usize;
                let x = self.cursor.x as usize;
                match mode {
                    0 => {
                        self.erase_cells(y, x, self.cols as usize);
                        for yy in y + 1..self.rows as usize {
                            self.erase_cells(yy, 0, self.cols as usize);
                        }
                    }
                    1 => {
                        self.erase_cells(y, 0, x + 1);
                        for yy in 0..y {
                            self.erase_cells(yy, 0, self.cols as usize);
                        }
                    }
                    _ => {
                        for yy in 0..self.rows as usize {
                            self.erase_cells(yy, 0, self.cols as usize);
                        }
                    }
                }
            }
            b'K' => {
                let mode = p.first().copied().unwrap_or(0);
                let y = self.cursor.y as usize;
                let x = self.cursor.x as usize;
                match mode {
                    0 => self.erase_cells(y, x, self.cols as usize),
                    1 => self.erase_cells(y, 0, x + 1),
                    _ => self.erase_cells(y, 0, self.cols as usize),
                }
            }
            b'X' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let y = self.cursor.y as usize;
                let x = self.cursor.x as usize;
                self.erase_cells(y, x, x + n);
            }
            b'L' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let cy = self.cursor.y as usize;
                if cy >= self.scroll_top && cy <= self.scroll_bottom {
                    for _ in 0..n {
                        self.grid.remove(self.scroll_bottom);
                        self.grid.insert(cy, Row::new(self.cols));
                    }
                }
            }
            b'M' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let cy = self.cursor.y as usize;
                if cy >= self.scroll_top && cy <= self.scroll_bottom {
                    for _ in 0..n {
                        self.grid.remove(cy);
                        self.grid.insert(self.scroll_bottom, Row::new(self.cols));
                    }
                }
            }
            b'P' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let y = self.cursor.y as usize;
                let x = self.cursor.x as usize;
                if let Some(row) = self.grid.get_mut(y) {
                    for _ in 0..n {
                        if x < row.cells.len() {
                            row.cells.remove(x);
                        }
                    }
                    row.resize(self.cols);
                }
            }
            b'@' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let y = self.cursor.y as usize;
                let x = self.cursor.x as usize;
                if let Some(row) = self.grid.get_mut(y) {
                    for _ in 0..n {
                        row.cells.insert(x, Cell::default());
                    }
                    row.resize(self.cols);
                }
            }
            b'S' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                if self.in_alt || self.scroll_top != 0 {
                    self.scroll_up(n);
                } else {
                    self.scroll_up_pushback(n);
                }
            }
            b'T' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.scroll_down(n);
            }
            b'r' => {
                let top = p.first().copied().unwrap_or(1).max(1) as usize - 1;
                let bottom = p
                    .get(1)
                    .copied()
                    .map_or(self.rows as usize, usize::from)
                    .min(self.rows as usize);
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom - 1;
                    self.move_to(0, 0);
                }
            }
            b's' => self.saved_cursor = (self.cursor, self.pen),
            b'u' => (self.cursor, self.pen) = self.saved_cursor,
            b'n' => match p.first().copied().unwrap_or(0) {
                // DSR 5 → OK; DSR 6 → CPR (probe: row0 col2 → "\x1b[1;3R")
                5 => self.queue_response(b"\x1b[0n"),
                6 => {
                    let msg = format!("\x1b[{};{}R", self.cursor.y + 1, self.cursor.x + 1);
                    self.queue_response(msg.as_bytes());
                }
                _ => {}
            },
            b'c' => {
                // DA1 — ghostty replies with its XTVERSION string
                // (probe: DCS >|libghostty ST)
                self.queue_response(b"\x1bP>|libghostty\x1b\\");
            }
            b'q' => {
                // DECSCUSR (xterm values → main.zig style codes)
                match p.first().copied().unwrap_or(0) {
                    0..=2 => self.cursor.style = 1,
                    3 | 4 => self.cursor.style = 2,
                    5 | 6 => self.cursor.style = 0,
                    _ => self.cursor.style = 3,
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved_cursor = (self.cursor, self.pen),
            b'8' => (self.cursor, self.pen) = self.saved_cursor,
            b'M' => {
                // RI
                if self.cursor.y as usize == self.scroll_top {
                    self.scroll_down(1);
                } else if self.cursor.y > 0 {
                    self.cursor.y -= 1;
                }
                self.cursor.pending_wrap = false;
            }
            b'D' => self.linefeed_raw(),
            b'E' => {
                self.linefeed_raw();
                self.carriage_return();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        let num: Option<u16> = std::str::from_utf8(params[0])
            .ok()
            .and_then(|s| s.parse().ok());
        let joined: Vec<u8> = params[1..].concat();
        let text = String::from_utf8_lossy(&joined).to_string();
        match num {
            // OSC 10/11 set the panel defaults (probe: retroactive — the
            // whole pane repaints with the new default colors)
            Some(10) => {
                if let Some((r, g, b)) = parse_osc_color(&text) {
                    self.default_fg = (r, g, b);
                }
            }
            Some(11) => {
                if let Some((r, g, b)) = parse_osc_color(&text) {
                    self.default_bg = (r, g, b);
                }
            }
            Some(110) => self.default_fg = DEFAULT_FG,
            Some(111) => self.default_bg = DEFAULT_BG,
            _ => {}
        }
    }
}

fn parse_osc_color(text: &str) -> Option<(u8, u8, u8)> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r, g, b));
        }
    }
    if let Some(rest) = t.strip_prefix("rgb:") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() == 3 {
            let scale = |s: &str| -> Option<u8> {
                match s.len() {
                    1 => u8::from_str_radix(s, 16).ok().map(|v| v * 17),
                    2 => u8::from_str_radix(s, 16).ok(),
                    4 => u16::from_str_radix(s, 16).ok().map(|v| (v >> 8) as u8),
                    _ => None,
                }
            };
            return Some((scale(parts[0])?, scale(parts[1])?, scale(parts[2])?));
        }
    }
    None
}

impl Engine {
    fn sgr(&mut self, p: &[u16]) {
        if p.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        while i < p.len() {
            let code = p[i];
            match code {
                0 => self.pen = Pen::default(),
                1 => self.pen.attrs |= text_attributes::BOLD,
                2 => self.pen.attrs |= text_attributes::DIM,
                3 => self.pen.attrs |= text_attributes::ITALIC,
                4 => self.pen.attrs |= text_attributes::UNDERLINE,
                5 | 6 => self.pen.attrs |= text_attributes::BLINK,
                7 => self.pen.attrs |= text_attributes::INVERSE,
                8 => self.pen.attrs |= text_attributes::HIDDEN,
                9 => self.pen.attrs |= text_attributes::STRIKETHROUGH,
                21 | 22 => self.pen.attrs &= !(text_attributes::BOLD | text_attributes::DIM),
                23 => self.pen.attrs &= !text_attributes::ITALIC,
                24 => self.pen.attrs &= !text_attributes::UNDERLINE,
                25 => self.pen.attrs &= !text_attributes::BLINK,
                27 => self.pen.attrs &= !text_attributes::INVERSE,
                28 => self.pen.attrs &= !text_attributes::HIDDEN,
                29 => self.pen.attrs &= !text_attributes::STRIKETHROUGH,
                30..=37 => self.pen.fg = CellColor::Idx((code - 30) as u8),
                39 => self.pen.fg = CellColor::Default,
                40..=47 => self.pen.bg = CellColor::Idx((code - 40) as u8),
                49 => self.pen.bg = CellColor::Default,
                90..=97 => self.pen.fg = CellColor::Idx((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = CellColor::Idx((code - 100 + 8) as u8),
                38 | 48 => {
                    let (color, consumed) = parse_ext_color(p, i);
                    if let Some(color) = color {
                        if code == 38 {
                            self.pen.fg = color;
                        } else {
                            self.pen.bg = color;
                        }
                    }
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_dec_mode(&mut self, mode: u16, set: bool) {
        match mode {
            6 => self.origin_mode = set,
            7 => self.autowrap = set,
            25 => self.cursor.visible = set,
            47 | 1047 => self.set_alt_screen(set, false),
            1048 => {
                if set {
                    self.saved_cursor = (self.cursor, self.pen);
                } else {
                    (self.cursor, self.pen) = self.saved_cursor;
                }
            }
            1049 => self.set_alt_screen(set, true),
            1000 => self.mouse_event = set_then(MouseEventMode::Normal, set, &mut self.mouse_event),
            1002 => self.mouse_event = set_then(MouseEventMode::Button, set, &mut self.mouse_event),
            1003 => self.mouse_event = set_then(MouseEventMode::Any, set, &mut self.mouse_event),
            1005 => {
                self.mouse_format = if set {
                    MouseFormat::Utf8
                } else if self.mouse_format == MouseFormat::Utf8 {
                    MouseFormat::X10
                } else {
                    self.mouse_format
                }
            }
            1006 => {
                self.mouse_format = if set {
                    MouseFormat::Sgr
                } else if self.mouse_format == MouseFormat::Sgr {
                    MouseFormat::X10
                } else {
                    self.mouse_format
                }
            }
            1015 => {
                self.mouse_format = if set {
                    MouseFormat::Urxvt
                } else if self.mouse_format == MouseFormat::Urxvt {
                    MouseFormat::X10
                } else {
                    self.mouse_format
                }
            }
            1016 => {
                self.mouse_format = if set {
                    MouseFormat::SgrPixels
                } else if self.mouse_format == MouseFormat::SgrPixels {
                    MouseFormat::X10
                } else {
                    self.mouse_format
                }
            }
            2004 => self.bracketed_paste = set,
            1004 => self.focus_event = set,
            1 => self.cursor_keys_app = set,
            _ => {}
        }
    }

    fn set_alt_screen(&mut self, enter: bool, save_cursor: bool) {
        if enter == self.in_alt {
            return;
        }
        if enter {
            if save_cursor {
                self.saved_cursor = (self.cursor, self.pen);
            }
            let primary = std::mem::take(&mut self.grid);
            let sb_len = self.scrollback.len();
            self.saved_primary = Some((primary, self.cursor, self.pen, sb_len));
            self.grid = (0..self.rows).map(|_| Row::new(self.cols)).collect();
            self.in_alt = true;
            self.scroll_top = 0;
            self.scroll_bottom = self.rows.saturating_sub(1) as usize;
            self.move_to(0, 0);
        } else {
            let (primary, cursor, pen, sb_len) = self.saved_primary.take().unwrap_or_else(|| {
                (
                    (0..self.rows)
                        .map(|_| Row::new(self.cols))
                        .collect::<Vec<_>>(),
                    CursorState::default(),
                    Pen::default(),
                    0usize,
                )
            });
            self.grid = primary;
            self.scrollback.truncate(sb_len);
            self.cursor = cursor;
            self.pen = pen;
            self.in_alt = false;
            if save_cursor {
                (self.cursor, self.pen) = self.saved_cursor;
            }
            self.scroll_top = 0;
            self.scroll_bottom = self.rows.saturating_sub(1) as usize;
            self.clamp_cursor();
        }
        self.viewport_offset = 0;
    }
}

fn set_then(target: MouseEventMode, set: bool, current: &mut MouseEventMode) -> MouseEventMode {
    if set {
        target
    } else if *current == target {
        MouseEventMode::None
    } else {
        *current
    }
}

fn parse_ext_color(p: &[u16], i: usize) -> (Option<CellColor>, usize) {
    match p.get(i + 1).copied() {
        Some(5) => (p.get(i + 2).map(|&n| CellColor::Idx(n as u8)), 2),
        Some(2) => {
            let color = match (p.get(i + 2), p.get(i + 3), p.get(i + 4)) {
                (Some(r), Some(g), Some(b)) => Some(CellColor::Rgb(*r as u8, *g as u8, *b as u8)),
                _ => None,
            };
            (color, 4)
        }
        _ => (None, 0),
    }
}

// ---------------------------------------------------------------------------
// Key encoder — ghostty src/input/key_encode.zig
// ---------------------------------------------------------------------------

struct KeyEventEnc {
    action: KeyAction,
    key: LogicalKey,
    mods: Mods,
    utf8: String,
    unshifted_codepoint: u32,
}

struct EncoderOptions {
    cursor_key_application: bool,
    backarrow_key_mode: bool,
    alt_esc_prefix: bool,
    kitty_flags: u8,
}

/// kitty entries (input/kitty.zig) — functional keys.
fn kitty_entry(key: LogicalKey) -> Option<(u32, u8, bool)> {
    let (code, final_, modifier) = match key {
        LogicalKey::Escape => (27, b'u', false),
        LogicalKey::Enter => (13, b'u', false),
        LogicalKey::Tab => (9, b'u', false),
        LogicalKey::Backspace => (127, b'u', false),
        LogicalKey::Insert => (2, b'~', false),
        LogicalKey::Delete => (3, b'~', false),
        LogicalKey::ArrowLeft => (1, b'D', false),
        LogicalKey::ArrowRight => (1, b'C', false),
        LogicalKey::ArrowUp => (1, b'A', false),
        LogicalKey::ArrowDown => (1, b'B', false),
        LogicalKey::PageUp => (5, b'~', false),
        LogicalKey::PageDown => (6, b'~', false),
        LogicalKey::Home => (1, b'H', false),
        LogicalKey::End => (1, b'F', false),
        LogicalKey::CapsLock => (57358, b'u', true),
        LogicalKey::ScrollLock => (57359, b'u', false),
        LogicalKey::NumLock => (57360, b'u', true),
        LogicalKey::PrintScreen => (57361, b'u', false),
        LogicalKey::Pause => (57362, b'u', false),
        LogicalKey::F(n) => match n {
            1 => (1, b'P', false),
            2 => (1, b'Q', false),
            3 => (13, b'~', false),
            4 => (1, b'S', false),
            5 => (15, b'~', false),
            6 => (17, b'~', false),
            7 => (18, b'~', false),
            8 => (19, b'~', false),
            9 => (20, b'~', false),
            10 => (21, b'~', false),
            11 => (23, b'~', false),
            12 => (24, b'~', false),
            13..=25 => (57361 + (n as u32 - 12), b'u', false), // f13..f25
            _ => return None,
        },
        _ => return None,
    };
    Some((code, final_, modifier))
}

/// `encode` (key_encode.zig).
fn encode_key(out: &mut Vec<u8>, event: &KeyEventEnc, opts: &EncoderOptions) {
    if opts.kitty_flags != 0 {
        kitty_encode(out, event, opts);
    } else {
        legacy_encode(out, event, opts);
    }
}

fn is_control_cp(cp: u32) -> bool {
    cp < 0x20 || cp == 0x7f
}

fn is_control_str(s: &str) -> bool {
    s.len() == 1 && is_control_cp(s.as_bytes()[0] as u32)
}

fn kitty_encode(out: &mut Vec<u8>, event: &KeyEventEnc, opts: &EncoderOptions) {
    if event.action == KeyAction::Release && opts.kitty_flags & kitty_flags::REPORT_EVENTS == 0 {
        return;
    }
    if event.action == KeyAction::Release
        && opts.kitty_flags & kitty_flags::REPORT_ALL == 0
        && matches!(
            event.key,
            LogicalKey::Enter | LogicalKey::Backspace | LogicalKey::Tab
        )
    {
        return;
    }

    let binding_mods = event.mods.binding();

    let entry: Option<(u32, u8, bool)> = kitty_entry(event.key).or_else(|| {
        (event.unshifted_codepoint > 0).then_some((event.unshifted_codepoint, b'u', false))
    });

    // IME confirmation path (key_encode.zig preprocessing)
    if !event.utf8.is_empty()
        && matches!(event.key, LogicalKey::Enter | LogicalKey::Backspace)
        && !is_control_str(&event.utf8)
    {
        if event.key == LogicalKey::Backspace {
            return;
        }
        out.extend_from_slice(event.utf8.as_bytes());
        return;
    }

    if opts.kitty_flags & kitty_flags::REPORT_ALL == 0 {
        if binding_mods.is_empty() {
            match event.key {
                LogicalKey::Enter => {
                    out.push(b'\r');
                    return;
                }
                LogicalKey::Tab => {
                    out.push(b'\t');
                    return;
                }
                LogicalKey::Backspace => {
                    out.push(0x7f);
                    return;
                }
                _ => {}
            }
        }
        // plain text: printable, no binding mods, not release
        if !event.utf8.is_empty()
            && binding_mods.is_empty()
            && event.action != KeyAction::Release
            && event.utf8.chars().all(|c| !is_control_cp(c as u32))
        {
            out.extend_from_slice(event.utf8.as_bytes());
            return;
        }
    }

    let Some((key_code, final_, entry_modifier)) = entry else {
        if event.action == KeyAction::Release || event.utf8.is_empty() {
            return;
        }
        out.extend_from_slice(event.utf8.as_bytes());
        return;
    };

    if entry_modifier && opts.kitty_flags & kitty_flags::REPORT_ALL == 0 {
        return;
    }

    // KittyMods.fromInput (key_encode.zig: shift bit0, alt bit1, ctrl
    // bit2, super bit3, caps bit6, num bit7 — seqInt = int + 1)
    let mut mods_seq: u16 = 1;
    if event.mods.shift {
        mods_seq += 1;
    }
    if event.mods.alt {
        mods_seq += 2;
    }
    if event.mods.ctrl {
        mods_seq += 4;
    }
    if event.mods.super_ {
        mods_seq += 8;
    }
    if event.mods.caps_lock {
        mods_seq += 64;
    }
    if event.mods.num_lock {
        mods_seq += 128;
    }

    let event_code: u8 = if opts.kitty_flags & kitty_flags::REPORT_EVENTS != 0 {
        match event.action {
            KeyAction::Press => 1,
            KeyAction::Repeat => 2,
            KeyAction::Release => 3,
        }
    } else {
        0
    };

    // alternates (key_encode.zig report_alternates block)
    let mut alt_shifted: Option<u32> = None;
    let mut alt_base: Option<u32> = None;
    if opts.kitty_flags & kitty_flags::REPORT_ALTERNATES != 0 && !is_control_cp(key_code) {
        let mut cps = event.utf8.chars();
        if let Some(cp1) = cps.next() {
            let has_cp2 = cps.next().is_some();
            if cp1 as u32 != key_code && event.mods.shift {
                alt_shifted = Some(cp1 as u32);
            }
            if let Some(base) = event.key.codepoint() {
                if base as u32 != key_code && cp1 as u32 != base as u32 && !has_cp2 {
                    alt_base = Some(base as u32);
                }
            }
        } else if let Some(base) = event.key.codepoint() {
            if base as u32 != key_code {
                alt_base = Some(base as u32);
            }
        }
    }

    // KittySequence.encode — encodeFull / encodeSpecial
    if final_ == b'u' || final_ == b'~' {
        out.extend(format!("\x1b[{}", key_code).as_bytes());
        if let Some(s) = alt_shifted {
            out.extend(format!(":{}", s).as_bytes());
        }
        if let Some(b) = alt_base {
            if alt_shifted.is_none() {
                out.extend(format!("::{}", b).as_bytes());
            } else {
                out.extend(format!(":{}", b).as_bytes());
            }
        }
        if event_code != 0 && event_code != 1 {
            out.extend(format!(";{}:{}", mods_seq, event_code).as_bytes());
        } else if mods_seq > 1 {
            out.extend(format!(";{}", mods_seq).as_bytes());
        }
        out.push(final_);
    } else if event_code != 0 {
        out.extend(format!("\x1b[1;{}:{}{}", mods_seq, event_code, final_ as char).as_bytes());
    } else if mods_seq > 1 {
        out.extend(format!("\x1b[1;{}{}", mods_seq, final_ as char).as_bytes());
    } else {
        out.extend(format!("\x1b[{}", final_ as char).as_bytes());
    }
}

/// function_keys.zig `modifiers` — index + 2 = the mods code.
fn mods_code(m: Mods) -> Option<u8> {
    let mut code = 0u8;
    if m.shift {
        code |= 1;
    }
    if m.alt {
        code |= 2;
    }
    if m.ctrl {
        code |= 4;
    }
    if m.super_ {
        code |= 8;
    }
    // no mods → the pc-style table entries require exact matches; empty
    // means the plain entry (handled separately).
    if code == 0 {
        return None;
    }
    Some(code + 1) // shift=2, alt=3, shift+alt=4, ctrl=5, … (matches
                   // the 15-entry modifier table order)
}

/// pcStyleFunctionKey (key_encode.zig + function_keys.zig tables) —
/// the entries reachable from upstream's physicalKey vocabulary.
fn pc_style_function_key(
    key: LogicalKey,
    mods: Mods,
    cursor_app: bool,
    backarrow: bool,
) -> Option<Vec<u8>> {
    let binding = mods.binding();
    let code = mods_code(binding);
    let empty = binding.is_empty();

    let pc = |final_: char, prefix: u32, code: Option<u8>| -> Option<Vec<u8>> {
        code.map(|c| format!("\x1b[{};{c}{}", prefix, final_).into_bytes())
    };

    match key {
        LogicalKey::ArrowUp => {
            if empty {
                Some(if cursor_app {
                    b"\x1bOA".to_vec()
                } else {
                    b"\x1b[A".to_vec()
                })
            } else {
                pc('A', 1, code)
            }
        }
        LogicalKey::ArrowDown => {
            if empty {
                Some(if cursor_app {
                    b"\x1bOB".to_vec()
                } else {
                    b"\x1b[B".to_vec()
                })
            } else {
                pc('B', 1, code)
            }
        }
        LogicalKey::ArrowRight => {
            if empty {
                Some(if cursor_app {
                    b"\x1bOC".to_vec()
                } else {
                    b"\x1b[C".to_vec()
                })
            } else {
                pc('C', 1, code)
            }
        }
        LogicalKey::ArrowLeft => {
            if empty {
                Some(if cursor_app {
                    b"\x1bOD".to_vec()
                } else {
                    b"\x1b[D".to_vec()
                })
            } else {
                pc('D', 1, code)
            }
        }
        LogicalKey::Home => {
            if empty {
                Some(if cursor_app {
                    b"\x1bOH".to_vec()
                } else {
                    b"\x1b[H".to_vec()
                })
            } else {
                pc('H', 1, code)
            }
        }
        LogicalKey::End => {
            if empty {
                Some(if cursor_app {
                    b"\x1bOF".to_vec()
                } else {
                    b"\x1b[F".to_vec()
                })
            } else {
                pc('F', 1, code)
            }
        }
        LogicalKey::Insert => {
            if empty {
                Some(b"\x1b[2~".to_vec())
            } else {
                pc('~', 2, code)
            }
        }
        LogicalKey::Delete => {
            if empty {
                Some(b"\x1b[3~".to_vec())
            } else {
                pc('~', 3, code)
            }
        }
        LogicalKey::PageUp => {
            if empty {
                Some(b"\x1b[5~".to_vec())
            } else {
                pc('~', 5, code)
            }
        }
        LogicalKey::PageDown => {
            if empty {
                Some(b"\x1b[6~".to_vec())
            } else {
                pc('~', 6, code)
            }
        }
        LogicalKey::F(n) => match n {
            1 => {
                if empty {
                    Some(b"\x1bOP".to_vec())
                } else {
                    pc('P', 1, code)
                }
            }
            2 => {
                if empty {
                    Some(b"\x1bOQ".to_vec())
                } else {
                    pc('Q', 1, code)
                }
            }
            3 => {
                if empty {
                    Some(b"\x1bOR".to_vec())
                } else {
                    pc('~', 13, code)
                }
            }
            4 => {
                if empty {
                    Some(b"\x1bOS".to_vec())
                } else {
                    pc('S', 1, code)
                }
            }
            5 => {
                if empty {
                    Some(b"\x1b[15~".to_vec())
                } else {
                    pc('~', 15, code)
                }
            }
            6 => {
                if empty {
                    Some(b"\x1b[17~".to_vec())
                } else {
                    pc('~', 17, code)
                }
            }
            7 => {
                if empty {
                    Some(b"\x1b[18~".to_vec())
                } else {
                    pc('~', 18, code)
                }
            }
            8 => {
                if empty {
                    Some(b"\x1b[19~".to_vec())
                } else {
                    pc('~', 19, code)
                }
            }
            9 => {
                if empty {
                    Some(b"\x1b[20~".to_vec())
                } else {
                    pc('~', 20, code)
                }
            }
            10 => {
                if empty {
                    Some(b"\x1b[21~".to_vec())
                } else {
                    pc('~', 21, code)
                }
            }
            11 => {
                if empty {
                    Some(b"\x1b[23~".to_vec())
                } else {
                    pc('~', 23, code)
                }
            }
            12 => {
                if empty {
                    Some(b"\x1b[24~".to_vec())
                } else {
                    pc('~', 24, code)
                }
            }
            _ => None,
        },
        LogicalKey::Backspace => {
            let (s, a, c, su) = (binding.shift, binding.alt, binding.ctrl, binding.super_);
            match (s, a, c, su) {
                (false, false, true, false) => Some(if backarrow {
                    b"\x7f".to_vec()
                } else {
                    b"\x08".to_vec()
                }),
                (false, false, false, false) => Some(if backarrow {
                    b"\x08".to_vec()
                } else {
                    b"\x7f".to_vec()
                }),
                (true, false, true, false) => Some(b"\x08".to_vec()),
                (false, true, false, false) => Some(b"\x1b\x7f".to_vec()),
                (true, true, false, false) => Some(b"\x1b\x7f".to_vec()),
                (false, true, true, false) => Some(b"\x1b\x08".to_vec()),
                (true, true, true, false) => Some(b"\x1b\x08".to_vec()),
                (false, false, false, true) => Some(b"\x7f".to_vec()),
                (true, false, false, true) => Some(b"\x7f".to_vec()),
                (false, true, false, true) => Some(b"\x1b\x7f".to_vec()),
                (true, true, false, true) => Some(b"\x1b\x7f".to_vec()),
                (false, false, true, true) => Some(b"\x08".to_vec()),
                (true, false, true, true) => Some(b"\x08".to_vec()),
                (false, true, true, true) => Some(b"\x1b\x08".to_vec()),
                (true, true, true, true) => Some(b"\x1b\x08".to_vec()),
                _ => None,
            }
        }
        LogicalKey::Tab => match (binding.shift, binding.alt, binding.ctrl, binding.super_) {
            (true, false, false, false) => Some(b"\x1b[Z".to_vec()),
            (false, true, false, false) => Some(b"\x1b\t".to_vec()),
            (false, false, false, false) => Some(b"\t".to_vec()),
            _ => code.map(|c| format!("\x1b[27;{c};9~").into_bytes()),
        },
        LogicalKey::Enter => match (binding.shift, binding.alt, binding.ctrl, binding.super_) {
            (false, false, false, false) => Some(b"\r".to_vec()),
            _ => code.map(|c| format!("\x1b[27;{c};13~").into_bytes()),
        },
        LogicalKey::Escape => match (binding.shift, binding.alt, binding.ctrl, binding.super_) {
            (false, false, false, false) => Some(b"\x1b".to_vec()),
            _ => code.map(|c| format!("\x1b[27;{c};27~").into_bytes()),
        },
        _ => None,
    }
}

/// ctrlSeq (key_encode.zig) — Ctrl+char → C0.
fn ctrl_seq(key: LogicalKey, utf8: &str, unshifted: u32, mods: Mods) -> Option<u8> {
    if !mods.ctrl {
        return None;
    }
    let ctrl_only = Mods {
        ctrl: true,
        ..Mods::default()
    }
    .int();
    let mut unset = mods.binding();
    unset.alt = false;
    let mut char_: u8 = match utf8.as_bytes() {
        [b] => *b,
        _ => {
            // logical-key codepoint path (cyrillic layouts): only with
            // exactly ctrl pressed (key_encode.zig)
            {
                let cp = key.codepoint()?;
                if cp.is_ascii() {
                    if unset.int() != ctrl_only {
                        return None;
                    }
                    cp as u8
                } else {
                    return None;
                }
            }
        }
    };
    if unset.shift && !char_.is_ascii_uppercase() && char_ != b'@' {
        unset.shift = false;
    }
    if char_.is_ascii_uppercase() && unshifted > 0 && unshifted <= 0xff {
        char_ = unshifted as u8;
    }
    if unset.int() != ctrl_only {
        return None;
    }
    Some(match char_ {
        b' ' => 0,
        b'/' => 31,
        b'0' => 48,
        b'1' => 49,
        b'2' => 0,
        b'3' => 27,
        b'4' => 28,
        b'5' => 29,
        b'6' => 30,
        b'7' => 31,
        b'8' => 127,
        b'9' => 57,
        b'?' => 127,
        b'@' => 0,
        b'\\' => 28,
        b']' => 29,
        b'^' => 30,
        b'_' => 31,
        b'a'..=b'z' => char_ - b'a' + 1,
        b'~' => 30,
        _ => return None,
    })
}

fn legacy_encode(out: &mut Vec<u8>, event: &KeyEventEnc, opts: &EncoderOptions) {
    let all_mods = event.mods;
    let binding_mods = all_mods.binding();

    if event.action != KeyAction::Press && event.action != KeyAction::Repeat {
        return;
    }

    // PC-style function keys take priority (with the utf8 carve-outs)
    if let Some(sequence) = pc_style_function_key(
        event.key,
        all_mods,
        opts.cursor_key_application,
        opts.backarrow_key_mode,
    ) {
        if !event.utf8.is_empty() {
            match event.key {
                LogicalKey::Backspace => {
                    if !is_control_str(&event.utf8) {
                        return;
                    }
                }
                LogicalKey::Enter | LogicalKey::Escape if !is_control_str(&event.utf8) => {
                    out.extend_from_slice(event.utf8.as_bytes());
                    return;
                }
                _ => {}
            }
        }
        out.extend_from_slice(&sequence);
        return;
    }

    // ctrlSeq
    if let Some(c0) = ctrl_seq(event.key, &event.utf8, event.unshifted_codepoint, all_mods) {
        if binding_mods.alt {
            out.push(0x1b);
            out.push(c0);
            return;
        }
        out.push(c0);
        return;
    }

    if event.utf8.is_empty() {
        // legacyAltPrefix — alt_esc_prefix defaults off
        return;
    }

    // fixterms CSI u for ctrl (key_encode.zig legacy csiu block)
    if all_mods.ctrl {
        let mut mods: u16 = 1;
        if event.mods.shift {
            mods |= 1 << 1;
        }
        if event.mods.alt {
            mods |= 1 << 2;
        }
        if event.mods.ctrl {
            mods |= 1 << 3;
        }
        let mut cps = event.utf8.chars();
        if let (Some(mut ch), None) = (cps.next(), cps.next()) {
            if ch.is_ascii_uppercase() && event.mods.shift {
                ch = ch.to_ascii_lowercase();
            }
            if event.unshifted_codepoint != ch as u32 {
                mods &= !(1 << 1); // shift was needed to obtain the char
            }
            out.extend(format!("\x1b[{};{}u", ch as u32, mods).as_bytes());
            return;
        }
    }

    if opts.alt_esc_prefix && binding_mods.alt {
        out.push(0x1b);
        out.extend_from_slice(event.utf8.as_bytes());
        return;
    }

    out.extend_from_slice(event.utf8.as_bytes());
}

// ---------------------------------------------------------------------------
// Mouse encoder — input/mouse_encode.zig
// ---------------------------------------------------------------------------

struct MouseEncEvent {
    action: MouseAction,
    button: Option<MouseButton>,
    mods: Mods,
    pos: (f32, f32),
    cell: (i32, i32),
    screen: (i32, i32),
}

struct MouseEncOptions<'a> {
    event: MouseEventMode,
    format: MouseFormat,
    any_button_pressed: bool,
    last_cell: &'a mut Option<(i32, i32)>,
}

fn mouse_should_report(event: &MouseEncEvent, opts: &MouseEncOptions) -> bool {
    match opts.event {
        MouseEventMode::None => false,
        MouseEventMode::Normal => event.action != MouseAction::Motion,
        MouseEventMode::Button => event.button.is_some(),
        MouseEventMode::Any => true,
    }
}

fn mouse_button_code(event: &MouseEncEvent, opts: &MouseEncOptions) -> Option<u8> {
    // ghostty buttonCode (mouse_encode.zig): null button → 3 (motion
    // without a pressed button); legacy releases also encode 3
    let legacy_release = event.action == MouseAction::Release && opts.format != MouseFormat::Sgr;
    let button_base = match event.button {
        None => 3,
        Some(_) if legacy_release => 3,
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        Some(MouseButton::Four) => 64,
        Some(MouseButton::Five) => 65,
        Some(MouseButton::Six) => 66,
        Some(MouseButton::Seven) => 67,
    };
    let mut acc: u8 = button_base;
    if opts.event != MouseEventMode::None {
        // reported modes carry mods (X10 event mode not in the port's
        // mode set — `mouseButton` never produces its press-only shape)
        if event.mods.shift {
            acc += 4;
        }
        if event.mods.alt {
            acc += 8;
        }
        if event.mods.ctrl {
            acc += 16;
        }
    }
    if event.action == MouseAction::Motion {
        acc += 32;
    }
    Some(acc)
}

fn encode_mouse(out: &mut Vec<u8>, event: &MouseEncEvent, opts: MouseEncOptions) {
    if !mouse_should_report(event, &opts) {
        return;
    }
    if event.action != MouseAction::Release
        && (event.pos.0 < 0.0
            || event.pos.1 < 0.0
            || event.pos.0 > event.screen.0 as f32
            || event.pos.1 > event.screen.1 as f32)
    {
        let sends_motion = matches!(opts.event, MouseEventMode::Button | MouseEventMode::Any);
        if !sends_motion || !opts.any_button_pressed {
            return;
        }
    }
    let (cx, cy) = event.cell;
    if event.action == MouseAction::Motion && opts.format != MouseFormat::SgrPixels {
        if let Some(last) = *opts.last_cell {
            if last == (cx, cy) {
                return;
            }
        }
    }
    *opts.last_cell = Some((cx, cy));
    let Some(button_code) = mouse_button_code(event, &opts) else {
        return;
    };
    match opts.format {
        MouseFormat::X10 => {
            if cx > 222 || cy > 222 {
                return;
            }
            out.extend_from_slice(b"\x1b[M");
            out.push(32 + button_code);
            out.push(32 + cx as u8 + 1);
            out.push(32 + cy as u8 + 1);
        }
        MouseFormat::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            push_codepoint(out, 32 + button_code as u32);
            push_codepoint(out, cx as u32 + 33);
            push_codepoint(out, cy as u32 + 33);
        }
        MouseFormat::Sgr => {
            let final_ = if event.action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            out.extend(format!("\x1b[<{};{};{}{}", button_code, cx + 1, cy + 1, final_).as_bytes());
        }
        MouseFormat::Urxvt => {
            out.extend(
                format!("\x1b[{};{};{}M", 32 + button_code as u32, cx + 1, cy + 1).as_bytes(),
            );
        }
        MouseFormat::SgrPixels => {}
    }
}

fn push_codepoint(out: &mut Vec<u8>, cp: u32) {
    if let Some(c) = char::from_u32(cp) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
}
