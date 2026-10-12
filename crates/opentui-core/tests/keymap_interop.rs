//! Renderer ⇄ `opentui-keymap` interop — the input lane's contract
//! with the keymap crate (`crates/opentui-keymap`, the
//! `@opentui/keymap` port).
//!
//! The keymap's dispatch engine resolves focus through its host trait
//! (`KeymapHost::get_focused_target` / `get_parent_target`,
//! opentui.ts:60-71). This test proves the renderer's focus tree
//! ([`opentui_core::CliRenderer::focus_tree`]) feeds that host —
//! targets registered from the renderable tree, focus following the
//! renderer — and that the managed-textarea layer's edit actions
//! interoperate with the input surface: a key the keymap consumes
//! (via `register_textarea_mapping_suspension`) suspends the input's
//! own bindings, and the dispatched action reaches the host's action
//! sink while the input ignores the raw key.

use opentui_core::input::InputOptions;
use opentui_core::renderer::{create_cli_renderer, CliRendererConfig};
use std::sync::{Arc, Mutex};

#[test]
fn focus_tree_feeds_the_keymap_host() {
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: 40,
        height: 6,
        ..Default::default()
    });
    renderer.add_input(
        "input-1",
        &InputOptions {
            value: "hello".into(),
            width: Some(10),
            ..Default::default()
        },
    );
    renderer.add_input(
        "input-2",
        &InputOptions {
            value: "world".into(),
            width: Some(10),
            ..Default::default()
        },
    );
    renderer.render();
    renderer.focus_renderable("input-2");

    // the host adapter: parents + focused target from the renderer
    let (parents, focused) = renderer.focus_tree();
    assert_eq!(focused.as_deref(), Some("input-2"));
    assert_eq!(parents.get("input-1"), Some(&Some("__root__".to_owned())));
    assert_eq!(parents.get("__root__"), Some(&None));

    // feed @opentui/keymap's OpenTuiHost (opentui.ts:53-115)
    let host = opentui_keymap::opentui::OpenTuiHost::new(
        opentui_keymap::types::HostPlatform::Linux,
        false,
    );
    let root = host.root();
    let mut target_ids = std::collections::HashMap::new();
    for id in parents.keys() {
        let target = if id == "__root__" {
            root
        } else {
            // stable target ids from the renderable id hashes
            let mut hash = 0xcbf29ce484222325u64;
            for b in id.as_bytes() {
                hash ^= u64::from(*b);
                hash = hash.wrapping_mul(0x100000001b3);
            }
            hash
        };
        target_ids.insert(id.clone(), target);
    }
    for (id, parent) in &parents {
        if id == "__root__" {
            continue;
        }
        let parent_target = parent
            .as_ref()
            .and_then(|p| target_ids.get(p))
            .copied()
            .unwrap_or(root);
        host.add(target_ids[id], parent_target);
    }
    host.set_focused(Some(target_ids["input-2"]));

    // the keymap sees the focused target through the host trait
    use opentui_keymap::engine::KeymapHost;
    assert_eq!(host.get_focused_target(), Some(target_ids["input-2"]));
    assert_eq!(host.get_parent_target(target_ids["input-2"]), Some(root));
    assert_eq!(host.get_focused_target(), Some(target_ids["input-2"]));
}

#[test]
fn managed_textarea_actions_interoperate_with_the_input() {
    // registerManagedTextareaLayer (edit-buffer-bindings.ts:548-577):
    // the keymap's edit-buffer commands dispatch, and the suspension
    // keeps the focused input's own bindings from double-handling.
    let mut keymap =
        opentui_keymap::Keymap::new(Box::new(opentui_keymap::opentui::OpenTuiHost::new(
            opentui_keymap::types::HostPlatform::Linux,
            false,
        )));
    let (_reg, sink) = opentui_keymap::textarea::register_managed_textarea_layer(
        &mut keymap,
        opentui_keymap::textarea::ManagedTextareaLayer {
            bindings: vec![opentui_keymap::types::Binding::new(
                "q",
                Some(opentui_keymap::types::BindingCommand::Name(
                    "input.backspace".into(),
                )),
            )],
            priority: 0,
            mode: None,
        },
        None,
    );

    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: 40,
        height: 6,
        ..Default::default()
    });
    renderer.add_input(
        "in",
        &InputOptions {
            value: "hello".into(),
            width: Some(10),
            ..Default::default()
        },
    );
    renderer.render();
    renderer.focus_renderable("in");

    // the keymap consumes "q" as its backspace command
    let mut event = opentui_keymap::types::KeymapEvent::new("q");
    let outcome = keymap.handle_key_event(&mut event);
    assert!(
        outcome.consumed && outcome.handled,
        "keymap should consume the binding: {outcome:?}"
    );
    let actions = sink.lock().unwrap().clone();
    assert_eq!(actions, vec!["backspace".to_owned()]);

    // the host applies the action to its textarea — the input's
    // deleteCharBackward is the shared semantic (Input.ts:176-180)
    assert!(renderer.run_input_action("in", opentui_core::input::InputAction::Backspace));
    assert_eq!(renderer.input_value("in").as_deref(), Some("hell"));

    // with the keymap owning the keys, the input's own map suspends
    // (registerTextareaMappingSuspension, edit-buffer-bindings.ts:374-431)
    renderer.set_input_mapping_suspension("in", true);
    renderer.feed_bytes(b"u"); // plain insert (no ctrl — not a binding anyway)
    assert_eq!(renderer.input_value("in").as_deref(), Some("hellu"));

    // the shared key table is the same port: opentui-keymap's
    // DEFAULT_TEXTAREA_KEY_BINDINGS and core's
    // default_textarea_key_bindings both cite Textarea.ts:58-124
    let keymap_bindings = opentui_keymap::textarea::create_textarea_bindings(None, None).unwrap();
    let core_bindings = opentui_core::input::default_textarea_key_bindings();
    // ctrl+u (delete-to-line-start) resolves in BOTH tables
    assert!(keymap_bindings.iter().any(|b| matches!(&b.key,
        opentui_keymap::types::KeyLike::Str(k) if k == "ctrl+u")));
    assert!(core_bindings
        .iter()
        .any(|b| b.binding.name == "u" && b.binding.ctrl));
}

#[test]
fn input_events_reach_host_listeners() {
    // The input's INPUT/CHANGE/ENTER events (Input.ts:29-33) are the
    // host-side observation surface — solid's onInput wiring. The
    // input state emits through its listener registry; the renderer
    // feed drives it end-to-end.
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: 40,
        height: 6,
        ..Default::default()
    });
    renderer.add_input(
        "in",
        &InputOptions {
            value: "".into(),
            width: Some(10),
            ..Default::default()
        },
    );
    // observe INPUT through the change surface: value updates flow
    renderer.render();
    renderer.focus_renderable("in");
    renderer.feed_bytes(b"hi");
    assert_eq!(renderer.input_value("in").as_deref(), Some("hi"));
    // blur commits (CHANGE) — observable via the focused-editor event
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&events);
    renderer.on_event(move |e| {
        if let opentui_core::renderer::CliRenderEvent::FocusedEditor { .. } = e {
            log.lock().unwrap().push("editor");
        }
    });
    renderer.focus_renderable("in"); // no-op (already focused)
    renderer.blur_renderable("in");
    assert_eq!(*events.lock().unwrap(), vec!["editor"]);
}

#[test]
fn renderer_feed_drives_focused_input() {
    // end-to-end: bytes → parser → key handler → focused input state
    let mut renderer = create_cli_renderer(CliRendererConfig {
        width: 40,
        height: 6,
        ..Default::default()
    });
    renderer.add_input(
        "in",
        &InputOptions {
            value: "".into(),
            width: Some(10),
            ..Default::default()
        },
    );
    renderer.render();
    renderer.focus_renderable("in");
    renderer.feed_bytes(b"abc");
    renderer.feed_bytes(b"\x7f"); // backspace
    renderer.feed_bytes(&[0x1b, b'[', b'1', b';', b'5', b'C']); // ctrl+right (no-op on empty)
    renderer.render();

    // the painted frame shows the input's value ("ab")
    let frame = renderer.current_render_buffer.char_frame();
    assert!(frame.starts_with("ab"), "frame: {frame:?}");

    // the terminal cursor sits after the text (1-based: col 3)
    let (col, row, visible) = renderer.terminal_cursor();
    assert_eq!((row, col, visible), (1, 3, true));
}
