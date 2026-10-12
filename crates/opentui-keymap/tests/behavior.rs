//! Behavioral test suite — ports of the upstream keymap package's own
//! tests (`packages/keymap/src/addons/**/tests`, `extras/tests`, and
//! the dispatch semantics from `src/tests/keymap.*`), plus the
//! oc-tui-core characterization cases they re-prove (cited per test).
//!
//! Upstream suites drive a live renderer (`createTestRenderer` +
//! `mockInput`); the port drives the same keymap through the testing
//! host (`testing::create_test_keymap` + `handle_key_event`).

use opentui_keymap::addons::*;
use opentui_keymap::engine::{InterceptDecision, KeyAfterReason};
use opentui_keymap::extras::*;
use opentui_keymap::testing::create_test_keymap;
use opentui_keymap::types::*;
use opentui_keymap::{format_command_bindings, format_key_sequence, Keymap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A shared call log — upstream tests capture `calls: string[]`.
type Calls = Arc<Mutex<Vec<String>>>;

fn press(keymap: &mut Keymap, name: &str) -> opentui_keymap::DispatchOutcome {
    keymap.handle_key_event(&mut KeymapEvent::new(name))
}

fn press_mod(
    keymap: &mut Keymap,
    name: &str,
    ctrl: bool,
    shift: bool,
    meta: bool,
    super_key: bool,
) -> opentui_keymap::DispatchOutcome {
    let mut event = KeymapEvent::new(name).with_modifiers(ctrl, shift, meta, super_key, false);
    keymap.handle_key_event(&mut event)
}

fn binding(key: &str, cmd: &str) -> Binding {
    Binding::new(key, Some(BindingCommand::Name(cmd.to_owned())))
}

/// An always-accepting command layer for the given names (the
/// oc-tui-core suite's "always-true runner" equivalent).
fn commands_layer(names: &[&str]) -> Layer {
    Layer {
        commands: names
            .iter()
            .map(|name| Command::new(name, Box::new(|_| CommandResult::Handled)))
            .collect(),
        ..Layer::default()
    }
}

fn command_layer_logging(commands: &[(&str, Calls)]) -> Layer {
    Layer {
        commands: commands
            .iter()
            .map(|(name, calls)| {
                let calls = calls.clone();
                let name = (*name).to_owned();
                let command_name = name.clone();
                Command::new(
                    &command_name,
                    Box::new(move |_ctx| {
                        calls.lock().unwrap().push(name.clone());
                        CommandResult::Handled
                    }),
                )
            })
            .collect(),
        ..Layer::default()
    }
}

// ---------------------------------------------------------------------------
// escape-clears-pending-sequence.test.ts
// ---------------------------------------------------------------------------

/// escape test 1: "clears pending sequence on escape and only
/// intercepts escape while pending".
#[test]
fn escape_clears_pending_and_only_intercepts_while_pending() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(Layer {
        commands: vec![
            Command::new(
                "delete-line",
                Box::new({
                    let calls = calls.clone();
                    move |_| {
                        calls.lock().unwrap().push("delete".into());
                        CommandResult::Handled
                    }
                }),
            ),
            Command::new(
                "escape-command",
                Box::new({
                    let calls = calls.clone();
                    move |_| {
                        calls.lock().unwrap().push("escape".into());
                        CommandResult::Handled
                    }
                }),
            ),
        ],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![
            binding("dd", "delete-line"),
            binding("escape", "escape-command"),
        ],
        ..Layer::default()
    });
    register_escape_clears_pending_sequence(&mut keymap, None);

    press(&mut keymap, "d");
    assert!(keymap.has_pending_sequence());
    let pending = keymap.get_pending_sequence();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].stroke.name, "d");
    assert_eq!(pending[0].display, "d");

    let outcome = press(&mut keymap, "escape");
    assert!(outcome.consumed);
    assert!(!keymap.has_pending_sequence());
    assert!(keymap.get_pending_sequence().is_empty());
    assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());

    let _outcome = press(&mut keymap, "escape");
    // This test's handlers log literal tags (upstream bodies push
    // "escape").
    assert_eq!(*calls.lock().unwrap(), vec!["escape".to_owned()]);
}

/// escape test 2: "can clear pending sequence without consuming
/// escape" — `preventDefault: false` lets the escape binding run.
#[test]
fn escape_can_clear_without_consuming() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(command_layer_logging(&[
        ("delete-line", calls.clone()),
        ("escape-command", calls.clone()),
    ]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("dd", "delete-line"),
            binding("escape", "escape-command"),
        ],
        ..Layer::default()
    });
    register_escape_clears_pending_sequence(
        &mut keymap,
        Some(EscapeClearsPendingSequenceOptions {
            prevent_default: Some(false),
            priority: None,
        }),
    );

    press(&mut keymap, "d");
    let outcome = press(&mut keymap, "escape");
    assert!(!keymap.has_pending_sequence());
    // The cleared escape still dispatches to bindings.
    assert_eq!(outcome.command.as_deref(), Some("escape-command"));
    assert_eq!(*calls.lock().unwrap(), vec!["escape-command".to_owned()]);
}

/// escape test 3: "can be disposed to stop pending escape forwarding".
#[test]
fn escape_dispose_stops_behavior() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(command_layer_logging(&[
        ("delete-line", calls.clone()),
        ("escape-command", calls.clone()),
    ]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("dd", "delete-line"),
            binding("escape", "escape-command"),
        ],
        ..Layer::default()
    });
    let off = register_escape_clears_pending_sequence(
        &mut keymap,
        Some(EscapeClearsPendingSequenceOptions {
            prevent_default: Some(false),
            priority: None,
        }),
    );

    press(&mut keymap, "d");
    press(&mut keymap, "escape");
    assert_eq!(*calls.lock().unwrap(), vec!["escape-command".to_owned()]);

    press(&mut keymap, "d");
    keymap.release(off);
    press(&mut keymap, "escape");
    // Sequence cleared by the sequence-miss path (no addon); the miss
    // swallows the escape into the focused target — the binding does
    // NOT run (upstream asserts calls stay at one entry).
    assert!(!keymap.has_pending_sequence());
    assert_eq!(*calls.lock().unwrap(), vec!["escape-command".to_owned()]);
}

// ---------------------------------------------------------------------------
// backspace-pops-pending-sequence.test.ts
// ---------------------------------------------------------------------------

/// backspace test 1: pops one part at a time; only intercepts while
/// pending.
#[test]
fn backspace_pops_pending_parts() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(command_layer_logging(&[
        ("delete-ca", calls.clone()),
        ("backspace-command", calls.clone()),
    ]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("dca", "delete-ca"),
            binding("backspace", "backspace-command"),
        ],
        ..Layer::default()
    });
    register_backspace_pops_pending_sequence(&mut keymap, None);

    press(&mut keymap, "d");
    press(&mut keymap, "c");
    let pending = keymap.get_pending_sequence();
    assert_eq!(
        pending
            .iter()
            .map(|p| p.stroke.name.as_str())
            .collect::<Vec<_>>(),
        vec!["d", "c"]
    );

    press(&mut keymap, "backspace");
    let pending = keymap.get_pending_sequence();
    assert_eq!(
        pending
            .iter()
            .map(|p| p.stroke.name.as_str())
            .collect::<Vec<_>>(),
        vec!["d"]
    );
    assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());

    press(&mut keymap, "backspace");
    assert!(!keymap.has_pending_sequence());
    assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());

    press(&mut keymap, "backspace");
    // The log records the COMMAND name (upstream bodies push
    // "backspace"; the port logs the dispatched command id).
    assert_eq!(*calls.lock().unwrap(), vec!["backspace-command".to_owned()]);
}

/// backspace test 2: `preventDefault: false` pops without consuming.
#[test]
fn backspace_can_pop_without_consuming() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(command_layer_logging(&[
        ("delete-ca", calls.clone()),
        ("backspace-command", calls.clone()),
    ]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("dca", "delete-ca"),
            binding("backspace", "backspace-command"),
        ],
        ..Layer::default()
    });
    register_backspace_pops_pending_sequence(
        &mut keymap,
        Some(BackspacePopsPendingSequenceOptions {
            prevent_default: Some(false),
            priority: None,
        }),
    );

    press(&mut keymap, "d");
    press(&mut keymap, "c");
    let outcome = press(&mut keymap, "backspace");
    // Upstream asserts only that the sequence popped without
    // consuming; the popped backspace still walks dispatch (the
    // remaining [d] capture misses on "backspace").
    assert!(!outcome.consumed);
    assert!(!keymap.has_pending_sequence());
}

/// backspace test 3: dispose stops popping.
#[test]
fn backspace_dispose_stops_popping() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(command_layer_logging(&[(
        "backspace-command",
        calls.clone(),
    )]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("dd", "noop"),
            binding("backspace", "backspace-command"),
        ],
        ..Layer::default()
    });
    let off = register_backspace_pops_pending_sequence(
        &mut keymap,
        Some(BackspacePopsPendingSequenceOptions {
            prevent_default: Some(false),
            priority: None,
        }),
    );

    press(&mut keymap, "d");
    keymap.release(off);
    press(&mut keymap, "backspace");
    assert!(!keymap.has_pending_sequence());
}

// ---------------------------------------------------------------------------
// comma-bindings.test.ts
// ---------------------------------------------------------------------------

#[test]
fn comma_splits_into_multiple_bindings() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    register_comma_bindings(&mut keymap);
    keymap.register_layer(command_layer_logging(&[("command", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("x, y", "command")],
        ..Layer::default()
    });

    press(&mut keymap, "x");
    press(&mut keymap, "y");
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["command".to_owned(), "command".to_owned()]
    );
}

#[test]
fn comma_preserves_display_metadata_from_earlier_expanders() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    // An earlier-registered alias expander, then comma (upstream test
    // "preserves display metadata from earlier expanders").
    keymap.append_binding_expander(std::sync::Arc::new(|input, _displays| {
        (input == "alias").then(|| {
            vec![opentui_keymap::parser::BindingExpansion {
                key: "ctrl+x, ctrl+y".into(),
                displays: Some(vec!["alias-x, alias-y".into()]),
            }]
        })
    }));
    register_comma_bindings(&mut keymap);
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "command",
            Box::new({
                let calls = calls.clone();
                move |_| {
                    calls.lock().unwrap().push("command".into());
                    CommandResult::Handled
                }
            }),
        )],
        bindings: vec![binding("alias", "command")],
        ..Layer::default()
    });

    let displays: Vec<String> = keymap
        .active_keys()
        .into_iter()
        .map(|k| k.display)
        .collect();
    assert_eq!(displays, vec!["alias-x".to_owned(), "alias-y".to_owned()]);

    press_mod(&mut keymap, "x", true, false, false, false);
    press_mod(&mut keymap, "y", true, false, false, false);
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["command".to_owned(), "command".to_owned()]
    );
}

#[test]
fn comma_empty_entries_error_and_skip() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    register_comma_bindings(&mut keymap);
    keymap.register_layer(Layer {
        bindings: vec![binding("x,,y", "noop")],
        ..Layer::default()
    });
    assert_eq!(
        keymap.take_errors(),
        vec![
            "Invalid key sequence \"x,,y\": comma-separated bindings cannot contain empty entries"
                .to_owned()
        ]
    );
    assert!(keymap.active_keys().is_empty());
}

#[test]
fn comma_dispose_restores_sequence_semantics() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let off = register_comma_bindings(&mut keymap);
    keymap.release(off);
    keymap.register_layer(command_layer_logging(&[("sequence", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("x,y", "sequence")],
        ..Layer::default()
    });

    press(&mut keymap, "x");
    assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());
    press(&mut keymap, ",");
    assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());
    let outcome = press(&mut keymap, "y");
    assert_eq!(outcome.command.as_deref(), Some("sequence"));
}

// ---------------------------------------------------------------------------
// timed-leader.test.ts
// -------------------------------------------------------------------

#[test]
fn timed_leader_supports_extensions() {
    let (mut keymap, _host, clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    register_timed_leader(
        &mut keymap,
        TimedLeaderOptions::new(KeyStrokeInput {
            name: "x".into(),
            ctrl: Some(true),
            ..KeyStrokeInput::default()
        }),
    );
    keymap.register_layer(command_layer_logging(&[("leader-action", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("<leader>a", "leader-action")],
        ..Layer::default()
    });

    press_mod(&mut keymap, "x", true, false, false, false);
    clock.advance(100); // inside the 1500 ms default
    let outcome = press(&mut keymap, "a");
    assert_eq!(outcome.command.as_deref(), Some("leader-action"));
    assert_eq!(*calls.lock().unwrap(), vec!["leader-action".to_owned()]);
}

#[test]
fn timed_leader_accepts_binding_lookup_trigger() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    register_timed_leader(
        &mut keymap,
        TimedLeaderOptions {
            trigger: LeaderTrigger::Bindings(vec![KeyStrokeInput {
                name: "x".into(),
                ctrl: Some(true),
                ..KeyStrokeInput::default()
            }]),
            name: None,
            timeout_ms: None,
            on_arm: None,
            on_disarm: None,
        },
    );
    keymap.register_layer(command_layer_logging(&[("leader-action", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("<leader>a", "leader-action")],
        ..Layer::default()
    });

    press_mod(&mut keymap, "x", true, false, false, false);
    let outcome = press(&mut keymap, "a");
    assert_eq!(outcome.command.as_deref(), Some("leader-action"));
}

#[test]
fn timed_leader_disarms_after_timeout() {
    let (mut keymap, _host, clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let states: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    register_timed_leader(
        &mut keymap,
        TimedLeaderOptions {
            trigger: LeaderTrigger::Stroke(KeyStrokeInput {
                name: "x".into(),
                ctrl: Some(true),
                ..KeyStrokeInput::default()
            }),
            name: None,
            timeout_ms: Some(5),
            on_arm: Some({
                let states = states.clone();
                Arc::new(move || states.lock().unwrap().push("armed"))
            }),
            on_disarm: Some({
                let states = states.clone();
                Arc::new(move || states.lock().unwrap().push("disarmed"))
            }),
        },
    );
    keymap.register_layer(command_layer_logging(&[("leader-action", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("<leader>a", "leader-action")],
        ..Layer::default()
    });

    press_mod(&mut keymap, "x", true, false, false, false);
    clock.advance(20);
    let outcome = press(&mut keymap, "a");
    assert_eq!(outcome.command, None);
    assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());
    assert_eq!(*states.lock().unwrap(), vec!["armed", "disarmed"]);
}

#[test]
fn timed_leader_dispose_while_armed_unregisters_token() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let states: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let off = register_timed_leader(
        &mut keymap,
        TimedLeaderOptions {
            trigger: LeaderTrigger::Stroke(KeyStrokeInput {
                name: "x".into(),
                ctrl: Some(true),
                ..KeyStrokeInput::default()
            }),
            name: None,
            timeout_ms: Some(5),
            on_arm: Some({
                let states = states.clone();
                Arc::new(move || states.lock().unwrap().push("armed"))
            }),
            on_disarm: Some({
                let states = states.clone();
                Arc::new(move || states.lock().unwrap().push("disarmed"))
            }),
        },
    );
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "leader-action",
            Box::new(|_| CommandResult::Handled),
        )],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![binding("<leader>a", "leader-action")],
        ..Layer::default()
    });

    press_mod(&mut keymap, "x", true, false, false, false);
    keymap.release(off);

    assert_eq!(*states.lock().unwrap(), vec!["armed", "disarmed"]);
    // The token is gone: NEW <leader> bindings warn and skip (the
    // upstream recompilation warning).
    keymap.register_layer(Layer {
        bindings: vec![binding("<leader>z", "leader-action")],
        ..Layer::default()
    });
    let warnings = keymap.take_warnings();
    assert_eq!(
        warnings,
        vec![
            "Unknown token \"leader\" in key sequence \"<leader>z\"; binding was skipped until the token is registered".to_owned()
        ]
    );
}

#[test]
fn timed_leader_disarms_on_reactive_invalidation() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let states: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let enabled = Arc::new(AtomicBool::new(true));
    register_timed_leader(
        &mut keymap,
        TimedLeaderOptions {
            trigger: LeaderTrigger::Stroke(KeyStrokeInput {
                name: "x".into(),
                ctrl: Some(true),
                ..KeyStrokeInput::default()
            }),
            name: None,
            timeout_ms: Some(1000),
            on_arm: Some({
                let states = states.clone();
                Arc::new(move || states.lock().unwrap().push("armed"))
            }),
            on_disarm: Some({
                let states = states.clone();
                Arc::new(move || states.lock().unwrap().push("disarmed"))
            }),
        },
    );
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "leader-action",
            Box::new(|_| CommandResult::Handled),
        )],
        ..Layer::default()
    });
    {
        let enabled = enabled.clone();
        keymap.register_layer(Layer {
            enabled: Some(Enabled::Fn(Arc::new(move || {
                enabled.load(Ordering::SeqCst)
            }))),
            bindings: vec![binding("<leader>a", "leader-action")],
            ..Layer::default()
        });
    }

    press_mod(&mut keymap, "x", true, false, false, false);
    assert_eq!(*states.lock().unwrap(), vec!["armed"]);

    enabled.store(false, Ordering::SeqCst);
    // Any keymap interaction revalidates the pending sequence
    // (ensureValidPendingSequence — the reactive-invalidation path).
    let _ = keymap.has_pending_sequence();
    assert_eq!(*states.lock().unwrap(), vec!["armed", "disarmed"]);
}

// ---------------------------------------------------------------------------
// base-layout.test.ts
// ---------------------------------------------------------------------------

fn kitty_event(name: &str, base_code: u32, ctrl: bool) -> KeymapEvent {
    KeymapEvent::new(name)
        .with_modifiers(ctrl, false, false, false, false)
        .with_base_code(Some(base_code))
}

#[test]
fn base_layout_matches_through_kitty_codepoints() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    register_base_layout_fallback(&mut keymap);
    keymap.register_layer(command_layer_logging(&[("copy", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("ctrl+c", "copy")],
        ..Layer::default()
    });

    // current-layout name ᅊ (U+314A), base layout 'c' (99).
    let mut event = kitty_event("\u{314a}", 99, true);
    let outcome = keymap.handle_key_event(&mut event);
    assert_eq!(outcome.command.as_deref(), Some("copy"));
}

#[test]
fn base_layout_direct_matches_stay_ahead_of_fallbacks() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    register_base_layout_fallback(&mut keymap);
    keymap.register_layer(command_layer_logging(&[
        ("fallback-copy", calls.clone()),
        ("direct-copy", calls.clone()),
    ]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("ctrl+c", "fallback-copy"),
            Binding::new(
                KeyLike::Stroke(KeyStrokeInput {
                    name: "\u{314a}".into(),
                    ctrl: Some(true),
                    ..KeyStrokeInput::default()
                }),
                Some(BindingCommand::Name("direct-copy".into())),
            ),
        ],
        ..Layer::default()
    });

    let mut event = kitty_event("\u{314a}", 99, true);
    let outcome = keymap.handle_key_event(&mut event);
    assert_eq!(outcome.command.as_deref(), Some("direct-copy"));
}

#[test]
fn base_layout_lower_layer_direct_beats_higher_layer_fallback() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    register_base_layout_fallback(&mut keymap);
    // First-registered = lower precedence at equal priority.
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "delete-to-end",
            Box::new({
                let calls = calls.clone();
                move |_| {
                    calls.lock().unwrap().push("delete-to-end".into());
                    CommandResult::Handled
                }
            }),
        )],
        bindings: vec![binding("ctrl+k", "delete-to-end")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "paste",
            Box::new({
                let calls = calls.clone();
                move |_| {
                    calls.lock().unwrap().push("paste".into());
                    CommandResult::Handled
                }
            }),
        )],
        bindings: vec![binding("ctrl+v", "paste")],
        ..Layer::default()
    });

    // Direct name 'k' (107), base layout 'v' (118) — kitty
    // `[107::118;5u`: current layout reports 'k', base is 'v'.
    let mut event = kitty_event("k", 118, true);
    let outcome = keymap.handle_key_event(&mut event);
    assert_eq!(outcome.command.as_deref(), Some("delete-to-end"));
}

#[test]
fn base_layout_dispose_stops_fallback() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let off = register_base_layout_fallback(&mut keymap);
    keymap.register_layer(command_layer_logging(&[("copy", calls.clone())]));
    keymap.register_layer(Layer {
        bindings: vec![binding("ctrl+c", "copy")],
        ..Layer::default()
    });
    keymap.release(off);

    let mut event = kitty_event("\u{314a}", 99, true);
    let outcome = keymap.handle_key_event(&mut event);
    assert_eq!(outcome.command, None);
}

// ---------------------------------------------------------------------------
// edit-buffer-bindings.test.ts (engine-side behaviors)
// ---------------------------------------------------------------------------

#[test]
fn edit_buffer_sequence_bindings_and_passthrough() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let (_commands, sink) =
        opentui_keymap::textarea::register_edit_buffer_commands(&mut keymap, None);
    keymap.register_layer(Layer {
        bindings: vec![binding("dd", "input.delete.line")],
        ..Layer::default()
    });

    press(&mut keymap, "d");
    let outcome = press(&mut keymap, "d");
    assert_eq!(outcome.command.as_deref(), Some("input.delete.line"));
    assert_eq!(*sink.lock().unwrap(), vec!["delete-line".to_owned()]);

    // Uncaptured input passes through (not consumed).
    let outcome = press(&mut keymap, "x");
    assert!(!outcome.handled);
    assert!(!outcome.consumed);
    assert_eq!(sink.lock().unwrap().len(), 1);
}

#[test]
fn managed_textarea_layer_accepts_command_bindings_overrides() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let overrides = vec![("input.delete.line".to_owned(), KeyLike::Str("dd".into()))];
    let bindings = command_bindings(&overrides, None);
    let (off, sink) = opentui_keymap::textarea::register_managed_textarea_layer(
        &mut keymap,
        opentui_keymap::textarea::ManagedTextareaLayer {
            bindings,
            priority: 0,
            mode: None,
        },
        None,
    );

    press(&mut keymap, "d");
    let outcome = press(&mut keymap, "d");
    assert_eq!(outcome.command.as_deref(), Some("input.delete.line"));
    assert_eq!(*sink.lock().unwrap(), vec!["delete-line".to_owned()]);

    // Default textarea bindings ride along (left → input.move.left).
    let outcome = press(&mut keymap, "left");
    assert_eq!(outcome.command.as_deref(), Some("input.move.left"));
    keymap.release(off);
}

// ---------------------------------------------------------------------------
// dispatch semantics — the oc-tui-core suite re-proven against the
// upstream 0.5.14 source (cited in engine.rs docs)
// ---------------------------------------------------------------------------

#[test]
fn layer_ordering_priority_then_registration() {
    // getSortedLayers: priority DESC, order DESC (later registration
    // shadows at equal priority).
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(commands_layer(&["session.submit", "app.exit"]));
    keymap.register_layer(Layer {
        bindings: vec![binding("return", "session.submit")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![binding("return", "app.exit")],
        ..Layer::default()
    });
    let outcome = press(&mut keymap, "return");
    assert_eq!(
        outcome.command.as_deref(),
        Some("app.exit"),
        "later registration shadows at equal priority"
    );

    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(commands_layer(&["session.submit", "app.exit"]));
    keymap.register_layer(Layer {
        priority: 10,
        bindings: vec![binding("return", "session.submit")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![binding("return", "app.exit")],
        ..Layer::default()
    });
    let outcome = press(&mut keymap, "return");
    assert_eq!(
        outcome.command.as_deref(),
        Some("session.submit"),
        "priority wins"
    );
    let _ = calls;
}

#[test]
fn focus_scoped_layers_bubble() {
    // isLayerActiveForFocused + getActivationPath.
    let (mut keymap, host, _clock) = create_test_keymap(true);
    // Tree: root(0) → composer(1) → caret(2); sidebar(3) → root.
    host.add(1, 0);
    host.add(2, 1);
    host.add(3, 0);
    keymap.register_layer(commands_layer(&[
        "app.submit",
        "composer.enter",
        "composer.focus.tab",
    ]));
    keymap.register_layer(Layer {
        bindings: vec![binding("return", "app.submit")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        target: Some(1),
        bindings: vec![binding("return", "composer.enter")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        target: Some(1),
        target_mode: Some(TargetMode::Focus),
        bindings: vec![binding("tab", "composer.focus.tab")],
        ..Layer::default()
    });

    host.focus(Some(2));
    let outcome = press(&mut keymap, "return");
    assert_eq!(
        outcome.command.as_deref(),
        Some("composer.enter"),
        "path layer active on child"
    );
    assert_eq!(
        press(&mut keymap, "tab").command,
        None,
        "focus layer inactive on child"
    );

    host.focus(Some(1));
    assert_eq!(
        press(&mut keymap, "tab").command.as_deref(),
        Some("composer.focus.tab")
    );

    host.focus(Some(3));
    assert_eq!(
        press(&mut keymap, "return").command.as_deref(),
        Some("app.submit")
    );
}

#[test]
fn mode_gating_and_globals() {
    // v2's require("opencode.mode", value) — global layers stay
    // reachable in every mode.
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(commands_layer(&["app.global", "app.base", "app.form"]));
    keymap.register_layer(Layer {
        bindings: vec![binding("g", "app.global")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        mode: Some("base".into()),
        bindings: vec![binding("b", "app.base")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        mode: Some("form".into()),
        bindings: vec![binding("f", "app.form")],
        ..Layer::default()
    });

    // v2's createMode always publishes the current mode into data
    // (keymap.tsx:425-426); "base" is the stack-empty default.
    keymap.set_data("opencode.mode", serde_json::json!("base"));
    assert_eq!(keymap.current_mode(), "base");
    assert_eq!(press(&mut keymap, "b").command.as_deref(), Some("app.base"));
    keymap.set_data("opencode.mode", serde_json::json!("form"));
    assert_eq!(
        press(&mut keymap, "b").command,
        None,
        "base layer unreachable in form mode"
    );
    assert_eq!(press(&mut keymap, "f").command.as_deref(), Some("app.form"));
    assert_eq!(
        press(&mut keymap, "g").command.as_deref(),
        Some("app.global")
    );
    keymap.set_data("opencode.mode", serde_json::json!("base"));
    assert_eq!(press(&mut keymap, "b").command.as_deref(), Some("app.base"));
}

#[test]
fn rejected_commands_fall_through() {
    // runBinding returning false → the binding counts as unhandled.
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(Layer {
        bindings: vec![binding("return", "first"), binding("return", "second")],
        ..Layer::default()
    });
    // No command named "first" is registered → unresolved → rejected
    // (with a warning) → "second" also unresolved → unhandled.
    let outcome = press(&mut keymap, "return");
    assert!(!outcome.handled);
    assert!(!keymap.take_warnings().is_empty());

    // A registered command rejecting via `false`.
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "reject-me",
            Box::new(|_| CommandResult::Rejected),
        )],
        bindings: vec![binding("tab", "reject-me"), binding("tab", "accept-me")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "accept-me",
            Box::new(|_| CommandResult::Handled),
        )],
        ..Layer::default()
    });
    let outcome = press(&mut keymap, "tab");
    assert_eq!(outcome.command.as_deref(), Some("accept-me"));
}

#[test]
fn prevent_default_object_semantics() {
    // preventDefault:false — the command resolves WITHOUT consuming.
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(Layer {
        commands: vec![Command::new("paste", Box::new(|_| CommandResult::Handled))],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![Binding {
            prevent_default: Some(false),
            ..binding("ctrl+v", "paste")
        }],
        ..Layer::default()
    });
    let outcome = press_mod(&mut keymap, "v", true, false, false, false);
    assert_eq!(outcome.command.as_deref(), Some("paste"));
    assert!(
        !outcome.consumed,
        "preventDefault:false leaves suppression to the handler"
    );
}

#[test]
fn interceptors_run_before_layers_by_precedence() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let seen = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(commands_layer(&["bound"]));
    keymap.register_layer(Layer {
        bindings: vec![binding("x", "bound")],
        ..Layer::default()
    });
    {
        let seen = seen.clone();
        keymap.intercept_key(
            Arc::new(move |_event| {
                seen.lock().unwrap().push("appended".to_owned());
                InterceptDecision::pass()
            }),
            0,
        );
    }
    {
        let seen = seen.clone();
        keymap.intercept_key(
            Arc::new(move |_event| {
                seen.lock().unwrap().push("prepended".to_owned());
                InterceptDecision::pass()
            }),
            1,
        );
    }
    let outcome = press(&mut keymap, "x");
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["prepended".to_owned(), "appended".to_owned()]
    );
    assert_eq!(outcome.command.as_deref(), Some("bound"));

    // A consumer stops the layer dispatch.
    keymap.intercept_key(
        Arc::new(move |_event| InterceptDecision {
            consume: true,
            ..InterceptDecision::pass()
        }),
        2,
    );
    let outcome = press(&mut keymap, "x");
    assert_eq!(outcome.command, None);
    assert_eq!(outcome.reason, KeyAfterReason::InterceptConsumed);
}

#[test]
fn which_key_data_model() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    register_timed_leader(
        &mut keymap,
        TimedLeaderOptions::new(KeyStrokeInput {
            name: "x".into(),
            ctrl: Some(true),
            ..KeyStrokeInput::default()
        }),
    );
    keymap.register_layer(commands_layer(&["session.new", "slot1", "slot10"]));
    keymap.register_layer(Layer {
        bindings: vec![
            binding("<leader>n", "session.new"),
            binding("<leader>1", "slot1"),
            binding("<leader>0", "slot10"),
        ],
        ..Layer::default()
    });

    let outcome = press_mod(&mut keymap, "x", true, false, false, false);
    assert_eq!(outcome.reason, KeyAfterReason::SequencePending);
    let pending = keymap.get_pending_sequence();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].display, "<leader>");
    assert_eq!(pending[0].token_name.as_deref(), Some("leader"));

    let mut keys = keymap.active_keys();
    keys.sort_by(|left, right| left.display.cmp(&right.display));
    assert_eq!(keys.len(), 3, "n, 1, 0 reachable");
    assert!(keys.iter().all(|key| !key.continues));

    let outcome = press(&mut keymap, "n");
    assert_eq!(outcome.command.as_deref(), Some("session.new"));
    assert!(keymap.get_pending_sequence().is_empty());
    // Upstream `getActiveKeys` lists the reachable ROOT keys while no
    // sequence is pending — the leader stays reachable (a continues
    // root key). [Delta vs oc-tui-core, whose active_keys() returned
    // only pending-derived options.]
    let root = keymap.active_keys();
    assert!(root
        .iter()
        .any(|key| key.display == "<leader>" && key.continues));
}

#[test]
fn sequence_miss_clears_unconsumed() {
    // UPSTREAM 0.5.14 semantics (dispatch.ts:620-627): a pending miss
    // clears the sequence and leaves the event UNCONSUMED (it falls
    // through to the focused target). [Delta vs oc-tui-core, which
    // re-evaluated the stroke fresh from the root.]
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(Layer {
        bindings: vec![binding("dd", "delete-line")],
        ..Layer::default()
    });

    press(&mut keymap, "d");
    let outcome = press(&mut keymap, "x");
    assert_eq!(outcome.reason, KeyAfterReason::SequenceMiss);
    assert!(
        !outcome.consumed,
        "the missed stroke escapes to the focused target"
    );
    assert!(!keymap.has_pending_sequence());
    // The next `d` starts FRESH.
    press(&mut keymap, "d");
    assert!(keymap.has_pending_sequence());
}

#[test]
fn continuation_beats_exact_on_advance() {
    // dispatchPendingCapturesFromIndex (dispatch.ts:655-663): a
    // continuation capture for the typed prefix HOLDS over an exact
    // completion — the no-resolver ambiguity default. The
    // continuation layer must outrank the exact one (the root walk
    // stops at the first layer with an answer); this mirrors v2's
    // `<leader>1` (quick switch) vs `<leader>1,ctrl+1` (tab select).
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let commands = Layer {
        commands: vec![
            Command::new("goto", Box::new(|_| CommandResult::Handled)),
            Command::new("single-g", Box::new(|_| CommandResult::Handled)),
        ],
        ..Layer::default()
    };
    keymap.register_layer(commands.clone());
    // Lower layer: exact single "g".
    keymap.register_layer(Layer {
        bindings: vec![binding("g", "single-g")],
        ..Layer::default()
    });
    // Higher layer (registered later): the "gg" continuation.
    keymap.register_layer(Layer {
        bindings: vec![binding("gg", "goto")],
        ..Layer::default()
    });

    let outcome = press(&mut keymap, "g");
    assert_eq!(outcome.reason, KeyAfterReason::SequencePending);
    assert!(keymap.has_pending_sequence());
    let outcome = press(&mut keymap, "g");
    assert_eq!(outcome.command.as_deref(), Some("goto"));
}

#[test]
fn exact_in_higher_layer_beats_lower_continuation() {
    // The mirror case: the higher layer's exact answer wins the root
    // walk before the lower layer's continuation can hold.
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(Layer {
        commands: vec![
            Command::new("goto", Box::new(|_| CommandResult::Handled)),
            Command::new("single-g", Box::new(|_| CommandResult::Handled)),
        ],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![binding("gg", "goto")],
        ..Layer::default()
    });
    keymap.register_layer(Layer {
        bindings: vec![binding("g", "single-g")],
        ..Layer::default()
    });
    let outcome = press(&mut keymap, "g");
    assert_eq!(outcome.command.as_deref(), Some("single-g"));
    assert!(!keymap.has_pending_sequence());
}

#[test]
fn exact_only_layer_wins_when_no_continuations() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(commands_layer(&["single-g"]));
    keymap.register_layer(Layer {
        bindings: vec![binding("g", "single-g")],
        ..Layer::default()
    });
    let outcome = press(&mut keymap, "g");
    assert_eq!(outcome.command.as_deref(), Some("single-g"));
    assert!(!keymap.has_pending_sequence());
}

#[test]
fn disabled_layer_leader_does_not_arm() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    register_timed_leader(
        &mut keymap,
        TimedLeaderOptions::new(KeyStrokeInput {
            name: "x".into(),
            ctrl: Some(true),
            ..KeyStrokeInput::default()
        }),
    );
    keymap.register_layer(Layer {
        enabled: Some(Enabled::Bool(false)),
        bindings: vec![binding("<leader>q", "app.exit")],
        ..Layer::default()
    });
    let outcome = press_mod(&mut keymap, "x", true, false, false, false);
    assert!(!outcome.handled, "a disabled layer's leader does not arm");
    assert!(!keymap.has_pending_sequence());
}

#[test]
fn run_command_and_queries() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "app.exit",
            Box::new({
                let calls = calls.clone();
                move |ctx| {
                    calls.lock().unwrap().push(format!("exit:{}", ctx.input));
                    CommandResult::Handled
                }
            }),
        )],
        bindings: vec![binding("ctrl+q", "app.exit")],
        ..Layer::default()
    });

    let names: Vec<String> = keymap.get_commands().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["app.exit".to_owned()]);
    let bindings = keymap.get_command_bindings(&["app.exit"]);
    assert_eq!(bindings["app.exit"].len(), 1);
    assert!(bindings["app.exit"][0].prevent_default);

    let result = keymap.run_command("app.exit", Some("forced"));
    assert!(result.is_ok());
    assert_eq!(*calls.lock().unwrap(), vec!["exit:forced".to_owned()]);
    assert!(!keymap.run_command("missing", None).is_ok());
}

// ---------------------------------------------------------------------------
// extras/tests/binding-lookup.test.ts
// ---------------------------------------------------------------------------

fn str_value(key: &str) -> BindingValue {
    BindingValue::from(key)
}

/// A `BindingConfigItem` that is a bare key string.
fn item(key: &str) -> BindingConfigItem {
    BindingConfigItem::Key(KeyLike::Str(key.to_owned()))
}

#[test]
fn binding_lookup_resolves_flat_config() {
    // "resolves flat command config into command lookups and binding
    // arrays".
    let open_key = KeyStrokeInput {
        name: "o".into(),
        ctrl: Some(true),
        ..Default::default()
    };
    let config: BindingConfig = vec![
        (" show_dialog ".to_owned(), str_value("ctrl+d")),
        (
            "close_dialog".to_owned(),
            BindingValue::Items(vec![
                item("escape"),
                BindingConfigItem::Binding(Binding {
                    prevent_default: Some(false),
                    ..binding("ctrl+c", "ignored")
                }),
            ]),
        ),
        (
            "open_file".to_owned(),
            BindingValue::Item(BindingConfigItem::Key(KeyLike::Stroke(open_key))),
        ),
    ];
    let lookup = create_binding_lookup(config, None).unwrap();

    assert_eq!(lookup.bindings().len(), 4);
    assert_eq!(lookup.get(" show_dialog ").len(), 1);
    // Exact command identity — no trimming.
    assert!(lookup.get("show_dialog").is_empty());
    assert!(lookup.get("missing_command").is_empty());
    assert!(lookup.has(" show_dialog "));
    assert!(!lookup.has("show_dialog"));
}

#[test]
fn binding_lookup_config_key_is_command_identity() {
    // "uses config command keys as the binding command identity".
    let config: BindingConfig = vec![(
        "show_dialog".to_owned(),
        BindingValue::Item(BindingConfigItem::Binding(Binding {
            prevent_default: Some(false),
            ..binding("d", "ignored_command")
        })),
    )];
    let lookup = create_binding_lookup(config, None).unwrap();
    let resolved = lookup.get("show_dialog");
    assert_eq!(resolved[0].key, KeyLike::Str("d".into()));
    assert_eq!(
        resolved[0].cmd,
        Some(BindingCommand::Name("show_dialog".into()))
    );
    assert!(lookup.get("ignored_command").is_empty());
    assert!(!lookup.has("ignored_command"));
}

#[test]
fn binding_lookup_defaults_merge_under_explicit_fields() {
    // "applies binding defaults without overriding explicit binding
    // fields".
    let config: BindingConfig = vec![
        (
            "save_file".to_owned(),
            BindingValue::Item(BindingConfigItem::Binding(Binding {
                attrs: [
                    ("desc".to_owned(), serde_json::json!("Explicit description")),
                    ("group".to_owned(), serde_json::json!("Explicit group")),
                ]
                .into_iter()
                .collect(),
                prevent_default: Some(true),
                ..binding_from_stroke("s", "ignored")
            })),
        ),
        ("open_file".to_owned(), str_value("o")),
        (
            "multi_action".to_owned(),
            BindingValue::Items(vec![
                item("m"),
                BindingConfigItem::Binding(Binding {
                    attrs: [(
                        "group".to_owned(),
                        serde_json::json!("Explicit multi group"),
                    )]
                    .into_iter()
                    .collect(),
                    ..binding("shift+m", "ignored")
                }),
            ]),
        ),
        ("disabled_action".to_owned(), BindingValue::Disabled),
        ("empty_action".to_owned(), BindingValue::Items(vec![])),
    ];
    let mut calls: Vec<String> = Vec::new();
    let lookup = create_binding_lookup(
        config,
        Some(CreateBindingLookupOptions {
            command_map: None,
            binding_defaults: Some(Box::new(|_ctx: &BindingDefaultsContext| {
                // Defaults for desc/group/preventDefault — explicit
                // binding fields win.
                let mut defaults = std::collections::BTreeMap::new();
                defaults.insert("desc".to_owned(), serde_json::json!("Default description"));
                defaults.insert("group".to_owned(), serde_json::json!("Default group"));
                Some(defaults)
            })),
        }),
    )
    .unwrap();
    let _ = &mut calls;

    let save = lookup.get("save_file");
    assert_eq!(
        save[0].attrs.get("desc"),
        Some(&serde_json::json!("Explicit description"))
    );
    assert_eq!(
        save[0].attrs.get("group"),
        Some(&serde_json::json!("Explicit group"))
    );
    let open = lookup.get("open_file");
    assert_eq!(
        open[0].attrs.get("desc"),
        Some(&serde_json::json!("Default description"))
    );
    let multi = lookup.get("multi_action");
    assert_eq!(
        multi[0].attrs.get("group"),
        Some(&serde_json::json!("Default group"))
    );
    assert_eq!(
        multi[1].attrs.get("group"),
        Some(&serde_json::json!("Explicit multi group"))
    );
    assert!(lookup.get("disabled_action").is_empty());
    assert!(lookup.get("empty_action").is_empty());
}

fn binding_from_stroke(name: &str, cmd: &str) -> Binding {
    Binding::new(
        KeyLike::Stroke(KeyStrokeInput::new(name)),
        Some(BindingCommand::Name(cmd.to_owned())),
    )
}

#[test]
fn binding_lookup_command_map_and_gather_pick_omit() {
    // "maps exact config command names to internal command names" +
    // "picks and omits from existing gathered groups".
    let config: BindingConfig = vec![
        ("show_dialog".to_owned(), str_value("d")),
        (
            "close_dialog".to_owned(),
            BindingValue::Items(vec![
                item("escape"),
                BindingConfigItem::Binding(Binding {
                    prevent_default: Some(false),
                    ..binding("ctrl+c", "ignored")
                }),
            ]),
        ),
        ("unmapped_command".to_owned(), str_value("u")),
    ];
    let mut command_map = std::collections::HashMap::new();
    command_map.insert("show_dialog".to_owned(), "dialog.show".to_owned());
    command_map.insert("close_dialog".to_owned(), "dialog.close".to_owned());
    let mut lookup = create_binding_lookup(
        config,
        Some(CreateBindingLookupOptions {
            command_map: Some(command_map),
            binding_defaults: Some(Box::new(|ctx: &BindingDefaultsContext| {
                Some(
                    [(
                        ctx.command.to_owned(),
                        serde_json::json!("group-placeholder"),
                    )]
                    .into_iter()
                    .collect(),
                )
            })),
        }),
    )
    .unwrap();

    assert_eq!(
        lookup.bindings()[0].cmd,
        Some(BindingCommand::Name("dialog.show".into()))
    );
    assert!(lookup.has("dialog.show"));
    assert!(!lookup.has("show_dialog"));

    lookup.gather("dialog", &["dialog.show", "missing", "dialog.close"]);
    let picked = lookup.pick("dialog", &["dialog.close"]);
    assert_eq!(picked.len(), 2);
    assert_eq!(picked[0].key, KeyLike::Str("escape".into()));
    let omitted = lookup.omit("dialog", &["dialog.close"]);
    assert_eq!(omitted.len(), 1);
    assert_eq!(
        omitted[0].cmd,
        Some(BindingCommand::Name("dialog.show".into()))
    );
}

#[test]
fn binding_lookup_duplicate_mapped_commands_replace() {
    // "lets duplicate mapped commands replace or disable earlier
    // mapped commands".
    let config: BindingConfig = vec![
        ("first".to_owned(), str_value("1")),
        (
            "second".to_owned(),
            BindingValue::Items(vec![item("2a"), item("2b")]),
        ),
        ("disabled".to_owned(), BindingValue::Disabled),
        ("third".to_owned(), str_value("3")),
    ];
    let mut command_map = std::collections::HashMap::new();
    for name in ["first", "second", "disabled", "third"] {
        command_map.insert(name.to_owned(), "shared.command".to_owned());
    }
    let lookup = create_binding_lookup(
        config,
        Some(CreateBindingLookupOptions {
            command_map: Some(command_map),
            binding_defaults: None,
        }),
    )
    .unwrap();
    assert_eq!(lookup.bindings().len(), 1);
    assert_eq!(lookup.bindings()[0].key, KeyLike::Str("3".into()));
    assert!(lookup.has("shared.command"));
    assert!(!lookup.has("first"));
}

#[test]
fn binding_lookup_update_and_invalidate() {
    // "updates the command lookup and clears gathered groups" +
    // "invalidates one gathered group or all".
    let mut lookup = create_binding_lookup(
        vec![
            ("open_dialog".to_owned(), str_value("o")),
            ("close_dialog".to_owned(), str_value("escape")),
            ("submit_dialog".to_owned(), str_value("enter")),
        ],
        None,
    )
    .unwrap();

    lookup.gather("dialog", &["open_dialog"]);
    lookup.gather("submit", &["submit_dialog"]);
    lookup.invalidate(Some("dialog"));
    let next_dialog = lookup.gather("dialog", &["close_dialog"]);
    assert_eq!(next_dialog.len(), 1);
    // The submit group survives a targeted invalidate.
    assert_eq!(lookup.gather("submit", &["open_dialog"]).len(), 1);

    lookup.update(Some(vec![
        ("open_dialog".to_owned(), str_value("p")),
        ("close_dialog".to_owned(), str_value("escape")),
    ]));
    assert_eq!(
        lookup.get("open_dialog")[0].key,
        KeyLike::Str("p".into()),
        "update replaces the config"
    );
}

#[test]
fn binding_lookup_errors() {
    // "throws for invalid commands and binding values" — the type
    // system rules out invalid items; empty commands and bad command
    // maps error.
    assert!(create_binding_lookup(vec![("".to_owned(), str_value("s"))], None).is_err());
    let mut command_map = std::collections::HashMap::new();
    command_map.insert("save_file".to_owned(), String::new());
    let err = create_binding_lookup(
        vec![("save_file".to_owned(), str_value("s"))],
        Some(CreateBindingLookupOptions {
            command_map: Some(command_map),
            binding_defaults: None,
        }),
    )
    .unwrap_err();
    assert_eq!(
        err,
        "Invalid binding command map entry for \"save_file\": command cannot be empty"
    );
}

#[test]
fn command_bindings_maps_and_warns() {
    // extras/tests/command-bindings.test.ts shape. Insertion-ordered
    // pairs (upstream Object.entries order).
    let bindings: Vec<(String, KeyLike)> = vec![
        ("dialog.show".to_owned(), KeyLike::Str("d".into())),
        (" dialog.close ".to_owned(), KeyLike::Str("escape".into())), // trims
    ];
    let out = command_bindings(&bindings, None);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].cmd, Some(BindingCommand::Name("dialog.show".into())));
    assert_eq!(
        out[1].cmd,
        Some(BindingCommand::Name("dialog.close".into()))
    );

    // A command repeated via trimming replaces with a warning.
    let bindings: Vec<(String, KeyLike)> = vec![
        ("cmd".to_owned(), KeyLike::Str("a".into())),
        (" cmd ".to_owned(), KeyLike::Str("b".into())),
    ];
    let mut warnings = Vec::new();
    let out = command_bindings(&bindings, Some(&mut |warning| warnings.push(warning)));
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].command, "cmd");
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].key,
        KeyLike::Str("b".into()),
        "the later mapping replaced the earlier"
    );
}

// ---------------------------------------------------------------------------
// extras/tests/formatting.test.ts
// ---------------------------------------------------------------------------

fn parse(keymap: &Keymap, key: &str) -> Vec<KeySequencePart> {
    keymap.parse_key_sequence(key).unwrap()
}

#[test]
fn formatting_canonical_sequences() {
    let (keymap, _host, _clock) = create_test_keymap(true);
    assert_eq!(
        format_key_sequence(&parse(&keymap, "dd"), &Default::default()),
        "d d"
    );
    // The object form: ctrl+return → "ctrl+enter".
    let parts = vec![opentui_keymap::stringify::create_key_sequence_part(
        &KeyStrokeInput {
            name: "return".into(),
            ctrl: Some(true),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap()];
    assert_eq!(
        format_key_sequence(&parts, &Default::default()),
        "ctrl+enter"
    );
    assert_eq!(format_key_sequence(&[], &Default::default()), "");
}

#[test]
fn formatting_key_and_modifier_aliases() {
    let (keymap, _host, _clock) = create_test_keymap(true);
    let mut modifier_aliases = std::collections::HashMap::new();
    modifier_aliases.insert("meta", "alt".to_owned());
    let options = FormatKeySequenceOptions {
        key_name_aliases: Some(
            [("pageup".to_owned(), "pgup".to_owned())]
                .into_iter()
                .collect(),
        ),
        modifier_aliases: Some(modifier_aliases),
        ..FormatKeySequenceOptions::default()
    };
    let parts = vec![opentui_keymap::stringify::create_key_sequence_part(
        &KeyStrokeInput {
            name: "pageup".into(),
            meta: Some(true),
            shift: Some(true),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap()];
    assert_eq!(format_key_sequence(&parts, &options), "shift+alt+pgup");

    // The hyper case: meh+del.
    let mut modifier_aliases = std::collections::HashMap::new();
    modifier_aliases.insert("hyper", "meh".to_owned());
    let options = FormatKeySequenceOptions {
        key_name_aliases: Some(
            [("delete".to_owned(), "del".to_owned())]
                .into_iter()
                .collect(),
        ),
        modifier_aliases: Some(modifier_aliases),
        ..FormatKeySequenceOptions::default()
    };
    let parts = vec![opentui_keymap::stringify::create_key_sequence_part(
        &KeyStrokeInput {
            name: "delete".into(),
            hyper: Some(true),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap()];
    assert_eq!(format_key_sequence(&parts, &options), "meh+del");

    // Full modifier renaming with enter → return alias.
    let mut modifier_aliases = std::collections::HashMap::new();
    for (name, alias) in [
        ("ctrl", "C"),
        ("shift", "S"),
        ("meta", "M"),
        ("super", "Super"),
        ("hyper", "Hyper"),
    ] {
        modifier_aliases.insert(name, alias.to_owned());
    }
    let options = FormatKeySequenceOptions {
        key_name_aliases: Some(
            [("enter".to_owned(), "return".to_owned())]
                .into_iter()
                .collect(),
        ),
        modifier_aliases: Some(modifier_aliases),
        token_display: None,
        pattern_display: None,
        separator: None,
    };
    let parts = vec![opentui_keymap::stringify::create_key_sequence_part(
        &KeyStrokeInput {
            name: "return".into(),
            ctrl: Some(true),
            shift: Some(true),
            meta: Some(true),
            super_key: Some(true),
            hyper: Some(true),
        },
        None,
        None,
    )
    .unwrap()];
    assert_eq!(
        format_key_sequence(&parts, &options),
        "C+S+M+Super+Hyper+return"
    );
    let _ = keymap;
}

#[test]
fn formatting_token_display_and_separators() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_token(KeyToken {
        name: "leader".into(),
        key: KeyLike::Stroke(KeyStrokeInput::new("space")),
    });
    let sequence = keymap.parse_key_sequence("<leader>s").unwrap();

    // Token display preserved by default.
    assert_eq!(
        format_key_sequence(&sequence, &Default::default()),
        "<leader> s"
    );
    // Map overrides.
    let options = FormatKeySequenceOptions {
        token_display: Some(
            [("leader".to_owned(), "space".to_owned())]
                .into_iter()
                .collect(),
        ),
        ..FormatKeySequenceOptions::default()
    };
    assert_eq!(format_key_sequence(&sequence, &options), "space s");
    // Empty-string override allowed.
    let options = FormatKeySequenceOptions {
        token_display: Some([("leader".to_owned(), String::new())].into_iter().collect()),
        ..FormatKeySequenceOptions::default()
    };
    assert_eq!(format_key_sequence(&sequence, &options), " s");
    // Empty separator.
    let options = FormatKeySequenceOptions {
        separator: Some(String::new()),
        ..FormatKeySequenceOptions::default()
    };
    assert_eq!(format_key_sequence(&parse(&keymap, "dd"), &options), "dd");
}

#[test]
fn formatting_command_bindings_dedupe() {
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_token(KeyToken {
        name: "leader".into(),
        key: KeyLike::Stroke(KeyStrokeInput {
            name: "x".into(),
            ctrl: Some(true),
            ..Default::default()
        }),
    });
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "save-file",
            Box::new(|_| CommandResult::Handled),
        )],
        bindings: vec![
            binding("ctrl+s", "save-file"),
            binding("ctrl+s", "save-file"),
            binding("<leader>s", "save-file"),
        ],
        ..Layer::default()
    });
    let bindings = keymap.get_command_bindings(&["save-file"]);
    let sequences: Vec<Vec<KeySequencePart>> = bindings["save-file"]
        .iter()
        .map(|b| b.sequence.clone())
        .collect();
    assert_eq!(
        format_command_bindings(&sequences, &FormatCommandBindingsOptions::default()),
        Some("ctrl+s, <leader> s".to_owned())
    );

    // Custom separators + duplicate retention.
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "duplicate",
            Box::new(|_| CommandResult::Handled),
        )],
        bindings: vec![binding("dd", "duplicate"), binding("dd", "duplicate")],
        ..Layer::default()
    });
    let bindings = keymap.get_command_bindings(&["duplicate"]);
    let sequences: Vec<Vec<KeySequencePart>> = bindings["duplicate"]
        .iter()
        .map(|b| b.sequence.clone())
        .collect();
    let mut options = FormatCommandBindingsOptions::default();
    options.sequence.separator = Some(" then ".into());
    options.binding_separator = Some(" | ".into());
    assert_eq!(
        format_command_bindings(&sequences, &options),
        Some("d then d".to_owned())
    );
    let options = FormatCommandBindingsOptions {
        binding_separator: Some(" | ".into()),
        dedupe: false,
        ..FormatCommandBindingsOptions::default()
    };
    assert_eq!(
        format_command_bindings(&sequences, &options),
        Some("d d | d d".to_owned())
    );
    assert_eq!(format_command_bindings(&[], &options), None);

    // Dedupe by formatted display after aliases ("enter"/"return").
    let (mut keymap, _host, _clock) = create_test_keymap(true);
    keymap.register_layer(Layer {
        commands: vec![Command::new(
            "alias-duplicate",
            Box::new(|_| CommandResult::Handled),
        )],
        bindings: vec![
            binding("enter", "alias-duplicate"),
            binding("return", "alias-duplicate"),
        ],
        ..Layer::default()
    });
    let bindings = keymap.get_command_bindings(&["alias-duplicate"]);
    let sequences: Vec<Vec<KeySequencePart>> = bindings["alias-duplicate"]
        .iter()
        .map(|b| b.sequence.clone())
        .collect();
    assert_eq!(
        format_command_bindings(&sequences, &FormatCommandBindingsOptions::default()),
        Some("enter, return".to_owned())
    );
    let mut options = FormatCommandBindingsOptions::default();
    options.sequence.key_name_aliases = Some(
        [("enter".to_owned(), "ret".to_owned())]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        format_command_bindings(&sequences, &options),
        Some("ret".to_owned())
    );
    options.dedupe = false;
    assert_eq!(
        format_command_bindings(&sequences, &options),
        Some("ret, ret".to_owned())
    );
}

// ---------------------------------------------------------------------------
// solid provider
// ---------------------------------------------------------------------------

#[test]
fn solid_provider_and_selector() {
    let (keymap, _host, _clock) = create_test_keymap(true);
    let provider = opentui_keymap::solid::KeymapProvider::new(keymap);
    let handle = provider.use_keymap();
    assert!(handle.is_alive());

    let mut selector = provider.use_keymap_selector(|keymap| {
        keymap
            .get_commands()
            .into_iter()
            .map(|command| command.name)
            .collect::<Vec<_>>()
    });
    assert!(selector
        .read(|keymap| keymap
            .get_commands()
            .into_iter()
            .map(|command| command.name)
            .collect::<Vec<_>>())
        .is_empty());

    // Register through a child handle; the selector re-derives.
    handle.with_keymap(|keymap| {
        keymap.register_layer(Layer {
            commands: vec![Command::new(
                "cmd.one",
                Box::new(|_| CommandResult::Handled),
            )],
            ..Layer::default()
        });
    });
    let names = selector.read(|keymap| {
        keymap
            .get_commands()
            .into_iter()
            .map(|command| command.name)
            .collect::<Vec<_>>()
    });
    assert_eq!(names, vec!["cmd.one".to_owned()]);

    // useBindings registers a layer for the scope; dropping the guard
    // unregisters.
    let guard = provider.use_bindings(|| opentui_keymap::solid::UseBindingsLayer {
        bindings: vec![binding("ctrl+t", "cmd.one")],
        ..Default::default()
    });
    assert!(guard.is_some());
    let pending_names = handle.with_keymap(|keymap| {
        let names: Vec<String> = keymap.get_commands().into_iter().map(|c| c.name).collect();
        names
    });
    assert_eq!(pending_names, vec!["cmd.one".to_owned()]);
    drop(guard);
    let after = handle.with_keymap(|keymap| keymap.get_commands().len());
    assert_eq!(
        after, 1,
        "the command layer survives; the bindings layer is gone"
    );
}
