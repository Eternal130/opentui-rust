//! Key binding resolution — `packages/core/src/lib/keybinding.internal.ts`.
//!
//! Exact port: the alias map ([`DEFAULT_KEY_ALIASES`]), the
//! `name:ctrl:shift:meta:super` lookup-key format, the base-layout
//! (`baseCode`) normalization, and the binding-map build with alias
//! expansion. This is the resolution surface [`crate::input`]'s
//! `InputRenderable` uses; `opentui-keymap`'s textarea bindings table
//! ports the same upstream data for the keymap side (see its
//! `textarea.rs`, citing Textarea.ts:58-124).

use crate::parse_keypress::ParsedKey;
use std::collections::HashMap;

/// `KeyBindingLike` (keybinding.internal.ts:1-7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyBindingLike {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
}

impl KeyBindingLike {
    pub fn new(name: &str) -> Self {
        KeyBindingLike {
            name: name.to_owned(),
            ..Default::default()
        }
    }

    pub fn with_mods(name: &str, ctrl: bool, shift: bool, meta: bool, super_key: bool) -> Self {
        KeyBindingLike {
            name: name.to_owned(),
            ctrl,
            shift,
            meta,
            super_key,
        }
    }
}

impl From<&ParsedKey> for KeyBindingLike {
    fn from(key: &ParsedKey) -> Self {
        KeyBindingLike {
            name: key.name.clone(),
            ctrl: key.ctrl,
            shift: key.shift,
            meta: key.meta,
            super_key: key.super_key.unwrap_or(false),
        }
    }
}

/// `KeyBindingLookup` (keybinding.internal.ts:13-16) — a
/// [`KeyBindingLike`] plus the kitty base-layout codepoint.
#[derive(Debug, Clone, Default)]
pub struct KeyBindingLookup {
    pub binding: KeyBindingLike,
    pub base_code: Option<u32>,
}

impl KeyBindingLookup {
    pub fn from_parsed_key(key: &ParsedKey) -> Self {
        KeyBindingLookup {
            binding: KeyBindingLike::from(key),
            base_code: key.base_code,
        }
    }
}

/// `defaultKeyAliases` (keybinding.internal.ts:20-51) — exact.
pub const DEFAULT_KEY_ALIASES: &[(&str, &str)] = &[
    ("enter", "return"),
    ("esc", "escape"),
    ("kp0", "0"),
    ("kp1", "1"),
    ("kp2", "2"),
    ("kp3", "3"),
    ("kp4", "4"),
    ("kp5", "5"),
    ("kp6", "6"),
    ("kp7", "7"),
    ("kp8", "8"),
    ("kp9", "9"),
    ("kpdecimal", "."),
    ("kpdivide", "/"),
    ("kpmultiply", "*"),
    ("kpminus", "-"),
    ("kpplus", "+"),
    ("kpenter", "enter"),
    ("kpequal", "="),
    ("kpseparator", ","),
    ("kpleft", "left"),
    ("kpright", "right"),
    ("kpup", "up"),
    ("kpdown", "down"),
    ("kppageup", "pageup"),
    ("kppagedown", "pagedown"),
    ("kphome", "home"),
    ("kpend", "end"),
    ("kpinsert", "insert"),
    ("kpdelete", "delete"),
];

pub type KeyAliasMap = HashMap<String, String>;

/// `mergeKeyAliases` (keybinding.internal.ts:53-55) — spread override.
#[must_use]
pub fn merge_key_aliases(defaults: &KeyAliasMap, custom: &KeyAliasMap) -> KeyAliasMap {
    let mut merged = defaults.clone();
    merged.extend(custom.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

/// The default alias map as a `HashMap`.
#[must_use]
pub fn default_key_aliases() -> KeyAliasMap {
    DEFAULT_KEY_ALIASES
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// `getKeyBindingKey` (keybinding.internal.ts:73-75):
/// `name:ctrl:shift:meta:super` with 0/1 flags.
#[must_use]
pub fn get_key_binding_key(binding: &KeyBindingLike) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        binding.name,
        i32::from(binding.ctrl),
        i32::from(binding.shift),
        i32::from(binding.meta),
        i32::from(binding.super_key)
    )
}

/// `getBaseCodeKeyName` (keybinding.internal.ts:82-98): the kitty
/// base-layout codepoint normalized to a key name — single uppercase
/// ASCII letters lowercase, everything else passes through; control
/// codepoints (< 32, 127) are rejected.
fn get_base_code_key_name(base_code: u32) -> Option<String> {
    if base_code < 32 || base_code == 127 {
        return None;
    }
    let name = char::from_u32(base_code)?;
    if name.is_ascii_uppercase() {
        return Some(name.to_ascii_lowercase().to_string());
    }
    Some(name.to_string())
}

/// `getKeyBindingKeys` (keybinding.internal.ts:104-113): every lookup
/// key that can represent this event — the parsed name first, then
/// the base-layout name when kitty provides one.
#[must_use]
pub fn get_key_binding_keys(binding: &KeyBindingLookup) -> Vec<String> {
    let mut names = vec![binding.binding.name.clone()];
    if let Some(base_name) = binding.base_code.and_then(get_base_code_key_name) {
        if !names.contains(&base_name) {
            names.push(base_name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let mut b = binding.binding.clone();
            b.name = name;
            get_key_binding_key(&b)
        })
        .collect()
}

/// `getKeyBindingAction` (keybinding.internal.ts:115-128).
#[must_use]
pub fn get_key_binding_action(
    map: &HashMap<String, String>,
    binding: &KeyBindingLookup,
) -> Option<String> {
    for key in get_key_binding_keys(binding) {
        if let Some(action) = map.get(&key) {
            return Some(action.clone());
        }
    }
    None
}

/// `matchesKeyBinding` (keybinding.internal.ts:130-134).
#[must_use]
pub fn matches_key_binding(binding: &KeyBindingLookup, matched: &KeyBindingLike) -> bool {
    let match_key = get_key_binding_key(matched);
    get_key_binding_keys(binding).contains(&match_key)
}

/// `buildKeyBindingsMap` (keybinding.internal.ts:136-159): the
/// binding keys, then aliased versions of every binding.
#[must_use]
pub fn build_key_bindings_map(
    bindings: &[KeyBinding],
    alias_map: Option<&KeyAliasMap>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for binding in bindings {
        map.insert(
            get_key_binding_key(&binding.binding),
            binding.action.clone(),
        );
    }
    if let Some(aliases) = alias_map {
        for binding in bindings {
            let normalized = aliases
                .get(&binding.binding.name)
                .cloned()
                .unwrap_or_else(|| binding.binding.name.clone());
            if normalized != binding.binding.name {
                let mut aliased = binding.binding.clone();
                aliased.name = normalized;
                map.insert(get_key_binding_key(&aliased), binding.action.clone());
            }
        }
    }
    map
}

/// `KeyBinding<Action>` (keybinding.internal.ts:9-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub binding: KeyBindingLike,
    pub action: String,
}

impl KeyBinding {
    pub fn new(
        name: &str,
        ctrl: bool,
        shift: bool,
        meta: bool,
        super_key: bool,
        action: &str,
    ) -> Self {
        KeyBinding {
            binding: KeyBindingLike::with_mods(name, ctrl, shift, meta, super_key),
            action: action.to_owned(),
        }
    }
}

/// `mergeKeyBindings` (keybinding.internal.ts:57-71): defaults keyed
/// by lookup key, custom overriding, values in map order (JS Map
/// preserves insertion order — the defaults' first-insertion position
/// wins for duplicates, custom replaces in place).
#[must_use]
pub fn merge_key_bindings(defaults: &[KeyBinding], custom: &[KeyBinding]) -> Vec<KeyBinding> {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, KeyBinding> = HashMap::new();
    for binding in defaults.iter().chain(custom.iter()) {
        let key = get_key_binding_key(&binding.binding);
        if !map.contains_key(&key) {
            order.push(key.clone());
        }
        map.insert(key, binding.clone());
    }
    order
        .into_iter()
        .map(|key| map.remove(&key).expect("key present"))
        .collect()
}

/// `keyBindingToString` (keybinding.internal.ts:171-182):
/// ctrl+shift+meta+super order, `+`-joined.
#[must_use]
pub fn key_binding_to_string(binding: &KeyBinding) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if binding.binding.ctrl {
        parts.push("ctrl");
    }
    if binding.binding.shift {
        parts.push("shift");
    }
    if binding.binding.meta {
        parts.push("meta");
    }
    if binding.binding.super_key {
        parts.push("super");
    }
    parts.push(&binding.binding.name);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_key_format() {
        // keybinding.internal.test.ts coverage of getKeyBindingKey
        let b = KeyBindingLike::with_mods("y", true, true, false, false);
        assert_eq!(get_key_binding_key(&b), "y:1:1:0:0");
        let plain = KeyBindingLike::new("escape");
        assert_eq!(get_key_binding_key(&plain), "escape:0:0:0:0");
    }

    #[test]
    fn aliases_apply_to_map() {
        // kpenter → enter → return alias chain (buildKeyBindingsMap
        // applies ONE alias step per binding, exactly like upstream)
        let bindings = vec![
            KeyBinding::new("return", false, false, false, false, "newline"),
            KeyBinding::new("kpenter", false, false, false, false, "newline"),
        ];
        let map = build_key_bindings_map(&bindings, Some(&default_key_aliases()));
        assert!(map.contains_key("return:0:0:0:0"));
        // "enter" alias of return
        assert!(map.contains_key("enter:0:0:0:0"));
        // kpenter aliases to "enter" (not the second hop to return)
        assert!(map.contains_key("enter:0:0:0:0"));
        assert!(map.contains_key("kpenter:0:0:0:0"));
        // kp0 → 0
        let kp = vec![KeyBinding::new("kp0", false, false, false, false, "digit")];
        let map = build_key_bindings_map(&kp, Some(&default_key_aliases()));
        assert_eq!(map.get("0:0:0:0:0").map(String::as_str), Some("digit"));
    }

    #[test]
    fn base_code_lookup() {
        // getKeyBindingKeys: layout-stable fallback (ㅊ base 99 → "c")
        let lookup = KeyBindingLookup {
            binding: KeyBindingLike::with_mods("ㅊ", true, false, false, false),
            base_code: Some(99),
        };
        assert_eq!(
            get_key_binding_keys(&lookup),
            vec!["ㅊ:1:0:0:0".to_owned(), "c:1:0:0:0".to_owned()]
        );
        // uppercase base code lowercases
        let lookup = KeyBindingLookup {
            binding: KeyBindingLike::new("x"),
            base_code: Some(65),
        };
        assert!(get_key_binding_keys(&lookup).contains(&"a:0:0:0:0".to_owned()));
        // control codepoints rejected
        let lookup = KeyBindingLookup {
            binding: KeyBindingLike::new("x"),
            base_code: Some(27),
        };
        assert_eq!(get_key_binding_keys(&lookup), vec!["x:0:0:0:0".to_owned()]);
        // action resolves through the base-layout key
        let mut map = HashMap::new();
        map.insert("c:1:0:0:0".to_owned(), "copy".to_owned());
        let lookup = KeyBindingLookup {
            binding: KeyBindingLike::with_mods("ㅊ", true, false, false, false),
            base_code: Some(99),
        };
        assert_eq!(
            get_key_binding_action(&map, &lookup).as_deref(),
            Some("copy")
        );
    }

    #[test]
    fn merge_prefers_custom() {
        let defaults = vec![
            KeyBinding::new("left", false, false, false, false, "move-left"),
            KeyBinding::new("right", false, false, false, false, "move-right"),
        ];
        let custom = vec![KeyBinding::new(
            "left",
            false,
            false,
            false,
            false,
            "custom-left",
        )];
        let merged = merge_key_bindings(&defaults, &custom);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].action, "custom-left");
        assert_eq!(merged[1].action, "move-right");
    }

    #[test]
    fn stringifies_in_modifier_order() {
        // ctrl → shift → meta → super, then the key name
        let b = KeyBinding::new("y", true, true, false, false, "a");
        assert_eq!(key_binding_to_string(&b), "ctrl+shift+y");
        let b = KeyBinding::new("s", false, false, false, true, "a");
        assert_eq!(key_binding_to_string(&b), "super+s");
        let b = KeyBinding::new("c", true, false, true, false, "a");
        assert_eq!(key_binding_to_string(&b), "ctrl+meta+c");
    }
}
