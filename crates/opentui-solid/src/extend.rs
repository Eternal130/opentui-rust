//! `extend()` — custom intrinsic registration
//! (upstream `elements/catalogue.ts:117-139`).
//!
//! Upstream: `extend({ consoleButton: ButtonRenderable })` mutates the
//! global `componentCatalogue`; `createElement(tagName)` looks the tag
//! up there and `new`s it (reconciler.ts:200-206 — unknown tags throw
//! `[Reconciler] Unknown component type: ${tagName}`). v2 registers six
//! custom elements this way (retry_provider, fade_in_text, …).
//!
//! Rust has no classes to subclass `BaseRenderable`, so a custom
//! element is a FACTORY triple: build the [`Renderable`] (a composite
//! of core renderables, or a bare box/text), patch it from prop
//! changes, and clean it up on unmount. The registry is process-global
//! like upstream's module-level catalogue.

use crate::props::{rebuild_box, rebuild_text, PropMap};
use opentui_core::renderable::{Content, Renderable};
use opentui_core::renderer::CliRenderer;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The mount/patch/cleanup surface a custom element provides.
pub struct CustomElement {
    pub name: String,
    /// `createElement(tagName)` — build the renderable. `props` are the
    /// already-read static+dynamic values (upstream constructors receive
    /// the renderer context + options).
    #[allow(clippy::type_complexity)]
    pub build: Rc<dyn Fn(&mut CliRenderer, &PropMap, &str) -> Renderable>,
    /// `setProperty` routing for data props: update the live renderable
    /// from the full new prop map (prev included for diffing). Optional
    /// — when absent the default rebuilders run by content kind.
    #[allow(clippy::type_complexity)]
    pub patch: Option<Rc<dyn Fn(&mut CliRenderer, &str, &PropMap, &PropMap)>>,
    /// `onCleanup` teardown (upstream e.g. the image source clear,
    /// reconciler.ts:207-212).
    #[allow(clippy::type_complexity)]
    pub cleanup: Option<Rc<dyn Fn(&PropMap, &str)>>,
}

impl CustomElement {
    /// A composite/box-backed custom element using the standard
    /// rebuild patch path.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        build: impl Fn(&mut CliRenderer, &PropMap, &str) -> Renderable + 'static,
    ) -> Self {
        CustomElement {
            name: name.into(),
            build: Rc::new(build),
            patch: None,
            cleanup: None,
        }
    }

    #[must_use]
    pub fn with_patch(
        mut self,
        patch: impl Fn(&mut CliRenderer, &str, &PropMap, &PropMap) + 'static,
    ) -> Self {
        self.patch = Some(Rc::new(patch));
        self
    }

    #[must_use]
    pub fn with_cleanup(mut self, cleanup: impl Fn(&PropMap, &str) + 'static) -> Self {
        self.cleanup = Some(Rc::new(cleanup));
        self
    }
}

type Catalogue = HashMap<String, Rc<CustomElement>>;

fn with_catalogue<R>(f: impl FnOnce(&mut Catalogue) -> R) -> R {
    // module-level single-threaded registry — upstream's catalogue is a
    // plain module global (the solid runtime is single-threaded)
    thread_local! {
        static REGISTRY: RefCell<Catalogue> = RefCell::new(HashMap::new());
    }
    REGISTRY.with(|r| f(&mut r.borrow_mut()))
}

/// `extend(objects)` (catalogue.ts:133-135) — register custom elements
/// into the global catalogue (later registrations win, like
/// `Object.assign`).
pub fn extend(elements: Vec<CustomElement>) {
    with_catalogue(|cat| {
        for el in elements {
            cat.insert(el.name.clone(), Rc::new(el));
        }
    });
}

/// `getComponentCatalogue()` (catalogue.ts:137-139).
#[must_use]
pub fn get_component_catalogue() -> Vec<String> {
    with_catalogue(|cat| {
        let mut names: Vec<String> = cat.keys().cloned().collect();
        names.extend(
            [
                "box",
                "text",
                "scrollbox",
                "span",
                "b",
                "strong",
                "i",
                "em",
                "u",
                "a",
                "br",
            ]
            .iter()
            .map(|s| (*s).to_owned()),
        );
        names.sort();
        names.dedup();
        names
    })
}

/// Registry lookup (the reconciler's createElement path,
/// reconciler.ts:200-206).
#[must_use]
pub fn lookup_custom(name: &str) -> Option<Rc<CustomElement>> {
    with_catalogue(|cat| cat.get(name).cloned())
}

/// Build a custom element's renderable (the `new elements[tagName]`
/// step). Fails loudly for unknown tags — upstream throws
/// `Unknown component type: ${tagName}`.
pub fn create_custom(
    renderer: &mut CliRenderer,
    tag: &str,
    props: &PropMap,
    id: &str,
) -> Result<Renderable, String> {
    let Some(factory) = lookup_custom(tag) else {
        return Err(format!("[Reconciler] Unknown component type: {tag}"));
    };
    Ok((factory.build)(renderer, props, id))
}

/// Patch a live custom element (setProperty routing).
pub fn patch_custom(
    renderer: &mut CliRenderer,
    tag: &str,
    live_id: &str,
    new_props: &PropMap,
    prev_props: &PropMap,
) {
    if let Some(factory) = lookup_custom(tag) {
        if let Some(patch) = &factory.patch {
            patch(renderer, live_id, new_props, prev_props);
            return;
        }
    }
    // default path: rebuild by the renderable's content kind
    if rebuild_box(renderer, live_id, new_props) {
        return;
    }
    rebuild_text(renderer, live_id, new_props, &[]);
}

/// Run a custom element's cleanup (onCleanup).
pub fn cleanup_custom(tag: &str, props: &PropMap, live_id: &str) {
    if let Some(factory) = lookup_custom(tag) {
        if let Some(cleanup) = &factory.cleanup {
            cleanup(props, live_id);
        }
    }
}

/// The default content-kind probe used by [`patch_custom`].
#[must_use]
pub fn content_kind_of(renderer: &CliRenderer, live_id: &str) -> Option<&'static str> {
    renderer.root.inner.find(live_id).map(|n| match n.content {
        Content::Text(_) => "text",
        Content::Box(_) | Content::ScrollBox(_, _) | Content::ScrollContent(_, _) => "box",
        _ => "other",
    })
}
