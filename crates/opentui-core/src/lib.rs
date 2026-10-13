//! # opentui-core — the port of `@opentui/core` 0.5.14
//!
//! Spec source: `third_party/opentui` at tag `v0.5.14`
//! (commit `31a93fbe66992298d6d0f27481fa781f43d0c1e2`), MIT.
//! Every ported semantic cites its upstream `file:line`.
//!
//! The crate is render-first: colors ([`color`]), the cell grid
//! ([`buffer`]), SGR emission ([`ansi`]), flexbox layout via taffy
//! ([`layout`], replacing yoga — see ARCHITECTURE.md), the renderable tree
//! ([`renderable`]) with [`renderables::box_::BoxRenderable`] and
//! [`renderables::text::TextRenderable`], and the headless renderer
//! ([`renderer`], `createCliRenderer` equivalent).
//!
//! The events/input family: keypress parsing ([`parse_keypress`],
//! [`parse_keypress_kitty`]), mouse parsing ([`parse_mouse`]), paste
//! ([`paste`]), the key-handler dispatch contract ([`events`]), key
//! binding resolution ([`keybinding`]), the single-line input
//! renderable ([`input`]), and the frame scheduler semantics
//! ([`scheduler`]).
//!
//! The editor family: [`editor`] (EditBuffer/EditorView/selection —
//! the edit-buffer.test.ts core) and
//! [`renderables::textarea::TextareaState`] (the TextareaRenderable
//! port: the 36-action executor, scroll-follow, placeholder, cursor
//! SGR emission).

pub mod ansi;
pub mod buffer;
pub mod color;
pub mod editor;
#[cfg(test)]
mod editor_tests;
pub mod events;
pub mod input;
pub mod keybinding;
pub mod layout;
pub mod parse_keypress;
pub mod parse_keypress_kitty;
pub mod parse_mouse;
pub mod paste;
pub mod renderable;
pub mod renderables;
pub mod renderer;
pub mod styled_text;
#[cfg(test)]
mod text_buffer_tests;

pub mod scheduler;
pub mod scroll_acceleration;
pub mod text_buffer;
pub mod text_node;

pub use ansi::TextAttributes;
pub use buffer::OptimizedBuffer;
pub use color::{parse_color, ColorIntent, Rgba};
pub use editor::{
    EditBuffer, EditorView, SelectionBehavior, SelectionOccupancy, TextSelection, VisualCursor,
};
pub use events::{KeyEvent, KeyHandler, MouseButton, MouseEvent, PasteEvent};
pub use input::{InputAction, InputOptions, InputRenderableState, InputStateEvent};
pub use keybinding::KeyBinding;
pub use layout::{Dimension, LayoutOptions};
pub use parse_keypress::{
    non_alphanumeric_keys, parse_keypress, terminal_named_single_stroke_keys, KeyEventType,
    KeySource, ParseKeypressOptions, ParsedKey,
};
pub use parse_keypress_kitty::parse_kitty_keyboard;
pub use parse_mouse::{MouseEventType, MouseParser, RawMouseEvent, ScrollDirection, ScrollInfo};
pub use paste::{decode_paste_bytes, strip_ansi_sequences, BracketedPasteCollector};
pub use renderable::{Renderable, RenderableId, RootRenderable};
pub use renderables::box_::{BorderSpec, BorderStyle, BoxOptions, BoxRenderableState};
pub use renderables::scrollbar::{
    ArrowDirection, ArrowState, Orientation, ScrollBarState, ScrollUnit, SliderState,
};
pub use renderables::scrollbox::{
    get_objects_in_viewport, scroll_by, scroll_child_into_view, scroll_to, set_scroll_left,
    set_scroll_top, ScrollBoxOptions, ScrollBoxState, StickyStart, ViewportCulling,
};
pub use renderables::text::{TextOptions, TextRenderableState, WrapMode};
pub use renderables::textarea::{TextareaOptions, TextareaState, TEXTAREA_ACTIONS};
pub use renderer::{create_cli_renderer, CliRenderer, CliRendererConfig, RenderTarget};
pub use scheduler::RenderScheduler;
pub use scroll_acceleration::{LinearScrollAccel, MacOSScrollAccel, ScrollAcceleration};
pub use styled_text::{
    apply_style, create_text_attributes, string_to_styled_text, StylableInput, StyleAttrs,
    StyledText, TextChunk,
};
pub use text_buffer::{TextAlign, TextDocument};
pub use text_node::{NodeStyle, TextNodeOptions, TextNodeRenderable};

/// Upstream version this crate pins (`packages/core/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";
