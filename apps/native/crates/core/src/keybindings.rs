//! Keybindings: `packages/contracts/src/keybindings.ts`, `packages/shared/src/keybindings.ts`, and
//! `apps/web/src/keybindings.ts` (matching, labels, jump helpers), plus translation of resolved
//! rules into GPUI keystrokes and context predicates.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::Value;

const MAX_KEYBINDING_VALUE_LENGTH: usize = 64;
pub const MAX_WHEN_EXPRESSION_DEPTH: usize = 64;
pub const MAX_SCRIPT_ID_LENGTH: usize = 24;
pub const MAX_KEYBINDINGS_COUNT: usize = 256;

pub const THREAD_JUMP_KEYBINDING_COMMANDS: [&str; 9] = [
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

pub const MODEL_PICKER_JUMP_KEYBINDING_COMMANDS: [&str; 9] = [
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
];

pub const STATIC_KEYBINDING_COMMANDS: &[&str] = &[
    "workspace.focusNext",
    "workspace.focusPrevious",
    "sidebar.toggle",
    "navigation.back",
    "navigation.forward",
    "terminal.toggle",
    "terminal.split",
    "terminal.splitVertical",
    "terminal.new",
    "terminal.close",
    "rightPanel.toggle",
    "rightPanel.new",
    "threadPanel.toggle",
    "rightPanel.toggleMaximized",
    "rightPanel.close",
    "view.reopenClosed",
    "pullRequest.copyNumber",
    "diff.toggle",
    "preview.toggle",
    "preview.refresh",
    "preview.focusUrl",
    "preview.zoomIn",
    "preview.zoomOut",
    "preview.resetZoom",
    "commandPalette.toggle",
    "filePicker.toggle",
    "projectSearch.toggle",
    "usage.open",
    "theme.select",
    "appearance.cycle",
    "themeEditor.toggle",
    "composer.stash",
    "composer.sendAlternate",
    "composer.sendBackground",
    "composer.sendAndNewThread",
    "composer.host",
    "composer.cycleHost",
    "composer.effort",
    "composer.increaseEffort",
    "composer.decreaseEffort",
    "composer.mode",
    "composer.workspace",
    "composer.previousWorktree",
    "composer.branch",
    "chat.new",
    "chat.newLocal",
    "chat.newWithoutProject",
    "chat.find",
    "editor.openFavorite",
    "usage.cost",
    "usage.tokens",
    "usage.limits",
    "usage.period.day",
    "usage.period.week",
    "usage.period.month",
    "usage.period.quarter",
    "modelPicker.toggle",
    "modelPicker.previousProvider",
    "modelPicker.nextProvider",
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
    "thread.stop",
    "thread.steerQueuedMessage",
    "thread.editQueuedMessage",
    "thread.previous",
    "thread.next",
    "thread.copyReference",
    "thread.settle",
    "thread.pin",
    "thread.undo",
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

fn script_id(command: &str) -> Option<&str> {
    let id = command.strip_prefix("script.")?.strip_suffix(".run")?;
    let mut chars = id.chars();
    let valid = id.len() <= MAX_SCRIPT_ID_LENGTH
        && chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then_some(id)
}

pub fn is_keybinding_command(command: &str) -> bool {
    STATIC_KEYBINDING_COMMANDS.contains(&command) || script_id(command).is_some()
}

pub fn project_script_id_from_command(command: &str) -> Option<&str> {
    script_id(command.trim())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingShortcut {
    pub key: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub mod_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum KeybindingWhenNode {
    Identifier { name: String },
    Not { node: Box<KeybindingWhenNode> },
    And { left: Box<KeybindingWhenNode>, right: Box<KeybindingWhenNode> },
    Or { left: Box<KeybindingWhenNode>, right: Box<KeybindingWhenNode> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedKeybindingRule {
    pub command: String,
    pub shortcut: KeybindingShortcut,
    pub when_ast: Option<KeybindingWhenNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingRule {
    pub key: String,
    pub command: String,
    pub when: Option<String>,
}

type RawRule = (&'static str, &'static str, Option<&'static str>);

const DEFAULT_KEYBINDINGS_BEFORE_JUMPS: &[RawRule] = &[
    ("mod+b", "sidebar.toggle", None),
    ("f6", "workspace.focusNext", None),
    ("shift+f6", "workspace.focusPrevious", None),
    ("mod+f6", "workspace.focusNext", None),
    ("mod+shift+f6", "workspace.focusPrevious", None),
    ("mod+[", "navigation.back", Some("!terminalFocus")),
    ("mod+]", "navigation.forward", Some("!terminalFocus")),
    ("mod+j", "terminal.toggle", None),
    ("mod+alt+b", "rightPanel.toggle", None),
    ("mod+shift+b", "threadPanel.toggle", Some("!terminalFocus")),
    ("mod+t", "rightPanel.new", Some("rightPanelOpen && !terminalFocus")),
    ("mod+d", "terminal.split", Some("terminalFocus")),
    ("mod+shift+d", "terminal.splitVertical", Some("terminalFocus")),
    ("mod+n", "terminal.new", Some("terminalFocus")),
    ("mod+w", "terminal.close", Some("terminalFocus")),
    ("mod+w", "rightPanel.close", Some("!terminalFocus")),
    ("mod+shift+t", "view.reopenClosed", None),
    ("mod+d", "diff.toggle", Some("!terminalFocus")),
    ("mod+shift+j", "preview.toggle", None),
    ("mod+r", "preview.refresh", Some("previewFocus")),
    ("mod+l", "preview.focusUrl", Some("previewFocus")),
    ("mod+=", "preview.zoomIn", Some("previewFocus")),
    ("mod++", "preview.zoomIn", Some("previewFocus")),
    ("mod+-", "preview.zoomOut", Some("previewFocus")),
    ("mod+0", "preview.resetZoom", Some("previewFocus")),
    ("mod+k", "commandPalette.toggle", Some("!terminalFocus")),
    ("mod+p", "filePicker.toggle", Some("!terminalFocus")),
    ("mod+shift+f", "projectSearch.toggle", Some("!terminalFocus")),
    ("mod+u", "usage.open", Some("!terminalFocus")),
    ("mod+alt+a", "theme.select", Some("!terminalFocus")),
    ("mod+alt+shift+a", "appearance.cycle", Some("!terminalFocus")),
    ("mod+alt+shift+t", "themeEditor.toggle", None),
    ("mod+s", "composer.stash", Some("!terminalFocus")),
    ("mod+shift+enter", "thread.steerQueuedMessage", Some("!terminalFocus")),
    ("alt+arrowup", "thread.editQueuedMessage", Some("composerFocus")),
    ("mod+enter", "composer.sendAlternate", Some("composerFocus && turnRunning")),
    ("mod+enter", "composer.sendBackground", Some("composerFocus && draftThreadRoute")),
    ("mod+alt+enter", "composer.sendBackground", Some("composerFocus && draftThreadRoute")),
    ("mod+alt+enter", "composer.sendAndNewThread", Some("composerFocus && !draftThreadRoute")),
    ("mod+n", "chat.new", Some("!terminalFocus")),
    ("mod+shift+o", "chat.new", Some("!terminalFocus")),
    ("mod+shift+n", "chat.newLocal", Some("!terminalFocus")),
    ("mod+alt+n", "chat.newWithoutProject", Some("!terminalFocus")),
    ("mod+f", "chat.find", Some("!terminalFocus && !previewFocus")),
    ("mod+shift+m", "modelPicker.toggle", Some("!terminalFocus")),
    ("mod+shift+h", "composer.host", Some("!terminalFocus")),
    ("mod+shift+e", "composer.effort", Some("!terminalFocus")),
    ("mod+shift+a", "composer.mode", Some("!terminalFocus")),
    ("mod+shift+x", "composer.workspace", Some("!terminalFocus")),
    ("mod+shift+g", "composer.branch", Some("!terminalFocus")),
    ("mod+shift+l", "composer.previousWorktree", Some("!terminalFocus")),
    ("mod+shift+k", "pullRequest.copyNumber", Some("!terminalFocus")),
    ("mod+shift+arrowup", "modelPicker.previousProvider", Some("modelPickerOpen")),
    ("mod+shift+arrowdown", "modelPicker.nextProvider", Some("modelPickerOpen")),
    ("mod+o", "editor.openFavorite", None),
    ("mod+shift+[", "thread.previous", None),
    ("mod+shift+]", "thread.next", None),
    ("mod+shift+c", "thread.copyReference", Some("!terminalFocus")),
    ("mod+shift+s", "thread.settle", Some("!terminalFocus")),
    ("mod+shift+p", "thread.pin", Some("!terminalFocus")),
    ("mod+z", "thread.undo", Some("!terminalFocus && !editableFocus")),
];

const DEFAULT_KEYBINDINGS_AFTER_JUMPS: &[RawRule] = &[
    ("c", "usage.cost", Some("usagePageOpen")),
    ("t", "usage.tokens", Some("usagePageOpen")),
    ("l", "usage.limits", Some("usagePageOpen")),
    ("mod+shift+1", "usage.period.day", Some("usagePageOpen")),
    ("mod+shift+2", "usage.period.week", Some("usagePageOpen")),
    ("mod+shift+3", "usage.period.month", Some("usagePageOpen")),
    ("mod+shift+4", "usage.period.quarter", Some("usagePageOpen")),
];

fn raw_rule(&(key, command, when): &RawRule) -> KeybindingRule {
    KeybindingRule { key: key.into(), command: command.into(), when: when.map(Into::into) }
}

fn jump_rules(commands: [&'static str; 9], when: &'static str) -> impl Iterator<Item = KeybindingRule> {
    commands.into_iter().enumerate().map(move |(index, command)| KeybindingRule {
        key: format!("mod+{}", index + 1),
        command: command.into(),
        when: Some(when.into()),
    })
}

/// `DEFAULT_KEYBINDINGS`, in the same order.
pub fn default_keybindings() -> Vec<KeybindingRule> {
    DEFAULT_KEYBINDINGS_BEFORE_JUMPS
        .iter()
        .map(raw_rule)
        .chain(jump_rules(THREAD_JUMP_KEYBINDING_COMMANDS, "!modelPickerOpen && isDesktop"))
        .chain(jump_rules(MODEL_PICKER_JUMP_KEYBINDING_COMMANDS, "modelPickerOpen && isDesktop"))
        .chain(DEFAULT_KEYBINDINGS_AFTER_JUMPS.iter().map(raw_rule))
        .collect()
}

fn normalize_key_token(token: &str) -> &str {
    match token {
        "space" => " ",
        "esc" => "escape",
        other => other,
    }
}

pub fn parse_keybinding_shortcut(value: &str) -> Option<KeybindingShortcut> {
    let lowered = value.to_lowercase();
    let mut tokens: Vec<&str> = lowered.split('+').map(str::trim).collect();
    let mut trailing_empty = false;
    while tokens.last() == Some(&"") {
        tokens.pop();
        trailing_empty = true;
    }
    if trailing_empty {
        tokens.push("+");
    }
    if tokens.iter().any(|token| token.is_empty()) {
        return None;
    }

    let mut shortcut = KeybindingShortcut {
        key: String::new(),
        meta_key: false,
        ctrl_key: false,
        shift_key: false,
        alt_key: false,
        mod_key: false,
    };
    let mut key = None;
    for token in tokens {
        match token {
            "cmd" | "meta" => shortcut.meta_key = true,
            "ctrl" | "control" => shortcut.ctrl_key = true,
            "shift" => shortcut.shift_key = true,
            "alt" | "option" => shortcut.alt_key = true,
            "mod" => shortcut.mod_key = true,
            _ if key.is_some() => return None,
            _ => key = Some(normalize_key_token(token)),
        }
    }
    shortcut.key = key?.to_owned();
    Some(shortcut)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhenToken<'a> {
    Identifier(&'a str),
    Not,
    And,
    Or,
    LParen,
    RParen,
}

fn tokenize_when_expression(expression: &str) -> Option<Vec<WhenToken<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = expression;
    while let Some(current) = rest.chars().next() {
        if current.is_whitespace() {
            rest = &rest[current.len_utf8()..];
            continue;
        }
        let (token, len) = if rest.starts_with("&&") {
            (WhenToken::And, 2)
        } else if rest.starts_with("||") {
            (WhenToken::Or, 2)
        } else {
            match current {
                '!' => (WhenToken::Not, 1),
                '(' => (WhenToken::LParen, 1),
                ')' => (WhenToken::RParen, 1),
                c if c.is_ascii_alphabetic() || c == '_' => {
                    let len = rest
                        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
                        .unwrap_or(rest.len());
                    (WhenToken::Identifier(&rest[..len]), len)
                }
                _ => return None,
            }
        };
        tokens.push(token);
        rest = &rest[len..];
    }
    Some(tokens)
}

struct WhenParser<'a> {
    tokens: Vec<WhenToken<'a>>,
    index: usize,
}

impl<'a> WhenParser<'a> {
    fn peek(&self) -> Option<WhenToken<'a>> {
        self.tokens.get(self.index).copied()
    }

    fn parse_primary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        if depth > MAX_WHEN_EXPRESSION_DEPTH {
            return None;
        }
        match self.peek()? {
            WhenToken::Identifier(name) => {
                self.index += 1;
                Some(KeybindingWhenNode::Identifier { name: name.to_owned() })
            }
            WhenToken::LParen => {
                self.index += 1;
                let node = self.parse_or(depth + 1)?;
                if self.peek() != Some(WhenToken::RParen) {
                    return None;
                }
                self.index += 1;
                Some(node)
            }
            _ => None,
        }
    }

    fn parse_unary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut not_count = 0;
        while self.peek() == Some(WhenToken::Not) {
            self.index += 1;
            not_count += 1;
            if not_count > MAX_WHEN_EXPRESSION_DEPTH {
                return None;
            }
        }
        let mut node = self.parse_primary(depth)?;
        for _ in 0..not_count {
            node = KeybindingWhenNode::Not { node: Box::new(node) };
        }
        Some(node)
    }

    fn parse_and(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut left = self.parse_unary(depth)?;
        while self.peek() == Some(WhenToken::And) {
            self.index += 1;
            let right = self.parse_unary(depth)?;
            left = KeybindingWhenNode::And { left: Box::new(left), right: Box::new(right) };
        }
        Some(left)
    }

    fn parse_or(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut left = self.parse_and(depth)?;
        while self.peek() == Some(WhenToken::Or) {
            self.index += 1;
            let right = self.parse_and(depth)?;
            left = KeybindingWhenNode::Or { left: Box::new(left), right: Box::new(right) };
        }
        Some(left)
    }
}

pub fn parse_keybinding_when_expression(expression: &str) -> Option<KeybindingWhenNode> {
    let tokens = tokenize_when_expression(expression)?;
    if tokens.is_empty() {
        return None;
    }
    let mut parser = WhenParser { tokens, index: 0 };
    let ast = parser.parse_or(0)?;
    (parser.index == parser.tokens.len()).then_some(ast)
}

pub fn compile_resolved_keybinding_rule(rule: &KeybindingRule) -> Option<ResolvedKeybindingRule> {
    let shortcut = parse_keybinding_shortcut(&rule.key)?;
    let when_ast = match &rule.when {
        Some(when) => Some(parse_keybinding_when_expression(when)?),
        None => None,
    };
    Some(ResolvedKeybindingRule { command: rule.command.clone(), shortcut, when_ast })
}

fn keep_last(mut rules: Vec<ResolvedKeybindingRule>) -> Vec<ResolvedKeybindingRule> {
    rules.drain(..rules.len().saturating_sub(MAX_KEYBINDINGS_COUNT));
    rules
}

pub fn compile_resolved_keybindings_config(config: &[KeybindingRule]) -> Vec<ResolvedKeybindingRule> {
    keep_last(config.iter().filter_map(compile_resolved_keybinding_rule).collect())
}

static DEFAULT_RESOLVED_KEYBINDINGS: LazyLock<Vec<ResolvedKeybindingRule>> =
    LazyLock::new(|| compile_resolved_keybindings_config(&default_keybindings()));

pub fn default_resolved_keybindings() -> &'static [ResolvedKeybindingRule] {
    &DEFAULT_RESOLVED_KEYBINDINGS
}

/// Defaults for every command `custom` doesn't bind, followed by `custom`, so custom rules win ties.
pub fn merge_with_default_keybindings(custom: &[ResolvedKeybindingRule]) -> Vec<ResolvedKeybindingRule> {
    let overridden: HashSet<&str> = custom.iter().map(|binding| binding.command.as_str()).collect();
    let merged = default_resolved_keybindings()
        .iter()
        .filter(|binding| !overridden.contains(binding.command.as_str()))
        .chain(custom)
        .cloned()
        .collect();
    keep_last(merged)
}

fn decode_when_is_valid(node: &KeybindingWhenNode) -> bool {
    match node {
        KeybindingWhenNode::Identifier { name } => !name.is_empty(),
        KeybindingWhenNode::Not { node } => decode_when_is_valid(node),
        KeybindingWhenNode::And { left, right } | KeybindingWhenNode::Or { left, right } => {
            decode_when_is_valid(left) && decode_when_is_valid(right)
        }
    }
}

fn decode_resolved_keybinding_rule(value: &Value) -> Option<ResolvedKeybindingRule> {
    let mut rule = ResolvedKeybindingRule::deserialize(value).ok()?;
    let key = rule.shortcut.key.trim();
    let key_length = key.encode_utf16().count();
    if key_length == 0 || key_length > MAX_KEYBINDING_VALUE_LENGTH || !is_keybinding_command(&rule.command) {
        return None;
    }
    if !rule.when_ast.as_ref().is_none_or(decode_when_is_valid) {
        return None;
    }
    rule.shortcut.key = key.to_owned();
    Some(rule)
}

/// Decodes a wire `ResolvedKeybindingsConfig`, dropping entries this build can't decode.
pub fn decode_resolved_keybindings(value: &Value) -> Vec<ResolvedKeybindingRule> {
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    keep_last(entries.iter().filter_map(decode_resolved_keybinding_rule).collect())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MAC: bool = true;
    const WIN: bool = false;
    const LINUX: bool = false;

    fn sc(value: &str) -> KeybindingShortcut {
        parse_keybinding_shortcut(value).unwrap()
    }

    fn compile(rules: &[(&str, &str, &str)]) -> Vec<ResolvedKeybindingRule> {
        rules
            .iter()
            .map(|&(key, command, when)| ResolvedKeybindingRule {
                command: command.to_owned(),
                shortcut: sc(key),
                when_ast: (!when.is_empty()).then(|| parse_keybinding_when_expression(when).unwrap()),
            })
            .collect()
    }

    fn ev<'a>(key: &'a str, modifiers: &str) -> ShortcutEvent<'a> {
        let held = |name| modifiers.split('+').any(|modifier| modifier == name);
        ShortcutEvent {
            key,
            meta_key: held("meta"),
            ctrl_key: held("ctrl"),
            shift_key: held("shift"),
            alt_key: held("alt"),
            ..ShortcutEvent::default()
        }
    }

    fn ctx(flags: &[(&'static str, bool)]) -> ShortcutMatchContext {
        flags.iter().fold(ShortcutMatchContext::default(), |context, &(name, value)| context.with(name, value))
    }

    fn defaults() -> &'static [ResolvedKeybindingRule] {
        default_resolved_keybindings()
    }

    fn resolve<'a>(
        event: ShortcutEvent,
        keybindings: &'a [ResolvedKeybindingRule],
        flags: &[(&'static str, bool)],
        mac: bool,
    ) -> Option<&'a str> {
        resolve_shortcut_command(&event, keybindings, &ctx(flags), mac)
    }

    fn ident(name: &str) -> KeybindingWhenNode {
        KeybindingWhenNode::Identifier { name: name.to_owned() }
    }

    fn not(node: KeybindingWhenNode) -> KeybindingWhenNode {
        KeybindingWhenNode::Not { node: Box::new(node) }
    }

    fn and(left: KeybindingWhenNode, right: KeybindingWhenNode) -> KeybindingWhenNode {
        KeybindingWhenNode::And { left: Box::new(left), right: Box::new(right) }
    }

    fn or(left: KeybindingWhenNode, right: KeybindingWhenNode) -> KeybindingWhenNode {
        KeybindingWhenNode::Or { left: Box::new(left), right: Box::new(right) }
    }

    /// `DEFAULT_BINDINGS` from `apps/web/src/keybindings.test.ts`.
    fn web_fixture() -> Vec<ResolvedKeybindingRule> {
        compile(&[
            ("mod+b", "sidebar.toggle", ""),
            ("mod+j", "terminal.toggle", ""),
            ("mod+alt+b", "rightPanel.toggle", ""),
            ("mod+d", "terminal.split", "terminalFocus"),
            ("mod+shift+d", "terminal.splitVertical", "terminalFocus"),
            ("mod+n", "terminal.new", "terminalFocus"),
            ("mod+w", "terminal.close", "terminalFocus"),
            ("mod+w", "rightPanel.close", "!terminalFocus"),
            ("mod+d", "diff.toggle", "!terminalFocus"),
            ("mod+k", "commandPalette.toggle", "!terminalFocus"),
            ("mod+p", "filePicker.toggle", "!terminalFocus"),
            ("mod+shift+f", "projectSearch.toggle", "!terminalFocus"),
            ("mod+alt+shift+t", "themeEditor.toggle", ""),
            ("mod+f", "chat.find", "!terminalFocus && !previewFocus"),
            ("mod+shift+m", "modelPicker.toggle", "!terminalFocus"),
            ("mod+shift+o", "chat.new", ""),
            ("mod+shift+n", "chat.newLocal", ""),
            ("mod+o", "editor.openFavorite", ""),
            ("mod+shift+[", "thread.previous", ""),
            ("mod+shift+]", "thread.next", ""),
            ("mod+shift+c", "thread.copyReference", "!terminalFocus"),
            ("mod+shift+s", "thread.settle", "!terminalFocus"),
            ("mod+1", "thread.jump.1", ""),
            ("mod+2", "thread.jump.2", ""),
            ("mod+3", "thread.jump.3", ""),
            ("mod+1", "modelPicker.jump.1", "modelPickerOpen"),
            ("mod+2", "modelPicker.jump.2", "modelPickerOpen"),
            ("mod+3", "modelPicker.jump.3", "modelPickerOpen"),
        ])
    }

    #[test]
    fn passes_only_effective_preview_shortcuts_to_the_desktop_bridge() {
        let keybindings = compile(&[
            ("mod+shift+t", "view.reopenClosed", ""),
            ("mod+shift+y", "view.reopenClosed", "previewFocus"),
            ("mod+shift+t", "preview.toggle", "previewFocus"),
        ]);
        let focused = ctx(&[("previewFocus", true), ("previewOpen", true)]);
        assert_eq!(
            effective_shortcuts_for_command(&keybindings, "view.reopenClosed", &focused, MAC),
            vec![sc("mod+shift+y")]
        );
        assert_eq!(
            effective_shortcuts_for_command(&keybindings, "view.reopenClosed", &ctx(&[]), MAC),
            vec![sc("mod+shift+t")]
        );
    }

    #[test]
    fn settle_thread_shortcut_does_not_intercept_the_terminal() {
        let fixture = web_fixture();
        assert_eq!(resolve(ev("s", "ctrl+shift"), &fixture, &[("terminalFocus", true)], WIN), None);
    }

    #[test]
    fn thread_undo_leaves_native_undo_alone_inside_text_fields_and_terminals() {
        assert_eq!(resolve(ev("z", "ctrl"), defaults(), &[("editableFocus", true)], WIN), None);
        assert_eq!(resolve(ev("z", "ctrl"), defaults(), &[("terminalFocus", true)], WIN), None);
        assert_eq!(resolve(ev("z", "ctrl"), defaults(), &[], WIN), Some("thread.undo"));
    }

    #[test]
    fn split_and_new_terminal_shortcuts_support_when_expressions() {
        let keybindings = compile(&[
            ("mod+\\", "terminal.split", "terminalOpen && !terminalFocus"),
            ("mod+shift+n", "terminal.new", "terminalOpen && !terminalFocus"),
            ("mod+j", "terminal.toggle", ""),
        ]);
        let open = [("terminalOpen", true), ("terminalFocus", false)];
        assert_eq!(resolve(ev("\\", "ctrl"), &keybindings, &open, WIN), Some("terminal.split"));
        assert_eq!(resolve(ev("\\", "ctrl"), &keybindings, &[("terminalOpen", false)], WIN), None);
        assert_eq!(resolve(ev("n", "ctrl+shift"), &keybindings, &open, WIN), Some("terminal.new"));
    }

    #[test]
    fn label_is_none_for_commands_shadowed_by_a_later_conflicting_shortcut() {
        let bindings = compile(&[("mod+shift+1", "thread.jump.1", ""), ("mod+shift+1", "thread.jump.7", "")]);
        assert_eq!(shortcut_label_for_command(&bindings, "thread.jump.1", &ctx(&[]), MAC), None);
        assert_eq!(shortcut_label_for_command(&bindings, "thread.jump.7", &ctx(&[]), MAC).as_deref(), Some("⇧⌘1"));
    }

    #[test]
    fn label_respects_when_context() {
        let bindings = compile(&[("mod+d", "diff.toggle", ""), ("mod+d", "terminal.split", "terminalFocus")]);
        let unfocused = ctx(&[("terminalFocus", false)]);
        let focused = ctx(&[("terminalFocus", true)]);
        assert_eq!(shortcut_label_for_command(&bindings, "diff.toggle", &unfocused, LINUX).as_deref(), Some("Ctrl+D"));
        assert_eq!(shortcut_label_for_command(&bindings, "diff.toggle", &focused, LINUX), None);
        assert_eq!(shortcut_label_for_command(&bindings, "terminal.split", &focused, LINUX).as_deref(), Some("Ctrl+D"));
    }

    #[test]
    fn never_shows_jump_hints_while_the_terminal_is_focused() {
        let fixture = web_fixture();
        let meta = ev("Meta", "meta").modifiers();
        let hints = |flags: &[(&'static str, bool)]| {
            should_show_thread_jump_hints_for_modifiers(&meta, &fixture, &ctx(flags), MAC)
        };
        assert!(!hints(&[("terminalFocus", true)]));
        assert!(hints(&[("terminalFocus", false)]));
    }

    #[test]
    fn keeps_default_thread_jumps_off_the_web() {
        let input = ev("1", "meta");
        assert_eq!(resolve(input, defaults(), &[("isDesktop", false)], MAC), None);
        assert_eq!(resolve(input, defaults(), &[("isDesktop", true)], MAC), Some("thread.jump.1"));
        let meta = input.modifiers();
        let hints = |flags: &[(&'static str, bool)]| {
            should_show_thread_jump_hints_for_modifiers(&meta, defaults(), &ctx(flags), MAC)
        };
        assert!(!hints(&[("isDesktop", false)]));
        assert!(hints(&[("isDesktop", true)]));
        assert!(hints(&[]));
    }

    #[test]
    fn keeps_default_model_jumps_off_the_web_even_while_the_picker_is_open() {
        let input = ev("3", "meta");
        assert_eq!(resolve(input, defaults(), &[("isDesktop", false), ("modelPickerOpen", true)], MAC), None);
        assert_eq!(
            resolve(input, defaults(), &[("isDesktop", true), ("modelPickerOpen", true)], MAC),
            Some("modelPicker.jump.3")
        );
        assert_eq!(
            resolve(input, defaults(), &[("isDesktop", true), ("modelPickerOpen", false)], MAC),
            Some("thread.jump.3")
        );
    }

    #[test]
    fn toggles_the_thread_panel_by_default_without_taking_terminal_keystrokes() {
        for mac in [true, false] {
            let input = ev("b", if mac { "meta+shift" } else { "ctrl+shift" });
            let flags = [("terminalFocus", false), ("editableFocus", true)];
            assert_eq!(resolve(input, defaults(), &flags, mac), Some("threadPanel.toggle"));
            assert_eq!(resolve(input, defaults(), &[("terminalFocus", true)], mac), None);
        }
    }

    #[test]
    fn preserves_a_user_configured_thread_panel_shortcut_instead_of_its_default() {
        let bindings = merge_with_default_keybindings(&compile(&[("mod+alt+shift+b", "threadPanel.toggle", "")]));
        assert_eq!(resolve(ev("b", "meta+alt+shift"), &bindings, &[], MAC), Some("threadPanel.toggle"));
        assert_eq!(resolve(ev("b", "meta+shift"), &bindings, &[], MAC), None);
    }

    #[test]
    fn a_later_focused_rule_overrides_a_global_rule() {
        let keybindings = compile(&[("mod+n", "chat.new", ""), ("mod+n", "terminal.new", "terminalFocus")]);
        assert_eq!(resolve(ev("n", "meta"), &keybindings, &[("terminalFocus", true)], MAC), Some("terminal.new"));
        assert_eq!(resolve(ev("n", "meta"), &keybindings, &[("terminalFocus", false)], MAC), Some("chat.new"));
    }

    #[test]
    fn a_later_global_rule_still_wins_when_both_rules_match() {
        let keybindings = compile(&[("mod+n", "terminal.new", "terminalFocus"), ("mod+n", "chat.new", "")]);
        assert_eq!(resolve(ev("n", "ctrl"), &keybindings, &[("terminalFocus", true)], LINUX), Some("chat.new"));
    }

    fn chat<'a>(
        event: ShortcutEvent,
        primary: &'a [ResolvedKeybindingRule],
        active: &'a [ResolvedKeybindingRule],
        flags: &[(&'static str, bool)],
    ) -> Option<&'a str> {
        resolve_chat_shortcut_command(&event, primary, active, &ctx(flags), LINUX)
    }

    #[test]
    fn chat_runs_the_active_environments_script_shortcut_instead_of_the_primarys() {
        let primary = compile(&[("mod+r", "script.setup.run", "")]);
        let active = compile(&[("mod+t", "script.setup.run", "")]);
        assert_eq!(chat(ev("t", "ctrl"), &primary, &active, &[]), Some("script.setup.run"));
        assert_eq!(chat(ev("r", "ctrl"), &primary, &active, &[]), None);
        assert_eq!(chat(ev("r", "ctrl"), &primary, &[], &[]), None);
    }

    #[test]
    fn chat_keeps_primary_app_commands_ahead_of_active_scripts_and_remaps() {
        let primary = compile(&[("mod+k", "commandPalette.toggle", "")]);
        let active = compile(&[("mod+k", "script.setup.run", "")]);
        assert_eq!(chat(ev("k", "ctrl"), &primary, &active, &[]), Some("commandPalette.toggle"));

        let primary = compile(&[("mod+j", "terminal.toggle", "")]);
        let active = compile(&[("mod+t", "terminal.toggle", "")]);
        assert_eq!(chat(ev("j", "ctrl"), &primary, &active, &[]), Some("terminal.toggle"));
        assert_eq!(chat(ev("t", "ctrl"), &primary, &active, &[]), None);
    }

    #[test]
    fn chat_preserves_active_binding_precedence() {
        let primary = compile(&[("mod+r", "script.setup.run", "")]);
        let active = compile(&[("mod+r", "script.deploy.run", ""), ("mod+r", "terminal.new", "terminalFocus")]);
        for terminal_focus in [false, true] {
            let flags = [("terminalFocus", terminal_focus)];
            let input = ev("r", "ctrl");
            let expected = if terminal_focus { None } else { Some("script.deploy.run") };
            assert_eq!(chat(input, &primary, &active, &flags), expected);
            assert_eq!(chat(input, &active, &active, &flags), resolve(input, &active, &flags, LINUX));
        }
    }

    #[test]
    fn routes_mod_w_to_the_terminal_while_focused_and_to_the_right_panel_otherwise() {
        let fixture = web_fixture();
        let close = ev("w", "meta");
        assert_eq!(resolve(close, &fixture, &[("terminalFocus", true)], MAC), Some("terminal.close"));
        assert_eq!(resolve(close, &fixture, &[("terminalFocus", false)], MAC), Some("rightPanel.close"));
    }

    #[test]
    fn navigates_history_with_mod_brackets_outside_the_terminal() {
        let back = ShortcutEvent { code: Some("BracketLeft"), ..ev("[", "meta") };
        let forward = ShortcutEvent { code: Some("BracketRight"), ..ev("]", "ctrl") };
        assert_eq!(resolve(back, defaults(), &[], MAC), Some("navigation.back"));
        assert_eq!(resolve(forward, defaults(), &[], LINUX), Some("navigation.forward"));
        assert_eq!(resolve(back, defaults(), &[("terminalFocus", true)], MAC), None);
    }

    #[test]
    fn matches_punctuation_shortcuts_by_physical_key_across_layouts() {
        let keybindings = compile(&[("mod+shift+'", "diff.toggle", "")]);
        let quote_position = ShortcutEvent { code: Some("Quote"), ..ev("@", "meta+shift") };
        let digit_position = ShortcutEvent { code: Some("Digit2"), ..ev("\"", "meta+shift") };
        assert_eq!(resolve(quote_position, &keybindings, &[], MAC), Some("diff.toggle"));
        assert_eq!(resolve(digit_position, &keybindings, &[], MAC), None);
    }

    #[test]
    fn a_punctuation_position_does_not_shadow_a_latin_layout_key() {
        let keybindings = compile(&[("mod+m", "diff.toggle", ""), ("mod+;", "sidebar.toggle", "")]);
        let input = ShortcutEvent { code: Some("Semicolon"), ..ev("m", "meta") };
        assert_eq!(resolve(input, &keybindings, &[], MAC), Some("diff.toggle"));
    }

    #[test]
    fn matches_option_modified_and_non_latin_letters_by_physical_code() {
        let fixture = web_fixture();
        let option_b = ShortcutEvent { code: Some("KeyB"), ..ev("∫", "meta+alt") };
        assert_eq!(resolve(option_b, &fixture, &[], MAC), Some("rightPanel.toggle"));

        let keybindings = compile(&[("mod+d", "diff.toggle", "")]);
        let cyrillic_d = ShortcutEvent { code: Some("KeyD"), ..ev("в", "meta") };
        assert_eq!(resolve(cyrillic_d, &keybindings, &[], MAC), Some("diff.toggle"));

        let cyrillic_b = ShortcutEvent { code: Some("KeyB"), ..ev("и", "ctrl") };
        assert_eq!(resolve(cyrillic_b, &fixture, &[], WIN), Some("sidebar.toggle"));
    }

    #[test]
    fn ignores_the_physical_code_when_the_layout_types_a_different_latin_letter() {
        let keybindings = compile(&[("mod+d", "diff.toggle", "")]);
        let a_on_d = ShortcutEvent { code: Some("KeyD"), ..ev("a", "meta") };
        let d_on_l = ShortcutEvent { code: Some("KeyL"), ..ev("d", "meta") };
        assert_eq!(resolve(a_on_d, &keybindings, &[], MAC), None);
        assert_eq!(resolve(d_on_l, &keybindings, &[], MAC), Some("diff.toggle"));
    }

    #[test]
    fn fills_missing_number_shortcuts_without_replacing_the_saved_url_binding() {
        let mut custom: Vec<_> = defaults()
            .iter()
            .filter(|binding| binding.command != "pullRequest.copyNumber" && binding.command != "thread.copyReference")
            .cloned()
            .collect();
        custom.extend(compile_resolved_keybindings_config(&[KeybindingRule {
            key: "mod+shift+8".into(),
            command: "thread.copyReference".into(),
            when: Some("!terminalFocus".into()),
        }]));
        let bindings = merge_with_default_keybindings(&custom);
        for (key, command) in
            [("k", Some("pullRequest.copyNumber")), ("8", Some("thread.copyReference")), ("c", None), ("y", None)]
        {
            assert_eq!(resolve(ev(key, "meta+shift"), &bindings, &[], MAC), command, "key {key}");
        }
    }

    #[test]
    fn reserves_the_alternate_send_chord_for_the_focused_composer() {
        for mac in [true, false] {
            let shifted_enter = ev("Enter", if mac { "meta+shift" } else { "ctrl+shift" });
            for requires_modifier in [false, true] {
                let flags =
                    [("composerFocus", true), ("turnRunning", true), ("composerRequiresModifier", requires_modifier)];
                let expected = if requires_modifier { "composer.sendAlternate" } else { "thread.steerQueuedMessage" };
                assert_eq!(resolve(shifted_enter, defaults(), &flags, mac), Some(expected));
                let label = shortcut_label_for_command(defaults(), "composer.sendAlternate", &ctx(&flags), mac);
                let shortcut = if requires_modifier { sc("mod+shift+enter") } else { sc("mod+enter") };
                assert_eq!(label, Some(format_shortcut_label(&shortcut, mac)));
                let mut unfocused = flags;
                unfocused[0].1 = false;
                assert_eq!(resolve(shifted_enter, defaults(), &unfocused, mac), Some("thread.steerQueuedMessage"));
            }
        }
    }

    fn alt_effort_bindings() -> Vec<ResolvedKeybindingRule> {
        compile(&[("mod+alt+e", "composer.effort", "!terminalFocus")])
    }

    #[test]
    fn leaves_altgr_text_entry_alone_with_a_custom_alt_binding() {
        let bindings = alt_effort_bindings();
        let altgr = ShortcutEvent { code: Some("KeyE"), alt_graph: true, ..ev("€", "ctrl+alt") };
        for mac in [WIN, LINUX] {
            assert_eq!(resolve(altgr, &bindings, &[], mac), None);
            let plain = ShortcutEvent { alt_graph: false, ..altgr };
            assert_eq!(resolve(plain, &bindings, &[], mac), Some("composer.effort"));
        }
    }

    #[test]
    fn keeps_firefox_modifier_reporting_usable_on_windows_and_macos() {
        let bindings = alt_effort_bindings();
        let windows = ShortcutEvent { alt_graph: true, ..ev("e", "ctrl+alt") };
        assert_eq!(resolve(windows, &bindings, &[], WIN), Some("composer.effort"));
        let mac = ShortcutEvent { code: Some("KeyE"), alt_graph: true, ..ev("´", "meta+alt") };
        assert_eq!(resolve(mac, &bindings, &[], MAC), Some("composer.effort"));
    }

    #[test]
    fn preserves_desktop_numbered_thread_shortcuts_on_usage() {
        for mac in [false, true] {
            let input = ev("2", if mac { "meta" } else { "ctrl" });
            let flags = [("usagePageOpen", true), ("isDesktop", true)];
            assert_eq!(resolve(input, defaults(), &flags, mac), Some("thread.jump.2"));
            assert!(shortcut_label_for_command(defaults(), "thread.jump.2", &ctx(&flags), mac).is_some());
        }
    }

    #[test]
    fn matches_shifted_number_keys_for_usage_periods_only_on_usage() {
        let input = ShortcutEvent { code: Some("Digit1"), ..ev("!", "ctrl+shift") };
        assert_eq!(resolve(input, defaults(), &[("usagePageOpen", true)], LINUX), Some("usage.period.day"));
        assert_eq!(resolve(input, defaults(), &[], LINUX), None);
    }

    #[test]
    fn parses_shortcuts_including_plus_key() {
        let plain = |key: &str| KeybindingShortcut {
            key: key.to_owned(),
            meta_key: false,
            ctrl_key: false,
            shift_key: false,
            alt_key: false,
            mod_key: false,
        };
        assert_eq!(parse_keybinding_shortcut("mod+j"), Some(KeybindingShortcut { mod_key: true, ..plain("j") }));
        assert_eq!(parse_keybinding_shortcut("mod++"), Some(KeybindingShortcut { mod_key: true, ..plain("+") }));
        assert_eq!(parse_keybinding_shortcut("+"), Some(plain("+")));
        assert_eq!(parse_keybinding_shortcut("space"), Some(plain(" ")));
        assert_eq!(parse_keybinding_shortcut("Esc"), Some(plain("escape")));
        assert_eq!(
            parse_keybinding_shortcut(" Shift + Cmd + R "),
            Some(KeybindingShortcut { shift_key: true, meta_key: true, ..plain("r") })
        );
        assert_eq!(
            parse_keybinding_shortcut("control+option+meta+x"),
            Some(KeybindingShortcut { ctrl_key: true, alt_key: true, meta_key: true, ..plain("x") })
        );
        assert_eq!(parse_keybinding_shortcut("mod+shift+d+o"), None);
        assert_eq!(parse_keybinding_shortcut("a++b"), None);
        assert_eq!(parse_keybinding_shortcut("mod+shift"), None);
    }

    #[test]
    fn compiles_valid_rule_with_parsed_when_ast() {
        let compiled = compile_resolved_keybinding_rule(&KeybindingRule {
            key: "mod+d".into(),
            command: "terminal.split".into(),
            when: Some("terminalOpen && !terminalFocus".into()),
        });
        assert_eq!(
            compiled,
            Some(ResolvedKeybindingRule {
                command: "terminal.split".into(),
                shortcut: sc("mod+d"),
                when_ast: Some(and(ident("terminalOpen"), not(ident("terminalFocus")))),
            })
        );
    }

    #[test]
    fn rejects_invalid_rules() {
        let rule =
            |key: &str, when: Option<String>| KeybindingRule { key: key.into(), command: "terminal.new".into(), when };
        assert_eq!(compile_resolved_keybinding_rule(&rule("mod+shift+d+o", None)), None);
        assert_eq!(compile_resolved_keybinding_rule(&rule("mod+d", Some("terminalFocus && (".into()))), None);
        let many_nots = format!("{}terminalFocus", "!".repeat(300));
        assert_eq!(compile_resolved_keybinding_rule(&rule("mod+d", Some(many_nots))), None);
    }

    #[test]
    fn parses_when_expressions_with_precedence_and_parentheses() {
        let parse = |expression| parse_keybinding_when_expression(expression);
        assert_eq!(parse("a || b && !c"), Some(or(ident("a"), and(ident("b"), not(ident("c"))))));
        assert_eq!(parse("(a || b) && c"), Some(and(or(ident("a"), ident("b")), ident("c"))));
        assert_eq!(parse("a && b && c"), Some(and(and(ident("a"), ident("b")), ident("c"))));
        assert_eq!(parse("terminalFocus&&!previewFocus"), parse("terminalFocus && !previewFocus"));
        assert_eq!(parse("!!x.y-z_1"), Some(not(not(ident("x.y-z_1")))));
    }

    #[test]
    fn limits_when_expression_depth() {
        let nested = |depth: usize| format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
        assert!(parse_keybinding_when_expression(&nested(MAX_WHEN_EXPRESSION_DEPTH)).is_some());
        assert_eq!(parse_keybinding_when_expression(&nested(MAX_WHEN_EXPRESSION_DEPTH + 1)), None);
        let nots = |count: usize| format!("{}a", "!".repeat(count));
        assert!(parse_keybinding_when_expression(&nots(MAX_WHEN_EXPRESSION_DEPTH)).is_some());
        assert_eq!(parse_keybinding_when_expression(&nots(MAX_WHEN_EXPRESSION_DEPTH + 1)), None);
    }

    #[test]
    fn rejects_invalid_when_expressions() {
        for expression in [
            "",
            "   ",
            "a &&",
            "&& a",
            "a & b",
            "a | b",
            "(a",
            "a)",
            "a b",
            "1a",
            "a || || b",
            "!",
            "()",
            "a == b",
            "ä",
        ] {
            assert_eq!(parse_keybinding_when_expression(expression), None, "{expression:?}");
        }
    }

    #[test]
    fn literal_true_and_false_identifiers_ignore_context() {
        let keybindings = compile(&[("mod+k", "chat.find", "true"), ("mod+k", "diff.toggle", "false")]);
        let context = [("true", false), ("false", true)];
        assert_eq!(resolve(ev("k", "meta"), &keybindings, &context, MAC), Some("chat.find"));
    }

    #[test]
    fn builds_conflict_keys_with_mod_resolved_per_platform() {
        assert_eq!(shortcut_conflict_key(&sc("mod+shift+k"), MAC), "k|meta||shift|");
        assert_eq!(shortcut_conflict_key(&sc("mod+shift+k"), LINUX), "k||ctrl|shift|");
        assert_eq!(shortcut_conflict_key(&sc("cmd+k"), MAC), shortcut_conflict_key(&sc("mod+k"), MAC));
        assert_eq!(shortcut_conflict_key(&sc("alt+arrowup"), LINUX), "arrowup||||alt");
    }

    #[test]
    fn formats_labels_on_mac_and_elsewhere() {
        assert_eq!(format_shortcut_label(&sc("ctrl+alt+shift+meta+k"), MAC), "⌃⌥⇧⌘K");
        assert_eq!(format_shortcut_label(&sc("ctrl+alt+shift+meta+k"), LINUX), "Ctrl+Alt+Shift+Meta+K");
        assert_eq!(format_shortcut_label(&sc("mod+shift+["), MAC), "⇧⌘[");
        assert_eq!(format_shortcut_label(&sc("mod+shift+["), WIN), "Ctrl+Shift+[");
        assert_eq!(format_shortcut_label(&sc("f6"), WIN), "F6");
        for (key, label) in [
            (" ", "Space"),
            ("escape", "Esc"),
            ("arrowup", "Up"),
            ("arrowdown", "Down"),
            ("arrowleft", "Left"),
            ("arrowright", "Right"),
            ("enter", "Enter"),
            ("в", "В"),
        ] {
            assert_eq!(format_shortcut_key_label(key), label);
        }
    }

    #[test]
    fn maps_jump_and_traversal_commands() {
        assert_eq!(thread_jump_command_for_index(0), Some("thread.jump.1"));
        assert_eq!(thread_jump_command_for_index(8), Some("thread.jump.9"));
        assert_eq!(thread_jump_command_for_index(9), None);
        assert_eq!(thread_jump_index_from_command("thread.jump.9"), Some(8));
        assert_eq!(thread_jump_index_from_command("thread.jump.10"), None);
        assert_eq!(model_picker_jump_command_for_index(2), Some("modelPicker.jump.3"));
        assert_eq!(model_picker_jump_index_from_command("modelPicker.jump.3"), Some(2));
        assert_eq!(model_picker_jump_index_from_command("thread.jump.3"), None);
        assert_eq!(
            thread_traversal_direction_from_command("thread.previous"),
            Some(ThreadTraversalDirection::Previous)
        );
        assert_eq!(thread_traversal_direction_from_command("thread.next"), Some(ThreadTraversalDirection::Next));
        assert_eq!(thread_traversal_direction_from_command("thread.jump.1"), None);
    }

    #[test]
    fn defaults_compile_with_known_commands() {
        let raw = default_keybindings();
        assert_eq!(defaults().len(), raw.len());
        assert!(raw.iter().all(|rule| is_keybinding_command(&rule.command)));
        assert_eq!(raw[0], KeybindingRule { key: "mod+b".into(), command: "sidebar.toggle".into(), when: None });
        let jump = raw.iter().find(|rule| rule.command == "thread.jump.9").unwrap();
        assert_eq!((jump.key.as_str(), jump.when.as_deref()), ("mod+9", Some("!modelPickerOpen && isDesktop")));
        assert_eq!(raw.last().map(|rule| rule.command.as_str()), Some("usage.period.quarter"));
        for command in THREAD_JUMP_KEYBINDING_COMMANDS.iter().chain(&MODEL_PICKER_JUMP_KEYBINDING_COMMANDS) {
            assert!(STATIC_KEYBINDING_COMMANDS.contains(command));
        }
    }

    #[test]
    fn validates_script_commands() {
        assert!(is_keybinding_command("script.run-tests.run"));
        assert!(is_keybinding_command("script.0.run"));
        assert!(is_keybinding_command(&format!("script.{}.run", "a".repeat(MAX_SCRIPT_ID_LENGTH))));
        let too_long = format!("script.{}.run", "a".repeat(MAX_SCRIPT_ID_LENGTH + 1));
        for command in [
            "script..run",
            "script.-a.run",
            "script.Setup.run",
            "script.a.b.run",
            "script.a.start",
            "invalid.command",
            too_long.as_str(),
        ] {
            assert!(!is_keybinding_command(command), "{command}");
        }
        assert_eq!(project_script_id_from_command(" script.setup.run "), Some("setup"));
        assert_eq!(project_script_id_from_command("terminal.toggle"), None);
    }

    #[test]
    fn merges_custom_rules_over_defaults() {
        assert_eq!(merge_with_default_keybindings(&[]), defaults());

        let custom = compile(&[("mod+shift+y", "terminal.toggle", "")]);
        let merged = merge_with_default_keybindings(&custom);
        assert_eq!(merged.len(), defaults().len());
        assert_eq!(merged.last(), custom.last());
        assert_eq!(merged.iter().filter(|binding| binding.command == "terminal.toggle").count(), 1);

        let fillers: Vec<_> = (0..MAX_KEYBINDINGS_COUNT)
            .map(|index| ResolvedKeybindingRule {
                command: format!("script.filler-{index}.run"),
                shortcut: sc("mod+alt+f1"),
                when_ast: None,
            })
            .collect();
        assert_eq!(merge_with_default_keybindings(&fillers), fillers);
    }

    #[test]
    fn compiling_keeps_the_last_rules_past_the_limit() {
        let rules: Vec<_> = (0..MAX_KEYBINDINGS_COUNT + 2)
            .map(|index| KeybindingRule { key: "mod+x".into(), command: format!("script.s{index}.run"), when: None })
            .collect();
        let compiled = compile_resolved_keybindings_config(&rules);
        assert_eq!(compiled.len(), MAX_KEYBINDINGS_COUNT);
        assert_eq!(compiled[0].command, "script.s2.run");
    }

    #[test]
    fn decodes_wire_rules_and_skips_entries_it_cannot_decode() {
        let shortcut = json!({ "key": "d", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true });
        let wire = json!([
            { "command": "terminal.split", "shortcut": shortcut, "whenAst": {
                "type": "and",
                "left": { "type": "identifier", "name": "terminalOpen" },
                "right": { "type": "not", "node": { "type": "identifier", "name": "terminalFocus" } }
            }, "futureField": 1 },
            { "command": "command.from-a-newer-build", "shortcut": shortcut },
            { "command": "diff.toggle", "shortcut": { "key": "d" } },
            { "command": "diff.toggle", "shortcut": shortcut, "whenAst": { "type": "xor", "left": {}, "right": {} } },
            { "command": "diff.toggle", "shortcut": shortcut, "whenAst": { "type": "identifier", "name": "" } },
            { "command": "diff.toggle", "shortcut": { "key": "   ", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } },
            { "command": "diff.toggle", "shortcut": { "key": "x".repeat(65), "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } },
            "not an object",
            { "command": "script.run-tests.run", "shortcut": { "key": " r ", "metaKey": true, "ctrlKey": false, "shiftKey": true, "altKey": false, "modKey": false } },
        ]);
        let decoded = decode_resolved_keybindings(&wire);
        assert_eq!(
            decoded,
            vec![
                ResolvedKeybindingRule {
                    command: "terminal.split".into(),
                    shortcut: sc("mod+d"),
                    when_ast: Some(and(ident("terminalOpen"), not(ident("terminalFocus")))),
                },
                ResolvedKeybindingRule {
                    command: "script.run-tests.run".into(),
                    shortcut: sc("meta+shift+r"),
                    when_ast: None
                },
            ]
        );
        assert!(decode_resolved_keybindings(&json!({ "not": "an array" })).is_empty());
    }

    #[test]
    fn decoding_keeps_the_last_rules_past_the_limit() {
        let shortcut = json!({ "key": "x", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true });
        let wire: Vec<_> = (0..MAX_KEYBINDINGS_COUNT + 1)
            .map(|index| json!({ "command": format!("script.s{index}.run"), "shortcut": shortcut }))
            .collect();
        let decoded = decode_resolved_keybindings(&Value::Array(wire));
        assert_eq!(decoded.len(), MAX_KEYBINDINGS_COUNT);
        assert_eq!(decoded[0].command, "script.s1.run");
    }

    #[test]
    fn translates_shortcuts_to_gpui_keystrokes() {
        let cases: &[(&str, bool, &[&str])] = &[
            ("mod+shift+[", MAC, &["cmd-shift-[", "cmd-{"]),
            ("mod+shift+[", LINUX, &["ctrl-shift-["]),
            ("mod+shift+1", MAC, &["cmd-shift-1", "cmd-!"]),
            ("mod+shift+p", MAC, &["cmd-shift-p"]),
            ("mod+k", LINUX, &["ctrl-k"]),
            ("mod+k", MAC, &["cmd-k"]),
            ("meta+k", LINUX, &["cmd-k"]),
            ("alt+arrowup", MAC, &["alt-up"]),
            ("mod+shift+arrowdown", WIN, &["ctrl-shift-down"]),
            ("ctrl+alt+shift+cmd+arrowleft", MAC, &["ctrl-alt-cmd-shift-left"]),
            ("mod+alt+shift+t", MAC, &["alt-cmd-shift-t"]),
            ("mod+-", MAC, &["cmd--"]),
            ("mod++", MAC, &["cmd-+"]),
            ("mod+=", LINUX, &["ctrl-="]),
            ("space", MAC, &["space"]),
            ("esc", MAC, &["escape"]),
            ("mod+enter", MAC, &["cmd-enter"]),
            ("shift+f6", MAC, &["shift-f6"]),
        ];
        for &(shortcut, mac, expected) in cases {
            assert_eq!(gpui_keystrokes(&sc(shortcut), mac), expected, "{shortcut} mac={mac}");
        }
    }

    #[test]
    fn translates_when_clauses_to_gpui_predicates() {
        let predicate = |expression| gpui_predicate(&parse_keybinding_when_expression(expression).unwrap());
        assert_eq!(predicate("!terminalFocus && isDesktop"), "(!(!(!terminalFocus)) && !(!isDesktop))");
        assert_eq!(predicate("a || !(b && c)"), "(!(!a) || !((!(!b) && !(!c))))");
        assert_eq!(predicate("true"), "!__t3_never__");
        assert_eq!(predicate("false"), "!(!__t3_never__)");
    }
}
