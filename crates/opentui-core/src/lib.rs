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
pub mod layout;
pub mod renderable;
pub mod renderables;
pub mod renderer;

pub use ansi::TextAttributes;
pub use buffer::OptimizedBuffer;
pub use color::{parse_color, ColorIntent, Rgba};
pub use layout::{Dimension, LayoutOptions};
pub use renderable::{Renderable, RenderableId, RootRenderable};
pub use renderables::box_::{BorderSpec, BorderStyle, BoxOptions, BoxRenderableState};
pub use renderables::text::{TextOptions, TextRenderableState, WrapMode};
pub use renderer::{create_cli_renderer, CliRenderer, CliRendererConfig, RenderTarget};

/// Upstream version this crate pins (`packages/core/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";
