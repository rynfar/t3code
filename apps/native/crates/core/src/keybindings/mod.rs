//! Keybindings: `packages/contracts/src/keybindings.ts`, `packages/shared/src/keybindings.ts`, and
//! `apps/web/src/keybindings.ts` (matching, labels, jump helpers), plus translation of resolved
//! rules into GPUI keystrokes and context predicates.

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

mod resolve;
pub use resolve::*;

#[cfg(test)]
mod tests;
