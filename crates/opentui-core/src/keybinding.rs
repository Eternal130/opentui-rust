//! `lib/keybinding.internal.ts` (182 lines) — the key-binding lookup
//! machinery TextareaRenderable (and Input, Select, …) share:
//! modifier-scoped keys, the default key aliases, and Kitty
//! base-layout-codepoint fallback resolution.

use std::collections::BTreeMap;

/// `KeyBindingLike` (keybinding.internal.ts:2-8). `super_` spells the
/// upstream `super` field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyBindingLike {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_: bool,
}

impl KeyBindingLike {
    pub fn new(name: &str) -> Self {
        KeyBindingLike {
            name: name.to_owned(),
            ..Default::default()
        }
    }

    pub fn with_modifiers(name: &str, ctrl: bool, shift: bool, meta: bool, super_: bool) -> Self {
        KeyBindingLike {
            name: name.to_owned(),
            ctrl,
            shift,
            meta,
            super_,
        }
    }
}

/// `KeyBinding` (12-15) — a binding plus its action id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: KeyBindingLike,
    pub action: String,
}

impl KeyBinding {
    pub fn new(
        name: &str,
        ctrl: bool,
        shift: bool,
        meta: bool,
        super_: bool,
        action: &str,
    ) -> Self {
        KeyBinding {
            key: KeyBindingLike::with_modifiers(name, ctrl, shift, meta, super_),
            action: action.to_owned(),
        }
    }
}

/// `KeyBindingLookup` (10-15) — the event-side shape: a name, modifier
/// flags, and Kitty's base-layout codepoint.
#[derive(Debug, Clone, Default)]
pub struct KeyEvent {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_: bool,
    pub hyper: bool,
    /// Kitty's "base layout codepoint" — the character for the same
    /// physical key on the keyboard's base layout (17-23).
    pub base_code: Option<u32>,
    /// The raw input sequence (printable text path, Textarea.ts:283-296).
    pub sequence: Option<String>,
}

impl KeyEvent {
    pub fn named(name: &str) -> Self {
        KeyEvent {
            name: name.to_owned(),
            ..Default::default()
        }
    }
}

/// `defaultKeyAliases` (28-53).
pub fn default_key_aliases() -> BTreeMap<String, String> {
    let pairs: &[(&str, &str)] = &[
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
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// `mergeKeyAliases` (55-57).
pub fn merge_key_aliases(
    defaults: BTreeMap<String, String>,
    custom: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = defaults;
    out.extend(custom);
    out
}

/// `getKeyBindingKey` (74-76): `name:ctrl:shift:meta:super`.
pub fn get_key_binding_key(binding: &KeyBindingLike) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        binding.name,
        i32::from(binding.ctrl),
        i32::from(binding.shift),
        i32::from(binding.meta),
        i32::from(binding.super_)
    )
}

/// `getBaseCodeKeyName` (59-71): the base codepoint as a key name;
/// single uppercase letters lowercase.
fn base_code_key_name(base_code: Option<u32>) -> Option<String> {
    let cp = base_code?;
    if cp < 32 || cp == 127 {
        return None;
    }
    let name = char::from_u32(cp)?;
    if (name.is_ascii_uppercase()) && name.len_utf8() == 1 {
        return Some(name.to_ascii_lowercase().to_string());
    }
    Some(name.to_string())
}

/// `getKeyBindingKeys` (78-90): every lookup key an event can match —
/// the parsed name first, then the base-layout name.
pub fn get_key_binding_keys(binding: &KeyEvent) -> Vec<String> {
    let mut names = vec![binding.name.clone()];
    if let Some(base) = base_code_key_name(binding.base_code) {
        if base != binding.name {
            names.push(base);
        }
    }
    names
        .into_iter()
        .map(|name| {
            get_key_binding_key(&KeyBindingLike {
                name,
                ctrl: binding.ctrl,
                shift: binding.shift,
                meta: binding.meta,
                super_: binding.super_,
            })
        })
        .collect()
}

/// `getKeyBindingAction` (92-101).
pub fn get_key_binding_action(
    map: &BTreeMap<String, String>,
    binding: &KeyEvent,
) -> Option<String> {
    for key in get_key_binding_keys(binding) {
        if let Some(action) = map.get(&key) {
            return Some(action.clone());
        }
    }
    None
}

/// `mergeKeyBindings` (63-72): defaults first, custom overriding by key.
pub fn merge_key_bindings(defaults: &[KeyBinding], custom: &[KeyBinding]) -> Vec<KeyBinding> {
    let mut map: BTreeMap<String, KeyBinding> = BTreeMap::new();
    for b in defaults.iter().chain(custom.iter()) {
        map.insert(get_key_binding_key(&b.key), b.clone());
    }
    map.into_values().collect()
}

/// `buildKeyBindingsMap` (103-129): key → action, plus aliased entries
/// for bindings whose name has an alias.
pub fn build_key_bindings_map(
    bindings: &[KeyBinding],
    alias_map: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for b in bindings {
        map.insert(get_key_binding_key(&b.key), b.action.clone());
    }
    if let Some(aliases) = alias_map {
        for b in bindings {
            if let Some(normalized) = aliases.get(&b.key.name) {
                if normalized != &b.key.name {
                    let mut key = b.key.clone();
                    key.name = normalized.clone();
                    map.insert(get_key_binding_key(&key), b.action.clone());
                }
            }
        }
    }
    map
}

/// `matchesKeyBinding` (117-119).
pub fn matches_key_binding(binding: &KeyEvent, m: &KeyBindingLike) -> bool {
    get_key_binding_keys(binding).contains(&get_key_binding_key(m))
}

/// `keyBindingToString` (169-182): "ctrl+shift+y" style.
pub fn key_binding_to_string(binding: &KeyBinding) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if binding.key.ctrl {
        parts.push("ctrl");
    }
    if binding.key.shift {
        parts.push("shift");
    }
    if binding.key.meta {
        parts.push("meta");
    }
    if binding.key.super_ {
        parts.push("super");
    }
    let mut out = parts.join("+");
    if !out.is_empty() {
        out.push('+');
    }
    out.push_str(&binding.key.name);
    out
}
