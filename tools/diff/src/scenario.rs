//! Scenario loading + the Rust-side capture (mirrors js/driver.ts).

use anyhow::{Context, Result};
use opentui_core::renderables::box_::BorderSpec;
use opentui_core::renderables::scrollbar::ScrollUnit;
use opentui_core::renderables::scrollbox::{ScrollBoxOptions, StickyStart};
use opentui_core::renderables::text::WrapMode;
use opentui_core::renderables::{box_::BoxOptions, text::TextOptions};
use opentui_core::renderer::{create_cli_renderer, CliRenderer, CliRendererConfig};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Scenario {
    #[allow(dead_code)]
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub struct Step {
    pub op: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub options: serde_json::Value,
    #[serde(default)]
    pub prop: Option<String>,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub resize_width: Option<u32>,
    #[serde(default)]
    pub resize_height: Option<u32>,
    #[serde(default)]
    pub resize: Option<(u32, u32)>,
}

pub fn load(path: &std::path::Path) -> Result<Scenario> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading scenario {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing scenario {}", path.display()))
}

fn opt_f32(v: &serde_json::Value, key: &str) -> Option<opentui_core::layout::Dimension> {
    v.get(key)
        .and_then(|x| x.as_f64())
        .map(|f| opentui_core::layout::Dimension::Points(f as f32))
}

/// Layout-ish props both sides understand (`applyProps` in the JS driver).
fn apply_props(layout: &mut opentui_core::layout::LayoutOptions, o: &serde_json::Value) {
    use opentui_core::layout::{Dimension, LayoutOptions};
    let _ = LayoutOptions::default();
    if let Some(d) = o.get("flexDirection").and_then(|v| v.as_str()) {
        layout.flex_direction = Some(d.to_owned());
    }
    if let Some(g) = o.get("flexGrow").and_then(|v| v.as_f64()) {
        layout.flex_grow = Some(g as f32);
    }
    if let Some(p) = o.get("padding").and_then(|v| v.as_f64()) {
        layout.padding = Some(Dimension::Points(p as f32));
    }
    if let Some(p) = o.get("paddingTop").and_then(|v| v.as_f64()) {
        layout.padding_top = Some(Dimension::Points(p as f32));
    }
    if let Some(p) = o.get("paddingBottom").and_then(|v| v.as_f64()) {
        layout.padding_bottom = Some(Dimension::Points(p as f32));
    }
    if let Some(p) = o.get("paddingLeft").and_then(|v| v.as_f64()) {
        layout.padding_left = Some(Dimension::Points(p as f32));
    }
    if let Some(p) = o.get("paddingRight").and_then(|v| v.as_f64()) {
        layout.padding_right = Some(Dimension::Points(p as f32));
    }
}

/// One scenario node pending mount (kind + options + children). The id
/// lives in the staging maps; variants carry only their options.
enum PendingNode {
    Text(TextOptions),
    Box(BoxOptions),
    ScrollBox(Box<ScrollBoxOptions>),
}

/// Build + render the scenario with opentui-core, dump the same capture
/// shape the JS oracle emits.
pub fn render_rust(scenario: &Scenario) -> Result<Capture> {
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: scenario.width,
        height: scenario.height,
        ..Default::default()
    });

    // Stage nodes with their parent links; mount lazily — before the
    // first action step (flush/set/call/resize) and once more at the end —
    // so scripted mutations observe the mounted tree like the JS driver's
    // strictly sequential adds.
    let mut by_id: std::collections::HashMap<String, PendingNode> =
        std::collections::HashMap::new();
    let mut children_of: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut roots: Vec<String> = Vec::new();
    for step in &scenario.steps {
        match step.op.as_str() {
            "flush" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                renderer.render();
                continue;
            }
            "set" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let id = step.id.clone().context("set step needs id")?;
                let prop = step.prop.clone().context("set step needs prop")?;
                let value = step.value.context("set step needs value")? as f32;
                match prop.as_str() {
                    "scrollTop" => renderer.set_scroll_top(&id, value),
                    "scrollLeft" => renderer.set_scroll_left(&id, value),
                    other => anyhow::bail!("set: unsupported prop {other}"),
                }
                continue;
            }
            "call" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let id = step.id.clone().context("call step needs id")?;
                let method = step.method.clone().context("call step needs method")?;
                let args = step.args.clone().unwrap_or_default();
                match method.as_str() {
                    "scrollBy" => {
                        let mut dx = 0.0f32;
                        let mut dy = 0.0f32;
                        let mut unit = ScrollUnit::Absolute;
                        for (i, a) in args.iter().enumerate() {
                            match i {
                                0 if a.is_number() => {
                                    dy = a.as_f64().unwrap() as f32;
                                }
                                0 => {
                                    let o = a.as_object().context("scrollBy arg")?;
                                    dx = o
                                        .get("x")
                                        .and_then(serde_json::Value::as_f64)
                                        .unwrap_or(0.0)
                                        as f32;
                                    dy = o
                                        .get("y")
                                        .and_then(serde_json::Value::as_f64)
                                        .unwrap_or(0.0)
                                        as f32;
                                }
                                1 => {
                                    if a.as_str() == Some("viewport") {
                                        unit = ScrollUnit::Viewport;
                                    } else if a.as_str() == Some("content") {
                                        unit = ScrollUnit::Content;
                                    } else if a.as_str() == Some("step") {
                                        unit = ScrollUnit::Step;
                                    }
                                }
                                _ => {}
                            }
                        }
                        renderer.scroll_by(&id, dx, dy, unit);
                    }
                    "scrollTo" => {
                        let mut x = 0.0f32;
                        let mut y = 0.0f32;
                        for a in &args {
                            if a.is_number() {
                                y = a.as_f64().unwrap() as f32;
                            } else if let Some(o) = a.as_object() {
                                x = o
                                    .get("x")
                                    .and_then(serde_json::Value::as_f64)
                                    .unwrap_or(0.0) as f32;
                                y = o
                                    .get("y")
                                    .and_then(serde_json::Value::as_f64)
                                    .unwrap_or(0.0) as f32;
                            }
                        }
                        renderer.scroll_to(&id, x, y);
                    }
                    "scrollChildIntoView" => {
                        let child = args
                            .first()
                            .and_then(serde_json::Value::as_str)
                            .context("scrollChildIntoView needs child id")?;
                        renderer.scroll_child_into_view(&id, child);
                    }
                    other => anyhow::bail!("call: unsupported method {other}"),
                }
                continue;
            }
            "resize" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
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
                renderer.resize(w, h);
                continue;
            }
            _ => {}
        }
        let o = &step.options;
        let node = match step.op.as_str() {
            "text" => {
                let mut opts = TextOptions::default();
                if let Some(c) = o.get("content").and_then(|v| v.as_str()) {
                    opts.content = c.to_owned();
                }
                if let Some(fg) = o.get("fg").and_then(|v| v.as_str()) {
                    opts.fg = Some(opentui_core::color::parse_color(fg));
                }
                if let Some(bg) = o.get("bg").and_then(|v| v.as_str()) {
                    opts.bg = Some(opentui_core::color::parse_color(bg));
                }
                if let Some(w) = opt_f32(o, "width") {
                    opts.width = Some(w);
                }
                if let Some(h) = opt_f32(o, "height") {
                    opts.height = Some(h);
                }
                if let Some(w) = o.get("wrapMode").and_then(|v| v.as_str()) {
                    opts.wrap_mode = Some(WrapMode::parse(w));
                }
                PendingNode::Text(opts)
            }
            "box" => {
                let mut opts = BoxOptions::default();
                if let Some(c) = o.get("backgroundColor").and_then(|v| v.as_str()) {
                    opts.background_color = Some(c.to_owned());
                }
                if let Some(b) = o.get("border") {
                    opts.border = Some(if b.as_bool().unwrap_or(false) {
                        BorderSpec::All
                    } else {
                        BorderSpec::Off
                    });
                }
                if let Some(s) = o.get("borderStyle").and_then(|v| v.as_str()) {
                    opts.border_style = Some(s.to_owned());
                }
                if let Some(c) = o.get("borderColor").and_then(|v| v.as_str()) {
                    opts.border_color = Some(c.to_owned());
                }
                if let Some(f) = o.get("shouldFill").and_then(|v| v.as_bool()) {
                    opts.should_fill = Some(f);
                }
                if let Some(t) = o.get("title").and_then(|v| v.as_str()) {
                    opts.title = Some(t.to_owned());
                }
                if let Some(a) = o.get("titleAlignment").and_then(|v| v.as_str()) {
                    opts.title_alignment = Some(a.to_owned());
                }
                if let Some(w) = opt_f32(o, "width") {
                    opts.width = Some(w);
                }
                if let Some(h) = opt_f32(o, "height") {
                    opts.height = Some(h);
                }
                if let Some(d) = o.get("flexDirection").and_then(|v| v.as_str()) {
                    opts.layout.flex_direction = Some(d.to_owned());
                }
                if let Some(p) = o.get("padding").and_then(|v| v.as_f64()) {
                    opts.layout.padding = Some(opentui_core::layout::Dimension::Points(p as f32));
                }
                PendingNode::Box(opts)
            }
            "scrollbox" => {
                let mut opts = ScrollBoxOptions {
                    viewport_culling: true,
                    scroll_y: true,
                    scroll_x: false,
                    ..Default::default()
                };
                let mut box_opts = BoxOptions::default();
                if let Some(c) = o.get("backgroundColor").and_then(|v| v.as_str()) {
                    box_opts.background_color = Some(c.to_owned());
                }
                if let Some(b) = o.get("border") {
                    box_opts.border = Some(if b.as_bool().unwrap_or(false) {
                        BorderSpec::All
                    } else {
                        BorderSpec::Off
                    });
                }
                if let Some(c) = o.get("borderColor").and_then(|v| v.as_str()) {
                    box_opts.border_color = Some(c.to_owned());
                }
                if let Some(s) = o.get("borderStyle").and_then(|v| v.as_str()) {
                    box_opts.border_style = Some(s.to_owned());
                }
                if let Some(f) = o.get("shouldFill").and_then(|v| v.as_bool()) {
                    box_opts.should_fill = Some(f);
                }
                if let Some(w) = opt_f32(o, "width") {
                    box_opts.width = Some(w);
                }
                if let Some(h) = opt_f32(o, "height") {
                    box_opts.height = Some(h);
                }
                apply_props(&mut box_opts.layout, o);
                opts.box_options = box_opts;
                if let Some(v) = o.get("scrollX").and_then(|v| v.as_bool()) {
                    opts.scroll_x = v;
                }
                if let Some(v) = o.get("scrollY").and_then(|v| v.as_bool()) {
                    opts.scroll_y = v;
                }
                if let Some(v) = o.get("stickyScroll").and_then(|v| v.as_bool()) {
                    opts.sticky_scroll = v;
                }
                if let Some(v) = o.get("stickyStart").and_then(|v| v.as_str()) {
                    opts.sticky_start = StickyStart::parse(v);
                }
                if let Some(sb) = o.get("scrollbarOptions") {
                    if let Some(c) = sb.get("thumbColor").and_then(|v| v.as_str()) {
                        opts.scrollbar_thumb_color = Some(c.to_owned());
                    }
                    if let Some(c) = sb.get("trackColor").and_then(|v| v.as_str()) {
                        opts.scrollbar_track_color = Some(c.to_owned());
                    }
                }
                if let Some(vc) = o.get("viewportOptions") {
                    let mut b = BoxOptions::default();
                    apply_props(&mut b.layout, vc);
                    opts.viewport_options = Some(b);
                }
                if let Some(cc) = o.get("contentOptions") {
                    let mut b = BoxOptions::default();
                    apply_props(&mut b.layout, cc);
                    opts.content_options = Some(b);
                }
                if let Some(wo) = o.get("wrapperOptions") {
                    let mut b = BoxOptions::default();
                    apply_props(&mut b.layout, wo);
                    opts.wrapper_options = Some(b);
                }
                PendingNode::ScrollBox(Box::new(opts))
            }
            other => anyhow::bail!("unknown op: {other}"),
        };
        let Some(id) = &step.id else {
            anyhow::bail!("op {} needs an id", step.op);
        };
        match step.parent.as_deref() {
            None | Some("root") => roots.push(id.clone()),
            Some(p) => children_of
                .entry(p.to_owned())
                .or_default()
                .push(id.clone()),
        }
        by_id.insert(id.clone(), node);
    }

    mount(&mut renderer, &mut by_id, &children_of, None, &roots);

    renderer.render();

    let buf = &renderer.current_render_buffer;
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
        oracle: "opentui-core".into(),
        renderer: Size {
            width: buf.width,
            height: buf.height,
        },
        lines,
        char_frame: buf.char_frame(),
        // the full emission across frames — mirrors the oracle's
        // recorded stdout stream (multi-frame scroll scenarios)
        sgr_bytes: renderer.full_sgr_stream().bytes().collect(),
    })
}

/// Recursively mount staged nodes: `parent: None` mounts under the
/// renderer root; otherwise the node nests inside `parent` (ScrollBox ids
/// route into their content — ScrollBox.add, ScrollBox.ts:514-516).
fn mount(
    renderer: &mut CliRenderer,
    by_id: &mut std::collections::HashMap<String, PendingNode>,
    children_of: &std::collections::HashMap<String, Vec<String>>,
    parent: Option<&str>,
    ids: &[String],
) {
    for id in ids {
        let r = match by_id.remove(id) {
            Some(PendingNode::Text(opts)) => renderer.make_text(id, &opts),
            Some(PendingNode::Box(opts)) => renderer.make_box(id, &opts),
            Some(PendingNode::ScrollBox(mut opts)) => renderer.make_scrollbox(id, &mut opts),
            None => continue,
        };
        match parent {
            None => renderer.add(r),
            Some(p) => {
                if !renderer.mount_child(p, r) {
                    // unknown parent: fall back to root (JS would throw)
                    eprintln!("warning: parent {p} not found for {id}; mounting at root");
                }
            }
        }
        if let Some(kids) = children_of.get(id) {
            mount(renderer, by_id, children_of, Some(id), kids);
        }
    }
}

/// The capture shape shared by both sides (field names match the JS dump).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Capture {
    pub oracle: String,
    pub renderer: Size,
    pub lines: Vec<Line>,
    #[serde(rename = "charFrame")]
    pub char_frame: String,
    #[serde(rename = "sgrBytes")]
    pub sgr_bytes: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Line {
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub text: String,
    pub fg: Vec<u16>,
    pub bg: Vec<u16>,
    pub attributes: u32,
    pub width: u32,
}
