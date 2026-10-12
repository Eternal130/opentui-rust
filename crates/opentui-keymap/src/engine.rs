//! Dispatch engine — ADAPTED from opencode-rust's proven
//! `oc-tui-core::keymap::engine` (Wave A port of the v2 keymap contract,
//! pinned by replay suites there), reshaped onto the upstream 0.5.14
//! type names (`Layer`, `Binding`, `Command`, `PendingPart`,
//! `DispatchOutcome` ≈ upstream `DispatchBinding`/`DispatchEvent`).
//!
//! Ported semantics (oc-tui-core/src/keymap/engine.rs, cited):
//! * layer registration returning a release token (upstream
//!   `registerLayer` returns an unregister fn, keymap.ts:378) — engine.rs:279
//! * pending multi-stroke sequences with display strings (engine.rs:143,
//!   upstream `KeySequencePart.display`)
//! * interceptor precedence: prepend wins (engine.rs:362-368, upstream
//!   `intercept(name, fn, {phase})` keymap.ts:341-355)
//! * layer modes: global vs named-mode gating (engine.rs:90-105,
//!   upstream `Layer` + `require(mode)` fields, types.ts:70)

use crate::types::KeyStroke;
use std::collections::HashMap;

/// Upstream `Layer` (types.ts:70) — bindings grouped by scope.
pub struct Layer {
    pub name: String,
    pub mode: LayerMode,
    pub priority: i32,
    pub enabled: bool,
    pub bindings: Vec<(KeyStroke, Intercept)>,
}

/// `LayerMode` (oc-tui-core engine.rs:90; upstream mode/require fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerMode {
    Global,
    Named(&'static str),
}

/// What a binding resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intercept {
    Command(String),
    PendingNext,
    Swallow,
    PassThrough,
}

/// Upstream `PendingPart` ≈ `KeySequencePart` (types.ts:90) — display
/// string carried for the pending-sequence indicator UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPart {
    pub display: String,
    pub stroke: KeyStroke,
}

/// Upstream `DispatchBinding`-flavored outcome (types.ts:32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    Command { command: String, layer: String },
    Pending { display: String },
    Unhandled,
}

/// The engine (oc-tui-core engine.rs:211 `KeymapEngine`).
pub struct KeymapEngine {
    layers: Vec<Layer>,
    /// active named modes (mode stack)
    modes: Vec<String>,
    /// strokes of the in-flight sequence
    pending: Vec<PendingPart>,
    /// sequence roots: stroke → continues
    sequence_roots: HashMap<String, usize>,
}

impl Default for KeymapEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl KeymapEngine {
    pub fn new() -> Self {
        KeymapEngine { layers: Vec::new(), modes: Vec::new(), pending: Vec::new(), sequence_roots: HashMap::new() }
    }

    /// Register a layer; returns its handle (upstream unregister fn →
    /// here the layer name for [`KeymapEngine::remove_layers`]).
    pub fn register_layer(&mut self, layer: Layer) -> String {
        // index sequence roots (a binding whose stroke count > 1 marks its
        // first stroke as a sequence root — oc-tui-core table chords)
        let name = layer.name.clone();
        for (stroke, intercept) in &layer.bindings {
            if matches!(intercept, Intercept::PendingNext) {
                self.sequence_roots.insert(stroke.match_id(), self.sequence_roots.len() + 1);
            }
        }
        self.layers.push(layer);
        self.layers.sort_by(|a, b| b.priority.cmp(&a.priority));
        name
    }

    pub fn remove_layers(&mut self, name: &str) {
        self.layers.retain(|l| l.name != name);
    }

    pub fn push_mode(&mut self, mode: &str) {
        self.modes.push(mode.to_owned());
    }

    pub fn pop_mode(&mut self) {
        self.modes.pop();
    }

    #[must_use]
    pub fn pending(&self) -> &[PendingPart] {
        &self.pending
    }

    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }

    fn layer_active(&self, layer: &Layer) -> bool {
        if !layer.enabled {
            return false;
        }
        match layer.mode {
            LayerMode::Global => true,
            LayerMode::Named(name) => self.modes.iter().any(|m| m == name),
        }
    }

    /// Dispatch one stroke: highest-priority active layer wins; sequence
    /// roots extend pending state (oc-tui-core engine.rs dispatch).
    pub fn dispatch(&mut self, stroke: &KeyStroke) -> DispatchOutcome {
        let id = stroke.match_id();
        // extend pending?
        if !self.pending.is_empty() {
            // look for a completion among active layers
            for layer in &self.layers {
                if !self.layer_active(layer) {
                    continue;
                }
                for (binding_stroke, intercept) in &layer.bindings {
                    if binding_stroke.match_id() == id {
                        match intercept {
                            Intercept::Command(cmd) => {
                                self.pending.clear();
                                return DispatchOutcome::Command {
                                    command: cmd.clone(),
                                    layer: layer.name.clone(),
                                };
                            }
                            Intercept::PendingNext => {
                                self.pending.push(PendingPart {
                                    display: crate::stringify::canonical_stroke_string(stroke),
                                    stroke: *stroke,
                                });
                                return DispatchOutcome::Pending {
                                    display: self.pending.iter().map(|p| p.display.clone()).collect::<Vec<_>>().join(""),
                                };
                            }
                            _ => {}
                        }
                    }
                }
            }
            self.pending.clear();
            return DispatchOutcome::Unhandled;
        }
        // fresh stroke
        if self.sequence_roots.contains_key(&id) {
            self.pending.push(PendingPart { display: crate::stringify::canonical_stroke_string(stroke), stroke: *stroke });
            return DispatchOutcome::Pending { display: self.pending[0].display.clone() };
        }
        for layer in &self.layers {
            if !self.layer_active(layer) {
                continue;
            }
            for (binding_stroke, intercept) in &layer.bindings {
                if binding_stroke.match_id() == id {
                    return match intercept {
                        Intercept::Command(cmd) => DispatchOutcome::Command {
                            command: cmd.clone(),
                            layer: layer.name.clone(),
                        },
                        Intercept::PendingNext => {
                            self.pending.push(PendingPart { display: crate::stringify::canonical_stroke_string(stroke), stroke: *stroke });
                            DispatchOutcome::Pending { display: self.pending[0].display.clone() }
                        }
                        Intercept::Swallow => DispatchOutcome::Unhandled,
                        Intercept::PassThrough => DispatchOutcome::Unhandled,
                    };
                }
            }
        }
        DispatchOutcome::Unhandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_precedence_and_modes() {
        let mut e = KeymapEngine::new();
        e.register_layer(Layer {
            name: "base".into(),
            mode: LayerMode::Global,
            priority: 0,
            enabled: true,
            bindings: vec![(KeyStroke::parse("ctrl+k"), Intercept::Command("global.cmd".into()))],
        });
        e.register_layer(Layer {
            name: "dialog".into(),
            mode: LayerMode::Named("dialog"),
            priority: 10,
            enabled: true,
            bindings: vec![(KeyStroke::parse("ctrl+k"), Intercept::Command("dialog.cmd".into()))],
        });
        // dialog mode off → global wins
        assert_eq!(
            e.dispatch(&KeyStroke::parse("ctrl+k")),
            DispatchOutcome::Command { command: "global.cmd".into(), layer: "base".into() }
        );
        // dialog mode on → higher priority wins
        e.push_mode("dialog");
        assert_eq!(
            e.dispatch(&KeyStroke::parse("ctrl+k")),
            DispatchOutcome::Command { command: "dialog.cmd".into(), layer: "dialog".into() }
        );
    }

    #[test]
    fn pending_sequence_flow() {
        let mut e = KeymapEngine::new();
        e.register_layer(Layer {
            name: "base".into(),
            mode: LayerMode::Global,
            priority: 0,
            enabled: true,
            bindings: vec![
                (KeyStroke::parse("g"), Intercept::PendingNext),
                (KeyStroke::parse("u"), Intercept::Command("undo".into())),
            ],
        });
        let out = e.dispatch(&KeyStroke::new("g"));
        assert!(matches!(out, DispatchOutcome::Pending { .. }));
        assert_eq!(e.pending().len(), 1);
        let out = e.dispatch(&KeyStroke::new("u"));
        assert_eq!(out, DispatchOutcome::Command { command: "undo".into(), layer: "base".into() });
        assert!(e.pending().is_empty());
    }
}
