//! Keybindings: the web's `DEFAULT_KEYBINDINGS` registered as GPUI bindings. Each `when` clause
//! becomes a GPUI predicate over flat flags that the workspace root sets (`root_context`) or that
//! focused views carry (`Input` for any text field). Commands without a native surface yet bind
//! to `Unimplemented`, which lets the key fall through; the ledger lists them.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, KeyContext, actions};
use t3_core::keybindings::{
    KeybindingWhenNode, ResolvedKeybindingRule, ShortcutMatchContext, decode_resolved_keybindings, default_resolved_keybindings, gpui_keystrokes,
    gpui_predicate, merge_with_default_keybindings, shortcut_label_for_command,
};

actions!(
    t3_keys,
    [
        ToggleSidebar,
        PreviousThread,
        NextThread,
        JumpToThread1,
        JumpToThread2,
        JumpToThread3,
        JumpToThread4,
        JumpToThread5,
        JumpToThread6,
        JumpToThread7,
        JumpToThread8,
        JumpToThread9,
        SettleThread,
        PinThread,
        UndoThreadAction,
        CopyThreadReference,
        ToggleCommandPalette,
        Unimplemented,
    ]
);

pub fn action_for(command: &str) -> Box<dyn Action> {
    match command {
        "sidebar.toggle" => Box::new(ToggleSidebar),
        "thread.previous" => Box::new(PreviousThread),
        "thread.next" => Box::new(NextThread),
        "thread.jump.1" => Box::new(JumpToThread1),
        "thread.jump.2" => Box::new(JumpToThread2),
        "thread.jump.3" => Box::new(JumpToThread3),
        "thread.jump.4" => Box::new(JumpToThread4),
        "thread.jump.5" => Box::new(JumpToThread5),
        "thread.jump.6" => Box::new(JumpToThread6),
        "thread.jump.7" => Box::new(JumpToThread7),
        "thread.jump.8" => Box::new(JumpToThread8),
        "thread.jump.9" => Box::new(JumpToThread9),
        "thread.settle" => Box::new(SettleThread),
        "thread.pin" => Box::new(PinThread),
        "thread.undo" => Box::new(UndoThreadAction),
        "thread.copyReference" => Box::new(CopyThreadReference),
        "commandPalette.toggle" => Box::new(ToggleCommandPalette),
        "chat.new" => Box::new(crate::new_thread::NewThread),
        "modelPicker.toggle" | "composer.effort" => Box::new(crate::composer::ToggleModelPicker),
        "composer.mode" => Box::new(crate::composer::OpenPermissions),
        _ => Box::new(Unimplemented),
    }
}

/// Web `when` identifiers that are focus states map to the GPUI context of the focused view.
fn context_name(identifier: &str) -> &str {
    match identifier {
        "editableFocus" => "Input",
        "composerFocus" => "Composer",
        "terminalFocus" => "Terminal",
        "previewFocus" => "Preview",
        "modelPickerOpen" => "ModelPicker",
        other => other,
    }
}

fn rename(node: &KeybindingWhenNode) -> KeybindingWhenNode {
    match node {
        KeybindingWhenNode::Identifier { name } => KeybindingWhenNode::Identifier { name: context_name(name).to_owned() },
        KeybindingWhenNode::Not { node } => KeybindingWhenNode::Not { node: Box::new(rename(node)) },
        KeybindingWhenNode::And { left, right } => KeybindingWhenNode::And { left: Box::new(rename(left)), right: Box::new(rename(right)) },
        KeybindingWhenNode::Or { left, right } => KeybindingWhenNode::Or { left: Box::new(rename(left)), right: Box::new(rename(right)) },
    }
}

/// The resolved rules in effect: defaults merged with the server's user keybindings.
pub struct Keymap {
    pub rules: Vec<ResolvedKeybindingRule>,
    source: serde_json::Value,
    installed: HashSet<BindingKey>,
}

/// Keystroke, GPUI predicate, command.
type BindingKey = (String, Option<String>, String);

impl Global for Keymap {}

fn binding_keys(rules: &[ResolvedKeybindingRule]) -> Vec<BindingKey> {
    let mac = cfg!(target_os = "macos");
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    for rule in rules {
        let predicate = rule.when_ast.as_ref().map(|when| gpui_predicate(&rename(when)));
        for keystroke in gpui_keystrokes(&rule.shortcut, mac) {
            let key = (keystroke, predicate.clone(), rule.command.clone());
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }
    }
    keys
}

fn load((keystroke, predicate, command): &BindingKey, action: Box<dyn Action>) -> Option<KeyBinding> {
    let context = match predicate.as_deref().map(KeyBindingContextPredicate::parse).transpose() {
        Ok(context) => context.map(Rc::new),
        Err(error) => {
            tracing::warn!(%command, %error, "skipped keybinding with an unparseable when clause");
            return None;
        }
    };
    KeyBinding::load(keystroke, action, context, false, None, &DummyKeyboardMapper)
        .inspect_err(|error| tracing::warn!(%keystroke, %command, %error, "skipped keybinding"))
        .ok()
}

pub fn init(cx: &mut App) {
    install(&serde_json::Value::Null, cx);
}

/// Binds the defaults merged with the server's `keybindings` (from `subscribeServerConfig`) as
/// the web merges them. Later bindings win ties in GPUI, so a reinstall's overrides take effect.
pub fn install(server_keybindings: &serde_json::Value, cx: &mut App) {
    if cx.try_global::<Keymap>().is_some_and(|keymap| keymap.source == *server_keybindings) {
        return;
    }
    let rules = merge_with_default_keybindings(&decode_resolved_keybindings(server_keybindings));
    let keys = binding_keys(&rules);
    let installed: HashSet<BindingKey> = keys.iter().cloned().collect();
    // GPUI cannot remove a binding, so one a new set drops is shadowed by a later no-op that
    // lets the key through; the new set is bound after it and wins every tie.
    let dropped = cx.try_global::<Keymap>().map(|keymap| keymap.installed.difference(&installed).cloned().collect::<Vec<_>>()).unwrap_or_default();
    let shadows = dropped.iter().filter_map(|key| load(key, Box::new(Unimplemented)));
    let bindings: Vec<KeyBinding> = shadows.chain(keys.iter().filter_map(|key| load(key, action_for(&key.2)))).collect();
    cx.bind_keys(bindings);
    cx.set_global(Keymap { rules, source: server_keybindings.clone(), installed });
}

/// The display label for a command's effective shortcut, such as `⌘B`.
pub fn label(command: &str, cx: &App) -> Option<String> {
    let rules = cx.try_global::<Keymap>().map_or(default_resolved_keybindings(), |keymap| keymap.rules.as_slice());
    shortcut_label_for_command(rules, command, &ShortcutMatchContext::default(), cfg!(target_os = "macos"))
}

/// Flags app state contributes to `when` clauses, set on the workspace root.
pub fn root_context(right_panel_open: bool, turn_running: bool) -> KeyContext {
    let mut context = KeyContext::new_with_defaults();
    context.add("Workspace");
    context.add("isDesktop");
    if right_panel_open {
        context.add("rightPanelOpen");
    }
    if turn_running {
        context.add("turnRunning");
    }
    context
}
