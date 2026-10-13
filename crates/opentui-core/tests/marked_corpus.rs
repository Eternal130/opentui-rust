//! marked-corpus characterization — the Rust marked-17.0.1 port
//! (`renderables::markdown_parser`) must reproduce the real package's
//! lexer token trees byte-identically for the frozen corpus (generated
//! by `tools/diff/js/marked-corpus.ts` from `marked@17.0.1`, the exact
//! dependency of `@opentui/core@0.5.14`).

use opentui_core::renderables::markdown_parser::{self, Align, ListStart, Token};

fn simplify(token: &Token) -> serde_json::Value {
    use serde_json::json;
    let mut out = json!({ "type": token.token_type(), "raw": token.raw() });
    let mut set = |k: &str, v: serde_json::Value| {
        out.as_object_mut().unwrap().insert(k.to_owned(), v);
    };
    match token {
        Token::Code {
            text, lang, fenced, ..
        } => {
            set("text", json!(text));
            if *fenced {
                set("lang", json!(lang.clone().unwrap_or_default()));
            }
        }
        Token::Heading {
            depth,
            text,
            tokens,
            ..
        } => {
            set("depth", json!(depth));
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::Blockquote { text, tokens, .. } => {
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::List {
            ordered,
            start,
            loose,
            items,
            ..
        } => {
            set("ordered", json!(ordered));
            set(
                "start",
                match start {
                    ListStart::Empty => json!(""),
                    ListStart::Number(n) => json!(n),
                },
            );
            set("loose", json!(loose));
            set(
                "items",
                json!(items.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::ListItem {
            task,
            checked,
            loose,
            text,
            tokens,
            ..
        } => {
            set("task", json!(task));
            if let Some(checked) = checked {
                set("checked", json!(checked));
            }
            set("loose", json!(loose));
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::Table {
            align,
            header,
            rows,
            ..
        } => {
            set(
                "align",
                json!(align
                    .iter()
                    .map(|a| match a {
                        Align::Left => serde_json::json!("left"),
                        Align::Center => serde_json::json!("center"),
                        Align::Right => serde_json::json!("right"),
                        Align::None => serde_json::Value::Null,
                    })
                    .collect::<Vec<_>>()),
            );
            set(
                "header",
                json!(header.iter().map(cell_json).collect::<Vec<_>>()),
            );
            set(
                "rows",
                json!(rows
                    .iter()
                    .map(|row| row.iter().map(cell_json).collect::<Vec<_>>())
                    .collect::<Vec<_>>()),
            );
        }
        Token::Def { href, title, .. } => {
            set("href", json!(href));
            set("title", json!(title));
        }
        Token::Paragraph { text, tokens, .. } => {
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::Text { text, tokens, .. } => {
            set("text", json!(text));
            // block text tokens carry their inline tokens; inline ones
            // (empty) have no `tokens` field in marked
            if !tokens.is_empty() {
                set(
                    "tokens",
                    json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
                );
            }
        }
        Token::Escape { text, .. } => set("text", json!(text)),
        Token::Html { text, .. } => set("text", json!(text)),
        Token::Link {
            href,
            title,
            text,
            tokens,
            title_field,
            ..
        } => {
            set("href", json!(href));
            if *title_field {
                set("title", json!(title));
            }
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::Image {
            href,
            title,
            text,
            tokens,
            ..
        } => {
            set("href", json!(href));
            set("title", json!(title));
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::Strong { text, tokens, .. }
        | Token::Em { text, tokens, .. }
        | Token::Del { text, tokens, .. } => {
            set("text", json!(text));
            set(
                "tokens",
                json!(tokens.iter().map(simplify).collect::<Vec<_>>()),
            );
        }
        Token::Codespan { text, .. } => set("text", json!(text)),
        Token::Checkbox { checked, .. } => set("checked", json!(checked)),
        _ => {}
    }
    out
}

fn cell_json(cell: &opentui_core::renderables::markdown_parser::TableCell) -> serde_json::Value {
    serde_json::json!({
        "text": cell.text,
        "tokens": cell.tokens.iter().map(simplify).collect::<Vec<_>>(),
    })
}

#[test]
fn marked_corpus_matches_oracle() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/marked-corpus.json");
    let raw = std::fs::read_to_string(path).expect("marked-corpus.json");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("corpus json");

    let mut failures = 0usize;
    for case in &cases {
        let name = case["name"].as_str().unwrap_or("?");
        let content = case["content"].as_str().unwrap_or("");
        let expected = &case["tokens"];

        let produced = markdown_parser::lex(content);
        let actual: Vec<serde_json::Value> = produced.iter().map(simplify).collect();

        let actual_json = serde_json::json!(actual);
        if actual_json != *expected {
            failures += 1;
            eprintln!("== {name}: MISMATCH");
            // show a compact structural diff of the first divergence
            diff_json(expected, &actual_json, "");
        } else {
            eprintln!("== {name}: ok");
        }
    }
    assert_eq!(failures, 0, "{} corpus cases diverge from marked", failures);
}

fn diff_json(expected: &serde_json::Value, actual: &serde_json::Value, path: &str) {
    match (expected, actual) {
        (serde_json::Value::Array(e), serde_json::Value::Array(a)) => {
            if e.len() != a.len() {
                eprintln!("  {path}: len {} != {}", e.len(), a.len());
                for (i, item) in e.iter().enumerate().take(3) {
                    eprintln!(
                        "    expected[{i}]: {}",
                        serde_json::to_string(item).unwrap_or_default()
                    );
                }
                for (i, item) in a.iter().enumerate().take(3) {
                    eprintln!(
                        "    actual  [{i}]: {}",
                        serde_json::to_string(item).unwrap_or_default()
                    );
                }
                return;
            }
            for (i, (e1, a1)) in e.iter().zip(a.iter()).enumerate() {
                diff_json(e1, a1, &format!("{path}[{i}]"));
            }
        }
        (serde_json::Value::Object(e), serde_json::Value::Object(a)) => {
            for (k, v) in e {
                match a.get(k) {
                    Some(av) if av == v => {}
                    Some(av) => diff_json(v, av, &format!("{path}.{k}")),
                    None => eprintln!("  {path}.{k}: missing (expected {v})"),
                }
            }
            for k in a.keys() {
                if !e.contains_key(k) {
                    eprintln!("  {path}.{k}: unexpected");
                }
            }
        }
        _ if expected != actual => {
            eprintln!("  {path}: expected {expected} != actual {actual}");
        }
        _ => {}
    }
}
