//! Tree-scenario desugaring — the `tree` scenario form.
//!
//! A tree scenario describes a WHOLE renderable tree in one JSON value
//! (the shape the oc-tui tree capturer will emit) instead of a step
//! script. Both engines rebuild the tree and compare the settle frame,
//! so the desugar must be EXACTLY the same here and in the JS driver
//! (`desugarTree` in js/driver.ts) — same ids, same option spellings,
//! same mount order (document/DFS preorder = z-order, later on top).
//!
//! Contract (v1 — tools/diff/README "tree scenarios"):
//! ```json
//! {
//!   "name": "...", "width": 80, "height": 24,
//!   "tree": [                    // forest; single node = one-element array
//!     {
//!       "id": "hdr",             // optional; default n<preorder-index>
//!       "kind": "box",           // box|text|code|markdown|diff
//!       "rect": { "x": 0, "y": 0, "w": 80, "h": 3 },   // ABSOLUTE screen rect
//!       "style": {               // fg/bg/attributes apply per kind;
//!         "fg": "#fff", "bg": "#112233", "attributes": ["bold"],
//!         "border": true, "borderColor": "#0f0", "borderStyle": "round",
//!         "shouldFill": true, "title": "t", "titleAlignment": "center"
//!       },
//!       "content": "…",          // text/code/markdown source (chunk array ok for text)
//!       "diff": "…",             // diff kind: the unified diff source
//!       "options": { … },        // RAW passthrough, merged last (wrapMode,
//!                                //  filetype, styles, view, colors, …)
//!       "children": [ … ]        // FLOW children mounted INSIDE this node
//!     }
//!   ]
//! }
//! ```
//!
//! Desugar rules:
//! * every TOP-LEVEL node mounts ABSOLUTELY under `root`
//!   (`position:"absolute", left:x, top:y, width:w, height:h`) in
//!   array order — later siblings paint over earlier ones;
//! * `box`/`text` mount directly; `code`/`markdown`/`diff` mount inside
//!   a transparent absolute wrapper box at the same rect (their options
//!   carry no layout), sized `100%×100%`;
//! * `children` are flow-mounted inside the parent (parent must be a
//!   box or a wrapper) in order; a child may NOT carry `rect` (nesting
//!   is flow, not absolute — a deliberate contract simplification);
//! * children of kind `text` may carry `content`; `style` maps per kind
//!   exactly like the top level.
//!
//! The desugar runs on raw JSON before the `Scenario` struct parses,
//! so the step pipeline (staging, lazy mount, settle, capture, oracle
//! fingerprint) is reused unchanged.

use anyhow::{Context, Result};

/// Desugar a scenario JSON value with a `tree` field into the step
/// form. Scenarios without `tree` pass through untouched.
pub fn desugar_scenario(v: serde_json::Value) -> Result<serde_json::Value> {
    let Some(tree) = v.get("tree").cloned() else {
        return Ok(v);
    };
    let forest = match tree {
        serde_json::Value::Array(nodes) => nodes,
        node @ serde_json::Value::Object(_) => vec![node],
        other => anyhow::bail!("tree must be a node object or an array of nodes, got {other}"),
    };
    let mut steps = Vec::new();
    let mut counter = 0usize;
    for node in forest {
        emit_forest_node(node, "root", &mut steps, &mut counter, true)?;
    }
    let mut out = v;
    let obj = out
        .as_object_mut()
        .context("scenario must be a JSON object")?;
    obj.remove("tree");
    obj.insert("steps".into(), serde_json::Value::Array(steps));
    Ok(out)
}

/// One node → steps. `absolute` = mounted absolutely under `parent`
/// (top level); otherwise a flow child inside `parent`.
fn emit_forest_node(
    node: serde_json::Value,
    parent: &str,
    steps: &mut Vec<serde_json::Value>,
    counter: &mut usize,
    absolute: bool,
) -> Result<String> {
    let obj = node
        .as_object()
        .with_context(|| format!("tree node must be an object, got {node}"))?;
    let id = obj
        .get("id")
        .and_then(|i| i.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            *counter += 1;
            format!("n{counter}")
        });
    let kind = obj
        .get("kind")
        .and_then(|k| k.as_str())
        .context("tree node needs kind")?
        .to_owned();
    let style = obj.get("style").cloned().unwrap_or(serde_json::Value::Null);
    let passthrough = obj.get("options").cloned().unwrap_or(serde_json::Value::Null);

    // absolute placement options (position/left/top/width/height)
    let mut rect_opts = serde_json::Map::new();
    if absolute {
        let rect = obj
            .get("rect")
            .and_then(|r| r.as_object())
            .context("top-level tree node needs rect {x,y,w,h}")?;
        let get = |k: &str| {
            rect.get(k)
                .and_then(|n| n.as_f64())
                .with_context(|| format!("rect.{k} must be a number"))
        };
        rect_opts.insert("position".into(), "absolute".into());
        rect_opts.insert("left".into(), get("x")?.into());
        rect_opts.insert("top".into(), get("y")?.into());
        rect_opts.insert("width".into(), get("w")?.into());
        rect_opts.insert("height".into(), get("h")?.into());
    } else {
        if obj.get("rect").is_some() {
            anyhow::bail!("tree child {id} carries rect — nesting is flow, not absolute");
        }
        // flow children may still size themselves
        if let Some(w) = obj.get("width") {
            rect_opts.insert("width".into(), w.clone());
        }
        if let Some(h) = obj.get("height") {
            rect_opts.insert("height".into(), h.clone());
        }
    }

    let push = |op: &str,
                id: &str,
                parent: &str,
                opts: serde_json::Map<String, serde_json::Value>,
                steps: &mut Vec<serde_json::Value>| {
        steps.push(serde_json::json!({
            "op": op, "id": id, "parent": parent, "options": opts,
        }));
    };

    match kind.as_str() {
        "box" => {
            let mut opts = rect_opts;
            merge_style_box(&style, &mut opts)?;
            merge_passthrough(&passthrough, &mut opts);
            push("box", &id, parent, opts, steps);
            emit_children(obj, &id, steps, counter)?;
        }
        "text" => {
            let mut opts = rect_opts;
            if let Some(c) = obj.get("content") {
                opts.insert("content".into(), c.clone());
            }
            merge_style_text(&style, &mut opts)?;
            merge_passthrough(&passthrough, &mut opts);
            push("text", &id, parent, opts, steps);
            // text children would need TextNodeRenderable semantics —
            // not part of the v1 tree contract
            if obj.get("children").is_some_and(|c| c.as_array().is_some_and(|a| !a.is_empty())) {
                anyhow::bail!("tree node {id}: text nodes take no children (use content)");
            }
        }
        "code" | "markdown" | "diff" => {
            // these renderables carry no layout options — wrap in an
            // absolute transparent box at the same rect
            let wrapper = format!("{id}-w");
            let mut wopts = rect_opts;
            wopts.insert("shouldFill".into(), false.into());
            if let Some(bg) = style.get("bg") {
                wopts.insert("backgroundColor".into(), bg.clone());
            }
            push("box", &wrapper, parent, wopts, steps);
            let mut opts = serde_json::Map::new();
            opts.insert("width".into(), "100%".into());
            opts.insert("height".into(), "100%".into());
            if let Some(c) = obj.get("content") {
                opts.insert("content".into(), c.clone());
            }
            if let Some(d) = obj.get("diff") {
                opts.insert("diff".into(), d.clone());
            }
            if let Some(fg) = style.get("fg") {
                opts.insert("fg".into(), fg.clone());
            }
            if let Some(bg) = style.get("bg") {
                opts.insert("bg".into(), bg.clone());
            }
            merge_passthrough(&passthrough, &mut opts);
            push(&kind, &id, &wrapper, opts, steps);
            emit_children(obj, &wrapper, steps, counter)?;
        }
        other => anyhow::bail!("tree node {id}: unknown kind {other}"),
    }
    Ok(id)
}

fn emit_children(
    obj: &serde_json::Map<String, serde_json::Value>,
    mount_under: &str,
    steps: &mut Vec<serde_json::Value>,
    counter: &mut usize,
) -> Result<()> {
    let Some(children) = obj.get("children").and_then(|c| c.as_array()) else {
        return Ok(());
    };
    for child in children {
        emit_forest_node(child.clone(), mount_under, steps, counter, false)?;
    }
    Ok(())
}

/// `style` → box option spellings (same names the step DSL uses).
fn merge_style_box(style: &serde_json::Value, opts: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let Some(s) = style.as_object() else { return Ok(()) };
    for (from, to) in [
        ("bg", "backgroundColor"),
        ("border", "border"),
        ("borderColor", "borderColor"),
        ("borderStyle", "borderStyle"),
        ("shouldFill", "shouldFill"),
        ("title", "title"),
        ("titleAlignment", "titleAlignment"),
    ] {
        if let Some(v) = s.get(from) {
            opts.insert(to.into(), v.clone());
        }
    }
    Ok(())
}

/// `style` → text option spellings.
fn merge_style_text(style: &serde_json::Value, opts: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let Some(s) = style.as_object() else { return Ok(()) };
    for key in ["fg", "bg", "attributes"] {
        if let Some(v) = s.get(key) {
            opts.insert(key.into(), v.clone());
        }
    }
    Ok(())
}

fn merge_passthrough(
    passthrough: &serde_json::Value,
    opts: &mut serde_json::Map<String, serde_json::Value>,
) {
    if let Some(p) = passthrough.as_object() {
        for (k, v) in p {
            opts.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn steps_of(v: serde_json::Value) -> Vec<serde_json::Value> {
        desugar_scenario(v).unwrap()["steps"].as_array().unwrap().clone()
    }

    #[test]
    fn forest_of_two_absolute_boxes_in_order() {
        let steps = steps_of(json!({
            "name": "t", "width": 20, "height": 6,
            "tree": [
                { "kind": "box", "rect": {"x":0,"y":0,"w":10,"h":3},
                  "style": {"bg":"#111111","border":true,"borderColor":"#00FF00"} },
                { "kind": "box", "rect": {"x":5,"y":1,"w":10,"h":3},
                  "style": {"bg":"#222222"} }
            ]
        }));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["op"], "box");
        assert_eq!(steps[0]["id"], "n1");
        assert_eq!(steps[0]["parent"], "root");
        assert_eq!(steps[0]["options"]["position"], "absolute");
        assert_eq!(steps[0]["options"]["left"].as_f64(), Some(0.0));
        assert_eq!(steps[0]["options"]["top"].as_f64(), Some(0.0));
        assert_eq!(steps[0]["options"]["width"].as_f64(), Some(10.0));
        assert_eq!(steps[0]["options"]["height"].as_f64(), Some(3.0));
        assert_eq!(steps[0]["options"]["backgroundColor"], "#111111");
        assert_eq!(steps[0]["options"]["border"], true);
        assert_eq!(steps[1]["options"]["left"].as_f64(), Some(5.0));
        assert_eq!(steps[1]["options"]["top"].as_f64(), Some(1.0));
    }

    #[test]
    fn explicit_ids_and_flow_children() {
        let steps = steps_of(json!({
            "name": "t", "width": 20, "height": 6,
            "tree": [
                { "id": "panel", "kind": "box", "rect": {"x":1,"y":1,"w":14,"h":4},
                  "style": {"border": true},
                  "children": [
                    { "kind": "text", "content": "inner",
                      "style": {"fg":"#FFAA00","attributes":["bold"]} }
                  ] }
            ]
        }));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["id"], "panel");
        assert_eq!(steps[1]["op"], "text");
        assert_eq!(steps[1]["parent"], "panel");
        // flow child: no absolute placement, style mapped
        assert!(steps[1]["options"].get("position").is_none());
        assert_eq!(steps[1]["options"]["fg"], "#FFAA00");
        assert_eq!(steps[1]["options"]["attributes"], json!(["bold"]));
    }

    #[test]
    fn markdown_wraps_in_absolute_box() {
        let steps = steps_of(json!({
            "name": "t", "width": 40, "height": 10,
            "tree": [
                { "id": "md", "kind": "markdown", "rect": {"x":2,"y":0,"w":30,"h":8},
                  "content": "# hi",
                  "options": {"conceal": true} }
            ]
        }));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["op"], "box");
        assert_eq!(steps[0]["id"], "md-w");
        assert_eq!(steps[0]["options"]["left"].as_f64(), Some(2.0));
        assert_eq!(steps[0]["options"]["shouldFill"], false);
        assert_eq!(steps[1]["op"], "markdown");
        assert_eq!(steps[1]["parent"], "md-w");
        assert_eq!(steps[1]["options"]["width"], "100%");
        assert_eq!(steps[1]["options"]["height"], "100%");
        assert_eq!(steps[1]["options"]["conceal"], true);
    }

    #[test]
    fn child_rect_rejected() {
        let err = desugar_scenario(json!({
            "name":"t","width":10,"height":4,
            "tree":[{"kind":"box","rect":{"x":0,"y":0,"w":8,"h":3},
                      "children":[{"kind":"text","rect":{"x":1,"y":1,"w":4,"h":1}}]}]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("flow, not absolute"));
    }

    #[test]
    fn text_children_rejected() {
        let err = desugar_scenario(json!({
            "name":"t","width":10,"height":4,
            "tree":[{"kind":"text","rect":{"x":0,"y":0,"w":8,"h":3},
                      "content":"a","children":[{"kind":"box"}]}]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("no children"));
    }

    #[test]
    fn passthrough_merges_last() {
        let steps = steps_of(json!({
            "name":"t","width":10,"height":4,
            "tree":[{"kind":"box","rect":{"x":0,"y":0,"w":8,"h":3},
                      "style":{"bg":"#111111"},"options":{"backgroundColor":"#222222"}}]
        }));
        assert_eq!(steps[0]["options"]["backgroundColor"], "#222222");
    }

    /// The Rust-side rebuilder: a tree scenario desugars, loads, and
    /// replays into a renderer holding every declared node (wrapper
    /// boxes included) at the declared absolute rects/styles.
    #[test]
    fn rust_rebuild_nodes_rects_and_styles() {
        let json = desugar_scenario(json!({
            "name":"rebuild","width":24,"height":8,
            "tree":[
                { "id":"panel", "kind":"box",
                  "rect":{"x":1,"y":1,"w":20,"h":6},
                  "style":{"border":true,"borderColor":"#00FF00","bg":"#112233"},
                  "children":[{"id":"lbl","kind":"text","content":"hi","style":{"fg":"#FFAA00"}}] },
                { "id":"md", "kind":"markdown",
                  "rect":{"x":2,"y":7,"w":20,"h":1}, "content":"x" }
            ]
        }))
        .unwrap();
        let sc: crate::scenario::Scenario = serde_json::from_value(json).unwrap();
        let r = crate::scenario::replay(&sc).unwrap();

        // node count: panel + its text child + md + md's wrapper box
        assert!(r.has_node("panel"));
        assert!(r.has_node("md"));
        assert!(r.has_node("md-w"));
        assert!(r.has_node("lbl")); // the flow text child

        // rect assertions from the settled frame: the border lands at
        // (1,1) and spans 20x6; the md wrapper occupies row 7
        let frame = r.current_render_buffer.char_frame();
        let lines: Vec<&str> = frame.lines().collect();
        assert_eq!(lines.len(), 8);
        assert!(lines[1].starts_with(" ┌"), "panel border top at row 1");
        assert!(lines[6].starts_with(" └"), "panel border bottom at row 6");
        assert!(lines[2].contains("hi"), "flow text child rendered inside panel");
    }
}
