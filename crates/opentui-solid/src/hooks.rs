//! Hooks — reactive accessors over the renderer context
//! (upstream `elements/hooks.ts`).
//!
//! * `useRenderer` (hooks.ts:14-22): the context lookup — here the
//!   [`SolidRenderer`](crate::SolidRenderer) IS the context, so the
//!   port exposes it as a method.
//! * `useTerminalDimensions` (hooks.ts:36-50): terminal size AS A
//!   SIGNAL — the port keeps an internal `(width, height)` signal
//!   that [`SolidRenderer::resize`] sets (upstream: the renderer's
//!   `resize` event feeding `setTerminalDimensions`).
//! * `useKeyboard` (hooks.ts:73-89): key events as a stream — press
//!   events by default (with repeats), release events opt-in
//!   (`options.release`). The port models the renderer's `keyInput`
//!   emitter as an [`EventHub`]; the host feeds raw events through
//!   [`SolidRenderer::feed_key`] (the TUI app's PTY seam).
//! * `usePaste` (hooks.ts:91-101): paste events as a stream.
//! * `onResize`/`onFocus`/`onBlur` (hooks.ts:24-33, 108-130): event
//!   subscriptions tied to mount scope — the port returns
//!   unsubscribers (the caller detaches on unmount, like upstream's
//!   `onCleanup`).

use crate::reactive::Signal;
use std::cell::RefCell;
use std::rc::Rc;

/// `KeyEvent` — the fields the port surface needs
/// (core/lib/KeyHandler.ts:5-49: name/ctrl/meta/shift/option/
/// sequence/raw/eventType/source/repeated + preventDefault /
/// stopPropagation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub name: String,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub option: bool,
    pub sequence: String,
    pub raw: String,
    /// "press" | "release"
    pub event_type: KeyEventType,
    pub repeated: bool,
    pub default_prevented: std::cell::Cell<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Release,
}

impl KeyEvent {
    #[must_use]
    pub fn press(name: &str) -> Self {
        KeyEvent {
            name: name.to_owned(),
            ctrl: false,
            meta: false,
            shift: false,
            option: false,
            sequence: String::new(),
            raw: String::new(),
            event_type: KeyEventType::Press,
            repeated: false,
            default_prevented: std::cell::Cell::new(false),
        }
    }

    pub fn prevent_default(&self) {
        self.default_prevented.set(true);
    }

    #[must_use]
    pub fn default_prevented(&self) -> bool {
        self.default_prevented.get()
    }
}

/// `PasteEvent` (core/lib/KeyHandler.ts:64-80): pasted bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteEvent {
    pub bytes: Vec<u8>,
}

/// Event payload for node-level event props (`on:*`, onChange, … —
/// reconciler.ts:231-239, 272-334). The port keeps a closed set of
/// payloads the ported renderables can emit.
#[derive(Debug, Clone, PartialEq)]
pub enum EventData {
    /// A generic named event with a JSON-ish payload string.
    Custom { name: String, payload: String },
    /// Keyboard events forwarded from a focused node (future Input).
    Key(KeyEvent),
    /// A value-changed payload (onChange/onInput — Input/Select).
    Changed { value: String },
}

/// The event-stream shape shared by keyboard/paste/node events: a
/// registry of handlers with dispose handles (upstream: EventEmitter
/// `on`/`off` pairs managed by onMount/onCleanup, hooks.ts:77-88).
/// Interior-shared so unsubscribers detach without borrowing the hub.
pub struct EventHub<T> {
    inner: Rc<HubInner<T>>,
}

type HandlerList<T> = Vec<(u64, Rc<dyn Fn(&T)>)>;

struct HubInner<T> {
    handlers: RefCell<HandlerList<T>>,
    next_id: std::cell::Cell<u64>,
}

impl<T> Default for EventHub<T> {
    fn default() -> Self {
        EventHub {
            inner: Rc::new(HubInner {
                handlers: RefCell::new(Vec::new()),
                next_id: std::cell::Cell::new(1),
            }),
        }
    }
}

impl<T: 'static> EventHub<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// `emitter.on(event, cb)` — returns the `off` handle (upstream
    /// `onCleanup(() => keyHandler.off(...))`, hooks.ts:81-88).
    pub fn on(&self, cb: impl Fn(&T) + 'static) -> Box<dyn Fn()> {
        let id = self.inner.next_id.get();
        self.inner.next_id.set(id + 1);
        self.inner.handlers.borrow_mut().push((id, Rc::new(cb)));
        let weak = Rc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.handlers.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// `emitter.emit(event, payload)` — handlers in registration order.
    pub fn emit(&self, event: &T) {
        let taken = std::mem::take(&mut *self.inner.handlers.borrow_mut());
        for (_, cb) in &taken {
            cb(event);
        }
        self.inner.handlers.borrow_mut().extend(taken);
    }

    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.inner.handlers.borrow().len()
    }
}

/// Terminal dimensions as a signal (useTerminalDimensions,
/// hooks.ts:36-50).
pub type DimensionsSignal = Signal<(u32, u32)>;
