//! The Keybindings panel: `web/src/components/settings/KeybindingsSettings.logic.ts` and the
//! editing logic of `KeybindingsSettings.tsx`. `rows` summarizes each command for settings search;
//! `binding_rows` is the editable table, one row per binding with its source, default, and
//! conflicts. The when builder edits a tree addressed by child paths (`apply_when_edit`), and
//! edits go out as `server.upsertKeybinding` / `server.removeKeybinding` payloads.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::keybindings::{
    KeybindingShortcut, KeybindingWhenNode, ResolvedKeybindingRule, STATIC_KEYBINDING_COMMANDS, US_SHIFTED_TO_UNSHIFTED_KEYS,
    decode_resolved_keybindings, default_resolved_keybindings, format_shortcut_label, merge_with_default_keybindings, parse_keybinding_shortcut,
    parse_keybinding_when_expression, shortcut_conflict_key,
};

pub const SERVER_UPSERT_KEYBINDING: &str = "server.upsertKeybinding";
pub const SERVER_REMOVE_KEYBINDING: &str = "server.removeKeybinding";

/// One command and every shortcut bound to it, in keymap order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingRow {
    pub command: String,
    pub label: String,
    pub shortcuts: Vec<String>,
    /// Some binding only applies in a context (`when`).
    pub conditional: bool,
}

/// `commandLabel` for commands without a bespoke label: segments title-cased and joined by ": ".
pub fn command_label(command: &str) -> String {
    match command {
        "workspace.focusNext" => return "Focus Next Workspace Area".to_owned(),
        "workspace.focusPrevious" => return "Focus Previous Workspace Area".to_owned(),
        "composer.sendAlternate" => return "Composer: Opposite Queue or Steer Action".to_owned(),
        "composer.sendBackground" => return "Composer: Start in Background".to_owned(),
        "composer.sendAndNewThread" => return "Composer: Send and Start New Thread".to_owned(),
        "thread.steerQueuedMessage" => return "Queue: Send First Queued Message".to_owned(),
        "thread.editQueuedMessage" => return "Queue: Edit Last Queued Message".to_owned(),
        "thread.copyReference" => return "Pull Request: Copy Link or Thread".to_owned(),
        "view.reopenClosed" => return "Reopen Closed Tab".to_owned(),
        "preview.focusUrl" => return "Preview: Focus URL".to_owned(),
        _ => {}
    }
    if let Some((_, label)) = USAGE_COMMANDS.iter().find(|(id, _)| *id == command) {
        return (*label).to_owned();
    }
    if let Some(script) = command.strip_prefix("script.").and_then(|rest| rest.strip_suffix(".run")) {
        return format!("Run Script: {}", title_case_segment(script));
    }
    command.split('.').map(title_case_segment).collect::<Vec<_>>().join(": ")
}

/// `usage.open` then `METRIC_OPTIONS` and `WINDOW_OPTIONS` (`usage/usageShortcuts.ts`), with
/// their `commandLabel`s; the web ranks these commands in this order.
const USAGE_COMMANDS: &[(&str, &str)] = &[
    ("usage.open", "Usage: Open"),
    ("usage.cost", "Usage: Cost"),
    ("usage.tokens", "Usage: Tokens"),
    ("usage.limits", "Usage: Limits"),
    ("usage.period.day", "Usage: Period: Past 24h"),
    ("usage.period.week", "Usage: Period: 7 days"),
    ("usage.period.month", "Usage: Period: 30 days"),
    ("usage.period.quarter", "Usage: Period: 90 days"),
];

/// `compareCommands`: usage commands keep their rank and sort as the first metric among the rest.
fn compare_commands(left: &str, right: &str, key: impl Fn(&str) -> String) -> std::cmp::Ordering {
    let rank = |command: &str| USAGE_COMMANDS.iter().position(|(id, _)| *id == command);
    match (rank(left), rank(right)) {
        (Some(l), Some(r)) => l.cmp(&r),
        (l, r) => {
            let first = USAGE_COMMANDS[1].0;
            crate::thread::locale_compare(&key(if l.is_some() { first } else { left }), &key(if r.is_some() { first } else { right }))
        }
    }
}

/// `titleCaseCommandSegment`: camelCase, kebab, snake and spaces become capitalized words.
fn title_case_segment(segment: &str) -> String {
    let mut spaced = String::with_capacity(segment.len() + 4);
    let mut previous: Option<char> = None;
    for c in segment.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
        .split(['-', '_', ' ', '\t', '\n'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One row per bound command for settings search, sorted by label.
pub fn rows(keybindings: &Value, mac: bool) -> Vec<KeybindingRow> {
    let mut rows: Vec<KeybindingRow> = Vec::new();
    for rule in decode_resolved_keybindings(keybindings) {
        let label = format_shortcut_label(&rule.shortcut, mac);
        let conditional = rule.when_ast.is_some();
        match rows.iter_mut().find(|row| row.command == rule.command) {
            Some(row) => {
                if !row.shortcuts.contains(&label) {
                    row.shortcuts.push(label);
                }
                row.conditional |= conditional;
            }
            None => rows.push(KeybindingRow {
                label: command_label(&rule.command),
                command: rule.command,
                shortcuts: vec![label],
                conditional,
            }),
        }
    }
    rows.sort_by(|left, right| left.label.cmp(&right.label).then(left.command.cmp(&right.command)));
    rows
}

/// `KEYBINDING_GROUPS`: commands group by their first segment, the rest under "Other".
const GROUPS: &[(&str, &[&str])] = &[
    ("Navigation", &["sidebar", "rightPanel", "commandPalette", "filePicker", "projectSearch", "editor"]),
    ("Threads", &["thread", "chat", "pullRequest"]),
    ("Composer", &["composer", "modelPicker"]),
    ("Terminal", &["terminal"]),
    ("Preview & diff", &["preview", "diff"]),
    ("Appearance", &["theme", "appearance", "themeEditor"]),
    ("Project scripts", &["script"]),
];

/// Where a binding comes from (`KeybindingSource`): a default, the user's file, or a project script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    Default,
    Custom,
    Project,
}

impl BindingSource {
    pub fn label(self) -> &'static str {
        match self {
            BindingSource::Default => "Default",
            BindingSource::Custom => "Custom",
            BindingSource::Project => "Project",
        }
    }
}

/// One binding of the editable table (`KeybindingRow`).
#[derive(Debug, Clone, PartialEq)]
pub struct BindingRow {
    pub id: String,
    pub command: String,
    /// The keybinding as typed in `keybindings.json`, such as `mod+shift+k`.
    pub key: String,
    pub when: String,
    pub when_ast: Option<KeybindingWhenNode>,
    pub shortcut: KeybindingShortcut,
    pub source: BindingSource,
    pub default_key: Option<String>,
    pub default_when: String,
    /// Labels of the commands bound to the same chord where both can apply, sorted.
    pub conflicts: Vec<String>,
}

impl BindingRow {
    pub fn label(&self) -> String {
        command_label(&self.command)
    }

    /// The row menu's "Reset to default": a custom binding of a command that has a default.
    pub fn can_reset(&self) -> bool {
        self.source == BindingSource::Custom && self.default_key.is_some()
    }

    /// The row menu's "Remove": anything that is not a default.
    pub fn can_remove(&self) -> bool {
        self.source != BindingSource::Default
    }
}

/// `shortcutToKeybindingInput`.
pub fn shortcut_to_keybinding_input(shortcut: &KeybindingShortcut) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for (held, name) in [(shortcut.mod_key, "mod"), (shortcut.meta_key, "meta"), (shortcut.ctrl_key, "ctrl"), (shortcut.alt_key, "alt"), (shortcut.shift_key, "shift")] {
        if held {
            parts.push(name);
        }
    }
    parts.push(match shortcut.key.as_str() {
        " " => "space",
        "escape" => "esc",
        key => key,
    });
    parts.join("+")
}

/// `whenAstToExpression`.
pub fn when_ast_to_expression(node: Option<&KeybindingWhenNode>) -> String {
    let Some(node) = node else { return String::new() };
    let wrap = |node: &KeybindingWhenNode| match node {
        KeybindingWhenNode::Identifier { .. } | KeybindingWhenNode::Not { .. } => when_ast_to_expression(Some(node)),
        _ => format!("({})", when_ast_to_expression(Some(node))),
    };
    match node {
        KeybindingWhenNode::Identifier { name } => name.clone(),
        KeybindingWhenNode::Not { node } => format!("!{}", wrap(node)),
        KeybindingWhenNode::And { left, right } => format!("{} && {}", wrap(left), wrap(right)),
        KeybindingWhenNode::Or { left, right } => format!("{} || {}", wrap(left), wrap(right)),
    }
}

/// `parseWhenExpressionDraft`: empty means always; anything else must parse.
pub fn parse_when_draft(expression: &str) -> Result<Option<KeybindingWhenNode>, &'static str> {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    parse_keybinding_when_expression(trimmed).map(Some).ok_or("Use variables with !, &&, ||, and parentheses.")
}

fn same_binding(left: &ResolvedKeybindingRule, right: &ResolvedKeybindingRule) -> bool {
    left.command == right.command
        && shortcut_to_keybinding_input(&left.shortcut) == shortcut_to_keybinding_input(&right.shortcut)
        && when_ast_to_expression(left.when_ast.as_ref()) == when_ast_to_expression(right.when_ast.as_ref())
}

/// `sourceForBinding`.
fn source_for(binding: &ResolvedKeybindingRule) -> BindingSource {
    if binding.command.starts_with("script.") {
        BindingSource::Project
    } else if default_resolved_keybindings().iter().any(|entry| same_binding(entry, binding)) {
        BindingSource::Default
    } else {
        BindingSource::Custom
    }
}

/// `defaultBindingForBinding`: the identical default, else one with the same `when`, else any.
fn default_for(binding: &ResolvedKeybindingRule) -> Option<&'static ResolvedKeybindingRule> {
    let defaults = default_resolved_keybindings();
    let when = when_ast_to_expression(binding.when_ast.as_ref());
    defaults
        .iter()
        .find(|entry| same_binding(entry, binding))
        .or_else(|| defaults.iter().find(|entry| entry.command == binding.command && when_ast_to_expression(entry.when_ast.as_ref()) == when))
        .or_else(|| defaults.iter().find(|entry| entry.command == binding.command))
}

/// `conflictsWithWhen`: an unconditional binding overlaps everything; otherwise only equal clauses.
fn conflicts_with_when(left: &str, right: &str) -> bool {
    left.is_empty() || right.is_empty() || left == right
}

/// `keybindingConflictLabels`: the commands that resolve to the same chord as `key` where both
/// conditions can apply, other than `row_id` itself.
pub fn conflict_labels(rows: &[BindingRow], row_id: &str, key: &str, when: &str, mac: bool) -> Vec<String> {
    if key.trim().is_empty() {
        return Vec::new();
    }
    let Some(shortcut) = parse_keybinding_shortcut(key) else { return Vec::new() };
    let target = shortcut_conflict_key(&shortcut, mac);
    let labels: BTreeSet<String> = rows
        .iter()
        .filter(|row| row.id != row_id && shortcut_conflict_key(&row.shortcut, mac) == target && conflicts_with_when(&row.when, when))
        .map(BindingRow::label)
        .collect();
    labels.into_iter().collect()
}

/// `buildKeybindingRows` over `mergeWithDefaultKeybindings(serverKeybindings)`.
pub fn binding_rows(server_keybindings: &Value, mac: bool) -> Vec<BindingRow> {
    let merged = merge_with_default_keybindings(&decode_resolved_keybindings(server_keybindings));
    let mut rows: Vec<BindingRow> = Vec::new();
    for binding in merged {
        let key = shortcut_to_keybinding_input(&binding.shortcut);
        let when = when_ast_to_expression(binding.when_ast.as_ref());
        let id = format!("{}\u{0}{key}\u{0}{when}", binding.command);
        let default = default_for(&binding);
        let row = BindingRow {
            source: source_for(&binding),
            default_key: default.map(|entry| shortcut_to_keybinding_input(&entry.shortcut)),
            default_when: when_ast_to_expression(default.and_then(|entry| entry.when_ast.as_ref())),
            command: binding.command,
            shortcut: binding.shortcut,
            when_ast: binding.when_ast,
            conflicts: Vec::new(),
            id,
            key,
            when,
        };
        // A later identical binding replaces the earlier one in place, as the web's Map does.
        match rows.iter_mut().find(|existing| existing.id == row.id) {
            Some(existing) => *existing = row,
            None => rows.push(row),
        }
    }
    let conflicts: Vec<Vec<String>> = rows.iter().map(|row| conflict_labels(&rows, &row.id, &row.key, &row.when, mac)).collect();
    for (row, conflicts) in rows.iter_mut().zip(conflicts) {
        row.conflicts = conflicts;
    }
    rows.sort_by(|left, right| compare_commands(&left.command, &right.command, str::to_owned).then_with(|| crate::thread::locale_compare(&left.key, &right.key)));
    rows
}

/// `filterKeybindingRows`: a substring of the command, label, key, `when`, or source.
pub fn filter_binding_rows<'a>(rows: &'a [BindingRow], query: &str) -> Vec<&'a BindingRow> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|row| {
            [row.command.to_lowercase(), row.label().to_lowercase(), row.key.to_lowercase(), row.when.to_lowercase(), row.source.label().to_lowercase()]
                .iter()
                .any(|field| field.contains(&query))
        })
        .collect()
}

/// `groupKeybindingRows`: groups in the web's order, empty ones left out.
pub fn group_binding_rows<'a>(rows: &[&'a BindingRow]) -> Vec<(&'static str, Vec<&'a BindingRow>)> {
    let group_of = |command: &str| {
        let prefix = command.split('.').next().unwrap_or_default();
        GROUPS.iter().find(|(_, prefixes)| prefixes.contains(&prefix)).map_or("Other", |(title, _)| title)
    };
    GROUPS
        .iter()
        .map(|(title, _)| *title)
        .chain(["Other"])
        .filter_map(|title| {
            let members: Vec<&BindingRow> = rows.iter().copied().filter(|row| group_of(&row.command) == title).collect();
            (!members.is_empty()).then_some((title, members))
        })
        .collect()
}

/// `buildKeybindingCommandOptions`: every static command plus whatever the keymap binds, by label.
pub fn command_options(server_keybindings: &Value) -> Vec<String> {
    let merged = merge_with_default_keybindings(&decode_resolved_keybindings(server_keybindings));
    let mut commands: Vec<String> = STATIC_KEYBINDING_COMMANDS.iter().map(|command| (*command).to_owned()).collect();
    for binding in merged {
        if !commands.contains(&binding.command) {
            commands.push(binding.command);
        }
    }
    commands.sort_by(|left, right| compare_commands(left, right, command_label));
    commands
}

const CORE_WHEN_VARIABLES: [&str; 6] = ["terminalFocus", "terminalOpen", "isWeb", "isDesktop", "true", "false"];

fn collect_identifiers(node: &KeybindingWhenNode, into: &mut Vec<String>) {
    match node {
        KeybindingWhenNode::Identifier { name } => {
            if !into.contains(name) {
                into.push(name.clone());
            }
        }
        KeybindingWhenNode::Not { node } => collect_identifiers(node, into),
        KeybindingWhenNode::And { left, right } | KeybindingWhenNode::Or { left, right } => {
            collect_identifiers(left, into);
            collect_identifiers(right, into);
        }
    }
}

/// `KNOWN_WHEN_VARIABLES`: the core variables, then every identifier a default binding uses.
fn known_when_variables() -> Vec<String> {
    let mut known: Vec<String> = CORE_WHEN_VARIABLES.iter().map(|name| (*name).to_owned()).collect();
    for binding in default_resolved_keybindings() {
        if let Some(when) = &binding.when_ast {
            collect_identifiers(when, &mut known);
        }
    }
    known
}

/// `DEFAULT_WHEN_VARIABLE`: the first known variable that is not a literal.
pub fn default_when_variable() -> String {
    known_when_variables().into_iter().find(|name| name != "true" && name != "false").unwrap_or_else(|| "terminalFocus".to_owned())
}

pub fn is_known_when_variable(identifier: &str) -> bool {
    known_when_variables().iter().any(|name| name == identifier)
}

/// `unknownWhenVariables`, sorted.
pub fn unknown_when_variables(node: Option<&KeybindingWhenNode>) -> Vec<String> {
    let mut identifiers = Vec::new();
    if let Some(node) = node {
        collect_identifiers(node, &mut identifiers);
    }
    let mut unknown: Vec<String> = identifiers.into_iter().filter(|name| !is_known_when_variable(name)).collect();
    unknown.sort();
    unknown
}

/// `buildWhenVariableOptions`: core variables in their order, then the rest alphabetically.
pub fn when_variable_options() -> Vec<String> {
    let mut options = known_when_variables();
    let core_index = |name: &str| CORE_WHEN_VARIABLES.iter().position(|core| *core == name).unwrap_or(usize::MAX);
    options.sort_by(|left, right| core_index(left).cmp(&core_index(right)).then_with(|| crate::thread::locale_compare(left, right)));
    options
}

/// `unknownWhenVariable` tooltip label.
pub fn unknown_when_label(identifiers: &[String]) -> Option<String> {
    match identifiers {
        [] => None,
        [one] => Some(format!("Unknown condition: {one}")),
        many => Some(format!("Unknown conditions: {}", many.join(", "))),
    }
}

/// `KeybindingConflictWarning`'s label: up to three commands, then "and more".
pub fn conflict_description(labels: &[String]) -> Option<String> {
    match labels {
        [] => None,
        [one] => Some(format!("Conflicts with {one}.")),
        many => Some(format!("Conflicts with {}{}.", many[..many.len().min(3)].join(", "), if many.len() > 3 { ", and more" } else { "" })),
    }
}

/// `keybindingFromKeyboardEvent` with `layoutCharacter` for a GPUI keystroke: `mod` is Cmd on
/// macOS and Ctrl elsewhere. GPUI on macOS reports Shift+symbol as the shifted glyph without
/// Shift, which records as Shift and the US key under it. Modifier-only or unnamed keys give `None`.
pub fn keybinding_from_keystroke(key: &str, platform: bool, control: bool, alt: bool, shift: bool, mac: bool) -> Option<String> {
    let lowered = key.to_lowercase();
    let (token, shift) = match US_SHIFTED_TO_UNSHIFTED_KEYS.iter().find(|(shifted, _)| *shifted == lowered) {
        Some((_, unshifted)) if !alt => ((*unshifted).to_owned(), true),
        _ => (lowered, shift),
    };
    let token = match token.as_str() {
        "up" | "arrowup" => "arrowup".to_owned(),
        "down" | "arrowdown" => "arrowdown".to_owned(),
        "left" | "arrowleft" => "arrowleft".to_owned(),
        "right" | "arrowright" => "arrowright".to_owned(),
        " " | "space" => "space".to_owned(),
        "escape" | "esc" => "esc".to_owned(),
        "enter" | "tab" | "backspace" | "delete" | "home" | "end" | "pageup" | "pagedown" => token,
        _ if token.chars().count() == 1 => token,
        _ if token.len() <= 3 && token.starts_with('f') && token[1..].chars().all(|c| c.is_ascii_digit()) && token.len() > 1 => token,
        _ => return None,
    };
    let mut parts: Vec<&str> = Vec::new();
    if mac {
        if platform {
            parts.push("mod");
        }
        if control {
            parts.push("ctrl");
        }
    } else {
        if control {
            parts.push("mod");
        }
        if platform {
            parts.push("meta");
        }
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    parts.push(&token);
    Some(parts.join("+"))
}

/// The binding a replace or remove targets (`rowKeybindingTarget`).
fn target(command: &str, key: &str, when: &str) -> Value {
    let mut target = Map::new();
    target.insert("command".into(), json!(command));
    target.insert("key".into(), json!(key));
    if !when.trim().is_empty() {
        target.insert("when".into(), json!(when));
    }
    Value::Object(target)
}

/// `server.upsertKeybinding`, trimmed as `saveKeybinding` sends it, replacing `replace` if given.
pub fn upsert_payload(command: &str, key: &str, when: &str, replace: Option<&BindingRow>) -> Value {
    let mut payload = target(command, key.trim(), when.trim());
    if let Some(row) = replace {
        payload["replace"] = target(&row.command, &row.key, &row.when);
    }
    payload
}

/// `server.removeKeybinding` for `row`.
pub fn remove_payload(row: &BindingRow) -> Value {
    target(&row.command, &row.key, &row.when)
}

/// `resetKeybinding`: the default key and `when` in place of the custom binding.
pub fn reset_payload(row: &BindingRow) -> Option<Value> {
    row.default_key.as_deref().map(|key| upsert_payload(&row.command, key, &row.default_when, Some(row)))
}

/// The `keybindings.json` key pill (`KeybindingPill`): one cap per part, platform glyphs for
/// modifiers, single characters upper-cased.
pub fn key_pill_parts(key: &str, mac: bool) -> Vec<String> {
    key.split('+')
        .map(|part| match part {
            "mod" => (if mac { "⌘" } else { "Ctrl" }).to_owned(),
            "shift" => "⇧".to_owned(),
            "alt" => (if mac { "⌥" } else { "Alt" }).to_owned(),
            "ctrl" => "⌃".to_owned(),
            part if part.chars().count() == 1 => part.to_uppercase(),
            part => part.to_owned(),
        })
        .collect()
}

/// Boolean operator of a when group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhenOperator {
    And,
    Or,
}

/// What the when builder does at a node, addressed by its path of child indexes from the root.
/// A group's children are its flattened operands; a negated group has its operand at index 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhenEdit {
    SetIdentifier(String),
    SetNegated(bool),
    SetOperator(WhenOperator),
    AddCondition,
    AddGroup,
    Remove,
}

pub fn default_when_condition() -> KeybindingWhenNode {
    KeybindingWhenNode::Identifier { name: default_when_variable() }
}

/// `defaultWhenGroup`: `x <op> !x`.
pub fn default_when_group(operator: WhenOperator) -> KeybindingWhenNode {
    let negated = KeybindingWhenNode::Not { node: Box::new(default_when_condition()) };
    group_node(operator, default_when_condition(), negated)
}

fn group_node(operator: WhenOperator, left: KeybindingWhenNode, right: KeybindingWhenNode) -> KeybindingWhenNode {
    let (left, right) = (Box::new(left), Box::new(right));
    match operator {
        WhenOperator::And => KeybindingWhenNode::And { left, right },
        WhenOperator::Or => KeybindingWhenNode::Or { left, right },
    }
}

/// The operator of a group node.
pub fn when_operator(node: &KeybindingWhenNode) -> Option<WhenOperator> {
    match node {
        KeybindingWhenNode::And { .. } => Some(WhenOperator::And),
        KeybindingWhenNode::Or { .. } => Some(WhenOperator::Or),
        _ => None,
    }
}

/// `flattenWhenChildren`: a chain of the same operator as one list of operands.
pub fn flatten_when(node: &KeybindingWhenNode, operator: WhenOperator) -> Vec<KeybindingWhenNode> {
    flatten_refs(node, operator).into_iter().cloned().collect()
}

/// `buildWhenExpressionGroup`: operands folded left; one operand is itself.
pub fn build_when_group(children: Vec<KeybindingWhenNode>, operator: WhenOperator) -> Option<KeybindingWhenNode> {
    let mut children = children.into_iter();
    let first = children.next()?;
    Some(children.fold(first, |left, right| group_node(operator, left, right)))
}

/// `conditionParts`: an identifier, possibly negated.
pub fn condition_parts(node: &KeybindingWhenNode) -> Option<(&str, bool)> {
    match node {
        KeybindingWhenNode::Identifier { name } => Some((name, false)),
        KeybindingWhenNode::Not { node } => match node.as_ref() {
            KeybindingWhenNode::Identifier { name } => Some((name, true)),
            _ => None,
        },
        _ => None,
    }
}

/// The operands the builder shows under a node: a group's flattened children, or the operand of
/// a negated group. Conditions have none.
pub fn when_children(node: &KeybindingWhenNode) -> Vec<KeybindingWhenNode> {
    if condition_parts(node).is_some() {
        return Vec::new();
    }
    match node {
        KeybindingWhenNode::Not { node } => vec![node.as_ref().clone()],
        _ => when_operator(node).map(|operator| flatten_when(node, operator)).unwrap_or_default(),
    }
}

/// `whenNodeRemoveLabel`.
pub fn when_remove_label(node: &KeybindingWhenNode, depth: usize) -> &'static str {
    if depth == 0 {
        "Clear all conditions"
    } else if condition_parts(node).is_some() {
        "Remove condition"
    } else {
        "Remove group and its conditions"
    }
}

/// Whether the builder offers a remove button at `path`: the root, and every operand of a group
/// (the operand of a negated group is removed with its group).
pub fn when_can_remove(root: &KeybindingWhenNode, path: &[usize]) -> bool {
    let Some((_, parent_path)) = path.split_last() else { return true };
    node_at(root, parent_path).is_some_and(|parent| when_operator(parent).is_some())
}

/// The node at `path`.
pub fn node_at<'a>(root: &'a KeybindingWhenNode, path: &[usize]) -> Option<&'a KeybindingWhenNode> {
    let Some((&first, rest)) = path.split_first() else { return Some(root) };
    let child = match root {
        KeybindingWhenNode::Not { node } if condition_parts(root).is_none() => (first == 0).then_some(node.as_ref()),
        _ => when_operator(root).and_then(|operator| flatten_refs(root, operator).get(first).copied()),
    }?;
    node_at(child, rest)
}

fn flatten_refs(node: &KeybindingWhenNode, operator: WhenOperator) -> Vec<&KeybindingWhenNode> {
    match (node, operator) {
        (KeybindingWhenNode::And { left, right }, WhenOperator::And) | (KeybindingWhenNode::Or { left, right }, WhenOperator::Or) => {
            let mut children = flatten_refs(left, operator);
            children.extend(flatten_refs(right, operator));
            children
        }
        _ => vec![node],
    }
}

/// Applies a when builder edit at `path` and returns the new root (`None` is always). The web's
/// `WhenExpressionNodeEditor` and `WhenExpressionBuilder` handlers, as one pure function.
pub fn apply_when_edit(root: Option<&KeybindingWhenNode>, path: &[usize], edit: &WhenEdit) -> Option<KeybindingWhenNode> {
    let Some(root) = root else {
        // An empty clause takes its first condition or group from the builder's root buttons.
        return match edit {
            WhenEdit::AddCondition => Some(default_when_condition()),
            WhenEdit::AddGroup => Some(default_when_group(WhenOperator::Or)),
            _ => None,
        };
    };
    if path.is_empty() && *edit == WhenEdit::Remove {
        return None;
    }
    Some(edit_at(root, path, edit))
}

/// The root buttons ("+ Condition", "+ Group" beside "When"): and-ed onto the clause.
pub fn add_to_root(root: Option<&KeybindingWhenNode>, group: bool) -> KeybindingWhenNode {
    let addition = if group { default_when_group(WhenOperator::Or) } else { default_when_condition() };
    match root {
        None => addition,
        Some(root) => group_node(WhenOperator::And, root.clone(), addition),
    }
}

fn edit_at(node: &KeybindingWhenNode, path: &[usize], edit: &WhenEdit) -> KeybindingWhenNode {
    if let Some((&index, rest)) = path.split_first() {
        if let KeybindingWhenNode::Not { node: inner } = node
            && condition_parts(node).is_none()
        {
            return KeybindingWhenNode::Not { node: Box::new(edit_at(inner, rest, edit)) };
        }
        let Some(operator) = when_operator(node) else { return node.clone() };
        let mut children = flatten_when(node, operator);
        if index >= children.len() {
            return node.clone();
        }
        if rest.is_empty() && *edit == WhenEdit::Remove {
            children.remove(index);
            return build_when_group(children, operator).unwrap_or_else(default_when_condition);
        }
        children[index] = edit_at(&children[index], rest, edit);
        return build_when_group(children, operator).unwrap_or_else(default_when_condition);
    }
    match edit {
        WhenEdit::SetIdentifier(identifier) => match condition_parts(node) {
            Some((_, negated)) => {
                let next = KeybindingWhenNode::Identifier { name: identifier.clone() };
                if negated { KeybindingWhenNode::Not { node: Box::new(next) } } else { next }
            }
            None => node.clone(),
        },
        WhenEdit::SetNegated(negated) => match (condition_parts(node), node) {
            (Some((name, _)), _) => {
                let identifier = KeybindingWhenNode::Identifier { name: name.to_owned() };
                if *negated { KeybindingWhenNode::Not { node: Box::new(identifier) } } else { identifier }
            }
            (None, KeybindingWhenNode::Not { node: inner }) if !negated => inner.as_ref().clone(),
            (None, KeybindingWhenNode::Not { .. }) => node.clone(),
            (None, _) if *negated => KeybindingWhenNode::Not { node: Box::new(node.clone()) },
            (None, _) => node.clone(),
        },
        WhenEdit::SetOperator(next) => match when_operator(node) {
            Some(operator) if operator != *next => build_when_group(flatten_when(node, operator), *next).unwrap_or_else(|| node.clone()),
            _ => node.clone(),
        },
        WhenEdit::AddCondition | WhenEdit::AddGroup => match when_operator(node) {
            Some(operator) => {
                let mut children = flatten_when(node, operator);
                children.push(if *edit == WhenEdit::AddCondition {
                    default_when_condition()
                } else {
                    default_when_group(if operator == WhenOperator::And { WhenOperator::Or } else { WhenOperator::And })
                });
                build_when_group(children, operator).unwrap_or_else(|| node.clone())
            }
            None => node.clone(),
        },
        WhenEdit::Remove => node.clone(),
    }
}
