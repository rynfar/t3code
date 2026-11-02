//! Key matching, `when` evaluation, labels and jump helpers from `apps/web/src/keybindings.ts`
//! and `packages/shared/src/keybindings.ts`, plus GPUI keystroke and predicate translation.

use std::borrow::Cow;
use std::collections::HashSet;

use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShortcutModifierState {
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
}

/// A key press as the web's `KeyboardEvent` reports it: `key` is the layout character or named key
/// (`"ArrowUp"`, `"Enter"`), `code` the physical position (`"KeyB"`, `"BracketLeft"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShortcutEvent<'a> {
    pub key: &'a str,
    pub code: Option<&'a str>,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub alt_graph: bool,
}

impl ShortcutEvent<'_> {
    pub fn modifiers(&self) -> ShortcutModifierState {
        ShortcutModifierState {
            meta_key: self.meta_key,
            ctrl_key: self.ctrl_key,
            shift_key: self.shift_key,
            alt_key: self.alt_key,
        }
    }
}

pub const EVENT_CODE_SHORTCUT_KEYS: [(&str, &str); 21] = [
    ("Backquote", "`"),
    ("Backslash", "\\"),
    ("BracketLeft", "["),
    ("BracketRight", "]"),
    ("Comma", ","),
    ("Digit0", "0"),
    ("Digit1", "1"),
    ("Digit2", "2"),
    ("Digit3", "3"),
    ("Digit4", "4"),
    ("Digit5", "5"),
    ("Digit6", "6"),
    ("Digit7", "7"),
    ("Digit8", "8"),
    ("Digit9", "9"),
    ("Equal", "="),
    ("Minus", "-"),
    ("Period", "."),
    ("Quote", "'"),
    ("Semicolon", ";"),
    ("Slash", "/"),
];

pub const US_SHIFTED_TO_UNSHIFTED_KEYS: [(&str, &str); 21] = [
    ("!", "1"),
    ("@", "2"),
    ("#", "3"),
    ("$", "4"),
    ("%", "5"),
    ("^", "6"),
    ("&", "7"),
    ("*", "8"),
    ("(", "9"),
    (")", "0"),
    ("~", "`"),
    ("_", "-"),
    ("+", "="),
    ("{", "["),
    ("}", "]"),
    ("|", "\\"),
    (":", ";"),
    ("\"", "'"),
    ("<", ","),
    (">", "."),
    ("?", "/"),
];

pub fn normalize_event_key(key: &str) -> String {
    let normalized = key.to_lowercase();
    if normalized == "esc" { "escape".to_owned() } else { normalized }
}

fn is_latin_letter(key: &str) -> bool {
    matches!(key.as_bytes(), [b'a'..=b'z'])
}

pub fn shortcut_key_from_event(event: &ShortcutEvent) -> String {
    let layout_key = normalize_event_key(event.key);
    if is_latin_letter(&layout_key) {
        return layout_key;
    }
    event
        .code
        .and_then(|code| EVENT_CODE_SHORTCUT_KEYS.iter().find(|(name, _)| *name == code))
        .map_or(layout_key, |(_, key)| (*key).to_owned())
}

pub fn resolve_event_keys(event: &ShortcutEvent) -> HashSet<String> {
    let layout_key = normalize_event_key(event.key);
    let mut keys = HashSet::new();
    let letter_code = event
        .code
        .and_then(|code| code.strip_prefix("Key"))
        .filter(|letter| matches!(letter.as_bytes(), [b'A'..=b'Z']));
    if let Some(letter) = letter_code
        && !is_latin_letter(&layout_key)
    {
        keys.insert(letter.to_ascii_lowercase());
    }
    keys.insert(shortcut_key_from_event(event));
    keys.insert(layout_key);
    keys
}

fn expected_meta_ctrl(shortcut: &KeybindingShortcut, mac: bool) -> (bool, bool) {
    (shortcut.meta_key || (shortcut.mod_key && mac), shortcut.ctrl_key || (shortcut.mod_key && !mac))
}

pub fn matches_keybinding_shortcut_modifiers(
    modifiers: &ShortcutModifierState,
    shortcut: &KeybindingShortcut,
    mac: bool,
) -> bool {
    let (expected_meta, expected_ctrl) = expected_meta_ctrl(shortcut, mac);
    modifiers.meta_key == expected_meta
        && modifiers.ctrl_key == expected_ctrl
        && modifiers.shift_key == shortcut.shift_key
        && modifiers.alt_key == shortcut.alt_key
}

fn matches_event_keys(event: &ShortcutEvent, keys: &HashSet<String>, shortcut: &KeybindingShortcut, mac: bool) -> bool {
    if !mac && event.alt_graph && !matches!(event.key.as_bytes(), [c] if c.is_ascii_alphanumeric()) {
        return false;
    }
    matches_keybinding_shortcut_modifiers(&event.modifiers(), shortcut, mac) && keys.contains(&shortcut.key)
}

pub fn matches_keybinding_shortcut(event: &ShortcutEvent, shortcut: &KeybindingShortcut, mac: bool) -> bool {
    matches_event_keys(event, &resolve_event_keys(event), shortcut, mac)
}

/// The flags `when` clauses read. Absent flags are false; the native client is the desktop surface,
/// so `isDesktop` starts set and `isWeb` unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutMatchContext {
    flags: HashSet<&'static str>,
}

impl Default for ShortcutMatchContext {
    fn default() -> Self {
        Self { flags: HashSet::from(["isDesktop"]) }
    }
}

impl ShortcutMatchContext {
    pub fn with(mut self, name: &'static str, value: bool) -> Self {
        self.set(name, value);
        self
    }

    pub fn set(&mut self, name: &'static str, value: bool) {
        if value {
            self.flags.insert(name);
        } else {
            self.flags.remove(name);
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
}

fn evaluate_when_node(node: &KeybindingWhenNode, context: &ShortcutMatchContext) -> bool {
    match node {
        KeybindingWhenNode::Identifier { name } => match name.as_str() {
            "true" => true,
            "false" => false,
            name => context.flag(name),
        },
        KeybindingWhenNode::Not { node } => !evaluate_when_node(node, context),
        KeybindingWhenNode::And { left, right } => {
            evaluate_when_node(left, context) && evaluate_when_node(right, context)
        }
        KeybindingWhenNode::Or { left, right } => {
            evaluate_when_node(left, context) || evaluate_when_node(right, context)
        }
    }
}

fn matches_when_clause(when_ast: Option<&KeybindingWhenNode>, context: &ShortcutMatchContext) -> bool {
    when_ast.is_none_or(|node| evaluate_when_node(node, context))
}

pub fn shortcut_conflict_key(shortcut: &KeybindingShortcut, mac: bool) -> String {
    let (meta_key, ctrl_key) = expected_meta_ctrl(shortcut, mac);
    [
        shortcut.key.as_str(),
        if meta_key { "meta" } else { "" },
        if ctrl_key { "ctrl" } else { "" },
        if shortcut.shift_key { "shift" } else { "" },
        if shortcut.alt_key { "alt" } else { "" },
    ]
    .join("|")
}

/// Resolves a persisted Mod+Enter against the send preference without rewriting saved shortcuts.
fn shortcut_for_context<'a>(
    binding: &'a ResolvedKeybindingRule,
    context: &ShortcutMatchContext,
) -> Cow<'a, KeybindingShortcut> {
    let shortcut = &binding.shortcut;
    if context.flag("composerRequiresModifier")
        && binding.command == "composer.sendAlternate"
        && shortcut.key == "enter"
        && shortcut.mod_key
        && !shortcut.meta_key
        && !shortcut.ctrl_key
        && !shortcut.alt_key
        && !shortcut.shift_key
    {
        return Cow::Owned(KeybindingShortcut { shift_key: true, ..shortcut.clone() });
    }
    Cow::Borrowed(shortcut)
}

pub fn effective_shortcuts_for_command(
    keybindings: &[ResolvedKeybindingRule],
    command: &str,
    context: &ShortcutMatchContext,
    mac: bool,
) -> Vec<KeybindingShortcut> {
    let mut claimed_shortcuts = HashSet::new();
    let mut effective = Vec::new();
    for binding in keybindings.iter().rev() {
        if !matches_when_clause(binding.when_ast.as_ref(), context) {
            continue;
        }
        let shortcut = shortcut_for_context(binding, context);
        if !claimed_shortcuts.insert(shortcut_conflict_key(&shortcut, mac)) {
            continue;
        }
        if binding.command == command {
            effective.push(shortcut.into_owned());
        }
    }
    effective
}

/// The command a key press runs: the last rule whose `when` holds and whose shortcut matches.
pub fn resolve_shortcut_command<'a>(
    event: &ShortcutEvent,
    keybindings: &'a [ResolvedKeybindingRule],
    context: &ShortcutMatchContext,
    mac: bool,
) -> Option<&'a str> {
    let keys = resolve_event_keys(event);
    keybindings
        .iter()
        .rev()
        .find(|binding| {
            matches_when_clause(binding.when_ast.as_ref(), context)
                && matches_event_keys(event, &keys, &shortcut_for_context(binding, context), mac)
        })
        .map(|binding| binding.command.as_str())
}

/// App commands come from the primary environment's bindings, script commands from the active one's.
pub fn resolve_chat_shortcut_command<'a>(
    event: &ShortcutEvent,
    primary_keybindings: &'a [ResolvedKeybindingRule],
    active_keybindings: &'a [ResolvedKeybindingRule],
    context: &ShortcutMatchContext,
    mac: bool,
) -> Option<&'a str> {
    let primary = resolve_shortcut_command(event, primary_keybindings, context, mac);
    if let Some(command) = primary.filter(|command| project_script_id_from_command(command).is_none()) {
        return Some(command);
    }
    resolve_shortcut_command(event, active_keybindings, context, mac)
        .filter(|command| project_script_id_from_command(command).is_some())
}

pub fn format_shortcut_key_label(key: &str) -> String {
    match key {
        " " => "Space".to_owned(),
        _ if key.chars().count() == 1 => key.to_uppercase(),
        "escape" => "Esc".to_owned(),
        "arrowup" => "Up".to_owned(),
        "arrowdown" => "Down".to_owned(),
        "arrowleft" => "Left".to_owned(),
        "arrowright" => "Right".to_owned(),
        _ => {
            let mut chars = key.chars();
            chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
        }
    }
}

pub fn format_shortcut_label(shortcut: &KeybindingShortcut, mac: bool) -> String {
    let key_label = format_shortcut_key_label(&shortcut.key);
    let (show_meta, show_ctrl) = expected_meta_ctrl(shortcut, mac);
    let modifiers = [
        (show_ctrl, "⌃", "Ctrl"),
        (shortcut.alt_key, "⌥", "Alt"),
        (shortcut.shift_key, "⇧", "Shift"),
        (show_meta, "⌘", "Meta"),
    ];
    let shown = modifiers.iter().filter(|(shown, _, _)| *shown);
    if mac {
        shown.map(|(_, glyph, _)| *glyph).chain([key_label.as_str()]).collect()
    } else {
        shown.map(|(_, _, name)| *name).chain([key_label.as_str()]).collect::<Vec<_>>().join("+")
    }
}

pub fn shortcut_label_for_command(
    keybindings: &[ResolvedKeybindingRule],
    command: &str,
    context: &ShortcutMatchContext,
    mac: bool,
) -> Option<String> {
    effective_shortcuts_for_command(keybindings, command, context, mac)
        .first()
        .map(|shortcut| format_shortcut_label(shortcut, mac))
}

pub fn thread_jump_command_for_index(index: usize) -> Option<&'static str> {
    THREAD_JUMP_KEYBINDING_COMMANDS.get(index).copied()
}

pub fn thread_jump_index_from_command(command: &str) -> Option<usize> {
    THREAD_JUMP_KEYBINDING_COMMANDS.iter().position(|candidate| *candidate == command)
}

pub fn model_picker_jump_command_for_index(index: usize) -> Option<&'static str> {
    MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.get(index).copied()
}

pub fn model_picker_jump_index_from_command(command: &str) -> Option<usize> {
    MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.iter().position(|candidate| *candidate == command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadTraversalDirection {
    Previous,
    Next,
}

pub fn thread_traversal_direction_from_command(command: &str) -> Option<ThreadTraversalDirection> {
    match command {
        "thread.previous" => Some(ThreadTraversalDirection::Previous),
        "thread.next" => Some(ThreadTraversalDirection::Next),
        _ => None,
    }
}

pub fn should_show_thread_jump_hints_for_modifiers(
    modifiers: &ShortcutModifierState,
    keybindings: &[ResolvedKeybindingRule],
    context: &ShortcutMatchContext,
    mac: bool,
) -> bool {
    // Ghostty can write keys to the shell before window shortcuts run, regardless of the when clause.
    if context.flag("terminalFocus") {
        return false;
    }
    THREAD_JUMP_KEYBINDING_COMMANDS.iter().any(|command| {
        effective_shortcuts_for_command(keybindings, command, context, mac)
            .first()
            .is_some_and(|shortcut| matches_keybinding_shortcut_modifiers(modifiers, shortcut, mac))
    })
}

fn gpui_key(key: &str) -> String {
    let key = key.to_lowercase();
    match key.as_str() {
        "arrowup" => "up".to_owned(),
        "arrowdown" => "down".to_owned(),
        "arrowleft" => "left".to_owned(),
        "arrowright" => "right".to_owned(),
        " " => "space".to_owned(),
        _ => key,
    }
}

/// GPUI keystrokes that trigger `shortcut`, with modifiers in GPUI's own `ctrl-alt-cmd-shift-` order.
/// macOS GPUI reports Shift+symbol as the shifted character without Shift (Cmd+Shift+[ arrives as
/// `cmd-{`), so those shortcuts also get the shifted-character form.
pub fn gpui_keystrokes(shortcut: &KeybindingShortcut, mac: bool) -> Vec<String> {
    let (cmd, ctrl) = expected_meta_ctrl(shortcut, mac);
    let keystroke = |shift: bool, key: &str| {
        let mut keystroke = String::new();
        for (held, modifier) in [(ctrl, "ctrl-"), (shortcut.alt_key, "alt-"), (cmd, "cmd-"), (shift, "shift-")] {
            if held {
                keystroke.push_str(modifier);
            }
        }
        keystroke.push_str(key);
        keystroke
    };
    let key = gpui_key(&shortcut.key);
    let mut keystrokes = vec![keystroke(shortcut.shift_key, &key)];
    if mac && shortcut.shift_key {
        let shifted = US_SHIFTED_TO_UNSHIFTED_KEYS.iter().find(|(_, unshifted)| *unshifted == key);
        if let Some((shifted, _)) = shifted {
            keystrokes.push(keystroke(false, shifted));
        }
    }
    keystrokes
}

/// A GPUI context predicate equivalent to `node` evaluated over every context on the focus path.
/// GPUI matches a bare identifier against the innermost context only, while `!p` checks the whole
/// path, so each identifier `x` becomes `!(!x)`: "set anywhere on the path".
pub fn gpui_predicate(node: &KeybindingWhenNode) -> String {
    match node {
        KeybindingWhenNode::Identifier { name } => match name.as_str() {
            "true" => "!__t3_never__".to_owned(),
            "false" => "!(!__t3_never__)".to_owned(),
            name => format!("!(!{name})"),
        },
        KeybindingWhenNode::Not { node } => format!("!({})", gpui_predicate(node)),
        KeybindingWhenNode::And { left, right } => {
            format!("({} && {})", gpui_predicate(left), gpui_predicate(right))
        }
        KeybindingWhenNode::Or { left, right } => {
            format!("({} || {})", gpui_predicate(left), gpui_predicate(right))
        }
    }
}
