//! The component tree — the Rust equivalent of one JSX element tree.
//!
//! Upstream mapping (citations are `packages/solid` @ 0.5.14):
//! * an `Element` ≈ one JSX intrinsic element produced by
//!   `jsx(type, props)` (jsx-runtime.ts:41-55 → `createElement(tag)` +
//!   `spread(el, props)`).
//! * `key` is the reconciler identity. Upstream keys children by node
//!   INSTANCE identity (universal.js:108-163 `reconcileArrays`
//!   `a[aStart] === b[bStart]`); Rust rebuilds value trees, so identity
//!   is explicit: the caller's key, else the auto id (`getNextId`,
//!   utils/id-counter.ts:3-12 — `${tag}-${n}`, first id is `tag-1`).
//! * `Children::Dyn` ≈ the compiled control-flow expressions:
//!   `Show`/`For`/`insert(parent, accessor)` (universal.js:19-23 — a
//!   render effect re-runs the accessor and re-reconciles children).

use crate::reactive::{Mapped, Reactive};
use opentui_core::styled_text::StyledText;
use std::rc::Rc;

/// The intrinsic element catalogue (upstream `baseComponents`,
/// elements/catalogue.ts:92-115) — restricted to the renderables the
/// core port has, plus the reconciler's own glue kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    /// `<box>` — BoxRenderable (catalogue.ts:93).
    Box,
    /// `<text>` — TextRenderable (catalogue.ts:94).
    Text,
    /// `<scrollbox>` — ScrollBoxRenderable (catalogue.ts:100).
    ScrollBox,
    /// `<span>`/`<b>`/`<strong>`/`<i>`/`<em>`/`<u>`/`<a>`/`<br>` — the
    /// text-node keys (catalogue.ts:31, 107-114). Text-node children of
    /// a `<text>` element (the "orphan text" rule, reconciler.ts:80-89).
    Span(SpanKind),
    /// A raw text child — `createTextNode` (reconciler.ts:143-153;
    /// the JSX compiler emits these for text runs between tags).
    TextNode,
    /// A registered custom element (`extend()`, catalogue.ts:133-135).
    Custom(String),
    /// `<Portal>` (extras.ts:14-55): children render under `mount`.
    Portal,
    /// A layout marker (upstream `SlotRenderable`/`createSlotNode`,
    /// reconciler.ts:155-159) — occupies a position, paints nothing.
    /// Used for Portal anchors and fragments.
    Group,
}

/// The span family (catalogue.ts:31 textNodeKeys + class mapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Plain,     // span
    Bold,      // b | strong
    Italic,    // i | em
    Underline, // u
    Link,      // a (href prop → link)
    Break,     // br (adds "\n")
}

impl Tag {
    /// The id-counter / catalogue key spelling (upstream `getNextId`
    /// is keyed by tag name; custom elements by their registered name).
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Tag::Box => "box",
            Tag::Text => "text",
            Tag::ScrollBox => "scrollbox",
            Tag::Span(SpanKind::Plain) => "span",
            Tag::Span(SpanKind::Bold) => "b",
            Tag::Span(SpanKind::Italic) => "i",
            Tag::Span(SpanKind::Underline) => "u",
            Tag::Span(SpanKind::Link) => "a",
            Tag::Span(SpanKind::Break) => "br",
            Tag::TextNode => "text-node",
            Tag::Custom(name) => name,
            Tag::Portal => "portal",
            Tag::Group => "group",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Tag> {
        Some(match name {
            "box" => Tag::Box,
            "text" => Tag::Text,
            "scrollbox" => Tag::ScrollBox,
            "span" => Tag::Span(SpanKind::Plain),
            "b" | "strong" => Tag::Span(SpanKind::Bold),
            "i" | "em" => Tag::Span(SpanKind::Italic),
            "u" => Tag::Span(SpanKind::Underline),
            "a" => Tag::Span(SpanKind::Link),
            "br" => Tag::Span(SpanKind::Break),
            other => Tag::Custom(other.to_owned()),
        })
    }
}

/// One prop value — the typed equivalent of JSX's `any` prop values
/// the reconciler's `setProperty(node, name, value, prev)` receives
/// (reconciler.ts:230). `Style` bags carry the `style` prop's
/// per-sub-prop diffing (reconciler.ts:335-354); `Event` handlers
/// compare by handler identity (upstream compares `prev` references).
#[derive(Clone)]
pub enum PropValue {
    /// `undefined`/`null` — present-but-empty (clears a previous value).
    None,
    Str(String),
    Bool(bool),
    Num(f32),
    /// A color string ("#RRGGBB", "rgb(...)" — parsed on application,
    /// like upstream `parseColor` call sites).
    Color(String),
    /// Text attributes bitmask (`createTextAttributes`, utils.ts:4-29).
    Attrs(u32),
    /// A dimension prop: number or "NN%" (yoga Percent / Points).
    Dimension(String),
    /// `style={{ ... }}` — sub-props diffed individually.
    Style(Vec<(String, PropValue)>),
    /// `content` as styled chunks (upstream `StyledText`).
    Styled(StyledText),
    /// Event handler (`on:*` / onChange/… — reconciler.ts:231-239,
    /// 272-334). Equality = handler identity (Rc ptr), mirroring
    /// upstream's `prev` handler comparison.
    Event(Rc<dyn Fn(&crate::hooks::EventData)>),
    /// `children`-as-text (a string child of `<text>`).
    Text(String),
}

impl std::fmt::Debug for PropValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropValue::Event(cb) => write!(f, "Event({:p})", Rc::as_ptr(cb)),
            PropValue::Style(entries) => {
                write!(f, "Style(")?;
                for (name, v) in entries {
                    write!(f, "{name}:{v:?} ")?;
                }
                write!(f, ")")
            }
            other => write!(f, "{other:?}"),
        }
    }
}

impl PartialEq for PropValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PropValue::Event(a), PropValue::Event(b)) => Rc::ptr_eq(a, b),
            (PropValue::Style(a), PropValue::Style(b)) => a == b,
            (PropValue::None, PropValue::None) => true,
            (PropValue::Str(a), PropValue::Str(b)) => a == b,
            (PropValue::Bool(a), PropValue::Bool(b)) => a == b,
            (PropValue::Num(a), PropValue::Num(b)) => a == b,
            (PropValue::Color(a), PropValue::Color(b)) => a == b,
            (PropValue::Attrs(a), PropValue::Attrs(b)) => a == b,
            (PropValue::Dimension(a), PropValue::Dimension(b)) => a == b,
            (PropValue::Styled(a), PropValue::Styled(b)) => {
                // styled equality by chunk fields (StyledText: deriveless)
                a.chunks == b.chunks
            }
            (PropValue::Text(a), PropValue::Text(b)) => a == b,
            _ => false,
        }
    }
}

impl From<&str> for PropValue {
    fn from(v: &str) -> Self {
        PropValue::Str(v.to_owned())
    }
}
impl From<String> for PropValue {
    fn from(v: String) -> Self {
        PropValue::Str(v)
    }
}
impl From<bool> for PropValue {
    fn from(v: bool) -> Self {
        PropValue::Bool(v)
    }
}
impl From<f32> for PropValue {
    fn from(v: f32) -> Self {
        PropValue::Num(v)
    }
}
impl From<i32> for PropValue {
    fn from(v: i32) -> Self {
        PropValue::Num(v as f32)
    }
}
impl From<u32> for PropValue {
    fn from(v: u32) -> Self {
        PropValue::Num(v as f32)
    }
}
impl From<usize> for PropValue {
    fn from(v: usize) -> Self {
        PropValue::Num(v as f32)
    }
}

/// One prop — static, or a reactive source. Dynamic props re-patch in
/// place when their signal fires (the builder form of a JSX getter
/// prop: `spread(el, { get content() { return sig() } })` —
/// universal.js:198-206 reads the getter in a render effect and calls
/// `setProperty` only for changed props).
pub enum Prop {
    Static(PropValue),
    Dyn(Rc<dyn DynPropSource>),
}

impl Clone for Prop {
    fn clone(&self) -> Self {
        match self {
            Prop::Static(v) => Prop::Static(v.clone()),
            Prop::Dyn(src) => Prop::Dyn(Rc::clone(src)),
        }
    }
}

impl Prop {
    /// The current value (dynamic sources are read at mount/patch time).
    #[must_use]
    pub fn read(&self) -> PropValue {
        match self {
            Prop::Static(v) => v.clone(),
            Prop::Dyn(src) => src.read(),
        }
    }

    #[must_use]
    pub fn is_dyn(&self) -> bool {
        matches!(self, Prop::Dyn(_))
    }
}

/// A boxed event/patch callback (shared shape across the reactive
/// seams).
pub type Callback<T> = Box<dyn Fn(&Vec<T>)>;

/// The dynamic-prop source boundary (solid: the getter expression +
/// its effect subscription).
pub trait DynPropSource {
    fn read(&self) -> PropValue;
    fn subscribe(&self, cb: Box<dyn Fn(&PropValue)>) -> Box<dyn Fn()>;
}

/// Adapt any [`Reactive<T>`] with `T: Into<PropValue>` into a dynamic
/// prop source (upstream: any signal reads directly inside the spread
/// effect).
impl<S, U, F> DynPropSource for Mapped<S, U, PropValue, F>
where
    S: Reactive<U> + 'static,
    F: Fn(&U) -> PropValue + 'static,
{
    fn read(&self) -> PropValue {
        Reactive::get(self)
    }

    fn subscribe(&self, cb: Box<dyn Fn(&PropValue)>) -> Box<dyn Fn()> {
        Reactive::subscribe(self, cb)
    }
}

/// Wrap a typed reactive source as a dynamic prop.
///
/// ```ignore
/// let content = Signal::new("hello".to_owned());
/// text().content_dyn(dyn_prop(&content, |s| PropValue::Str(s.clone())))
/// ```
#[must_use]
pub fn dyn_prop<S, T, F>(source: S, f: F) -> Rc<dyn DynPropSource>
where
    S: Reactive<T> + 'static,
    T: Clone + 'static,
    F: Fn(&T) -> PropValue + 'static,
{
    Rc::new(Mapped::new(source, f))
}

/// Convenience for already-`PropValue`-shaped signals.
#[must_use]
pub fn dyn_value<S: Reactive<PropValue> + 'static>(source: S) -> Rc<dyn DynPropSource> {
    dyn_prop(source, |v: &PropValue| v.clone())
}

/// One component-tree node (a JSX element).
#[derive(Clone)]
pub struct Element {
    pub tag: Tag,
    /// Reconciler identity — see the module doc on keying.
    pub key: Option<String>,
    /// Props in declaration order (later wins on duplicate names, like
    /// JSX's last-prop-wins object literal).
    pub props: Vec<(String, Prop)>,
    pub children: Children,
    /// `onCleanup(fn)` handlers (solid-js onCleanup registered inside
    /// the component body; run on unmount — children first, LIFO).
    pub cleanups: Vec<Rc<dyn Fn()>>,
}

/// The children of an element — static JSX children, or a reactive
/// expression (the compiled output of `Show`/`For`/`{expr}` children).
pub enum Children {
    None,
    Static(Vec<Element>),
    Dyn(Rc<dyn DynChildren>),
}

impl Clone for Children {
    fn clone(&self) -> Self {
        match self {
            Children::None => Children::None,
            Children::Static(v) => Children::Static(v.clone()),
            Children::Dyn(src) => Children::Dyn(Rc::clone(src)),
        }
    }
}

impl Children {
    /// Static children snapshot (dynamic sources are read once — used
    /// by tests/simple trees).
    #[must_use]
    pub fn snapshot(&self) -> Vec<Element> {
        match self {
            Children::None => Vec::new(),
            Children::Static(v) => v.clone(),
            Children::Dyn(src) => src.read(),
        }
    }
}

/// The reactive-children boundary: produces the current element list;
/// subscribers re-reconcile on change (universal.js:22-23 `insert`).
pub trait DynChildren {
    fn read(&self) -> Vec<Element>;
    fn subscribe(&self, cb: ElementsCallback) -> Unsubscribe;
}

/// A children subscription callback.
pub type ElementsCallback = Box<dyn Fn(&Vec<Element>)>;
/// A ForItems subscription callback.
pub type ForItemsCallback = Box<dyn Fn(&Vec<ForItem>)>;
/// A subscription dispose handle (upstream: effect disposal).
pub type Unsubscribe = Box<dyn Fn()>;
/// The For render fn shape.
pub type ForRender = Rc<dyn Fn(&ForItem, usize) -> Element>;

/// `<Show when={cond}>{children}</Show>` (solid-js Show via the
/// reconciler's `insert`): children mounted while `when` reads true,
/// unmounted when false.
pub struct ShowChildren {
    pub when: Rc<dyn Reactive<bool>>,
    pub render: Rc<dyn Fn() -> Vec<Element>>,
}

impl DynChildren for ShowChildren {
    fn read(&self) -> Vec<Element> {
        if self.when.get() {
            (self.render)()
        } else {
            Vec::new()
        }
    }

    fn subscribe(&self, cb: Box<dyn Fn(&Vec<Element>)>) -> Box<dyn Fn()> {
        let render = Rc::clone(&self.render);
        self.when.subscribe(Box::new(move |&on| {
            let els = if on { render() } else { Vec::new() };
            cb(&els);
        }))
    }
}

/// One item of a keyed list (upstream `<For each>` item identity).
#[derive(Clone, Debug, PartialEq)]
pub struct ForItem {
    /// The item key (upstream: the item's reference identity; here:
    /// explicit — `key_of(item)`).
    pub key: String,
    /// The item payload handed to the render fn.
    pub value: PropValue,
}

/// The `<For each={list}>` source boundary.
pub trait ForSource {
    fn items(&self) -> Vec<ForItem>;
    fn subscribe(&self, cb: ForItemsCallback) -> Unsubscribe;
}

/// The For construct: keyed children over a reactive list
/// (solid-js `For`: items keep their nodes across reorders —
/// key-matched, moved, inserted, removed). The rendered element's key
/// is FORCED to the item key (upstream: item identity IS node
/// identity; mapArray keys its node map by the item).
pub struct ForChildren {
    pub source: Rc<dyn ForSource>,
    pub render: ForRender,
}

impl ForChildren {
    fn render_keyed(&self, item: &ForItem, i: usize) -> Element {
        let mut el = (self.render)(item, i);
        el.key = Some(item.key.clone());
        el
    }
}

impl DynChildren for ForChildren {
    fn read(&self) -> Vec<Element> {
        self.source
            .items()
            .iter()
            .enumerate()
            .map(|(i, item)| self.render_keyed(item, i))
            .collect()
    }

    fn subscribe(&self, cb: Box<dyn Fn(&Vec<Element>)>) -> Box<dyn Fn()> {
        let render = Rc::clone(&self.render);
        self.source.subscribe(Box::new(move |items: &Vec<ForItem>| {
            let els: Vec<Element> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let mut el = render(item, i);
                    el.key = Some(item.key.clone());
                    el
                })
                .collect();
            cb(&els);
        }))
    }
}

/// A plain reactive children expression: `children: () => els`
/// (upstream `insert(parent, accessor)` over an arbitrary accessor).
pub struct ExprChildren<S: Reactive<Vec<Element>>> {
    pub source: S,
}

impl<S: Reactive<Vec<Element>>> DynChildren for ExprChildren<S> {
    fn read(&self) -> Vec<Element> {
        self.source.get()
    }

    fn subscribe(&self, cb: Box<dyn Fn(&Vec<Element>)>) -> Box<dyn Fn()> {
        self.source.subscribe(cb)
    }
}

impl Element {
    /// Construct without an explicit key (auto ids assign at mount —
    /// upstream `getNextId`, reconciler.ts:195). Unkeyed elements in
    /// dynamic lists use replace semantics (never reuse).
    #[must_use]
    pub fn new(tag: Tag) -> Self {
        Element {
            tag,
            key: None,
            props: Vec::new(),
            children: Children::None,
            cleanups: Vec::new(),
        }
    }

    /// The reconciler identity (upstream: node identity). The
    /// fallback only fires for literally-constructed elements without
    /// a key — the builder path always assigns one.
    #[must_use]
    pub fn key(&self) -> String {
        self.key
            .clone()
            .unwrap_or_else(|| format!("\0{}", self.tag.name()))
    }

    /// Upstream prop-order `id` (used as key when no explicit key).
    #[must_use]
    pub fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
}
