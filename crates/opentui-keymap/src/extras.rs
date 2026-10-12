//! `@opentui/keymap/extras` — `createBindingLookup`
//! (extras/binding-lookup.ts), `formatKeySequence` /
//! `formatCommandBindings` (extras/formatting.ts), and
//! `commandBindings` (extras/command-bindings.ts). Exact output
//! formats, ported test-first against extras/tests/*.

use crate::types::{
    Binding, BindingCommand, BindingCommandMap, BindingConfig, BindingConfigItem,
    BindingDefaultsContext, BindingValue, KeyLike, KeySequencePart,
};
use std::collections::{BTreeMap, HashMap};

/// The `bindingDefaults` callback shape (binding-lookup.ts:29-31):
/// returns default fields merged UNDER the binding's own.
pub type BindingDefaultsFn =
    Box<dyn Fn(&BindingDefaultsContext<'_>) -> Option<BTreeMap<String, serde_json::Value>>>;

/// `CreateBindingLookupOptions` (binding-lookup.ts:33-36).
#[derive(Default)]
pub struct CreateBindingLookupOptions {
    pub command_map: Option<BindingCommandMap>,
    pub binding_defaults: Option<BindingDefaultsFn>,
}

/// `BindingLookup` (binding-lookup.ts:38-47).
pub struct BindingLookup {
    normalized: NormalizedBindings,
    command_map: Option<BindingCommandMap>,
    gathered: HashMap<String, GatheredGroup>,
    binding_defaults: Option<BindingDefaultsFn>,
    current_config: BindingConfig,
}

struct NormalizedBindings {
    bindings: Vec<Binding>,
    by_command: HashMap<String, Vec<Binding>>,
}

struct GatheredGroup {
    bindings: Vec<Binding>,
    by_command: HashMap<String, Vec<Binding>>,
}

/// `normalizeCommand` (binding-lookup.ts:75-81): empty commands error
/// (whitespace-only keys stay EXACT — binding-lookup.test.ts:432).
fn normalize_command(command: &str) -> Result<String, String> {
    if command.is_empty() {
        return Err("Invalid binding command: command cannot be empty".to_owned());
    }
    Ok(command.to_owned())
}

/// `mapCommand` (binding-lookup.ts:83-99): exact own-property lookup
/// (the Rust map has no prototype chain to inherit from).
fn map_command(command: &str, command_map: &Option<BindingCommandMap>) -> Result<String, String> {
    let config_command = normalize_command(command)?;
    let Some(command_map) = command_map else {
        return Ok(config_command);
    };
    let Some(mapped) = command_map.get(&config_command) else {
        return Ok(config_command);
    };
    if mapped.is_empty() {
        return Err(format!(
            "Invalid binding command map entry for \"{config_command}\": command cannot be empty"
        ));
    }
    Ok(mapped.clone())
}

/// `resolveBindingItem` (binding-lookup.ts:108-134): a key becomes
/// `{ key, cmd: command }`; a binding object keeps its fields but its
/// `cmd` is REPLACED — the config key is the command identity
/// (binding-lookup.test.ts:27-38). Invalid items are a type error in
/// Rust (upstream throws `invalidBindingValue`).
fn resolve_binding_item(command: &str, item: &BindingConfigItem) -> Binding {
    match item {
        BindingConfigItem::Key(key) => {
            Binding::new(key.clone(), Some(BindingCommand::Name(command.to_owned())))
        }
        BindingConfigItem::Binding(binding) => {
            let mut resolved = binding.clone();
            resolved.cmd = Some(BindingCommand::Name(command.to_owned()));
            resolved
        }
    }
}

/// `withBindingDefaults` (binding-lookup.ts:164-172): `{ ...defaults,
/// ...binding }` — explicit binding fields win.
fn with_binding_defaults(
    command: &str,
    binding: Binding,
    defaults: &dyn Fn(&BindingDefaultsContext<'_>) -> Option<BTreeMap<String, serde_json::Value>>,
) -> Binding {
    let context = BindingDefaultsContext {
        command,
        binding: &binding,
    };
    let Some(defaults) = defaults(&context) else {
        return binding;
    };
    let mut merged = binding;
    for (name, value) in defaults {
        merged.attrs.entry(name).or_insert(value);
    }
    merged
}

/// A borrowed defaults callback.
type BindingDefaultsRef<'a> =
    &'a dyn Fn(&BindingDefaultsContext<'_>) -> Option<BTreeMap<String, serde_json::Value>>;

/// `normalizeBindings` (binding-lookup.ts:194-220): map commands,
/// resolve values (`false`/`"none"`/`[]` disable by DELETING the
/// command), flatten in config order (`flattenBindings`, lines
/// 174-192).
fn normalize_bindings(
    config: &BindingConfig,
    command_map: &Option<BindingCommandMap>,
    binding_defaults: Option<BindingDefaultsRef<'_>>,
) -> Result<NormalizedBindings, String> {
    let mut by_command: HashMap<String, Vec<Binding>> = HashMap::new();
    for (raw_command, value) in config {
        let command = map_command(raw_command, command_map)?;
        let resolved: Option<Vec<Binding>> = match value {
            BindingValue::Disabled => None,
            BindingValue::Items(items) if items.is_empty() => None,
            BindingValue::Items(items) => {
                let mut bindings = Vec::with_capacity(items.len());
                for item in items {
                    let binding = resolve_binding_item(&command, item);
                    bindings.push(match binding_defaults {
                        Some(defaults) => with_binding_defaults(&command, binding, defaults),
                        None => binding,
                    });
                }
                Some(bindings)
            }
            BindingValue::Item(item) => {
                let binding = resolve_binding_item(&command, item);
                Some(vec![match binding_defaults {
                    Some(defaults) => with_binding_defaults(&command, binding, defaults),
                    None => binding,
                }])
            }
        };
        match resolved {
            None => {
                by_command.remove(&command);
            }
            Some(bindings) => {
                by_command.insert(command, bindings);
            }
        }
    }
    // flattenBindings (lines 174-192) — unique commands in config
    // order (upstream's Map preserves first-insertion order).
    let mut bindings = Vec::new();
    let mut flattened: Vec<String> = Vec::new();
    for (raw_command, _) in config {
        let command = map_command(raw_command, command_map)?;
        if flattened.contains(&command) {
            continue;
        }
        flattened.push(command.clone());
        if let Some(list) = by_command.get(&command) {
            bindings.extend(list.iter().cloned());
        }
    }
    Ok(NormalizedBindings {
        bindings,
        by_command,
    })
}

impl BindingLookup {
    /// `lookup.bindings` (line 251).
    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.normalized.bindings
    }

    /// `lookup.get(command)` (line 254): exact command identity — no
    /// trimming (binding-lookup.test.ts:170-179).
    #[must_use]
    pub fn get(&self, command: &str) -> &[Binding] {
        self.normalized
            .by_command
            .get(command)
            .map_or([].as_slice(), Vec::as_slice)
    }

    /// `lookup.has(command)` (line 257).
    #[must_use]
    pub fn has(&self, command: &str) -> bool {
        self.normalized.by_command.contains_key(command)
    }

    /// `lookup.gather(name, commands)` (lines 260-267): gather ONCE
    /// per name; later calls return the cached group (identity by
    /// pointer upstream — by content here).
    pub fn gather(&mut self, name: &str, commands: &[&str]) -> &[Binding] {
        if !self.gathered.contains_key(name) {
            let mut by_command: HashMap<String, Vec<Binding>> = HashMap::new();
            let mut bindings = Vec::new();
            for command in commands {
                if let Some(list) = self.normalized.by_command.get(*command) {
                    by_command.insert((*command).to_owned(), list.clone());
                    bindings.extend(list.iter().cloned());
                }
            }
            self.gathered.insert(
                name.to_owned(),
                GatheredGroup {
                    bindings,
                    by_command,
                },
            );
        }
        &self.gathered[name].bindings
    }

    /// `lookup.pick(name, commands)` (lines 268-282): the gathered
    /// group's bindings for the given commands, in command order.
    pub fn pick(&self, name: &str, commands: &[&str]) -> Vec<Binding> {
        let Some(group) = self.gathered.get(name) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for command in commands {
            if let Some(bindings) = group.by_command.get(*command) {
                result.extend(bindings.iter().cloned());
            }
        }
        result
    }

    /// `lookup.omit(name, commands)` (lines 283-309): the gathered
    /// group minus the omitted commands; empty omission = the whole
    /// group.
    pub fn omit(&self, name: &str, commands: &[&str]) -> Vec<Binding> {
        let Some(group) = self.gathered.get(name) else {
            return Vec::new();
        };
        if commands.is_empty() {
            return group.bindings.clone();
        }
        group
            .bindings
            .iter()
            .filter(|binding| match &binding.cmd {
                Some(BindingCommand::Name(cmd)) => !commands.contains(&cmd.as_str()),
                _ => true,
            })
            .cloned()
            .collect()
    }

    /// `lookup.invalidate(name?)` (lines 310-317).
    pub fn invalidate(&mut self, name: Option<&str>) {
        match name {
            None => self.gathered.clear(),
            Some(name) => {
                self.gathered.remove(name);
            }
        }
    }

    /// `lookup.update(config?)` (lines 318-325): renormalize (the
    /// stored command map + defaults still apply) and clear groups.
    pub fn update(&mut self, config: Option<BindingConfig>) {
        if let Some(config) = config {
            self.current_config = config;
        }
        let defaults = self.binding_defaults.as_deref();
        self.normalized = normalize_bindings(&self.current_config, &self.command_map, defaults)
            .expect("binding config revalidated");
        self.gathered.clear();
    }
}

/// `createBindingLookup` (binding-lookup.ts:242-327). Upstream never
/// throws for construction-time input kinds the type system rules out
/// here; command/value errors surface as `Err` (upstream throws).
pub fn create_binding_lookup(
    config: BindingConfig,
    options: Option<CreateBindingLookupOptions>,
) -> Result<BindingLookup, String> {
    let options = options.unwrap_or_default();
    let command_map = options.command_map.clone();
    let defaults = options.binding_defaults.as_deref();
    let normalized = normalize_bindings(&config, &command_map, defaults)?;
    Ok(BindingLookup {
        normalized,
        command_map,
        gathered: HashMap::new(),
        binding_defaults: options.binding_defaults,
        current_config: config,
    })
}

// -------------------------------------------------------------------------
// formatting — extras/formatting.ts
// -------------------------------------------------------------------------

/// `KeyModifierName` (formatting.ts:3).
pub type KeyModifierName = &'static str;

/// `FormatKeySequenceOptions` (formatting.ts:21-27). The display
/// resolvers are maps (the fn-resolver variant is the caller's
/// pre-computation in Rust). Defaults: separator `" "`.
#[derive(Debug, Clone, Default)]
pub struct FormatKeySequenceOptions {
    pub token_display: Option<HashMap<String, String>>,
    pub pattern_display: Option<HashMap<String, String>>,
    pub key_name_aliases: Option<HashMap<String, String>>,
    pub modifier_aliases: Option<HashMap<KeyModifierName, String>>,
    pub separator: Option<String>,
}

/// `FormatCommandBindingsOptions` (formatting.ts:29-32). Defaults:
/// `bindingSeparator: ", "`, `dedupe: true` (lines 117-118).
#[derive(Debug, Clone)]
pub struct FormatCommandBindingsOptions {
    pub sequence: FormatKeySequenceOptions,
    pub binding_separator: Option<String>,
    pub dedupe: bool,
}

impl Default for FormatCommandBindingsOptions {
    fn default() -> Self {
        FormatCommandBindingsOptions {
            sequence: FormatKeySequenceOptions::default(),
            binding_separator: Some(", ".to_owned()),
            dedupe: true,
        }
    }
}

impl From<FormatKeySequenceOptions> for FormatCommandBindingsOptions {
    fn from(sequence: FormatKeySequenceOptions) -> Self {
        FormatCommandBindingsOptions {
            sequence,
            ..Self::default()
        }
    }
}

/// `formatStroke` (formatting.ts:38-93): token display first
/// (resolvers may override, `""` allowed); then modifier aliases
/// (`+`-joined, ctrl→shift→meta→super→hyper) with key-name aliases
/// (`return` → `enter` BEFORE aliasing, line 90-92).
fn format_stroke(part: &KeySequencePart, options: &FormatKeySequenceOptions) -> String {
    if let Some(token_name) = &part.token_name {
        return match &options.token_display {
            None => part.display.clone(),
            Some(map) => map
                .get(token_name)
                .cloned()
                .unwrap_or_else(|| part.display.clone()),
        };
    }
    if options.key_name_aliases.is_none() && options.modifier_aliases.is_none() {
        return part.display.clone();
    }
    let stroke = &part.stroke;
    let alias = |name: KeyModifierName, on: bool| -> Option<String> {
        if !on {
            return None;
        }
        Some(
            options
                .modifier_aliases
                .as_ref()
                .and_then(|map| map.get(name).cloned())
                .unwrap_or_else(|| name.to_owned()),
        )
    };
    let mut pieces: Vec<String> = Vec::new();
    if let Some(piece) = alias("ctrl", stroke.ctrl) {
        pieces.push(piece);
    }
    if let Some(piece) = alias("shift", stroke.shift) {
        pieces.push(piece);
    }
    if let Some(piece) = alias("meta", stroke.meta) {
        pieces.push(piece);
    }
    if let Some(piece) = alias("super", stroke.super_key) {
        pieces.push(piece);
    }
    if let Some(piece) = alias("hyper", stroke.hyper) {
        pieces.push(piece);
    }
    let name = if stroke.name == "return" {
        "enter"
    } else {
        stroke.name.as_str()
    };
    let key_name = options
        .key_name_aliases
        .as_ref()
        .and_then(|map| map.get(name).cloned())
        .unwrap_or_else(|| name.to_owned());
    pieces.push(key_name);
    pieces.join("+")
}

/// `formatKeySequence` (formatting.ts:95-109): joins with the
/// separator (default `" "`); empty → `""`.
#[must_use]
pub fn format_key_sequence(
    parts: &[KeySequencePart],
    options: &FormatKeySequenceOptions,
) -> String {
    if parts.is_empty() {
        return String::new();
    }
    let separator = options.separator.as_deref().unwrap_or(" ");
    parts
        .iter()
        .map(|part| format_stroke(part, options))
        .collect::<Vec<_>>()
        .join(separator)
}

/// `formatCommandBindings` (formatting.ts:111-138): format each
/// binding's sequence, skip empties, dedupe by the FORMATTED string,
/// join with the binding separator (default `", "`). `None` for an
/// empty list.
#[must_use]
pub fn format_command_bindings(
    bindings: &[Vec<KeySequencePart>],
    options: &FormatCommandBindingsOptions,
) -> Option<String> {
    if bindings.is_empty() {
        return None;
    }
    let binding_separator = options.binding_separator.as_deref().unwrap_or(", ");
    let mut seen: Vec<String> = Vec::new();
    let mut formatted: Vec<String> = Vec::new();
    for sequence in bindings {
        let item = format_key_sequence(sequence, &options.sequence);
        if item.is_empty() {
            continue;
        }
        if options.dedupe {
            if seen.contains(&item) {
                continue;
            }
            seen.push(item.clone());
        }
        formatted.push(item);
    }
    Some(formatted.join(binding_separator))
}

// -------------------------------------------------------------------------
// commandBindings — extras/command-bindings.ts:28-75
// -------------------------------------------------------------------------

/// `CommandBindingsOverrideWarning` (command-bindings.ts:5-10).
#[derive(Debug, Clone, PartialEq)]
pub struct CommandBindingsOverrideWarning {
    pub command: String,
    pub previous_key: KeyLike,
    pub next_key: KeyLike,
}

/// `commandBindings` (command-bindings.ts:28-75): command → key map
/// into bindings; commands TRIM (line 50) and a repeated command
/// REPLACES its earlier binding with a warning. Upstream's
/// `Object.entries` preserves insertion order — the port takes
/// ordered pairs (a `HashMap` would lose it).
pub fn command_bindings(
    bindings: &[(String, KeyLike)],
    mut on_warning: Option<&mut dyn FnMut(CommandBindingsOverrideWarning)>,
) -> Vec<Binding> {
    let mut normalized: Vec<Binding> = Vec::new();
    let mut index_by_command: HashMap<String, usize> = HashMap::new();
    for (command, key) in bindings {
        let normalized_command = command.trim().to_owned();
        let next_binding = Binding::new(
            key.clone(),
            Some(BindingCommand::Name(normalized_command.clone())),
        );
        if let Some(index) = index_by_command.get(&normalized_command).copied() {
            let previous_key = normalized[index].key.clone();
            if let Some(sink) = on_warning.as_mut() {
                sink(CommandBindingsOverrideWarning {
                    command: normalized_command.clone(),
                    previous_key,
                    next_key: key.clone(),
                });
            }
            normalized[index] = next_binding;
            continue;
        }
        index_by_command.insert(normalized_command, normalized.len());
        normalized.push(next_binding);
    }
    normalized
}

impl std::fmt::Debug for BindingLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingLookup")
            .field("bindings", &self.normalized.bindings.len())
            .field("commands", &self.normalized.by_command.len())
            .field("gathered", &self.gathered.keys().collect::<Vec<_>>())
            .finish()
    }
}
