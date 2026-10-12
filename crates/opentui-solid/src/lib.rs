//! # opentui-solid — the Rust reactive-glue equivalent of
//! `@opentui/solid` 0.5.14
//!
//! ## What upstream solid is
//!
//! `@opentui/solid` binds `@opentui/core` to solid-js: a custom
//! reconciler (`reconciler.ts`) maps JSX component trees onto
//! renderable trees, `render()` boots a `createCliRenderer` and mounts
//! the tree, hooks (`useRenderer`, `useTerminalDimensions`,
//! `useKeyboard`, `usePaste`) subscribe to renderer events, `Portal`
//! reparents subtrees, `extend()` registers custom components, and
//! `createScrollbackWriter`/`writeSolidToScrollback` drive the
//! split-footer scrollback.
//!
//! ## Design: the Rust equivalent (decision record)
//!
//! Rust has no solid-js. The consumer (opencode-rust) already runs a
//! Signal/Store reactive core (`oc-tui` screens). The port's shape:
//!
//! * **signals stay consumer-owned.** `opentui-solid` defines the
//!   minimal `Reactive` trait boundary (get/set + subscribe) rather
//!   than importing a signals crate, so opencode-rust's signals adapt
//!   with a zero-cost impl and standalone users get the built-in
//!   [`Signal`].
//! * **the component tree is a builder tree, not JSX.** TSX components
//!   become Rust builder constructors (`text("hello")`,
//!   `box_(opts).child(...)`) consumed by [`render`] — the 1:1 mapping
//!   the synthesis migration needs (TSX → Rust builders).
//! * **reconciler semantics (tier 2):** keyed diffing of builder trees
//!   onto live renderables (upstream reconciler.ts) — props update in
//!   place, children reorder by key, unmount destroys. The skeleton
//!   implements mount-only; diffing lands with the ScrollBox lane.
//!
//! ## Status
//!
//! Skeleton: [`Signal`], the [`Reactive`] trait, builder nodes, and a
//! mount-only [`render`] that walks a builder tree into a
//! [`opentui_core::CliRenderer`]. See PORT-MATRIX.md for the solid
//! surface (11 tier-1 symbols) and lane order.

use opentui_core::renderables::box_::BoxOptions;
use opentui_core::renderables::text::TextOptions;
use opentui_core::renderer::CliRenderer;

/// Upstream version this crate pins (`packages/solid/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";

/// The reactive boundary (design doc above). `get` reads the current
/// value; `subscribe` registers a listener fired on set (solid's
/// signal subscribers).
pub trait Reactive<T> {
    fn get(&self) -> T;
    fn subscribe(&self, cb: Box<dyn Fn(&T)>);
}

/// Minimal built-in signal (standalone users; opencode-rust adapts its
/// own via [`Reactive`]).
type Subscribers<T> = Vec<Box<dyn Fn(&T)>>;

pub struct Signal<T: Copy> {
    value: std::cell::RefCell<T>,
    subs: std::cell::RefCell<Subscribers<T>>,
}

impl<T: Copy> Signal<T> {
    #[must_use]
    pub fn new(value: T) -> Self {
        Signal {
            value: std::cell::RefCell::new(value),
            subs: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub fn set(&self, value: T) {
        *self.value.borrow_mut() = value;
        let subs = self.subs.borrow();
        for cb in subs.iter() {
            cb(&value);
        }
    }
}

impl<T: Copy + 'static> Reactive<T> for Signal<T> {
    fn get(&self) -> T {
        *self.value.borrow()
    }

    fn subscribe(&self, cb: Box<dyn Fn(&T)>) {
        self.subs.borrow_mut().push(cb);
    }
}

/// A builder node — the Rust equivalent of one JSX element
/// (design doc: TSX → Rust builders 1:1).
pub enum Node {
    Text { id: String, opts: TextOptions },
    Box { id: String, opts: BoxOptions },
    Children(Vec<Node>),
}

/// Builder constructors (`text()` ≈ `<text>` element — the reconciler's
/// createElement targets).
#[must_use]
pub fn text(id: &str, content: &str) -> Node {
    Node::Text {
        id: id.into(),
        opts: TextOptions {
            content: opentui_core::renderables::text::ContentInput::Str(content.to_owned()),
            ..Default::default()
        },
    }
}

#[must_use]
pub fn box_(id: &str, opts: BoxOptions) -> Node {
    Node::Box {
        id: id.into(),
        opts,
    }
}

#[must_use]
pub fn children(nodes: Vec<Node>) -> Node {
    Node::Children(nodes)
}

/// Materialize a builder node into the renderer's tree — mount-only
/// reconciler (upstream reconciler.ts createInstance/appendChild).
pub fn mount(renderer: &mut CliRenderer, node: Node) {
    match node {
        Node::Children(list) => {
            for child in list {
                mount(renderer, child);
            }
        }
        Node::Text { id, opts } => renderer.add_text(&id, &opts),
        Node::Box { id, opts } => renderer.add_box(&id, &opts),
    }
}

/// `render()` (upstream index: boot a renderer and mount the tree) —
/// headless form: mount + one frame, returning the renderer for the
/// caller to keep ticking.
pub fn render(config: opentui_core::renderer::CliRendererConfig, tree: Node) -> CliRenderer {
    let mut renderer = opentui_core::renderer::create_cli_renderer(config);
    mount(&mut renderer, tree);
    renderer.render();
    renderer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_fires_subscribers() {
        let s = Signal::new(1);
        let hits = std::rc::Rc::new(std::cell::Cell::new(0));
        let h2 = hits.clone();
        s.subscribe(Box::new(move |&v| {
            if v == 2 {
                h2.set(h2.get() + 1);
            }
        }));
        s.set(2);
        assert_eq!(hits.get(), 1);
    }

    #[test]
    fn render_mounts_and_draws() {
        let renderer = render(
            opentui_core::renderer::CliRendererConfig {
                width: 10,
                height: 2,
                ..Default::default()
            },
            children(vec![text("t", "hello")]),
        );
        let frame = renderer.current_render_buffer.char_frame();
        assert!(frame.starts_with("hello"));
    }
}
