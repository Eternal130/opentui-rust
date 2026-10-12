//! `@opentui/keymap/solid` (`src/solid/index.ts`) — the provider and
//! hooks, as Rust-idiomatic APIs over the crate's reactive
//! conventions.
//!
//! Upstream is JSX + solid-js; Rust has neither, so the mapping is
//! (documented in the crate README):
//!
//! | upstream | port |
//! |---|---|
//! | `KeymapProvider { keymap }` + `useContext` | [`KeymapProvider`]
//! | `useKeymap()` | [`KeymapProvider::use_keymap`] |
//! | `useKeymapStateVersion` (solid/index.ts:62-79) | the provider's
//!   state version subscription |
//! | `useKeymapSelector(selector)` (solid/index.ts:85-105) |
//!   [`KeymapProvider::use_keymap_selector`] — re-runs the selector on
//!   each batched state change |
//! | `useBindings(createLayer)` (solid/index.ts:107-149) |
//!   [`KeymapProvider::use_bindings`] — registers a layer for a
//!   scope; dropping the guard unregisters (`onCleanup`) |
//! | `useActiveKeys` / `usePendingSequence` (react/index.ts:82-100) |
//!   selector-based reads over the same version signal |
//! | `reactiveMatcherFromSignal` (solid/index.ts:156-171) |
//!   [`reactive_matcher_from_signal`] |
//!
//! No JSX: the provider is a context struct owning the shared keymap
//! and the hooks are accessor methods. The crate's reactive
//! convention is the engine's `state` event (a version counter with
//! subscribers — `useKeymapStateVersion`'s bump channel).

use crate::engine::{KeymapEngine, Registration, TargetId};
use crate::types::{Enabled, KeySequencePart, KeymapEvent, Layer, ReactiveMatcher, TargetMode};
use std::sync::{Arc, Mutex, Weak};

/// `KeymapProvider` (solid/index.ts:27-36) — the context root. The
/// keymap is shared behind an `Arc<Mutex<…>>` (JSX context → an
/// explicit handle); children hold clones.
pub struct KeymapProvider {
    keymap: Arc<Mutex<crate::Keymap>>,
}

impl KeymapProvider {
    /// Mounts the provider over a keymap (the JSX element constructor).
    #[must_use]
    pub fn new(keymap: crate::Keymap) -> Self {
        KeymapProvider {
            keymap: Arc::new(Mutex::new(keymap)),
        }
    }

    /// The shared keymap handle (what descendants receive).
    #[must_use]
    pub fn handle(&self) -> KeymapHandle {
        KeymapHandle {
            keymap: Arc::downgrade(&self.keymap),
        }
    }

    /// `useKeymap()` (solid/index.ts:52-60): the context's keymap;
    /// panics outside a live provider ("Keymap not found. Wrap the
    /// tree in <KeymapProvider>.").
    #[must_use]
    pub fn use_keymap(&self) -> KeymapHandle {
        self.handle()
    }

    /// `useKeymapSelector(selector)` (solid/index.ts:85-105):
    /// reactively derive any view from the keymap by re-running
    /// `selector` on each batched keymap state change.
    #[must_use]
    pub fn use_keymap_selector<T: Clone>(
        &self,
        selector: impl Fn(&mut crate::Keymap) -> T,
    ) -> Selector<T> {
        let handle = self.handle();
        let initial = handle.with_keymap(|keymap| selector(keymap));
        let version = handle.with_keymap(|keymap| keymap.state_version());
        Selector {
            handle,
            value: initial,
            last_version: version,
        }
    }

    /// `useBindings(createLayer)` (solid/index.ts:107-149): register a
    /// layer for the provider's scope. Local bindings need a target
    /// (the `target` accessor); targetMode defaults to
    /// `focus-within` for explicit targets (line 116). Dropping the
    /// returned guard unregisters (`onCleanup`, lines 124-127).
    pub fn use_bindings(
        &self,
        create_layer: impl FnOnce() -> UseBindingsLayer,
    ) -> Option<BindingsGuard> {
        let layer = create_layer();
        let handle = self.handle();
        handle.with_keymap(|keymap| {
            let has_explicit_target = layer.target.is_some();
            let target_mode = layer
                .target_mode
                .or(has_explicit_target.then_some(TargetMode::FocusWithin));
            if target_mode.is_some() && !has_explicit_target {
                // solid/index.ts:131-133 — "useBindings local bindings
                // need a target accessor".
                panic!("useBindings local bindings need a target accessor");
            }
            if has_explicit_target && layer.target.is_none() {
                // A target-mode layer without a CURRENT target skips
                // registration (solid/index.ts:135-137).
                return None;
            }
            let registration = keymap.engine.register_layer(Layer {
                target: layer.target,
                target_mode,
                priority: layer.priority,
                bindings: layer.bindings,
                commands: layer.commands,
                mode: layer.mode,
                enabled: layer.enabled,
            });
            Some(BindingsGuard {
                handle: self.handle(),
                registration,
            })
        })
    }
}

/// A child's shared-keymap reference (`useContext(KeymapContext)`).
#[derive(Clone)]
pub struct KeymapHandle {
    keymap: Weak<Mutex<crate::Keymap>>,
}

impl KeymapHandle {
    /// Run a read/write against the shared keymap (the reactive
    /// scope's tracked access — every `with_keymap` observes the
    /// current state version).
    pub fn with_keymap<T>(&self, run: impl FnOnce(&mut crate::Keymap) -> T) -> T {
        let keymap = self
            .keymap
            .upgrade()
            .expect("Keymap not found. Wrap the tree in <KeymapProvider>.");
        let mut guard = keymap.lock().expect("keymap lock");
        run(&mut guard)
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.keymap.upgrade().is_some()
    }
}

/// The `useKeymapSelector` memo (solid's `createMemo` + version
/// signal): [`Selector::read`] re-runs the selector when the keymap's
/// state version moved since the last read.
pub struct Selector<T> {
    handle: KeymapHandle,
    value: T,
    last_version: u64,
}

impl<T: Clone> Selector<T> {
    /// The memoized read — re-derives on state change
    /// (`createMemo((previous) => { version(); … })`).
    #[must_use]
    pub fn read(&mut self, selector: impl Fn(&mut crate::Keymap) -> T) -> T {
        let version = self.handle.with_keymap(|keymap| keymap.state_version());
        if version != self.last_version {
            self.last_version = version;
            self.value = self.handle.with_keymap(|keymap| selector(keymap));
        }
        self.value.clone()
    }
}

/// `UseBindingsLayer` (solid/index.ts:40-46): a layer whose target is
/// an accessor; the Rust port resolves the accessor at registration
/// time (the reactive re-run is a new `use_bindings` call).
#[derive(Default)]
pub struct UseBindingsLayer {
    pub target: Option<TargetId>,
    pub target_mode: Option<TargetMode>,
    pub priority: i32,
    pub bindings: crate::types::Bindings,
    pub commands: Vec<crate::types::Command>,
    pub mode: Option<String>,
    pub enabled: Option<Enabled>,
}

/// The `useBindings` cleanup guard (`onCleanup(() => dispose())`).
pub struct BindingsGuard {
    handle: KeymapHandle,
    registration: Registration,
}

impl Drop for BindingsGuard {
    fn drop(&mut self) {
        let registration = self.registration.clone();
        self.handle
            .with_keymap(|keymap| keymap.release(registration));
    }
}

/// `usePendingSequence()` (react/index.ts:92-100; v2's
/// `useLeaderActive` reads `pending()[0]?.tokenName === "leader"`).
#[must_use]
pub fn use_pending_sequence(provider: &KeymapProvider) -> Vec<KeySequencePart> {
    provider
        .use_keymap_selector(|keymap| keymap.engine.get_pending_sequence())
        .read(|keymap| keymap.engine.get_pending_sequence())
}

/// `useActiveKeys()` (react/index.ts:82-90).
#[must_use]
pub fn use_active_keys(provider: &KeymapProvider) -> Vec<crate::engine::ActiveKey> {
    provider
        .use_keymap_selector(|keymap| keymap.engine.active_keys())
        .read(|keymap| keymap.engine.active_keys())
}

/// A signal's subscribe function (`subscribe(onChange) => dispose`).
pub type SignalSubscribe =
    Box<dyn Fn(Box<dyn Fn() + Send + Sync>) -> Box<dyn Fn() + Send + Sync> + Send + Sync>;

/// v2's `KeymapCommand::run` channel (`run(input, event) -> bool`,
/// keymap.tsx:221).
pub type KeymapCommandRun = Arc<dyn Fn(Option<&str>, &KeymapEvent) -> bool + Send + Sync>;

/// `reactiveMatcherFromSignal` (solid/index.ts:156-171): adapt a
/// get + subscribe signal to [`ReactiveMatcher`]. Pass a `predicate`
/// when the value is not already boolean.
pub fn reactive_matcher_from_signal<T, F>(
    get: impl Fn() -> T + Send + Sync + 'static,
    subscribe: impl Fn(Box<dyn Fn() + Send + Sync>) -> Box<dyn Fn() + Send + Sync>
        + Send
        + Sync
        + 'static,
    predicate: Option<F>,
) -> Box<dyn ReactiveMatcher>
where
    T: Send + Sync + 'static,
    F: Fn(&T) -> bool + Send + Sync + 'static,
{
    struct SignalMatcher<T, F>
    where
        T: Send + Sync,
        F: Fn(&T) -> bool + Send + Sync,
    {
        get: Box<dyn Fn() -> T + Send + Sync>,
        subscribe: SignalSubscribe,
        predicate: Option<F>,
        _event: std::marker::PhantomData<KeymapEvent>,
    }

    impl<T, F> ReactiveMatcher for SignalMatcher<T, F>
    where
        T: Send + Sync,
        F: Fn(&T) -> bool + Send + Sync,
    {
        fn get(&self) -> bool {
            let value = (self.get)();
            match &self.predicate {
                Some(predicate) => predicate(&value),
                // `Boolean(accessor())` (solid/index.ts:162).
                None => {
                    let _ = &value;
                    // The signal must be boolean without a predicate;
                    // non-boolean sources require one (typed at the
                    // call site).
                    unreachable!(
                        "reactiveMatcherFromSignal needs a predicate for non-boolean signals"
                    )
                }
            }
        }

        fn subscribe(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
            (self.subscribe)(on_change)
        }
    }

    Box::new(SignalMatcher {
        get: Box::new(get),
        subscribe: Box::new(subscribe),
        predicate,
        _event: std::marker::PhantomData,
    })
}

#[allow(dead_code)]
fn engine_type_witness(_engine: &KeymapEngine) {}
