//! `InputRenderable` — `packages/core/src/renderables/Input.ts` (the
//! single-line input), over the single-line slice of the
//! `EditBufferRenderable` / `EditBuffer` / `EditorView` stack
//! (`renderables/EditBufferRenderable.ts`,
//! `native/src/edit-buffer.zig`, `native/src/editor-view.zig`,
//! `native/src/buffer.zig`).
//!
//! The full multi-line EditBuffer (rope, multi-cursor, extmarks) is
//! the textarea lane; this module ports the semantics the Input
//! surface observes, with the native word-boundary scan
//! (`utf8.zig` `findWordChunkLayoutInfo`, `edit-buffer.zig`
//! `getNextWordBoundary`/`getPrevWordBoundary`) translated to
//! grapheme-cluster passes.
//!
//! Ported with citations:
//! * constructor defaults + value sanitization (Input.ts:46-97)
//! * the action set (Textarea.ts:17-128) and `handleKeyPress`
//!   (Textarea.ts:265-300) over the keybinding resolution
//!   ([`crate::keybinding`], keybinding.internal.ts)
//! * insert/delete/word/history ops (EditBufferRenderable.ts:680-969)
//! * focus/blur commit + INPUT/CHANGE/ENTER events (Input.ts:99-236)
//! * rendering: placeholder switch (editor-view.zig:969-984), the
//!   prefilled viewport bg + transparent-text fast path
//!   (buffer.zig:1671-1679, 1028-1062, 2005-2065), scroll-to-cursor
//!   (editor-view.zig:208-263).

use crate::buffer::OptimizedBuffer;
use crate::color::{parse_color, Rgba};
use crate::events::{KeyEvent, PasteEvent};
use crate::keybinding::{
    build_key_bindings_map, default_key_aliases, get_key_binding_action, KeyAliasMap, KeyBinding,
    KeyBindingLookup,
};
use crate::paste::{decode_paste_bytes, strip_ansi_sequences};
use std::cell::Cell;
use std::sync::{Arc, Mutex};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

// ---------------------------------------------------------------------------
// Word-boundary scan — native/src/utf8.zig (lines 131-165, 197-286,
// 1597-1900) specialized to a single line.
// ---------------------------------------------------------------------------

/// `WordClass` (utf8.zig:223-227).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    AsciiWord,
    CjkWord,
    Other,
}

/// `LayoutWrapBreakKind` (utf8.zig:108-123) — the kinds a cluster can
/// carry in the word scan. Upstream's `cjk_intercharacter` is a
/// line-break-only opportunity (never a word boundary) and
/// `script_transition` breaks carry no extra behavior in the single-
/// line scan, so both reduce to "word boundary": every emitted break
/// here IS a word boundary, exactly like the word-only layout scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakKind {
    Whitespace,
    PreservedWhitespace,
    Punctuation,
}

impl BreakKind {
    /// All recorded kinds are word-boundary kinds
    /// (`LayoutWrapBreakKind.isWordBoundary`, utf8.zig:118-123).
    fn is_word_boundary(self) -> bool {
        matches!(
            self,
            BreakKind::Whitespace | BreakKind::PreservedWhitespace | BreakKind::Punctuation
        )
    }
}

/// `isAsciiWordByte` (utf8.zig:229-234).
fn is_ascii_word_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
}

/// `isCjkWordCodepoint` (utf8.zig:236-260) — exact range table.
fn is_cjk_word_codepoint(cp: u32) -> bool {
    matches!(cp,
        0x3400..=0x4DBF   // Han ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xF900..=0xFAFF // compat ideographs
        | 0x20000..=0x2A6DF
        | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F
        | 0x2B820..=0x2CEAF
        | 0x2CEB0..=0x2EBEF
        | 0x2EBF0..=0x2EE5D
        | 0x2F800..=0x2FA1F
        // Hiragana + Katakana
        | 0x3040..=0x309F
        | 0x30A0..=0x30FF
        | 0x31F0..=0x31FF
        | 0xFF66..=0xFF9D
        // Hangul
        | 0x1100..=0x11FF
        | 0x3130..=0x318F
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7AF
        | 0xD7B0..=0xD7FF)
}

/// `classifyWordClass` (utf8.zig:262-268).
fn classify_word_class(cp: u32) -> WordClass {
    if cp <= 0x7f {
        return if is_ascii_word_byte(cp as u8) {
            WordClass::AsciiWord
        } else {
            WordClass::Other
        };
    }
    if is_cjk_word_codepoint(cp) {
        return WordClass::CjkWord;
    }
    WordClass::Other
}

/// `asciiLayoutWrapBreakKind` (utf8.zig:159-165).
fn ascii_break_kind(b: u8) -> Option<BreakKind> {
    match b {
        b' ' | b'\t' => Some(BreakKind::Whitespace),
        b'-' | b'/' | b'\\' | b'.' | b',' | b';' | b':' | b'!' | b'?' | b'(' | b')' | b'['
        | b']' | b'{' | b'}' => Some(BreakKind::Punctuation),
        _ => None,
    }
}

/// `unicodeLayoutWrapBreakKind` (utf8.zig:197-218).
fn unicode_break_kind(cp: u32) -> Option<BreakKind> {
    match cp {
        // whitespace
        0x00A0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 | 0x200B => {
            Some(BreakKind::Whitespace)
        }
        // punctuation
        0x00AD | 0x2010 | 0x3001 | 0x3002 | 0xFF01 | 0xFF0C | 0xFF1A | 0xFF1F => {
            Some(BreakKind::Punctuation)
        }
        _ => None,
    }
}

/// One grapheme cluster with its display column.
#[derive(Debug, Clone)]
struct Cluster {
    /// byte offset in the line text
    start: usize,
    /// cluster text (borrowed view reconstructed via offsets)
    len: usize,
    /// display column of the cluster start
    col: u32,
    /// display width in columns (tabs expand to the next 2-col stop,
    /// text-buffer default `tab_width = 2`)
    width: u32,
    class: WordClass,
    kind: BreakKindOption,
}

/// The break kind a cluster contributes (`Option<BreakKind>`: `None`
/// is `LayoutWrapBreakKind.none`).
type BreakKindOption = Option<BreakKind>;

/// A word-boundary break: the window identifies the cluster that
/// CREATES the break; `col_end` crosses it (utf8.zig:126-145). Only
/// word-boundary kinds are recorded (the `cjk_intercharacter`
/// line-break-only kind is never emitted in word mode, so the kind
/// needs no runtime check downstream).
#[derive(Debug, Clone, Copy)]
struct WrapBreak {
    col_start: u32,
    width: u32,
}

impl WrapBreak {
    fn col_end(&self) -> u32 {
        self.col_start + self.width
    }
}

/// The analyzed single line (the word-mode layout scan,
/// utf8.zig:1597-1900 specialized: ASCII per delimiter byte,
/// non-ASCII per cluster, script transitions between adjacent
/// clusters — the intra-chunk part of the scan; chunk-edge
/// transitions do not occur on a single line).
#[derive(Debug, Clone, Default)]
pub struct LineLayout {
    clusters: Vec<Cluster>,
    /// total display width
    width: u32,
    breaks: Vec<WrapBreak>,
}

fn tab_width_cols(col: u32) -> u32 {
    // tab stops every 2 columns (TextBuffer default tab_width = 2)
    2 - (col % 2)
}

fn grapheme_cols(g: &str, col: u32) -> u32 {
    if g == "\t" {
        return tab_width_cols(col);
    }
    let mut w = 0u32;
    for c in g.chars() {
        w += c.width().unwrap_or(0) as u32;
    }
    w
}

fn analyze_line(text: &str) -> LineLayout {
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut breaks = Vec::new();
    let mut col = 0u32;

    for g in text.graphemes(true) {
        let width = grapheme_cols(g, col);
        let first_cp = g.chars().next().map_or(0, |c| c as u32);
        let class = classify_word_class(first_cp);
        // cluster break kind: first char's kind, with the
        // all-whitespace elision rule (utf8.zig:1874-1887) — a cluster
        // mixing whitespace and non-whitespace is preserved_whitespace
        let mut kind: BreakKindOption = if first_cp <= 0x7f {
            ascii_break_kind(first_cp as u8)
        } else {
            unicode_break_kind(first_cp)
        };
        if kind == Some(BreakKind::Whitespace) {
            for c in g.chars().skip(1) {
                let next_kind = if (c as u32) <= 0x7f {
                    ascii_break_kind(c as u8)
                } else {
                    unicode_break_kind(c as u32)
                };
                if next_kind != Some(BreakKind::Whitespace) {
                    kind = Some(BreakKind::PreservedWhitespace);
                    break;
                }
            }
        }

        // Script transition against the PREVIOUS cluster commits a
        // break on the previous cluster's window (utf8.zig:1574-1595).
        if let Some(prev) = clusters.last() {
            let transition = (prev.class == WordClass::CjkWord && class == WordClass::AsciiWord)
                || (prev.class == WordClass::AsciiWord && class == WordClass::CjkWord);
            if transition {
                breaks.push(WrapBreak {
                    col_start: prev.col,
                    width: prev.width,
                });
            }
        }

        let start = g.as_ptr() as usize - text.as_ptr() as usize;
        clusters.push(Cluster {
            start,
            len: g.len(),
            col,
            width,
            class,
            kind,
        });
        col += width;
    }

    // Cluster commits: each whitespace/punctuation cluster emits its
    // break when the NEXT cluster starts (or at end-of-line for the
    // trailing one) — utf8.zig:1883-1895. For ASCII this is
    // per-delimiter-byte (each delimiter is its own cluster).
    for (i, c) in clusters.iter().enumerate() {
        if let Some(kind) = c.kind {
            if kind.is_word_boundary() {
                breaks.push(WrapBreak {
                    col_start: c.col,
                    width: c.width,
                });
            }
        }
        let _ = i;
    }
    breaks.sort_by_key(|b| b.col_start);

    LineLayout {
        clusters,
        width: col,
        breaks,
    }
}

impl LineLayout {
    /// Cluster index for a display column (the cluster that OCCUPIES
    /// the column), or `clusters.len()` when past the end.
    fn cluster_at_col(&self, col: u32) -> usize {
        for (i, c) in self.clusters.iter().enumerate() {
            if col >= c.col && col < c.col + c.width {
                return i;
            }
        }
        self.clusters.len()
    }

    /// Cluster index of the boundary AT a display column (a cursor
    /// position), or None when the column is inside a cluster.
    fn boundary_at_col(&self, col: u32) -> Option<usize> {
        if col == 0 {
            return Some(0);
        }
        self.clusters
            .iter()
            .position(|c| c.col == col)
            .or(if col >= self.width {
                Some(self.clusters.len())
            } else {
                None
            })
    }

    /// The codepoint starting at a break's window (for the
    /// boundary-at-cursor word-codepoint check, edit-buffer.zig:861-873).
    fn break_starts_on_word(&self, b: &WrapBreak) -> bool {
        self.clusters
            .iter()
            .find(|c| c.col == b.col_start)
            .is_some_and(|c| c.class != WordClass::Other)
    }
}

/// `getNextWordBoundary` (edit-buffer.zig:804-891), single line: the
/// chunk loop degenerates (`passed_cursor` stays false for the one
/// chunk, so the `break_col > local_cursor_col` and the
/// boundary-at-cursor-word-start rules apply), the next-line branch
/// is unreachable, and the tail clamps to the line width.
fn next_word_boundary(layout: &LineLayout, cursor_col: u32) -> u32 {
    let line_width = layout.width;
    for b in &layout.breaks {
        if b.col_start > cursor_col {
            if b.col_end() > cursor_col && b.col_end() <= line_width {
                return b.col_end();
            }
        } else if b.col_start == cursor_col && layout.break_starts_on_word(b) {
            // A boundary at the cursor can still be the next word step
            // for script transitions like "a日" (edit-buffer.zig:858-873).
            if b.col_end() > cursor_col && b.col_end() <= line_width {
                return b.col_end();
            }
        }
    }
    line_width
}

/// `getPrevWordBoundary` (edit-buffer.zig:893-946), single line: the
/// last break whose end is strictly before the cursor, else 0.
fn prev_word_boundary(layout: &LineLayout, cursor_col: u32) -> u32 {
    let mut last_boundary: Option<u32> = None;
    for b in &layout.breaks {
        if b.col_end() < cursor_col {
            last_boundary = Some(b.col_end());
        }
    }
    last_boundary.unwrap_or(0)
}

// ---------------------------------------------------------------------------
// EditLine — the single-line edit model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    /// cursor as a byte offset (remapped to a cluster boundary on restore)
    cursor_byte: usize,
}

/// The single-line edit model: text + cursor (cluster boundary) +
/// selection + undo/redo history. Mutating ops push a snapshot first
/// (`autoStoreUndo`, edit-buffer.zig:379); `set_text` resets history
/// (edit-buffer.ts:80-95 "completely reset the buffer state").
#[derive(Debug, Clone, Default)]
struct EditLine {
    text: String,
    layout: LineLayout,
    /// cursor as a cluster boundary index (0..=clusters.len())
    cursor: usize,
    /// selection anchor/focus as cluster boundary indices
    selection: Option<(usize, usize)>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl EditLine {
    fn new() -> Self {
        Self::default()
    }

    fn reanalyze(&mut self) {
        let cursor_byte = self.cursor_byte();
        self.layout = analyze_line(&self.text);
        self.cursor = self.boundary_for_byte(cursor_byte);
    }

    /// Cluster-boundary index for a byte offset (snapped forward).
    fn boundary_for_byte(&self, byte: usize) -> usize {
        for (i, c) in self.layout.clusters.iter().enumerate() {
            if c.start >= byte {
                return i;
            }
        }
        self.layout.clusters.len()
    }

    /// The byte offset of the cursor boundary.
    fn cursor_byte(&self) -> usize {
        if self.cursor >= self.layout.clusters.len() {
            return self.text.len();
        }
        self.layout.clusters[self.cursor].start
    }

    /// Display column of the cursor.
    fn cursor_col(&self) -> u32 {
        if self.cursor >= self.layout.clusters.len() {
            return self.layout.width;
        }
        self.layout.clusters[self.cursor].col
    }

    fn set_cursor_col(&mut self, col: u32) {
        self.cursor = self
            .layout
            .boundary_at_col(col)
            .unwrap_or(self.layout.clusters.len());
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor_byte: self.cursor_byte(),
        }
    }

    fn store_undo(&mut self) {
        self.undo.push(self.snapshot());
        self.redo.clear();
    }

    /// `setText` (edit-buffer.ts:84-95): full reset, history cleared.
    fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
        self.undo.clear();
        self.redo.clear();
        self.selection = None;
        self.reanalyze();
        self.cursor = self.layout.clusters.len();
    }

    fn selection_range(&self) -> Option<(usize, usize)> {
        // sorted cluster indices
        self.selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    fn has_selection(&self) -> bool {
        self.selection.is_some_and(|(a, b)| a != b)
    }

    fn byte_range(&self, a: usize, b: usize) -> (usize, usize) {
        let start = if a >= self.layout.clusters.len() {
            self.text.len()
        } else {
            self.layout.clusters[a].start
        };
        let end = if b >= self.layout.clusters.len() {
            self.text.len()
        } else {
            self.layout.clusters[b].start
        };
        (start.min(end), start.max(end))
    }

    fn delete_selection(&mut self) {
        if let Some((a, b)) = self.selection_range() {
            if a != b {
                self.store_undo();
                let (start, end) = self.byte_range(a, b);
                self.text.replace_range(start..end, "");
                self.cursor = a;
                self.reanalyze();
                self.cursor = a.min(self.layout.clusters.len());
            }
            self.selection = None;
        }
    }

    /// `insertText` (edit-buffer.zig:375-…; EditBufferRenderable.ts:680):
    /// selection replaced, cursor lands after the inserted text.
    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.has_selection() {
            self.delete_selection();
        }
        self.store_undo();
        let at = self.cursor_byte();
        self.text.insert_str(at, text);
        self.reanalyze();
        // cursor after the inserted text
        self.cursor = self.boundary_for_byte(at + text.len());
    }

    /// `deleteChar` — delete the cluster at the cursor.
    fn delete_char(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if self.cursor < self.layout.clusters.len() {
            self.store_undo();
            let (start, end) = self.byte_range(self.cursor, self.cursor + 1);
            self.text.replace_range(start..end, "");
            self.reanalyze();
            self.cursor = self.cursor.min(self.layout.clusters.len());
        }
    }

    /// `deleteCharBackward` — delete the cluster before the cursor.
    fn delete_char_backward(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if self.cursor > 0 {
            self.store_undo();
            let (start, end) = self.byte_range(self.cursor - 1, self.cursor);
            self.text.replace_range(start..end, "");
            self.cursor -= 1;
            self.reanalyze();
            self.cursor = self.cursor.min(self.layout.clusters.len());
        }
    }

    /// `deleteRange` between two cluster boundaries.
    fn delete_range(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        self.store_undo();
        let (start, end) = self.byte_range(a, b);
        self.text.replace_range(start..end, "");
        self.reanalyze();
        self.cursor = a.min(self.layout.clusters.len());
    }

    /// `deleteLine` on the only line (edit-buffer.zig:677-684): clear it.
    fn delete_line(&mut self) {
        if !self.text.is_empty() {
            self.store_undo();
            let len = self.text.len();
            self.text.replace_range(0..len, "");
            self.reanalyze();
            self.cursor = 0;
        }
    }

    /// `moveCursorLeft` (edit-buffer.zig — previous grapheme boundary).
    fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_cursor_right(&mut self) {
        if self.cursor < self.layout.clusters.len() {
            self.cursor += 1;
        }
    }

    /// `undo` (edit-buffer.ts:322-328): restore text + cursor.
    fn undo(&mut self) -> bool {
        if let Some(snap) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.apply_snapshot(snap);
            true
        } else {
            false
        }
    }

    fn redo(&mut self) -> bool {
        if let Some(snap) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.apply_snapshot(snap);
            true
        } else {
            false
        }
    }

    fn apply_snapshot(&mut self, snap: Snapshot) {
        self.text = snap.text;
        self.selection = None;
        self.reanalyze();
        // cursor at the snapshot's byte offset, snapped to a boundary
        self.cursor = self.boundary_for_byte(snap.cursor_byte);
    }
}

// ---------------------------------------------------------------------------
// InputRenderable
// ---------------------------------------------------------------------------

/// `TextareaAction` (Textarea.ts:17-54) — `InputAction`
/// (Input.ts:11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputAction {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    LineHome,
    LineEnd,
    SelectLineHome,
    SelectLineEnd,
    VisualLineHome,
    VisualLineEnd,
    SelectVisualLineHome,
    SelectVisualLineEnd,
    BufferHome,
    BufferEnd,
    SelectBufferHome,
    SelectBufferEnd,
    DeleteLine,
    DeleteToLineEnd,
    DeleteToLineStart,
    Backspace,
    Delete,
    Newline,
    Undo,
    Redo,
    WordForward,
    WordBackward,
    SelectWordForward,
    SelectWordBackward,
    DeleteWordForward,
    DeleteWordBackward,
    SelectAll,
    Submit,
}

impl InputAction {
    /// The action id string (the map values of keybinding maps).
    pub fn as_str(self) -> &'static str {
        use InputAction::*;
        match self {
            MoveLeft => "move-left",
            MoveRight => "move-right",
            MoveUp => "move-up",
            MoveDown => "move-down",
            SelectLeft => "select-left",
            SelectRight => "select-right",
            SelectUp => "select-up",
            SelectDown => "select-down",
            LineHome => "line-home",
            LineEnd => "line-end",
            SelectLineHome => "select-line-home",
            SelectLineEnd => "select-line-end",
            VisualLineHome => "visual-line-home",
            VisualLineEnd => "visual-line-end",
            SelectVisualLineHome => "select-visual-line-home",
            SelectVisualLineEnd => "select-visual-line-end",
            BufferHome => "buffer-home",
            BufferEnd => "buffer-end",
            SelectBufferHome => "select-buffer-home",
            SelectBufferEnd => "select-buffer-end",
            DeleteLine => "delete-line",
            DeleteToLineEnd => "delete-to-line-end",
            DeleteToLineStart => "delete-to-line-start",
            Backspace => "backspace",
            Delete => "delete",
            Newline => "newline",
            Undo => "undo",
            Redo => "redo",
            WordForward => "word-forward",
            WordBackward => "word-backward",
            SelectWordForward => "select-word-forward",
            SelectWordBackward => "select-word-backward",
            DeleteWordForward => "delete-word-forward",
            DeleteWordBackward => "delete-word-backward",
            SelectAll => "select-all",
            Submit => "submit",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        use InputAction::*;
        Some(match s {
            "move-left" => MoveLeft,
            "move-right" => MoveRight,
            "move-up" => MoveUp,
            "move-down" => MoveDown,
            "select-left" => SelectLeft,
            "select-right" => SelectRight,
            "select-up" => SelectUp,
            "select-down" => SelectDown,
            "line-home" => LineHome,
            "line-end" => LineEnd,
            "select-line-home" => SelectLineHome,
            "select-line-end" => SelectLineEnd,
            "visual-line-home" => VisualLineHome,
            "visual-line-end" => VisualLineEnd,
            "select-visual-line-home" => SelectVisualLineHome,
            "select-visual-line-end" => SelectVisualLineEnd,
            "buffer-home" => BufferHome,
            "buffer-end" => BufferEnd,
            "select-buffer-home" => SelectBufferHome,
            "select-buffer-end" => SelectBufferEnd,
            "delete-line" => DeleteLine,
            "delete-to-line-end" => DeleteToLineEnd,
            "delete-to-line-start" => DeleteToLineStart,
            "backspace" => Backspace,
            "delete" => Delete,
            "newline" => Newline,
            "undo" => Undo,
            "redo" => Redo,
            "word-forward" => WordForward,
            "word-backward" => WordBackward,
            "select-word-forward" => SelectWordForward,
            "select-word-backward" => SelectWordBackward,
            "delete-word-forward" => DeleteWordForward,
            "delete-word-backward" => DeleteWordBackward,
            "select-all" => SelectAll,
            "submit" => Submit,
            _ => return None,
        })
    }
}

/// `defaultTextareaKeyBindings` (Textarea.ts:58-128) — order-exact.
/// `opentui-keymap::textarea` carries the same table for the keymap
/// side (ported from the `@opentui/keymap` addon that imports it);
/// the two ports cite the same upstream lines.
pub fn default_textarea_key_bindings() -> Vec<KeyBinding> {
    use InputAction::*;
    let b = |name: &str, ctrl: bool, shift: bool, meta: bool, sup: bool, action: InputAction| {
        KeyBinding::new(name, ctrl, shift, meta, sup, action.as_str())
    };
    vec![
        b("left", false, false, false, false, MoveLeft),
        b("right", false, false, false, false, MoveRight),
        b("up", false, false, false, false, MoveUp),
        b("down", false, false, false, false, MoveDown),
        b("left", false, true, false, false, SelectLeft),
        b("right", false, true, false, false, SelectRight),
        b("up", false, true, false, false, SelectUp),
        b("down", false, true, false, false, SelectDown),
        b("home", false, false, false, false, BufferHome),
        b("end", false, false, false, false, BufferEnd),
        b("home", false, true, false, false, SelectBufferHome),
        b("end", false, true, false, false, SelectBufferEnd),
        b("a", true, false, false, false, LineHome),
        b("e", true, false, false, false, LineEnd),
        b("a", true, true, false, false, SelectLineHome),
        b("e", true, true, false, false, SelectLineEnd),
        b("a", false, false, true, false, VisualLineHome),
        b("e", false, false, true, false, VisualLineEnd),
        b("a", false, true, true, false, SelectVisualLineHome),
        b("e", false, true, true, false, SelectVisualLineEnd),
        b("f", true, false, false, false, MoveRight),
        b("b", true, false, false, false, MoveLeft),
        b("w", true, false, false, false, DeleteWordBackward),
        b("backspace", true, false, false, false, DeleteWordBackward),
        b("d", false, false, true, false, DeleteWordForward),
        b("delete", false, false, true, false, DeleteWordForward),
        b("delete", true, false, false, false, DeleteWordForward),
        b("d", true, true, false, false, DeleteLine),
        b("k", true, false, false, false, DeleteToLineEnd),
        b("u", true, false, false, false, DeleteToLineStart),
        b("backspace", false, false, false, false, Backspace),
        b("backspace", false, true, false, false, Backspace),
        b("d", true, false, false, false, Delete),
        b("delete", false, false, false, false, Delete),
        b("delete", false, true, false, false, Delete),
        b("return", false, false, false, false, Newline),
        b("kpenter", false, false, false, false, Newline),
        b("linefeed", false, false, false, false, Newline),
        b("return", false, false, true, false, Submit),
        b("kpenter", false, false, true, false, Submit),
        // undo/redo
        b("-", true, false, false, false, Undo),
        b(".", true, false, false, false, Redo),
        b("z", false, false, false, true, Undo),
        b("z", false, true, false, true, Redo),
        b("f", false, false, true, false, WordForward),
        b("b", false, false, true, false, WordBackward),
        b("right", false, false, true, false, WordForward),
        b("left", false, false, true, false, WordBackward),
        b("right", true, false, false, false, WordForward),
        b("left", true, false, false, false, WordBackward),
        b("f", false, true, true, false, SelectWordForward),
        b("b", false, true, true, false, SelectWordBackward),
        b("right", false, true, true, false, SelectWordForward),
        b("left", false, true, true, false, SelectWordBackward),
        b("backspace", false, false, true, false, DeleteWordBackward),
        // super (cmd/win) + arrow keys for Kitty Keyboard mode
        b("left", false, false, false, true, VisualLineHome),
        b("right", false, false, false, true, VisualLineEnd),
        b("up", false, false, false, true, BufferHome),
        b("down", false, false, false, true, BufferEnd),
        b("left", false, true, false, true, SelectVisualLineHome),
        b("right", false, true, false, true, SelectVisualLineEnd),
        b("up", false, true, false, true, SelectBufferHome),
        b("down", false, true, false, true, SelectBufferEnd),
        b("a", false, false, false, true, SelectAll),
    ]
}

/// `InputRenderableEvents` (Input.ts:29-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputStateEvent {
    Input,
    Change,
    Enter,
}

type InputEventListener = Box<dyn FnMut(&str) + Send>;

/// `InputRenderableOptions` (Input.ts:14-26) — the subset the port
/// models (colors as parsed [`Rgba`] inputs).
#[derive(Debug, Clone, Default)]
pub struct InputOptions {
    pub id: String,
    pub value: String,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub placeholder: Option<String>,
    pub text_color: Option<Rgba>,
    pub background_color: Option<Rgba>,
    pub focused_text_color: Option<Rgba>,
    pub focused_background_color: Option<Rgba>,
    pub placeholder_color: Option<Rgba>,
    /// explicit width (the harness vocabulary); None → measured
    pub width: Option<u32>,
    /// extra bindings merged over the defaults (options.keyBindings)
    pub key_bindings: Vec<KeyBinding>,
    pub key_alias_map: KeyAliasMap,
}

/// `InputRenderable`'s drawn + interactive state.
pub struct InputRenderableState {
    pub id: String,
    edit: EditLine,
    /// `InputRenderable.defaultOptions` (Input.ts:52-59)
    max_length: usize,
    min_length: usize,
    placeholder: String,
    last_committed_value: String,
    /// Textarea colors (Textarea.ts:159-187)
    unfocused_text_color: Rgba,
    unfocused_background_color: Rgba,
    focused_text_color: Rgba,
    focused_background_color: Rgba,
    placeholder_color: Rgba,
    focused: bool,
    /// `traits.suspend` — a keymap owning the keys suspends the
    /// internal bindings (Textarea.ts:266; keymap's
    /// `registerTextareaMappingSuspension`).
    pub suspend_mappings: bool,
    bindings_map: std::collections::HashMap<String, String>,
    key_bindings: Vec<KeyBinding>,
    key_aliases: KeyAliasMap,
    /// viewport x offset + width (EditorView viewport; wrap-none
    /// single line only needs the column window). The offset is
    /// interior-mutable so the paint walk stays `&self` (the renderable
    /// tree paints through shared references; upstream mutates the
    /// EditorView inside render).
    viewport_x: Cell<u32>,
    viewport_width: u32,
    /// `scrollMargin` default (EditBufferRenderable.ts:97,125)
    scroll_margin: f64,
    listeners: Arc<Mutex<Vec<(InputStateEvent, InputEventListener)>>>,
}

impl Default for InputRenderableState {
    fn default() -> Self {
        InputRenderableState::new(&InputOptions {
            id: "input".into(),
            ..Default::default()
        })
    }
}

impl InputRenderableState {
    /// The constructor port (Input.ts:61-97 + Textarea.ts:168-203).
    pub fn new(options: &InputOptions) -> Self {
        let max_length = options.max_length.unwrap_or(1000);
        let min_length = options.min_length.unwrap_or(0);
        let raw_value = options.value.clone();
        let initial_value = sanitize_single_line(&raw_value, max_length);

        // Input.ts:69-71 (throws → panic documented as the port's
        // constructor-error contract)
        assert!(
            min_length <= max_length,
            "InputRenderable: minLength ({min_length}) cannot be greater than maxLength ({max_length})"
        );

        let mut edit = EditLine::new();
        edit.set_text(&initial_value);

        // Textarea.ts:159-166 defaults
        let unfocused_text_color = options.text_color.unwrap_or_else(|| parse_color("#FFFFFF"));
        let unfocused_background_color = options
            .background_color
            .unwrap_or_else(|| parse_color("transparent"));
        let focused_text_color = options
            .focused_text_color
            .or(options.text_color)
            .unwrap_or_else(|| parse_color("#FFFFFF"));
        let focused_background_color = options
            .focused_background_color
            .or(options.background_color)
            .unwrap_or_else(|| parse_color("transparent"));
        let placeholder_color = options
            .placeholder_color
            .unwrap_or_else(|| parse_color("#666666"));

        // Input.ts:81-86: return/kpenter/linefeed submit instead of newline
        let mut bindings = vec![
            KeyBinding::new("return", false, false, false, false, "submit"),
            KeyBinding::new("kpenter", false, false, false, false, "submit"),
            KeyBinding::new("linefeed", false, false, false, false, "submit"),
        ];
        bindings.extend(options.key_bindings.iter().cloned());

        let mut state = InputRenderableState {
            id: options.id.clone(),
            edit,
            max_length,
            min_length,
            placeholder: options.placeholder.clone().unwrap_or_default(),
            last_committed_value: initial_value.clone(),
            unfocused_text_color,
            unfocused_background_color,
            focused_text_color,
            focused_background_color,
            placeholder_color,
            focused: false,
            suspend_mappings: false,
            bindings_map: build_key_bindings_map(
                &merge_bindings(bindings.clone()),
                Some(&merge_aliases(&options.key_alias_map)),
            ),
            key_bindings: bindings,
            key_aliases: merge_aliases(&options.key_alias_map),
            viewport_x: Cell::new(0),
            viewport_width: options.width.unwrap_or(80),
            scroll_margin: 0.2,
            listeners: Arc::new(Mutex::new(Vec::new())),
        };
        state.edit.set_text(&initial_value);
        // Set cursor to end of initial value (Input.ts:93-96)
        state.edit.cursor = state.edit.layout.clusters.len();
        state
    }

    /// `on(InputRenderableEvents.X, handler)`.
    pub fn on<F>(&self, event: InputStateEvent, handler: F)
    where
        F: FnMut(&str) + Send + 'static,
    {
        self.listeners
            .lock()
            .expect("input listeners")
            .push((event, Box::new(handler)));
    }

    fn emit(&self, event: InputStateEvent, value: &str) {
        let mut listeners = self.listeners.lock().expect("input listeners");
        for (kind, handler) in listeners.iter_mut() {
            if *kind == event {
                handler(value);
            }
        }
    }

    /// `value` getter (Input.ts:132-134).
    #[must_use]
    pub fn value(&self) -> &str {
        &self.edit.text
    }

    /// `value` setter (Input.ts:136-144).
    pub fn set_value(&mut self, value: &str) {
        let new_value = sanitize_single_line(value, self.max_length);
        if self.edit.text != new_value {
            let cursor_end = new_value.len();
            self.edit.set_text(&new_value);
            self.edit.cursor = self.edit.layout.clusters.len();
            let _ = cursor_end;
            self.emit(InputStateEvent::Input, &new_value);
        }
    }

    /// `cursorOffset` (EditBufferRenderable.ts:238-246) — byte offset.
    #[must_use]
    pub fn cursor_offset(&self) -> usize {
        self.edit.cursor_byte()
    }

    pub fn set_cursor_offset(&mut self, offset: usize) {
        self.edit.selection = None;
        let byte = offset.min(self.edit.text.len());
        self.edit.cursor = self.edit.boundary_for_byte(byte);
    }

    /// The cursor's display column (the paint position).
    #[must_use]
    pub fn cursor_col(&self) -> u32 {
        self.edit.cursor_col()
    }

    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn set_placeholder(&mut self, placeholder: &str) {
        self.placeholder = placeholder.to_owned();
    }

    #[must_use]
    pub fn min_length(&self) -> usize {
        self.min_length
    }

    #[must_use]
    pub fn max_length(&self) -> usize {
        self.max_length
    }

    /// `maxLength` setter (Input.ts:238-244).
    pub fn set_max_length(&mut self, max_length: usize) {
        self.max_length = max_length;
        if self.edit.text.chars().count() > max_length {
            let truncated: String = self.edit.text.chars().take(max_length).collect();
            self.edit.set_text(&truncated);
        }
    }

    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// `focus()` (Input.ts:146-149 + EditBufferRenderable.ts:1024-1028).
    pub fn focus(&mut self) {
        self.focused = true;
        self.last_committed_value = self.edit.text.clone();
    }

    /// `blur()` (Input.ts:151-160): CHANGE fires when the value
    /// drifted from the focus-time snapshot.
    pub fn blur(&mut self) {
        if self.focused {
            let current = self.edit.text.clone();
            if current != self.last_committed_value {
                self.last_committed_value = current.clone();
                self.emit(InputStateEvent::Change, &current);
            }
            self.focused = false;
        }
    }

    /// `submit()` (Input.ts:162-174).
    pub fn submit(&mut self) -> bool {
        let current = self.edit.text.clone();
        if current.len() < self.min_length {
            return false;
        }
        if current != self.last_committed_value {
            self.last_committed_value = current.clone();
            self.emit(InputStateEvent::Change, &current);
        }
        self.emit(InputStateEvent::Enter, &current);
        true
    }

    /// `handlePaste` (Input.ts:109-114).
    pub fn handle_paste(&mut self, event: &mut PasteEvent) {
        let sanitized = sanitize_single_line(
            &strip_ansi_sequences(&decode_paste_bytes(&event.bytes)),
            usize::MAX,
        );
        if !sanitized.is_empty() {
            self.insert_text(&sanitized);
        }
    }

    /// `insertText` (Input.ts:119-130): newlines stripped, maxLength
    /// clamped, INPUT emitted.
    pub fn insert_text(&mut self, text: &str) {
        let sanitized = sanitize_single_line(text, usize::MAX);
        if sanitized.is_empty() {
            return;
        }
        let current_length = self.edit.text.chars().count();
        let remaining = self.max_length.saturating_sub(current_length);
        if remaining == 0 {
            return;
        }
        let to_insert: String = sanitized.chars().take(remaining).collect();
        self.edit.insert_text(&to_insert);
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
    }

    /// The delete family — every override emits INPUT (Input.ts:176-228).
    pub fn delete_char_backward(&mut self) -> bool {
        self.edit.delete_char_backward();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn delete_char(&mut self) -> bool {
        self.edit.delete_char();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn delete_line(&mut self) -> bool {
        self.edit.delete_line();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn delete_word_backward(&mut self) -> bool {
        self.delete_word_backward_impl();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn delete_word_forward(&mut self) -> bool {
        self.delete_word_forward_impl();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn delete_to_line_start(&mut self) -> bool {
        let cursor = self.edit.cursor;
        self.edit.delete_range(0, cursor);
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn delete_to_line_end(&mut self) -> bool {
        let cursor = self.edit.cursor;
        let end = self.edit.layout.clusters.len();
        self.edit.delete_range(cursor, end);
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn undo(&mut self) -> bool {
        self.edit.undo();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.edit.redo();
        let value = self.edit.text.clone();
        self.emit(InputStateEvent::Input, &value);
        true
    }

    fn delete_word_backward_impl(&mut self) {
        if self.edit.has_selection() {
            self.edit.delete_selection();
            return;
        }
        let current_col = self.edit.cursor_col();
        let prev = prev_word_boundary(&self.edit.layout, current_col);
        if prev < current_col {
            let target = self.edit.layout.cluster_at_col(prev).min(self.edit.cursor);
            let cursor = self.edit.cursor;
            self.edit.delete_range(target, cursor);
        }
    }

    fn delete_word_forward_impl(&mut self) {
        if self.edit.has_selection() {
            self.edit.delete_selection();
            return;
        }
        let current_col = self.edit.cursor_col();
        let next = next_word_boundary(&self.edit.layout, current_col);
        if next > current_col {
            let target = self.edit.layout.cluster_at_col(next).max(self.edit.cursor);
            let cursor = self.edit.cursor;
            self.edit.delete_range(cursor, target);
        }
    }

    /// Cursor movement — `moveCursorLeft/Right`
    /// (EditBufferRenderable.ts:736-756) with selection collapse
    /// (lines 727-734) and shift-select.
    pub fn move_cursor_left(&mut self, select: bool) -> bool {
        if !select && self.collapse_selection_to_edge(false) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        self.edit.move_cursor_left();
        self.update_selection_for_movement(select, false);
        true
    }

    pub fn move_cursor_right(&mut self, select: bool) -> bool {
        if !select && self.collapse_selection_to_edge(true) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        self.edit.move_cursor_right();
        self.update_selection_for_movement(select, false);
        true
    }

    pub fn move_word_forward(&mut self, select: bool) -> bool {
        if !select && self.collapse_selection_to_edge(true) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        let col = next_word_boundary(&self.edit.layout, self.edit.cursor_col());
        self.edit.set_cursor_col(col);
        self.update_selection_for_movement(select, false);
        true
    }

    pub fn move_word_backward(&mut self, select: bool) -> bool {
        if !select && self.collapse_selection_to_edge(false) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        let col = prev_word_boundary(&self.edit.layout, self.edit.cursor_col());
        self.edit.set_cursor_col(col);
        self.update_selection_for_movement(select, false);
        true
    }

    pub fn goto_home(&mut self, select: bool) -> bool {
        if !select && self.collapse_selection_to_edge(false) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        self.edit.cursor = 0;
        self.update_selection_for_movement(select, false);
        true
    }

    pub fn goto_end(&mut self, select: bool) -> bool {
        if !select && self.collapse_selection_to_edge(true) {
            return true;
        }
        self.update_selection_for_movement(select, true);
        self.edit.cursor = self.edit.layout.clusters.len();
        self.update_selection_for_movement(select, false);
        true
    }

    /// `selectAll` (EditBufferRenderable.ts:868-872).
    pub fn select_all(&mut self) -> bool {
        self.edit.selection = Some((0, self.edit.layout.clusters.len()));
        true
    }

    /// `collapseSelectionToEdge` (EditBufferRenderable.ts:727-734).
    fn collapse_selection_to_edge(&mut self, end: bool) -> bool {
        let Some((a, b)) = self.edit.selection_range() else {
            return false;
        };
        if a == b {
            self.edit.selection = None;
            return false;
        }
        self.edit.cursor = if end { b } else { a };
        self.edit.selection = None;
        true
    }

    /// `updateSelectionForMovement` (EditBufferRenderable.ts:1163-1191)
    /// — the keyboard-selection core (the renderer-level selection
    /// plumbing is the textarea lane).
    fn update_selection_for_movement(&mut self, select: bool, before: bool) {
        if !select {
            self.edit.selection = None;
            return;
        }
        if before {
            // start (or keep) the selection: anchor = the existing
            // anchor if a selection is active, else the cursor
            let anchor = self.edit.selection.map_or(self.edit.cursor, |(a, _)| a);
            self.edit.selection = Some((anchor, self.edit.cursor));
        } else if let Some((anchor, _)) = self.edit.selection {
            // after the move: extend the focus to the new cursor
            self.edit.selection = Some((anchor, self.edit.cursor));
        }
    }

    /// The resolved action for a key (Textarea.ts:266-274).
    #[must_use]
    pub fn action_for_key(&self, key: &KeyEvent) -> Option<InputAction> {
        if self.suspend_mappings {
            return None;
        }
        let lookup = KeyBindingLookup {
            binding: crate::keybinding::KeyBindingLike {
                name: key.name().to_owned(),
                ctrl: key.ctrl(),
                shift: key.shift(),
                meta: key.meta(),
                super_key: key.super_key(),
            },
            base_code: key.base_code(),
        };
        get_key_binding_action(&self.bindings_map, &lookup).and_then(|s| InputAction::parse(&s))
    }

    /// `handleKeyPress` (Textarea.ts:265-300).
    pub fn handle_key(&mut self, key: &mut KeyEvent) -> bool {
        if !self.suspend_mappings {
            if let Some(action) = self.action_for_key(key) {
                return self.run_action(action);
            }
        }

        if !key.ctrl() && !key.meta() && !key.super_key() && !key.hyper() {
            if key.name() == "space" {
                self.insert_text(" ");
                return true;
            }
            let sequence = key.sequence();
            if !sequence.is_empty() {
                let first = sequence.chars().next().unwrap_or('\0');
                let first_char_code = first as u32;
                if first_char_code < 32 {
                    return false;
                }
                if first_char_code == 127 {
                    return false;
                }
                self.insert_text(sequence);
                return true;
            }
        }
        false
    }

    /// The action handlers (Textarea.ts:220-259) with Input's
    /// single-line semantics.
    pub fn run_action(&mut self, action: InputAction) -> bool {
        use InputAction::*;
        match action {
            MoveLeft | SelectLeft => self.move_cursor_left(action == SelectLeft),
            MoveRight | SelectRight => self.move_cursor_right(action == SelectRight),
            MoveUp | MoveDown | SelectUp | SelectDown => {
                // single visual line (wrap none) — no vertical move
                true
            }
            LineHome | VisualLineHome | BufferHome => self.goto_home(false),
            LineEnd | VisualLineEnd | BufferEnd => self.goto_end(false),
            SelectLineHome | SelectVisualLineHome | SelectBufferHome => self.goto_home(true),
            SelectLineEnd | SelectVisualLineEnd | SelectBufferEnd => self.goto_end(true),
            DeleteLine => self.delete_line(),
            DeleteToLineEnd => self.delete_to_line_end(),
            DeleteToLineStart => self.delete_to_line_start(),
            Backspace => self.delete_char_backward(),
            Delete => self.delete_char(),
            Newline => {
                // Input overrides newline → submit (Input.ts:102-104 +
                // the keyBindings override)
                self.submit()
            }
            Undo => self.undo(),
            Redo => self.redo(),
            WordForward => self.move_word_forward(false),
            WordBackward => self.move_word_backward(false),
            SelectWordForward => self.move_word_forward(true),
            SelectWordBackward => self.move_word_backward(true),
            DeleteWordForward => self.delete_word_forward(),
            DeleteWordBackward => self.delete_word_backward(),
            SelectAll => self.select_all(),
            Submit => self.submit(),
        }
    }

    /// `set keyBindings` (Textarea.ts:410-414).
    pub fn set_key_bindings(&mut self, bindings: Vec<KeyBinding>) {
        self.key_bindings = bindings;
        self.rebuild_bindings_map();
    }

    /// `set keyAliasMap` (Textarea.ts:416-420).
    pub fn set_key_alias_map(&mut self, aliases: KeyAliasMap) {
        self.key_aliases = merge_aliases(&aliases);
        self.rebuild_bindings_map();
    }

    fn rebuild_bindings_map(&mut self) {
        self.bindings_map = build_key_bindings_map(
            &merge_bindings(self.key_bindings.clone()),
            Some(&self.key_aliases),
        );
    }

    /// `onResize` → `setViewportSize` (EditBufferRenderable.ts:481-483).
    pub fn resize(&mut self, width: u32, _height: u32) {
        self.viewport_width = width;
    }

    /// The edit buffer's display width (the measure input).
    #[must_use]
    pub fn edit_width_cols(&self) -> u32 {
        self.edit.layout.width
    }

    /// The measure contract (native-renderable.zig:63-81):
    /// `(max(1, line width), max(1, line count))`, clamped to the
    /// at-most width for non-absolute nodes.
    #[must_use]
    pub fn measure(&self, at_most_width: Option<f32>) -> (f32, f32) {
        let w = self.edit.layout.width.max(1) as f32;
        match at_most_width {
            Some(avail) => (w.min(avail.max(0.0)), 1.0),
            None => (w, 1.0),
        }
    }

    /// `ensureCursorVisible` (editor-view.zig:208-263), the wrap-none
    /// column branch (lines 240-253). `margin_cols` is
    /// `max(1, floor(width * scrollMargin))` clamped to
    /// `(width - 1) / 2` (lines 219-221).
    pub fn ensure_cursor_visible(&self) {
        let width = self.viewport_width;
        if width == 0 {
            return;
        }
        let raw_margin = ((width as f64) * self.scroll_margin).floor().max(1.0) as u32;
        let max_margin = if width > 1 { (width - 1) / 2 } else { 0 };
        let margin_cols = raw_margin.min(max_margin);

        let cursor_col = self.edit.cursor_col();
        let vp_x = self.viewport_x.get();
        let mut new_offset_x = vp_x;
        if cursor_col < vp_x + margin_cols {
            new_offset_x = cursor_col.saturating_sub(margin_cols);
        } else if cursor_col >= vp_x + width - margin_cols {
            new_offset_x = cursor_col + margin_cols - width + 1;
        }
        self.viewport_x.set(new_offset_x);
    }

    /// The visible slice start (viewport x offset).
    #[must_use]
    pub fn viewport_offset_x(&self) -> u32 {
        self.viewport_x.get()
    }

    /// The terminal cursor position (col relative to the viewport) —
    /// `renderCursor` (EditBufferRenderable.ts:1010-1022) computes
    /// `screenX + visualCursor.visualCol + 1`; the +1 terminal
    /// coordinate stays with the caller.
    #[must_use]
    pub fn visual_cursor_col(&self) -> u32 {
        self.edit.cursor_col().saturating_sub(self.viewport_x.get())
    }

    /// `renderSelf` — the editor-view draw (buffer.zig:1652-2087,
    /// single line): placeholder switch (editor-view.zig:969-984),
    /// prefilled viewport bg (buffer.zig:1671-1679), viewport-cropped
    /// cluster walk with the transparent-text fast path
    /// (buffer.zig:1801-1859, 2005-2065).
    pub fn paint(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32) {
        if screen_y < 0 || screen_y >= buffer.height as i32 {
            return;
        }
        self.ensure_cursor_visible();
        let vp_x = self.viewport_x.get();
        let vp_w = self.viewport_width;

        // effective colors (Textarea.updateColors, Textarea.ts:302-308)
        let fg = if self.focused {
            self.focused_text_color
        } else {
            self.unfocused_text_color
        };
        let bg = if self.focused {
            self.focused_background_color
        } else {
            self.unfocused_background_color
        };

        // placeholder shown iff the text is empty and a placeholder
        // exists (editor-view.zig:971-984)
        let show_placeholder = self.edit.text.is_empty() && !self.placeholder.is_empty();

        // Prefilled viewport bg: only when the EDIT buffer's default bg
        // is non-transparent (buffer.zig:1671-1679) — painted even
        // while the placeholder shows.
        if bg.alpha() > 0 && vp_w > 0 {
            buffer.fill_rect_clipped(screen_x, screen_y, vp_w, 1, bg);
        }

        let layout = if show_placeholder {
            analyze_line(&self.placeholder)
        } else {
            self.edit.layout.clone()
        };
        let draw_fg = if show_placeholder {
            self.placeholder_color
        } else {
            fg
        };
        // The placeholder buffer's default bg is null → transparent
        // (drawTextBufferInternal lineBg fallback, buffer.zig:1726).
        let draw_bg = if show_placeholder {
            Rgba::from_ints(0, 0, 0, 0)
        } else {
            bg
        };

        let transparent_fast_path = draw_bg.alpha() == 0;

        for cluster in &layout.clusters {
            let col = cluster.col;
            // viewport crop (buffer.zig:1801-1811)
            if col < vp_x {
                continue;
            }
            if col >= vp_x + vp_w {
                break;
            }
            // wide glyphs crossing the right edge are skipped whole
            // (buffer.zig:1840-1849)
            if cluster.width > 1 && col + cluster.width > vp_x + vp_w {
                continue;
            }
            let screen_col = screen_x + (col - vp_x) as i32;
            if screen_col < 0 {
                continue;
            }
            if screen_col >= buffer.width as i32 {
                break;
            }

            let text = if show_placeholder {
                &self.placeholder[cluster.start..cluster.start + cluster.len]
            } else {
                &self.edit.text[cluster.start..cluster.start + cluster.len]
            };
            let is_tab = text == "\t";

            // char word: single printable ASCII → the byte; otherwise a
            // grapheme-pool id (buffer.zig:2036-2048)
            // zero-width clusters (ZWJ, VS16) render nothing
            // (buffer.zig:1980-1984)
            if cluster.width == 0 {
                continue;
            }
            let char_word = if text.len() == 1 && cluster.width == 1 && text.as_bytes()[0] >= 32 {
                u32::from(text.as_bytes()[0])
            } else {
                let width = cluster.width;
                let gid = buffer.alloc_grapheme_pub(if is_tab { " " } else { text });
                crate::buffer::pack_grapheme_start(gid, width)
            };

            if is_tab {
                // tabs draw as spaces per column (buffer.zig:2007-2034)
                for tab_col in 0..cluster.width {
                    let x = screen_col + tab_col as i32;
                    if x < 0 || x >= buffer.width as i32 {
                        continue;
                    }
                    if col + tab_col >= vp_x + vp_w {
                        break;
                    }
                    self.write_cell(
                        buffer,
                        x as u32,
                        screen_y as u32,
                        u32::from(b' '),
                        draw_fg,
                        draw_bg,
                        transparent_fast_path,
                    );
                }
            } else {
                self.write_cell(
                    buffer,
                    screen_col as u32,
                    screen_y as u32,
                    char_word,
                    draw_fg,
                    draw_bg,
                    transparent_fast_path,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_cell(
        &self,
        buffer: &mut OptimizedBuffer,
        x: u32,
        y: u32,
        char_word: u32,
        fg: Rgba,
        bg: Rgba,
        transparent_fast_path: bool,
    ) {
        let attributes = 0u32;
        if transparent_fast_path {
            if buffer.try_set_transparent_text_cell(x, y, char_word, fg, attributes) {
                return;
            }
            // grapheme/link cells fall through to the blending write
            // (buffer.zig:2050-2065)
            buffer.set_cell_with_alpha_blending_cell(
                x,
                y,
                crate::buffer::make_cell(char_word, fg, bg, attributes),
            );
            return;
        }
        buffer.set_cell_word(x, y, char_word, fg, bg, attributes);
    }
}

/// Input.ts:67 — strip `\r`/`\n` and clamp to `max_len` CHARS
/// (`substring` operates on UTF-16 units; the port clamps chars).
fn sanitize_single_line(value: &str, max_len: usize) -> String {
    let cleaned: String = value.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if max_len == usize::MAX {
        cleaned
    } else {
        cleaned.chars().take(max_len).collect()
    }
}

/// `mergeKeyBindings(defaultTextareaKeyBindings, custom)`
/// (Textarea.ts:191) — Input PREPENDS its overrides
/// (Input.ts:81-86), so the merge key dedupes custom-over-default.
fn merge_bindings(custom: Vec<KeyBinding>) -> Vec<KeyBinding> {
    crate::keybinding::merge_key_bindings(&default_textarea_key_bindings(), &custom)
}

fn merge_aliases(custom: &KeyAliasMap) -> KeyAliasMap {
    crate::keybinding::merge_key_aliases(&default_key_aliases(), custom)
}

// ---------------------------------------------------------------------------
// Test-support surface — the word-boundary scan and selection queries
// the behavioral tests exercise directly (the fields are otherwise
// private to keep the edit model encapsulated).
// ---------------------------------------------------------------------------

/// Test view of [`LineLayout`].
pub type LineLayoutForTest = LineLayout;

/// Test view of [`analyze_line`].
#[must_use]
pub fn analyze_line_for_test(text: &str) -> LineLayout {
    analyze_line(text)
}

/// Test view of [`next_word_boundary`].
#[must_use]
pub fn next_word_boundary_for_test(layout: &LineLayout, cursor_col: u32) -> u32 {
    next_word_boundary(layout, cursor_col)
}

/// Test view of [`prev_word_boundary`].
#[must_use]
pub fn prev_word_boundary_for_test(layout: &LineLayout, cursor_col: u32) -> u32 {
    prev_word_boundary(layout, cursor_col)
}

impl InputRenderableState {
    /// Test view of the selection state.
    #[must_use]
    pub fn has_selection_for_test(&self) -> bool {
        self.edit.has_selection()
    }
}
