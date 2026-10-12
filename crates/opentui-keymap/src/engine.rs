//! Dispatch engine — the 0.5.14 dispatch contract, ported from
//! `services/dispatch.ts` (+ `services/sequence-index.ts`,
//! `services/primitives/active-layers.ts`, `services/activation.ts`)
//! onto the oc-tui-core-proven engine skeleton.
//!
//! Ported contract points (upstream citations):
//!
//! * **Layer ordering** — `getSortedLayers`
//!   (services/primitives/active-layers.ts:69-77): priority DESC, then
//!   registration order DESC (later registration shadows earlier at
//!   equal priority).
//! * **Focus scope** — `isLayerActiveForFocused`
//!   (active-layers.ts:84-99): target-less layers are global;
//!   `focus` restricts to the exactly-focused target; `focus-within`
//!   (default) activates along the focused target's ancestor chain.
//! * **Event match keys** — `resolveEventMatchKeys` (dispatch.ts:
//!   1226-1281): the direct stroke match first, then resolver-provided
//!   alternates (the base-layout fallback); each match key gets a
//!   FULL root dispatch and only an unhandled one falls to the next
//!   (dispatch.ts:568-586) — direct matches stay ahead of fallbacks
//!   across layers.
//! * **Root dispatch** — `dispatchFromRootAtIndex` (dispatch.ts:
//!   704-770): walk active layers; a layer whose bindings START with
//!   the stroke yields captures; continuations (longer sequences)
//!   HOLD the pending sequence (`holdSequence`, dispatch.ts:409-423);
//!   exact matches run in order — the first accepted non-fallthrough
//!   binding stops the walk (`runCaptureBindings`, dispatch.ts:
//!   1359-1390).
//! * **Pending advance** — `dispatchPendingSequence` (dispatch.ts:
//!   566-630): advance each capture whose next part matches; captures
//!   consuming via an EARLIER match key outrank later ones
//!   (`capturePriority`, sequence-index.ts:234-246); continuation
//!   captures for the same prefix win over exact captures (the
//!   no-resolver ambiguity default, dispatch.ts:655-663); a total
//!   miss clears the sequence UNCONSUMED ("sequence-miss",
//!   dispatch.ts:620-627) — the event falls through to the focused
//!   target.
//! * **Rejection fall-through** — `runBinding`
//!   (command-executor.ts:249-312): a handler returning `false`
//!   rejects the binding; dispatch continues to the next
//!   binding/layer.
//! * **preventDefault** — `applyBindingEventEffects`
//!   (command-executor.ts:331-345): an ACCEPTED binding whose
//!   `preventDefault` is true consumes the event.
//! * **Interceptors** — `handleKeyEvent` (dispatch.ts:441-504):
//!   priority DESC, registration order ASC (`PriorityRegistry`,
//!   lib/registry.ts:79-95); a consumed event stops dispatch
//!   ("intercept-consumed").
//! * **popPendingSequence** — activation.ts:178-208: pop the last
//!   part; a one-part sequence clears entirely; `true` when a pending
//!   sequence existed.
//! * **ensureValidPendingSequence** — activation.ts:121-149: captures
//!   whose layer/binding went inactive (focus change, `enabled`
//!   matcher flips, mode change) are dropped; none remaining clears
//!   the sequence.
//! * **Timed leader** — addons/universal/timed-leader.ts:14-86: the
//!   leader arms when the pending sequence's FIRST part matches the
//!   trigger, re-arming the timeout on every pending change; expiry
//!   (default 1500 ms) clears the sequence. No background timer
//!   exists in the Rust port — expiry is evaluated lazily at each
//!   keymap interaction (and via [`KeymapEngine::tick`]).

use crate::parser::{
    expand_binding_key, parse_binding_sequence, parse_object_key, BindingExpander,
};
use crate::stringify::{build_key_match_id, normalize_key_stroke};
use crate::types::{
    BindingCommand, BindingEvent, Command, CommandContext, Enabled, EventData, KeyLike,
    KeySequencePart, KeyStrokeInput, KeyToken, KeymapEvent, Layer, RunCommandResult, TargetMode,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A focus-scope target handle. Hosts map their surface tree onto
/// stable ids; `0` is conventionally the root.
pub type TargetId = u64;

/// `KeymapHost` (types.ts:27-42) — the surface the engine resolves
/// focus against. Event subscription (`onKeyPress`/`onKeyRelease`/
/// `onFocusChange`) is elided: the Rust port drives dispatch
/// explicitly through [`KeymapEngine::handle_key_event`] (hosts own
/// their event loop; see README "Divergences").
pub trait KeymapHost: Send + Sync {
    fn metadata(&self) -> crate::types::HostMetadata;
    fn root_target(&self) -> TargetId;
    fn is_destroyed(&self) -> bool;
    fn get_focused_target(&self) -> Option<TargetId>;
    fn get_parent_target(&self, target: TargetId) -> Option<TargetId>;
    fn is_target_destroyed(&self, target: TargetId) -> bool;
}

/// A time source (the timed-leader clock; `setTimeout` upstream).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Wall-clock [`Clock`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

/// Controllable [`Clock`] for tests (mirrors oc-tui-core's
/// `ManualClock`).
#[derive(Debug, Default)]
pub struct ManualClock {
    now: std::sync::atomic::AtomicU64,
}

impl ManualClock {
    pub fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set(&self, ms: u64) {
        self.now.store(ms, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// `BindingState` (types.ts:683-691) — a compiled binding.
#[derive(Clone)]
pub struct BindingState {
    pub sequence: Vec<KeySequencePart>,
    pub command: Option<BindingCommand>,
    pub event: BindingEvent,
    pub prevent_default: bool,
    pub fallthrough: bool,
    pub attrs: EventData,
}

impl std::fmt::Debug for BindingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let display: Vec<&str> = self.sequence.iter().map(|p| p.display.as_str()).collect();
        f.debug_struct("BindingState")
            .field("sequence", &display)
            .field("command", &self.command)
            .field("prevent_default", &self.prevent_default)
            .field("fallthrough", &self.fallthrough)
            .finish()
    }
}

/// `RegisteredLayer` (types.ts:739-756) — the compiled layer.
pub struct RegisteredLayer {
    pub order: u64,
    pub name: Option<String>,
    pub target: Option<TargetId>,
    pub target_mode: TargetMode,
    pub priority: i32,
    /// v2's `require("opencode.mode", value)` conditions
    /// (keymap.tsx:427-431) — all must match the engine data.
    pub requires: Vec<(String, serde_json::Value)>,
    pub enabled: Enabled,
    pub commands: Vec<Command>,
    pub bindings: Vec<BindingState>,
}

impl RegisteredLayer {
    fn matches_conditions(&self, data: &EventData) -> bool {
        if !self.enabled.get() {
            return false;
        }
        self.requires
            .iter()
            .all(|(key, value)| data.get(key) == Some(value))
    }
}

/// One in-flight sequence capture (`PendingSequenceCapture`,
/// types.ts:758-764). Layers are referenced by their stable
/// registration order id (upstream holds object refs; indices would
/// go stale when layers unregister). `index` is the sequence position
/// of the last CONSUMED part (`advanceSequenceBinding`,
/// sequence-index.ts:279) — exact = `index == len - 1`,
/// continuation = `index + 1 < len`.
#[derive(Clone)]
struct Capture {
    layer_order: u64,
    binding: usize,
    /// The next part position to consume (== parts consumed).
    index: usize,
    parts: Vec<KeySequencePart>,
}

impl Capture {
    fn new(layer_order: u64, binding: usize, index: usize, parts: Vec<KeySequencePart>) -> Self {
        Capture {
            layer_order,
            binding,
            index,
            parts,
        }
    }
}

/// `KeyAfterReason` (types.ts:379-387).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAfterReason {
    InterceptConsumed,
    BindingHandled,
    BindingRejected,
    NoMatch,
    SequencePending,
    SequenceMiss,
    SequenceCleared,
}

/// The `state` hook callback (`on("state")`).
pub type StateChangeCallback = Arc<dyn Fn() + Send + Sync>;

/// The `pendingSequence` hook callback (`on("pendingSequence")`).
pub type PendingSequenceCallback = Arc<dyn Fn(&[KeySequencePart]) + Send + Sync>;

/// The dispatch outcome (a `KeyDispatchOutcome` +
/// `KeyAfterInputContext` reading, types.ts:369-399).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOutcome {
    pub reason: KeyAfterReason,
    pub handled: bool,
    /// The event was consumed (`preventDefault`/
    /// `stopPropagation`): the focused target never sees it.
    pub consumed: bool,
    pub command: Option<String>,
}

impl DispatchOutcome {
    fn no_match() -> Self {
        DispatchOutcome {
            reason: KeyAfterReason::NoMatch,
            handled: false,
            consumed: false,
            command: None,
        }
    }
}

/// What a key interceptor decided (`intercept("key", fn)`,
/// types.ts:648-652). Hooks are pure decisions — the engine applies
/// them — because upstream interceptors mutate the keymap (e.g.
/// `clearPendingSequence`) from inside the hook, which Rust borrow
/// discipline expresses as a returned decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InterceptDecision {
    /// Consume the event (`consume()`): no layer dispatch, the
    /// focused target never sees it.
    pub consume: bool,
    /// `keymap.clearPendingSequence()` (the escape addon).
    pub clear_pending: bool,
    /// `keymap.popPendingSequence()` (the backspace addon).
    pub pop_pending: bool,
    /// Only act while a pending sequence exists (the escape/backspace
    /// addons' `if (!keymap.hasPendingSequence()) return` guard).
    pub only_when_pending: bool,
}

impl InterceptDecision {
    #[must_use]
    pub fn pass() -> Self {
        Self::default()
    }
}

/// A key interceptor hook — `(ctx) => void` upstream; returns the
/// applied decision here.
pub type KeyInterceptorHook = Arc<dyn Fn(&KeymapEvent) -> InterceptDecision + Send + Sync>;

struct Interceptor {
    priority: i32,
    order: u64,
    hook: KeyInterceptorHook,
}

/// `EventMatchResolver` (types.ts:70-73): derive additional match
/// keys for an event (the base-layout fallback appends one).
pub type EventMatchResolver = Arc<dyn Fn(&KeymapEvent) -> Vec<String> + Send + Sync>;

/// `ActiveKey` (types.ts:415-424) — the which-key option datum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveKey {
    pub display: String,
    pub command: Option<String>,
    pub continues: bool,
}

/// `ActiveBinding` (types.ts:337-345) — the command-bindings datum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBinding {
    pub sequence: Vec<KeySequencePart>,
    pub command: Option<String>,
    pub prevent_default: bool,
    pub fallthrough: bool,
}

/// Timed-leader state (addons/universal/timed-leader.ts:26-64).
struct TimedLeader {
    trigger_match: String,
    /// The registered token name — releasing the leader unregisters it
    /// (timed-leader.ts:72-83 `offLeader()`).
    token_name: String,
    timeout_ms: u64,
    expires_at: Option<u64>,
    armed: bool,
    on_arm: Option<Arc<dyn Fn() + Send + Sync>>,
    on_disarm: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// A disposal handle (upstream `register*` returns `() => void`;
/// the port returns a token released via [`KeymapEngine::release`] —
/// mirrors oc-tui-core's layer-handle convention).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    Layer(u64),
    Token(String),
    Interceptor(u64),
    Expander(u64),
    Resolver(u64),
    TimedLeader(u64),
    Resource(u64),
    /// Several registrations released together (composite addons —
    /// `registerManagedTextareaLayer`'s combined disposer,
    /// edit-buffer-bindings.ts:567-571).
    Composite(Vec<Registration>),
}

struct ResourceEntry {
    count: u64,
    dispose: Vec<Registration>,
}

/// The engine: layers + focus + conditions + interceptors + the
/// pending sequence, dispatching one event at a time.
pub struct KeymapEngine {
    host: Box<dyn KeymapHost>,
    clock: Arc<dyn Clock>,
    layers: Vec<RegisteredLayer>,
    next_order: u64,
    next_interceptor: u64,
    next_expander: u64,
    next_resolver: u64,
    next_timed_leader: u64,
    next_resource: u64,
    layer_ids: HashMap<u64, usize>,
    interceptors: Vec<Interceptor>,
    expanders: Vec<(u64, BindingExpander)>,
    resolvers: Vec<(u64, EventMatchResolver)>,
    tokens: HashMap<String, KeyToken>,
    timed_leaders: Vec<(u64, TimedLeader)>,
    resources: HashMap<u64, ResourceEntry>,
    pending: Option<Vec<Capture>>,
    data: EventData,
    warnings: Vec<String>,
    errors: Vec<String>,
    state_version: u64,
    state_subscribers: Vec<StateChangeCallback>,
    pending_subscribers: Vec<PendingSequenceCallback>,
    /// The default event-match resolver (registerDefaultKeys,
    /// addons/universal/default-parser.ts:494-505).
    default_resolver_installed: bool,
    /// The default binding parser installed (registerDefaultKeys).
    default_parser_installed: bool,
    /// Metadata/enabled fields installed (registerEnabledFields /
    /// registerMetadataFields — shape flags; the port compiles
    /// `enabled`/`desc`/`group`/`title`/`category` unconditionally).
    enabled_fields_installed: bool,
    metadata_fields_installed: bool,
}

impl KeymapEngine {
    pub fn new(host: Box<dyn KeymapHost>) -> Self {
        Self::with_clock(host, Arc::new(SystemClock))
    }

    pub fn with_clock(host: Box<dyn KeymapHost>, clock: Arc<dyn Clock>) -> Self {
        KeymapEngine {
            host,
            clock,
            layers: Vec::new(),
            next_order: 0,
            next_interceptor: 0,
            next_expander: 0,
            next_resolver: 0,
            next_timed_leader: 0,
            next_resource: 0,
            layer_ids: HashMap::new(),
            interceptors: Vec::new(),
            expanders: Vec::new(),
            resolvers: Vec::new(),
            tokens: HashMap::new(),
            timed_leaders: Vec::new(),
            resources: HashMap::new(),
            pending: None,
            data: EventData::new(),
            warnings: Vec::new(),
            errors: Vec::new(),
            state_version: 0,
            state_subscribers: Vec::new(),
            pending_subscribers: Vec::new(),
            default_resolver_installed: false,
            default_parser_installed: false,
            enabled_fields_installed: false,
            metadata_fields_installed: false,
        }
    }

    // -- registration --------------------------------------------------

    /// `registerLayer` (keymap.ts:378) — compile the layer's bindings
    /// through the expander pipeline + default parser
    /// (services/compiler.ts `compileBindings`).
    pub fn register_layer(&mut self, layer: Layer) -> Registration {
        let id = self.next_order;
        self.next_order += 1;
        let mut bindings = Vec::new();
        for binding in &layer.bindings {
            match self.compile_binding(binding) {
                Ok(compiled) => bindings.extend(compiled),
                Err(error) => {
                    // compiler.ts reports and skips the binding; unknown
                    // tokens WARN, malformed keys ERROR.
                    if error.unknown_tokens.is_empty() {
                        self.errors.push(error.message);
                    } else {
                        self.warnings.push(error.message);
                    }
                }
            }
        }
        let registered = RegisteredLayer {
            order: id,
            name: None,
            target: layer.target,
            target_mode: layer.target_mode.unwrap_or_default(),
            priority: layer.priority,
            requires: layer
                .mode
                .as_ref()
                .filter(|mode| mode.as_str() != "global")
                .map(|mode| {
                    (
                        "opencode.mode".to_owned(),
                        serde_json::Value::String(mode.clone()),
                    )
                })
                .into_iter()
                .collect(),
            enabled: layer.enabled.unwrap_or(Enabled::Bool(true)),
            commands: layer.commands,
            bindings,
        };
        self.layer_ids.insert(id, self.layers.len());
        self.layers.push(registered);
        self.notify_state_change();
        Registration::Layer(id)
    }

    /// Compile one binding: expand its key (comma expander etc.),
    /// parse each expansion, attach per-part displays
    /// (`applyExpansionDisplays`, compiler.ts:478+).
    fn compile_binding(
        &self,
        binding: &crate::types::Binding,
    ) -> Result<Vec<BindingState>, crate::parser::ParseError> {
        let sequences: Vec<(Vec<KeySequencePart>, Option<Vec<String>>)> = match &binding.key {
            KeyLike::Stroke(stroke) => vec![(vec![parse_object_key(stroke)?], None)],
            KeyLike::Str(text) => {
                let expanders: Vec<BindingExpander> = self
                    .expanders
                    .iter()
                    .map(|(_, expander)| expander.clone())
                    .collect();
                let expansions = expand_binding_key(text, &expanders)?;
                expansions
                    .into_iter()
                    .map(|expansion| {
                        let mut parts = parse_binding_sequence(&expansion.key, &self.tokens)?;
                        if let Some(displays) = &expansion.displays {
                            for (part, display) in parts.iter_mut().zip(displays) {
                                part.display.clone_from(display);
                            }
                        }
                        Ok((parts, expansion.displays))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        let mut out = Vec::new();
        for (sequence, _displays) in sequences {
            out.push(BindingState {
                sequence,
                command: binding.cmd.clone(),
                event: binding.event,
                prevent_default: binding.prevents_default(),
                fallthrough: binding.fallthrough,
                attrs: binding.attrs.clone(),
            });
        }
        Ok(out)
    }

    /// `registerToken` (keymap.ts:434).
    pub fn register_token(&mut self, token: KeyToken) -> Registration {
        let name = crate::stringify::normalize_binding_token_name(&token.name)
            .map_err(|e| e.to_string())
            .unwrap_or_else(|_| token.name.trim().to_ascii_lowercase());
        let key = token.key.clone();
        self.tokens.insert(
            name.clone(),
            KeyToken {
                name: name.clone(),
                key,
            },
        );
        self.notify_state_change();
        Registration::Token(name)
    }

    /// `intercept("key", fn, options)` (keymap.ts:341).
    pub fn intercept_key(&mut self, hook: KeyInterceptorHook, priority: i32) -> Registration {
        let id = self.next_interceptor;
        self.next_interceptor += 1;
        self.interceptors.push(Interceptor {
            priority,
            order: id,
            hook,
        });
        Registration::Interceptor(id)
    }

    /// `appendBindingExpander` (keymap.ts:446).
    pub fn append_binding_expander(&mut self, expander: BindingExpander) -> Registration {
        let id = self.next_expander;
        self.next_expander += 1;
        self.expanders.push((id, expander));
        self.notify_state_change();
        Registration::Expander(id)
    }

    /// `appendEventMatchResolver` (keymap.ts:490).
    pub fn append_event_match_resolver(&mut self, resolver: EventMatchResolver) -> Registration {
        let id = self.next_resolver;
        self.next_resolver += 1;
        self.resolvers.push((id, resolver));
        Registration::Resolver(id)
    }

    /// `acquireResource(key, setup)` (keymap.ts:286-306):
    /// reference-counted setup keyed by the caller's stable resource
    /// key (upstream keys on a `Symbol` the addon owns — the consts
    /// in [`crate::addons`] play that role).
    pub fn acquire_resource_with_key(
        &mut self,
        key: u64,
        setup: impl FnOnce(&mut Self) -> Vec<Registration>,
    ) -> Registration {
        if let Some(entry) = self.resources.get_mut(&key) {
            entry.count += 1;
            return Registration::Resource(key);
        }
        let dispose = setup(self);
        self.resources
            .insert(key, ResourceEntry { count: 1, dispose });
        Registration::Resource(key)
    }

    /// Allocate a fresh ad-hoc resource key.
    pub fn alloc_resource_key(&mut self) -> u64 {
        let key = self.next_resource;
        self.next_resource += 1;
        // Ad-hoc keys live above the well-known addon range.
        key + 1_000
    }

    /// Release any registration (the upstream `() => void` disposer).
    pub fn release(&mut self, registration: &Registration) {
        match registration {
            Registration::Composite(parts) => {
                for part in parts.clone() {
                    self.release(&part);
                }
            }
            Registration::Layer(id) => {
                if let Some(index) = self.layer_ids.remove(id) {
                    self.layers.remove(index);
                    self.layer_ids.values_mut().for_each(|slot| {
                        if *slot > index {
                            *slot -= 1;
                        }
                    });
                    // Captures referencing the removed layer die at the
                    // next ensure-valid pass (activation.ts:129-137).
                    self.notify_state_change();
                }
            }
            Registration::Token(name) => {
                self.tokens.remove(name);
                self.notify_state_change();
            }
            Registration::Interceptor(id) => {
                self.interceptors.retain(|i| i.order != *id);
            }
            Registration::Expander(id) => {
                self.expanders.retain(|(eid, _)| eid != id);
                self.notify_state_change();
            }
            Registration::Resolver(id) => {
                self.resolvers.retain(|(rid, _)| rid != id);
            }
            Registration::TimedLeader(id) => {
                if let Some(position) = self.timed_leaders.iter().position(|(lid, _)| lid == id) {
                    let (_, mut leader) = self.timed_leaders.remove(position);
                    if leader.armed {
                        leader.armed = false;
                        if let Some(on_disarm) = leader.on_disarm.take() {
                            on_disarm();
                        }
                    }
                    // offLeader() — unregister the leader token
                    // (timed-leader.ts:74).
                    self.tokens.remove(&leader.token_name);
                    self.notify_state_change();
                }
            }
            Registration::Resource(key) => {
                let mut drained = None;
                if let Some(entry) = self.resources.get_mut(key) {
                    entry.count -= 1;
                    if entry.count == 0 {
                        drained = self.resources.remove(key);
                    }
                }
                if let Some(entry) = drained {
                    for registration in entry.dispose {
                        self.release(&registration);
                    }
                }
            }
        }
    }

    // -- runtime data / mode -------------------------------------------

    /// `setData` / `getData` (keymap.ts:228-233) — v2's mode channel.
    pub fn set_data(&mut self, name: &str, value: serde_json::Value) {
        if value.is_null() {
            self.data.remove(name);
        } else {
            self.data.insert(name.to_owned(), value);
        }
        self.notify_state_change();
    }

    #[must_use]
    pub fn get_data(&self, name: &str) -> Option<&serde_json::Value> {
        self.data.get(name)
    }

    /// The current mode (v2 keymap.tsx:424: the last enabled stack
    /// entry, `"base"` default).
    #[must_use]
    pub fn current_mode(&self) -> String {
        self.data
            .get("opencode.mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("base")
            .to_owned()
    }

    // -- pending sequence ----------------------------------------------

    /// `ensureValidPendingSequence` (activation.ts:121-149) + lazy
    /// timed-leader expiry.
    fn ensure_valid_pending(&mut self) {
        self.expire_timed_leaders();
        let Some(captures) = self.pending.clone() else {
            return;
        };
        let mut valid = Vec::new();
        for capture in captures {
            if !self.capture_still_valid(&capture) {
                continue;
            }
            valid.push(capture);
        }
        if valid.is_empty() {
            self.set_pending(None);
        } else if valid.len() != self.pending.as_ref().map_or(0, Vec::len) {
            let next = Some(valid);
            self.set_pending(next);
        }
    }

    fn capture_still_valid(&self, capture: &Capture) -> bool {
        let Some(layer) = self.layers.iter().find(|l| l.order == capture.layer_order) else {
            return false;
        };
        if !layer.matches_conditions(&self.data) {
            return false;
        }
        if !self.layer_in_focus_scope(layer) {
            return false;
        }
        let Some(binding) = layer.bindings.get(capture.binding) else {
            return false;
        };
        // captureHasContinuations (pending-captures.ts:29-45).
        capture.index + 1 < binding.sequence.len()
    }

    /// `hasPendingSequence` (keymap.ts:240).
    #[must_use]
    pub fn has_pending_sequence(&mut self) -> bool {
        self.ensure_valid_pending();
        self.pending.is_some()
    }

    /// `getPendingSequence` (keymap.ts:100) — the display surface
    /// (`collectSequencePartsFromPending`, pending-sequence.ts:106-161:
    /// per-position display conflicts fall back to the canonical
    /// stroke string).
    #[must_use]
    pub fn get_pending_sequence(&mut self) -> Vec<KeySequencePart> {
        self.ensure_valid_pending();
        self.collect_pending_parts()
    }

    fn collect_pending_parts(&self) -> Vec<KeySequencePart> {
        let Some(captures) = &self.pending else {
            return Vec::new();
        };
        let Some(first) = captures.first() else {
            return Vec::new();
        };
        let mut parts = Vec::with_capacity(first.parts.len());
        for position in 0..first.parts.len() {
            let mut display: Option<String> = None;
            let mut token_name: Option<String> = None;
            let mut has_display_conflict = false;
            let mut has_token_conflict = false;
            for capture in captures {
                let Some(part) = capture.parts.get(position) else {
                    continue;
                };
                match &display {
                    None => {
                        display = Some(part.display.clone());
                        token_name = part.token_name.clone();
                    }
                    Some(current) => {
                        if !has_display_conflict && current != &part.display {
                            has_display_conflict = true;
                        }
                        if !has_token_conflict && token_name != part.token_name {
                            has_token_conflict = true;
                        }
                    }
                }
            }
            let mut part = first.parts[position].clone();
            if has_display_conflict || display.is_none() {
                part.display = crate::stringify::canonical_stroke_string(&part.stroke);
            }
            if has_token_conflict {
                part.token_name = None;
            }
            parts.push(part);
        }
        parts
    }

    /// `clearPendingSequence` (keymap.ts:264).
    pub fn clear_pending_sequence(&mut self) {
        self.set_pending(None);
    }

    /// `popPendingSequence` (keymap.ts:268; activation.ts:178-208):
    /// pop the last part; a one-part sequence clears entirely.
    /// Returns whether a pending sequence existed.
    pub fn pop_pending_sequence(&mut self) -> bool {
        self.ensure_valid_pending();
        let Some(captures) = self.pending.clone() else {
            return false;
        };
        let Some(first) = captures.first() else {
            self.set_pending(None);
            return true;
        };
        if first.parts.len() <= 1 {
            self.set_pending(None);
            return true;
        }
        let mut next = Vec::new();
        for capture in captures {
            // popCapture (pending-sequence.ts:68-103): drop the last
            // part (pattern bookkeeping elided).
            if capture.parts.len() <= 1 {
                continue;
            }
            next.push(Capture::new(
                capture.layer_order,
                capture.binding,
                capture.index.saturating_sub(1),
                capture.parts[..capture.parts.len() - 1].to_vec(),
            ));
        }
        if next.is_empty() {
            self.set_pending(None);
        } else {
            self.set_pending(Some(next));
        }
        true
    }

    /// The single mutation point for the pending state
    /// (`setPendingSequence`, activation.ts:109-119) — fires the
    /// `pendingSequence` hooks (the timed leader's arm/disarm) and
    /// queues a state change.
    fn set_pending(&mut self, next: Option<Vec<Capture>>) {
        let same = match (&self.pending, &next) {
            (None, None) => true,
            (Some(current), Some(incoming)) => captures_equal(current, incoming),
            _ => false,
        };
        if same {
            return;
        }
        self.pending = next;
        self.sync_timed_leaders();
        let parts = self.collect_pending_parts();
        let subscribers: Vec<PendingSequenceCallback> = self.pending_subscribers.clone();
        for subscriber in subscribers {
            subscriber(&parts);
        }
        self.notify_state_change();
    }

    // -- timed leaders ---------------------------------------------------

    /// `registerTimedLeader` (addons/universal/timed-leader.ts:14-86):
    /// a token (via `registerLeader`, leader.ts:37-50) + the pending
    /// watcher arming a timeout.
    pub fn register_timed_leader(
        &mut self,
        trigger: &crate::types::KeyStrokeInput,
        name: Option<&str>,
        timeout_ms: u64,
        on_arm: Option<Arc<dyn Fn() + Send + Sync>>,
        on_disarm: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Registration {
        let token_name = crate::stringify::normalize_binding_token_name(name.unwrap_or("leader"))
            .unwrap_or_else(|_| "leader".to_owned());
        let token = KeyToken {
            name: token_name.clone(),
            key: KeyLike::Stroke(trigger.clone()),
        };
        self.register_token(token);
        let id = self.next_timed_leader;
        self.next_timed_leader += 1;
        let stroke = normalize_key_stroke(trigger).ok();
        let trigger_match = stroke
            .as_ref()
            .map(|s| format!("key:{}", build_key_match_id(s)))
            .unwrap_or_default();
        self.timed_leaders.push((
            id,
            TimedLeader {
                trigger_match,
                token_name,
                timeout_ms,
                expires_at: None,
                armed: false,
                on_arm,
                on_disarm,
            },
        ));
        Registration::TimedLeader(id)
    }

    /// `syncArmedState` (timed-leader.ts:45-64): armed = the pending
    /// sequence's first part matches the trigger; each armed sync
    /// RESETS the timeout (scheduleTimeout).
    fn sync_timed_leaders(&mut self) {
        let first_match = self
            .pending
            .as_ref()
            .and_then(|captures| captures.first())
            .and_then(|capture| capture.parts.first())
            .map(|part| part.match_id.clone());
        for (_, leader) in &mut self.timed_leaders {
            let next_armed = first_match.as_ref() == Some(&leader.trigger_match);
            if next_armed {
                leader.expires_at = Some(self.clock.now_ms() + leader.timeout_ms);
            } else {
                leader.expires_at = None;
            }
            if next_armed == leader.armed {
                continue;
            }
            leader.armed = next_armed;
            if next_armed {
                if let Some(on_arm) = &leader.on_arm {
                    on_arm();
                }
            } else if let Some(on_disarm) = &leader.on_disarm {
                on_disarm();
            }
        }
    }

    /// Lazy timeout expiry (`setTimeout` → clearPendingSequence).
    fn expire_timed_leaders(&mut self) {
        let now = self.clock.now_ms();
        let expired = self
            .timed_leaders
            .iter()
            .any(|(_, leader)| leader.expires_at.is_some_and(|at| now >= at));
        if expired {
            self.clear_pending_sequence();
        }
    }

    /// Force a lazy-expiry check (hosts without key traffic call this
    /// from their tick).
    pub fn tick(&mut self) {
        self.ensure_valid_pending();
    }

    // -- queries ---------------------------------------------------------

    /// `getActiveLayers` — sorted (priority DESC, order DESC) and
    /// filtered by focus scope + conditions
    /// (active-layers.ts:43-67 + dispatch.ts walk).
    fn active_layers(&self) -> Vec<usize> {
        let focused = self.host.get_focused_target();
        let mut indices: Vec<usize> = (0..self.layers.len()).collect();
        indices.sort_by(|left, right| {
            let l = &self.layers[*left];
            let r = &self.layers[*right];
            r.priority
                .cmp(&l.priority)
                .then_with(|| r.order.cmp(&l.order))
        });
        indices
            .into_iter()
            .filter(|index| {
                let layer = &self.layers[*index];
                layer.matches_conditions(&self.data)
                    && self.layer_in_focus_scope_for(layer, focused)
            })
            .collect()
    }

    fn layer_in_focus_scope(&self, layer: &RegisteredLayer) -> bool {
        self.layer_in_focus_scope_for(layer, self.host.get_focused_target())
    }

    /// `isLayerActiveForFocused` (active-layers.ts:84-99) over the
    /// activation path (focused → root, active-layers.ts:20-35).
    fn layer_in_focus_scope_for(&self, layer: &RegisteredLayer, focused: Option<TargetId>) -> bool {
        let Some(target) = layer.target else {
            return true;
        };
        if self.host.is_target_destroyed(target) {
            return false;
        }
        match layer.target_mode {
            TargetMode::Focus => focused == Some(target),
            TargetMode::FocusWithin => {
                let mut current = focused.or(Some(self.host.root_target()));
                while let Some(node) = current {
                    if node == target {
                        return true;
                    }
                    if node == self.host.root_target() {
                        return target == self.host.root_target();
                    }
                    current = self.host.get_parent_target(node);
                }
                false
            }
        }
    }

    /// `getActiveKeys` (keymap.ts facade; activation.ts:210+) — the
    /// which-key option set: at the root, every visible first stroke;
    /// pending, the reachable next strokes.
    #[must_use]
    pub fn active_keys(&mut self) -> Vec<ActiveKey> {
        self.ensure_valid_pending();
        let mut out: Vec<ActiveKey> = Vec::new();
        if let Some(captures) = &self.pending {
            for capture in captures {
                let Some(binding) = self
                    .layers
                    .iter()
                    .find(|l| l.order == capture.layer_order)
                    .and_then(|layer| layer.bindings.get(capture.binding))
                else {
                    continue;
                };
                let Some(next) = binding.sequence.get(capture.index + 1) else {
                    continue;
                };
                let continues = capture.index + 2 < binding.sequence.len();
                if let Some(existing) = out.iter_mut().find(|entry| entry.display == next.display) {
                    existing.continues |= continues;
                } else {
                    out.push(ActiveKey {
                        display: next.display.clone(),
                        command: binding
                            .command
                            .as_ref()
                            .and_then(|c| command_name(c).map(str::to_owned))
                            .filter(|_| !continues),
                        continues,
                    });
                }
            }
            return out;
        }
        for index in self.active_layers() {
            let layer = &self.layers[index];
            for binding in &layer.bindings {
                if binding.event != BindingEvent::Press {
                    continue;
                }
                let Some(first) = binding.sequence.first() else {
                    continue;
                };
                let continues = binding.sequence.len() > 1;
                if let Some(existing) = out.iter_mut().find(|entry| entry.display == first.display)
                {
                    existing.continues |= continues;
                } else {
                    out.push(ActiveKey {
                        display: first.display.clone(),
                        command: binding
                            .command
                            .as_ref()
                            .and_then(|c| command_name(c).map(str::to_owned))
                            .filter(|_| !continues),
                        continues,
                    });
                }
            }
        }
        out
    }

    /// `getCommands` (keymap.ts:272) — registered commands in layer
    /// dispatch order, deduped by name.
    #[must_use]
    pub fn get_commands(&self) -> Vec<Command> {
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for index in self.active_layers() {
            for command in &self.layers[index].commands {
                if seen.contains(&command.name) {
                    continue;
                }
                seen.push(command.name.clone());
                out.push(command.clone());
            }
        }
        // Commands on inactive layers still count as "registered".
        for layer in &self.layers {
            for command in &layer.commands {
                if seen.contains(&command.name) {
                    continue;
                }
                seen.push(command.name.clone());
                out.push(command.clone());
            }
        }
        out
    }

    /// `getCommandBindings` (keymap.ts:280) — command → its compiled
    /// bindings (v2's `useShortcuts` reads this at
    /// `visibility: "registered"`).
    #[must_use]
    pub fn get_command_bindings(&self, commands: &[&str]) -> HashMap<String, Vec<ActiveBinding>> {
        let mut out: HashMap<String, Vec<ActiveBinding>> = HashMap::new();
        let mut indices: Vec<usize> = (0..self.layers.len()).collect();
        indices.sort_by(|left, right| {
            let l = &self.layers[*left];
            let r = &self.layers[*right];
            r.priority
                .cmp(&l.priority)
                .then_with(|| r.order.cmp(&l.order))
        });
        for index in indices {
            for binding in &self.layers[index].bindings {
                let Some(name) = binding.command.as_ref().and_then(command_name) else {
                    continue;
                };
                if !commands.is_empty() && !commands.contains(&name) {
                    continue;
                }
                out.entry(name.to_owned()).or_default().push(ActiveBinding {
                    sequence: binding.sequence.clone(),
                    command: binding
                        .command
                        .as_ref()
                        .and_then(command_name)
                        .map(str::to_owned),
                    prevent_default: binding.prevent_default,
                    fallthrough: binding.fallthrough,
                });
            }
        }
        out
    }

    // -- dispatch --------------------------------------------------------

    /// `handleKeyEvent` (dispatch.ts:441-504) for a press or release.
    pub fn handle_key_event(&mut self, event: &mut KeymapEvent) -> DispatchOutcome {
        self.ensure_valid_pending();
        // Interceptors: priority DESC, registration order ASC; a
        // consumed event stops everything (dispatch.ts:479-491).
        let mut ordered: Vec<&Interceptor> = self.interceptors.iter().collect();
        ordered.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.order.cmp(&right.order))
        });
        let hooks: Vec<KeyInterceptorHook> = ordered.iter().map(|i| i.hook.clone()).collect();
        for hook in hooks {
            let decision = hook(event);
            if decision.only_when_pending && self.pending.is_none() {
                continue;
            }
            if decision.clear_pending {
                self.clear_pending_sequence();
            }
            if decision.pop_pending {
                self.pop_pending_sequence();
            }
            if decision.consume {
                event.consume();
            }
            if event.is_consumed() {
                return DispatchOutcome {
                    reason: KeyAfterReason::InterceptConsumed,
                    handled: true,
                    consumed: true,
                    command: None,
                };
            }
        }
        if event.event_type == BindingEvent::Release {
            return self.dispatch_release_layers(event);
        }
        self.dispatch_layers(event)
    }

    /// `resolveEventMatchKeys` (dispatch.ts:1226-1281): the default
    /// resolver yields the direct canonical match; extra resolvers
    /// append (deduped).
    fn resolve_event_match_keys(&self, event: &KeymapEvent) -> Vec<String> {
        let direct = format!(
            "key:{}",
            build_key_match_id(
                &normalize_key_stroke(&KeyStrokeInput {
                    name: event.name.clone(),
                    ctrl: Some(event.ctrl),
                    shift: Some(event.shift),
                    meta: Some(event.meta),
                    super_key: Some(event.super_key),
                    hyper: Some(event.hyper),
                })
                .unwrap_or_default()
            )
        );
        let mut keys = vec![direct];
        if !self.default_resolver_installed && self.resolvers.is_empty() {
            return keys;
        }
        for (_, resolver) in &self.resolvers {
            for candidate in resolver(event) {
                if !keys.contains(&candidate) {
                    keys.push(candidate);
                }
            }
        }
        keys
    }

    /// `dispatchLayers` (dispatch.ts:565-586).
    fn dispatch_layers(&mut self, event: &mut KeymapEvent) -> DispatchOutcome {
        let match_keys = self.resolve_event_match_keys(event);
        if self.pending.is_some() {
            return self.dispatch_pending_sequence(&match_keys, event);
        }
        let mut outcome = DispatchOutcome::no_match();
        for match_key in &match_keys {
            let result = self.dispatch_from_root(match_key, event);
            if result.handled {
                return result;
            }
            if result.reason != KeyAfterReason::NoMatch {
                outcome = result;
            }
        }
        outcome
    }

    /// `dispatchFromRootAtIndex` (dispatch.ts:704-770).
    fn dispatch_from_root(&mut self, match_key: &str, event: &mut KeymapEvent) -> DispatchOutcome {
        let active = self.active_layers();
        let mut outcome = DispatchOutcome::no_match();
        for (walk, layer_index) in active.iter().enumerate() {
            let layer = &self.layers[*layer_index];
            let mut captures = Vec::new();
            let mut best_priority = usize::MAX;
            for (binding_index, binding) in layer.bindings.iter().enumerate() {
                if binding.event != BindingEvent::Press {
                    continue;
                }
                let Some(first) = binding.sequence.first() else {
                    continue;
                };
                if first.match_id != *match_key {
                    continue;
                }
                // capturePriority (sequence-index.ts:234-246) — 0 for
                // the direct key at the root.
                let priority = 0usize;
                if priority < best_priority {
                    best_priority = priority;
                    captures.clear();
                }
                // `advanceSequenceBinding` keeps the position of the
                // JUST-CONSUMED part as `index` (sequence-index.ts:279).
                captures.push(Capture::new(
                    layer.order,
                    binding_index,
                    0,
                    vec![first.clone()],
                ));
            }
            if captures.is_empty() {
                continue;
            }
            let has_continuations = captures
                .iter()
                .any(|capture| self.capture_has_continuations(capture));
            if has_continuations {
                // holdSequence (dispatch.ts:727-741): collect
                // continuation captures from THIS layer onward.
                let continuation_captures =
                    self.collect_continuation_captures(&active[walk..], match_key);
                self.set_pending(Some(continuation_captures));
                event.consume();
                return DispatchOutcome {
                    reason: KeyAfterReason::SequencePending,
                    handled: true,
                    consumed: true,
                    command: None,
                };
            }
            let exact: Vec<Capture> = captures
                .into_iter()
                .filter(|capture| self.capture_is_exact(capture))
                .collect();
            let (handled, stop, result) = self.run_capture_bindings(&exact, event);
            if result.handled {
                outcome = result;
            }
            if handled && stop {
                return outcome;
            }
        }
        outcome
    }

    /// `dispatchPendingSequence` (dispatch.ts:589-630).
    fn dispatch_pending_sequence(
        &mut self,
        match_keys: &[String],
        event: &mut KeymapEvent,
    ) -> DispatchOutcome {
        let Some(current) = self.pending.clone() else {
            return self.dispatch_from_root(&match_keys[0].clone(), event);
        };
        let mut advanced: Vec<Capture> = Vec::new();
        for capture in &current {
            // advanceSequenceBinding (sequence-index.ts:249-282).
            let Some(binding) = self
                .layers
                .iter()
                .find(|l| l.order == capture.layer_order)
                .and_then(|layer| layer.bindings.get(capture.binding))
            else {
                continue;
            };
            // The NEXT part to consume sits at index + 1
            // (`advanceSequenceBinding(…, capture.index + 1, …)`,
            // sequence-index.ts:282).
            let Some(part) = binding.sequence.get(capture.index + 1) else {
                continue;
            };
            if !match_keys.contains(&part.match_id) {
                continue;
            }
            let mut parts = capture.parts.clone();
            parts.push(part.clone());
            advanced.push(Capture::new(
                capture.layer_order,
                capture.binding,
                capture.index + 1,
                parts,
            ));
        }
        // capturePriority: earliest match key wins (sequence-index.ts:
        // 234-246).
        let best_priority = advanced
            .iter()
            .map(|capture| {
                let last = capture.parts.last().expect("advanced captures have parts");
                match_keys
                    .iter()
                    .position(|key| key == &last.match_id)
                    .unwrap_or(match_keys.len())
            })
            .min()
            .unwrap_or(usize::MAX);
        let prioritized: Vec<Capture> = advanced
            .into_iter()
            .filter(|capture| {
                let last = capture.parts.last().expect("advanced captures have parts");
                match_keys
                    .iter()
                    .position(|key| key == &last.match_id)
                    .unwrap_or(match_keys.len())
                    == best_priority
            })
            .collect();
        if prioritized.is_empty() {
            // sequence-miss (dispatch.ts:620-627): clear, UNCONSUMED —
            // the event falls through to the focused target.
            self.set_pending(None);
            return DispatchOutcome {
                reason: KeyAfterReason::SequenceMiss,
                handled: false,
                consumed: false,
                command: None,
            };
        }
        // dispatchPendingCapturesFromIndex (dispatch.ts:632-693).
        let mut outcome = DispatchOutcome::no_match();
        let mut processed_exact: Vec<(u64, usize)> = Vec::new();
        let mut index = 0;
        while index < prioritized.len() {
            let capture = &prioritized[index];
            if processed_exact.contains(&(capture.layer_order, capture.binding)) {
                index += 1;
                continue;
            }
            let continuations: Vec<&Capture> = prioritized
                .iter()
                .enumerate()
                .filter(|(candidate_index, candidate)| {
                    *candidate_index >= index
                        && self.capture_has_continuations(candidate)
                        && same_parts(&candidate.parts, &capture.parts)
                })
                .map(|(_, candidate)| candidate)
                .collect();
            if !continuations.is_empty() {
                // The no-resolver ambiguity default: continuations win
                // (dispatch.ts:655-663).
                let holds: Vec<Capture> = prioritized
                    .iter()
                    .filter(|candidate| {
                        self.capture_has_continuations(candidate)
                            && same_parts(&candidate.parts, &capture.parts)
                    })
                    .cloned()
                    .collect();
                self.set_pending(Some(holds));
                event.consume();
                return DispatchOutcome {
                    reason: KeyAfterReason::SequencePending,
                    handled: true,
                    consumed: true,
                    command: None,
                };
            }
            if !self.capture_is_exact(capture) {
                index += 1;
                continue;
            }
            let prefix = &capture.parts;
            let exact: Vec<Capture> = prioritized
                .iter()
                .filter(|candidate| {
                    candidate.layer_order == capture.layer_order
                        && self.capture_is_exact(candidate)
                        && same_parts(&candidate.parts, prefix)
                })
                .cloned()
                .collect();
            for done in &exact {
                processed_exact.push((done.layer_order, done.binding));
            }
            let (handled, stop, result) = self.run_capture_bindings(&exact, event);
            if result.handled {
                outcome = result;
            }
            if handled && stop {
                self.set_pending(None);
                return outcome;
            }
            index += 1;
        }
        // Everything exhausted without a stop: clear (dispatch.ts:690-692).
        self.set_pending(None);
        outcome
    }

    /// `dispatchReleaseLayers` (dispatch.ts:529-562) — release-event
    /// bindings matched by their first part.
    fn dispatch_release_layers(&mut self, event: &mut KeymapEvent) -> DispatchOutcome {
        let match_keys = self.resolve_event_match_keys(event);
        let active = self.active_layers();
        let mut outcome = DispatchOutcome::no_match();
        for match_key in &match_keys {
            for layer_index in &active {
                let mut handled = false;
                let bindings: Vec<(usize, BindingState)> = self.layers[*layer_index]
                    .bindings
                    .iter()
                    .enumerate()
                    .filter(|(_, binding)| {
                        binding.event == BindingEvent::Release
                            && binding
                                .sequence
                                .first()
                                .is_some_and(|first| first.match_id == *match_key)
                    })
                    .map(|(index, binding)| (index, binding.clone()))
                    .collect();
                for (_, binding) in bindings {
                    let accepted = self.run_binding(&binding, event);
                    if accepted {
                        outcome = DispatchOutcome {
                            reason: KeyAfterReason::BindingHandled,
                            handled: true,
                            consumed: binding.prevent_default && event.is_consumed(),
                            command: binding
                                .command
                                .as_ref()
                                .and_then(command_name)
                                .map(str::to_owned),
                        };
                        handled = true;
                        if !binding.fallthrough {
                            return outcome;
                        }
                    }
                }
                if handled {
                    continue;
                }
            }
        }
        outcome
    }

    /// `collectPendingCapturesFromRoot` (dispatch.ts:1169-1197):
    /// continuation captures from the given layers.
    fn collect_continuation_captures(&self, layers: &[usize], match_key: &str) -> Vec<Capture> {
        let mut captures = Vec::new();
        for layer_index in layers {
            let layer = &self.layers[*layer_index];
            for (binding_index, binding) in layer.bindings.iter().enumerate() {
                if binding.event != BindingEvent::Press {
                    continue;
                }
                let Some(first) = binding.sequence.first() else {
                    continue;
                };
                if first.match_id != match_key {
                    continue;
                }
                if binding.sequence.len() > 1 {
                    // `advanceSequenceBinding` keeps the position of the
                    // JUST-CONSUMED part as `index` (sequence-index.ts:279).
                    captures.push(Capture::new(
                        layer.order,
                        binding_index,
                        0,
                        vec![first.clone()],
                    ));
                }
            }
        }
        captures
    }

    /// `captureHasContinuations` (pending-captures.ts:29-45).
    fn capture_has_continuations(&self, capture: &Capture) -> bool {
        self.layers
            .iter()
            .find(|l| l.order == capture.layer_order)
            .and_then(|layer| layer.bindings.get(capture.binding))
            .is_some_and(|binding| capture.index + 1 < binding.sequence.len())
    }

    /// `captureIsExact` (pending-captures.ts:47-50).
    fn capture_is_exact(&self, capture: &Capture) -> bool {
        self.layers
            .iter()
            .find(|l| l.order == capture.layer_order)
            .and_then(|layer| layer.bindings.get(capture.binding))
            .is_some_and(|binding| capture.index + 1 == binding.sequence.len())
    }

    /// `runCaptureBindings` (dispatch.ts:1359-1390): run in order; the
    /// first accepted non-fallthrough binding stops.
    fn run_capture_bindings(
        &mut self,
        captures: &[Capture],
        event: &mut KeymapEvent,
    ) -> (bool, bool, DispatchOutcome) {
        let mut outcome = DispatchOutcome::no_match();
        for capture in captures {
            let Some(binding) = self
                .layers
                .iter()
                .find(|l| l.order == capture.layer_order)
                .and_then(|layer| layer.bindings.get(capture.binding))
                .cloned()
            else {
                continue;
            };
            let accepted = self.run_binding(&binding, event);
            if !accepted {
                outcome = DispatchOutcome {
                    reason: KeyAfterReason::BindingRejected,
                    handled: false,
                    consumed: false,
                    command: binding
                        .command
                        .as_ref()
                        .and_then(command_name)
                        .map(str::to_owned),
                };
                continue;
            }
            outcome = DispatchOutcome {
                reason: KeyAfterReason::BindingHandled,
                handled: true,
                consumed: binding.prevent_default,
                command: binding
                    .command
                    .as_ref()
                    .and_then(command_name)
                    .map(str::to_owned),
            };
            if !binding.fallthrough {
                return (true, true, outcome);
            }
        }
        (outcome.handled, false, outcome)
    }

    /// `runBinding` (command-executor.ts:249-312): execute the inline
    /// handler or resolve the command name; `false` rejects.
    fn run_binding(&mut self, binding: &BindingState, event: &mut KeymapEvent) -> bool {
        let accepted = match &binding.command {
            Some(BindingCommand::Handler(handler)) => {
                let context = CommandContext {
                    event,
                    focused: self.host.get_focused_target(),
                    target: None,
                    data: &self.data,
                    command: "<function>".to_owned(),
                    input: String::new(),
                    payload: serde_json::Value::Null,
                };
                matches!((handler)(&context), result if result.is_accepted())
            }
            Some(BindingCommand::Name(name)) => {
                let Some((command, target)) = self.resolve_command(name) else {
                    self.warnings.push(format!(
                        "[Keymap] Binding command \"{name}\" is not registered"
                    ));
                    return false;
                };
                let context = CommandContext {
                    event,
                    focused: self.host.get_focused_target(),
                    target,
                    data: &self.data,
                    command: name.clone(),
                    input: name.clone(),
                    payload: serde_json::Value::Null,
                };
                matches!((command.run)(&context), result if result.is_accepted())
            }
            None => false,
        };
        if accepted && binding.prevent_default {
            // applyBindingEventEffects (command-executor.ts:331-345).
            event.consume();
        }
        accepted
    }

    /// Resolve a command name against registered commands (layer
    /// dispatch order; the owning layer's target rides along).
    fn resolve_command(&self, name: &str) -> Option<(&Command, Option<TargetId>)> {
        let mut indices: Vec<usize> = (0..self.layers.len()).collect();
        indices.sort_by(|left, right| {
            let l = &self.layers[*left];
            let r = &self.layers[*right];
            r.priority
                .cmp(&l.priority)
                .then_with(|| r.order.cmp(&l.order))
        });
        for index in indices {
            let layer = &self.layers[index];
            if let Some(command) = layer.commands.iter().find(|command| command.name == name) {
                return Some((command, layer.target));
            }
        }
        None
    }

    /// `runCommand` / `dispatchCommand` (keymap.ts:308-314) over a
    /// synthetic command event (`host.createCommandEvent`,
    /// opentui.ts:8-20).
    pub fn run_command(&mut self, name: &str, input: Option<&str>) -> RunCommandResult {
        let Some((command, target)) = self.resolve_command(name) else {
            return RunCommandResult::NotFound;
        };
        let mut synthetic = KeymapEvent::new("command");
        synthetic.event_type = BindingEvent::Press;
        let context = CommandContext {
            event: &synthetic,
            focused: self.host.get_focused_target(),
            target,
            data: &self.data,
            command: name.to_owned(),
            input: input.unwrap_or_default().to_owned(),
            payload: serde_json::Value::Null,
        };
        match (command.run)(&context) {
            result if result.is_accepted() => RunCommandResult::Ok {
                command: Some(name.to_owned()),
            },
            _ => RunCommandResult::Rejected,
        }
    }

    // -- hooks / diagnostics --------------------------------------------

    /// `on("state")` (keymap.ts:316) — batched derived-state signal
    /// (the solid hooks' version source).
    pub fn on_state_change(&mut self, callback: StateChangeCallback) {
        self.state_subscribers.push(callback);
    }

    /// `on("pendingSequence")` (keymap.ts:318) — synchronous pending
    /// updates.
    pub fn on_pending_sequence(&mut self, callback: PendingSequenceCallback) {
        self.pending_subscribers.push(callback);
    }

    fn notify_state_change(&mut self) {
        self.state_version += 1;
        let subscribers: Vec<StateChangeCallback> = self.state_subscribers.clone();
        for subscriber in subscribers {
            subscriber();
        }
    }

    /// The state version (the `useKeymapStateVersion` counter source).
    #[must_use]
    pub fn state_version(&self) -> u64 {
        self.state_version
    }

    /// Diagnostics (`warning`/`error` events; the test harness's
    /// `takeWarnings`/`takeErrors`).
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    #[must_use]
    pub fn host_metadata(&self) -> crate::types::HostMetadata {
        self.host.metadata()
    }

    #[must_use]
    pub fn host_is_destroyed(&self) -> bool {
        self.host.is_destroyed()
    }

    // -- addon install flags ---------------------------------------------

    pub(crate) fn install_default_keys(&mut self) {
        self.default_parser_installed = true;
        self.default_resolver_installed = true;
    }

    pub(crate) fn install_enabled_fields(&mut self) {
        self.enabled_fields_installed = true;
    }

    pub(crate) fn install_metadata_fields(&mut self) {
        self.metadata_fields_installed = true;
    }

    #[must_use]
    pub fn tokens(&self) -> &HashMap<String, KeyToken> {
        &self.tokens
    }
}

fn command_name(command: &BindingCommand) -> Option<&str> {
    match command {
        BindingCommand::Name(name) => Some(name),
        BindingCommand::Handler(_) => None,
    }
}

fn same_parts(left: &[KeySequencePart], right: &[KeySequencePart]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(l, r)| l.match_id == r.match_id)
}

fn captures_equal(left: &[Capture], right: &[Capture]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(l, r)| {
            l.layer_order == r.layer_order
                && l.binding == r.binding
                && l.index == r.index
                && same_parts(&l.parts, &r.parts)
        })
}
