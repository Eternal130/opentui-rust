//! Contract-shape tests for the Code.ts hook surface
//! (`OnHighlightCallback`/`OnChunksCallback`, Code.ts:11-32) — lane D1c
//! item b. The upstream pipeline is async worker → hook →
//! `setStyledText`; the port runs it synchronously at construction and
//! these tests pin the settled contract: the hooks see the context and
//! can replace the payload.

use opentui_core::renderables::code::{
    build_code_document_with_hooks, CodeOptions, CodePipelineHooks, HighlightContext,
    OnHighlightCallback,
};
use opentui_core::syntax_style::SyntaxStyle;
use opentui_core::SimpleHighlight;

fn options() -> CodeOptions {
    CodeOptions {
        content: "const x = 1;".to_string(),
        filetype: Some("ts".to_string()),
        ..Default::default()
    }
}

#[test]
fn on_highlight_sees_context_and_can_replace() {
    let mut hooks = CodePipelineHooks::new();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let sink = seen.clone();
    let callback: Box<OnHighlightCallback<'_>> = Box::new(
        move |_highlights: &[SimpleHighlight], ctx: &HighlightContext<'_>| {
            *sink.lock().unwrap() = vec![ctx.content.to_string(), ctx.filetype.to_string()];
            // a synthetic replacement the chunk conversion must consume
            Some(vec![SimpleHighlight {
                start: 0,
                end: 5,
                group: "keyword.control".to_string(),
                ..Default::default()
            }])
        },
    );
    hooks.on_highlight = Some(callback);
    let (doc, highlights) =
        build_code_document_with_hooks(&options(), &SyntaxStyle::create(), &mut hooks);

    // the hook observed the HighlightContext (Code.ts:11-15)
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["const x = 1;".to_string(), "ts".to_string()]
    );
    // replacement propagated both to the chunks and the returned list
    assert_eq!(highlights.len(), 1);
    assert_eq!(highlights[0].group, "keyword.control");
    assert!(!doc.plain, "synthetic highlight forces the styled path");
    assert_eq!(
        doc.chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<String>(),
        "const x = 1;"
    );
}

#[test]
fn on_highlight_returning_none_keeps_upstream_highlights() {
    let mut hooks = CodePipelineHooks::new();
    hooks.on_highlight = Some(Box::new(
        |_h: &[SimpleHighlight], _c: &HighlightContext<'_>| None,
    ));
    let (_, highlights) =
        build_code_document_with_hooks(&options(), &SyntaxStyle::create(), &mut hooks);
    // no parser registered for "ts" in the test profile → the
    // empty (undefined) upstream result passes through unchanged
    assert!(highlights.is_empty());
}

#[test]
fn on_chunks_can_rewrite_chunks() {
    let mut hooks = CodePipelineHooks::new();
    hooks.on_highlight = Some(Box::new(
        |_h: &[SimpleHighlight], _c: &HighlightContext<'_>| {
            Some(vec![SimpleHighlight {
                start: 0,
                end: 5,
                group: "keyword.control".to_string(),
                ..Default::default()
            }])
        },
    ));
    hooks.on_chunks = Some(Box::new(|mut chunks, _ctx| {
        for chunk in &mut chunks {
            chunk.text = chunk.text.to_uppercase();
        }
        Some(chunks)
    }));
    let (doc, _) = build_code_document_with_hooks(&options(), &SyntaxStyle::create(), &mut hooks);
    assert_eq!(
        doc.chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<String>(),
        "CONST X = 1;"
    );
}

#[test]
fn default_hooks_are_noops() {
    let with_hooks = build_code_document_with_hooks(
        &options(),
        &SyntaxStyle::create(),
        &mut CodePipelineHooks::new(),
    );
    let plain =
        opentui_core::renderables::code::build_code_document(&options(), &SyntaxStyle::create());
    assert_eq!(with_hooks.1.len(), 0);
    assert_eq!(with_hooks.0.plain, plain.plain);
}
