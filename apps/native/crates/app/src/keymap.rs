//! Keybindings: the web's `DEFAULT_KEYBINDINGS` registered as GPUI bindings. Each `when` clause
//! becomes a GPUI predicate over flat flags that the workspace root sets (`root_context`) or that
//! focused views carry (`Input` for any text field). Commands without a native surface yet bind
//! to `Unimplemented`, which lets the key fall through; the ledger lists them.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, KeyContext, actions};
use t3_core::keybindings::{
    KeybindingShortcut, KeybindingWhenNode, ResolvedKeybindingRule, ShortcutMatchContext, decode_resolved_keybindings, default_resolved_keybindings,
    gpui_keystrokes, gpui_predicate, merge_with_default_keybindings, shifts_with_composer_modifier, shortcut_label_for_command,
};

actions!(
    t3_keys,
    [
        ToggleSidebar,
        FocusNextWorkspaceArea,
        FocusPreviousWorkspaceArea,
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
        ToggleFilePicker,
        NavigateBack,
        NavigateForward,
        ToggleProjectSearch,
        SelectTheme,
        CycleAppearance,
        StopThread,
        RegenerateThreadTitle,
        SendBackground,
        Unimplemented,
    ]
);

pub fn action_for(command: &str) -> Box<dyn Action> {
    match command {
        "themeEditor.toggle" => Box::new(crate::theme_editor::ToggleThemeEditor),
        "theme.select" => Box::new(SelectTheme),
        "appearance.cycle" => Box::new(CycleAppearance),
        "sidebar.toggle" => Box::new(ToggleSidebar),
        "workspace.focusNext" => Box::new(FocusNextWorkspaceArea),
        "workspace.focusPrevious" => Box::new(FocusPreviousWorkspaceArea),
        "thread.stop" => Box::new(StopThread),
        "thread.regenerateTitle" => Box::new(RegenerateThreadTitle),
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
        "filePicker.toggle" => Box::new(ToggleFilePicker),
        "navigation.back" => Box::new(NavigateBack),
        "navigation.forward" => Box::new(NavigateForward),
        "projectSearch.toggle" => Box::new(ToggleProjectSearch),
        "chat.new" | "chat.newLocal" => Box::new(crate::new_thread::NewThread),
        "chat.newWithoutProject" => Box::new(crate::new_thread::NewThreadWithoutProject),
        "chat.find" => Box::new(crate::thread_view::FindInThread),
        "modelPicker.toggle" | "composer.effort" => Box::new(crate::composer::ToggleModelPicker),
        "composer.workspace" => Box::new(crate::composer::OpenWorkspace),
        "composer.host" => Box::new(crate::composer::OpenHost),
        "composer.cycleHost" => Box::new(crate::composer::CycleHost),
        "composer.previousWorktree" => Box::new(crate::composer::UsePreviousWorktree),
        "composer.mode" => Box::new(crate::composer::OpenPermissions),
        "composer.stash" => Box::new(crate::composer::StashPrompt),
        "composer.sendAlternate" => Box::new(crate::composer::SendAlternate),
        "composer.sendBackground" => Box::new(SendBackground),
        "composer.dictation" => Box::new(crate::voice::Dictation),
        "thread.steerQueuedMessage" => Box::new(crate::composer::SteerQueuedMessage),
        "thread.editQueuedMessage" => Box::new(crate::composer::EditQueuedMessage),
        "rightPanel.toggle" => Box::new(crate::right_panel::ToggleRightPanel),
        "rightPanel.toggleMaximized" => Box::new(crate::right_panel::ToggleMaximized),
        "rightPanel.close" => Box::new(crate::right_panel::CloseActiveSurface),
        "view.reopenClosed" => Box::new(crate::right_panel::ReopenClosedView),
        "diff.toggle" => Box::new(crate::right_panel::ToggleDiff),
        "preview.toggle" => Box::new(crate::preview::TogglePreview),
        "preview.refresh" => Box::new(crate::preview::Refresh),
        "preview.focusUrl" => Box::new(crate::preview::FocusUrl),
        "preview.zoomIn" => Box::new(crate::preview::ZoomIn),
        "preview.zoomOut" => Box::new(crate::preview::ZoomOut),
        "preview.resetZoom" => Box::new(crate::preview::ResetZoom),
        "pullRequest.copyNumber" => Box::new(crate::pull_requests::CopyPullRequestNumber),
        "terminal.toggle" => Box::new(crate::terminal::ToggleTerminal),
        "terminal.split" => Box::new(crate::terminal::SplitTerminal),
        "terminal.splitVertical" => Box::new(crate::terminal::SplitTerminalVertical),
        "terminal.new" => Box::new(crate::terminal::NewTerminal),
        "terminal.close" => Box::new(crate::terminal::CloseTerminal),
        "threadPanel.toggle" => Box::new(crate::thread_details::ToggleThreadPanel),
        "editor.openFavorite" => Box::new(crate::thread_details::OpenFavoriteEditor),
        "usage.open" => Box::new(crate::usage::OpenUsage),
        "usage.cost" => Box::new(crate::usage::ShowCost),
        "usage.tokens" => Box::new(crate::usage::ShowTokens),
        "usage.limits" => Box::new(crate::usage::ShowLimits),
        "usage.period.day" => Box::new(crate::usage::PeriodDay),
        "usage.period.week" => Box::new(crate::usage::PeriodWeek),
        "usage.period.month" => Box::new(crate::usage::PeriodMonth),
        "usage.period.quarter" => Box::new(crate::usage::PeriodQuarter),
        other => match t3_core::project_scripts::project_script_id_from_command(other) {
            Some(script_id) => Box::new(crate::thread_details::RunProjectScript { script_id: script_id.to_owned() }),
            None => Box::new(Unimplemented),
        },
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
    pub revision: usize,
    source: serde_json::Value,
    installed: HashSet<BindingKey>,
}

/// Keystroke, GPUI predicate, command.
type BindingKey = (String, Option<String>, String);

impl Global for Keymap {}

fn and_when(base: Option<KeybindingWhenNode>, extra: KeybindingWhenNode) -> KeybindingWhenNode {
    match base {
        Some(left) => KeybindingWhenNode::And { left: Box::new(left), right: Box::new(extra) },
        None => extra,
    }
}

fn binding_keys(rules: &[ResolvedKeybindingRule]) -> Vec<BindingKey> {
    let mac = cfg!(target_os = "macos");
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    let mut push = |shortcut: &KeybindingShortcut, predicate: Option<String>, command: &str| {
        for keystroke in gpui_keystrokes(shortcut, mac) {
            let key = (keystroke, predicate.clone(), command.to_owned());
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }
    };
    for rule in rules {
        let renamed = rule.when_ast.as_ref().map(rename);
        if shifts_with_composer_modifier(rule) {
            let modifier_flag = KeybindingWhenNode::Identifier { name: "composerRequiresModifier".to_owned() };
            let unshifted_pred = gpui_predicate(&and_when(
                renamed.clone(),
                KeybindingWhenNode::Not { node: Box::new(modifier_flag.clone()) },
            ));
            let shifted_pred = gpui_predicate(&and_when(renamed, modifier_flag));
            push(&rule.shortcut, Some(unshifted_pred), &rule.command);
            let shifted = KeybindingShortcut { shift_key: true, ..rule.shortcut.clone() };
            push(&shifted, Some(shifted_pred), &rule.command);
        } else {
            let predicate = renamed.as_ref().map(gpui_predicate);
            push(&rule.shortcut, predicate, &rule.command);
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
    let revision = cx.try_global::<Keymap>().map_or(0, |map| map.revision.wrapping_add(1));
    cx.set_global(Keymap { rules, revision, source: server_keybindings.clone(), installed });
}

/// The display label for a command's effective shortcut, such as `⌘B`.
pub fn label(command: &str, cx: &App) -> Option<String> {
    label_in(command, &ShortcutMatchContext::default(), cx)
}

/// The display label for a command's effective shortcut under an explicit `when` context.
pub fn label_in(command: &str, context: &ShortcutMatchContext, cx: &App) -> Option<String> {
    let rules = cx.try_global::<Keymap>().map_or(default_resolved_keybindings(), |keymap| keymap.rules.as_slice());
    shortcut_label_for_command(rules, command, context, cfg!(target_os = "macos"))
}

/// Flags app state contributes to `when` clauses, set on the workspace root.
#[cfg_attr(not(test), expect(dead_code))]
pub fn root_context(right_panel_open: bool, turn_running: bool) -> KeyContext {
    root_context_with(right_panel_open, turn_running, false, false, false)
}

/// Full workspace `when` flags: right panel, running turn, open terminal/preview, and draft route.
#[cfg_attr(not(test), expect(dead_code))]
pub fn root_context_with(
    right_panel_open: bool,
    turn_running: bool,
    terminal_open: bool,
    preview_open: bool,
    draft_thread_route: bool,
) -> KeyContext {
    root_context_full(right_panel_open, turn_running, terminal_open, preview_open, draft_thread_route, false)
}

/// Full workspace `when` flags including `composerRequiresModifier`.
pub fn root_context_full(
    right_panel_open: bool,
    turn_running: bool,
    terminal_open: bool,
    preview_open: bool,
    draft_thread_route: bool,
    composer_requires_modifier: bool,
) -> KeyContext {
    let mut context = KeyContext::new_with_defaults();
    context.add("Workspace");
    context.add("isDesktop");
    if right_panel_open {
        context.add("rightPanelOpen");
    }
    if turn_running {
        context.add("turnRunning");
    }
    if terminal_open {
        context.add("terminalOpen");
    }
    if preview_open {
        context.add("previewOpen");
    }
    if draft_thread_route {
        context.add("draftThreadRoute");
    }
    if composer_requires_modifier {
        context.add("composerRequiresModifier");
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;

    #[test]
    fn action_for_maps_theme_appearance_thread_and_chat_new_local() {
        assert_eq!(action_for("theme.select").as_any().type_id(), TypeId::of::<SelectTheme>());
        assert_eq!(action_for("appearance.cycle").as_any().type_id(), TypeId::of::<CycleAppearance>());
        assert_eq!(action_for("thread.stop").as_any().type_id(), TypeId::of::<StopThread>());
        assert_eq!(action_for("thread.regenerateTitle").as_any().type_id(), TypeId::of::<RegenerateThreadTitle>());
        assert_eq!(action_for("chat.newLocal").as_any().type_id(), TypeId::of::<crate::new_thread::NewThread>());
        assert_eq!(action_for("workspace.focusNext").as_any().type_id(), TypeId::of::<FocusNextWorkspaceArea>());
        assert_eq!(action_for("workspace.focusPrevious").as_any().type_id(), TypeId::of::<FocusPreviousWorkspaceArea>());
        assert_eq!(action_for("composer.sendBackground").as_any().type_id(), TypeId::of::<SendBackground>());
    }

    #[test]
    fn root_context_with_sets_expected_when_flags() {
        assert!(root_context(true, false).contains("rightPanelOpen"));
        let ctx = root_context_with(true, true, true, true, true);
        for flag in ["Workspace", "isDesktop", "rightPanelOpen", "turnRunning", "terminalOpen", "previewOpen", "draftThreadRoute"] {
            assert!(ctx.contains(flag), "missing flag {flag}");
        }
        assert!(!ctx.contains("composerRequiresModifier"));
        let full = root_context_full(true, true, true, true, true, true);
        assert!(full.contains("composerRequiresModifier"));
    }

    #[test]
    fn binding_keys_splits_mod_enter_by_composer_requires_modifier() {
        let keys = binding_keys(default_resolved_keybindings());
        let bg_keys: Vec<_> = keys.iter().filter(|(_, _, cmd)| cmd == "composer.sendBackground").collect();
        let mod_enter = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };
        let mod_shift_enter = if cfg!(target_os = "macos") { "cmd-shift-enter" } else { "ctrl-shift-enter" };
        assert!(bg_keys.iter().any(|(k, pred, _)| k == mod_enter && pred.as_deref().is_some_and(|p| p.contains("!(!(!composerRequiresModifier))"))));
        assert!(bg_keys.iter().any(|(k, pred, _)| k == mod_shift_enter && pred.as_deref().is_some_and(|p| p.contains("!(!composerRequiresModifier)"))));
    }
}
