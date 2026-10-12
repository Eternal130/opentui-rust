//! Binding-string grammar + expander pipeline — port of
//! `addons/universal/default-parser.ts` (`defaultBindingParser`,
//! lines 400-492) and the compiler's expander cascade
//! (`services/compiler.ts:413-484`, `expandBindingKeyWithExpanders`).
//!
//! Grammar (default-parser.ts):
//! * index 0 and the string contains `+` AND whitespace → the WHOLE
//!   string is one `+`-split stroke (line 403-408);
//! * `<token>` → registered token; display `<token>`; unknown tokens
//!   report and skip (lines 415-439);
//! * `{pattern}` → sequence pattern (elided with the pattern
//!   subsystem; parses as an unknown-pattern error);
//! * modifier chains (`ctrl+`, `shift+`, `meta+`/`alt+`/`option+`,
//!   `super+`, `hyper+`) followed by a named single-stroke key or one
//!   char (lines 320-398);
//! * named single-stroke keys, longest ASCII-case-insensitive prefix
//!   (lines 302-314: the set + `f1`..`f999`);
//! * otherwise one literal char (`"dd"` = `d`,`d`; `"dca"` = `d`,`c`,
//!   `a`), with `"` "` "` → `space` and `"+"` → `+` (lines 212-219).

use crate::stringify::create_key_sequence_part;
use crate::types::{KeyLike, KeySequencePart, KeyStrokeInput, KeyToken};
use std::collections::HashMap;

/// `namedSingleStrokeKeys` (default-parser.ts:11-114).
pub const NAMED_SINGLE_STROKE_KEYS: &[&str] = &[
    "up",
    "down",
    "left",
    "right",
    "clear",
    "escape",
    "return",
    "linefeed",
    "enter",
    "tab",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "space",
    "lt",
    "gt",
    "plus",
    "minus",
    "equal",
    "comma",
    "period",
    "slash",
    "backslash",
    "semicolon",
    "quote",
    "backquote",
    "leftbracket",
    "rightbracket",
    "capslock",
    "numlock",
    "scrolllock",
    "printscreen",
    "pause",
    "menu",
    "apps",
    "kp0",
    "kp1",
    "kp2",
    "kp3",
    "kp4",
    "kp5",
    "kp6",
    "kp7",
    "kp8",
    "kp9",
    "kpdecimal",
    "kpdivide",
    "kpmultiply",
    "kpminus",
    "kpplus",
    "kpenter",
    "kpequal",
    "kpseparator",
    "kpleft",
    "kpright",
    "kpup",
    "kpdown",
    "kppageup",
    "kppagedown",
    "kphome",
    "kpend",
    "kpinsert",
    "kpdelete",
    "mediaplay",
    "mediapause",
    "mediaplaypause",
    "mediareverse",
    "mediastop",
    "mediafastforward",
    "mediarewind",
    "medianext",
    "mediaprev",
    "mediarecord",
    "volumedown",
    "volumeup",
    "mute",
    "leftshift",
    "leftctrl",
    "leftalt",
    "leftsuper",
    "lefthyper",
    "leftmeta",
    "rightshift",
    "rightctrl",
    "rightalt",
    "rightsuper",
    "righthyper",
    "rightmeta",
    "iso_level3_shift",
    "iso_level5_shift",
    "option",
    "alt",
    "meta",
    "super",
    "hyper",
    "control",
    "ctrl",
    "shift",
];

/// `modifierKeyNames` (default-parser.ts:116).
const MODIFIER_KEY_NAMES: &[&str] = &[
    "ctrl", "control", "shift", "meta", "alt", "option", "super", "hyper",
];

/// A parse failure (`defaultBindingParser` throws; `Invalid key …`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Tokens the input referenced but were not registered
    /// (`unknownTokens`, default-parser.ts:428-431) — reported, not
    /// fatal, matching the compiler's warn-and-skip.
    pub unknown_tokens: Vec<String>,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

fn parse_error(message: impl Into<String>) -> ParseError {
    ParseError {
        message: message.into(),
        unknown_tokens: Vec::new(),
    }
}

/// `toLowerAsciiCode` (default-parser.ts:148-150).
fn to_lower_ascii(code: u8) -> u8 {
    if (65..=90).contains(&code) {
        code + 32
    } else {
        code
    }
}

fn is_digit(code: u8) -> bool {
    (48..=57).contains(&code)
}

fn char_at(input: &[u8], index: usize) -> Option<u8> {
    input.get(index).copied()
}

/// `startsWithAsciiInsensitive` (default-parser.ts:156-172) over bytes.
fn starts_with_ascii_insensitive(input: &str, prefix: &str, index: usize) -> bool {
    let bytes = input.as_bytes();
    if bytes.len() < index + prefix.len() {
        return false;
    }
    if &bytes[index..index + prefix.len()] == prefix.as_bytes() {
        return true;
    }
    prefix
        .bytes()
        .enumerate()
        .all(|(offset, pb)| to_lower_ascii(bytes[index + offset]) == pb)
}

/// `findNamedSingleStrokeKey` (default-parser.ts:302-314): longest
/// named-key prefix (case-insensitive) at `index`, plus `f1`..`f999`.
fn find_named_single_stroke_key(input: &str, index: usize) -> Option<String> {
    let bytes = input.as_bytes();
    let mut best: Option<&str> = None;
    for candidate in NAMED_SINGLE_STROKE_KEYS {
        if starts_with_ascii_insensitive(input, candidate, index)
            && best.is_none_or(|current| candidate.len() > current.len())
        {
            best = Some(candidate);
        }
    }
    if let Some(found) = best {
        return Some(found.to_owned());
    }
    // `f` + 1-3 digits (default-parser.ts:308-313).
    if char_at(bytes, index) == Some(b'f') && is_digit(char_at(bytes, index + 1).unwrap_or(0)) {
        let end = if is_digit(char_at(bytes, index + 2).unwrap_or(0)) {
            index + 3
        } else {
            index + 2
        };
        return Some(input[index..end].to_ascii_lowercase());
    }
    None
}

/// `findModifierKey` (default-parser.ts:316-318) — longest modifier
/// prefix; only a modifier IMMEDIATELY followed by `+` qualifies
/// (checked by the caller, default-parser.ts:343-346).
fn find_modifier_key(input: &str, index: usize) -> Option<String> {
    let mut best: Option<&str> = None;
    for candidate in MODIFIER_KEY_NAMES {
        if starts_with_ascii_insensitive(input, candidate, index)
            && best.is_none_or(|current| candidate.len() > current.len())
        {
            best = Some(candidate);
        }
    }
    best.map(ToOwned::to_owned)
}

fn modified_part(
    name: &str,
    ctrl: bool,
    shift: bool,
    meta: bool,
    super_key: bool,
    hyper: bool,
) -> Result<KeySequencePart, ParseError> {
    // displayParts assembly (default-parser.ts:274-293 / 374-395).
    let mut display_parts: Vec<String> = Vec::new();
    if ctrl {
        display_parts.push("ctrl".into());
    }
    if shift {
        display_parts.push("shift".into());
    }
    if meta {
        display_parts.push("meta".into());
    }
    if super_key {
        display_parts.push("super".into());
    }
    if hyper {
        display_parts.push("hyper".into());
    }
    let display_name = if name == " " { "space" } else { name }.to_ascii_lowercase();
    display_parts.push(display_name);
    let input = KeyStrokeInput {
        name: if name == " " { "space" } else { name }.to_owned(),
        ctrl: Some(ctrl),
        shift: Some(shift),
        meta: Some(meta),
        super_key: Some(super_key),
        hyper: Some(hyper),
    };
    let display = display_parts.join("+");
    create_key_sequence_part(&input, Some(&display), None)
        .map_err(|error| parse_error(error.to_string()))
}

/// `parseStringKeyPart` (default-parser.ts:212-295) — the `+`-split
/// whole-string form (used for lone `" "`/`"+"` and the index-0
/// whole-string rule).
fn parse_string_key_part(input: &str) -> Result<KeySequencePart, ParseError> {
    if input == " " {
        return modified_part(" ", false, false, false, false, false);
    }
    if input == "+" {
        return modified_part("+", false, false, false, false, false);
    }
    let mut name = String::new();
    let mut display_name = String::new();
    let (mut ctrl, mut shift, mut meta, mut super_key, mut hyper) =
        (false, false, false, false, false);
    for raw_part in input.split('+') {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "meta" | "alt" | "option" => meta = true,
            "super" => super_key = true,
            "hyper" => hyper = true,
            lowered => {
                if !name.is_empty() {
                    return Err(parse_error(format!(
                        "Invalid key \"{input}\": multiple key names are not supported"
                    )));
                }
                name = part.to_owned();
                display_name = lowered.to_owned();
            }
        }
    }
    if name.is_empty() {
        return Err(parse_error(format!(
            "Invalid key \"{input}\": missing key name"
        )));
    }
    let mut display_parts: Vec<String> = Vec::new();
    if ctrl {
        display_parts.push("ctrl".into());
    }
    if shift {
        display_parts.push("shift".into());
    }
    if meta {
        display_parts.push("meta".into());
    }
    if super_key {
        display_parts.push("super".into());
    }
    if hyper {
        display_parts.push("hyper".into());
    }
    display_parts.push(display_name);
    let stroke_input = KeyStrokeInput {
        name,
        ctrl: Some(ctrl),
        shift: Some(shift),
        meta: Some(meta),
        super_key: Some(super_key),
        hyper: Some(hyper),
    };
    create_key_sequence_part(&stroke_input, Some(&display_parts.join("+")), None)
        .map_err(|error| parse_error(error.to_string()))
}

/// `parseModifiedKeyPart` (default-parser.ts:320-398): consume
/// `modifier+` chains, then one named key or char.
fn parse_modified_key_part(
    input: &str,
    index: usize,
) -> Result<Option<(KeySequencePart, usize)>, ParseError> {
    let bytes = input.as_bytes();
    let mut cursor = index;
    let (mut ctrl, mut shift, mut meta, mut super_key, mut hyper) =
        (false, false, false, false, false);
    while cursor < input.len() {
        let Some(modifier) = find_modifier_key(input, cursor) else {
            break;
        };
        let plus_index = cursor + modifier.len();
        if char_at(bytes, plus_index) != Some(b'+') {
            break;
        }
        match modifier.as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "meta" | "alt" | "option" => meta = true,
            "super" => super_key = true,
            "hyper" => hyper = true,
            _ => {}
        }
        cursor = plus_index + 1;
    }
    if cursor == index {
        return Ok(None);
    }
    let Some(char_at_cursor) = input[cursor..].chars().next().map(|c| c.to_string()) else {
        return Err(parse_error(format!(
            "Invalid key \"{}\": missing key name",
            &input[index..]
        )));
    };
    let name = find_named_single_stroke_key(input, cursor).unwrap_or(char_at_cursor);
    let next_index = cursor + name.len();
    Ok(Some((
        modified_part(&name, ctrl, shift, meta, super_key, hyper)?,
        next_index,
    )))
}

/// `defaultBindingParser` (default-parser.ts:400-492): parse a key
/// string into sequence parts against the registered tokens.
/// Unknown tokens are reported in `ParseError::unknown_tokens` and the
/// binding skips (the compiler warns and drops it — compiler.ts:
/// "Unknown token … binding was skipped until the token is
/// registered").
pub fn parse_binding_sequence(
    input: &str,
    tokens: &HashMap<String, KeyToken>,
) -> Result<Vec<KeySequencePart>, ParseError> {
    let mut parts: Vec<KeySequencePart> = Vec::new();
    let mut unknown_tokens: Vec<String> = Vec::new();
    // The index-0 whole-string rule (default-parser.ts:403-408).
    if input.contains('+') && input.chars().any(char::is_whitespace) {
        return Ok(vec![parse_string_key_part(input)?]);
    }
    let mut index = 0;
    while index < input.len() {
        let Some(char_at_cursor) = input[index..].chars().next() else {
            break;
        };
        let char_len = char_at_cursor.len_utf8();
        if char_at_cursor == '<' {
            let Some(end) = input[index..].find('>').map(|offset| index + offset) else {
                parts.push(parse_string_key_part("<")?);
                index += 1;
                continue;
            };
            let token_name = crate::stringify::normalize_binding_token_name(&input[index + 1..end])
                .map_err(|error| parse_error(error.to_string()))?;
            let Some(token) = tokens.get(&token_name) else {
                unknown_tokens.push(token_name.clone());
                index = end + 1;
                continue;
            };
            let stroke = match &token.key {
                KeyLike::Stroke(stroke) => crate::stringify::normalize_key_stroke(stroke)
                    .map_err(|error| parse_error(error.to_string()))?,
                KeyLike::Str(text) => {
                    let parsed = parse_string_key_part(text)?;
                    parsed.stroke
                }
            };
            parts.push(KeySequencePart {
                display: format!("<{token_name}>"),
                match_id: format!("key:{}", crate::stringify::build_key_match_id(&stroke)),
                token_name: Some(token_name),
                stroke,
            });
            index = end + 1;
            continue;
        }
        if char_at_cursor == '{' {
            // Sequence patterns are elided with the pattern subsystem;
            // the grammar position is preserved and reported.
            if let Some(end) = input[index..].find('}').map(|offset| index + offset) {
                let pattern_name = input[index + 1..end].to_ascii_lowercase();
                unknown_tokens.push(format!("{{pattern:{pattern_name}}}"));
                index = end + 1;
                continue;
            }
        }
        if let Some((part, next_index)) = parse_modified_key_part(input, index)? {
            parts.push(part);
            index = next_index;
            continue;
        }
        if let Some(named) = find_named_single_stroke_key(input, index) {
            let normalized = named.trim().to_ascii_lowercase();
            parts.push(modified_part(
                &normalized,
                false,
                false,
                false,
                false,
                false,
            )?);
            index += named.len();
            continue;
        }
        parts.push(parse_string_key_part(&char_at_cursor.to_string())?);
        index += char_len;
    }
    if !unknown_tokens.is_empty() {
        return Err(ParseError {
            message: format!(
                "Unknown token \"{}\" in key sequence \"{input}\"; binding was skipped until the token is registered",
                unknown_tokens[0]
            ),
            unknown_tokens,
        });
    }
    if parts.is_empty() {
        return Err(parse_error(format!(
            "Invalid key \"{input}\": missing key name"
        )));
    }
    Ok(parts)
}

/// `parseObjectKey` path for a stroke-object key (compiler.ts:
/// `parseObjectKeyPart`) — one part, canonical display.
pub fn parse_object_key(stroke: &KeyStrokeInput) -> Result<KeySequencePart, ParseError> {
    create_key_sequence_part(stroke, None, None).map_err(|error| parse_error(error.to_string()))
}

// -------------------------------------------------------------------------
// Binding expanders — types.ts:511-531 + compiler.ts:413-484
// -------------------------------------------------------------------------

/// `BindingExpansion` (types.ts:517-520).
#[derive(Debug, Clone)]
pub struct BindingExpansion {
    pub key: String,
    pub displays: Option<Vec<String>>,
}

/// `BindingExpander` (types.ts:531): `None` keeps the candidate;
/// `Some` replaces it with expansions (at least one).
pub type BindingExpander =
    std::sync::Arc<dyn Fn(&str, Option<&[String]>) -> Option<Vec<BindingExpansion>> + Send + Sync>;

/// The comma expander (addons/universal/comma-bindings.ts:5-28):
/// splits `j,k` into separate bindings; preserves per-part displays
/// from earlier expanders.
pub fn comma_binding_expander(
    input: &str,
    displays: Option<&[String]>,
) -> Option<Vec<BindingExpansion>> {
    if !input.contains(',') {
        return None;
    }
    let parts: Vec<String> = input
        .split(',')
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();
    if parts.iter().any(String::is_empty) {
        // comma-bindings.ts:11-13 — throws upstream; surfaced as an
        // error string for the compiler to report.
        return Some(vec![BindingExpansion {
            key: format!("\0error:Invalid key sequence \"{input}\": comma-separated bindings cannot contain empty entries"),
            displays: None,
        }]);
    }
    let Some(displays) = displays else {
        return Some(
            parts
                .into_iter()
                .map(|key| BindingExpansion {
                    key,
                    displays: None,
                })
                .collect(),
        );
    };
    let display_parts: Vec<String> = if displays.len() == 1 {
        displays[0]
            .split(',')
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect()
    } else {
        displays.to_vec()
    };
    if display_parts.len() != parts.len() || display_parts.iter().any(String::is_empty) {
        return Some(
            parts
                .into_iter()
                .map(|key| BindingExpansion {
                    key,
                    displays: None,
                })
                .collect(),
        );
    }
    Some(
        parts
            .into_iter()
            .zip(display_parts)
            .map(|(key, display)| BindingExpansion {
                key,
                displays: Some(
                    display
                        .split_whitespace()
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                        .collect(),
                ),
            })
            .collect(),
    )
}

/// `expandBindingKeyWithExpanders` (compiler.ts:413-484): expanders run
/// in registration order (prepend first — `OrderedRegistry`,
/// lib/registry.ts:53-62) and CASCADE — each expander sees every
/// candidate produced so far.
pub fn expand_binding_key(
    key: &str,
    expanders: &[BindingExpander],
) -> Result<Vec<BindingExpansion>, ParseError> {
    if expanders.is_empty() {
        return Ok(vec![BindingExpansion {
            key: key.to_owned(),
            displays: None,
        }]);
    }
    let mut candidates = vec![BindingExpansion {
        key: key.to_owned(),
        displays: None,
    }];
    for expander in expanders {
        let mut next: Vec<BindingExpansion> = Vec::new();
        for candidate in &candidates {
            let displays = candidate.displays.as_deref();
            match expander(&candidate.key, displays) {
                None => next.push(candidate.clone()),
                Some(expansions) => {
                    if expansions.is_empty() {
                        return Err(parse_error(format!(
                            "Keymap binding expander must return at least one key sequence for \"{}\"",
                            candidate.key
                        )));
                    }
                    for expanded in expansions {
                        // The comma expander reports empty entries via a
                        // sentinel key (upstream throws, compiler reports).
                        if let Some(message) = expanded.key.strip_prefix("\0error:") {
                            return Err(parse_error(message));
                        }
                        next.push(expanded);
                    }
                }
            }
        }
        candidates = next;
    }
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chars_and_named_keys() {
        // "dd" = d,d; "dca" = d,c,a (per-char fallback,
        // default-parser.ts:488-491).
        let parts = parse_binding_sequence("dca", &HashMap::new()).unwrap();
        let names: Vec<&str> = parts.iter().map(|p| p.stroke.name.as_str()).collect();
        assert_eq!(names, vec!["d", "c", "a"]);
        // Named keys parse whole (longest prefix).
        let parts = parse_binding_sequence("pagedown", &HashMap::new()).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].stroke.name, "pagedown");
        // Modifier chains.
        let parts = parse_binding_sequence("ctrl+shift+tab", &HashMap::new()).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].stroke.ctrl && parts[0].stroke.shift);
        assert_eq!(parts[0].stroke.name, "tab");
        assert_eq!(parts[0].display, "ctrl+shift+tab");
    }

    #[test]
    fn modifier_prefix_needs_plus() {
        // "control" alone is a named key (modifier only counts with a
        // following `+`, default-parser.ts:343-346).
        let parts = parse_binding_sequence("control", &HashMap::new()).unwrap();
        assert_eq!(parts[0].stroke.name, "control");
        assert!(!parts[0].stroke.ctrl);
    }

    #[test]
    fn tokens_and_unknown_tokens() {
        let mut tokens = HashMap::new();
        tokens.insert(
            "leader".to_owned(),
            KeyToken {
                name: "leader".into(),
                key: KeyLike::Stroke(KeyStrokeInput {
                    name: "x".into(),
                    ctrl: Some(true),
                    ..KeyStrokeInput::default()
                }),
            },
        );
        let parts = parse_binding_sequence("<leader>q", &tokens).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].display, "<leader>");
        assert_eq!(parts[0].token_name.as_deref(), Some("leader"));
        assert!(parts[0].stroke.ctrl);
        assert_eq!(parts[1].stroke.name, "q");
        // Unknown token → error carrying unknown_tokens.
        let err = parse_binding_sequence("<nope>x", &tokens).unwrap_err();
        assert_eq!(err.unknown_tokens, vec!["nope"]);
    }

    #[test]
    fn whole_string_whitespace_rule() {
        // index-0 `+` AND whitespace → whole string is one stroke
        // (default-parser.ts:403-408).
        let parts = parse_binding_sequence("ctrl + x", &HashMap::new()).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].stroke.name, "x");
        assert!(parts[0].stroke.ctrl);
    }

    #[test]
    fn space_and_plus_chars() {
        // Lone " " → the `space` stroke; lone "+" → the `+` stroke
        // (default-parser.ts:212-219).
        let parts = parse_binding_sequence(" ", &HashMap::new()).unwrap();
        assert_eq!(parts[0].stroke.name, "space");
        assert_eq!(parts[0].display, "space");
        let parts = parse_binding_sequence("+", &HashMap::new()).unwrap();
        assert_eq!(parts[0].stroke.name, "+");
        // A space inside a sequence: "a b" = a, space, b (the
        // whole-string rule triggers only when `+` is also present;
        // a lone " " char normalizes to the `space` stroke,
        // default-parser.ts:212-215).
        let parts = parse_binding_sequence("a b", &HashMap::new()).unwrap();
        let names: Vec<&str> = parts.iter().map(|p| p.stroke.name.as_str()).collect();
        assert_eq!(names, vec!["a", "space", "b"]);
    }

    #[test]
    fn comma_expander_splits_and_keeps_displays() {
        // comma-bindings.ts:5-28.
        let out = comma_binding_expander("x, y", None).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].key, "x");
        assert_eq!(out[1].key, "y");
        // Single display with commas splits per part.
        let displays = vec!["alias-x, alias-y".to_owned()];
        let out = comma_binding_expander("ctrl+x, ctrl+y", Some(&displays)).unwrap();
        assert_eq!(
            out[0].displays.as_deref(),
            Some(&["alias-x".to_owned()][..])
        );
        assert_eq!(
            out[1].displays.as_deref(),
            Some(&["alias-y".to_owned()][..])
        );
        // Empty entries error.
        let err = expand_binding_key(
            "x,,y",
            &[std::sync::Arc::new(|input, displays| {
                comma_binding_expander(input, displays)
            })],
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "Invalid key sequence \"x,,y\": comma-separated bindings cannot contain empty entries"
        );
        // No comma → passthrough (None).
        assert!(comma_binding_expander("x", None).is_none());
    }

    #[test]
    fn expander_cascade_order() {
        // compiler.ts:427-446 — each expander transforms every
        // candidate in registration order.
        let alias = |input: &str, _d: Option<&[String]>| -> Option<Vec<BindingExpansion>> {
            (input == "alias").then(|| {
                vec![BindingExpansion {
                    key: "ctrl+x, ctrl+y".into(),
                    displays: Some(vec!["alias-x, alias-y".into()]),
                }]
            })
        };
        let expanders: Vec<BindingExpander> = vec![
            std::sync::Arc::new(alias),
            std::sync::Arc::new(comma_binding_expander),
        ];
        let out = expand_binding_key("alias", &expanders).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].key, "ctrl+x");
        assert_eq!(
            out[0].displays.as_deref(),
            Some(&["alias-x".to_owned()][..])
        );
        assert_eq!(out[1].key, "ctrl+y");
    }

    #[test]
    fn f_keys_parse() {
        let parts = parse_binding_sequence("f12", &HashMap::new()).unwrap();
        assert_eq!(parts[0].stroke.name, "f12");
        let parts = parse_binding_sequence("f2", &HashMap::new()).unwrap();
        assert_eq!(parts[0].stroke.name, "f2");
    }
}
