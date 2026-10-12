//! `@opentui/keymap/addons` (+`/opentui`) — the six integrations v2's
//! `context/keymap.tsx:78-146` installs, ported exactly.
//!
//! Rust shape: upstream `register*(keymap, …) → () => void` becomes
//! `register_*(&mut Keymap, …) -> Registration` released via
//! [`Keymap::release`](crate::Keymap::release) (mirrors oc-tui-core's
//! handle convention). Reference-counted addons key on stable
//! resource consts (upstream keys on `Symbol`s the addon owns).

use crate::engine::{Clock, InterceptDecision, Registration, SystemClock};
use crate::parser::comma_binding_expander;
use crate::stringify::create_key_match;
use crate::types::{
    HostCapability, HostMetadata, HostModifier, HostPlatform, KeyStrokeInput, KeymapEvent,
    PrimaryModifier,
};
use std::sync::Arc;

/// `COMMA_BINDINGS_RESOURCE` (comma-bindings.ts:3).
pub const COMMA_BINDINGS_RESOURCE: u64 = 1;
/// `BASE_LAYOUT_FALLBACK_RESOURCE` (base-layout.ts:4).
pub const BASE_LAYOUT_FALLBACK_RESOURCE: u64 = 2;
/// `ENABLED_FIELDS_RESOURCE` (enabled.ts:5).
pub const ENABLED_FIELDS_RESOURCE: u64 = 3;
/// `METADATA_FIELDS_RESOURCE` (metadata.ts:4).
pub const METADATA_FIELDS_RESOURCE: u64 = 4;
/// The default-keys install (registerDefaultKeys has no symbol; the
/// default keymap owns one install).
pub const DEFAULT_KEYS_RESOURCE: u64 = 5;
/// `EDIT_BUFFER_COMMANDS_RESOURCE` (edit-buffer-bindings.ts:211).
pub const EDIT_BUFFER_COMMANDS_RESOURCE: u64 = 6;
/// `TEXTAREA_MAPPING_SUSPENSION_RESOURCE` (edit-buffer-bindings.ts:212).
pub const TEXTAREA_MAPPING_SUSPENSION_RESOURCE: u64 = 7;

/// `LeaderTrigger` (leader.ts:3): a stroke, `{ key }`, or a
/// single-binding array (from `createBindingLookup().get("leader")`).
pub enum LeaderTrigger {
    Stroke(KeyStrokeInput),
    Key { key: KeyStrokeInput },
    Bindings(Vec<KeyStrokeInput>),
}

/// `resolveLeaderTrigger` (leader.ts:14-32).
#[must_use]
pub fn resolve_leader_trigger(trigger: &LeaderTrigger) -> Option<KeyStrokeInput> {
    match trigger {
        LeaderTrigger::Stroke(stroke) => Some(stroke.clone()),
        LeaderTrigger::Key { key } => Some(key.clone()),
        LeaderTrigger::Bindings(bindings) => {
            if bindings.is_empty() {
                return None;
            }
            if bindings.len() > 1 {
                panic!("Invalid leader trigger: expected exactly one binding");
            }
            Some(bindings[0].clone())
        }
    }
}

/// `LeaderOptions` (leader.ts:5-8).
pub struct LeaderOptions {
    pub trigger: LeaderTrigger,
    pub name: Option<String>,
}

/// `registerLeader` (leader.ts:37-50): defines a token such as
/// `leader` the default parser references as `<leader>`.
pub fn register_leader(keymap: &mut crate::Keymap, options: LeaderOptions) -> Registration {
    let Some(trigger) = resolve_leader_trigger(&options.trigger) else {
        // leader.ts:42-44 — an empty trigger registers nothing.
        return Registration::Resource(u64::MAX);
    };
    let name = options.name.unwrap_or_else(|| "leader".to_owned());
    keymap.engine.register_token(crate::types::KeyToken {
        name,
        key: crate::types::KeyLike::Stroke(trigger),
    })
}

/// `TimedLeaderOptions` (timed-leader.ts:4-8): the leader trigger,
/// the timeout (upstream default 1500 ms — timed-leader.ts:24; v2
/// passes 2000 ms, keymap.tsx:143), and the arm/disarm callbacks.
pub struct TimedLeaderOptions {
    pub trigger: LeaderTrigger,
    pub name: Option<String>,
    pub timeout_ms: Option<u64>,
    pub on_arm: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_disarm: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl TimedLeaderOptions {
    #[must_use]
    pub fn new(trigger: impl Into<KeyStrokeInput>) -> Self {
        TimedLeaderOptions {
            trigger: LeaderTrigger::Stroke(trigger.into()),
            name: None,
            timeout_ms: None,
            on_arm: None,
            on_disarm: None,
        }
    }

    #[must_use]
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }
}

/// `registerTimedLeader` (timed-leader.ts:14-86): defines the leader
/// token and clears the pending sequence if no follow-up key arrives
/// before the timeout (lazy expiry in the Rust port — see the engine
/// docs).
pub fn register_timed_leader(
    keymap: &mut crate::Keymap,
    options: TimedLeaderOptions,
) -> Registration {
    let Some(trigger) = resolve_leader_trigger(&options.trigger) else {
        return Registration::Resource(u64::MAX);
    };
    keymap.engine.register_timed_leader(
        &trigger,
        options.name.as_deref(),
        options.timeout_ms.unwrap_or(1500),
        options.on_arm,
        options.on_disarm,
    )
}

/// `EscapeClearsPendingSequenceOptions`
/// (escape-clears-pending-sequence.ts:3-10).
#[derive(Debug, Clone, Copy)]
pub struct EscapeClearsPendingSequenceOptions {
    /// Consume Escape after clearing so it does not reach the focused
    /// renderable or lower-priority listeners (default `true`).
    pub prevent_default: Option<bool>,
    pub priority: Option<i32>,
}

/// `registerEscapeClearsPendingSequence`
/// (escape-clears-pending-sequence.ts:16-41): lets Escape cancel an
/// in-progress multi-key sequence before it dispatches further.
/// Consumes the escape only while a sequence was pending.
pub fn register_escape_clears_pending_sequence(
    keymap: &mut crate::Keymap,
    options: Option<EscapeClearsPendingSequenceOptions>,
) -> Registration {
    let prevent_default = options.and_then(|o| o.prevent_default).unwrap_or(true);
    let priority = options.and_then(|o| o.priority).unwrap_or(0);
    keymap.engine.intercept_key(
        Arc::new(move |event: &KeymapEvent| {
            if event.name != "escape" {
                return InterceptDecision::pass();
            }
            InterceptDecision {
                consume: prevent_default,
                clear_pending: true,
                pop_pending: false,
                only_when_pending: true,
            }
        }),
        priority,
    )
}

/// `BackspacePopsPendingSequenceOptions`
/// (backspace-pops-pending-sequence.ts:3-10).
#[derive(Debug, Clone, Copy)]
pub struct BackspacePopsPendingSequenceOptions {
    pub prevent_default: Option<bool>,
    pub priority: Option<i32>,
}

/// `registerBackspacePopsPendingSequence`
/// (backspace-pops-pending-sequence.ts:15-38): lets Backspace step
/// back through the current pending multi-key sequence. Consumes the
/// backspace only while a sequence was pending.
pub fn register_backspace_pops_pending_sequence(
    keymap: &mut crate::Keymap,
    options: Option<BackspacePopsPendingSequenceOptions>,
) -> Registration {
    let prevent_default = options.and_then(|o| o.prevent_default).unwrap_or(true);
    let priority = options.and_then(|o| o.priority).unwrap_or(0);
    keymap.engine.intercept_key(
        Arc::new(move |event: &KeymapEvent| {
            if event.name != "backspace" {
                return InterceptDecision::pass();
            }
            InterceptDecision {
                consume: prevent_default,
                clear_pending: false,
                pop_pending: true,
                only_when_pending: true,
            }
        }),
        priority,
    )
}

/// `registerCommaBindings` (comma-bindings.ts:34-40): expands
/// comma-separated binding strings such as `j,k` into separate
/// bindings (the expander itself:
/// [`comma_binding_expander`](crate::parser::comma_binding_expander)).
pub fn register_comma_bindings(keymap: &mut crate::Keymap) -> Registration {
    keymap
        .engine
        .acquire_resource_with_key(COMMA_BINDINGS_RESOURCE, |engine| {
            vec![
                engine.append_binding_expander(std::sync::Arc::new(|input, displays| {
                    comma_binding_expander(input, displays)
                })),
            ]
        })
}

/// `getBaseLayoutKeyName` (opentui/base-layout.ts:6-22): map a kitty
/// base-layout codepoint to its key name — control codes and DEL are
/// dropped, `A`-`Z` lowercases.
#[must_use]
pub fn base_layout_key_name(base_code: Option<u32>) -> Option<String> {
    let base_code = base_code?;
    if base_code < 32 || base_code == 127 {
        return None;
    }
    char::from_u32(base_code).map(|name| {
        if name.is_ascii_uppercase() {
            name.to_ascii_lowercase().to_string()
        } else {
            name.to_string()
        }
    })
}

/// `registerBaseLayoutFallback` (opentui/base-layout.ts:28-48): an
/// appended event-match resolver that ALSO matches the event's base
/// layout codepoint, so bindings ignore active keyboard layout
/// changes. Direct stroke matches stay ahead of the fallback (the
/// engine dispatches each match key in order — dispatch.ts:576-584).
pub fn register_base_layout_fallback(keymap: &mut crate::Keymap) -> Registration {
    keymap
        .engine
        .acquire_resource_with_key(BASE_LAYOUT_FALLBACK_RESOURCE, |engine| {
            vec![
                engine.append_event_match_resolver(Arc::new(|event: &KeymapEvent| {
                    let Some(name) = base_layout_key_name(event.base_code) else {
                        return Vec::new();
                    };
                    let stroke = KeyStrokeInput {
                        name,
                        ctrl: Some(event.ctrl),
                        shift: Some(event.shift),
                        meta: Some(event.meta),
                        super_key: Some(event.super_key),
                        hyper: Some(event.hyper),
                    };
                    create_key_match(&stroke).map_or_else(|_| Vec::new(), |key| vec![key])
                })),
            ]
        })
}

/// `registerEnabledFields` (addons/universal/enabled.ts:43-84): the
/// `enabled` layer/command field. The port compiles
/// [`Enabled`](crate::types::Enabled) directly; this installs the
/// feature (flag + reference-counted resource).
pub fn register_enabled_fields(keymap: &mut crate::Keymap) -> Registration {
    keymap
        .engine
        .acquire_resource_with_key(ENABLED_FIELDS_RESOURCE, |engine| {
            engine.install_enabled_fields();
            Vec::new()
        })
}

/// `registerMetadataFields` (addons/universal/metadata.ts:29-66):
/// `desc`/`group` binding attrs and `desc`/`title`/`category`
/// command attrs (the port carries them in `Binding::attrs` /
/// `Command::fields` directly).
pub fn register_metadata_fields(keymap: &mut crate::Keymap) -> Registration {
    keymap
        .engine
        .acquire_resource_with_key(METADATA_FIELDS_RESOURCE, |engine| {
            engine.install_metadata_fields();
            Vec::new()
        })
}

/// `registerDefaultKeys` (addons/universal/default-parser.ts:529-539):
/// the standard binding parser + event matcher used by most keymaps.
pub fn register_default_keys(keymap: &mut crate::Keymap) -> Registration {
    keymap
        .engine
        .acquire_resource_with_key(DEFAULT_KEYS_RESOURCE, |engine| {
            engine.install_default_keys();
            Vec::new()
        })
}

/// Wall-clock helper (the timed leader's `setTimeout` timebase when
/// no clock is injected).
#[must_use]
pub fn now_ms() -> u64 {
    SystemClock.now_ms()
}

/// `createOpenTuiHostMetadata` (opentui.ts:33-52): platform +
/// kitty-keyboard capability matrix (`super`/`hyper` are only known
/// under the kitty protocol).
#[must_use]
pub fn create_open_tui_host_metadata(
    platform: HostPlatform,
    has_kitty_keyboard: bool,
) -> HostMetadata {
    let primary_modifier = match platform {
        HostPlatform::Macos => PrimaryModifier::Super,
        HostPlatform::Unknown => PrimaryModifier::Unknown,
        _ => PrimaryModifier::Ctrl,
    };
    let capability = |flag: bool| {
        if flag {
            HostCapability::Supported
        } else {
            HostCapability::Unknown
        }
    };
    HostMetadata {
        platform,
        primary_modifier,
        modifiers: [
            (HostModifier::Ctrl, HostCapability::Supported),
            (HostModifier::Shift, HostCapability::Supported),
            (HostModifier::Meta, HostCapability::Supported),
            (HostModifier::Super, capability(has_kitty_keyboard)),
            (HostModifier::Hyper, capability(has_kitty_keyboard)),
        ]
        .into_iter()
        .collect(),
    }
}
