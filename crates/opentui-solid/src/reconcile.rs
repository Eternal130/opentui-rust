//! The reconciler — the port of `@opentui/solid`'s custom renderer
//! (`src/renderer/universal.js` driving `src/reconciler.ts`).
//!
//! Upstream shape (citations are `packages/solid` @ 0.5.14):
//!
//! * **element creation** — `createElement(tagName)` looks up the
//!   component catalogue and constructs the renderable with
//!   `getNextId(tagName)` (reconciler.ts:193-214; unknown tags throw
//!   `[Reconciler] Unknown component type`, :200-204).
//! * **prop patching** — `spread(el, props)` runs one render effect
//!   per spread; each prop with `value !== prevProps[prop]` calls
//!   `setProperty` (universal.js:198-206). `setProperty`'s switch
//!   (reconciler.ts:230-367) routes `on:*` events (:231-239), `id`
//!   (:260-263), `focused` (:264-270), onChange/onInput/onSubmit/
//!   onSelect (:272-334), `style` (per-sub-prop diff, :335-354),
//!   `text`/`content` (:355-361), and everything else as field
//!   assignment (:363-366).
//! * **children diffing** — `insert`/`insertExpression` over arrays
//!   run `reconcileArrays` (universal.js:108-163): prefix/suffix
//!   trims, swap detection, and a map+sequence heuristic — keyed by
//!   NODE IDENTITY upstream, by element KEY here (Rust rebuilds value
//!   trees, so identity is explicit — `element.rs`). Unkeyed elements
//!   never match across passes (fresh nodes each build — the churn
//!   equivalent of upstream re-running an accessor expression).
//! * **insert/remove** — `_insertNode` appends or inserts at the
//!   anchor's live index (reconciler.ts:60-111); `_removeNode`
//!   detaches and destroys the subtree on the next tick (:113-141 —
//!   a synchronous drop here). Keyed MOVES detach + re-attach the
//!   intact subtree (upstream `add`'s same-parent branch,
//!   Renderable.ts:1225-1240).
//! * **unmount/cleanup** — solid owner disposal: child scopes first,
//!   own cleanups in reverse registration order (`onCleanup`,
//!   reconciler.ts:23, 207-212).

use crate::element::{Children, Element, Prop, PropValue, Tag};
use crate::extend;
use crate::hooks::{DimensionsSignal, EventData, EventHub, KeyEvent, PasteEvent};
use crate::props::{self, PropMap};
use crate::reactive::Signal;
use opentui_core::layout::{Dimension, LayoutOptions};
use opentui_core::renderable::{Content, Renderable};
use opentui_core::renderer::{create_cli_renderer, CliRenderer, CliRendererConfig};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// One shadow-tree node: the reconciler's mirror of one live
/// renderable (upstream: the node instance IS the identity; here the
/// key is — see the module doc).
pub struct ShadowNode {
    /// Reconciler identity (upstream: instance identity).
    pub key: String,
    /// Live renderable id (upstream `node.id` — `getNextId`).
    pub live_id: String,
    pub tag: Tag,
    /// last-applied props (universal.js `prevProps`).
    pub props: PropMap,
    /// The live id children mount under (own id; portals point at
    /// their container — extras.ts:27-37).
    pub children_parent_live: String,
    pub children: Vec<ShadowNode>,
    /// Last-applied children elements (the rebuild source for text
    /// node-tree content on data-prop patches).
    last_children: Vec<Element>,
    /// `onCleanup` handlers (child scopes dispose first, then own
    /// cleanups LIFO — solid owner disposal).
    pub cleanups: Vec<Box<dyn Fn()>>,
    /// Signal subscription detachers (upstream: effect disposal).
    unsubscribers: Vec<Box<dyn Fn()>>,
    /// Per-node event handlers (`on:*`, reconciler.ts:231-239).
    events: HashMap<String, EventHandlers>,
}

impl ShadowNode {
    fn new(key: String, live_id: String, tag: Tag) -> Self {
        ShadowNode {
            children_parent_live: live_id.clone(),
            key,
            live_id,
            tag,
            props: Vec::new(),
            children: Vec::new(),
            last_children: Vec::new(),
            cleanups: Vec::new(),
            unsubscribers: Vec::new(),
            events: HashMap::new(),
        }
    }

    #[must_use]
    pub fn find(&self, key: &str) -> Option<&ShadowNode> {
        if self.key == key {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(key))
    }

    fn find_mut(&mut self, key: &str) -> Option<&mut ShadowNode> {
        if self.key == key {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(key))
    }
}

type PendingPatch = Box<dyn FnOnce(&mut SolidRenderer)>;
type EventHandlers = Vec<Rc<dyn Fn(&EventData)>>;

/// The solid renderer — upstream `render()`'s mounted root
/// (index.ts:9-83: renderer + solid root + disposer; the port owns
/// both halves).
pub struct SolidRenderer {
    /// The live renderable tree (upstream `CliRenderer`).
    pub renderer: CliRenderer,
    root: ShadowNode,
    inbox: Rc<RefCell<Vec<PendingPatch>>>,
    /// The reconciler op log — upstream `log()` (utils/log.ts) is
    /// DEBUG-gated stdout; the port records ops for tests
    /// (`OT_SOLID_DEBUG=1` mirrors both).
    ops: Vec<String>,
    /// `getNextId` counters (utils/id-counter.ts:3-12).
    id_counters: HashMap<String, u32>,
    /// terminal size signal (useTerminalDimensions, hooks.ts:36-50).
    dimensions: DimensionsSignal,
    /// `onResize` stream (hooks.ts:24-33).
    resize_hub: EventHub<(u32, u32)>,
    /// keyboard stream (useKeyboard, hooks.ts:73-89).
    keyboard: EventHub<KeyEvent>,
    /// paste stream (usePaste, hooks.ts:91-101).
    paste: EventHub<PasteEvent>,
}

impl SolidRenderer {
    /// `testRender`-shaped boot (index.ts:85-97): a headless renderer
    /// of the given size + an empty solid root.
    #[must_use]
    pub fn new(config: CliRendererConfig) -> Self {
        let dimensions = Signal::new((config.width, config.height));
        SolidRenderer {
            renderer: create_cli_renderer(config),
            root: ShadowNode::new("__root__".into(), "__root__".into(), Tag::Group),
            inbox: Rc::new(RefCell::new(Vec::new())),
            ops: Vec::new(),
            id_counters: HashMap::new(),
            dimensions,
            resize_hub: EventHub::new(),
            keyboard: EventHub::new(),
            paste: EventHub::new(),
        }
    }

    /// Upstream `getNextId(elementType)` (utils/id-counter.ts:3-12):
    /// `${elementType}-${n}` starting at 1.
    fn next_id(&mut self, tag: &str) -> String {
        let counter = self.id_counters.entry(tag.to_owned()).or_insert(0);
        *counter += 1;
        format!("{tag}-{counter}")
    }

    /// The recorded op log (the upstream `log()` seam — DEBUG-gated
    /// stdout there, in-memory here; test observability).
    #[must_use]
    pub fn ops_log(&self) -> &[String] {
        &self.ops
    }

    fn log(&mut self, msg: String) {
        if std::env::var("OT_SOLID_DEBUG").is_ok() {
            eprintln!("[Reconciler] {msg}");
        }
        self.ops.push(msg);
    }

    // ------------------------------------------------------------------
    // hooks (elements/hooks.ts)
    // ------------------------------------------------------------------

    /// `useRenderer()` — the renderer context (always present inside
    /// the mounted root; upstream throws outside a renderer,
    /// hooks.ts:16-20).
    #[must_use]
    pub fn use_renderer(&self) -> &CliRenderer {
        &self.renderer
    }

    /// `useTerminalDimensions()` — terminal size as a signal
    /// (hooks.ts:36-50). Read via the [`Reactive`](crate::Reactive)
    /// trait or `.get()`; updated by [`SolidRenderer::resize`].
    #[must_use]
    pub fn terminal_dimensions(&self) -> &DimensionsSignal {
        &self.dimensions
    }

    /// `onResize(cb)` (hooks.ts:24-33). Returns the `off` handle
    /// (upstream `onCleanup` detaches).
    pub fn on_resize(&self, cb: impl Fn(u32, u32) + 'static) -> Box<dyn Fn()> {
        self.resize_hub.on(move |&(w, h)| cb(w, h))
    }

    /// `useKeyboard` — the key event stream (presses + repeats by
    /// default; `release` events through the same hub —
    /// hooks.ts:73-89).
    #[must_use]
    pub fn keyboard(&self) -> &EventHub<KeyEvent> {
        &self.keyboard
    }

    /// `usePaste` — the paste event stream (hooks.ts:91-101).
    #[must_use]
    pub fn paste(&self) -> &EventHub<PasteEvent> {
        &self.paste
    }

    /// Host seam: feed a parsed key event (the PTY → KeyEvent step
    /// the upstream renderer's keyInput owns). Emits then flushes.
    pub fn feed_key(&mut self, event: KeyEvent) {
        self.keyboard.emit(&event);
        self.flush();
    }

    /// Host seam: feed a paste event.
    pub fn feed_paste(&mut self, event: PasteEvent) {
        self.paste.emit(&event);
        self.flush();
    }

    /// `renderer.resize(w, h)` routed through the solid layer: core
    /// resize + the dimensions signal + the resize stream (the
    /// `resize` event feeding `setTerminalDimensions`,
    /// hooks.ts:43-49).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
        self.dimensions.set((width, height));
        self.resize_hub.emit(&(width, height));
    }

    // ------------------------------------------------------------------
    // mount
    // ------------------------------------------------------------------

    /// Mount a tree under the root and render one frame —
    /// `render(() => node, renderer)` (index.ts:70-83 →
    /// `_render(() => createComponent(...), renderer.root)`).
    pub fn render_root(&mut self, element: Element) {
        self.mount_element("__root__", usize::MAX, element);
        self.flush();
    }

    /// Host/driver seam: `createElement` + `spread` + `insertNode`
    /// under an existing shadow key (`"__root__"` for the renderer
    /// root). `usize::MAX` appends. Returns the mounted key.
    pub fn mount(&mut self, parent_key: &str, index: usize, element: Element) -> String {
        self.mount_element(parent_key, index, element)
    }

    /// Host/driver seam: patch one prop on a mounted node through the
    /// `setProperty` path (the scenario driver's prop steps).
    pub fn set_prop(&mut self, key: &str, name: &str, value: PropValue) {
        self.patch_prop(key, name, value);
    }

    /// The renderer root's shadow key (mount target for host code).
    pub const ROOT: &'static str = "__root__";

    /// Whether a shadow key is mounted (driver parent resolution —
    /// mounted elements shadow the reserved "root" name).
    #[must_use]
    pub fn has_mounted(&self, key: &str) -> bool {
        self.root.find(key).is_some()
    }

    /// `createElement` + `spread` + `insertNode` for one element under
    /// `parent_key` at `index` (append at `usize::MAX`). Returns the
    /// mounted node's key.
    fn mount_element(&mut self, parent_key: &str, index: usize, element: Element) -> String {
        let Element {
            tag,
            key,
            props,
            children,
            cleanups: element_cleanups,
        } = element;

        // identity + live id: an explicit key doubles as the live id
        // (upstream: apps address nodes by id; otherwise getNextId —
        // reconciler.ts:195). The reconciler identity equals the live
        // id until an `id` prop patch renames the live node.
        let live_id = key.clone().unwrap_or_else(|| self.next_id(tag.name()));
        let shadow_key = live_id.clone();

        // initial props: static + current dynamic reads (spread's
        // first pass — universal.js:192-208 with prevProps = {})
        let mut initial: PropMap = Vec::new();
        for (name, prop) in &props {
            props::prop_set(&mut initial, name, prop.read());
        }

        self.log(format!("Creating element: {}", tag.name()));
        self.log(format!("Element created with id: {live_id}"));

        let mut node = ShadowNode::new(shadow_key.clone(), live_id.clone(), tag.clone());

        // portal: a slot marker at the JSX position + a box container
        // at the mount point (extras.ts:14-55). The marker is what the
        // parent owns; the container owns the children.
        let mut portal: Option<(String, String)> = None; // (target, container live id)
        if tag == Tag::Portal {
            let target = props
                .iter()
                .find(|(n, _)| n == "mount")
                .map(|(_, p)| p.read())
                .and_then(|v| match v {
                    PropValue::Str(s) => Some(s),
                    _ => None,
                })
                .unwrap_or_else(|| "__root__".to_owned());
            let container_id = self.next_id("box");
            portal = Some((target.clone(), container_id.clone()));
            node.children_parent_live = container_id.clone();
            // the container: a plain box appended at the target
            let mut opts = opentui_core::renderables::box_::BoxOptions::default();
            opts.layout.flex_direction = Some("column".into());
            let container = self.renderer.make_box(&container_id, &opts);
            self.renderer
                .insert_child_at(&target, usize::MAX, container);
            self.log(format!(
                "Portal {shadow_key}: container {container_id} into {target}"
            ));
        }

        let live = self.create_live(&node, &initial, &children, portal.is_some());

        // insertNode (reconciler.ts:60-111): append or insert at the
        // anchor index
        let parent_live = self
            .root
            .find(parent_key)
            .map(|p| p.children_parent_live.clone())
            .unwrap_or_else(|| "__root__".to_owned());
        let existing = self.renderer.child_ids(&parent_live).map_or(0, |v| v.len());
        let idx = if index == usize::MAX {
            existing
        } else {
            index.min(existing)
        };
        let ok = self.renderer.insert_child_at(&parent_live, idx, live);
        debug_assert!(ok, "insert into {parent_live} failed");
        self.log(format!(
            "Inserting node: {live_id} into parent: {parent_live} anchor: {idx}"
        ));

        // the last-applied props (prevProps — spread's initial pass
        // records every applied prop, universal.js:192-208)
        node.props = initial.clone();

        // onCleanup handlers declared on the element (solid-js
        // onCleanup inside the component body)
        node.cleanups = element_cleanups
            .into_iter()
            .map(|c| -> Box<dyn Fn()> { Box::new(move || c()) })
            .collect();

        // bind events + dynamic props (spread's render effects)
        self.bind_events(&mut node, &props);
        for (name, prop) in &props {
            if let Prop::Dyn(src) = prop {
                let key = node.key.clone();
                let name = name.clone();
                let inbox = Rc::clone(&self.inbox);
                let src = Rc::clone(src);
                let unsub = src.subscribe(Box::new(move |v: &PropValue| {
                    let v = v.clone();
                    let name = name.clone();
                    let key = key.clone();
                    inbox
                        .borrow_mut()
                        .push(Box::new(move |sr: &mut SolidRenderer| {
                            sr.patch_prop(&key, &name, v.clone());
                        }));
                }));
                node.unsubscribers.push(unsub);
            }
        }

        // attach the shadow node BEFORE recursing (children find it)
        {
            let parent = self.root.find_mut(parent_key).expect("parent exists");
            let at = if index == usize::MAX {
                parent.children.len()
            } else {
                index.min(parent.children.len())
            };
            parent.children.insert(at, node);
        }

        // children: static mount or dynamic subscribe (universal.js'
        // insert over accessor children)
        match children {
            Children::None => {}
            Children::Static(kids) => {
                if let Some(n) = self.root.find_mut(&shadow_key) {
                    n.last_children = kids.clone();
                }
                // text-node children fold into the text's content at
                // build (props.rs::gather_text_children) — they are not
                // independent renderables
                let is_text = tag == Tag::Text;
                let mut i = 0;
                for kid in kids {
                    if is_text && matches!(kid.tag, Tag::Span(_) | Tag::TextNode) {
                        continue;
                    }
                    self.mount_element(&shadow_key, i, kid);
                    i += 1;
                }
            }
            Children::Dyn(src) => {
                let kids = src.read();
                if let Some(n) = self.root.find_mut(&shadow_key) {
                    n.last_children = kids.clone();
                }
                for (i, kid) in kids.into_iter().enumerate() {
                    self.mount_element(&shadow_key, i, kid);
                }
                self.bind_children_source(&shadow_key, &src);
            }
        }
        shadow_key
    }

    fn bind_children_source(&mut self, key: &str, src: &Rc<dyn crate::element::DynChildren>) {
        let inbox = Rc::clone(&self.inbox);
        let pk = key.to_owned();
        let src = Rc::clone(src);
        let unsub = src.subscribe(Box::new(move |els: &Vec<Element>| {
            let els = els.clone();
            let pk = pk.clone();
            inbox
                .borrow_mut()
                .push(Box::new(move |sr: &mut SolidRenderer| {
                    sr.reconcile_children(&pk, els.clone());
                }));
        }));
        if let Some(n) = self.root.find_mut(key) {
            n.unsubscribers.push(unsub);
        }
    }

    /// The `new elements[tagName](renderer, { id })` step
    /// (reconciler.ts:193-214): build the renderable for the tag from
    /// the initial prop map.
    fn create_live(
        &mut self,
        node: &ShadowNode,
        initial: &PropMap,
        children: &Children,
        is_portal: bool,
    ) -> Renderable {
        if is_portal {
            // the JSX-position marker: a layout slot (createSlotNode,
            // reconciler.ts:155-159) — occupies the position, paints
            // nothing
            let marker_id = self.next_id("slot-node");
            return self.make_group(&marker_id);
        }
        match &node.tag {
            Tag::Box => {
                let opts = props::box_options(initial);
                self.renderer.make_box(&node.live_id, &opts)
            }
            Tag::Text => {
                let opts = props::text_options(initial, &children.snapshot());
                self.renderer.make_text(&node.live_id, &opts)
            }
            Tag::ScrollBox => {
                let mut opts = props::scrollbox_options(initial);
                self.renderer.make_scrollbox(&node.live_id, &mut opts)
            }
            Tag::Custom(name) => {
                extend::create_custom(&mut self.renderer, name, initial, &node.live_id)
                    .unwrap_or_else(|e| panic!("{e}"))
            }
            Tag::Group | Tag::Portal => self.make_group(&node.live_id),
            // span/text-node elements fold into their text parent's
            // content (props.rs::gather_text_children); reaching here
            // means an orphan text node — the upstream error
            // (reconciler.ts:80-89)
            Tag::Span(_) | Tag::TextNode => {
                panic!(
                    "Orphan text error: \"{}\" must have a <text> as a parent",
                    node.key
                )
            }
        }
    }

    fn make_group(&mut self, id: &str) -> Renderable {
        Renderable::new(
            &mut self.renderer.tree,
            id,
            LayoutOptions::default(),
            Dimension::Auto,
            Dimension::Auto,
            Content::Group,
            None,
        )
    }

    /// `setProperty`'s `on:`/onChange/… routing (reconciler.ts:231-239,
    /// 272-334): event props live in the node's registry.
    fn bind_events(&mut self, node: &mut ShadowNode, props: &[(String, Prop)]) {
        for (name, prop) in props {
            let v = prop.read();
            if let PropValue::Event(cb) = v {
                if let Some(event) = canonical_event_name(name) {
                    node.events.entry(event).or_default().push(cb);
                    self.log(format!("Event bound: {} on {}", name, node.key));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // patches
    // ------------------------------------------------------------------

    /// Apply queued signal patches in fire order, then render one
    /// frame (upstream: effects run synchronously per signal write;
    /// `requestRender` batches the frame — flush is the port's batch
    /// boundary).
    pub fn flush(&mut self) {
        let mut guard = 0;
        loop {
            let batch: Vec<PendingPatch> = std::mem::take(&mut self.inbox.borrow_mut());
            if batch.is_empty() {
                break;
            }
            for patch in batch {
                patch(self);
            }
            guard += 1;
            if guard > 16 {
                break;
            }
        }
        self.renderer.render();
    }

    /// Patch ONE prop on a node (the dynamic-prop render effect body:
    /// `setProperty(node, name, value, prev)` when changed,
    /// universal.js:202-204's equality skip).
    fn patch_prop(&mut self, key: &str, name: &str, value: PropValue) {
        let prev = self
            .root
            .find(key)
            .and_then(|n| props::prop_get(&n.props, name).cloned());
        if prev.as_ref() == Some(&value) {
            return;
        }
        self.set_property(key, name, value, prev);
    }

    /// `setProperty(node, name, value, prev)` — the reconciler.ts:230
    /// switch, adapted to the ported renderable kinds.
    fn set_property(&mut self, key: &str, name: &str, value: PropValue, prev: Option<PropValue>) {
        // on:* events (reconciler.ts:231-239)
        if let Some(event) = name.strip_prefix("on:") {
            self.swap_event(key, event, &value, &prev);
            if let Some(n) = self.root.find_mut(key) {
                props::prop_set(&mut n.props, name, value);
            }
            return;
        }
        // onChange/onInput/onSubmit/onSelect (reconciler.ts:272-334):
        // canonical per-kind events; the ported kinds have none of
        // Input/Select/TabSelect, so only custom elements that emit
        // these names receive them (`if (!event) break` upstream).
        if matches!(name, "onChange" | "onInput" | "onSubmit" | "onSelect") {
            if let Some(event) = canonical_event_name(name) {
                self.swap_event(key, &event, &value, &prev);
                if let Some(n) = self.root.find_mut(key) {
                    props::prop_set(&mut n.props, name, value);
                }
            }
            return;
        }
        match name {
            // node.id = value (reconciler.ts:260-263)
            "id" => {
                if let PropValue::Str(new_id) = &value {
                    let old = self.root.find(key).map(|n| n.live_id.clone());
                    if let Some(old) = old {
                        if self.renderer.rename_node(&old, new_id) {
                            self.log(format!("Id mapped {old} = {new_id}"));
                            if let Some(n) = self.root.find_mut(key) {
                                n.live_id = new_id.clone();
                                if n.children_parent_live == old {
                                    n.children_parent_live = new_id.clone();
                                }
                                props::prop_set(&mut n.props, name, value);
                            }
                        }
                    }
                }
            }
            // focus()/blur() (reconciler.ts:264-270) — focus
            // infrastructure belongs to the Input/Select kinds (not
            // ported); recorded, deferred.
            "focused" => {
                let on = matches!(value, PropValue::Bool(true));
                self.log(format!(
                    "focus {}: {}",
                    if on { "()" } else { "→ blur()" },
                    self.root
                        .find(key)
                        .map_or_else(|| "?".into(), |n| n.live_id.clone())
                ));
                if let Some(n) = self.root.find_mut(key) {
                    props::prop_set(&mut n.props, name, value);
                }
            }
            // scroll setters (ScrollBox.ts:168-171 script mutations)
            "scrollTop" | "scrollLeft" => {
                if let PropValue::Num(v) = value {
                    let live_id = self.root.find(key).map(|n| n.live_id.clone());
                    if let Some(live_id) = live_id {
                        if name == "scrollTop" {
                            self.renderer.set_scroll_top(&live_id, v);
                        } else {
                            self.renderer.set_scroll_left(&live_id, v);
                        }
                        if let Some(n) = self.root.find_mut(key) {
                            props::prop_set(&mut n.props, name, PropValue::Num(v));
                        }
                    }
                }
            }
            // style bag: per-sub-prop diff (reconciler.ts:335-354) —
            // only changed sub-props apply
            "style" => {
                if let PropValue::Style(entries) = &value {
                    let prev_style = prev.and_then(|p| match p {
                        PropValue::Style(e) => Some(e),
                        _ => None,
                    });
                    for (sub, v) in entries {
                        let sub_prev = prev_style
                            .as_ref()
                            .and_then(|e| e.iter().find(|(k, _)| k == sub).map(|(_, v)| v.clone()));
                        if sub_prev.as_ref() == Some(v) {
                            continue;
                        }
                        self.apply_data_prop(key, sub, v.clone());
                    }
                }
                if let Some(n) = self.root.find_mut(key) {
                    props::prop_set(&mut n.props, name, value);
                }
            }
            // data props: content/fg/bg/layout/… (reconciler.ts:355-366)
            _ => self.apply_data_prop(key, name, value),
        }
    }

    /// A data prop: record into the node's prop map and rebuild the
    /// renderable state from the full map (field-identical to
    /// upstream's single-field assignment — see props.rs).
    fn apply_data_prop(&mut self, key: &str, name: &str, value: PropValue) {
        let Some(node) = self.root.find(key) else {
            return;
        };
        let mut new_props = node.props.clone();
        props::prop_set(&mut new_props, name, value.clone());
        let live_id = node.live_id.clone();
        let tag = node.tag.clone();
        let last_children = node.last_children.clone();
        match tag {
            Tag::Text => {
                props::rebuild_text(&mut self.renderer, &live_id, &new_props, &last_children);
            }
            Tag::Box => {
                props::rebuild_box(&mut self.renderer, &live_id, &new_props);
            }
            Tag::ScrollBox => {
                props::rebuild_scrollbox(&mut self.renderer, &live_id, &new_props);
            }
            Tag::Custom(ref custom) => {
                let prev = node.props.clone();
                extend::patch_custom(&mut self.renderer, custom, &live_id, &new_props, &prev);
            }
            _ => {}
        }
        if let Some(n) = self.root.find_mut(key) {
            n.props = new_props;
        }
    }

    fn swap_event(&mut self, key: &str, event: &str, value: &PropValue, prev: &Option<PropValue>) {
        // `if (prev) node.off(eventName, prev)` (reconciler.ts:236-238)
        if let Some(PropValue::Event(old)) = prev {
            if let Some(n) = self.root.find_mut(key) {
                if let Some(list) = n.events.get_mut(event) {
                    list.retain(|cb| !Rc::ptr_eq(cb, old));
                }
            }
        }
        // `if (value) node.on(eventName, value)` (:233-234)
        if let PropValue::Event(cb) = value {
            if let Some(n) = self.root.find_mut(key) {
                n.events
                    .entry(event.to_owned())
                    .or_default()
                    .push(cb.clone());
            }
        }
    }

    /// Trigger a node event (host/tests — the renderer's event
    /// emission seam). Handlers in registration order.
    pub fn trigger_event(&mut self, key: &str, event: &str, payload: EventData) {
        let handlers = self
            .root
            .find(key)
            .and_then(|n| n.events.get(event))
            .cloned()
            .unwrap_or_default();
        for cb in handlers {
            cb(&payload);
        }
    }

    /// Observability: registered event names on a node.
    #[must_use]
    pub fn events_of(&self, key: &str) -> Vec<String> {
        self.root
            .find(key)
            .map(|n| n.events.keys().cloned().collect())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // children reconciliation (universal.js:108-163)
    // ------------------------------------------------------------------

    /// Reconcile `parent_key`'s children to `b` — the keyed port of
    /// `reconcileArrays`. The work list `a` holds the ORIGINAL child
    /// keys in original order (upstream's `a` array of node instances
    /// — immutable; mutations happen on the live tree); `live` mirrors
    /// the live child order. Every op applies to both trees (the ops
    /// upstream performs through insertNode/removeNode/replaceNode).
    fn reconcile_children(&mut self, parent_key: &str, b: Vec<Element>) {
        if self.root.find(parent_key).is_none() {
            return;
        }
        let parent_live = self
            .root
            .find(parent_key)
            .expect("checked")
            .children_parent_live
            .clone();
        let mut live = std::mem::take(&mut self.root.find_mut(parent_key).unwrap().children);
        // upstream's `a` — the old children, immutable order
        let mut a: Vec<String> = live.iter().map(|n| n.key.clone()).collect();

        let b_length = b.len();
        let mut a_end = a.len();
        let mut b_end = b_length;
        let mut a_start = 0usize;
        let mut b_start = 0usize;
        // `after` — the live position following the last original
        // child (universal.js:114), captured before any mutation.
        let after_pos: usize = live
            .last()
            .and_then(|last| self.live_pos(&parent_live, &last.live_id))
            .map_or(usize::MAX, |p| p + 1);
        let mut map: Option<HashMap<String, usize>> = None;

        while a_start < a_end || b_start < b_end {
            // prefix match (universal.js:117-120)
            if a_start < a_end && b_start < b_end && a[a_start] == b[b_start].key() {
                let key = a[a_start].clone();
                let el = b[b_start].clone();
                self.patch_matched(&key, el);
                a_start += 1;
                b_start += 1;
                continue;
            }
            // suffix match (universal.js:122-125)
            if a_start < a_end && b_start < b_end && a[a_end - 1] == b[b_end - 1].key() {
                let key = a[a_end - 1].clone();
                let el = b[b_end - 1].clone();
                self.patch_matched(&key, el);
                a_end -= 1;
                b_end -= 1;
                continue;
            }
            if a_end == a_start {
                // old run exhausted: insert the new run before the
                // anchor (universal.js:126-128)
                let anchor_pos: usize = if b_end < b_length {
                    if b_start > 0 {
                        // live position AFTER b[b_start-1]'s match
                        let key = b[b_start - 1].key();
                        Self::live_key_pos(&live, &key).map_or(usize::MAX, |p| p + 1)
                    } else {
                        // b[bEnd] with bStart == 0 — the suffix-matched
                        // item already in place
                        let key = b[b_end].key();
                        Self::live_key_pos(&live, &key).unwrap_or(usize::MAX)
                    }
                } else {
                    after_pos
                };
                let mut at = anchor_pos;
                while b_start < b_end {
                    let el = b[b_start].clone();
                    b_start += 1;
                    self.insert_new(parent_key, &parent_live, &mut live, at, el);
                    if at != usize::MAX {
                        at = at.saturating_add(1);
                    }
                }
                continue;
            }
            if b_end == b_start {
                // new run exhausted: remove the old run
                // (universal.js:129-133) — map-matched items spared
                while a_start < a_end {
                    let remove = map.as_ref().is_none_or(|m| !m.contains_key(&a[a_start]));
                    if remove {
                        let key = a[a_start].clone();
                        let Some(pos) = Self::live_key_pos(&live, &key) else {
                            a_start += 1;
                            continue;
                        };
                        self.remove_at(&parent_live, &mut live, pos);
                    }
                    a_start += 1;
                }
                continue;
            }
            // swap (universal.js:134-138): the first old matches the
            // last new and vice versa — two moves instead of replaces
            if a[a_start] == b[b_end - 1].key() && b[b_start].key() == a[a_end - 1] {
                // node = getNextSibling(a[--aEnd])
                a_end -= 1;
                let back_key = a[a_end].clone(); // matches b[b_start]
                let front_key = a[a_start].clone(); // matches b[b_end-1]
                                                    // anchor positions BEFORE the moves (upstream reads
                                                    // getNextSibling live at each step)
                let anchor_front =
                    Self::live_key_pos(&live, &front_key).map_or(usize::MAX, |p| p + 1);
                let anchor_tail =
                    Self::live_key_pos(&live, &back_key).map_or(usize::MAX, |p| p + 1);
                self.move_within(&parent_live, &mut live, &back_key, anchor_front);
                self.move_within(&parent_live, &mut live, &front_key, anchor_tail);
                // a[aEnd] = b[bEnd] — the front item's node now occupies
                // the vacated tail slot of the work list
                a[a_end] = front_key;
                a_start += 1;
                b_start += 1;
                b_end -= 1;
                continue;
            }
            // map + sequence heuristic (universal.js:139-161)
            if map.is_none() {
                let mut m = HashMap::new();
                for (i, item) in b.iter().enumerate().take(b_end).skip(b_start) {
                    m.insert(item.key(), i);
                }
                map = Some(m);
            }
            let a_key = a[a_start].clone();
            let index = map.as_ref().and_then(|m| m.get(&a_key).copied());
            match index {
                None => {
                    // not in the new list — remove (universal.js:160)
                    if let Some(pos) = Self::live_key_pos(&live, &a_key) {
                        self.remove_at(&parent_live, &mut live, pos);
                    }
                    a_start += 1;
                }
                Some(index) => {
                    if b_start < index && index < b_end {
                        // count the consecutive a-run mapping into
                        // b[index..] (universal.js:148-154)
                        let m = map.as_ref().expect("built");
                        let mut sequence = 1usize;
                        let mut i = a_start;
                        loop {
                            i += 1;
                            if i >= a_end || i >= b_end {
                                break;
                            }
                            match m.get(&a[i]).copied() {
                                Some(t) if t == index + sequence => sequence += 1,
                                _ => break,
                            }
                        }
                        if sequence > index - b_start {
                            // insert the b-run before a[aStart]
                            // (universal.js:155-157 — aStart does NOT
                            // advance; the item re-matches via prefix)
                            let anchor_pos =
                                Self::live_key_pos(&live, &a_key).unwrap_or(usize::MAX);
                            let mut at = anchor_pos;
                            while b_start < index {
                                let el = b[b_start].clone();
                                b_start += 1;
                                self.insert_new(parent_key, &parent_live, &mut live, at, el);
                                if at != usize::MAX {
                                    at = at.saturating_add(1);
                                }
                            }
                        } else {
                            // replace (universal.js:158 — aStart advances)
                            let el = b[b_start].clone();
                            b_start += 1;
                            a_start += 1;
                            if let Some(pos) = Self::live_key_pos(&live, &a_key) {
                                self.replace_at(parent_key, &parent_live, &mut live, pos, el);
                            } else {
                                self.insert_new(
                                    parent_key,
                                    &parent_live,
                                    &mut live,
                                    usize::MAX,
                                    el,
                                );
                            }
                        }
                    } else {
                        // match outside the middle window — stays in
                        // place (universal.js:159)
                        a_start += 1;
                    }
                }
            }
        }

        if let Some(parent) = self.root.find_mut(parent_key) {
            parent.children = live;
            parent.last_children = b;
        }
    }

    /// The live position (child index) of `live_id` under
    /// `parent_live` (upstream `children.indexOf(anchor)`,
    /// reconciler.ts:105).
    fn live_pos(&self, parent_live: &str, live_id: &str) -> Option<usize> {
        self.renderer
            .child_ids(parent_live)?
            .iter()
            .position(|id| id == live_id)
    }

    /// Position of a shadow key inside the live-order working list.
    fn live_key_pos(live: &[ShadowNode], key: &str) -> Option<usize> {
        live.iter().position(|n| n.key == key)
    }

    /// Insert an element at `pos` (usize::MAX = append). A key that
    /// already exists in the working list is a MOVE (upstream
    /// `insertNode(parent, node, anchor)` re-adds an existing child —
    /// Renderable.ts:1225-1240 moves it); otherwise a fresh mount.
    /// Callers inserting a RUN before one anchor increment `pos` per
    /// item (each insert pushes the anchor right).
    fn insert_new(
        &mut self,
        parent_key: &str,
        parent_live: &str,
        live: &mut Vec<ShadowNode>,
        pos: usize,
        element: Element,
    ) {
        let key = element.key();
        if Self::live_key_pos(live, &key).is_some() {
            self.move_within(parent_live, live, &key, pos);
            return;
        }
        let count_before = self.renderer.child_ids(parent_live).map_or(0, |v| v.len());
        let at = pos.min(count_before);
        let key = self.mount_element(parent_key, at, element);
        if let Some(node) = self.take_shadow_child(parent_key, &key) {
            let at = pos.min(live.len());
            live.insert(at, node);
        }
    }

    /// Detach a shadow child by key (splice helper).
    fn take_shadow_child(&mut self, parent_key: &str, key: &str) -> Option<ShadowNode> {
        let parent = self.root.find_mut(parent_key)?;
        let pos = parent.children.iter().position(|n| n.key == key)?;
        Some(parent.children.remove(pos))
    }

    /// Remove the working-list item at `pos`: live unmount + shadow
    /// detach + cleanup (upstream `_removeNode` + owner disposal).
    fn remove_at(&mut self, parent_live: &str, live: &mut Vec<ShadowNode>, pos: usize) {
        let mut node = live.remove(pos);
        self.log(format!(
            "Removing node: {} from parent: {parent_live}",
            node.live_id
        ));
        self.renderer.unmount_child(parent_live, &node.live_id);
        self.run_cleanups(&mut node);
    }

    /// `replaceNode(new, old)` (universal.js:188-191): insert new at
    /// the old's position, remove the old.
    fn replace_at(
        &mut self,
        parent_key: &str,
        parent_live: &str,
        live: &mut Vec<ShadowNode>,
        pos: usize,
        element: Element,
    ) {
        let count_before = self.renderer.child_ids(parent_live).map_or(0, |v| v.len());
        let at = pos.min(count_before);
        let key = self.mount_element(parent_key, at, element);
        if let Some(node) = self.take_shadow_child(parent_key, &key) {
            live[pos] = node; // the new node takes the old's position
        }
        // the displaced old node sits at pos+1 now
        if pos + 1 < live.len() {
            let mut old = live.remove(pos + 1);
            self.log(format!(
                "Removing node: {} from parent: {parent_live}",
                old.live_id
            ));
            self.renderer.unmount_child(parent_live, &old.live_id);
            self.run_cleanups(&mut old);
        }
    }

    /// Move a node (by key) to `pos` (usize::MAX = append): detach +
    /// re-attach the INTACT subtree on both trees (the yoga
    /// removeChild/insertChild pair upstream's re-add runs,
    /// Renderable.ts:1225-1240).
    fn move_within(
        &mut self,
        parent_live: &str,
        live: &mut Vec<ShadowNode>,
        key: &str,
        pos: usize,
    ) {
        let Some(from) = Self::live_key_pos(live, key) else {
            return;
        };
        if pos != usize::MAX && from == pos {
            return;
        }
        let live_id = live[from].live_id.clone();
        let Some(renderable) = self.renderer.detach_child(parent_live, &live_id) else {
            return;
        };
        let at = if pos == usize::MAX {
            self.renderer.child_ids(parent_live).map_or(0, |v| v.len())
        } else {
            pos
        };
        let ok = self.renderer.insert_child_at(parent_live, at, renderable);
        debug_assert!(ok);
        self.log(format!(
            "Moving node: {live_id} to index {at} in {parent_live}"
        ));
        let node = live.remove(from);
        let at = if pos == usize::MAX {
            live.len()
        } else {
            pos.min(live.len())
        };
        live.insert(at, node);
    }

    /// Patch a key-matched node in place: minimal prop diff +
    /// recursive children reconcile + dynamic-children rebinding.
    fn patch_matched(&mut self, key: &str, element: Element) {
        for (name, prop) in &element.props {
            let v = prop.read();
            self.patch_prop(key, name, v);
        }
        match element.children {
            Children::None => self.reconcile_children(key, Vec::new()),
            Children::Static(kids) => self.reconcile_children(key, kids),
            Children::Dyn(src) => {
                let kids = src.read();
                self.reconcile_children(key, kids);
                // the element tree is a fresh value — rebind its
                // DynChildren source in place of the mounted one
                // (upstream: the accessor closure is stable; value
                // trees rebuild it)
                if let Some(node) = self.root.find_mut(key) {
                    for u in std::mem::take(&mut node.unsubscribers) {
                        u();
                    }
                }
                self.bind_children_source(key, &src);
            }
        }
    }

    /// Unmount a node and its subtree: live remove + teardown
    /// (cleanups: children first, then own, LIFO — solid owner
    /// disposal; effects detach via unsubscribers).
    fn run_cleanups(&mut self, node: &mut ShadowNode) {
        for child in &mut node.children {
            self.run_cleanups(child);
        }
        for u in std::mem::take(&mut node.unsubscribers).into_iter().rev() {
            u();
        }
        // custom element teardown (extend()'s onCleanup)
        if let Tag::Custom(name) = &node.tag {
            let props = node.props.clone();
            let live_id = node.live_id.clone();
            extend::cleanup_custom(name, &props, &live_id);
        }
        for c in std::mem::take(&mut node.cleanups).into_iter().rev() {
            c();
        }
    }

    /// Unmount a subtree by key (host-facing `_removeNode`).
    pub fn unmount(&mut self, key: &str) -> bool {
        let Some(parent_key) = self.find_parent_of("__root__", key) else {
            return false;
        };
        let (parent_live, pos) = {
            let Some(parent) = self.root.find(&parent_key) else {
                return false;
            };
            let live = parent.children_parent_live.clone();
            let pos = parent.children.iter().position(|n| n.key == key);
            (live, pos)
        };
        let Some(pos) = pos else { return false };
        let mut node = {
            let parent = self.root.find_mut(&parent_key).unwrap();
            parent.children.remove(pos)
        };
        if node.tag == Tag::Portal {
            // the container goes with the portal (extras.ts:39-48)
            let container = node.children_parent_live.clone();
            let target = self.portal_target_of(&node);
            self.log(format!(
                "Removing portal container: {container} from {target}"
            ));
            self.renderer.unmount_child(&target, &container);
        }
        self.log(format!(
            "Removing node: {} from parent: {parent_live}",
            node.live_id
        ));
        self.renderer.unmount_child(&parent_live, &node.live_id);
        self.run_cleanups(&mut node);
        true
    }

    fn portal_target_of(&self, node: &ShadowNode) -> String {
        props::prop_get(&node.props, "mount")
            .and_then(|v| match v {
                PropValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "__root__".to_owned())
    }

    fn find_parent_of(&self, from: &str, key: &str) -> Option<String> {
        let node = self.root.find(from)?;
        for c in &node.children {
            if c.key == key {
                return Some(from.to_owned());
            }
            if let Some(found) = self.find_parent_of(&c.key, key) {
                return Some(found);
            }
        }
        None
    }
}

/// onChange/onInput/onSubmit/onSelect → the per-kind event names
/// (reconciler.ts:272-334). The ported kinds have no Select/Input, so
/// these stay generic names for custom elements that declare them.
fn canonical_event_name(prop: &str) -> Option<String> {
    match prop {
        "onChange" => Some("change".into()),
        "onInput" => Some("input".into()),
        "onSubmit" => Some("enter".into()),
        "onSelect" => Some("itemSelected".into()),
        other => other.strip_prefix("on:").map(str::to_owned),
    }
}
