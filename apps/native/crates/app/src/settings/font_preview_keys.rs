//! The local-only boundary for `SettingsFontPreviews.tsx`, before GPUI dispatches keybindings.

use gpui::{Action, App, Entity, EntityInputHandler, Focusable, Keystroke, Subscription};
use gpui_component::input::TextareaState;

use super::super::super::{
    CloseSettings, FocusNextInSettings, FocusPreviousInSettings, FocusSettingsSearch,
};
use super::terminal::TerminalPreview;

pub(super) const CONTEXT: &str = "TypographyPreview";

pub(super) fn install(
    prompt: Entity<TextareaState>,
    terminal: Entity<TerminalPreview>,
    cx: &mut App,
) -> Subscription {
    cx.intercept_keystrokes(move |event, window, cx| {
        if !event
            .context_stack
            .iter()
            .any(|context| context.contains(CONTEXT))
        {
            return;
        }
        let prompt_focused = prompt.read(cx).focus_handle(cx).is_focused(window);
        if !prompt_focused && !terminal.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let key = &event.keystroke;
        if let Some(action) = navigation(key) {
            window.dispatch_action(action, cx);
            cx.stop_propagation();
            return;
        }
        let bindings = window.possible_bindings_for_input(&[] as &[Keystroke]);
        let matching: Vec<_> = bindings
            .iter()
            .filter(|binding| {
                binding
                    .match_keystrokes(std::slice::from_ref(key))
                    .is_some()
            })
            .collect();
        if matching.is_empty() {
            return;
        }
        // Key listeners run after actions; dispatch only known local edits and stop every workspace fallback.
        if prompt_focused {
            if let Some(binding) = matching.iter().find(|binding| {
                binding.match_keystrokes(std::slice::from_ref(key)) == Some(false)
                    && local_edit(binding.action())
            }) {
                window.dispatch_action(binding.action().boxed_clone(), cx);
            } else if !key.modifiers.platform
                && !key.modifiers.control
                && !key.modifiers.alt
                && let Some(text) = &key.key_char
            {
                prompt.update(cx, |input, cx| {
                    input.replace_text_in_range(None, text, window, cx)
                });
            }
        } else {
            terminal.update(cx, |terminal, cx| terminal.bound_key(key, cx));
        }
        cx.stop_propagation();
    })
}

fn navigation(key: &Keystroke) -> Option<Box<dyn Action>> {
    let modifiers = key.modifiers;
    if !modifiers.platform && !modifiers.control && !modifiers.alt {
        match key.key.as_str() {
            "tab" if modifiers.shift => return Some(Box::new(FocusPreviousInSettings)),
            "tab" => return Some(Box::new(FocusNextInSettings)),
            "escape" if !modifiers.shift => return Some(Box::new(CloseSettings)),
            _ => {}
        }
    }
    (key.key == "f"
        && modifiers.platform
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.shift)
        .then(|| Box::new(FocusSettingsSearch) as Box<dyn Action>)
}

fn local_edit(action: &dyn Action) -> bool {
    let Some(name) = action.name().strip_prefix("input::") else {
        return false;
    };
    name.starts_with("Move")
        || name.starts_with("Select")
        || matches!(
            name,
            "Enter"
                | "Backspace"
                | "Delete"
                | "DeleteToBeginningOfLine"
                | "DeleteToEndOfLine"
                | "DeleteToPreviousWordStart"
                | "DeleteToNextWordEnd"
                | "Indent"
                | "Outdent"
                | "IndentInline"
                | "OutdentInline"
                | "Undo"
                | "Redo"
        )
}
