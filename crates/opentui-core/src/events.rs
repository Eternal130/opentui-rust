//! Event types + the key handler — `packages/core/src/lib/KeyHandler.ts`
//! and the renderer-side event shapes (`renderer.ts:653-711`).
//!
//! * [`KeyEvent`] / [`PasteEvent`] — the dispatched event objects with
//!   `preventDefault` / `stopPropagation` (KeyHandler.ts:5-91).
//! * [`MouseEvent`] / [`MouseButton`] — renderer.ts:653-711.
//! * [`KeyHandler`] — the emit-with-priority dispatch contract
//!   (KeyHandler.ts:99-222, `InternalKeyHandler.emitWithPriority`):
//!   global listeners first, then renderable (internal) handlers, with
//!   propagation/prevention checks between every handler, and a
//!   handler-list snapshot so listeners added mid-dispatch do not
//!   receive the in-flight event (KeyHandler.ts:169-171).

use crate::parse_keypress::{KeyEventType, KeySource, ParsedKey};
use crate::parse_mouse::{MouseEventType, MouseModifiers, RawMouseEvent, ScrollInfo};
use crate::paste::PasteMetadata;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `KeyEvent` (KeyHandler.ts:5-62) — a [`ParsedKey`] plus the
/// mutable-prevention flags. The parser output is immutable; the event
/// wraps it and tracks prevention.
#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub key: ParsedKey,
    default_prevented: bool,
    propagation_stopped: bool,
}

impl KeyEvent {
    pub fn new(key: ParsedKey) -> Self {
        KeyEvent {
            key,
            default_prevented: false,
            propagation_stopped: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.key.name
    }

    pub fn ctrl(&self) -> bool {
        self.key.ctrl
    }

    pub fn meta(&self) -> bool {
        self.key.meta
    }

    pub fn shift(&self) -> bool {
        self.key.shift
    }

    pub fn option(&self) -> bool {
        self.key.option
    }

    pub fn number(&self) -> bool {
        self.key.number
    }

    pub fn sequence(&self) -> &str {
        &self.key.sequence
    }

    pub fn raw(&self) -> &str {
        &self.key.raw
    }

    pub fn event_type(&self) -> KeyEventType {
        self.key.event_type
    }

    pub fn source(&self) -> KeySource {
        self.key.source
    }

    pub fn code(&self) -> Option<&str> {
        self.key.code.as_deref()
    }

    pub fn super_key(&self) -> bool {
        self.key.super_key.unwrap_or(false)
    }

    pub fn hyper(&self) -> bool {
        self.key.hyper.unwrap_or(false)
    }

    pub fn caps_lock(&self) -> bool {
        self.key.caps_lock.unwrap_or(false)
    }

    pub fn num_lock(&self) -> bool {
        self.key.num_lock.unwrap_or(false)
    }

    pub fn base_code(&self) -> Option<u32> {
        self.key.base_code
    }

    pub fn repeated(&self) -> bool {
        self.key.repeated.unwrap_or(false)
    }

    pub fn default_prevented(&self) -> bool {
        self.default_prevented
    }

    pub fn propagation_stopped(&self) -> bool {
        self.propagation_stopped
    }

    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }

    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
}

/// `PasteEvent` (KeyHandler.ts:64-91).
#[derive(Debug, Clone)]
pub struct PasteEvent {
    pub bytes: Vec<u8>,
    pub metadata: Option<PasteMetadata>,
    default_prevented: bool,
    propagation_stopped: bool,
}

impl PasteEvent {
    pub fn new(bytes: Vec<u8>, metadata: Option<PasteMetadata>) -> Self {
        PasteEvent {
            bytes,
            metadata,
            default_prevented: false,
            propagation_stopped: false,
        }
    }

    /// Decoded text (`decodePasteBytes` on the bytes — Input.ts:110).
    pub fn text(&self) -> String {
        crate::paste::decode_paste_bytes(&self.bytes)
    }

    pub fn default_prevented(&self) -> bool {
        self.default_prevented
    }

    pub fn propagation_stopped(&self) -> bool {
        self.propagation_stopped
    }

    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }

    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
}

/// The events a [`KeyHandler`] dispatches (`KeyHandlerEventMap`,
/// KeyHandler.ts:93-97).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyHandlerEvent {
    Keypress,
    Keyrelease,
    Paste,
}

type SharedKeyHandlerFn = Arc<Mutex<Box<dyn FnMut(&mut KeyEvent) + Send>>>;
type SharedPasteHandlerFn = Arc<Mutex<Box<dyn FnMut(&mut PasteEvent) + Send>>>;

/// Subscription handle — dropping it unregisters the handler
/// (upstream `off(handler)` / `offInternal`).
pub struct Subscription {
    key: Option<(KeyHandlerEvent, u64)>,
    registry: Arc<Mutex<HandlerRegistry>>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("key", &self.key)
            .finish()
    }
}

impl Subscription {
    /// Explicit removal (upstream `off`). Dropping does the same.
    pub fn off(self) {}
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some((event, id)) = self.key.take() {
            if let Ok(mut reg) = self.registry.lock() {
                reg.remove(event, id);
            }
        }
    }
}

#[derive(Default)]
struct HandlerRegistry {
    next_id: u64,
    /// Global listeners (`EventEmitter.on`, KeyHandler.ts:147).
    global_key: HashMap<KeyHandlerEvent, Vec<(u64, SharedKeyHandlerFn)>>,
    global_paste: Vec<(u64, SharedPasteHandlerFn)>,
    /// Renderable listeners (`onInternal`, KeyHandler.ts:203-211) —
    /// dispatched AFTER the globals.
    internal_key: HashMap<KeyHandlerEvent, Vec<(u64, SharedKeyHandlerFn)>>,
    internal_paste: Vec<(u64, SharedPasteHandlerFn)>,
}

impl HandlerRegistry {
    fn remove(&mut self, event: KeyHandlerEvent, id: u64) {
        if event == KeyHandlerEvent::Paste {
            self.global_paste.retain(|(hid, _)| *hid != id);
            self.internal_paste.retain(|(hid, _)| *hid != id);
        } else {
            if let Some(list) = self.global_key.get_mut(&event) {
                list.retain(|(hid, _)| *hid != id);
            }
            if let Some(list) = self.internal_key.get_mut(&event) {
                list.retain(|(hid, _)| *hid != id);
            }
        }
    }
}

/// `KeyHandler` (KeyHandler.ts:99-128) with the
/// `InternalKeyHandler` (KeyHandler.ts:134-222) priority dispatch
/// merged in: `on*` registers global listeners; `on_internal_*`
/// registers renderable listeners that only run when the globals did
/// not prevent or stop the event.
///
/// Divergence: upstream catches handler exceptions and continues
/// dispatch (`try/catch` + console.error, KeyHandler.ts:113-116,
/// 152-164); Rust handlers are infallible — a panicking handler
/// propagates the panic.
#[derive(Default)]
pub struct KeyHandler {
    registry: Arc<Mutex<HandlerRegistry>>,
}

impl std::fmt::Debug for KeyHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reg = self.registry.lock().map(|r| {
            (
                r.global_key.values().map(Vec::len).sum::<usize>(),
                r.internal_key.values().map(Vec::len).sum::<usize>(),
                r.global_paste.len() + r.internal_paste.len(),
            )
        });
        match reg {
            Ok((g, i, p)) => write!(f, "KeyHandler({g} global key, {i} internal key, {p} paste)"),
            Err(_) => write!(f, "KeyHandler(<locked>)"),
        }
    }
}

impl KeyHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// `on("keypress" | "keyrelease", handler)`.
    pub fn on_key<F>(&self, event: KeyHandlerEvent, handler: F) -> Subscription
    where
        F: FnMut(&mut KeyEvent) + Send + 'static,
    {
        self.register_key(event, handler, false)
    }

    /// `on("paste", handler)`.
    pub fn on_paste<F>(&self, handler: F) -> Subscription
    where
        F: FnMut(&mut PasteEvent) + Send + 'static,
    {
        self.register_paste(handler, false)
    }

    /// `InternalKeyHandler.onInternal` (KeyHandler.ts:203-211).
    pub fn on_internal_key<F>(&self, event: KeyHandlerEvent, handler: F) -> Subscription
    where
        F: FnMut(&mut KeyEvent) + Send + 'static,
    {
        self.register_key(event, handler, true)
    }

    pub fn on_internal_paste<F>(&self, handler: F) -> Subscription
    where
        F: FnMut(&mut PasteEvent) + Send + 'static,
    {
        self.register_paste(handler, true)
    }

    fn register_key<F>(&self, event: KeyHandlerEvent, handler: F, internal: bool) -> Subscription
    where
        F: FnMut(&mut KeyEvent) + Send + 'static,
    {
        let mut reg = self.registry.lock().expect("key handler registry lock");
        reg.next_id += 1;
        let id = reg.next_id;
        let shared: SharedKeyHandlerFn = Arc::new(Mutex::new(Box::new(handler)));
        if internal {
            reg.internal_key
                .entry(event)
                .or_default()
                .push((id, shared));
        } else {
            reg.global_key.entry(event).or_default().push((id, shared));
        }
        Subscription {
            key: Some((event, id)),
            registry: Arc::clone(&self.registry),
        }
    }

    fn register_paste<F>(&self, handler: F, internal: bool) -> Subscription
    where
        F: FnMut(&mut PasteEvent) + Send + 'static,
    {
        let mut reg = self.registry.lock().expect("key handler registry lock");
        reg.next_id += 1;
        let id = reg.next_id;
        let shared: SharedPasteHandlerFn = Arc::new(Mutex::new(Box::new(handler)));
        if internal {
            reg.internal_paste.push((id, shared));
        } else {
            reg.global_paste.push((id, shared));
        }
        Subscription {
            key: Some((KeyHandlerEvent::Paste, id)),
            registry: Arc::clone(&self.registry),
        }
    }

    /// `processParsedKey` (KeyHandler.ts:100-119): dispatch by event
    /// type. Returns whether any listener ran.
    pub fn process_parsed_key(&self, parsed_key: ParsedKey) -> bool {
        let event_kind = match parsed_key.event_type {
            KeyEventType::Release => KeyHandlerEvent::Keyrelease,
            _ => KeyHandlerEvent::Keypress,
        };
        let mut event = KeyEvent::new(parsed_key);
        self.emit_key(event_kind, &mut event)
    }

    /// `processPaste` (KeyHandler.ts:121-127).
    pub fn process_paste(&self, bytes: Vec<u8>, metadata: Option<PasteMetadata>) -> bool {
        let mut event = PasteEvent::new(bytes, metadata);
        self.emit_paste(&mut event)
    }

    /// `emitWithPriority` (KeyHandler.ts:141-201).
    fn emit_key(&self, event: KeyHandlerEvent, key_event: &mut KeyEvent) -> bool {
        // Snapshot the handler list so listeners added during dispatch
        // (e.g. via focus changes) do not receive the in-flight event
        // (KeyHandler.ts:169-171).
        let global = self.snapshot_key(event, false);
        let internal = self.snapshot_key(event, true);
        let has_global = !global.is_empty();
        let has_internal = !internal.is_empty();

        for handler in global {
            {
                let mut f = handler.lock().expect("handler lock");
                f(key_event);
            }
            if key_event.propagation_stopped() {
                return has_global;
            }
        }

        if has_internal {
            if key_event.default_prevented() || key_event.propagation_stopped() {
                return has_global || has_internal;
            }
            for handler in internal {
                {
                    let mut f = handler.lock().expect("handler lock");
                    f(key_event);
                }
                if key_event.propagation_stopped() {
                    return has_global || has_internal;
                }
            }
        }
        has_global || has_internal
    }

    fn emit_paste(&self, paste_event: &mut PasteEvent) -> bool {
        let global = self.snapshot_paste(false);
        let internal = self.snapshot_paste(true);
        let has_global = !global.is_empty();
        let has_internal = !internal.is_empty();

        for handler in global {
            {
                let mut f = handler.lock().expect("handler lock");
                f(paste_event);
            }
            if paste_event.propagation_stopped() {
                return has_global;
            }
        }

        if has_internal {
            if paste_event.default_prevented() || paste_event.propagation_stopped() {
                return has_global || has_internal;
            }
            for handler in internal {
                {
                    let mut f = handler.lock().expect("handler lock");
                    f(paste_event);
                }
                if paste_event.propagation_stopped() {
                    return has_global || has_internal;
                }
            }
        }
        has_global || has_internal
    }

    fn snapshot_key(&self, event: KeyHandlerEvent, internal: bool) -> Vec<SharedKeyHandlerFn> {
        let reg = self.registry.lock().expect("key handler registry lock");
        let map = if internal {
            &reg.internal_key
        } else {
            &reg.global_key
        };
        map.get(&event)
            .map(|list| list.iter().map(|(_, h)| Arc::clone(h)).collect())
            .unwrap_or_default()
    }

    fn snapshot_paste(&self, internal: bool) -> Vec<SharedPasteHandlerFn> {
        let reg = self.registry.lock().expect("key handler registry lock");
        let list = if internal {
            &reg.internal_paste
        } else {
            &reg.global_paste
        };
        list.iter().map(|(_, h)| Arc::clone(h)).collect()
    }
}

/// `MouseButton` (renderer.ts:705-711).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    WheelUp = 4,
    WheelDown = 5,
}

impl From<MouseButton> for i32 {
    fn from(b: MouseButton) -> i32 {
        b as i32
    }
}

/// `MouseEvent` (renderer.ts:653-698) — the DOM-shaped mouse event the
/// renderable tree receives. `target`/`source` carry renderable ids
/// (upstream: the Renderable instances).
#[derive(Debug, Clone)]
pub struct MouseEvent {
    pub event_type: MouseEventType,
    pub button: i32,
    pub x: i32,
    pub y: i32,
    pub modifiers: MouseModifiers,
    pub scroll: Option<ScrollInfo>,
    pub target: Option<String>,
    /// `source` — the drag-capture source renderable id (renderer.ts:3759).
    pub source: Option<String>,
    pub is_dragging: bool,
    default_prevented: bool,
    propagation_stopped: bool,
}

impl MouseEvent {
    /// `new MouseEvent(target, attributes)` (renderer.ts:679-689).
    pub fn new(
        target: Option<&str>,
        attributes: &RawMouseEvent,
        source: Option<&str>,
        is_dragging: bool,
    ) -> Self {
        MouseEvent {
            event_type: attributes.event_type,
            button: attributes.button,
            x: attributes.x,
            y: attributes.y,
            modifiers: attributes.modifiers,
            scroll: attributes.scroll,
            target: target.map(str::to_owned),
            source: source.map(str::to_owned),
            is_dragging,
            default_prevented: false,
            propagation_stopped: false,
        }
    }

    /// The `type` override used for out/over/drag-end/drop synthesis
    /// (renderer.ts:3748-3783).
    pub fn with_type(mut self, event_type: MouseEventType) -> Self {
        self.event_type = event_type;
        self
    }

    pub fn default_prevented(&self) -> bool {
        self.default_prevented
    }

    pub fn propagation_stopped(&self) -> bool {
        self.propagation_stopped
    }

    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }

    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keypress::{parse_keypress, ParseKeypressOptions};

    fn key(name: &str) -> KeyEvent {
        KeyEvent::new(parse_keypress(name, ParseKeypressOptions::default()).unwrap())
    }

    #[test]
    fn dispatch_by_event_type() {
        // KeyHandler.processParsedKey routes release → keyrelease
        let handler = KeyHandler::new();
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let _s1 = {
            let seen = Arc::clone(&seen);
            handler.on_key(KeyHandlerEvent::Keypress, move |_e| {
                seen.lock().unwrap().push("press");
            })
        };
        let _s2 = {
            let seen = Arc::clone(&seen);
            handler.on_key(KeyHandlerEvent::Keyrelease, move |_e| {
                seen.lock().unwrap().push("release");
            })
        };
        let mut parsed = key("a").key;
        parsed.event_type = KeyEventType::Release;
        handler.process_parsed_key(parsed);
        handler.process_parsed_key(key("a").key);
        assert_eq!(*seen.lock().unwrap(), vec!["release", "press"]);
    }

    #[test]
    fn unsubscription_removes_handler() {
        let handler = KeyHandler::new();
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let s = {
            let seen = Arc::clone(&seen);
            handler.on_key(KeyHandlerEvent::Keypress, move |_e| {
                seen.lock().unwrap().push("press");
            })
        };
        drop(s);
        assert!(!handler.process_parsed_key(key("a").key));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn globals_before_internal_and_prevention_gates_internal() {
        // InternalKeyHandler.emitWithPriority (KeyHandler.ts:141-201):
        // global listeners run first; internal handlers are skipped
        // when the event is defaultPrevented or propagationStopped.
        let handler = KeyHandler::new();
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let _g = {
            let order = Arc::clone(&order);
            handler.on_key(KeyHandlerEvent::Keypress, move |e| {
                order.lock().unwrap().push("global");
                e.prevent_default();
            })
        };
        let _i = {
            let order = Arc::clone(&order);
            handler.on_internal_key(KeyHandlerEvent::Keypress, move |_e| {
                order.lock().unwrap().push("internal");
            })
        };
        handler.process_parsed_key(key("a").key);
        // defaultPrevented after the global → internal never runs
        assert_eq!(*order.lock().unwrap(), vec!["global"]);
    }

    #[test]
    fn no_prevention_runs_both_in_order() {
        let handler = KeyHandler::new();
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let _g = {
            let order = Arc::clone(&order);
            handler.on_key(KeyHandlerEvent::Keypress, move |_e| {
                order.lock().unwrap().push("global");
            })
        };
        let _i = {
            let order = Arc::clone(&order);
            handler.on_internal_key(KeyHandlerEvent::Keypress, move |_e| {
                order.lock().unwrap().push("internal");
            })
        };
        assert!(handler.process_parsed_key(key("a").key));
        assert_eq!(*order.lock().unwrap(), vec!["global", "internal"]);
    }

    #[test]
    fn stop_propagation_between_globals() {
        let handler = KeyHandler::new();
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let _g1 = {
            let order = Arc::clone(&order);
            handler.on_key(KeyHandlerEvent::Keypress, move |e| {
                order.lock().unwrap().push("g1");
                e.stop_propagation();
            })
        };
        let _g2 = {
            let order = Arc::clone(&order);
            handler.on_key(KeyHandlerEvent::Keypress, move |_e| {
                order.lock().unwrap().push("g2");
            })
        };
        handler.process_parsed_key(key("a").key);
        assert_eq!(*order.lock().unwrap(), vec!["g1"]);
    }

    #[test]
    fn handler_added_during_dispatch_not_called_in_flight() {
        // KeyHandler.ts:169-171 snapshot semantics: a listener added
        // during dispatch (e.g. via a focus change) must not receive
        // the in-flight key event.
        use std::sync::OnceLock;
        let handler: Arc<KeyHandler> = Arc::new(KeyHandler::new());
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        struct LateState {
            hits: Arc<Mutex<Vec<&'static str>>>,
            subscription: Mutex<Option<Subscription>>,
        }
        static LATE: OnceLock<LateState> = OnceLock::new();

        let _g = {
            let seen = Arc::clone(&seen);
            let late_handler = Arc::clone(&handler);
            handler.on_key(KeyHandlerEvent::Keypress, move |_e| {
                seen.lock().unwrap().push("global");
                let late = LATE.get_or_init(|| LateState {
                    hits: Arc::new(Mutex::new(Vec::new())),
                    subscription: Mutex::new(None),
                });
                let hits = Arc::clone(&late.hits);
                // keep the subscription: dropping it unregisters
                // (upstream `offInternal`; the RAII port needs the
                // handle held by the owner)
                let sub = late_handler.on_internal_key(KeyHandlerEvent::Keypress, move |_e| {
                    hits.lock().unwrap().push("internal");
                });
                *late.subscription.lock().unwrap() = Some(sub);
            })
        };
        handler.process_parsed_key(key("a").key);
        assert_eq!(*seen.lock().unwrap(), vec!["global"]);
        let late = LATE.get().unwrap();
        assert!(
            late.hits.lock().unwrap().is_empty(),
            "in-flight event leaked to late handler"
        );
        // the NEXT event does reach the late-registered handler
        handler.process_parsed_key(key("a").key);
        assert_eq!(*late.hits.lock().unwrap(), vec!["internal"]);
    }

    #[test]
    fn paste_events_flow() {
        let handler = KeyHandler::new();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let _p = {
            let seen = Arc::clone(&seen);
            handler.on_paste(move |e| {
                seen.lock().unwrap().push(e.text());
            })
        };
        handler.process_paste(b"pasted text".to_vec(), None);
        assert_eq!(*seen.lock().unwrap(), vec!["pasted text".to_owned()]);
    }

    #[test]
    fn mouse_event_shape() {
        let raw = RawMouseEvent {
            event_type: MouseEventType::Down,
            button: 0,
            x: 3,
            y: 4,
            modifiers: MouseModifiers::default(),
            scroll: None,
        };
        let mut evt = MouseEvent::new(Some("input-1"), &raw, None, false);
        assert_eq!(evt.target.as_deref(), Some("input-1"));
        assert_eq!(evt.button, MouseButton::Left as i32);
        assert!(!evt.default_prevented());
        evt.prevent_default();
        assert!(evt.default_prevented());
        let out = evt.with_type(MouseEventType::Out);
        assert_eq!(out.event_type, MouseEventType::Out);
    }
}
