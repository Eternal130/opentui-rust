//! # opentui-keymap — API-shape port of `@opentui/keymap` 0.5.14
//!
//! Spec source: `third_party/opentui/packages/keymap` at `v0.5.14`
//! (byte-exact vendored reference). Subpath layout mirrors the
//! upstream package exports (package.json `"exports"`):
//!
//! | upstream subpath | Rust module |
//! |---|---|
//! | `.` (index.ts) | [`Keymap`] + [`types`] + [`stringify`] |
//! | `./addons` + `./addons/opentui` | [`addons`], [`textarea`] |
//! | `./extras` | [`extras`] |
//! | `./opentui` | [`opentui`] |
//! | `./solid` | [`solid`] |
//! | `./testing` | [`testing`] |
//! | `./html`, `./react`, `./runtime-modules` | elided (html: no DOM;
//!   react: see [`solid`]'s react mapping; runtime-modules: bundler
//!   machinery) |
//!
//! The dispatch engine (adapted from opencode-rust's battle-tested
//! `oc-tui-core::keymap`, reshaped onto the upstream 0.5.14 source)
//! lives in [`engine`]; per-contract citations are in its module
//! docs. See README.md for the provider mapping and divergences.

pub mod addons;
pub mod engine;
pub mod extras;
pub mod opentui;
pub mod parser;
pub mod solid;
pub mod stringify;
pub mod testing;
pub mod textarea;
pub mod types;

pub use addons::{
    base_layout_key_name, register_backspace_pops_pending_sequence, register_base_layout_fallback,
    register_comma_bindings, register_escape_clears_pending_sequence, register_timed_leader,
};
pub use engine::{
    ActiveBinding, ActiveKey, Clock, DispatchOutcome, InterceptDecision, KeyAfterReason,
    KeymapEngine, KeymapHost, ManualClock, Registration, SystemClock, TargetId,
};
pub use extras::{create_binding_lookup, format_command_bindings, format_key_sequence};
pub use stringify::{stringify_key_sequence, stringify_key_stroke};
pub use types::{
    Binding, BindingCommand, BindingEvent, Command, CommandContext, CommandResult, Enabled,
    KeyLike, KeySequencePart, KeyStroke, KeyStrokeInput, KeyToken, KeymapEvent, Layer,
    NormalizedKeyStroke, ReactiveMatcher, RunCommandResult, StringifyOptions, TargetMode,
};

/// Upstream version this crate pins (`packages/keymap/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";

/// `KEYMAP_EXTENSION_CONTEXT` (services/extension-context.ts:7): the
/// extension-context symbol. The port exposes the same name as a
/// documentation marker — first-party engine access goes through
/// [`Keymap`]'s own API (the symbol's capability surface: state,
/// host, conditions, catalog, activation).
pub const KEYMAP_EXTENSION_CONTEXT: &str = "keymap-extension-context";

/// `Keymap` — the public facade (upstream keymap.ts:73-552). The
/// upstream class delegates to host-provided services; the Rust
/// shape owns an engine ([`KeymapEngine`]) plus the host.
pub struct Keymap {
    pub(crate) engine: KeymapEngine,
}

impl Keymap {
    /// `new Keymap(host)` (keymap.ts:103): refuses a destroyed host
    /// (panics — upstream throws).
    #[must_use]
    pub fn new(host: Box<dyn KeymapHost>) -> Self {
        if host.is_destroyed() {
            panic!("Cannot create a keymap for a destroyed host");
        }
        Keymap {
            engine: KeymapEngine::new(host),
        }
    }

    /// [`Keymap::new`] with an injected clock (tests; upstream uses
    /// real timers).
    #[must_use]
    pub fn with_clock(host: Box<dyn KeymapHost>, clock: Arc<dyn Clock>) -> Self {
        if host.is_destroyed() {
            panic!("Cannot create a keymap for a destroyed host");
        }
        Keymap {
            engine: KeymapEngine::with_clock(host, clock),
        }
    }

    /// `keymap.registerLayer(layer)` (keymap.ts:378) — returns the
    /// release token.
    pub fn register_layer(&mut self, layer: Layer) -> Registration {
        self.engine.register_layer(layer)
    }

    /// `keymap.registerToken(token)` (keymap.ts:434).
    pub fn register_token(&mut self, token: KeyToken) -> Registration {
        self.engine.register_token(token)
    }

    /// `keymap.appendBindingExpander(expander)` (keymap.ts:446).
    pub fn append_binding_expander(&mut self, expander: parser::BindingExpander) -> Registration {
        self.engine.append_binding_expander(expander)
    }

    /// `keymap.appendEventMatchResolver(resolver)` (keymap.ts:490).
    pub fn append_event_match_resolver(
        &mut self,
        resolver: engine::EventMatchResolver,
    ) -> Registration {
        self.engine.append_event_match_resolver(resolver)
    }

    /// `keymap.intercept("key", fn, options)` (keymap.ts:341).
    pub fn intercept_key(
        &mut self,
        hook: std::sync::Arc<dyn Fn(&KeymapEvent) -> InterceptDecision + Send + Sync>,
        priority: i32,
    ) -> Registration {
        self.engine.intercept_key(hook, priority)
    }

    /// Release a registration (the upstream disposer functions).
    pub fn release(&mut self, registration: Registration) {
        self.engine.release(&registration);
    }

    /// `keymap.hasPendingSequence()` (keymap.ts:240).
    pub fn has_pending_sequence(&mut self) -> bool {
        self.engine.has_pending_sequence()
    }

    /// `keymap.getPendingSequence()` (keymap.ts:100).
    pub fn get_pending_sequence(&mut self) -> Vec<KeySequencePart> {
        self.engine.get_pending_sequence()
    }

    /// `keymap.clearPendingSequence()` (keymap.ts:264).
    pub fn clear_pending_sequence(&mut self) {
        self.engine.clear_pending_sequence();
    }

    /// `keymap.popPendingSequence()` (keymap.ts:268).
    pub fn pop_pending_sequence(&mut self) -> bool {
        self.engine.pop_pending_sequence()
    }

    /// `keymap.getActiveKeys(options)` (keymap.ts facade over
    /// activation).
    pub fn active_keys(&mut self) -> Vec<ActiveKey> {
        self.engine.active_keys()
    }

    /// `keymap.getCommands(query)` (keymap.ts:272) — the port serves
    /// the registered catalog (v2's `useCommands`/
    /// `useShortcuts` queries).
    pub fn get_commands(&self) -> Vec<Command> {
        self.engine.get_commands()
    }

    /// `keymap.getCommandBindings(query)` (keymap.ts:280) — command →
    /// bindings (v2 `useShortcuts`'s `visibility: "registered"`).
    pub fn get_command_bindings(&self, commands: &[&str]) -> HashMap<String, Vec<ActiveBinding>> {
        self.engine.get_command_bindings(commands)
    }

    /// `keymap.runCommand(cmd)` / `dispatchCommand` (keymap.ts:308-314)
    /// over a synthetic command event.
    pub fn run_command(&mut self, cmd: &str, input: Option<&str>) -> RunCommandResult {
        self.engine.run_command(cmd, input)
    }

    /// `keymap.parseKeySequence(key)` (keymap.ts:256).
    pub fn parse_key_sequence(
        &self,
        key: &str,
    ) -> Result<Vec<KeySequencePart>, parser::ParseError> {
        parser::parse_binding_sequence(key, self.engine.tokens())
    }

    /// `keymap.formatKey(key, options)` (keymap.ts:260).
    pub fn format_key(&self, key: &str) -> String {
        self.parse_key_sequence(key).map_or_else(
            |_| key.to_owned(),
            |parts| stringify_key_sequence(&parts, &StringifyOptions::default()),
        )
    }

    /// `keymap.setData(name, value)` / `getData` (keymap.ts:228-233).
    pub fn set_data(&mut self, name: &str, value: serde_json::Value) {
        self.engine.set_data(name, value);
    }

    #[must_use]
    pub fn get_data(&self, name: &str) -> Option<&serde_json::Value> {
        self.engine.get_data(name)
    }

    /// The current v2 mode (`createMode.current`, keymap.tsx:424).
    #[must_use]
    pub fn current_mode(&self) -> String {
        self.engine.current_mode()
    }

    /// `keymap.getHostMetadata()` (keymap.ts:236).
    #[must_use]
    pub fn host_metadata(&self) -> types::HostMetadata {
        self.engine.host_metadata()
    }

    /// `keymap.on("state")` (keymap.ts:316) — batched state signal.
    pub fn on_state_change(&mut self, callback: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.engine.on_state_change(callback);
    }

    /// `keymap.on("pendingSequence")` (keymap.ts:318).
    pub fn on_pending_sequence(&mut self, callback: engine::PendingSequenceCallback) {
        self.engine.on_pending_sequence(callback);
    }

    /// The state version (the solid hooks' selector invalidation
    /// source).
    #[must_use]
    pub fn state_version(&self) -> u64 {
        self.engine.state_version()
    }

    /// The host event entry point (the Rust port's explicit pump —
    /// upstream subscribes through the host; see README).
    pub fn handle_key_event(&mut self, event: &mut KeymapEvent) -> DispatchOutcome {
        self.engine.handle_key_event(event)
    }

    /// Diagnostics (`takeWarnings`/`takeErrors`, testing/index.ts).
    pub fn take_warnings(&mut self) -> Vec<String> {
        self.engine.take_warnings()
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        self.engine.take_errors()
    }
}

use std::collections::HashMap;
use std::sync::Arc;
