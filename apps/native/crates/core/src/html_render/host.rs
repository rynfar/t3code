//! Page theme and bridge from `packages/shared/src/htmlRender.ts` and `apps/web/src/components/files/BrowserDocumentFrame.tsx`.
use crate::keybindings::{self, ResolvedKeybindingRule, ShortcutEvent, ShortcutMatchContext};
use crate::themes::Colors;
use serde_json::{Value, json};

pub const DEFAULT_SANS: &str = r#"-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif"#;
pub const DEFAULT_MONO: &str = r#""SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace"#;
pub fn font_stack(custom: &str, fallback: &str) -> String {
    let families = custom
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let quoted = (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''));
            let bare = s.starts_with(|c: char| c.is_ascii_alphabetic()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if quoted || bare { s.to_owned() } else { format!("\"{}\"", s.replace('\"', "")) }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if families.is_empty() { fallback.into() } else { format!("{families}, {fallback}") }
}

pub fn theme(colors: &Colors, dark: bool, sans: &str, mono: &str) -> Value {
    let mut variables = serde_json::Map::new();
    for (variable, role) in [
        ("--background", "canvas"),
        ("--foreground", "text"),
        ("--muted", "muted"),
        ("--muted-foreground", "mutedForeground"),
        ("--card", "surface"),
        ("--card-foreground", "text"),
        ("--popover", "surfaceOverlay"),
        ("--popover-foreground", "text"),
        ("--secondary", "secondary"),
        ("--secondary-foreground", "secondaryForeground"),
        ("--border", "border"),
        ("--input", "input"),
        ("--ring", "focus"),
        ("--primary", "messageAction"),
        ("--primary-foreground", "messageActionForeground"),
        ("--accent", "accent"),
        ("--accent-foreground", "accentForeground"),
        ("--accent-surface", "accentSurface"),
        ("--accent-surface-foreground", "accentSurfaceForeground"),
        ("--destructive", "error"),
        ("--destructive-foreground", "errorForeground"),
        ("--destructive-surface", "errorSurface"),
        ("--warning", "warning"),
        ("--warning-foreground", "warningForeground"),
        ("--warning-surface", "warningSurface"),
        ("--code-background", "codeBackground"),
        ("--code-foreground", "codeForeground"),
        ("--chart-1", "accent"),
    ] {
        if let Some(color) = colors.get(role) {
            variables.insert(variable.into(), crate::themes::editor::picker::hex(color).unwrap_or_else(|| color.clone()).into());
        }
    }
    let fixed = if dark {
        ["#10b981", "#34d399", "#3b82f6", "#60a5fa", "#2dd4bf", "#fbbf24", "#c084fc", "#fb7185", "#a3e635"]
    } else {
        ["#10b981", "#047857", "#3b82f6", "#1d4ed8", "#0d9488", "#d97706", "#9333ea", "#e11d48", "#65a30d"]
    };
    for (key, value) in ["--success", "--success-foreground", "--info", "--info-foreground", "--chart-2", "--chart-3", "--chart-4", "--chart-5", "--chart-6"].into_iter().zip(fixed)
    {
        variables.insert(key.into(), value.into());
    }
    variables.insert("--radius".into(), "0.625rem".into());
    variables.insert("--font-sans".into(), font_stack(sans, DEFAULT_SANS).into());
    variables.insert("--font-mono".into(), font_stack(mono, DEFAULT_MONO).into());
    json!({"appearance": if dark { "dark" } else { "light" }, "variables": variables})
}
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRequest {
    pub id: Value,
    pub url: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
}
fn params<'a>(value: &'a Value, method: &str) -> Option<&'a Value> {
    (value.get("jsonrpc")?.as_str()? == "2.0" && value.get("method")?.as_str()? == method).then(|| value.get("params")).flatten().filter(|v| v.is_object())
}

pub fn read_link(value: &Value) -> Option<LinkRequest> {
    let p = params(value, "ui/open-link")?;
    let id = value.get("id")?.clone();
    if !id.is_string() && !id.is_number() {
        return None;
    }
    let url = p.get("url")?.as_str()?;
    if !web_url(url) {
        return None;
    }
    Some(LinkRequest { id, url: url.into(), meta_key: p.get("metaKey") == Some(&Value::Bool(true)), ctrl_key: p.get("ctrlKey") == Some(&Value::Bool(true)) })
}
pub fn web_url(url: &str) -> bool {
    url.len() <= 16384
        && !url.chars().any(char::is_whitespace)
        && (url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) || url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")))
        && url::Url::parse(url).ok().is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyDown {
    pub key: String,
    pub code: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub repeat: bool,
}
pub fn read_key(value: &Value) -> Option<KeyDown> {
    let p = params(value, "t3/notifications/keydown")?;
    let key = p.get("key")?.as_str()?.to_owned();
    let code = p.get("code")?.as_str()?.to_owned();
    if key.len() > 64 || code.len() > 64 {
        return None;
    }
    let flag = |name| p.get(name) == Some(&Value::Bool(true));
    Some(KeyDown { key, code, meta_key: flag("metaKey"), ctrl_key: flag("ctrlKey"), shift_key: flag("shiftKey"), alt_key: flag("altKey"), repeat: flag("repeat") })
}
pub fn shortcut_allowed(command: &str) -> bool {
    matches!(
        command,
        "commandPalette.toggle"
            | "filePicker.toggle"
            | "projectSearch.toggle"
            | "chat.find"
            | "sidebar.toggle"
            | "rightPanel.toggle"
            | "rightPanel.toggleMaximized"
            | "threadPanel.toggle"
            | "terminal.toggle"
            | "preview.toggle"
            | "navigation.back"
            | "navigation.forward"
            | "thread.previous"
            | "thread.next"
    ) || keybindings::THREAD_JUMP_KEYBINDING_COMMANDS.contains(&command)
        || keybindings::MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.contains(&command)
}
pub fn shortcut_command<'a>(key: &KeyDown, rules: &'a [ResolvedKeybindingRule], context: &ShortcutMatchContext, mac: bool) -> Option<&'a str> {
    if !key.meta_key && !key.ctrl_key {
        return None;
    }
    let event =
        ShortcutEvent { key: &key.key, code: Some(&key.code), meta_key: key.meta_key, ctrl_key: key.ctrl_key, shift_key: key.shift_key, alt_key: key.alt_key, alt_graph: false };
    let matching = |r: &&ResolvedKeybindingRule| keybindings::matches_keybinding_shortcut(&event, &r.shortcut, mac);
    if rules.iter().filter(matching).any(|r| !shortcut_allowed(&r.command)) {
        return None;
    }
    if key.key.encode_utf16().count() != 1 && key.key != "Dead" && !rules.iter().filter(matching).any(|r| r.shortcut.key == keybindings::normalize_event_key(&key.key)) {
        return None;
    }
    keybindings::resolve_shortcut_command(&event, rules, context, mac).filter(|c| shortcut_allowed(c))
}

pub fn shortcuts_message(rules: &[ResolvedKeybindingRule], mac: bool) -> Value {
    let serialize = |r: &ResolvedKeybindingRule| json!({"key":r.shortcut.key, "metaKey":r.shortcut.meta_key || (r.shortcut.mod_key && mac), "ctrlKey":r.shortcut.ctrl_key || (r.shortcut.mod_key && !mac), "shiftKey":r.shortcut.shift_key, "altKey":r.shortcut.alt_key});
    let shortcuts: Vec<_> = rules.iter().filter(|r| shortcut_allowed(&r.command) && (r.shortcut.meta_key || r.shortcut.ctrl_key || r.shortcut.mod_key)).map(serialize).collect();
    let blocked: Vec<_> = rules.iter().filter(|r| !shortcut_allowed(&r.command)).map(serialize).collect();
    let codes: std::collections::BTreeMap<_, _> = keybindings::EVENT_CODE_SHORTCUT_KEYS.into_iter().collect();
    json!({"jsonrpc":"2.0", "method":"t3/notifications/shortcuts", "params":{"shortcuts":shortcuts,"blocked":blocked,"codes":codes}})
}
