//! # opentui-keymap — API-shape port of `@opentui/keymap` 0.5.14
//!
//! Spec source: `third_party/opentui/packages/keymap` at `v0.5.14`.
//! Upstream exports (index.ts:1-98): the `Keymap` class,
//! `KEYMAP_EXTENSION_CONTEXT`, `stringifyKeySequence`,
//! `stringifyKeyStroke` + a large type surface; subpaths add
//! `/opentui` (default host), `/solid` (provider/hooks), and
//! `addons/opentui`.
//!
//! Engine note: the dispatch ENGINE semantics (layers, focus scopes,
//! pending sequences, interceptor precedence) were already ported and
//! battle-tested in opencode-rust's `oc-tui-core::keymap` (Wave A,
//! pinned by replay suites). This crate adapts that proven logic —
//! cited per item — onto the upstream 0.5.14 API SHAPE. It is
//! standalone (no dependency on opencode-rust crates); future lanes
//! grow it toward the full upstream type surface (see PORT-MATRIX.md).
//!
//! Status: skeleton — [`stringify`] is fully ported and pinned;
//! [`engine`] carries the adapted core; the [`Keymap`] facade is minimal.

pub mod engine;
pub mod stringify;
pub mod types;

pub use engine::{DispatchOutcome, Intercept, KeymapEngine, Layer, PendingPart};
pub use stringify::{stringify_key_sequence, stringify_key_stroke};
pub use types::{Binding, Command, KeyStroke, KeyStrokeInput};

/// Upstream version this crate pins (`packages/keymap/package.json`).
pub const SPEC_VERSION: &str = "0.5.14";

/// `Keymap` — the public facade (upstream keymap.ts:73-552 delegates to
/// host-provided services; the Rust shape owns an engine directly).
pub struct Keymap {
    engine: KeymapEngine,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    #[must_use]
    pub fn new() -> Self {
        Keymap { engine: KeymapEngine::new() }
    }

    /// `keymap.registerLayer(layer)` (keymap.ts:378).
    pub fn register_layer(&mut self, layer: Layer) -> String {
        self.engine.register_layer(layer)
    }

    /// `keymap.hasPendingSequence()` (keymap.ts:240).
    #[must_use]
    pub fn has_pending_sequence(&self) -> bool {
        !self.engine.pending().is_empty()
    }

    /// `keymap.getPendingSequence()` (keymap.ts:100).
    #[must_use]
    pub fn pending_sequence(&self) -> &[PendingPart] {
        self.engine.pending()
    }

    /// `keymap.clearPendingSequence()` (keymap.ts:264).
    pub fn clear_pending_sequence(&mut self) {
        self.engine.clear_pending();
    }

    /// `keymap.parseKeySequence(key)` (keymap.ts:256) — parse a
    /// `ctrl+x`-style token into stroke parts.
    #[must_use]
    pub fn parse_key_sequence(&self, key: &str) -> Vec<KeyStroke> {
        key.split(',')
            .map(|token| KeyStroke::parse(token.trim()))
            .collect()
    }

    /// `keymap.formatKey(key, options)` (keymap.ts:260).
    #[must_use]
    pub fn format_key(&self, key: &str) -> String {
        self.parse_key_sequence(key)
            .iter()
            .map(stringify::canonical_stroke_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Dispatch one stroke — upstream `keymap` feeds `key.after` /
    /// host dispatch (types.ts `BindingEvent` pipeline).
    pub fn dispatch(&mut self, stroke: &KeyStroke) -> DispatchOutcome {
        self.engine.dispatch(stroke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_round_trip() {
        let mut km = Keymap::new();
        km.register_layer(Layer {
            name: "base".into(),
            mode: engine::LayerMode::Global,
            priority: 0,
            enabled: true,
            bindings: vec![(
                KeyStroke::parse("ctrl+c"),
                engine::Intercept::Command("app.exit".into()),
            )],
        });
        let out = km.dispatch(&KeyStroke::parse("ctrl+c"));
        assert!(matches!(out, DispatchOutcome::Command { .. }));
        assert_eq!(km.format_key("ctrl+c,ctrl+d"), "ctrl+c,ctrl+d");
    }
}
