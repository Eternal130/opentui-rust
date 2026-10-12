//! Full type surface — `packages/keymap/src/types.ts` (0.5.14) shaped for
//! the Rust port, plus the `/extras` config types
//! (`extras/binding-lookup.ts:5-47`) and v2's `KeymapLayer` input shape
//! (`@opencode/plugin/tui/context`, consumed by v2's
//! `context/keymap.tsx:204-251`).
//!
//! Citations are `types.ts` line numbers unless noted. Elisions are
//! documented on each type; the elided subsystems (sequence patterns,
//! disambiguation resolvers, binding transformers) are not exercised by
//! v2's integration surface.

use crate::engine::TargetId;
use std::collections::BTreeMap;
use std::sync::Arc;

/// `HostPlatform` (types.ts:15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostPlatform {
    Macos,
    Windows,
    Linux,
    Unknown,
}

impl HostPlatform {
    /// `normalizeRuntimePlatform` (opentui.ts:20-32): Node platform
    /// names → the keymap's platform enum.
    #[must_use]
    pub fn from_runtime_platform(platform: &str) -> Self {
        match platform {
            "darwin" => HostPlatform::Macos,
            "win32" => HostPlatform::Windows,
            "linux" => HostPlatform::Linux,
            _ => HostPlatform::Unknown,
        }
    }
}

/// `HostModifier` (types.ts:17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostModifier {
    Ctrl,
    Shift,
    Meta,
    Super,
    Hyper,
}

/// `HostCapability` (types.ts:19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostCapability {
    Supported,
    Unsupported,
    Unknown,
}

/// `HostMetadata` (types.ts:21-25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMetadata {
    pub platform: HostPlatform,
    /// `"ctrl" | "super" | "unknown"` — modelled as [`HostModifier`] plus
    /// an `Unknown` variant via [`PrimaryModifier`].
    pub primary_modifier: PrimaryModifier,
    pub modifiers: BTreeMap<HostModifier, HostCapability>,
}

/// The `primaryModifier` string union of `HostMetadata`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryModifier {
    Ctrl,
    Super,
    Unknown,
}

/// `KeyStrokeInput` (types.ts:48-55) — modifier flags default (unset)
/// when absent; `normalize_key_stroke` fills them with `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyStrokeInput {
    pub name: String,
    pub ctrl: Option<bool>,
    pub shift: Option<bool>,
    pub meta: Option<bool>,
    pub super_key: Option<bool>,
    pub hyper: Option<bool>,
}

impl KeyStrokeInput {
    #[must_use]
    pub fn new(name: &str) -> Self {
        KeyStrokeInput {
            name: name.to_owned(),
            ..KeyStrokeInput::default()
        }
    }

    #[must_use]
    pub fn with_modifiers(mut self, ctrl: bool, shift: bool, meta: bool, super_key: bool) -> Self {
        self.ctrl = Some(ctrl);
        self.shift = Some(shift);
        self.meta = Some(meta);
        self.super_key = Some(super_key);
        self
    }
}

impl From<&str> for KeyStrokeInput {
    fn from(name: &str) -> Self {
        KeyStrokeInput::new(name)
    }
}

/// `NormalizedKeyStroke` (types.ts:57-62): name trimmed + lowercased,
/// flags defaulted to `false` (`normalizeKeyStroke`,
/// services/keys.ts:29-38). `hyper` is tri-state upstream
/// (`boolean | undefined`); the port normalizes to `bool`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NormalizedKeyStroke {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
    pub hyper: bool,
}

impl Default for NormalizedKeyStroke {
    fn default() -> Self {
        NormalizedKeyStroke::new("")
    }
}

impl NormalizedKeyStroke {
    #[must_use]
    pub fn new(name: &str) -> Self {
        NormalizedKeyStroke {
            name: name.trim().to_ascii_lowercase(),
            ctrl: false,
            shift: false,
            meta: false,
            super_key: false,
            hyper: false,
        }
    }
}

/// Historical alias kept for the oc-tui-core-adapted engine entry points.
pub type KeyStroke = NormalizedKeyStroke;

/// `KeyLike` (types.ts:150): a binding key is either the string grammar
/// (`"ctrl+x"`, `"<leader>q"`, `"dd"`) or a literal stroke object.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyLike {
    Str(String),
    Stroke(KeyStrokeInput),
}

impl From<&str> for KeyLike {
    fn from(value: &str) -> Self {
        KeyLike::Str(value.to_owned())
    }
}

impl From<String> for KeyLike {
    fn from(value: String) -> Self {
        KeyLike::Str(value)
    }
}

impl From<KeyStrokeInput> for KeyLike {
    fn from(value: KeyStrokeInput) -> Self {
        KeyLike::Stroke(value)
    }
}

/// `KeyMatch` (types.ts:64): opaque match id — `"key:"` prefixed
/// `buildKeyMatchId` string (services/keys.ts:107-109).
pub type KeyMatch = String;

/// `KeySequencePart` (types.ts:130-137). Pattern fields
/// (`patternName`/`payloadKey`) are elided with the sequence-pattern
/// subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySequencePart {
    pub stroke: NormalizedKeyStroke,
    pub display: String,
    pub match_id: KeyMatch,
    pub token_name: Option<String>,
}

/// `StringifyOptions` (types.ts:139-142).
#[derive(Debug, Clone, Default)]
pub struct StringifyOptions {
    pub prefer_display: bool,
    /// Upstream default: `""` (`stringifyKeySequence`,
    /// services/keys.ts:133).
    pub separator: Option<String>,
}

/// `BindingEvent` (types.ts:242).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingEvent {
    Press,
    Release,
}

/// The payload passed to command handlers.
pub type EventData = BTreeMap<String, serde_json::Value>;

/// `CommandResult` (types.ts:224-228): `boolean | void | RunCommandResult`
/// collapses to handled / rejected / a structured status.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandResult {
    /// `run()` returned `true`/`void` — accepted; the binding is handled.
    Handled,
    /// `run()` returned `false` — REJECTED: the binding counts as
    /// unhandled and dispatch continues (command-executor.ts:262-265).
    Rejected,
    /// `run()` returned a `RunCommandResult` object.
    Status(RunCommandResult),
}

impl From<bool> for CommandResult {
    fn from(value: bool) -> Self {
        if value {
            CommandResult::Handled
        } else {
            CommandResult::Rejected
        }
    }
}

impl CommandResult {
    /// Whether the command accepted the invocation (executor.ts:262).
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        match self {
            CommandResult::Handled => true,
            CommandResult::Rejected => false,
            CommandResult::Status(result) => result.is_ok(),
        }
    }
}

/// `RunCommandResult` (types.ts:200-207).
#[derive(Debug, Clone, PartialEq)]
pub enum RunCommandResult {
    Ok { command: Option<String> },
    NotFound,
    Inactive,
    Disabled,
    InvalidArgs,
    Rejected,
    Error,
}

impl RunCommandResult {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, RunCommandResult::Ok { .. })
    }
}

/// `CommandContext` (types.ts:209-222). The `keymap` back-reference is
/// elided (Rust borrow discipline); the context carries the same
/// per-invocation data.
#[derive(Debug, Clone)]
pub struct CommandContext<'a> {
    pub event: &'a crate::types::KeymapEvent,
    pub focused: Option<TargetId>,
    pub target: Option<TargetId>,
    pub data: &'a EventData,
    /// The resolved command name (`"<function>"` for inline handlers —
    /// command-executor.ts:259).
    pub command: String,
    /// v2's invocation input (`keymap.tsx:69-77`).
    pub input: String,
    pub payload: serde_json::Value,
}

/// `CommandHandler` (types.ts:232-236).
pub type CommandHandler = Box<dyn Fn(&CommandContext) -> CommandResult + Send + Sync>;

/// `BindingCommand` (types.ts:238-240): a command name resolved against
/// registered commands, or an inline handler.
#[derive(Clone)]
pub enum BindingCommand {
    Name(String),
    Handler(Arc<CommandHandler>),
}

impl std::fmt::Debug for BindingCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindingCommand::Name(name) => write!(f, "BindingCommand::Name({name})"),
            BindingCommand::Handler(_) => write!(f, "BindingCommand::Handler(<fn>)"),
        }
    }
}

impl PartialEq for BindingCommand {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BindingCommand::Name(a), BindingCommand::Name(b)) => a == b,
            _ => false,
        }
    }
}

impl From<&str> for BindingCommand {
    fn from(value: &str) -> Self {
        BindingCommand::Name(value.to_owned())
    }
}

/// `Binding` (types.ts:244-264). `preventDefault` defaults `true`,
/// `fallthrough` defaults `false`; unknown fields ride in `attrs`
/// (the metadata-fields addon moves `desc`/`group` there).
#[derive(Debug, Clone)]
pub struct Binding {
    pub key: KeyLike,
    pub cmd: Option<BindingCommand>,
    pub event: BindingEvent,
    /// `None` = the upstream default `true`.
    pub prevent_default: Option<bool>,
    pub fallthrough: bool,
    /// Extra binding fields (upstream spreads them through).
    pub attrs: BTreeMap<String, serde_json::Value>,
}

impl Binding {
    #[must_use]
    pub fn new(key: impl Into<KeyLike>, cmd: Option<BindingCommand>) -> Self {
        Binding {
            key: key.into(),
            cmd,
            event: BindingEvent::Press,
            prevent_default: None,
            fallthrough: false,
            attrs: BTreeMap::new(),
        }
    }

    /// The effective `preventDefault` (types.ts:249-256).
    #[must_use]
    pub fn prevents_default(&self) -> bool {
        self.prevent_default.unwrap_or(true)
    }

    #[must_use]
    pub fn with_attr(mut self, name: &str, value: serde_json::Value) -> Self {
        self.attrs.insert(name.to_owned(), value);
        self
    }
}

impl From<&str> for Binding {
    fn from(key: &str) -> Self {
        Binding::new(key, None)
    }
}

/// `Bindings` (types.ts:266-269).
pub type Bindings = Vec<Binding>;

/// `TargetMode` (types.ts:271).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TargetMode {
    /// Active only while the target IS the focused node.
    Focus,
    /// Active while the target is on the focus (ancestor) path — the
    /// default (`isLayerActiveForFocused`,
    /// services/primitives/active-layers.ts:84-99).
    #[default]
    FocusWithin,
}

/// `ReactiveMatcher` (types.ts:431-434): boolean source with
/// subscription-based invalidation.
pub trait ReactiveMatcher: Send + Sync {
    fn get(&self) -> bool;
    /// Subscribe; returns the unsubscribe function.
    fn subscribe(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Box<dyn Fn() + Send + Sync>;
}

/// `Enabled` (addons/universal/enabled.ts:16): boolean, plain predicate,
/// or reactive matcher.
#[derive(Clone)]
pub enum Enabled {
    Bool(bool),
    Fn(Arc<dyn Fn() -> bool + Send + Sync>),
    Matcher(Arc<dyn ReactiveMatcher>),
}

impl Enabled {
    #[must_use]
    pub fn get(&self) -> bool {
        match self {
            Enabled::Bool(value) => *value,
            Enabled::Fn(f) => f(),
            Enabled::Matcher(m) => m.get(),
        }
    }
}

impl Default for Enabled {
    fn default() -> Self {
        Enabled::Bool(true)
    }
}

impl From<bool> for Enabled {
    fn from(value: bool) -> Self {
        Enabled::Bool(value)
    }
}

impl std::fmt::Debug for Enabled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Enabled::Bool(value) => write!(f, "Enabled::Bool({value})"),
            Enabled::Fn(_) => write!(f, "Enabled::Fn(<fn>)"),
            Enabled::Matcher(_) => write!(f, "Enabled::Matcher(<matcher>)"),
        }
    }
}

/// `Command` (types.ts:157-165): `{ name, run, ...fields }`. Custom
/// fields (`desc`, `title`, `category`, `namespace`, v2's `opencode`)
/// ride in `fields`.
#[derive(Clone)]
pub struct Command {
    pub name: String,
    pub run: Arc<dyn Fn(&CommandContext) -> CommandResult + Send + Sync>,
    pub fields: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Command {
    #[must_use]
    pub fn new(name: &str, run: CommandHandler) -> Self {
        Command {
            name: name.to_owned(),
            run: Arc::from(run),
            fields: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_field(mut self, name: &str, value: serde_json::Value) -> Self {
        self.fields.insert(name.to_owned(), value);
        self
    }
}

/// `Layer` (types.ts:273-285). The `mode` field is v2's layer-field
/// `require("opencode.mode", value)` (keymap.tsx:427-431) surfaced as a
/// first-class require; `enabled` is the enabled-fields addon
/// (addons/universal/enabled.ts:43-66).
#[derive(Clone, Default)]
pub struct Layer {
    pub target: Option<TargetId>,
    pub priority: i32,
    pub bindings: Bindings,
    pub commands: Vec<Command>,
    pub target_mode: Option<TargetMode>,
    /// v2's mode gating: `Some(mode)` requires the engine data key
    /// `opencode.mode` to equal `mode`; `None` = global.
    pub mode: Option<String>,
    pub enabled: Option<Enabled>,
}

impl std::fmt::Debug for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layer")
            .field("target", &self.target)
            .field("priority", &self.priority)
            .field("bindings", &self.bindings.len())
            .field(
                "commands",
                &self
                    .commands
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field("target_mode", &self.target_mode)
            .field("mode", &self.mode)
            .field("enabled", &self.enabled.as_ref().map(Enabled::get))
            .finish()
    }
}

/// `KeyToken` (types.ts:306-309): `registerToken` input.
#[derive(Debug, Clone)]
pub struct KeyToken {
    pub name: String,
    pub key: KeyLike,
}

/// `KeymapEvent` (types.ts:3-13) as an owned value: the key press (or
/// release) with modifier flags, the physical `baseCode` (kitty), and
/// the consumption flags interceptors/`preventDefault` set.
#[derive(Debug, Clone)]
pub struct KeymapEvent {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
    pub hyper: bool,
    /// kitty keyboard protocol base-layout codepoint.
    pub base_code: Option<u32>,
    pub event_type: BindingEvent,
    consumed: bool,
}

impl KeymapEvent {
    #[must_use]
    pub fn new(name: &str) -> Self {
        KeymapEvent {
            name: name.to_owned(),
            ctrl: false,
            shift: false,
            meta: false,
            super_key: false,
            hyper: false,
            base_code: None,
            event_type: BindingEvent::Press,
            consumed: false,
        }
    }

    #[must_use]
    pub fn with_modifiers(
        mut self,
        ctrl: bool,
        shift: bool,
        meta: bool,
        super_key: bool,
        hyper: bool,
    ) -> Self {
        self.ctrl = ctrl;
        self.shift = shift;
        self.meta = meta;
        self.super_key = super_key;
        self.hyper = hyper;
        self
    }

    #[must_use]
    pub fn with_base_code(mut self, base_code: Option<u32>) -> Self {
        self.base_code = base_code;
        self
    }

    /// `preventDefault()` + `stopPropagation()` collapse into one
    /// consumed flag (types.ts:10-12).
    pub fn consume(&mut self) {
        self.consumed = true;
    }

    /// `propagationStopped` (types.ts:12).
    #[must_use]
    pub fn is_consumed(&self) -> bool {
        self.consumed
    }
}

// ---------------------------------------------------------------------------
// /extras config types — extras/binding-lookup.ts:5-47
// ---------------------------------------------------------------------------

/// `BindingConfigItem` (binding-lookup.ts:5-7): a key or a full binding.
#[derive(Debug, Clone)]
pub enum BindingConfigItem {
    Key(KeyLike),
    Binding(Binding),
}

/// `BindingValue` (binding-lookup.ts:9-13): `false`/`"none"` disable,
/// a single item enables, an array enables with many; an empty array
/// disables (binding-lookup.ts:145-148).
#[derive(Debug, Clone)]
pub enum BindingValue {
    Disabled,
    Item(BindingConfigItem),
    Items(Vec<BindingConfigItem>),
}

impl From<&str> for BindingValue {
    fn from(key: &str) -> Self {
        if key == "none" {
            BindingValue::Disabled
        } else {
            BindingValue::Item(BindingConfigItem::Key(KeyLike::Str(key.to_owned())))
        }
    }
}

impl From<KeyStrokeInput> for BindingValue {
    fn from(key: KeyStrokeInput) -> Self {
        BindingValue::Item(BindingConfigItem::Key(KeyLike::Stroke(key)))
    }
}

impl From<Binding> for BindingValue {
    fn from(binding: Binding) -> Self {
        BindingValue::Item(BindingConfigItem::Binding(binding))
    }
}

impl From<bool> for BindingValue {
    fn from(value: bool) -> Self {
        if value {
            BindingValue::Item(BindingConfigItem::Key(KeyLike::Str(String::new())))
        } else {
            BindingValue::Disabled
        }
    }
}

/// `BindingConfig` (binding-lookup.ts:15-17): command → value.
pub type BindingConfig = Vec<(String, BindingValue)>;

/// `BindingCommandMap` (binding-lookup.ts:19): config command name →
/// internal command name.
pub type BindingCommandMap = std::collections::HashMap<String, String>;

/// `BindingDefaultsContext` (binding-lookup.ts:24-27).
#[derive(Debug)]
pub struct BindingDefaultsContext<'a> {
    pub command: &'a str,
    pub binding: &'a Binding,
}

// ---------------------------------------------------------------------------
// v2 layer input — @opencode/plugin/tui/context `KeymapLayer`
// ---------------------------------------------------------------------------

/// v2's `KeymapCommand` (plugin/tui/context; consumed by
/// `context/keymap.tsx:204-275`): a named palette/slash command or an
/// inline `bind`-only command. `run(input, event)` mirrors
/// keymap.tsx:221.
#[derive(Clone)]
pub struct KeymapCommand {
    /// Named when present (`id !== undefined`, keymap.tsx:258-262).
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub group: Option<String>,
    /// `false` disables; a string binds by default (keymap.tsx:241-246).
    pub bind: Option<KeyLike>,
    pub palette: bool,
    pub enabled: Option<Enabled>,
    /// `run(input, event)` — `false` rejects (keymap.tsx:231-233).
    pub run: crate::solid::KeymapCommandRun,
}

impl std::fmt::Debug for KeymapCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeymapCommand")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// v2's `KeymapLayer` (plugin/tui/context): the input shape v2 feeds
/// `useBindings` (keymap.tsx:204-251) — commands, config-driven
/// binding ids, mode gating, and layer options.
#[derive(Clone, Default)]
pub struct KeymapLayer {
    pub commands: Vec<KeymapCommand>,
    /// Config keybind ids pulled via `config.keybinds.get(id)`
    /// (keymap.tsx:247).
    pub bindings: Vec<String>,
    /// `Some("global")` = global; otherwise the mode name to require
    /// (keymap.tsx:214).
    pub mode: Option<String>,
    pub priority: i32,
    pub enabled: Option<Enabled>,
    pub target: Option<TargetId>,
    pub target_mode: Option<TargetMode>,
}

impl std::fmt::Debug for KeymapLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeymapLayer")
            .field("commands", &self.commands.len())
            .field("bindings", &self.bindings)
            .field("mode", &self.mode)
            .field("priority", &self.priority)
            .finish()
    }
}
