//! `@opentui/keymap/opentui` (`src/opentui.ts`) — the default host
//! and keymap factories.
//!
//! Upstream binds a `CliRenderer` (key events, focus, renderable
//! tree). opentui-core's Rust renderer is the headless render target
//! so far (no key input / focus yet — that's the input lane), so the
//! port provides:
//!
//! * [`OpenTuiHost`] — a [`KeymapHost`] over host-supplied platform +
//!   kitty flags with an explicit focus tree (the pieces of
//!   `createOpenTuiKeymapHost`, opentui.ts:53-115, that the keymap
//!   needs; the renderer event wiring lands with the input lane);
//! * [`create_open_tui_keymap`] — `new Keymap(host)` (opentui.ts:
//!   118-123), refusing a destroyed host;
//! * [`create_default_opentui_keymap`] — + `registerDefaultKeys` +
//!   `registerEnabledFields` + `registerMetadataFields`
//!   (opentui.ts:126-135).

use crate::addons::{
    create_open_tui_host_metadata, register_default_keys, register_enabled_fields,
    register_metadata_fields,
};
use crate::engine::{KeymapHost, TargetId};
use crate::types::HostPlatform;
use crate::Keymap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// `createOpenTuiKeymapHost` (opentui.ts:53-115) — the OpenTUI-side
/// host: platform metadata (`createOpenTuiHostMetadata`, lines
/// 33-52), a focus tree with parent links (`getParentTarget`), and
/// destroy tracking. Key events arrive via
/// [`Keymap::handle_key_event`] (the renderer's `keyInput` wiring is
/// the input lane's contract).
pub struct OpenTuiHost {
    platform: HostPlatform,
    has_kitty_keyboard: bool,
    destroyed: AtomicBool,
    focus: Mutex<HostFocusState>,
}

struct HostFocusState {
    focused: Option<TargetId>,
    /// node → parent (the renderable tree's parent links).
    parents: HashMap<TargetId, TargetId>,
}

impl OpenTuiHost {
    #[must_use]
    pub fn new(platform: HostPlatform, has_kitty_keyboard: bool) -> Self {
        OpenTuiHost {
            platform,
            has_kitty_keyboard,
            destroyed: AtomicBool::new(false),
            focus: Mutex::new(HostFocusState {
                focused: None,
                parents: HashMap::new(),
            }),
        }
    }

    /// `renderer.root` (the tree's root target id, conventionally 0).
    pub fn root(&self) -> TargetId {
        0
    }

    /// Attach a target under a parent (`renderer.root.add(target)`).
    pub fn add(&self, target: TargetId, parent: TargetId) {
        let mut state = self.focus.lock().expect("host focus lock");
        state.parents.insert(target, parent);
    }

    /// `renderer.currentFocusedRenderable` (opentui.ts:60-71): the
    /// focused target, `None` while unfocused.
    pub fn set_focused(&self, target: Option<TargetId>) {
        let mut state = self.focus.lock().expect("host focus lock");
        state.focused = target;
    }

    /// `renderer.destroy()` (opentui.ts:31-33).
    pub fn destroy(&self) {
        self.destroyed.store(true, Ordering::SeqCst);
    }
}

impl KeymapHost for OpenTuiHost {
    fn metadata(&self) -> crate::types::HostMetadata {
        create_open_tui_host_metadata(self.platform, self.has_kitty_keyboard)
    }

    fn root_target(&self) -> TargetId {
        self.root()
    }

    fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    fn get_focused_target(&self) -> Option<TargetId> {
        self.focus.lock().expect("host focus lock").focused
    }

    fn get_parent_target(&self, target: TargetId) -> Option<TargetId> {
        let state = self.focus.lock().expect("host focus lock");
        state.parents.get(&target).copied()
    }

    fn is_target_destroyed(&self, _target: TargetId) -> bool {
        // Target lifetime rides the renderer's renderable destroy
        // events (opentui.ts:68-70); the headless host keeps targets
        // alive for the session.
        false
    }
}

/// `createOpenTuiKeymap` (opentui.ts:118-123): refuses a destroyed
/// host (upstream throws; the port returns `None` — documented
/// divergence "throws → Result/Option").
pub fn create_open_tui_keymap(host: Box<dyn KeymapHost>) -> Option<Keymap> {
    if host.is_destroyed() {
        return None;
    }
    Some(Keymap::new(host))
}

/// `createDefaultOpenTuiKeymap` (opentui.ts:126-135): the OpenTUI
/// keymap with the default parser/matcher, `enabled` fields, and
/// metadata fields registered.
pub fn create_default_opentui_keymap(host: Box<dyn KeymapHost>) -> Option<Keymap> {
    let mut keymap = create_open_tui_keymap(host)?;
    let default_keys = register_default_keys(&mut keymap);
    let enabled = register_enabled_fields(&mut keymap);
    let metadata = register_metadata_fields(&mut keymap);
    let _ = (default_keys, enabled, metadata);
    Some(keymap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HostCapability;

    #[test]
    fn host_metadata_matches_upstream_matrix() {
        // opentui.ts:33-52 — super/hyper only "supported" under kitty.
        let host = OpenTuiHost::new(HostPlatform::Linux, false);
        let metadata = KeymapHost::metadata(&host);
        assert_eq!(
            metadata.modifiers[&crate::types::HostModifier::Ctrl],
            HostCapability::Supported
        );
        assert_eq!(
            metadata.modifiers[&crate::types::HostModifier::Super],
            HostCapability::Unknown
        );
        let kitty = OpenTuiHost::new(HostPlatform::Macos, true);
        let metadata = KeymapHost::metadata(&kitty);
        assert_eq!(
            metadata.modifiers[&crate::types::HostModifier::Super],
            HostCapability::Supported
        );
        assert_eq!(
            metadata.primary_modifier,
            crate::types::PrimaryModifier::Super
        );
    }

    #[test]
    fn factories_refuse_destroyed_hosts() {
        let host = OpenTuiHost::new(HostPlatform::Linux, false);
        host.destroy();
        let boxed: Box<dyn KeymapHost> = Box::new(host);
        assert!(create_open_tui_keymap(boxed).is_none());
    }
}
