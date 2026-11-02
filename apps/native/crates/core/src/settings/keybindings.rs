//! The Keybindings panel's read-only table: the environment's resolved keymap
//! (`ResolvedKeybindingsConfig` in `contracts/src/keybindings.ts`) grouped by command, labelled
//! with `commandLabel` from `web/src/components/settings/KeybindingsSettings.logic.ts` and
//! `formatShortcutLabel` from `web/src/keybindings.ts`. Rules that fail to decode are dropped.

use serde::Deserialize;
use serde_json::Value;

use super::search::normalize;

/// `KeybindingShortcut`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shortcut {
    pub key: String,
    #[serde(default)]
    pub meta_key: bool,
    #[serde(default)]
    pub ctrl_key: bool,
    #[serde(default)]
    pub shift_key: bool,
    #[serde(default)]
    pub alt_key: bool,
    #[serde(default)]
    pub mod_key: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ResolvedRule {
    command: String,
    shortcut: Shortcut,
    #[serde(default, rename = "whenAst")]
    when_ast: Option<Value>,
}

/// One command and every shortcut bound to it, in keymap order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingRow {
    pub command: String,
    pub label: String,
    pub shortcuts: Vec<String>,
    /// Some binding only applies in a context (`when`).
    pub conditional: bool,
}

/// `formatShortcutKeyLabel`.
pub fn key_label(key: &str) -> String {
    match key {
        " " => "Space".to_owned(),
        "escape" => "Esc".to_owned(),
        "arrowup" => "Up".to_owned(),
        "arrowdown" => "Down".to_owned(),
        "arrowleft" => "Left".to_owned(),
        "arrowright" => "Right".to_owned(),
        _ if key.chars().count() == 1 => key.to_uppercase(),
        _ => {
            let mut chars = key.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

/// `formatShortcutLabel`: macOS glyphs in ⌃⌥⇧⌘ order, `Ctrl+Alt+Shift+Meta+Key` elsewhere.
pub fn shortcut_label(shortcut: &Shortcut, mac: bool) -> String {
    let key = key_label(&shortcut.key);
    let meta = shortcut.meta_key || (shortcut.mod_key && mac);
    let ctrl = shortcut.ctrl_key || (shortcut.mod_key && !mac);
    if mac {
        let glyph = |on: bool, glyph: &'static str| if on { glyph } else { "" };
        return format!(
            "{}{}{}{}{key}",
            glyph(ctrl, "\u{2303}"),
            glyph(shortcut.alt_key, "\u{2325}"),
            glyph(shortcut.shift_key, "\u{21e7}"),
            glyph(meta, "\u{2318}")
        );
    }
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if shortcut.alt_key {
        parts.push("Alt");
    }
    if shortcut.shift_key {
        parts.push("Shift");
    }
    if meta {
        parts.push("Meta");
    }
    parts.push(&key);
    parts.join("+")
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
    if let Some(script) = command.strip_prefix("script.").and_then(|rest| rest.strip_suffix(".run")) {
        return format!("Run Script: {}", title_case_segment(script));
    }
    command.split('.').map(title_case_segment).collect::<Vec<_>>().join(": ")
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

/// The table: one row per bound command, sorted by label.
pub fn rows(keybindings: &Value, mac: bool) -> Vec<KeybindingRow> {
    let Value::Array(rules) = keybindings else { return Vec::new() };
    let mut rows: Vec<KeybindingRow> = Vec::new();
    for rule in rules.iter().filter_map(|rule| serde_json::from_value::<ResolvedRule>(rule.clone()).ok()) {
        let label = shortcut_label(&rule.shortcut, mac);
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

/// The table filter: every token in the label, the command id, or a shortcut.
pub fn filter<'a>(rows: &'a [KeybindingRow], query: &str) -> Vec<&'a KeybindingRow> {
    let query = normalize(query);
    if query.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|row| {
            let fields = [normalize(&row.label), normalize(&row.command), normalize(&row.shortcuts.join(" "))];
            query.split(' ').all(|token| fields.iter().any(|field| field.contains(token)))
        })
        .collect()
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

/// `groupKeybindingRows`: groups in the web's order, empty ones left out.
pub fn groups<'a>(rows: &[&'a KeybindingRow]) -> Vec<(&'static str, Vec<&'a KeybindingRow>)> {
    let group_of = |row: &KeybindingRow| {
        let prefix = row.command.split('.').next().unwrap_or_default();
        GROUPS.iter().find(|(_, prefixes)| prefixes.contains(&prefix)).map_or("Other", |(title, _)| title)
    };
    GROUPS
        .iter()
        .map(|(title, _)| *title)
        .chain(["Other"])
        .filter_map(|title| {
            let members: Vec<&KeybindingRow> = rows.iter().copied().filter(|row| group_of(row) == title).collect();
            (!members.is_empty()).then_some((title, members))
        })
        .collect()
}
