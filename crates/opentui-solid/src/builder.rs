//! The builder API — the Rust 1:1 of TSX JSX structure.
//!
//! Every TSX element tree has an equivalent builder tree (this is the
//! surface the opencode-rust TUI migration authors against):
//!
//! ```text
//! <box flexDirection="row" paddingLeft={2} backgroundColor="#112233">
//!   <text content="hello" fg="#00FFAA" />
//!   {show(cond, () => <text content="opt" />)}
//! </box>
//! ```
//!
//! ```ignore
//! box_! {
//!     flexDirection: "row", paddingLeft: 2, backgroundColor: "#112233",
//!     children: [
//!         text_! { content: "hello", fg: "#00FFAA" },
//!     ],
//! }.children_dyn(show(cond_signal, || vec![text_! { content: "opt" }]))
//! ```
//!
//! Two equivalent forms:
//! * the **macro form** (`box_! { … }`) — struct-literal syntax with
//!   the upstream camelCase prop names (`flexDirection`, `paddingLeft`
//!   …), 1:1 with the TSX attribute list;
//! * the **chain form** (`box_().flex_direction("row").padding_left(2.0)`)
//!   — snake_case methods, Rust-idiomatic.
//!
//! Dynamic props take reactive sources (`paddingLeft: dyn_(sig)` —
//! the JSX getter-prop form `paddingLeft={sig()}`; upstream spread's
//! render effects, universal.js:198-206).

use crate::element::{
    Children, DynChildren, DynPropSource, Element, ExprChildren, ForChildren, ForItem, ForSource,
    Prop, PropValue, ShowChildren, SpanKind, Tag,
};
use crate::reactive::{Mapped, Reactive};
use std::rc::Rc;

/// What an element property can be: a value, a dynamic source, or
/// children (the `children` key).
pub enum PropOrChildren {
    Prop(Prop),
    Children(Children),
}

impl From<PropValue> for PropOrChildren {
    fn from(v: PropValue) -> Self {
        PropOrChildren::Prop(Prop::Static(v))
    }
}
impl From<&str> for PropOrChildren {
    fn from(v: &str) -> Self {
        PropValue::Str(v.to_owned()).into()
    }
}
impl From<String> for PropOrChildren {
    fn from(v: String) -> Self {
        PropValue::Str(v).into()
    }
}
impl From<bool> for PropOrChildren {
    fn from(v: bool) -> Self {
        PropValue::Bool(v).into()
    }
}
impl From<f32> for PropOrChildren {
    fn from(v: f32) -> Self {
        PropValue::Num(v).into()
    }
}
impl From<f64> for PropOrChildren {
    fn from(v: f64) -> Self {
        PropValue::Num(v as f32).into()
    }
}
impl From<i32> for PropOrChildren {
    fn from(v: i32) -> Self {
        PropValue::Num(v as f32).into()
    }
}
impl From<u32> for PropOrChildren {
    fn from(v: u32) -> Self {
        PropValue::Num(v as f32).into()
    }
}
impl From<usize> for PropOrChildren {
    fn from(v: usize) -> Self {
        PropValue::Num(v as f32).into()
    }
}
impl From<Rc<dyn DynPropSource>> for PropOrChildren {
    fn from(v: Rc<dyn DynPropSource>) -> Self {
        PropOrChildren::Prop(Prop::Dyn(v))
    }
}
impl From<Element> for PropOrChildren {
    fn from(v: Element) -> Self {
        PropOrChildren::Children(Children::Static(vec![v]))
    }
}
impl From<Vec<Element>> for PropOrChildren {
    fn from(v: Vec<Element>) -> Self {
        PropOrChildren::Children(Children::Static(v))
    }
}
impl From<Vec<ElementBuilder>> for PropOrChildren {
    fn from(v: Vec<ElementBuilder>) -> Self {
        PropOrChildren::Children(Children::Static(
            v.into_iter().map(ElementBuilder::into_element).collect(),
        ))
    }
}
impl<const N: usize> From<[ElementBuilder; N]> for PropOrChildren {
    fn from(v: [ElementBuilder; N]) -> Self {
        PropOrChildren::Children(Children::Static(
            v.into_iter().map(ElementBuilder::into_element).collect(),
        ))
    }
}
impl<const N: usize> From<[Element; N]> for PropOrChildren {
    fn from(v: [Element; N]) -> Self {
        PropOrChildren::Children(Children::Static(v.into_iter().collect()))
    }
}
impl From<Children> for PropOrChildren {
    fn from(v: Children) -> Self {
        PropOrChildren::Children(v)
    }
}
impl From<Rc<dyn DynChildren>> for PropOrChildren {
    fn from(v: Rc<dyn DynChildren>) -> Self {
        PropOrChildren::Children(Children::Dyn(v))
    }
}
impl From<ShowChildren> for PropOrChildren {
    fn from(v: ShowChildren) -> Self {
        PropOrChildren::Children(Children::Dyn(Rc::new(v)))
    }
}
impl From<ForChildren> for PropOrChildren {
    fn from(v: ForChildren) -> Self {
        PropOrChildren::Children(Children::Dyn(Rc::new(v)))
    }
}

/// The element builder — chain form of one JSX element.
pub struct ElementBuilder {
    element: Element,
}

impl ElementBuilder {
    #[must_use]
    pub fn new(tag: Tag) -> Self {
        ElementBuilder {
            element: Element::new(tag),
        }
    }

    /// An explicit identity key (upstream: JSX `key`; our reconciler
    /// identity).
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.element.key = Some(key.into());
        self
    }

    /// A static prop by its upstream name (`flexDirection`, …).
    #[must_use]
    pub fn prop(mut self, name: &str, value: impl Into<PropValue>) -> Self {
        self.element
            .props
            .push((name.to_owned(), Prop::Static(value.into())));
        self
    }

    /// A dynamic prop by its upstream name (a JSX getter prop).
    #[must_use]
    pub fn dyn_prop(mut self, name: &str, source: Rc<dyn DynPropSource>) -> Self {
        self.element
            .props
            .push((name.to_owned(), Prop::Dyn(source)));
        self
    }

    /// Static children.
    #[must_use]
    pub fn children(mut self, kids: impl Into<Children>) -> Self {
        self.element.children = kids.into();
        self
    }

    /// One static child.
    #[must_use]
    pub fn child(self, kid: impl Into<ElementBuilder>) -> Self {
        let kid: ElementBuilder = kid.into();
        self.children(Children::Static(vec![kid.into_element()]))
    }

    /// Dynamic children (`Show`/`For`/expression).
    #[must_use]
    pub fn children_dyn(mut self, source: Rc<dyn DynChildren>) -> Self {
        self.element.children = Children::Dyn(source);
        self
    }

    /// `onCleanup(fn)` (solid-js onCleanup inside the component body):
    /// runs when THIS element unmounts — child scopes first, then own
    /// handlers in reverse registration order.
    #[must_use]
    pub fn on_cleanup(mut self, cb: impl Fn() + 'static) -> Self {
        self.element.cleanups.push(Rc::new(cb));
        self
    }

    #[must_use]
    pub fn into_element(self) -> Element {
        self.element
    }
}

impl From<ElementBuilder> for Element {
    fn from(b: ElementBuilder) -> Element {
        b.into_element()
    }
}

/// The macro form's dispatcher: `children` routes to children, `key`
/// sets the identity key, everything else to a prop (type-directed
/// via [`PropOrChildren`]).
#[must_use]
pub fn __prop_or_child(
    b: ElementBuilder,
    name: &str,
    v: impl Into<PropOrChildren>,
) -> ElementBuilder {
    // `key:` — the JSX key (identity), not a prop
    let poc = v.into();
    if name == "key" {
        if let PropOrChildren::Prop(Prop::Static(PropValue::Str(k))) = poc {
            return b.key(k);
        }
    }
    match poc {
        PropOrChildren::Children(c) => b.children(c),
        PropOrChildren::Prop(p) => {
            let mut b = b;
            b.element.props.push((name.to_owned(), p));
            b
        }
    }
}

// ---------------------------------------------------------------------------
// element constructors (the JSX tags)
// ---------------------------------------------------------------------------

#[must_use]
pub fn box_() -> ElementBuilder {
    ElementBuilder::new(Tag::Box)
}
#[must_use]
pub fn text() -> ElementBuilder {
    ElementBuilder::new(Tag::Text)
}
#[must_use]
pub fn scrollbox() -> ElementBuilder {
    ElementBuilder::new(Tag::ScrollBox)
}
#[must_use]
pub fn span() -> ElementBuilder {
    ElementBuilder::new(Tag::Span(SpanKind::Plain))
}
#[must_use]
pub fn bold() -> ElementBuilder {
    ElementBuilder::new(Tag::Span(SpanKind::Bold))
}
#[must_use]
pub fn italic() -> ElementBuilder {
    ElementBuilder::new(Tag::Span(SpanKind::Italic))
}
#[must_use]
pub fn underline() -> ElementBuilder {
    ElementBuilder::new(Tag::Span(SpanKind::Underline))
}
#[must_use]
pub fn link() -> ElementBuilder {
    ElementBuilder::new(Tag::Span(SpanKind::Link))
}
#[must_use]
pub fn br() -> ElementBuilder {
    ElementBuilder::new(Tag::Span(SpanKind::Break))
}
/// A raw text run (JSX text between tags — `createTextNode`).
#[must_use]
pub fn text_node(s: impl Into<String>) -> ElementBuilder {
    let mut b = ElementBuilder::new(Tag::TextNode);
    b = b.prop("text", s.into());
    b
}
/// A fragment-like marker (mounts a paint-nothing group).
#[must_use]
pub fn group() -> ElementBuilder {
    ElementBuilder::new(Tag::Group)
}
/// `<Portal mount={target}>{children}</Portal>` (extras.ts:14-55).
#[must_use]
pub fn portal(mount: Option<impl Into<String>>) -> ElementBuilder {
    let mut b = ElementBuilder::new(Tag::Portal);
    if let Some(m) = mount {
        b = b.prop("mount", m.into());
    }
    b
}
/// A custom (extend-registered) element.
#[must_use]
pub fn custom(name: &str) -> ElementBuilder {
    ElementBuilder::new(Tag::Custom(name.to_owned()))
}

// ---------------------------------------------------------------------------
// reactive scope constructs (Show / For / expression children)
// ---------------------------------------------------------------------------

/// `<Show when={cond}>{children}</Show>` (solid-js Show through the
/// reconciler's insert): children mounted while `when` reads true.
pub fn show<F>(when: Rc<dyn Reactive<bool>>, render: F) -> ShowChildren
where
    F: Fn() -> Vec<Element> + 'static,
{
    ShowChildren {
        when,
        render: Rc::new(render),
    }
}

/// A [`ForSource`] over any reactive list of items with a key
/// function — `<For each={list}>{(item) => …}</For>` keyed by
/// `key_of(item)` (upstream keys by item reference identity).
pub struct KeyedListSource<S, T, K> {
    source: S,
    key_of: Rc<K>,
    map: fn(&T) -> PropValue,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<S, T, K> KeyedListSource<S, T, K>
where
    S: Reactive<Vec<T>>,
    K: Fn(&T) -> String + 'static,
    T: Clone + Into<PropValue> + 'static,
{
    #[must_use]
    pub fn new(source: S, key_of: K) -> Self {
        KeyedListSource {
            source,
            key_of: Rc::new(key_of),
            map: |t: &T| t.clone().into(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<S, T, K> ForSource for KeyedListSource<S, T, K>
where
    S: Reactive<Vec<T>> + 'static,
    K: Fn(&T) -> String + 'static,
    T: Clone + 'static,
{
    fn items(&self) -> Vec<ForItem> {
        self.source
            .get()
            .iter()
            .map(|t| ForItem {
                key: (self.key_of)(t),
                value: (self.map)(t),
            })
            .collect()
    }

    fn subscribe(&self, cb: Box<dyn Fn(&Vec<ForItem>)>) -> Box<dyn Fn()> {
        let key_of = Rc::clone(&self.key_of);
        let map = self.map;
        let items_of = move |v: &Vec<T>| -> Vec<ForItem> {
            v.iter()
                .map(|t| ForItem {
                    key: key_of(t),
                    value: map(t),
                })
                .collect()
        };
        self.source
            .subscribe(Box::new(move |v: &Vec<T>| cb(&items_of(v))))
    }
}

/// A [`ForSource`] over a reactive `Vec<ForItem>` (the driver/app
/// shape: keys + payload values directly).
pub struct ForItemSource<S: Reactive<Vec<ForItem>>> {
    pub source: S,
}

impl<S: Reactive<Vec<ForItem>>> ForSource for ForItemSource<S> {
    fn items(&self) -> Vec<ForItem> {
        self.source.get()
    }

    fn subscribe(&self, cb: Box<dyn Fn(&Vec<ForItem>)>) -> Box<dyn Fn()> {
        self.source.subscribe(cb)
    }
}

/// `<For each={list}>{(item) => el}</For>` (solid-js For through the
/// reconciler): keyed children over a reactive list. Matched keys
/// keep their nodes (moves are detach+attach); new keys mount;
/// missing keys unmount.
pub fn for_list(
    source: Rc<dyn ForSource>,
    render: impl Fn(&ForItem, usize) -> Element + 'static,
) -> ForChildren {
    ForChildren {
        source,
        render: Rc::new(render),
    }
}

/// Expression children: `children: () => els` (upstream
/// `insert(parent, accessor)` over an arbitrary accessor).
pub fn children_expr<S: Reactive<Vec<Element>> + 'static>(source: S) -> Rc<dyn DynChildren> {
    Rc::new(ExprChildren { source })
}

/// Wrap a typed reactive source as a dynamic prop value source.
#[must_use]
pub fn dyn_<S, T, F>(source: S, f: F) -> Rc<dyn DynPropSource>
where
    S: Reactive<T> + 'static,
    T: Clone + 'static,
    F: Fn(&T) -> PropValue + 'static,
{
    crate::element::dyn_prop(source, f)
}

/// A signal of a plain value as a dynamic prop.
#[must_use]
pub fn dyn_value<S: Reactive<PropValue> + 'static>(source: S) -> Rc<dyn DynPropSource> {
    crate::element::dyn_value(source)
}

/// A mapped view over a reactive source (createMemo-shaped).
#[must_use]
pub fn map<S, U, T, F>(source: S, f: F) -> Mapped<S, U, T, F>
where
    S: Reactive<U>,
    F: Fn(&U) -> T,
{
    Mapped::new(source, f)
}

// ---------------------------------------------------------------------------
// typed convenience methods (snake_case; upstream names in the docs)
// ---------------------------------------------------------------------------

macro_rules! prop_methods {
    ($($method:ident => $name:literal),* $(,)?) => {
        impl ElementBuilder {
            $(
                #[doc = concat!("The `", $name, "` prop (upstream name).")]
                #[must_use]
                pub fn $method(self, v: impl Into<PropValue>) -> Self {
                    self.prop($name, v)
                }
            )*
        }
    };
}

prop_methods! {
    // layout (applyYogaProps option names)
    flex_direction => "flexDirection",
    flex_grow => "flexGrow",
    flex_shrink => "flexShrink",
    flex_basis => "flexBasis",
    flex_wrap => "flexWrap",
    align_items => "alignItems",
    justify_content => "justifyContent",
    align_self => "alignSelf",
    position => "position",
    overflow => "overflow",
    top => "top",
    right => "right",
    bottom => "bottom",
    left => "left",
    min_width => "minWidth",
    max_width => "maxWidth",
    min_height => "minHeight",
    max_height => "maxHeight",
    margin => "margin",
    margin_x => "marginX",
    margin_y => "marginY",
    margin_top => "marginTop",
    margin_right => "marginRight",
    margin_bottom => "marginBottom",
    margin_left => "marginLeft",
    padding => "padding",
    padding_x => "paddingX",
    padding_y => "paddingY",
    padding_top => "paddingTop",
    padding_right => "paddingRight",
    padding_bottom => "paddingBottom",
    padding_left => "paddingLeft",
    width => "width",
    height => "height",
    // box
    background_color => "backgroundColor",
    border => "border",
    border_style => "borderStyle",
    border_color => "borderColor",
    focused_border_color => "focusedBorderColor",
    should_fill => "shouldFill",
    title => "title",
    title_color => "titleColor",
    title_alignment => "titleAlignment",
    bottom_title => "bottomTitle",
    bottom_title_alignment => "bottomTitleAlignment",
    // text
    content => "content",
    fg => "fg",
    bg => "bg",
    attributes => "attributes",
    wrap_mode => "wrapMode",
    text_align => "textAlign",
    tab_indicator => "tabIndicator",
    tab_indicator_color => "tabIndicatorColor",
    truncate => "truncate",
    // scrollbox
    scroll_x => "scrollX",
    scroll_y => "scrollY",
    sticky_scroll => "stickyScroll",
    sticky_start => "stickyStart",
    // misc
    id => "id",
    focused => "focused",
    href => "href",
}

// ---------------------------------------------------------------------------
// macros (the struct-literal JSX form)
// ---------------------------------------------------------------------------

/// The shared macro body: every `name: value` pair routes through
/// [`__prop_or_child`] (children by type, props by name, `key` sets
/// the identity).
#[macro_export]
macro_rules! solid_element {
    ($ctor:path, { $($name:ident : $val:expr),* $(,)? }) => {{
        #[allow(unused_mut, clippy::redundant_closure_call)]
        let mut b = $ctor();
        $( b = $crate::builder::__prop_or_child(b, stringify!($name), $val); )*
        b
    }};
}

/// `<box …>` — struct-literal form with upstream prop names.
/// `box_! { flexDirection: "row", children: […] }` or
/// `box_!({ … })`.
#[macro_export]
macro_rules! box_ {
    ( { $($rest:tt)* } ) => {
        $crate::solid_element!($crate::builder::box_, { $($rest)* })
    };
    ( $($rest:tt)* ) => {
        $crate::solid_element!($crate::builder::box_, { $($rest)* })
    };
}

/// `<text …>`.
#[macro_export]
macro_rules! text_ {
    ( { $($rest:tt)* } ) => {
        $crate::solid_element!($crate::builder::text, { $($rest)* })
    };
    ( $($rest:tt)* ) => {
        $crate::solid_element!($crate::builder::text, { $($rest)* })
    };
}

/// `<scrollbox …>`.
#[macro_export]
macro_rules! scrollbox_ {
    ( { $($rest:tt)* } ) => {
        $crate::solid_element!($crate::builder::scrollbox, { $($rest)* })
    };
    ( $($rest:tt)* ) => {
        $crate::solid_element!($crate::builder::scrollbox, { $($rest)* })
    };
}

/// `<span …>` (plus the b/i/em/strong/u builder variants).
#[macro_export]
macro_rules! span_ {
    ( { $($rest:tt)* } ) => {
        $crate::solid_element!($crate::builder::span, { $($rest)* })
    };
    ( $($rest:tt)* ) => {
        $crate::solid_element!($crate::builder::span, { $($rest)* })
    };
}
