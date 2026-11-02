//! Settings → Projects → Actions: `ProjectActionsSettings.tsx`, `ProjectActionsList.tsx` and
//! `useProjectScriptSettings.ts`, the form rules of `projectScriptEditor.tsx`, `buildProjectScript`,
//! `releaseClaimedRoles` and `nextProjectScriptId` (`web/src/projectScripts.ts`), and
//! `lib/projectScriptKeybindings.ts`. A project's actions are the `defaultProjectScripts` its
//! settings override holds; `t3.json`'s scripts import through `parseT3ProjectFile`
//! (`shared/src/t3ProjectFile.ts`, `schemaJson.ts`'s lenient JSON).

use serde_json::{Map, Value, json};

use super::keybindings::{SERVER_REMOVE_KEYBINDING, SERVER_UPSERT_KEYBINDING};
use super::projects::cleared_override;
use crate::keybindings::{ResolvedKeybindingRule, is_keybinding_command};
use crate::project_scripts::{ProjectScript, ScriptIcon, command_for_project_script, resolve_project_scripts};
use crate::server_config::ServerSettings;

const MAX_SCRIPT_ID_LENGTH: usize = 24;
const MAX_KEYBINDING_VALUE_LENGTH: usize = 64;
const T3_PROJECT_FILE_MAX_SCRIPTS: usize = 50;
const T3_PROJECT_FILE_PATH_MAX_LENGTH: usize = 512;

pub const INVALID_KEYBINDING: &str = "Invalid keybinding.";
pub const NO_ACTIONS: &str = "No actions configured.";
pub const ACTIONS_DESCRIPTION: &str = "Commands that run in this project's checkout or its worktree, with optional shortcuts.";

/// `SCRIPT_ICONS`: the editor's icon choices with their wire names and labels.
pub const SCRIPT_ICONS: [(ScriptIcon, &str, &str); 6] = [
    (ScriptIcon::Play, "play", "Play"),
    (ScriptIcon::Test, "test", "Test"),
    (ScriptIcon::Lint, "lint", "Lint"),
    (ScriptIcon::Configure, "configure", "Configure"),
    (ScriptIcon::Build, "build", "Build"),
    (ScriptIcon::Debug, "debug", "Debug"),
];

/// `NewProjectScriptInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptInput {
    pub name: String,
    pub command: String,
    pub icon: ScriptIcon,
    pub run_on_worktree_create: bool,
    pub wait_for_setup: bool,
    pub run_on_settle: bool,
    /// `None` keeps the current shortcut; `Some(None)` clears it.
    pub keybinding: Option<Option<String>>,
    pub preview_url: Option<String>,
    pub auto_open_preview: bool,
}

impl Default for ScriptInput {
    /// `EMPTY_PROJECT_SCRIPT_INPUT`.
    fn default() -> Self {
        ScriptInput {
            name: String::new(),
            command: String::new(),
            icon: ScriptIcon::Play,
            run_on_worktree_create: false,
            wait_for_setup: false,
            run_on_settle: false,
            keybinding: Some(None),
            preview_url: None,
            auto_open_preview: false,
        }
    }
}

/// `ProjectScriptEditorRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRequest {
    pub script_id: Option<String>,
    pub initial: ScriptInput,
    pub error: Option<String>,
}

impl EditorRequest {
    pub fn add() -> EditorRequest {
        EditorRequest { script_id: None, initial: ScriptInput::default(), error: None }
    }

    /// `editorRequestForScript`.
    pub fn edit(script: &ProjectScript, keybindings: &[ResolvedKeybindingRule]) -> EditorRequest {
        let command = command_for_project_script(&script.id);
        EditorRequest {
            script_id: Some(script.id.clone()),
            initial: ScriptInput {
                name: script.name.clone(),
                command: script.command.clone(),
                icon: script.icon,
                run_on_worktree_create: script.run_on_worktree_create,
                wait_for_setup: script.run_on_worktree_create && script.run_async == Some(false),
                run_on_settle: script.runs_on_settle(),
                keybinding: Some(keybinding_value_for_command(keybindings, command.as_deref())),
                preview_url: script.preview_url.clone(),
                auto_open_preview: script.auto_open_preview.unwrap_or(false),
            },
            error: None,
        }
    }
}

/// `keybindingValueForCommand`: the last binding for `command`, spelled as a config key.
pub fn keybinding_value_for_command(keybindings: &[ResolvedKeybindingRule], command: Option<&str>) -> Option<String> {
    let command = command?;
    let binding = keybindings.iter().rev().find(|binding| binding.command == command)?;
    let shortcut = &binding.shortcut;
    let mut parts: Vec<&str> = [(shortcut.mod_key, "mod"), (shortcut.ctrl_key, "ctrl"), (shortcut.meta_key, "meta"), (shortcut.alt_key, "alt"), (shortcut.shift_key, "shift")]
        .into_iter()
        .filter_map(|(held, name)| held.then_some(name))
        .collect();
    parts.push(match shortcut.key.as_str() {
        " " => "space",
        "escape" => "esc",
        key => key,
    });
    Some(parts.join("+"))
}

/// `decodeProjectScriptKeybindingRule`: the trimmed key, or none for an empty field.
pub fn decode_keybinding(keybinding: Option<&str>, command: Option<&str>) -> Result<Option<(String, String)>, &'static str> {
    let key = keybinding.map(str::trim).unwrap_or_default();
    if key.is_empty() {
        return Ok(None);
    }
    let command = command.filter(|command| is_keybinding_command(command)).ok_or(INVALID_KEYBINDING)?;
    if key.chars().count() > MAX_KEYBINDING_VALUE_LENGTH {
        return Err(INVALID_KEYBINDING);
    }
    Ok(Some((key.to_owned(), command.to_owned())))
}

/// `buildProjectScript`.
pub fn build_project_script(id: &str, input: &ScriptInput) -> ProjectScript {
    ProjectScript {
        id: id.to_owned(),
        name: input.name.clone(),
        command: input.command.clone(),
        icon: input.icon,
        run_on_worktree_create: input.run_on_worktree_create,
        run_async: (input.run_on_worktree_create && input.wait_for_setup).then_some(false),
        run_on_settle: input.run_on_settle.then_some(true),
        preview_url: input.preview_url.clone(),
        auto_open_preview: input.preview_url.is_some().then_some(input.auto_open_preview),
    }
}

/// `releaseClaimedRoles`: one script runs on worktree creation and one on settle, so the saved
/// action takes those roles from the others.
pub fn release_claimed_roles(script: &ProjectScript, saved: &ScriptInput) -> ProjectScript {
    let mut script = script.clone();
    if saved.run_on_worktree_create && script.run_on_worktree_create {
        script.run_on_worktree_create = false;
    }
    if saved.run_on_settle && script.run_on_settle == Some(true) {
        script.run_on_settle = Some(false);
    }
    script
}

fn normalize_script_id(value: &str) -> String {
    let mut cleaned = String::new();
    for ch in value.trim().to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            cleaned.push(ch);
        } else if !cleaned.ends_with('-') {
            cleaned.push('-');
        }
    }
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        return "script".to_owned();
    }
    if cleaned.len() <= MAX_SCRIPT_ID_LENGTH {
        return cleaned.to_owned();
    }
    let cut = cleaned[..MAX_SCRIPT_ID_LENGTH].trim_end_matches('-');
    if cut.is_empty() { "script".to_owned() } else { cut.to_owned() }
}

/// `nextProjectScriptId`: the name as an id, numbered past the ids already taken.
pub fn next_project_script_id<'a>(name: &str, existing: impl IntoIterator<Item = &'a str>, now_ms: u64) -> String {
    let taken: std::collections::HashSet<&str> = existing.into_iter().collect();
    let base = normalize_script_id(name);
    if !taken.contains(base.as_str()) {
        return base;
    }
    for suffix in 2..10_000u32 {
        let candidate = format!("{base}-{suffix}");
        let candidate = if candidate.len() <= MAX_SCRIPT_ID_LENGTH {
            candidate
        } else {
            let keep = MAX_SCRIPT_ID_LENGTH.saturating_sub(suffix.to_string().len() + 1).max(1);
            format!("{}-{suffix}", &base[..keep.min(base.len())])
        };
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
    }
    let fallback = format!("{base}-{now_ms}");
    fallback[..fallback.len().min(MAX_SCRIPT_ID_LENGTH)].to_owned()
}

/// What the editor's fields hold when it saves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormValues {
    pub name: String,
    pub command: String,
    pub icon: ScriptIcon,
    pub run_on_worktree_create: bool,
    pub wait_for_setup: bool,
    pub run_on_settle: bool,
    pub keybinding: String,
    pub preview_url: String,
    pub auto_open_preview: bool,
}

impl FormValues {
    pub fn from_input(input: &ScriptInput) -> FormValues {
        FormValues {
            name: input.name.clone(),
            command: input.command.clone(),
            icon: input.icon,
            run_on_worktree_create: input.run_on_worktree_create,
            wait_for_setup: input.wait_for_setup,
            run_on_settle: input.run_on_settle,
            keybinding: input.keybinding.clone().flatten().unwrap_or_default(),
            preview_url: input.preview_url.clone().unwrap_or_default(),
            auto_open_preview: input.auto_open_preview,
        }
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// The editor's `submit` checks: permission, a shortcut this connection may change, a name, a
/// command and a valid shortcut. The shortcut is sent only when it changed, or for a new action
/// this connection may bind.
pub fn validate_form(request: &EditorRequest, form: &FormValues, scripts: &[ProjectScript], can_edit: bool, can_write_settings: bool, now_ms: u64) -> Result<ScriptInput, String> {
    if !can_edit {
        return Err("This connection cannot change project actions.".to_owned());
    }
    let initial_key = request.initial.keybinding.as_ref().and_then(|key| key.as_deref()).and_then(non_empty);
    let changes_keybinding = non_empty(&form.keybinding) != initial_key;
    if changes_keybinding && !can_write_settings {
        return Err("This connection cannot change keyboard shortcuts.".to_owned());
    }
    let name = form.name.trim();
    let command = form.command.trim();
    if name.is_empty() {
        return Err("Name is required.".to_owned());
    }
    if command.is_empty() {
        return Err("Command is required.".to_owned());
    }
    let id = request.script_id.clone().unwrap_or_else(|| next_project_script_id(name, scripts.iter().map(|script| script.id.as_str()), now_ms));
    let rule = decode_keybinding(Some(&form.keybinding), command_for_project_script(&id).as_deref()).map_err(str::to_owned)?;
    let preview_url = non_empty(&form.preview_url).map(str::to_owned);
    Ok(ScriptInput {
        name: name.to_owned(),
        command: command.to_owned(),
        icon: form.icon,
        run_on_worktree_create: form.run_on_worktree_create,
        wait_for_setup: form.run_on_worktree_create && form.wait_for_setup,
        run_on_settle: form.run_on_settle,
        keybinding: ((request.script_id.is_none() && can_write_settings) || changes_keybinding).then(|| rule.map(|(key, _)| key)),
        auto_open_preview: preview_url.is_some() && form.auto_open_preview,
        preview_url,
    })
}

/// The list the section shows for one member: its override, else the environment's actions
/// (`resolveProjectSettings`' `defaultProjectScripts`).
pub fn shown_scripts<'a>(settings: &'a ServerSettings, project_id: &str) -> &'a [ProjectScript] {
    settings.project_settings_overrides.get(project_id).and_then(|entry| entry.default_project_scripts.as_deref()).unwrap_or(&settings.default_project_scripts)
}

/// The members disagree, so the list cannot be edited as one.
pub fn scripts_mixed(settings: &ServerSettings, project_ids: &[&str]) -> bool {
    let Some((first, rest)) = project_ids.split_first() else { return false };
    let shown = shown_scripts(settings, first);
    rest.iter().any(|id| shown_scripts(settings, id) != shown)
}

/// A member's actions override exists, so the row offers "Reset to inherited value".
pub fn scripts_overridden(settings: &ServerSettings, project_ids: &[&str]) -> bool {
    project_ids.iter().any(|id| settings.project_settings_overrides.get(*id).is_some_and(|entry| entry.raw.contains_key("defaultProjectScripts")))
}

/// A project a write targets: its id and its own scripts (`project.scripts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub scripts: Vec<ProjectScript>,
}

/// One RPC an actions save sends, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionWrite {
    Settings(Value),
    UpsertKeybinding(Value),
    RemoveKeybinding(Value),
}

impl ActionWrite {
    pub fn method(&self) -> &'static str {
        match self {
            ActionWrite::Settings(_) => crate::commands::method::SERVER_UPDATE_SETTINGS,
            ActionWrite::UpsertKeybinding(_) => SERVER_UPSERT_KEYBINDING,
            ActionWrite::RemoveKeybinding(_) => SERVER_REMOVE_KEYBINDING,
        }
    }

    pub fn payload(&self) -> &Value {
        match self {
            ActionWrite::Settings(payload) | ActionWrite::UpsertKeybinding(payload) | ActionWrite::RemoveKeybinding(payload) => payload,
        }
    }
}

fn keybinding_rule(key: &str, command: &str) -> Value {
    json!({ "key": key, "command": command })
}

/// `useProjectScriptSettings`' `writeTargets`: for each member, the override patch, then the
/// shortcut writes for the actions it touched. A shortcut leaves only when no other project still
/// runs the same action. `next` maps the member's current actions to its new ones; `None` resets
/// the override. `keybinding` is `None` to keep shortcuts as they are.
pub fn plan_writes(
    settings: &ServerSettings,
    keybindings: &[ResolvedKeybindingRule],
    projects: &[Member],
    members: &[Member],
    next: impl Fn(&[ProjectScript]) -> Option<Vec<ProjectScript>>,
    script_id: Option<&str>,
    keybinding: Option<Option<&str>>,
) -> Vec<ActionWrite> {
    let mut writes = Vec::new();
    for member in members {
        let current = resolve_project_scripts(settings, &member.id, member.scripts.clone());
        let next_scripts = next(&current);
        let effective = next_scripts.as_deref().unwrap_or(&settings.default_project_scripts);
        let entry = match &next_scripts {
            None => cleared_override(settings, &member.id, &["defaultProjectScripts"]),
            Some(scripts) => {
                let mut entry = settings.project_settings_overrides.get(&member.id).map(|entry| entry.raw.clone()).unwrap_or_default();
                entry.insert("defaultProjectScripts".to_owned(), json!(scripts));
                Value::Object(entry)
            }
        };
        let mut overrides = Map::new();
        overrides.insert(member.id.clone(), entry);
        writes.push(ActionWrite::Settings(json!({ "patch": { "projectSettingsOverrides": overrides } })));

        let changed: Vec<String> = match script_id {
            Some(id) => vec![id.to_owned()],
            None => current.iter().filter(|script| !effective.iter().any(|next| next.id == script.id)).map(|script| script.id.clone()).collect(),
        };
        for id in changed {
            if keybinding.is_none() && effective.iter().any(|script| script.id == id) {
                continue;
            }
            let command = command_for_project_script(&id);
            let previous = keybinding_value_for_command(keybindings, command.as_deref())
                .and_then(|value| decode_keybinding(Some(&value), command.as_deref()).ok().flatten());
            let wanted = decode_keybinding(keybinding.flatten(), command.as_deref()).ok().flatten();
            let has = |scripts: &[ProjectScript]| scripts.iter().any(|script| script.id == id);
            let retained_elsewhere = !next_scripts.as_deref().is_some_and(has)
                && (has(&settings.default_project_scripts)
                    || settings.project_settings_overrides.iter().any(|(project_id, entry)| *project_id != member.id && entry.default_project_scripts.as_deref().is_some_and(has))
                    || projects.iter().any(|other| other.id != member.id && has(&resolve_project_scripts(settings, &other.id, other.scripts.clone()))));
            match (wanted, previous) {
                (Some((key, command)), previous) => {
                    let mut payload = keybinding_rule(&key, &command);
                    if let Some((previous_key, previous_command)) = previous.filter(|(previous_key, _)| *previous_key != key) {
                        payload["replace"] = keybinding_rule(&previous_key, &previous_command);
                    }
                    writes.push(ActionWrite::UpsertKeybinding(payload));
                }
                (None, Some((key, command))) if !retained_elsewhere => writes.push(ActionWrite::RemoveKeybinding(keybinding_rule(&key, &command))),
                _ => {}
            }
        }
    }
    writes
}

/// `submit`: the id a new action takes (past every id in use), and the list each member saves
/// with it, the others giving up the roles it claims.
pub fn submit_id(settings: &ServerSettings, projects: &[Member], members: &[Member], script_id: Option<&str>, name: &str, now_ms: u64) -> String {
    if let Some(id) = script_id {
        return id.to_owned();
    }
    let mut existing: Vec<&str> = projects.iter().flat_map(|project| project.scripts.iter().map(|script| script.id.as_str())).collect();
    for member in members {
        existing.extend(settings.default_project_scripts.iter().map(|script| script.id.as_str()));
        existing.extend(settings.project_settings_overrides.values().flat_map(|entry| entry.default_project_scripts.iter().flatten()).map(|script| script.id.as_str()));
        existing.extend(member.scripts.iter().map(|script| script.id.as_str()));
    }
    next_project_script_id(name, existing, now_ms)
}

pub fn submitted_scripts(current: &[ProjectScript], id: &str, input: &ScriptInput, is_new: bool) -> Vec<ProjectScript> {
    let saved = build_project_script(id, input);
    let mut scripts: Vec<ProjectScript> = current.iter().map(|script| if script.id == id { saved.clone() } else { release_claimed_roles(script, input) }).collect();
    if is_new {
        scripts.push(saved);
    }
    scripts
}

/// `T3ProjectFileScript`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileScript {
    pub name: String,
    pub command: String,
    pub icon: Option<ScriptIcon>,
    pub run_on_worktree_create: Option<bool>,
    pub run_on_settle: Option<bool>,
    pub run_async: Option<bool>,
    pub preview_url: Option<String>,
    pub auto_open_preview: Option<bool>,
}

impl FileScript {
    /// `importFileScript`'s payload: imported actions take no shortcut.
    pub fn import_input(&self) -> ScriptInput {
        let setup = self.run_on_worktree_create == Some(true);
        ScriptInput {
            name: self.name.clone(),
            command: self.command.clone(),
            icon: self.icon.unwrap_or(ScriptIcon::Play),
            run_on_worktree_create: setup,
            wait_for_setup: setup && self.run_async == Some(false),
            run_on_settle: self.run_on_settle.unwrap_or(false),
            keybinding: Some(None),
            preview_url: self.preview_url.clone(),
            auto_open_preview: self.preview_url.is_some() && self.auto_open_preview.unwrap_or(false),
        }
    }
}

/// `useT3ProjectFileState` once the read settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFile {
    Missing,
    /// The file exists but fails to parse, so everything it declares is ignored.
    Invalid,
    Valid { scripts: Vec<FileScript>, default_thread_env_mode: Option<crate::server_config::ThreadEnvMode> },
}

impl ProjectFile {
    pub fn scripts(&self) -> &[FileScript] {
        match self {
            ProjectFile::Valid { scripts, .. } => scripts,
            _ => &[],
        }
    }
}

/// `projects.readFile`'s reply for `t3.json`: missing, truncated or absent contents read as missing.
pub fn project_file_from_read(result: &Value) -> ProjectFile {
    match crate::workspace::project_file_contents(result) {
        Some(contents) => parse_project_file(contents),
        None => ProjectFile::Missing,
    }
}

/// `parseT3ProjectFile`: lenient JSON, then the `T3ProjectFile` schema; any violation is invalid.
pub fn parse_project_file(contents: &str) -> ProjectFile {
    let Some(Value::Object(file)) = serde_json::from_str::<Value>(&strip_lenient_json(contents)).ok() else { return ProjectFile::Invalid };
    decode_project_file(&file).unwrap_or(ProjectFile::Invalid)
}

fn trimmed_text(value: &Value, max: Option<usize>) -> Option<String> {
    let text = value.as_str()?;
    if text.is_empty() || max.is_some_and(|max| text.chars().count() > max) {
        return None;
    }
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Decodes an optional key: absent is `Ok(None)`, present but invalid is `Err`.
fn optional<T>(object: &Map<String, Value>, key: &str, decode: impl Fn(&Value) -> Option<T>) -> Result<Option<T>, ()> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => decode(value).map(Some).ok_or(()),
    }
}

fn decode_project_file(file: &Map<String, Value>) -> Result<ProjectFile, ()> {
    optional(file, "$schema", |value| value.as_str().map(str::to_owned))?;
    optional(file, "iconPath", |value| trimmed_text(value, Some(T3_PROJECT_FILE_PATH_MAX_LENGTH)))?;
    let default_thread_env_mode = optional(file, "defaultThreadEnvMode", |value| serde_json::from_value(value.clone()).ok())?;
    optional(file, "worktreeSubmodules", |value| value.as_str().filter(|mode| matches!(*mode, "recursive" | "top-level" | "none")).map(str::to_owned))?;
    let scripts = optional(file, "scripts", |value| {
        let entries = value.as_array().filter(|entries| entries.len() <= T3_PROJECT_FILE_MAX_SCRIPTS)?;
        entries.iter().map(decode_file_script).collect::<Option<Vec<_>>>()
    })?;
    Ok(ProjectFile::Valid { scripts: scripts.unwrap_or_default(), default_thread_env_mode })
}

fn decode_file_script(value: &Value) -> Option<FileScript> {
    let script = value.as_object()?;
    let boolean = |key: &str| optional(script, key, Value::as_bool);
    Some(FileScript {
        name: trimmed_text(script.get("name")?, None)?,
        command: trimmed_text(script.get("command")?, None)?,
        icon: optional(script, "icon", |value| SCRIPT_ICONS.iter().find(|(_, name, _)| Some(*name) == value.as_str()).map(|(icon, _, _)| *icon)).ok()?,
        run_on_worktree_create: boolean("runOnWorktreeCreate").ok()?,
        run_on_settle: boolean("runOnSettle").ok()?,
        run_async: boolean("async").ok()?,
        preview_url: optional(script, "previewUrl", |value| trimmed_text(value, None)).ok()?,
        auto_open_preview: boolean("autoOpenPreview").ok()?,
    })
}

/// `parseLenientJsonGetter`: comments and trailing commas outside strings drop out.
fn strip_lenient_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '"' => {
                let start = index;
                index += 1;
                while index < chars.len() && chars[index] != '"' {
                    index += if chars[index] == '\\' { 2 } else { 1 };
                }
                index = (index + 1).min(chars.len());
                out.extend(&chars[start..index]);
            }
            '/' if chars.get(index + 1) == Some(&'/') => {
                while index < chars.len() && !matches!(chars[index], '\r' | '\n') {
                    index += 1;
                }
                out.push(' ');
            }
            '/' if chars.get(index + 1) == Some(&'*') => match (index + 2..chars.len().saturating_sub(1)).find(|at| chars[*at] == '*' && chars[*at + 1] == '/') {
                Some(end) => {
                    index = end + 2;
                    out.push(' ');
                }
                None => {
                    out.push('/');
                    index += 1;
                }
            },
            ',' => {
                let next = chars[index + 1..].iter().find(|ch| !ch.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(',');
                }
                index += 1;
            }
            ch => {
                out.push(ch);
                index += 1;
            }
        }
    }
    out
}

/// `importableScripts`: file scripts whose command and name no action already has.
pub fn importable<'a>(file_scripts: &'a [FileScript], scripts: &[ProjectScript]) -> Vec<&'a FileScript> {
    file_scripts
        .iter()
        .filter(|file| !scripts.iter().any(|script| script.command == file.command || script.name.to_lowercase() == file.name.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests;
