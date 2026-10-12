//! Key normalize + stringify — exact port of
//! `packages/keymap/src/services/keys.ts` (0.5.14).
//!
//! * `normalizeKeyName` (keys.ts:20-27): trim + lowercase; empty errors.
//! * `normalizeKeyStroke` (keys.ts:29-38): flags default `false`.
//! * `createKeyMatch` (keys.ts:107-109): `"key:" + buildKeyMatchId`.
//! * `buildKeyMatchId` (keys.ts:162-164):
//!   `name:ctrl:shift:meta:super:hyper` as 0/1 flags.
//! * `stringifyCanonicalStroke` (keys.ts:136-160): modifiers in order
//!   ctrl, shift, meta, super, hyper; joined with `+`; `return` prints
//!   as `enter`.
//! * `stringifyKeyStroke` (keys.ts:120-130) honors `preferDisplay`;
//!   `stringifyKeySequence` (keys.ts:132-134) joins with the separator
//!   (default `""`).

use crate::types::{KeySequencePart, KeyStrokeInput, NormalizedKeyStroke, StringifyOptions};

/// A key-normalization failure (`normalizeBindingTokenName` /
/// `normalizeKeyName` throw upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    EmptyToken,
    EmptyKeyName,
    EmptyMatchId,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::EmptyToken => write!(f, "Invalid keymap token: token cannot be empty"),
            KeyError::EmptyKeyName => write!(f, "Invalid key name: key name cannot be empty"),
            KeyError::EmptyMatchId => write!(f, "Invalid keymap match id: id cannot be empty"),
        }
    }
}

impl std::error::Error for KeyError {}

/// `normalizeBindingTokenName` (keys.ts:11-18).
pub fn normalize_binding_token_name(token: &str) -> Result<String, KeyError> {
    let normalized = token.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(KeyError::EmptyToken);
    }
    Ok(normalized)
}

/// `normalizeKeyName` (keys.ts:20-27).
pub fn normalize_key_name(name: &str) -> Result<String, KeyError> {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(KeyError::EmptyKeyName);
    }
    Ok(normalized)
}

/// `normalizeKeyStroke` (keys.ts:29-38).
pub fn normalize_key_stroke(input: &KeyStrokeInput) -> Result<NormalizedKeyStroke, KeyError> {
    Ok(NormalizedKeyStroke {
        name: normalize_key_name(&input.name)?,
        ctrl: input.ctrl.unwrap_or(false),
        shift: input.shift.unwrap_or(false),
        meta: input.meta.unwrap_or(false),
        super_key: input.super_key.unwrap_or(false),
        hyper: input.hyper.unwrap_or(false),
    })
}

/// `buildKeyMatchId` (keys.ts:162-164).
#[must_use]
pub fn build_key_match_id(stroke: &NormalizedKeyStroke) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        stroke.name,
        i32::from(stroke.ctrl),
        i32::from(stroke.shift),
        i32::from(stroke.meta),
        i32::from(stroke.super_key),
        i32::from(stroke.hyper),
    )
}

/// `createKeyMatch` (keys.ts:107-109).
pub fn create_key_match(input: &KeyStrokeInput) -> Result<String, KeyError> {
    Ok(format!(
        "key:{}",
        build_key_match_id(&normalize_key_stroke(input)?)
    ))
}

/// `createTextKeyMatch` (keys.ts:111-118) — kept for host parity.
pub fn create_text_key_match(id: &str) -> Result<String, KeyError> {
    let normalized = id.trim();
    if normalized.is_empty() {
        return Err(KeyError::EmptyMatchId);
    }
    Ok(format!("text:{normalized}"))
}

/// `createKeySequencePart` (keys.ts:62-78): display defaults to the
/// canonical stroke string; token names normalize.
pub fn create_key_sequence_part(
    input: &KeyStrokeInput,
    display: Option<&str>,
    token_name: Option<&str>,
) -> Result<KeySequencePart, KeyError> {
    let stroke = normalize_key_stroke(input)?;
    Ok(KeySequencePart {
        match_id: format!("key:{}", build_key_match_id(&stroke)),
        display: display.map_or_else(|| canonical_stroke_string(&stroke), ToOwned::to_owned),
        token_name: match token_name {
            Some(name) => Some(normalize_binding_token_name(name)?),
            None => None,
        },
        stroke,
    })
}

/// `stringifyCanonicalStroke` (keys.ts:136-160): modifier order
/// ctrl→shift→meta→super→hyper, `+`-joined; `return` prints as `enter`.
#[must_use]
pub fn canonical_stroke_string(stroke: &NormalizedKeyStroke) -> String {
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
    if stroke.hyper {
        parts.push("hyper");
    }
    parts.push(if stroke.name == "return" {
        "enter"
    } else {
        stroke.name.as_str()
    });
    parts.join("+")
}

/// `stringifyKeyStroke` (keys.ts:120-130): a sequence part honors
/// `preferDisplay`; a bare stroke canonicalizes.
#[must_use]
pub fn stringify_key_stroke(part: &KeySequencePart, options: &StringifyOptions) -> String {
    if options.prefer_display {
        return part.display.clone();
    }
    canonical_stroke_string(&part.stroke)
}

/// `stringifyKeySequence` (keys.ts:132-134): joins with the separator
/// (`""` default).
#[must_use]
pub fn stringify_key_sequence(parts: &[KeySequencePart], options: &StringifyOptions) -> String {
    parts
        .iter()
        .map(|part| stringify_key_stroke(part, options))
        .collect::<Vec<_>>()
        .join(options.separator.as_deref().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_order_and_enter_alias() {
        // keys.ts:136-160 — modifier order + `return` → `enter`.
        let k = NormalizedKeyStroke {
            name: "return".into(),
            ctrl: true,
            shift: true,
            meta: true,
            super_key: true,
            hyper: true,
        };
        assert_eq!(
            canonical_stroke_string(&k),
            "ctrl+shift+meta+super+hyper+enter"
        );
        assert_eq!(
            canonical_stroke_string(&NormalizedKeyStroke::new("escape")),
            "escape"
        );
    }

    #[test]
    fn match_id_shape() {
        // keys.ts:107-109 + 162-164.
        let stroke = normalize_key_stroke(&KeyStrokeInput::new("x")).unwrap();
        assert_eq!(build_key_match_id(&stroke), "x:0:0:0:0:0");
        assert_eq!(
            create_key_match(&KeyStrokeInput::new("x")).unwrap(),
            "key:x:0:0:0:0:0"
        );
    }

    #[test]
    fn sequence_joins_with_default_empty_separator() {
        // keys.ts:133 — `options?.separator ?? ""`.
        let a = create_key_sequence_part(&KeyStrokeInput::new("ctrl+x"), None, None).unwrap();
        let b = create_key_sequence_part(&KeyStrokeInput::new("k"), None, None).unwrap();
        assert_eq!(
            stringify_key_sequence(&[a.clone(), b.clone()], &StringifyOptions::default()),
            "ctrl+xk"
        );
        let opts = StringifyOptions {
            separator: Some(",".into()),
            ..Default::default()
        };
        assert_eq!(stringify_key_sequence(&[a, b], &opts), "ctrl+x,k");
    }

    #[test]
    fn prefer_display_and_empty_errors() {
        let part = create_key_sequence_part(
            &KeyStrokeInput::new("return"),
            Some("<leader>"),
            Some("Leader"),
        )
        .unwrap();
        assert_eq!(part.token_name.as_deref(), Some("leader"));
        assert_eq!(part.display, "<leader>");
        assert_eq!(
            stringify_key_stroke(&part, &StringifyOptions::default()),
            "enter"
        );
        let opts = StringifyOptions {
            prefer_display: true,
            ..Default::default()
        };
        assert_eq!(stringify_key_stroke(&part, &opts), "<leader>");
        assert_eq!(normalize_key_name("   "), Err(KeyError::EmptyKeyName));
        assert_eq!(normalize_binding_token_name(""), Err(KeyError::EmptyToken));
    }
}
