//! `@opentui/keymap/testing` (`src/testing/index.ts`) — the host
//! harness the upstream test suites drive keymaps through
//! (`createTestKeymap`, `TestKeymapTarget`, `TestKeymapEvent`,
//! `host.press(...)`).

use crate::engine::{KeymapEngine, KeymapHost, ManualClock, TargetId};
use crate::types::{
    HostCapability, HostMetadata, HostModifier, HostPlatform, KeymapEvent, PrimaryModifier,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `DEFAULT_TEST_HOST_METADATA` (testing/index.ts:57-67).
#[must_use]
pub fn default_test_host_metadata() -> HostMetadata {
    HostMetadata {
        platform: HostPlatform::Unknown,
        primary_modifier: PrimaryModifier::Unknown,
        modifiers: [
            (HostModifier::Ctrl, HostCapability::Unknown),
            (HostModifier::Shift, HostCapability::Unknown),
            (HostModifier::Meta, HostCapability::Unknown),
            (HostModifier::Super, HostCapability::Unknown),
            (HostModifier::Hyper, HostCapability::Unknown),
        ]
        .into_iter()
        .collect(),
    }
}

/// `TestKeymapTarget` (testing/index.ts:73-89): a focus-tree node
/// with an id and a parent.
pub struct TestHostState {
    pub focused: Option<TargetId>,
    pub parents: HashMap<TargetId, TargetId>,
}

/// `TestKeymapHost` (testing/index.ts:91+): the in-memory host.
pub struct TestHost {
    state: Mutex<TestHostState>,
    metadata: HostMetadata,
}

impl TestHost {
    #[must_use]
    pub fn new() -> Self {
        TestHost {
            state: Mutex::new(TestHostState {
                focused: None,
                parents: HashMap::new(),
            }),
            metadata: default_test_host_metadata(),
        }
    }

    /// `target.append(child)` (testing/index.ts:82-88).
    pub fn add(&self, target: TargetId, parent: TargetId) {
        self.state
            .lock()
            .expect("test host lock")
            .parents
            .insert(target, parent);
    }

    /// Focus a target (`target.focus()`).
    pub fn focus(&self, target: Option<TargetId>) {
        self.state.lock().expect("test host lock").focused = target;
    }

    /// `host.press(name, modifiers)` — construct a press event.
    #[must_use]
    pub fn event(&self, name: &str) -> KeymapEvent {
        KeymapEvent::new(name)
    }
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl KeymapHost for TestHost {
    fn metadata(&self) -> HostMetadata {
        self.metadata.clone()
    }

    fn root_target(&self) -> TargetId {
        0
    }

    fn is_destroyed(&self) -> bool {
        false
    }

    fn get_focused_target(&self) -> Option<TargetId> {
        self.state.lock().expect("test host lock").focused
    }

    fn get_parent_target(&self, target: TargetId) -> Option<TargetId> {
        self.state
            .lock()
            .expect("test host lock")
            .parents
            .get(&target)
            .copied()
    }

    fn is_target_destroyed(&self, _target: TargetId) -> bool {
        false
    }
}

/// `createTestKeymap({ defaultKeys })` (testing/index.ts:181+): a
/// keymap over the test host with a controllable clock (the upstream
/// harness runs real timers; Rust tests inject [`ManualClock`]).
#[must_use]
pub fn create_test_keymap(default_keys: bool) -> (crate::Keymap, Arc<TestHost>, Arc<ManualClock>) {
    let host = Arc::new(TestHost::new());
    let clock = Arc::new(ManualClock::default());
    let mut keymap =
        crate::Keymap::with_clock(Box::new(TestHostAdapter(host.clone())), clock.clone());
    if default_keys {
        let _ = crate::addons::register_default_keys(&mut keymap);
    }
    (keymap, host, clock)
}

/// Adapter giving `Arc<TestHost>` a `KeymapHost` impl (host sharing
/// for assertions).
struct TestHostAdapter(Arc<TestHost>);

impl KeymapHost for TestHostAdapter {
    fn metadata(&self) -> HostMetadata {
        self.0.metadata()
    }

    fn root_target(&self) -> TargetId {
        self.0.root_target()
    }

    fn is_destroyed(&self) -> bool {
        self.0.is_destroyed()
    }

    fn get_focused_target(&self) -> Option<TargetId> {
        self.0.get_focused_target()
    }

    fn get_parent_target(&self, target: TargetId) -> Option<TargetId> {
        self.0.get_parent_target(target)
    }

    fn is_target_destroyed(&self, target: TargetId) -> bool {
        self.0.is_target_destroyed(target)
    }
}

#[allow(dead_code)]
fn engine_witness(_engine: &KeymapEngine) {}
