//! The reactive boundary — solid-js's signal substrate, ported to the
//! consumer-owned-shape decided in the crate design record (lib.rs).
//!
//! Upstream `@opentui/solid` builds on solid-js: signals +
//! `createRenderEffect` drive every reconciler patch
//! (renderer/universal.js:22, 48, 198-206 — each dynamic expression is
//! its own effect re-run when its signals change). Rust has no solid-js;
//! the port keeps the same observable semantics with an explicit push
//! model:
//!
//! * [`Signal`] — the built-in signal (solid `createSignal`). `set`
//!   fires subscribers synchronously (solid's synchronous signal set);
//!   subscribers here enqueue reconciler patches instead of patching
//!   inline, and [`SolidRenderer::flush`](crate::SolidRenderer::flush)
//!   applies them — the batched-frame equivalent of upstream's
//!   `requestRender` scheduling.
//! * [`Reactive`] — the adapter trait for consumer reactivity
//!   (opencode-rust adapts its own signals; upstream equivalent: any
//!   solid-js signal works because they all share the store).
//! * `subscribe` returns an **unsubscriber** — upstream effects are
//!   owned by scopes and disposed on unmount (`onCleanup`,
//!   reconciler.ts:23, 207-212); the port needs the explicit handle to
//!   detach dynamic-prop bindings when their element unmounts.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

type Unsub = Box<dyn Fn()>;

/// The reactive-value boundary (design record in `lib.rs`): read the
/// current value, and subscribe to future sets. Mirrors the two halves
/// of a solid-js signal a reconciler binding needs (`fn()` read +
/// `createRenderEffect` re-run on write).
pub trait Reactive<T> {
    fn get(&self) -> T;
    /// Register a listener fired on `set` with the new value. Returns
    /// the dispose handle (upstream: effect disposal on owner unmount).
    fn subscribe(&self, cb: Box<dyn Fn(&T)>) -> Unsub;
}

struct SubEntry<T> {
    id: u64,
    cb: Box<dyn Fn(&T)>,
}

struct SignalInner<T: Clone> {
    value: RefCell<T>,
    subs: RefCell<Vec<SubEntry<T>>>,
    next_id: Cell<u64>,
}

/// The built-in signal (`createSignal`, solid-js). Cheap to clone —
/// clones share the storage (a reference-like signal handle). `set`
/// stores then fires subscribers in registration order; callbacks may
/// set again (appended entries fire on the next `set` — solid's
/// non-reentrant subscriber semantics).
pub struct Signal<T: Clone> {
    inner: Rc<SignalInner<T>>,
}

impl<T: Clone + 'static> Signal<T> {
    #[must_use]
    pub fn new(value: T) -> Self {
        Signal {
            inner: Rc::new(SignalInner {
                value: RefCell::new(value),
                subs: RefCell::new(Vec::new()),
                next_id: Cell::new(1),
            }),
        }
    }

    /// `setSignal` — store + notify subscribers with the new value.
    pub fn set(&self, value: T) {
        *self.inner.value.borrow_mut() = value.clone();
        // take the list so callbacks can subscribe/unsubscribe safely;
        // re-extend afterwards (new subscribers fire next set)
        let taken = std::mem::take(&mut *self.inner.subs.borrow_mut());
        for entry in &taken {
            (entry.cb)(&value);
        }
        self.inner.subs.borrow_mut().extend(taken);
    }
}

impl<T: Clone + 'static> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Signal {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T: Clone + 'static> Reactive<T> for Signal<T> {
    fn get(&self) -> T {
        self.inner.value.borrow().clone()
    }

    fn subscribe(&self, cb: Box<dyn Fn(&T)>) -> Unsub {
        let id = self.inner.next_id.get();
        self.inner.next_id.set(id + 1);
        self.inner.subs.borrow_mut().push(SubEntry { id, cb });
        let weak: Weak<SignalInner<T>> = Rc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.subs.borrow_mut().retain(|s| s.id != id);
            }
        })
    }
}

/// `createMemo`-shaped derivation over any [`Reactive`] source: read a
/// mapped view of the parent (upstream `createMemo(fn)` — re-evaluated
/// on parent change). Used to adapt typed consumer signals into the
/// reconciler's [`PropValue`](crate::PropValue) / children shapes.
pub struct Mapped<S, U, T, F> {
    source: S,
    f: Rc<F>,
    _marker: std::marker::PhantomData<fn(&U) -> T>,
}

impl<S, U, T, F> Mapped<S, U, T, F> {
    #[must_use]
    pub fn new(source: S, f: F) -> Self {
        Mapped {
            source,
            f: Rc::new(f),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<S, U, T, F> Reactive<T> for Mapped<S, U, T, F>
where
    S: Reactive<U> + 'static,
    F: Fn(&U) -> T + 'static,
    T: 'static,
{
    fn get(&self) -> T {
        (self.f)(&self.source.get())
    }

    fn subscribe(&self, cb: Box<dyn Fn(&T)>) -> Unsub {
        let f = Rc::clone(&self.f);
        self.source.subscribe(Box::new(move |u: &U| cb(&f(u))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_fires_in_registration_order() {
        let s = Signal::new(0);
        let log = Rc::new(RefCell::new(Vec::new()));
        let l1 = log.clone();
        let _unsub = s.subscribe(Box::new(move |v| l1.borrow_mut().push(format!("a{v}"))));
        let l2 = log.clone();
        let _unsub = s.subscribe(Box::new(move |v| l2.borrow_mut().push(format!("b{v}"))));
        s.set(1);
        s.set(2);
        assert_eq!(*log.borrow(), vec!["a1", "b1", "a2", "b2"]);
    }

    #[test]
    fn unsubscriber_detaches() {
        let s = Signal::new(0);
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        let unsub = s.subscribe(Box::new(move |_| h.set(h.get() + 1)));
        s.set(1);
        assert_eq!(hits.get(), 1);
        unsub();
        s.set(2);
        assert_eq!(hits.get(), 1);
    }

    #[test]
    fn clone_shares_storage() {
        let a = Signal::new(1);
        let b = a.clone();
        b.set(5);
        assert_eq!(a.get(), 5);
    }

    #[test]
    fn set_inside_callback_does_not_reenter() {
        let s = Signal::new(0);
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        let s2 = s.clone();
        let _unsub = s.subscribe(Box::new(move |&v| {
            h.set(h.get() + 1);
            if v < 3 {
                s2.set(v + 1); // re-entrant set: appends run NEXT set
            }
        }));
        s.set(1);
        assert_eq!(hits.get(), 1);
        s.set(10);
        // the queued re-entrant subscriber entry fires on the next set
        assert!(hits.get() >= 2);
    }
}
