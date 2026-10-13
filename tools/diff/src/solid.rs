//! The solid-layer differential driver — runs a scenario through
//! `opentui-solid`'s reconciler (the Rust port of `@opentui/solid`),
//! mirroring `js/driver.ts`'s solid vocabulary which drives the REAL
//! `@opentui/solid` 0.5.14 (solid-js `Show`/`For` +
//! `createElement`/`spread`/`insertNode`/`setProp`).
//!
//! Vocabulary (generic — element/prop/signal/show/for/remove/flush,
//! no port-internal branching):
//!
//! ```json
//! { "op": "signal", "id": "s1", "value": 1 }
//! { "op": "signal", "id": "list", "value": [{"key": "a", "value": "alpha"}] }
//! { "op": "el", "tag": "box", "id": "e1", "parent": "root",
//!   "anchor": "e0", "options": { "flexDirection": "row",
//!   "content": {"$signal": "s1"} }, "children": [ …el specs… ] }
//! { "op": "prop", "id": "e1", "prop": "backgroundColor", "value": "#F00" }
//! { "op": "show", "id": "sh1", "parent": "root", "signal": "cond",
//!   "children": [ …el specs… ] }
//! { "op": "for", "id": "f1", "parent": "root", "signal": "list",
//!   "item": { "tag": "text", "options": { "content": "$item" } } }
//! { "op": "el-remove", "id": "e1" }
//! { "op": "flush" }
//! ```
//!
//! `$signal` binds a reactive prop (the JSX getter-prop form); a
//! `value` that is an array of `{key, value}` objects declares a keyed
//! LIST signal (the For shape).

use crate::scenario::{Capture, Line, Scenario, Size, Span, Step};
use anyhow::{Context, Result};
use opentui_core::renderer::CliRendererConfig;
use opentui_solid::element::{Children, Element, ForChildren, ForItem, Prop, PropValue, Tag};
use opentui_solid::reactive::{Mapped, Reactive, Signal};
use opentui_solid::show;
use opentui_solid::SolidRenderer;
use std::collections::HashMap;
use std::rc::Rc;

type ValueSignals = HashMap<String, Rc<Signal<PropValue>>>;
type ListSignals = HashMap<String, Rc<Signal<Vec<ForItem>>>>;

/// A JSON value → [`PropValue`] (`{"$signal": "s"}` resolves against
/// the value-signal registry; `"$item"` against the For payload).
fn prop_value(
    v: &serde_json::Value,
    signals: &ValueSignals,
    item: Option<&PropValue>,
) -> Result<PropKind> {
    if let Some(sig) = v.get("$signal").and_then(|s| s.as_str()) {
        let s = signals
            .get(sig)
            .with_context(|| format!("unknown signal {sig}"))?;
        return Ok(PropKind::Dyn(Rc::clone(s)));
    }
    if v.as_str() == Some("$item") {
        return Ok(PropKind::Static(
            item.cloned().context("$item outside a for template")?,
        ));
    }
    Ok(PropKind::Static(json_prop(v)?))
}

enum PropKind {
    Static(PropValue),
    Dyn(Rc<Signal<PropValue>>),
}

fn json_prop(v: &serde_json::Value) -> Result<PropValue> {
    Ok(match v {
        serde_json::Value::Null => PropValue::None,
        serde_json::Value::Bool(b) => PropValue::Bool(*b),
        serde_json::Value::Number(n) => PropValue::Num(n.as_f64().unwrap_or(0.0) as f32),
        serde_json::Value::String(s) => PropValue::Str(s.clone()),
        serde_json::Value::Array(chunks) => {
            // styled content chunks: [{text, fg?, bg?, attributes?}]
            let mut styled = Vec::new();
            for c in chunks {
                styled.push((
                    c.get("text")
                        .and_then(|t| t.as_str())
                        .context("chunk needs text")?
                        .to_owned(),
                    c.get("fg").and_then(|x| x.as_str()).map(str::to_owned),
                    c.get("bg").and_then(|x| x.as_str()).map(str::to_owned),
                    c.get("attributes").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                ));
            }
            let st = opentui_core::styled_text::StyledText::new(
                styled
                    .into_iter()
                    .map(
                        |(text, fg, bg, attributes)| opentui_core::styled_text::TextChunk {
                            text,
                            fg: fg.map(|s| opentui_core::color::parse_color(&s)),
                            bg: bg.map(|s| opentui_core::color::parse_color(&s)),
                            attributes,
                            link: None,
                        },
                    )
                    .collect(),
            );
            PropValue::Styled(st)
        }
        serde_json::Value::Object(map) => {
            // a style bag: {paddingLeft: 2, …}
            let mut entries = Vec::new();
            for (k, val) in map {
                entries.push((k.clone(), json_prop(val)?));
            }
            PropValue::Style(entries)
        }
    })
}

/// One el-spec → [`Element`]. `$item` resolves against the For payload.
fn el_from_json(
    spec: &serde_json::Value,
    signals: &ValueSignals,
    item: Option<&PropValue>,
) -> Result<Element> {
    let tag_name = spec
        .get("tag")
        .and_then(|t| t.as_str())
        .context("el spec needs tag")?;
    let tag = match tag_name {
        "box" => Tag::Box,
        "text" => Tag::Text,
        "scrollbox" => Tag::ScrollBox,
        "portal" => Tag::Portal,
        other => Tag::Custom(other.to_owned()),
    };
    let mut el = Element::new(tag);
    if let Some(id) = spec.get("id").and_then(|i| i.as_str()) {
        el.key = Some(id.to_owned());
    }
    if let Some(options) = spec.get("options").and_then(|o| o.as_object()) {
        for (name, value) in options {
            match prop_value(value, signals, item)? {
                PropKind::Static(v) => el.props.push((name.clone(), Prop::Static(v))),
                PropKind::Dyn(sig) => {
                    el.props.push((
                        name.clone(),
                        Prop::Dyn(opentui_solid::dyn_value((*sig).clone())),
                    ));
                }
            }
        }
    }
    if let Some(kids) = spec.get("children").and_then(|c| c.as_array()) {
        let mut children = Vec::new();
        for kid in kids {
            children.push(el_from_json(kid, signals, item)?);
        }
        el.children = Children::Static(children);
    }
    Ok(el)
}

fn parent_key_of(sr: &SolidRenderer, step: &Step) -> String {
    let parent = step.parent.as_deref().unwrap_or("root");
    // mounted elements shadow the reserved "root" name (the JS
    // driver's nodes map does the same)
    if parent != "root" || sr.has_mounted("root") {
        parent.to_owned()
    } else {
        SolidRenderer::ROOT.to_owned()
    }
}

/// Interpret the scenario's steps against a [`SolidRenderer`].
pub fn render_solid(scenario: &Scenario) -> Result<Capture> {
    // the differential's custom-element fixture (the JS driver
    // registers a BoxRenderable subclass through @opentui/solid's
    // extend(); here: the box-backed factory + default box patch path)
    opentui_solid::extend::extend(vec![opentui_solid::extend::CustomElement::new(
        "badge_box",
        |renderer, props, id| {
            let opts = opentui_solid::props::box_options(props);
            renderer.make_box(id, &opts)
        },
    )]);

    let mut sr = SolidRenderer::new(CliRendererConfig {
        width: scenario.width,
        height: scenario.height,
        ..Default::default()
    });
    let mut signals: ValueSignals = HashMap::new();
    let mut lists: ListSignals = HashMap::new();

    for step in &scenario.steps {
        match step.op.as_str() {
            "signal" => {
                let id = step.id.clone().context("signal step needs id")?;
                let value = step.value_json().context("signal step needs value")?;
                if let Some(items) = parse_list(&value) {
                    let entry = lists
                        .entry(id)
                        .or_insert_with(|| Rc::new(Signal::new(Vec::new())));
                    (**entry).set(items);
                } else {
                    let entry = signals
                        .entry(id)
                        .or_insert_with(|| Rc::new(Signal::new(PropValue::None)));
                    (**entry).set(json_prop(&value)?);
                }
            }
            "el" => {
                let tag = step.tag.clone().context("el step needs tag")?;
                let mut spec = serde_json::json!({ "tag": tag });
                if let Some(id) = &step.id {
                    spec["id"] = serde_json::json!(id);
                }
                if !step.options.is_null() {
                    spec["options"] = step.options.clone();
                }
                if let Some(kids) = &step.children {
                    spec["children"] = serde_json::json!(kids);
                }
                let el = el_from_json(&spec, &signals, None)?;
                let parent_key = parent_key_of(&sr, step);
                let index = anchor_index(&sr, &parent_key, step.anchor.as_deref());
                sr.mount(&parent_key, index, el);
            }
            "prop" => {
                let id = step.id.clone().context("prop step needs id")?;
                let name = step.prop.clone().context("prop step needs prop")?;
                let value = step.value_json().context("prop step needs value")?;
                let v = match prop_value(&value, &signals, None)? {
                    PropKind::Static(v) => v,
                    PropKind::Dyn(sig) => sig.get(),
                };
                sr.set_prop(&id, &name, v);
            }
            "show" => {
                let sig_id = step.signal.clone().context("show step needs signal")?;
                let cond_sig = signals
                    .get(&sig_id)
                    .with_context(|| format!("unknown signal {sig_id}"))?;
                let cond: Rc<dyn Reactive<bool>> =
                    Rc::new(Mapped::new((**cond_sig).clone(), |v: &PropValue| {
                        matches!(v, PropValue::Bool(true))
                    }));
                let specs = step.children.clone().context("show step needs children")?;
                let show_children = show(cond, move || {
                    specs
                        .iter()
                        .filter_map(|k| el_from_json(k, &HashMap::new(), None).ok())
                        .collect()
                });
                let id = step.id.clone().context("show step needs id")?;
                let parent_key = parent_key_of(&sr, step);
                let mut host = Element::new(Tag::Group);
                host.key = Some(id);
                host.children = Children::Dyn(Rc::new(show_children));
                sr.mount(&parent_key, usize::MAX, host);
            }
            "for" => {
                let sig_id = step.signal.clone().context("for step needs signal")?;
                let list = lists
                    .get(&sig_id)
                    .with_context(|| format!("unknown list signal {sig_id}"))?;
                let template = step.item.clone().context("for step needs item template")?;
                let for_children = ForChildren {
                    source: Rc::new(opentui_solid::builder::ForItemSource {
                        source: (**list).clone(),
                    }),
                    render: Rc::new(move |item: &ForItem, _i: usize| {
                        el_from_json(&template, &HashMap::new(), Some(&item.value))
                            .unwrap_or_else(|e| panic!("for template: {e}"))
                    }),
                };
                let id = step.id.clone().context("for step needs id")?;
                let parent_key = parent_key_of(&sr, step);
                let mut host = Element::new(Tag::Group);
                host.key = Some(id);
                host.children = Children::Dyn(Rc::new(for_children));
                sr.mount(&parent_key, usize::MAX, host);
            }
            "el-remove" => {
                let id = step.id.clone().context("el-remove step needs id")?;
                if !sr.unmount(&id) {
                    anyhow::bail!("el-remove: unknown element {id}");
                }
            }
            "flush" => sr.flush(),
            "resize" => {
                let w = step
                    .resize
                    .map(|r| r.0)
                    .or(step.resize_width)
                    .or(step.width)
                    .context("resize needs width")?;
                let h = step
                    .resize
                    .map(|r| r.1)
                    .or(step.resize_height)
                    .or(step.height)
                    .context("resize needs height")?;
                sr.resize(w, h);
            }
            other => anyhow::bail!("unknown solid op: {other}"),
        }
    }

    sr.flush();

    let buf = &sr.renderer.current_render_buffer;
    let lines = buf
        .get_span_lines()
        .into_iter()
        .map(|line| Line {
            spans: line
                .spans
                .into_iter()
                .map(|s| Span {
                    text: s.text,
                    fg: s.fg.0.to_vec(),
                    bg: s.bg.0.to_vec(),
                    attributes: u32::from(s.attributes),
                    width: s.width,
                })
                .collect(),
        })
        .collect();
    Ok(Capture {
        oracle: "opentui-solid".into(),
        // rust-side captures carry no oracle fingerprint (they are
        // produced by the live harness, never cached)
        capture: None,
        renderer: Size {
            width: buf.width,
            height: buf.height,
        },
        lines,
        char_frame: buf.char_frame(),
        sgr_bytes: sr.renderer.full_sgr_stream().bytes().collect(),
        cursor: None,
    })
}

/// `[{key, value}, …]` → ForItems (a LIST signal declaration).
fn parse_list(v: &serde_json::Value) -> Option<Vec<ForItem>> {
    let arr = v.as_array()?;
    let mut items = Vec::new();
    for entry in arr {
        let key = entry.get("key").and_then(|k| k.as_str())?;
        let value = entry
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        items.push(ForItem {
            key: key.to_owned(),
            value: json_prop(&value).ok()?,
        });
    }
    Some(items)
}

fn anchor_index(sr: &SolidRenderer, parent_key: &str, anchor: Option<&str>) -> usize {
    let Some(anchor) = anchor else {
        return usize::MAX;
    };
    sr.renderer
        .child_ids(parent_key)
        .and_then(|ids| ids.iter().position(|id| id == anchor))
        .unwrap_or(usize::MAX)
}

impl Step {
    fn value_json(&self) -> Option<serde_json::Value> {
        self.options.get("value").cloned().or(self.value.clone())
    }
}
