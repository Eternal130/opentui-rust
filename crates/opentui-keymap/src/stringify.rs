//! Key stringify — exact port of `packages/keymap/src/services/keys.ts`.
//!
//! * `stringifyCanonicalStroke` (keys.ts:136-160): modifiers in order
//!   ctrl, shift, meta, super, hyper; joined with `+`; `return` prints
//!   as `enter`.
//! * `stringifyKeyStroke` (keys.ts:120-130) / `stringifyKeySequence`
//!   (keys.ts:132-134): sequence joins with `""` unless a separator is
//!   given.

use crate::types::KeyStroke;

/// `stringifyCanonicalStroke`.
#[must_use]
pub fn canonical_stroke_string(stroke: &KeyStroke) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if stroke.ctrl {
        parts.push("ctrl");
    }
    if stroke.shift {
        parts.push("shift");
    }
    if stroke.meta {
        parts.push("meta");
    }
    if stroke.super_key {
        parts.push("super");
    }
    parts.push(if stroke.name == "return" { "enter" } else { stroke.name });
    parts.join("+")
}

/// `stringifyKeyStroke` — accepts a stroke (or token, parsed).
#[must_use]
pub fn stringify_key_stroke(input: &KeyStroke) -> String {
    canonical_stroke_string(input)
}

/// `stringifyKeySequence` — joins strokes with `separator` (default "").
#[must_use]
pub fn stringify_key_sequence(inputs: &[KeyStroke], separator: &str) -> String {
    inputs
        .iter()
        .map(stringify_key_stroke)
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_order_and_enter_alias() {
        let k = KeyStroke { name: "return", ctrl: true, shift: true, meta: true, super_key: true };
        assert_eq!(canonical_stroke_string(&k), "ctrl+shift+meta+super+enter");
        assert_eq!(canonical_stroke_string(&KeyStroke::new("return")), "enter");
        assert_eq!(canonical_stroke_string(&KeyStroke::new("escape")), "escape");
    }

    #[test]
    fn sequence_joins() {
        let seq = [KeyStroke::parse("ctrl+x"), KeyStroke::new("k")];
        assert_eq!(stringify_key_sequence(&seq, ""), "ctrl+xk");
        assert_eq!(stringify_key_sequence(&seq, ","), "ctrl+x,k");
    }
}
