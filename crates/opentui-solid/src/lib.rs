//! # opentui-solid — the Rust reactive-glue port of `@opentui/solid`
//! 0.5.14
//!
//! ## What upstream solid is
//!
//! `@opentui/solid` binds `@opentui/core` to solid-js: a custom
//! reconciler (`src/reconciler.ts` over `src/renderer/universal.js`)
//! maps JSX component trees onto renderable trees, `render()` boots a
//! `createCliRenderer` and mounts the tree, hooks (`useRenderer`,
//! `useTerminalDimensions`, `useKeyboard`, `usePaste`) subscribe to
//! renderer events, `Portal` reparents subtrees, `extend()` registers
//! custom components, and `createScrollbackWriter` drives the
//! split-footer scrollback.
//!
//! ## Design: the Rust port (decision record)
//!
//! Rust has no solid-js. The consumer (opencode-rust) runs its own
//! Signal/Store reactive core. The port's shape:
//!
//! * **signals stay consumer-owned.** The [`Reactive`] trait is the
//!   minimal boundary (get + subscribe-with-unsubscribe); standalone
//!   users get the built-in [`Signal`](reactive::Signal), consumers
//!   adapt theirs.
//! * **the component tree is a builder tree, not JSX.** TSX elements
//!   become [`Element`]s built with [`box_!`]/[`text_!`]/[`scrollbox_!`]
//!   macros (upstream-camelCase prop names) or snake_case builder
//!   chains — the 1:1 target of the TSX→Rust transplants. `Show`/`For`
//!   are reactive-scope constructs ([`show`]/[`for_list`]).
//! * **reconciler semantics are keyed, not identity-based.**
//!   Upstream `reconcileArrays` matches children by node INSTANCE
//!   identity (universal.js:108-163); Rust rebuilds value trees, so
//!   identity is explicit — element keys (For forces item keys;
//!   unkeyed elements never match across passes). The algorithm
//!   (prefix/suffix trims, swap detection, map+sequence heuristic)
//!   is ported op-for-op; see [`reconcile`].
//! * **dynamic props are subscriptions, not effects.** A JSX getter
//!   prop (`content={sig()}`) becomes [`Prop::Dyn`]; its subscription
//!   enqueues a minimal patch (per-prop equality skip,
//!   universal.js:202-204), applied at [`SolidRenderer::flush`] — the
//!   batched equivalent of synchronous render effects +
//!   `requestRender`.
//! * **unmount = detach + drop + cleanups.** Upstream defers
//!   `destroyRecursively` to the next tick (reconciler.ts:135-140);
//!   the port drops synchronously and runs `onCleanup` hooks
//!   children-first, own LIFO (solid owner disposal).
//!
//! ## Module map (upstream citations in each module doc)
//!
//! * [`reactive`] — the signal substrate (solid-js createSignal /
//!   createMemo shapes).
//! * [`element`] — the Element/Prop/Children model (one JSX element).
//! * [`builder`] — the JSX-equivalent builder API (macros + chains).
//! * [`props`] — `setProperty` + option builders (reconciler.ts:230).
//! * [`reconcile`] — [`SolidRenderer`]: mount/diff/unmount engine,
//!   hooks, Portal, events.
//! * [`extend`] — the custom-element registry (catalogue.ts:117-139).
//! * [`scrollback`] — `createScrollbackWriter` (scrollback.ts:145+).
//!
//! See PORT-MATRIX.md for the symbol-by-symbol surface.

pub mod builder;
pub mod element;
pub mod extend;
pub mod hooks;
pub mod props;
pub mod reactive;
pub mod reconcile;
pub mod runtime_plugin_support;
pub mod scrollback;
pub mod time_to_first_draw;

#[cfg(test)]
mod tests;

pub use builder::{
    bold, box_, br, children_expr, custom, dyn_, dyn_value, for_list, group, italic, link, map,
    portal, scrollbox, show, span, text, text_node, underline, ElementBuilder, ForItemSource,
    KeyedListSource,
};
pub use element::{
    Children, DynChildren, DynPropSource, Element, ExprChildren, ForChildren, ForItem, ForSource,
    Prop, PropValue, ShowChildren, SpanKind, Tag,
};
pub use hooks::{EventData, EventHub, KeyEvent, KeyEventType, PasteEvent};
pub use reactive::{Mapped, Reactive, Signal};
pub use reconcile::{ShadowNode, SolidRenderer};
pub use runtime_plugin_support::{
    ensure_runtime_plugin_support, RuntimePluginRewriteOptions, SolidRuntimePluginSupportOptions,
    DEFAULT_RUNTIME_MODULES,
};
pub use scrollback::{
    create_scrollback_writer, write_solid_to_scrollback, ScrollbackRenderContext,
    ScrollbackSnapshot, ScrollbackWriter, SolidScrollbackWriterOptions,
};
pub use time_to_first_draw::{
    register_time_to_first_draw, time_to_first_draw, TIME_TO_FIRST_DRAW_TAG,
};

/// Upstream version this crate pins (`packages/solid/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";

/// `render(node, config)` (index.ts:70-83): boot a headless renderer
/// and mount the tree. Returns the mounted [`SolidRenderer`] for the
/// caller to keep flushing.
#[must_use]
pub fn render(
    config: opentui_core::renderer::CliRendererConfig,
    tree: element::Element,
) -> SolidRenderer {
    let mut sr = SolidRenderer::new(config);
    sr.render_root(tree);
    sr
}

/// `render(node, renderer)` (index.ts:70-83, the
/// `rendererOrConfig instanceof CliRenderer` arm): mount the solid
/// tree onto a LIVE renderer's root — `_render(() =>
/// createComponent(...), renderer.root)` via `mountSolidRoot`
/// (index.ts:9-68). This is the split-footer footer-mount face
/// (upstream mounts the footer component with exactly this call
/// shape). [`SolidRenderer::dispose_root`] is the disposeFn half.
#[must_use]
pub fn render_into(renderer: opentui_core::renderer::CliRenderer, tree: element::Element) -> SolidRenderer {
    let mut sr = SolidRenderer::with_renderer(renderer);
    sr.render_root(tree);
    sr
}
