//! Reconciler behavior tests — mount/diff/unmount sequences ported
//! from upstream's reconciler semantics (packages/solid
//! universal.js:108-163 `reconcileArrays`, :192-208 `spreadExpression`;
//! reconciler.ts:60-141 insert/remove) plus characterization of the
//! port's observable op log where upstream has no test file.

use crate::builder::{bold, custom, dyn_, dyn_value, for_list, portal, text, text_node};
use crate::element::{Element, ForItem, PropValue};
use crate::extend::extend;
use crate::extend::CustomElement;
use crate::hooks::{EventData, KeyEvent};
use crate::reactive::{Reactive, Signal};
use crate::reconcile::SolidRenderer;
use crate::scrollback::{
    create_scrollback_writer, ScrollbackRenderContext, SolidScrollbackWriterOptions,
};
use crate::scrollbox_;
use crate::{box_, show, text_};
use opentui_core::renderer::CliRendererConfig;
use std::cell::RefCell;
use std::rc::Rc;

fn sr(w: u32, h: u32) -> SolidRenderer {
    SolidRenderer::new(CliRendererConfig {
        width: w,
        height: h,
        ..Default::default()
    })
}

fn frame_of(sr: &SolidRenderer) -> Vec<String> {
    sr.renderer
        .current_render_buffer
        .char_frame()
        .split('\n')
        .map(|l| l.trim_end().to_owned())
        .collect()
}

/// A For source over a `Signal<Vec<ForItem>>` (the driver/app shape).
fn items_signal(items: Vec<(&str, &str)>) -> Signal<Vec<ForItem>> {
    Signal::new(
        items
            .into_iter()
            .map(|(k, v)| ForItem {
                key: k.to_owned(),
                value: PropValue::Str(v.to_owned()),
            })
            .collect(),
    )
}

fn for_over(sig: &Signal<Vec<ForItem>>) -> std::rc::Rc<dyn crate::element::ForSource> {
    std::rc::Rc::new(crate::builder::ForItemSource {
        source: sig.clone(),
    })
}

fn row_element(item: &ForItem, _i: usize) -> Element {
    text()
        .key(item.key.clone())
        .content(item.value.clone())
        .into_element()
}

// ---------------------------------------------------------------------------
// mount + element creation
// ---------------------------------------------------------------------------

#[test]
fn mount_renders_builder_tree() {
    let mut s = sr(30, 4);
    let tree = box_! {
        flexDirection: "column",
        children: [
            text_! { content: "hello" },
            text_! { content: "world" },
        ],
    }
    .into_element();
    s.render_root(tree);
    let rows = frame_of(&s);
    assert_eq!(rows[0], "hello");
    assert_eq!(rows[1], "world");
}

#[test]
fn auto_ids_follow_getnextid_counter() {
    // upstream getNextId: `${elementType}-${n}` starting at 1
    // (utils/id-counter.ts:3-12)
    let mut s = sr(10, 3);
    s.render_root(
        box_! { children: [text_! { content: "x" }, text_! { content: "y" }] }.into_element(),
    );
    let ops = s.ops_log().to_vec();
    assert!(ops.iter().any(|o| o == "Element created with id: box-1"));
    assert!(ops.iter().any(|o| o == "Element created with id: text-1"));
    assert!(ops.iter().any(|o| o == "Element created with id: text-2"));
}

#[test]
fn insert_ops_log_anchors() {
    let mut s = sr(10, 5);
    s.render_root(box_! { key: "root-box" }.into_element());
    s.mount(
        "root-box",
        usize::MAX,
        text().key("a").content("a").into_element(),
    );
    s.mount("root-box", 0, text().key("b").content("b").into_element());
    s.flush();
    let ops = s.ops_log().to_vec();
    // the second mount inserted at anchor index 0 (reconciler.ts:98-111)
    assert!(ops
        .iter()
        .any(|o| o.contains("Inserting node") && o.contains("anchor: 0")));
    assert_eq!(
        s.renderer.child_ids("root-box").unwrap(),
        vec!["b".to_owned(), "a".to_owned()]
    );
}

// ---------------------------------------------------------------------------
// keyed diffing (universal.js:108-163)
// ---------------------------------------------------------------------------

#[test]
fn keyed_append_and_insert_before() {
    let mut s = sr(20, 10);
    let sig = items_signal(vec![("a", "alpha"), ("c", "gamma")]);
    let tree =
        box_! { key: "list", children: for_list(for_over(&sig), row_element) }.into_element();
    s.render_root(tree);

    // insert "b" between a and c (universal.js:126-128's insert-run)
    sig.set(
        vec![("a", "alpha"), ("b", "bravo"), ("c", "gamma")]
            .into_iter()
            .map(|(k, v)| ForItem {
                key: k.into(),
                value: PropValue::Str(v.into()),
            })
            .collect(),
    );
    s.flush();
    assert_eq!(
        s.renderer.child_ids("list").unwrap(),
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );
    let rows = frame_of(&s);
    assert_eq!(rows[0], "alpha");
    assert_eq!(rows[1], "bravo");
    assert_eq!(rows[2], "gamma");
}

#[test]
fn keyed_move_to_front_uses_moves_not_replaces() {
    let mut s = sr(20, 10);
    let sig = items_signal(vec![("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")]);
    s.render_root(
        box_! { key: "list", children: for_list(for_over(&sig), row_element) }.into_element(),
    );

    sig.set(
        vec![("d", "4"), ("a", "1"), ("b", "2"), ("c", "3")]
            .into_iter()
            .map(|(k, v)| ForItem {
                key: k.into(),
                value: PropValue::Str(v.into()),
            })
            .collect(),
    );
    s.flush();
    assert_eq!(
        s.renderer.child_ids("list").unwrap(),
        vec!["d", "a", "b", "c"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
    // the diff is a MOVE (detach+attach), never remove+recreate for
    // matched keys — no "Removing node" ops for a/b/c
    let removing: Vec<_> = s
        .ops_log()
        .iter()
        .filter(|o| o.starts_with("Removing node"))
        .collect();
    assert!(
        removing.is_empty(),
        "matched keys must not unmount: {removing:?}"
    );
    let rows = frame_of(&s);
    assert_eq!(rows[0], "4");
}

#[test]
fn keyed_swap_detected_as_two_moves() {
    // universal.js:134-138: a[aStart] matches b[bEnd-1] and vice versa
    // → swap branch (two moves, no replaces)
    let mut s = sr(20, 10);
    let sig = items_signal(vec![("a", "1"), ("b", "2"), ("c", "3")]);
    s.render_root(
        box_! { key: "list", children: for_list(for_over(&sig), row_element) }.into_element(),
    );

    sig.set(
        vec![("c", "3"), ("b", "2"), ("a", "1")]
            .into_iter()
            .map(|(k, v)| ForItem {
                key: k.into(),
                value: PropValue::Str(v.into()),
            })
            .collect(),
    );
    s.flush();
    assert_eq!(
        s.renderer.child_ids("list").unwrap(),
        vec!["c", "b", "a"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
    let moves = s
        .ops_log()
        .iter()
        .filter(|o| o.starts_with("Moving node"))
        .count();
    assert_eq!(moves, 2, "swap = exactly two moves");
    let rows = frame_of(&s);
    assert_eq!((rows[0].as_str(), rows[2].as_str()), ("3", "1"));
}

#[test]
fn keyed_remove_unmounts_only_missing() {
    let mut s = sr(20, 10);
    let sig = items_signal(vec![("a", "1"), ("b", "2"), ("c", "3")]);
    s.render_root(
        box_! { key: "list", children: for_list(for_over(&sig), row_element) }.into_element(),
    );

    sig.set(
        vec![("b", "2")]
            .into_iter()
            .map(|(k, v)| ForItem {
                key: k.into(),
                value: PropValue::Str(v.into()),
            })
            .collect(),
    );
    s.flush();
    assert_eq!(s.renderer.child_ids("list").unwrap(), vec!["b".to_owned()]);
    let removed: Vec<_> = s
        .ops_log()
        .iter()
        .filter(|o| o.starts_with("Removing node"))
        .collect();
    assert_eq!(removed.len(), 2);
    // removed live ids no longer exist in the tree
    assert!(s.renderer.root.inner.find("a").is_none());
    assert!(s.renderer.root.inner.find("c").is_none());
    assert!(s.renderer.root.inner.find("b").is_some());
}

#[test]
fn keyed_middle_insert_uses_sequence_heuristic() {
    // universal.js:139-158: the map+sequence path inserts the b-run
    // before the matched a-item instead of replacing
    let mut s = sr(20, 12);
    let sig = items_signal(vec![
        ("h", "head"),
        ("x", "tail1"),
        ("y", "tail2"),
        ("z", "tail3"),
    ]);
    s.render_root(
        box_! { key: "list", children: for_list(for_over(&sig), row_element) }.into_element(),
    );

    // b-run [m, n] before x: sequence(1) > index - bStart? → insert run
    sig.set(
        vec![
            ("h", "head"),
            ("m", "mid1"),
            ("n", "mid2"),
            ("x", "tail1"),
            ("y", "tail2"),
            ("z", "tail3"),
        ]
        .into_iter()
        .map(|(k, v)| ForItem {
            key: k.into(),
            value: PropValue::Str(v.into()),
        })
        .collect(),
    );
    s.flush();
    assert_eq!(
        s.renderer.child_ids("list").unwrap(),
        vec!["h", "m", "n", "x", "y", "z"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
    let rows = frame_of(&s);
    assert_eq!(rows[1], "mid1");
    assert_eq!(rows[3], "tail1");
}

// ---------------------------------------------------------------------------
// unmount / cleanup ordering (solid owner disposal)
// ---------------------------------------------------------------------------

#[test]
fn unmount_cleanup_runs_children_first_then_own_lifo() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut s = sr(20, 6);

    let mk = |name: &'static str, log: Rc<RefCell<Vec<String>>>| {
        let l1 = log.clone();
        let l2 = log.clone();
        let l3 = log.clone();
        box_()
            .key(name.to_owned())
            .on_cleanup(move || l1.borrow_mut().push(format!("{name}-c1")))
            .on_cleanup(move || l2.borrow_mut().push(format!("{name}-c2")))
            .child(
                box_()
                    .key(format!("{name}-child"))
                    .on_cleanup(move || l3.borrow_mut().push(format!("{name}-child-c1"))),
            )
    };

    let tree = box_! { key: "root", children: [mk("p", log.clone())] };
    s.render_root(tree.into_element());
    assert!(s.unmount("p"));
    s.flush();
    // child scope disposes first, then own cleanups in reverse
    // registration (solid owner disposal)
    assert_eq!(
        *log.borrow(),
        vec![
            "p-child-c1".to_owned(),
            "p-c2".to_owned(),
            "p-c1".to_owned(),
        ]
    );
    // the subtree is gone from the live tree
    assert!(s.renderer.root.inner.find("p-child").is_none());
}

#[test]
fn unmount_detaches_signal_subscriptions() {
    // after unmount, firing the signal must not patch (no panics, no
    // ghost nodes) — the effect disposal path
    let mut s = sr(20, 4);
    let content = Signal::new("v1".to_owned());
    let el = text()
        .key("t")
        .dyn_prop(
            "content",
            dyn_(content.clone(), |v| PropValue::Str(v.clone())),
        )
        .into_element();
    s.render_root(el);
    s.unmount("t");
    content.set("v2".into());
    s.flush();
    assert!(s.renderer.root.inner.find("t").is_none());
}

// ---------------------------------------------------------------------------
// prop patching (universal.js:198-206 + reconciler.ts:230-367)
// ---------------------------------------------------------------------------

#[test]
fn dynamic_prop_updates_render_in_place() {
    let mut s = sr(20, 3);
    let content = Signal::new("first".to_owned());
    s.render_root(
        text()
            .key("t")
            .dyn_prop(
                "content",
                dyn_(content.clone(), |v| PropValue::Str(v.clone())),
            )
            .into_element(),
    );
    assert_eq!(frame_of(&s)[0], "first");
    // same live node keeps its position (no unmount ops)
    // the explicit key doubles as the live id
    let node_id = s.renderer.root.inner.find("t").map(|n| n.id.0.clone());
    assert_eq!(node_id.as_deref(), Some("t"));
    content.set("second".into());
    s.flush();
    assert_eq!(frame_of(&s)[0], "second");
    assert!(s.ops_log().iter().all(|o| !o.starts_with("Removing node")));
}

/// Prop-patch minimalism: a counting custom element observes exactly
/// the changed data prop patches (equal values skip —
/// universal.js:202-204).
#[test]
fn prop_patches_are_minimal() {
    let counter = Rc::new(RefCell::new(Vec::<Option<PropValue>>::new()));
    extend(vec![CustomElement::new("counter-box", {
        let counter = counter.clone();
        move |renderer, props, id| {
            let c = counter.clone();
            let opts = crate::props::box_options(props);
            let r = renderer.make_box(id, &opts);
            let _ = c;
            r
        }
    })
    .with_patch({
        let counter = counter.clone();
        move |_renderer, _id, new_props, _prev| {
            counter
                .borrow_mut()
                .push(crate::props::prop_get(new_props, "title").cloned());
        }
    })]);

    let mut s = sr(20, 3);
    let title = Signal::new(PropValue::Str("one".into()));
    s.render_root(
        custom("counter-box")
            .key("cb")
            .dyn_prop("title", dyn_value(title.clone()))
            .into_element(),
    );
    // mount applies initial props WITHOUT the patch path
    assert!(counter.borrow().is_empty());

    // SAME value → no patch (universal.js:202-204)
    title.set(PropValue::Str("one".into()));
    s.flush();
    assert!(counter.borrow().is_empty());

    // different value → exactly one patch carrying the new value
    title.set(PropValue::Str("two".into()));
    s.flush();
    assert_eq!(*counter.borrow(), vec![Some(PropValue::Str("two".into()))]);
}

#[test]
fn style_bag_patches_only_changed_sub_props() {
    let seen = Rc::new(RefCell::new(Vec::<(String, PropValue)>::new()));
    extend(vec![CustomElement::new("style-box", |_r, props, id| {
        let opts = crate::props::box_options(props);
        _r.make_box(id, &opts)
    })
    .with_patch({
        let seen = seen.clone();
        move |_r, _id, new_props, _prev| {
            // the DEFAULT rebuild path; observe what the reconciler
            // applied via the prop map
            if let Some(v) = crate::props::prop_get(new_props, "style") {
                seen.borrow_mut().push(("style".into(), v.clone()));
            }
        }
    })]);

    let mut s = sr(20, 3);
    let style = Signal::new(PropValue::Style(vec![
        ("paddingLeft".into(), PropValue::Num(1.0)),
        ("flexDirection".into(), PropValue::Str("row".into())),
    ]));
    s.render_root(
        custom("style-box")
            .key("sb")
            .dyn_prop("style", dyn_value(style.clone()))
            .into_element(),
    );
    style.set(PropValue::Style(vec![
        ("paddingLeft".into(), PropValue::Num(1.0)), // unchanged
        ("flexDirection".into(), PropValue::Str("column".into())), // changed
    ]));
    s.flush();
    // the style-bag diff applied flexDirection only; the prop map
    // records the new bag, and rebuild saw the changed sub-prop
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn id_prop_renames_live_node() {
    // node.id = value through setProperty (reconciler.ts:260-263)
    let mut s = sr(20, 3);
    s.render_root(box_! { key: "b" }.into_element());
    assert!(s.renderer.root.inner.find("b").is_some());
    s.set_prop("b", "id", PropValue::Str("renamed".into()));
    assert!(s.renderer.root.inner.find("renamed").is_some());
    assert!(s.renderer.root.inner.find("b").is_none());
    // the shadow key is unchanged — patches still address "b"
    s.set_prop("b", "backgroundColor", PropValue::Str("#FF0000".into()));
    s.flush();
    assert!(s.renderer.root.inner.find("renamed").is_some());
}

#[test]
fn scroll_prop_routes_to_scrollbox_setter() {
    let mut s = sr(20, 8);
    s.render_root(scrollbox_! { key: "sb", width: 20.0, height: 8.0 }.into_element());
    // 10 rows to scroll (children route into the content viewport,
    // ScrollBox.add — ScrollBox.ts:514-516)
    for i in 0..10 {
        let t = text().key(format!("r{i}")).content(format!("line-{i:02}"));
        s.mount("sb", usize::MAX, t.into_element());
    }
    s.flush();
    // scrollTop prop patch (setProperty default branch → ScrollBox
    // scrollTop setter, ScrollBox.ts:168-171)
    s.set_prop("sb", "scrollTop", PropValue::Num(2.0));
    s.flush();
    let top_now = opentui_core::renderables::scrollbox::scroll_top(&s.renderer.root.inner, "sb");
    assert_eq!(top_now, Some(2.0));
    let rows = frame_of(&s);
    assert_eq!(rows[0], "line-02");
}

// ---------------------------------------------------------------------------
// Show (solid-js Show through insert)
// ---------------------------------------------------------------------------

#[test]
fn show_toggles_subtree_mount_unmount() {
    let mut s = sr(20, 4);
    let cond = Signal::new(true);
    let cond_rc: Rc<dyn crate::reactive::Reactive<bool>> = Rc::new(cond.clone());
    s.render_root(
        box_! { key: "root" }
            .children_dyn(std::rc::Rc::new(show(cond_rc, || {
                vec![text_! { key: "opt", content: "visible" }.into_element()]
            })))
            .into_element(),
    );
    assert_eq!(frame_of(&s)[0], "visible");
    cond.set(false);
    s.flush();
    assert!(s.renderer.root.inner.find("opt").is_none());
    let rows = frame_of(&s);
    assert_eq!(rows[0], "");
    cond.set(true);
    s.flush();
    assert!(s.renderer.root.inner.find("opt").is_some());
    assert_eq!(frame_of(&s)[0], "visible");
}

// ---------------------------------------------------------------------------
// Portal (extras.ts:14-55)
// ---------------------------------------------------------------------------

#[test]
fn portal_renders_into_target_container() {
    let mut s = sr(40, 6);
    // target: a keyed box mounted first
    s.render_root(
        box_! {
            key: "shell",
            children: [
                box_! { key: "main", height: 3.0, children: [
                    text_! { key: "m1", content: "main-line" }
                ]},
            ],
        }
        .into_element(),
    );
    // portal into "shell" (a sibling overlay)
    s.mount(
        SolidRenderer::ROOT,
        usize::MAX,
        portal(Some("shell"))
            .key("p1")
            .child(text().key("pt").content("portal-line"))
            .into_element(),
    );
    s.flush();
    // the portal's children live under a container appended to shell
    assert!(s.renderer.root.inner.find("pt").is_some());
    // unmounting the portal removes the container
    assert!(s.unmount("p1"));
    s.flush();
    assert!(s.renderer.root.inner.find("pt").is_none());
}

// ---------------------------------------------------------------------------
// events (reconciler.ts:231-239)
// ---------------------------------------------------------------------------

#[test]
fn events_bind_trigger_and_swap() {
    extend(vec![CustomElement::new("evt-box", |r, props, id| {
        let opts = crate::props::box_options(props);
        r.make_box(id, &opts)
    })]);
    let hits = Rc::new(RefCell::new(Vec::new()));
    let mut s = sr(20, 3);
    let h1 = {
        let hits = hits.clone();
        Rc::new(move |_e: &EventData| hits.borrow_mut().push("h1".to_owned()))
    };
    let h2 = {
        let hits = hits.clone();
        Rc::new(move |_e: &EventData| hits.borrow_mut().push("h2".to_owned()))
    };
    s.render_root(
        custom("evt-box")
            .key("e")
            .prop("on:ping", PropValue::Event(h1.clone()))
            .into_element(),
    );
    assert!(s.events_of("e").contains(&"ping".to_owned()));
    s.trigger_event(
        "e",
        "ping",
        EventData::Custom {
            name: "ping".into(),
            payload: String::new(),
        },
    );
    assert_eq!(*hits.borrow(), vec!["h1".to_owned()]);

    // swapping the handler prop removes the old (node.off(prev),
    // reconciler.ts:236-238) and binds the new
    s.set_prop("e", "on:ping", PropValue::Event(h2.clone()));
    s.trigger_event(
        "e",
        "ping",
        EventData::Custom {
            name: "ping".into(),
            payload: String::new(),
        },
    );
    assert_eq!(*hits.borrow(), vec!["h1".to_owned(), "h2".to_owned()]);

    // None clears the handler (upstream: `if (value)` only binds)
    s.set_prop("e", "on:ping", PropValue::None);
    s.trigger_event(
        "e",
        "ping",
        EventData::Custom {
            name: "ping".into(),
            payload: String::new(),
        },
    );
    assert_eq!(*hits.borrow(), vec!["h1".to_owned(), "h2".to_owned()]);
}

// ---------------------------------------------------------------------------
// hooks (elements/hooks.ts)
// ---------------------------------------------------------------------------

#[test]
fn terminal_dimensions_signal_tracks_resize() {
    let mut s = sr(40, 10);
    assert_eq!(s.terminal_dimensions().get(), (40, 10));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen2 = seen.clone();
    let _off = s.on_resize(move |w, h| seen2.borrow_mut().push((w, h)));
    s.resize(60, 20);
    assert_eq!(s.terminal_dimensions().get(), (60, 20));
    assert_eq!(*seen.borrow(), vec![(60, 20)]);
    // the core renderer resized too
    assert_eq!((s.renderer.width(), s.renderer.height()), (60, 20));
}

#[test]
fn keyboard_stream_press_release_and_unsubscribe() {
    let mut s = sr(20, 3);
    let got = Rc::new(RefCell::new(Vec::new()));
    let got2 = got.clone();
    let off = s.keyboard().on(move |e: &KeyEvent| {
        got2.borrow_mut().push(format!("{}:{}", e.name, e.repeated));
    });
    s.feed_key(KeyEvent::press("a"));
    s.feed_key(KeyEvent {
        repeated: true,
        ..KeyEvent::press("a")
    });
    off();
    s.feed_key(KeyEvent::press("b"));
    assert_eq!(
        *got.borrow(),
        vec!["a:false".to_owned(), "a:true".to_owned()]
    );
}

// ---------------------------------------------------------------------------
// text children (spans — catalogue.ts:31, 107-114)
// ---------------------------------------------------------------------------

#[test]
fn text_span_children_render_styled() {
    let mut s = sr(20, 2);
    let tree = text_! {
        key: "t",
        children: [
            text_node("plain "),
            bold().child(text_node("bold")),
            text_node(" tail"),
        ],
    }
    .into_element();
    s.render_root(tree);
    let rows = frame_of(&s);
    assert_eq!(rows[0], "plain bold tail");
    // the bold span carries BOLD attributes
    let spans = s.renderer.current_render_buffer.get_span_lines();
    let s1 = &spans[0].spans[1];
    assert_eq!(s1.text, "bold");
    assert_eq!(s1.attributes, 1);
}

#[test]
#[should_panic(expected = "Orphan text error")]
fn orphan_text_node_outside_text_panics() {
    let mut s = sr(20, 3);
    // a span directly under a box — the orphan text error
    // (reconciler.ts:80-89)
    s.render_root(box_! { children: [bold().child(text_node("x")).into_element()] }.into_element());
}

// ---------------------------------------------------------------------------
// scrollback writer (scrollback.ts:145-227)
// ---------------------------------------------------------------------------

#[test]
fn scrollback_writer_measures_and_paints() {
    let writer = create_scrollback_writer(
        || {
            box_! {
                flexDirection: "column",
                children: [
                    text_! { content: "first line" },
                    text_! { content: "second line" },
                ],
            }
            .into_element()
        },
        SolidScrollbackWriterOptions::default(),
    );
    let snap = writer.write(&ScrollbackRenderContext {
        width: 20,
        tail_column: 0,
    });
    // auto height converges to the content height (2 rows)
    assert_eq!(snap.height(), 2);
    let lines = snap.paint_lines();
    assert_eq!(lines[0].trim_end(), "first line");
    assert_eq!(lines[1].trim_end(), "second line");
}

// ---------------------------------------------------------------------------
// shadow tree / op-log observability
// ---------------------------------------------------------------------------

#[test]
fn ops_log_is_empty_for_noop_flush() {
    let mut s = sr(10, 3);
    let before = s.ops_log().len();
    s.render_root(text_! { key: "t", content: "x" }.into_element());
    let after_mount = s.ops_log().len();
    s.flush(); // nothing changed → no new ops
    assert_eq!(s.ops_log().len(), after_mount);
    assert!(after_mount > before);
}
