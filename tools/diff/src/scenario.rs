//! Scenario loading + the Rust-side capture (mirrors js/driver.ts).

use anyhow::{Context, Result};
use opentui_core::renderables::box_::BorderSpec;
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
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub options: serde_json::Value,
}

pub fn load(path: &std::path::Path) -> Result<Scenario> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading scenario {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing scenario {}", path.display()))
}

fn opt_f32(v: &serde_json::Value, key: &str) -> Option<opentui_core::layout::Dimension> {
    v.get(key).and_then(|x| x.as_f64()).map(|f| opentui_core::layout::Dimension::Points(f as f32))
}

/// One scenario node pending mount (kind + options + children).
enum PendingNode {
    Text { id: String, opts: TextOptions },
    Box { id: String, opts: BoxOptions },
}

/// Build + render the scenario with opentui-core, dump the same capture
/// shape the JS oracle emits.
pub fn render_rust(scenario: &Scenario) -> Result<Capture> {
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: scenario.width,
        height: scenario.height,
        ..Default::default()
    });

    // Stage nodes with their parent links, then mount recursively in
    // step order (children after parents, like the JS driver's adds).
    let mut by_id: std::collections::HashMap<String, PendingNode> = std::collections::HashMap::new();
    let mut children_of: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut roots: Vec<String> = Vec::new();
    for step in &scenario.steps {
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
                    opts.wrap_mode = Some(WrapMode::from_str(w));
                }
                PendingNode::Text { id: step.id.clone(), opts }
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
                    opts.layout.padding =
                        Some(opentui_core::layout::Dimension::Points(p as f32));
                }
                PendingNode::Box { id: step.id.clone(), opts }
            }
            other => anyhow::bail!("unknown op: {other}"),
        };
        match step.parent.as_deref() {
            None | Some("root") => roots.push(step.id.clone()),
            Some(p) => children_of.entry(p.to_owned()).or_default().push(step.id.clone()),
        }
        by_id.insert(step.id.clone(), node);
    }

    mount(&mut renderer, &by_id, &children_of, None, &roots);

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
        renderer: Size { width: buf.width, height: buf.height },
        lines,
        char_frame: buf.char_frame(),
        sgr_bytes: renderer.last_frame_sgr().bytes().collect(),
    })
}

/// Recursively mount staged nodes: `parent: None` mounts under the
/// renderer root; otherwise the node nests inside `parent`.
fn mount(
    renderer: &mut CliRenderer,
    by_id: &std::collections::HashMap<String, PendingNode>,
    children_of: &std::collections::HashMap<String, Vec<String>>,
    parent: Option<&str>,
    ids: &[String],
) {
    for id in ids {
        let r = match by_id.get(id) {
            Some(PendingNode::Text { opts, .. }) => renderer.make_text(id, opts),
            Some(PendingNode::Box { opts, .. }) => renderer.make_box(id, opts),
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
