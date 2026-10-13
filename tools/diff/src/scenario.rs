//! Scenario loading + the Rust-side capture (mirrors js/driver.ts).

use anyhow::{Context, Result};
use opentui_core::color::parse_color;
use opentui_core::editor::SelectionBehavior;
use opentui_core::events::KeyEvent;
use opentui_core::input::InputOptions;
use opentui_core::parse_keypress::{KeyEventType, KeySource, ParsedKey};
use opentui_core::renderables::box_::{BorderSpec, BoxOptions};
use opentui_core::renderables::scrollbar::ScrollUnit;
use opentui_core::renderables::scrollbox::{ScrollBoxOptions, StickyStart};
use opentui_core::renderables::text::{ContentInput, TextOptions, WrapMode};
use opentui_core::renderables::textarea::TextareaOptions;
use opentui_core::renderer::{create_cli_renderer, CliRenderer, CliRendererConfig};
use opentui_core::styled_text::{StyledText, TextChunk};
use opentui_core::text_buffer::TextAlign;
use opentui_core::text_node::{TextNodeOptions, TextNodeRenderable};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct Scenario {
    #[allow(dead_code)]
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// "core" (default — direct renderable mounts) or "solid" (the
    /// opentui-solid reconciler vs the real @opentui/solid).
    #[serde(default)]
    pub mode: Option<String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub struct Step {
    pub op: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    /// target id for `text-add`/`ta-op` (else the step's own id)
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub options: serde_json::Value,
    #[serde(default)]
    pub text: Option<String>,
    pub prop: Option<String>,
    /// A step value (signal/prop/set) — any JSON; the core-mode `set`
    /// op reads it as a number.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// `key` op: raw bytes through the renderer's input pipeline
    #[serde(default)]
    pub sequence: Option<Vec<u8>>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub resize_width: Option<u32>,
    #[serde(default)]
    pub resize_height: Option<u32>,
    #[serde(default)]
    pub resize: Option<(u32, u32)>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    // solid-vocabulary fields (mode: "solid")
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub children: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub signal: Option<String>,
    #[serde(default)]
    pub item: Option<serde_json::Value>,
    /// `textarea`/`ta-op`: ordered edit ops (the shared vocabulary)
    #[serde(default, rename = "textOps")]
    pub text_ops: Vec<TextOp>,
}

/// One `textOps` entry — the EditBufferRenderable/Textarea public
/// surface, mirrored 1:1 with the JS driver's `textareaOp`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextOp {
    pub op: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub row: Option<u32>,
    #[serde(default)]
    pub col: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub select: Option<bool>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub start: Option<u32>,
    #[serde(default)]
    pub end: Option<u32>,
    #[serde(default)]
    pub start_row: Option<u32>,
    #[serde(default)]
    pub start_col: Option<u32>,
    #[serde(default)]
    pub end_row: Option<u32>,
    #[serde(default)]
    pub end_col: Option<u32>,
    #[serde(default)]
    pub anchor_x: Option<i32>,
    #[serde(default)]
    pub anchor_y: Option<i32>,
    #[serde(default)]
    pub focus_x: Option<i32>,
    #[serde(default)]
    pub focus_y: Option<i32>,
    #[serde(default)]
    pub update_cursor: Option<bool>,
    #[serde(default)]
    pub follow_cursor: Option<bool>,
    #[serde(default)]
    pub behavior: Option<String>,
    #[serde(default)]
    pub x: Option<u32>,
    #[serde(default)]
    pub y: Option<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub move_cursor: Option<bool>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub delta: Option<u32>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    // key op
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub ctrl: Option<bool>,
    #[serde(default)]
    pub shift: Option<bool>,
    #[serde(default)]
    pub meta: Option<bool>,
    #[serde(default, rename = "super")]
    pub super_key: Option<bool>,
    #[serde(default)]
    pub sequence: Option<String>,
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

/// Attribute names → bits — the JSON spelling of `createTextAttributes`
/// (utils.ts:4-29); raw numbers pass through.
fn parse_attributes(v: Option<&serde_json::Value>) -> Result<u32> {
    let Some(v) = v else { return Ok(0) };
    match v {
        serde_json::Value::Number(n) => Ok(n.as_u64().unwrap_or(0) as u32),
        serde_json::Value::Array(names) => {
            let mut bits = 0u32;
            for name in names {
                let Some(name) = name.as_str() else {
                    anyhow::bail!("attribute names must be strings: {name}")
                };
                bits |= match name {
                    "bold" => 1,
                    "dim" => 1 << 1,
                    "italic" => 1 << 2,
                    "underline" => 1 << 3,
                    "blink" => 1 << 4,
                    "inverse" | "reverse" => 1 << 5,
                    "hidden" => 1 << 6,
                    "strikethrough" => 1 << 7,
                    other => anyhow::bail!("unknown attribute: {other}"),
                };
            }
            Ok(bits)
        }
        _ => anyhow::bail!("attributes must be a number or name array"),
    }
}

/// One styled chunk from JSON: `{text, fg?, bg?, attributes?}` →
/// `TextChunk` (fg/bg parsed like the driver's `parseColor`).
fn parse_chunk(v: &serde_json::Value) -> Result<TextChunk> {
    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .context("chunk needs text")?;
    Ok(TextChunk {
        text: text.to_owned(),
        fg: v.get("fg").and_then(|c| c.as_str()).map(parse_color),
        bg: v.get("bg").and_then(|c| c.as_str()).map(parse_color),
        attributes: parse_attributes(v.get("attributes"))?,
        link: None,
    })
}

/// `content` — string | chunk array. Strings keep their identity
/// (`ContentInput::Str`): upstream's `_hasManualStyledText` treats an
/// empty string as NOT manual (Text.ts:32).
fn parse_styled_content(v: &serde_json::Value) -> Result<ContentInput> {
    match v {
        serde_json::Value::Null => Ok(ContentInput::None),
        serde_json::Value::String(s) => Ok(ContentInput::Str(s.clone())),
        serde_json::Value::Array(chunks) => Ok(ContentInput::Styled(StyledText::new(
            chunks.iter().map(parse_chunk).collect::<Result<_>>()?,
        ))),
        _ => anyhow::bail!("content must be a string or chunk array"),
    }
}

/// A node-tree child: string | node object (`{fg?, bg?, attributes?,
/// children?}` → `TextNodeRenderable`).
fn parse_node_tree(v: &serde_json::Value, id: &str) -> Result<TextNodeRenderable> {
    let options = TextNodeOptions {
        fg: v.get("fg").and_then(|c| c.as_str()).map(str::to_owned),
        bg: v.get("bg").and_then(|c| c.as_str()).map(str::to_owned),
        attributes: Some(parse_attributes(v.get("attributes"))?),
        link: None,
    };
    let mut node = TextNodeRenderable::new(id, options);
    if let Some(children) = v.get("children").and_then(|c| c.as_array()) {
        for (i, child) in children.iter().enumerate() {
            match child {
                serde_json::Value::String(s) => {
                    node.add_text(s);
                }
                serde_json::Value::Object(_) => {
                    let child_id = format!("{id}-c{i}");
                    node.add_node(parse_node_tree(child, &child_id)?);
                }
                _ => anyhow::bail!("node children must be strings or objects"),
            }
        }
    }
    Ok(node)
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
/// lives in the staging maps; variants carry only their options. Text
/// entries stay mutable so later `text-node`/`text-add` steps patch the
/// same options the mount reads (the JS driver patches the live object).
enum PendingNode {
    Text(TextOptions),
    Box(BoxOptions),
    ScrollBox(Box<ScrollBoxOptions>),
    Input(InputOptions),
    Code(
        Box<opentui_core::renderables::code::CodeOptions>,
        opentui_core::syntax_style::SyntaxStyle,
    ),
    Markdown(
        Box<opentui_core::renderables::markdown::MarkdownOptions>,
        opentui_core::syntax_style::SyntaxStyle,
    ),
}

/// `styles` option → `SyntaxStyle.fromStyles` (syntax-style.ts:106-114).
fn parse_syntax_style(o: &serde_json::Value) -> Result<opentui_core::syntax_style::SyntaxStyle> {
    use opentui_core::syntax_style::{StyleDefinitionInput, SyntaxStyle};
    let Some(styles) = o.get("styles").and_then(|v| v.as_object()) else {
        return Ok(SyntaxStyle::create());
    };
    let mut style = SyntaxStyle::create();
    for (name, def) in styles {
        let input = StyleDefinitionInput {
            fg: def.get("fg").and_then(|c| c.as_str()).map(str::to_owned),
            bg: def.get("bg").and_then(|c| c.as_str()).map(str::to_owned),
            bold: def.get("bold").and_then(|b| b.as_bool()),
            italic: def.get("italic").and_then(|b| b.as_bool()),
            underline: def.get("underline").and_then(|b| b.as_bool()),
            dim: def.get("dim").and_then(|b| b.as_bool()),
        };
        style.register_style(name, &input);
    }
    Ok(style)
}

fn text_options_from_json(o: &serde_json::Value) -> Result<TextOptions> {
    let mut opts = TextOptions {
        content: parse_styled_content(o.get("content").unwrap_or(&serde_json::Value::Null))?,
        ..Default::default()
    };
    if let Some(tree) = o.get("nodeContent") {
        opts.node_tree = Some(parse_node_tree(tree, "root-node")?);
    }
    if let Some(fg) = o.get("fg").and_then(|v| v.as_str()) {
        opts.fg = Some(parse_color(fg));
    }
    if let Some(bg) = o.get("bg").and_then(|v| v.as_str()) {
        opts.bg = Some(parse_color(bg));
    }
    if let Some(attrs) = o.get("attributes") {
        opts.attributes = parse_attributes(Some(attrs))?;
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
    if let Some(a) = o.get("textAlign").and_then(|v| v.as_str()) {
        opts.text_align = Some(TextAlign::parse(a));
    }
    if let Some(t) = o.get("tabIndicator") {
        opts.tab_indicator = match t {
            serde_json::Value::String(s) => Some(s.chars().next().map_or(0, |c| c as u32)),
            serde_json::Value::Number(n) => Some(n.as_u64().unwrap_or(0) as u32),
            _ => anyhow::bail!("tabIndicator must be a string or number"),
        };
    }
    if let Some(c) = o.get("tabIndicatorColor").and_then(|v| v.as_str()) {
        opts.tab_indicator_color = Some(parse_color(c));
    }
    if let Some(t) = o.get("truncate").and_then(|v| v.as_bool()) {
        opts.truncate = t;
    }
    Ok(opts)
}

/// Build + render the scenario with opentui-core, dump the same capture
/// shape the JS oracle emits.
///
/// Execution is step-sequential (mirroring the JS driver): legacy
/// text/box nodes stage and mount lazily at the next flush point (a
/// textarea/ta-op/frame step or the end) — behaviorally identical to
/// the old single-pass mount when a scenario has no flush points.
pub fn render_rust(scenario: &Scenario) -> Result<Capture> {
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: scenario.width,
        height: scenario.height,
        ..Default::default()
    });

    // Stage nodes with their parent links; mount lazily — before the
    // first action step (flush/frame/set/call/resize/textarea/ta-op)
    // and once more at the end — so scripted mutations observe the
    // mounted tree like the JS driver's strictly sequential adds.
    let mut by_id: HashMap<String, PendingNode> = HashMap::new();
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: Vec<String> = Vec::new();

    for step in &scenario.steps {
        match step.op.as_str() {
            "flush" | "frame" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                renderer.render();
                continue;
            }
            "focus" => {
                // focus a renderable (mount staged nodes first so the
                // target exists — upstream focuses the live instance)
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let id = step.id.clone().context("focus step needs id")?;
                renderer.focus_renderable(&id);
                continue;
            }
            "key" => {
                // raw bytes through the REAL stdin -> parser -> key
                // handler pipeline (the JS driver emits on stdin)
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let seq = step.sequence.clone().context("key step needs sequence")?;
                renderer.feed_bytes(&seq);
                continue;
            }
            "textarea" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let opts = textarea_options_from_json(&step.options)?;
                let id = step.id.clone().context("textarea step needs id")?;
                let r = renderer.make_textarea(&id, &opts);
                match step.parent.as_deref() {
                    None | Some("root") => renderer.add(r),
                    Some(parent) => {
                        if !renderer.mount_child(parent, r) {
                            eprintln!(
                                "warning: parent {parent} not found for {id}; mounting at root"
                            );
                        }
                    }
                }
                apply_text_ops(&mut renderer, &id, &step.text_ops)?;
                continue;
            }
            "ta-op" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let target = step
                    .target
                    .clone()
                    .or_else(|| step.id.clone())
                    .context("ta-op needs target")?;
                apply_text_ops(&mut renderer, &target, &step.text_ops)?;
                continue;
            }
            "set" => {
                mount(&mut renderer, &mut by_id, &children_of, None, &roots);
                let id = step.id.clone().context("set step needs id")?;
                let prop = step.prop.clone().context("set step needs prop")?;
                let value = step
                    .value
                    .as_ref()
                    .and_then(serde_json::Value::as_f64)
                    .context("set step needs numeric value")? as f32;
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
        // text-node/text-add patch STAGED nodes (no mount, no node yield)
        match step.op.as_str() {
            "text-node" => {
                // attach a TextNodeRenderable to an existing text's tree
                // (patches the staged options — the mount reads them after)
                let target = step.parent.as_deref().context("text-node needs parent")?;
                let Some(PendingNode::Text(opts)) = by_id.get_mut(target) else {
                    anyhow::bail!("text-node parent {target} is not a text renderable");
                };
                let node_options = TextNodeOptions {
                    fg: o.get("fg").and_then(|c| c.as_str()).map(str::to_owned),
                    bg: o.get("bg").and_then(|c| c.as_str()).map(str::to_owned),
                    attributes: Some(parse_attributes(o.get("attributes"))?),
                    link: None,
                };
                let mut node = TextNodeRenderable::new(
                    step.id.clone().unwrap_or_else(|| "text-node".to_owned()),
                    node_options,
                );
                if let Some(t) = &step.text {
                    node.add_text(t);
                }
                opts.node_tree
                    .get_or_insert_with(|| {
                        TextNodeRenderable::new("root-node", TextNodeOptions::default())
                    })
                    .add_node(node);
                continue;
            }
            "text-add" => {
                let target = step.target.as_deref().context("text-add needs target")?;
                let Some(PendingNode::Text(opts)) = by_id.get_mut(target) else {
                    anyhow::bail!("text-add target {target} is not a text renderable");
                };
                let text = step.text.as_deref().context("text-add needs text")?;
                opts.node_tree
                    .get_or_insert_with(|| {
                        TextNodeRenderable::new("root-node", TextNodeOptions::default())
                    })
                    .add_text(text);
                continue;
            }
            _ => {}
        }
        let node = match step.op.as_str() {
            "text" => PendingNode::Text(text_options_from_json(o)?),
            "box" => PendingNode::Box(box_options_from_json(o)?),
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
            "input" => {
                let mut opts = InputOptions::default();
                if let Some(v) = o.get("value").and_then(|v| v.as_str()) {
                    opts.value = v.to_owned();
                }
                if let Some(v) = o.get("placeholder").and_then(|v| v.as_str()) {
                    opts.placeholder = Some(v.to_owned());
                }
                if let Some(v) = o.get("width").and_then(|v| v.as_f64()) {
                    opts.width = Some(v as u32);
                }
                if let Some(v) = o.get("maxLength").and_then(|v| v.as_u64()) {
                    opts.max_length = Some(v as usize);
                }
                if let Some(v) = o.get("textColor").and_then(|v| v.as_str()) {
                    opts.text_color = Some(opentui_core::color::parse_color(v));
                }
                if let Some(v) = o.get("focusedTextColor").and_then(|v| v.as_str()) {
                    opts.focused_text_color = Some(opentui_core::color::parse_color(v));
                }
                if let Some(v) = o.get("backgroundColor").and_then(|v| v.as_str()) {
                    opts.background_color = Some(opentui_core::color::parse_color(v));
                }
                if let Some(v) = o.get("placeholderColor").and_then(|v| v.as_str()) {
                    opts.placeholder_color = Some(opentui_core::color::parse_color(v));
                }
                PendingNode::Input(opts)
            }
            "code" => {
                let mut opts = opentui_core::renderables::code::CodeOptions {
                    content: o
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    ..Default::default()
                };
                if let Some(ft) = o.get("filetype").and_then(|v| v.as_str()) {
                    opts.filetype = Some(ft.to_owned());
                }
                if let Some(c) = o.get("conceal").and_then(|v| v.as_bool()) {
                    opts.conceal = Some(c);
                }
                if let Some(c) = o.get("fg").and_then(|v| v.as_str()) {
                    opts.fg = Some(parse_color(c));
                }
                if let Some(c) = o.get("bg").and_then(|v| v.as_str()) {
                    opts.bg = Some(parse_color(c));
                }
                let style = parse_syntax_style(o)?;
                PendingNode::Code(Box::new(opts), style)
            }
            "markdown" => {
                let mut opts = opentui_core::renderables::markdown::MarkdownOptions {
                    content: o
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    ..Default::default()
                };
                if let Some(c) = o.get("fg").and_then(|v| v.as_str()) {
                    opts.fg = Some(parse_color(c));
                }
                if let Some(c) = o.get("bg").and_then(|v| v.as_str()) {
                    opts.bg = Some(parse_color(c));
                }
                if let Some(c) = o.get("conceal").and_then(|v| v.as_bool()) {
                    opts.conceal = Some(c);
                }
                if let Some(c) = o.get("concealCode").and_then(|v| v.as_bool()) {
                    opts.conceal_code = Some(c);
                }
                let style = parse_syntax_style(o)?;
                PendingNode::Markdown(Box::new(opts), style)
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
        cursor: {
            let (col, row, visible) = renderer.terminal_cursor();
            Some(Cursor { row, col, visible })
        },
    })
}

/// The `textarea` option subset (Textarea.ts:132-143 over
/// EditBufferOptions:64-83).
fn textarea_options_from_json(o: &serde_json::Value) -> Result<TextareaOptions> {
    let mut opts = TextareaOptions::default();
    if let Some(v) = o.get("initialValue").and_then(|v| v.as_str()) {
        opts.initial_value = Some(v.to_owned());
    }
    if let Some(c) = o.get("textColor").and_then(|v| v.as_str()) {
        opts.text_color = Some(parse_color(c));
    }
    if let Some(c) = o.get("backgroundColor").and_then(|v| v.as_str()) {
        opts.background_color = Some(parse_color(c));
    }
    if let Some(c) = o.get("focusedTextColor").and_then(|v| v.as_str()) {
        opts.focused_text_color = Some(parse_color(c));
    }
    if let Some(c) = o.get("focusedBackgroundColor").and_then(|v| v.as_str()) {
        opts.focused_background_color = Some(parse_color(c));
    }
    if let Some(c) = o.get("placeholderColor").and_then(|v| v.as_str()) {
        opts.placeholder_color = Some(parse_color(c));
    }
    if let Some(p) = o.get("placeholder").and_then(|v| v.as_str()) {
        opts.placeholder = Some(vec![TextChunk {
            text: p.to_owned(),
            ..Default::default()
        }]);
    }
    if let Some(c) = o.get("selectionBg").and_then(|v| v.as_str()) {
        opts.selection_bg = Some(parse_color(c));
    }
    if let Some(c) = o.get("selectionFg").and_then(|v| v.as_str()) {
        opts.selection_fg = Some(parse_color(c));
    }
    if let Some(m) = o.get("wrapMode").and_then(|v| v.as_str()) {
        opts.wrap_mode = Some(WrapMode::parse(m));
    }
    if let Some(m) = o.get("scrollMargin").and_then(|v| v.as_f64()) {
        opts.scroll_margin = Some(m as f32);
    }
    if let Some(v) = o.get("showCursor").and_then(|v| v.as_bool()) {
        opts.show_cursor = Some(v);
    }
    if let Some(c) = o.get("cursorColor").and_then(|v| v.as_str()) {
        opts.cursor_color = Some(parse_color(c));
    }
    if let Some(w) = opt_f32(o, "width") {
        opts.width = Some(w);
    }
    if let Some(h) = opt_f32(o, "height") {
        opts.height = Some(h);
    }
    Ok(opts)
}

/// The action-name mapping for vocabulary motion/delete ops (each maps
/// to the same EditBufferRenderable method the action handler calls).
fn op_to_action(op: &str, select: bool) -> Option<&'static str> {
    Some(match op {
        "moveCursorLeft" => {
            if select {
                "select-left"
            } else {
                "move-left"
            }
        }
        "moveCursorRight" => {
            if select {
                "select-right"
            } else {
                "move-right"
            }
        }
        "moveCursorUp" => {
            if select {
                "select-up"
            } else {
                "move-up"
            }
        }
        "moveCursorDown" => {
            if select {
                "select-down"
            } else {
                "move-down"
            }
        }
        "moveWordForward" => {
            if select {
                "select-word-forward"
            } else {
                "word-forward"
            }
        }
        "moveWordBackward" => {
            if select {
                "select-word-backward"
            } else {
                "word-backward"
            }
        }
        "gotoLineHome" => {
            if select {
                "select-line-home"
            } else {
                "line-home"
            }
        }
        "gotoLineEnd" => {
            if select {
                "select-line-end"
            } else {
                "line-end"
            }
        }
        "gotoVisualLineHome" => {
            if select {
                "select-visual-line-home"
            } else {
                "visual-line-home"
            }
        }
        "gotoVisualLineEnd" => {
            if select {
                "select-visual-line-end"
            } else {
                "visual-line-end"
            }
        }
        "gotoBufferHome" => {
            if select {
                "select-buffer-home"
            } else {
                "buffer-home"
            }
        }
        "gotoBufferEnd" => {
            if select {
                "select-buffer-end"
            } else {
                "buffer-end"
            }
        }
        "selectAll" => "select-all",
        "deleteChar" => "delete",
        "deleteCharBackward" => "backspace",
        "deleteLine" => "delete-line",
        "deleteToLineEnd" => "delete-to-line-end",
        "deleteToLineStart" => "delete-to-line-start",
        "deleteWordForward" => "delete-word-forward",
        "deleteWordBackward" => "delete-word-backward",
        "newLine" => "newline",
        "undo" => "undo",
        "redo" => "redo",
        _ => return None,
    })
}

fn apply_text_ops(renderer: &mut CliRenderer, id: &str, ops: &[TextOp]) -> Result<()> {
    for op in ops {
        let select = op.select.unwrap_or(false);
        if let Some(action) = op_to_action(&op.op, select) {
            renderer.textarea_action(id, action);
            renderer.refresh_textarea_measure(id);
            continue;
        }
        match op.op.as_str() {
            "setText" => {
                let text = op.text.as_deref().unwrap_or("");
                renderer.textarea_state_mut(id, |ta| ta.set_text(text));
            }
            "replaceText" => {
                let text = op.text.as_deref().unwrap_or("");
                renderer.textarea_state_mut(id, |ta| ta.replace_text(text));
            }
            "insertText" => {
                let text = op.text.as_deref().unwrap_or("");
                renderer.textarea_state_mut(id, |ta| ta.insert_text(text));
            }
            "insertChar" => {
                let text = op.text.as_deref().unwrap_or("");
                renderer.textarea_state_mut(id, |ta| ta.insert_char(text));
            }
            "setCursor" => {
                let (row, col) = (op.row.unwrap_or(0), op.col.unwrap_or(0));
                renderer.textarea_state_mut(id, |ta| ta.set_cursor(row, col));
            }
            "setCursorByOffset" => {
                let offset = op.offset.unwrap_or(0);
                renderer.textarea_state_mut(id, |ta| ta.set_cursor_offset(offset));
            }
            "gotoLine" => {
                let line = op.line.unwrap_or(0);
                renderer.textarea_state_mut(id, |ta| ta.goto_line(line));
            }
            "deleteRange" => {
                renderer.textarea_state_mut(id, |ta| {
                    ta.delete_range(
                        op.start_row.unwrap_or(0),
                        op.start_col.unwrap_or(0),
                        op.end_row.unwrap_or(0),
                        op.end_col.unwrap_or(0),
                    );
                });
            }
            "clear" => {
                renderer.textarea_state_mut(id, |ta| ta.clear());
            }
            "setSelection" => {
                let (start, end) = (op.start.unwrap_or(0), op.end.unwrap_or(0));
                renderer.textarea_state_mut(id, |ta| ta.set_selection(start, end));
            }
            "setSelectionInclusive" => {
                let (start, end) = (op.start.unwrap_or(0), op.end.unwrap_or(0));
                renderer.textarea_state_mut(id, |ta| ta.set_selection_inclusive(start, end));
            }
            "updateSelection" => {
                let end = op.end.unwrap_or(0);
                renderer.textarea_state_mut(id, |ta| {
                    ta.view.update_selection(end, None, None);
                });
            }
            "clearSelection" => {
                renderer.textarea_state_mut(id, |ta| {
                    ta.clear_selection();
                });
            }
            "deleteSelection" => {
                renderer.textarea_state_mut(id, |ta| {
                    ta.delete_selection();
                });
            }
            "setLocalSelection" => {
                let behavior = parse_behavior(op.behavior.as_deref())?;
                renderer.textarea_state_mut(id, |ta| {
                    ta.view.set_local_selection(
                        (op.anchor_x.unwrap_or(0), op.anchor_y.unwrap_or(0)),
                        (op.focus_x.unwrap_or(0), op.focus_y.unwrap_or(0)),
                        (None, None),
                        op.update_cursor.unwrap_or(false),
                        op.follow_cursor.unwrap_or(false),
                        behavior,
                    );
                });
            }
            "updateLocalSelection" => {
                let behavior = parse_behavior(op.behavior.as_deref())?;
                renderer.textarea_state_mut(id, |ta| {
                    ta.view.update_local_selection(
                        (op.anchor_x.unwrap_or(0), op.anchor_y.unwrap_or(0)),
                        (op.focus_x.unwrap_or(0), op.focus_y.unwrap_or(0)),
                        (None, None),
                        op.update_cursor.unwrap_or(false),
                        op.follow_cursor.unwrap_or(false),
                        behavior,
                    );
                });
            }
            "setViewport" => {
                let vp = renderer
                    .textarea_state(id, |ta| ta.get_viewport())
                    .context("setViewport target missing")?;
                let (x, y) = (op.x.unwrap_or(0), op.y.unwrap_or(0));
                let (w, h) = (op.width.unwrap_or(vp.width), op.height.unwrap_or(vp.height));
                let move_cursor = op.move_cursor.unwrap_or(true);
                renderer.textarea_state_mut(id, |ta| {
                    ta.set_viewport(x, y, w, h, move_cursor);
                });
            }
            "scrollBy" => {
                let direction = op
                    .direction
                    .as_deref()
                    .context("scrollBy needs direction")?;
                let delta = op.delta.unwrap_or(0);
                renderer.textarea_state_mut(id, |ta| {
                    ta.scroll_by(direction, delta);
                });
            }
            "focus" => {
                renderer.textarea_state_mut(id, |ta| ta.focus());
            }
            "blur" => {
                renderer.textarea_state_mut(id, |ta| ta.blur());
            }
            "placeholder" => {
                let value = op.value.clone();
                renderer.textarea_state_mut(id, |ta| {
                    let chunks = value.as_ref().map(|v| {
                        vec![TextChunk {
                            text: v.clone(),
                            ..Default::default()
                        }]
                    });
                    ta.set_placeholder(chunks);
                });
            }
            "setWrapMode" => {
                let mode = op.mode.as_deref().context("setWrapMode needs mode")?;
                let mode = WrapMode::parse(mode);
                renderer.textarea_state_mut(id, |ta| ta.set_wrap_mode(mode));
            }
            "key" => {
                let name = op.name.as_deref().context("key op needs name")?;
                let key = KeyEvent::new(ParsedKey {
                    name: name.to_owned(),
                    ctrl: op.ctrl.unwrap_or(false),
                    meta: op.meta.unwrap_or(false),
                    shift: op.shift.unwrap_or(false),
                    option: false,
                    sequence: op.sequence.clone().unwrap_or_default(),
                    number: false,
                    raw: op.sequence.clone().unwrap_or_default(),
                    event_type: KeyEventType::Press,
                    source: KeySource::Raw,
                    code: None,
                    super_key: Some(op.super_key.unwrap_or(false)),
                    hyper: Some(false),
                    caps_lock: None,
                    num_lock: None,
                    base_code: None,
                    repeated: None,
                });
                renderer.textarea_key_press(id, &key);
            }
            other => anyhow::bail!("unknown textarea op: {other}"),
        }
        renderer.refresh_textarea_measure(id);
    }
    Ok(())
}

fn parse_behavior(s: Option<&str>) -> Result<SelectionBehavior> {
    Ok(match s {
        Some("word") => SelectionBehavior::Word,
        Some("line") => SelectionBehavior::Line,
        _ => SelectionBehavior::Cell,
    })
}

fn box_options_from_json(o: &serde_json::Value) -> Result<BoxOptions> {
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
    Ok(opts)
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
            Some(PendingNode::Input(opts)) => renderer.make_input(id, &opts),
            Some(PendingNode::Code(opts, style)) => renderer.make_code(id, &opts, &style),
            Some(PendingNode::Markdown(opts, style)) => renderer.make_markdown(id, &opts, &style),
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
    /// The terminal cursor after the last frame (input scenarios);
    /// `None` in older captures.
    #[serde(default)]
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub row: u32,
    pub col: u32,
    pub visible: bool,
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
