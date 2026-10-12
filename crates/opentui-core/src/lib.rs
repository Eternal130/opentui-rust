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

pub mod ansi;
pub mod buffer;
pub mod color;
pub mod editor;
#[cfg(test)]
mod editor_tests;
pub mod keybinding;
pub mod layout;
pub mod renderable;
pub mod renderables;
pub mod renderer;
pub mod styled_text;
#[cfg(test)]
mod text_buffer_tests;

pub mod scroll_acceleration;
pub mod text_buffer;
pub mod text_node;

pub use ansi::TextAttributes;
pub use buffer::OptimizedBuffer;
pub use color::{parse_color, ColorIntent, Rgba};
pub use editor::{
    EditBuffer, EditorView, SelectionBehavior, SelectionOccupancy, TextSelection, VisualCursor,
};
pub use keybinding::{KeyBinding, KeyEvent};
pub use layout::{Dimension, LayoutOptions};
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
pub use scroll_acceleration::{LinearScrollAccel, MacOSScrollAccel, ScrollAcceleration};
pub use styled_text::{
    apply_style, create_text_attributes, string_to_styled_text, StylableInput, StyleAttrs,
    StyledText, TextChunk,
};
pub use text_buffer::{TextAlign, TextDocument};
pub use text_node::{NodeStyle, TextNodeOptions, TextNodeRenderable};

/// Upstream version this crate pins (`packages/core/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";
