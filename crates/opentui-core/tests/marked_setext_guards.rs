//! Probe-pinned lexer facts for marked@17.0.1's setext (lheading) and
//! paragraph-interruption semantics (G-426 ㊀/㊁ root-cause fix).
//!
//! Every expected token stream below was pinned against the real
//! `marked@17.0.1` package (the exact dependency of @opentui/core 0.5.14)
//! via `Lexer.lex(src, { gfm: true })` run under bun — see
//! /opt/opencode-rust/.superpowers/sdd/gb16/w3-report.md and the
//! gb17 probe logs.

use opentui_core::renderables::markdown_parser::{lex, Token};

fn shapes(tokens: &[Token]) -> Vec<(&'static str, String)> {
    tokens
        .iter()
        .map(|t| (t.token_type(), t.raw().to_owned()))
        .collect()
}

#[test]
fn setext_underline_after_plain_paragraph() {
    // probe a) `para\n-\n` — a bare `-` right after a plain paragraph
    // line IS a setext h2 underline in marked@17.
    let tokens = lex("para\n-\n");
    assert_eq!(shapes(&tokens), vec![("heading", "para\n-\n".to_owned())]);
    assert!(matches!(&tokens[0], Token::Heading { depth: 2, .. }));
}

#[test]
fn normal_list_interrupts_paragraph() {
    // probe b) `para\n- item\n` — bullet+space interrupts.
    let tokens = lex("para\n- item\n");
    assert_eq!(
        shapes(&tokens),
        vec![
            ("paragraph", "para\n".to_owned()),
            ("list", "- item\n".to_owned()),
        ]
    );
}

#[test]
fn blank_separated_empty_list_item() {
    // probe c) `para\n\n-\n` — blank-line separated empty list item.
    let tokens = lex("para\n\n-\n");
    assert_eq!(
        shapes(&tokens),
        vec![
            ("paragraph", "para".to_owned()),
            ("space", "\n\n".to_owned()),
            ("list", "-\n".to_owned()),
        ]
    );
}

#[test]
fn indented_continuation_blocks_setext_and_empty_item_breaks() {
    // `aaa\n    ind\n-\ntext\n` — the indented (blockCode) line cannot be
    // setext content, so `-` is NOT an underline: paragraph + empty list
    // item + paragraph. The indented line lazily continues the paragraph
    // (marked's code-into-paragraph merge keeps it in the raw).
    let tokens = lex("aaa\n    ind\n-\ntext\n");
    assert_eq!(
        shapes(&tokens),
        vec![
            ("paragraph", "aaa\n    ind\n".to_owned()),
            ("list", "-\n".to_owned()),
            ("paragraph", "text\n".to_owned()),
        ]
    );
}

#[test]
fn g426_corpus_shape_message_one() {
    // The G-426 recorded corpus (rec-20261114-205531-bb7f1e74) message-1
    // tail: paragraph … indented line … then `-\n---\n` must lex as
    // paragraph + empty list + hr (NOT a setext heading swallowing it).
    let src = "---xxxx\nxx xxxx-- -\n    xxxxxxx---xxxxx xxxxx---\n-\n---\n";
    let tokens = lex(src);
    assert_eq!(
        shapes(&tokens),
        vec![
            (
                "paragraph",
                "---xxxx\nxx xxxx-- -\n    xxxxxxx---xxxxx xxxxx---\n".to_owned()
            ),
            ("list", "-\n".to_owned()),
            ("hr", "---\n".to_owned()),
        ]
    );
}

#[test]
fn g426_corpus_shape_message_two() {
    // Corpus message-2 tail: bullet+space list followed by `---` hr.
    let src = "---xxxx\n- xxx xxx - xxxxxxx-xxxxx-xxx---\n---\n";
    let tokens = lex(src);
    assert_eq!(
        shapes(&tokens),
        vec![
            ("paragraph", "---xxxx\n".to_owned()),
            ("list", "- xxx xxx - xxxxxxx-xxxxx-xxx---\n".to_owned()),
            ("hr", "---\n".to_owned()),
        ]
    );
}

#[test]
fn setext_regression_multiline_and_eq() {
    // plain multiline content still closes under both underline chars
    let tokens = lex("aaa\nbbb\n-\n");
    assert_eq!(
        shapes(&tokens),
        vec![("heading", "aaa\nbbb\n-\n".to_owned())]
    );
    let tokens = lex("para\n=\n");
    assert_eq!(shapes(&tokens), vec![("heading", "para\n=\n".to_owned())]);
    assert!(matches!(&tokens[0], Token::Heading { depth: 1, .. }));
}

#[test]
fn setext_regression_tail_after_underline() {
    let tokens = lex("aaa\n-\nafter\n");
    assert_eq!(
        shapes(&tokens),
        vec![
            ("heading", "aaa\n-\n".to_owned()),
            ("paragraph", "after\n".to_owned()),
        ]
    );
}

#[test]
fn hr_shaped_line_is_still_a_setext_underline() {
    // `para\n---\n` is h2 (hr is not in marked's lheading guard set)
    let tokens = lex("para\n---\n");
    assert_eq!(shapes(&tokens), vec![("heading", "para\n---\n".to_owned())]);
}

#[test]
fn lheading_initial_guard_bullets() {
    // `^(?!bull …)` — content may not start with a bullet+space line
    let tokens = lex("- item\n===\n");
    assert_eq!(shapes(&tokens), vec![("list", "- item\n===\n".to_owned())]);
}
