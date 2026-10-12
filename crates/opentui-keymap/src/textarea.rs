//! `addons/opentui/edit-buffer-bindings.ts` (577 lines) — the
//! standard edit-buffer command set, the default textarea bindings
//! table, and `registerManagedTextareaLayer`.
//!
//! The command name/description/fine-group maps (lines 80-195) and
//! `createTextareaBindings` (overrides prepended, lines 341-358) are
//! order-exact data ports. The command HANDLERS (lines 446-489) call
//! `@opentui/core`'s `EditBufferRenderable` — the Rust port resolves
//! the command and records the action on a shared sink the host
//! binds to its textarea (`withFocusedEditor`, lines 433-440); the
//! renderable-tree wiring lands with the opentui-core input lane
//! (see README "Divergences").

use crate::addons::{EDIT_BUFFER_COMMANDS_RESOURCE, TEXTAREA_MAPPING_SUSPENSION_RESOURCE};
use crate::engine::Registration;
use crate::types::{Binding, BindingCommand, Command, CommandResult, Layer};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// `EditBufferCommandName` (edit-buffer-bindings.ts:74) — the action
/// ids in upstream declaration order (lines 35-72).
pub const EDIT_BUFFER_ACTIONS: &[&str] = &[
    "move-left",
    "move-right",
    "move-up",
    "move-down",
    "select-left",
    "select-right",
    "select-up",
    "select-down",
    "line-home",
    "line-end",
    "select-line-home",
    "select-line-end",
    "visual-line-home",
    "visual-line-end",
    "select-visual-line-home",
    "select-visual-line-end",
    "buffer-home",
    "buffer-end",
    "select-buffer-home",
    "select-buffer-end",
    "delete-line",
    "delete-to-line-end",
    "delete-to-line-start",
    "backspace",
    "delete",
    "newline",
    "undo",
    "redo",
    "word-forward",
    "word-backward",
    "select-word-forward",
    "select-word-backward",
    "delete-word-forward",
    "delete-word-backward",
    "select-all",
    "submit",
];

/// `editBufferCommandNames` (lines 80-117), positionally parallel to
/// [`EDIT_BUFFER_ACTIONS`].
pub const EDIT_BUFFER_COMMAND_IDS: &[&str] = &[
    "input.move.left",
    "input.move.right",
    "input.move.up",
    "input.move.down",
    "input.select.left",
    "input.select.right",
    "input.select.up",
    "input.select.down",
    "input.line.home",
    "input.line.end",
    "input.select.line.home",
    "input.select.line.end",
    "input.visual.line.home",
    "input.visual.line.end",
    "input.select.visual.line.home",
    "input.select.visual.line.end",
    "input.buffer.home",
    "input.buffer.end",
    "input.select.buffer.home",
    "input.select.buffer.end",
    "input.delete.line",
    "input.delete.to.line.end",
    "input.delete.to.line.start",
    "input.backspace",
    "input.delete",
    "input.newline",
    "input.undo",
    "input.redo",
    "input.word.forward",
    "input.word.backward",
    "input.select.word.forward",
    "input.select.word.backward",
    "input.delete.word.forward",
    "input.delete.word.backward",
    "input.select.all",
    "input.submit",
];

/// `editBufferCommandDescriptions` (lines 119-156), parallel.
pub const EDIT_BUFFER_COMMAND_DESCRIPTIONS: &[&str] = &[
    "Cursor left",
    "Cursor right",
    "Cursor up",
    "Cursor down",
    "Select left",
    "Select right",
    "Select up",
    "Select down",
    "Line start",
    "Line end",
    "Select to line start",
    "Select to line end",
    "Visual line start",
    "Visual line end",
    "Select to visual line start",
    "Select to visual line end",
    "Buffer start",
    "Buffer end",
    "Select to buffer start",
    "Select to buffer end",
    "Delete line",
    "Delete to line end",
    "Delete to line start",
    "Delete backward",
    "Delete forward",
    "New line",
    "Undo",
    "Redo",
    "Next word",
    "Previous word",
    "Select next word",
    "Select previous word",
    "Delete next word",
    "Delete previous word",
    "Select all",
    "Submit",
];

/// `editBufferFineGroups` (lines 158-195), parallel.
pub const EDIT_BUFFER_FINE_GROUPS: &[&str] = &[
    "Cursor",
    "Cursor",
    "Cursor",
    "Cursor",
    "Selection",
    "Selection",
    "Selection",
    "Selection",
    "Cursor",
    "Cursor",
    "Selection",
    "Selection",
    "Cursor",
    "Cursor",
    "Selection",
    "Selection",
    "Cursor",
    "Cursor",
    "Selection",
    "Selection",
    "Delete",
    "Delete",
    "Delete",
    "Delete",
    "Delete",
    "Insert",
    "History",
    "History",
    "Cursor",
    "Cursor",
    "Selection",
    "Selection",
    "Delete",
    "Delete",
    "Selection",
    "Submit",
];

/// `DEFAULT_EDIT_BUFFER_METADATA_GROUP` (line 78).
pub const DEFAULT_EDIT_BUFFER_METADATA_GROUP: &str = "Text Editing";

/// One entry of `@opentui/core`'s `defaultTextareaKeyBindings`
/// (core/src/renderables/Textarea.ts:58-124).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultKeyBinding {
    pub name: &'static str,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
    pub action: &'static str,
}

const fn key(name: &'static str, action: &'static str) -> DefaultKeyBinding {
    DefaultKeyBinding {
        name,
        ctrl: false,
        shift: false,
        meta: false,
        super_key: false,
        action,
    }
}

const fn key_mod(
    name: &'static str,
    ctrl: bool,
    shift: bool,
    meta: bool,
    super_key: bool,
    action: &'static str,
) -> DefaultKeyBinding {
    DefaultKeyBinding {
        name,
        ctrl,
        shift,
        meta,
        super_key,
        action,
    }
}

/// `defaultTextareaKeyBindings` (Textarea.ts:58-124), order-exact.
pub const DEFAULT_TEXTAREA_KEY_BINDINGS: &[DefaultKeyBinding] = &[
    key("left", "move-left"),
    key("right", "move-right"),
    key("up", "move-up"),
    key("down", "move-down"),
    key_mod("left", false, true, false, false, "select-left"),
    key_mod("right", false, true, false, false, "select-right"),
    key_mod("up", false, true, false, false, "select-up"),
    key_mod("down", false, true, false, false, "select-down"),
    key("home", "buffer-home"),
    key("end", "buffer-end"),
    key_mod("home", false, true, false, false, "select-buffer-home"),
    key_mod("end", false, true, false, false, "select-buffer-end"),
    key_mod("a", true, false, false, false, "line-home"),
    key_mod("e", true, false, false, false, "line-end"),
    key_mod("a", true, true, false, false, "select-line-home"),
    key_mod("e", true, true, false, false, "select-line-end"),
    key_mod("a", false, false, true, false, "visual-line-home"),
    key_mod("e", false, false, true, false, "visual-line-end"),
    key_mod("a", false, true, true, false, "select-visual-line-home"),
    key_mod("e", false, true, true, false, "select-visual-line-end"),
    key_mod("f", true, false, false, false, "move-right"),
    key_mod("b", true, false, false, false, "move-left"),
    key_mod("w", true, false, false, false, "delete-word-backward"),
    key_mod(
        "backspace",
        true,
        false,
        false,
        false,
        "delete-word-backward",
    ),
    key_mod("d", false, false, true, false, "delete-word-forward"),
    key_mod("delete", false, false, true, false, "delete-word-forward"),
    key_mod("delete", true, false, false, false, "delete-word-forward"),
    key_mod("d", true, true, false, false, "delete-line"),
    key_mod("k", true, false, false, false, "delete-to-line-end"),
    key_mod("u", true, false, false, false, "delete-to-line-start"),
    key("backspace", "backspace"),
    key_mod("backspace", false, true, false, false, "backspace"),
    key_mod("d", true, false, false, false, "delete"),
    key("delete", "delete"),
    key_mod("delete", false, true, false, false, "delete"),
    key("return", "newline"),
    key("kpenter", "newline"),
    key("linefeed", "newline"),
    key_mod("return", false, false, true, false, "submit"),
    key_mod("kpenter", false, false, true, false, "submit"),
    // undo/redo
    key_mod("-", true, false, false, false, "undo"),
    key_mod(".", true, false, false, false, "redo"),
    key_mod("z", false, false, false, true, "undo"),
    key_mod("z", false, true, false, true, "redo"),
    key_mod("f", false, false, true, false, "word-forward"),
    key_mod("b", false, false, true, false, "word-backward"),
    key_mod("right", false, false, true, false, "word-forward"),
    key_mod("left", false, false, true, false, "word-backward"),
    key_mod("right", true, false, false, false, "word-forward"),
    key_mod("left", true, false, false, false, "word-backward"),
    key_mod("f", false, true, true, false, "select-word-forward"),
    key_mod("b", false, true, true, false, "select-word-backward"),
    key_mod("right", false, true, true, false, "select-word-forward"),
    key_mod("left", false, true, true, false, "select-word-backward"),
    key_mod(
        "backspace",
        false,
        false,
        true,
        false,
        "delete-word-backward",
    ),
    // super (cmd/win) + arrow keys for Kitty Keyboard mode
    key_mod("left", false, false, false, true, "visual-line-home"),
    key_mod("right", false, false, false, true, "visual-line-end"),
    key_mod("up", false, false, false, true, "buffer-home"),
    key_mod("down", false, false, false, true, "buffer-end"),
    key_mod("left", false, true, false, true, "select-visual-line-home"),
    key_mod("right", false, true, false, true, "select-visual-line-end"),
    key_mod("up", false, true, false, true, "select-buffer-home"),
    key_mod("down", false, true, false, true, "select-buffer-end"),
    key_mod("a", false, false, false, true, "select-all"),
];

/// `keyBindingToString` (edit-buffer-bindings.ts:22-33):
/// ctrl→shift→meta→super order, `+`-joined.
#[must_use]
pub fn key_binding_to_string(binding: &DefaultKeyBinding) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if binding.ctrl {
        parts.push("ctrl");
    }
    if binding.shift {
        parts.push("shift");
    }
    if binding.meta {
        parts.push("meta");
    }
    if binding.super_key {
        parts.push("super");
    }
    parts.push(binding.name);
    parts.join("+")
}

/// `EditBufferCommandOptions` (edit-buffer-bindings.ts:203-209).
#[derive(Debug, Clone, Default)]
pub struct EditBufferCommandOptions {
    pub category: Option<String>,
    pub group: Option<String>,
    pub include_fine_group: bool,
    /// Partial action → command id overrides (validated unique,
    /// lines 287-299).
    pub command_names: BTreeMap<String, String>,
    /// Partial action → description overrides.
    pub descriptions: BTreeMap<String, String>,
}

#[derive(Clone)]
struct ResolvedOptions {
    command_names: BTreeMap<String, String>,
    descriptions: BTreeMap<String, String>,
    category: String,
    group: String,
    include_fine_group: bool,
}

/// `resolveEditBufferCommandNames` (lines 267-302) +
/// `resolveEditBufferCommandDescriptions` (lines 224-248) +
/// `resolveEditBufferMetadataOptions` (lines 259-265).
fn resolve_options(options: &EditBufferCommandOptions) -> Result<ResolvedOptions, String> {
    let mut command_names: BTreeMap<String, String> = BTreeMap::new();
    for (index, action) in EDIT_BUFFER_ACTIONS.iter().enumerate() {
        command_names.insert(
            (*action).to_owned(),
            EDIT_BUFFER_COMMAND_IDS[index].to_owned(),
        );
    }
    for action in EDIT_BUFFER_ACTIONS {
        if let Some(raw) = options.command_names.get(*action) {
            let normalized = raw.trim();
            if normalized.is_empty() {
                return Err(format!(
                    "Edit buffer command name for \"{action}\" cannot be empty"
                ));
            }
            command_names.insert((*action).to_owned(), normalized.to_owned());
        }
    }
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for action in EDIT_BUFFER_ACTIONS {
        let command_name = command_names[*action].clone();
        if let Some(existing) = seen.get(&command_name) {
            return Err(format!(
                "Edit buffer command name \"{command_name}\" is assigned to both \"{existing}\" and \"{action}\""
            ));
        }
        seen.insert(command_name, (*action).to_owned());
    }
    let mut descriptions: BTreeMap<String, String> = BTreeMap::new();
    for (index, action) in EDIT_BUFFER_ACTIONS.iter().enumerate() {
        descriptions.insert(
            (*action).to_owned(),
            EDIT_BUFFER_COMMAND_DESCRIPTIONS[index].to_owned(),
        );
    }
    for action in EDIT_BUFFER_ACTIONS {
        if let Some(raw) = options.descriptions.get(*action) {
            let normalized = raw.trim();
            if normalized.is_empty() {
                return Err(format!(
                    "Edit buffer command description for \"{action}\" cannot be empty"
                ));
            }
            descriptions.insert((*action).to_owned(), normalized.to_owned());
        }
    }
    Ok(ResolvedOptions {
        command_names,
        descriptions,
        category: options
            .category
            .as_deref()
            .map_or_else(|| DEFAULT_EDIT_BUFFER_METADATA_GROUP, str::trim)
            .trim()
            .to_owned(),
        group: options
            .group
            .as_deref()
            .map_or_else(|| DEFAULT_EDIT_BUFFER_METADATA_GROUP, str::trim)
            .trim()
            .to_owned(),
        include_fine_group: options.include_fine_group,
    })
}

fn fine_group(action: &str) -> String {
    let index = EDIT_BUFFER_ACTIONS
        .iter()
        .position(|a| *a == action)
        .unwrap_or(0);
    EDIT_BUFFER_FINE_GROUPS[index].to_owned()
}

/// `createDefaultTextareaBindings` (lines 315-334): the core table
/// mapped to bindings with command id, description, and group (+
/// optional fine group).
fn create_default_bindings(resolved: &ResolvedOptions) -> Vec<Binding> {
    DEFAULT_TEXTAREA_KEY_BINDINGS
        .iter()
        .map(|key| {
            let action = key.action;
            let mut binding = Binding::new(
                key_binding_to_string(key),
                Some(BindingCommand::Name(resolved.command_names[action].clone())),
            )
            .with_attr(
                "desc",
                serde_json::Value::String(resolved.descriptions[action].clone()),
            )
            .with_attr("group", serde_json::Value::String(resolved.group.clone()));
            if resolved.include_fine_group {
                binding =
                    binding.with_attr("fineGroup", serde_json::Value::String(fine_group(action)));
            }
            binding
        })
        .collect()
}

/// `createTextareaBindings` (lines 341-358): overrides PREPENDED so
/// they take precedence, then the defaults.
pub fn create_textarea_bindings(
    overrides: Option<Vec<Binding>>,
    options: Option<&EditBufferCommandOptions>,
) -> Result<Vec<Binding>, String> {
    let resolved = resolve_options(&options.cloned().unwrap_or_default())?;
    let mut bindings = overrides.unwrap_or_default();
    bindings.extend(create_default_bindings(&resolved));
    Ok(bindings)
}

/// Records resolved edit-buffer actions — the Rust binding of
/// `renderer.currentFocusedEditor` observation (hosts apply actions
/// to their live textarea; tests assert the sequence).
pub type ActionSink = Arc<Mutex<Vec<String>>>;

/// `createEditBufferCommands` (lines 509-518) + `registerEditBuffer-
/// Commands` (lines 526-540): one command per action,
/// reference-counted per keymap under
/// [`EDIT_BUFFER_COMMANDS_RESOURCE`].
pub fn register_edit_buffer_commands(
    keymap: &mut crate::Keymap,
    options: Option<&EditBufferCommandOptions>,
) -> (Registration, ActionSink) {
    let resolved = resolve_options(&options.cloned().unwrap_or_default())
        .expect("edit buffer command options");
    let sink: ActionSink = Arc::new(Mutex::new(Vec::new()));
    let registration = keymap
        .engine
        .acquire_resource_with_key(EDIT_BUFFER_COMMANDS_RESOURCE, {
            let resolved = resolved.clone();
            let sink = sink.clone();
            move |engine| {
                let commands: Vec<Command> = EDIT_BUFFER_ACTIONS
                    .iter()
                    .map(|action| {
                        let sink = sink.clone();
                        let mut command = Command::new(
                            &resolved.command_names[*action],
                            Box::new(move |_ctx| {
                                sink.lock()
                                    .map(|mut sink| sink.push((*action).to_owned()))
                                    .ok();
                                CommandResult::Handled
                            }),
                        )
                        .with_field(
                            "desc",
                            serde_json::Value::String(resolved.descriptions[*action].clone()),
                        )
                        .with_field(
                            "category",
                            serde_json::Value::String(resolved.category.clone()),
                        );
                        if resolved.include_fine_group {
                            command = command.with_field(
                                "fineGroup",
                                serde_json::Value::String(fine_group(action)),
                            );
                        }
                        command
                    })
                    .collect();
                vec![engine.register_layer(Layer {
                    commands,
                    ..Layer::default()
                })]
            }
        });
    (registration, sink)
}

/// `registerTextareaMappingSuspension` (lines 374-431): suspends the
/// focused textarea's own key handling so keymap bindings take over.
/// The port installs the reference-counted resource; the renderer
/// trait-suspension is the host's side of the contract.
pub fn register_textarea_mapping_suspension(keymap: &mut crate::Keymap) -> Registration {
    keymap
        .engine
        .acquire_resource_with_key(TEXTAREA_MAPPING_SUSPENSION_RESOURCE, |_| Vec::new())
}

/// `ManagedTextareaLayer` (lines 214-218): a layer minus target
/// scoping — managed textarea layers are always global.
pub struct ManagedTextareaLayer {
    pub bindings: Vec<Binding>,
    pub priority: i32,
    pub mode: Option<String>,
}

/// `registerManagedTextareaLayer` (lines 548-577): registers the
/// edit-buffer commands, the mapping suspension, and the layer with
/// default bindings plus overrides — reference-counted pieces, safe
/// to combine with the lower-level helpers. The combined disposer
/// releases layer → suspension → commands (lines 567-571).
pub fn register_managed_textarea_layer(
    keymap: &mut crate::Keymap,
    layer: ManagedTextareaLayer,
    options: Option<&EditBufferCommandOptions>,
) -> (Registration, ActionSink) {
    let (commands, sink) = register_edit_buffer_commands(keymap, options);
    let suspension = register_textarea_mapping_suspension(keymap);
    let resolved = resolve_options(&options.cloned().unwrap_or_default())
        .expect("edit buffer command options");
    let mut bindings = layer.bindings;
    bindings.extend(create_default_bindings(&resolved));
    let layer_registration = keymap.engine.register_layer(Layer {
        bindings,
        priority: layer.priority,
        mode: layer.mode,
        ..Layer::default()
    });
    (
        Registration::Composite(vec![layer_registration, suspension, commands]),
        sink,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_parallel() {
        let n = EDIT_BUFFER_ACTIONS.len();
        assert_eq!(EDIT_BUFFER_COMMAND_IDS.len(), n);
        assert_eq!(EDIT_BUFFER_COMMAND_DESCRIPTIONS.len(), n);
        assert_eq!(EDIT_BUFFER_FINE_GROUPS.len(), n);
    }

    #[test]
    fn create_textarea_bindings_prepends_overrides() {
        // edit-buffer-bindings.test.ts "createTextareaBindings
        // prepends override-style bindings ahead of textarea defaults".
        let bindings = create_textarea_bindings(
            Some(vec![
                Binding::new("left", Some(BindingCommand::Name("custom-left".into()))),
                Binding::new("dd", Some(BindingCommand::Name("input.delete.line".into()))),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(bindings[0].key, crate::types::KeyLike::Str("left".into()));
        assert_eq!(
            bindings[0].cmd,
            Some(BindingCommand::Name("custom-left".into()))
        );
        assert_eq!(bindings[1].key, crate::types::KeyLike::Str("dd".into()));
        assert!(bindings.iter().any(|b| matches!(&b.key,
            crate::types::KeyLike::Str(key) if key == "right"
                && b.cmd == Some(BindingCommand::Name("input.move.right".into())))));
        assert!(bindings.iter().any(|b| matches!(&b.key,
            crate::types::KeyLike::Str(key) if key == "left"
                && b.cmd == Some(BindingCommand::Name("input.move.left".into())))));
        let backspace = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "backspace"))
            .unwrap();
        assert_eq!(
            backspace.attrs.get("desc"),
            Some(&serde_json::Value::String("Delete backward".into()))
        );
    }

    #[test]
    fn metadata_and_fine_groups_and_custom_names() {
        // The three createTextareaBindings metadata tests.
        let bindings = create_textarea_bindings(None, None).unwrap();
        let left = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "left"))
            .unwrap();
        assert_eq!(
            left.attrs.get("group"),
            Some(&serde_json::Value::String("Text Editing".into()))
        );
        assert!(!left.attrs.contains_key("fineGroup"));

        let options = EditBufferCommandOptions {
            include_fine_group: true,
            group: Some("Editor".into()),
            ..EditBufferCommandOptions::default()
        };
        let bindings = create_textarea_bindings(None, Some(&options)).unwrap();
        let left = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "left"))
            .unwrap();
        assert_eq!(
            left.attrs.get("group"),
            Some(&serde_json::Value::String("Editor".into()))
        );
        assert_eq!(
            left.attrs.get("fineGroup"),
            Some(&serde_json::Value::String("Cursor".into()))
        );
        let backspace = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "backspace"))
            .unwrap();
        assert_eq!(
            backspace.attrs.get("fineGroup"),
            Some(&serde_json::Value::String("Delete".into()))
        );
        let undo = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "ctrl+-"))
            .unwrap();
        assert_eq!(
            undo.attrs.get("fineGroup"),
            Some(&serde_json::Value::String("History".into()))
        );
        let newline = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "return"))
            .unwrap();
        assert_eq!(
            newline.attrs.get("fineGroup"),
            Some(&serde_json::Value::String("Insert".into()))
        );
        let submit = bindings
            .iter()
            .find(|b| matches!(&b.key, crate::types::KeyLike::Str(key) if key == "meta+return"))
            .unwrap();
        assert_eq!(
            submit.attrs.get("fineGroup"),
            Some(&serde_json::Value::String("Submit".into()))
        );

        let mut options = EditBufferCommandOptions::default();
        options
            .command_names
            .insert("move-left".into(), "input_move_left".into());
        options
            .command_names
            .insert("submit".into(), "input_submit".into());
        let bindings = create_textarea_bindings(None, Some(&options)).unwrap();
        assert!(bindings.iter().any(|b| matches!(&b.key,
            crate::types::KeyLike::Str(key) if key == "left"
                && b.cmd == Some(BindingCommand::Name("input_move_left".into())))));
        assert!(!bindings.iter().any(|b| matches!(&b.key,
            crate::types::KeyLike::Str(key) if key == "left"
                && b.cmd == Some(BindingCommand::Name("input.move.left".into())))));
        assert!(bindings.iter().any(|b| matches!(&b.key,
            crate::types::KeyLike::Str(key) if key == "meta+return"
                && b.cmd == Some(BindingCommand::Name("input_submit".into())))));
        // Duplicate command ids rejected (lines 287-299).
        let mut options = EditBufferCommandOptions::default();
        options
            .command_names
            .insert("move-left".into(), "input.move.right".into());
        assert!(create_textarea_bindings(None, Some(&options)).is_err());
    }
}
