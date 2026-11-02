//! Settings → Projects: the project list (`ProjectsSettings.tsx`, `SettingsScopeNotice.tsx` for a
//! project, `useSettingsProjectGroups.ts`) and one project's page (`ProjectSettingsPanel.tsx` with
//! `ProjectSettingsPanel.logic.ts`): name, icon, pin, archive, checkouts and removal, written as
//! `useProjectGroupUpdate.ts` sends them. The removal confirmation is `planProjectRemoval`
//! (`client-runtime/src/state/projectRemoval.ts`), split the way `ConfirmDialogHost.tsx` splits a
//! message. The project half of `scopedSettings.ts` writes the page's project overrides. The
//! native client holds one environment, so the web scope's environment and checkout axes have no
//! counterpart here: a project's members are every checkout in the group.

use serde_json::{Map, Value, json};

use crate::model::ModelSelection;
use crate::providers;
use crate::server_config::{ServerConfig, ServerSettings, ThreadEnvMode};
use crate::sidebar::grouping::{ProjectGroup, normalize_project_path};
use crate::thread::locale_compare;
use crate::workspace::{self, Source};

pub const SCRATCH_PROJECT_TITLE: &str = "No project";
const SCRATCH_ICON_NAME: &str = "message-square-dashed";
const SCRATCH_ICON_COLOR: &str = "gray";

pub const NO_PROJECT_NOTICE: &str = "Choose a project to manage its name, icon, checkouts and actions.";
pub const NO_PROJECTS: &str = "Add a project from the sidebar to configure it here.";

/// `useSettingsProjectGroups`: groups sorted by display name.
pub fn sort_groups(groups: &mut [ProjectGroup]) {
    groups.sort_by(|left, right| locale_compare(&left.label, &right.label));
}

/// A project-list row's second line (`SettingsScopeNotice`, target "project"): archived, the
/// machine when another project shares the name, then the folder.
pub fn choice_secondary(group: &ProjectGroup, groups: &[ProjectGroup], workspace_root: &str, environment_label: &str) -> String {
    let mut parts = Vec::new();
    if group.archived {
        parts.push("Archived");
    }
    if groups.iter().any(|other| other.key != group.key && other.label == group.label) {
        parts.push(environment_label);
    }
    parts.push(workspace_root);
    parts.join(" \u{b7} ")
}

/// The group the page shows: the chosen key, else the group that now holds the members it last
/// showed, so a grouping change follows the project (`ProjectSettingsPanel`'s successor).
pub fn resolve_selection<'a>(groups: &'a [ProjectGroup], key: &str, last_members: &[String]) -> Option<&'a ProjectGroup> {
    groups
        .iter()
        .find(|group| group.key == key)
        .or_else(|| groups.iter().find(|group| group.member_project_ids.iter().any(|id| last_members.contains(id))))
}

/// `isScratchProject`.
pub fn is_scratch(workspace_root: &str, scratch_root: Option<&str>) -> bool {
    scratch_root.is_some_and(|root| {
        let root = normalize_project_path(root);
        !root.is_empty() && normalize_project_path(workspace_root) == root
    })
}

/// `ProjectIconOverride` as `ReceivedProjectIcon` decodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIcon {
    Lucide { name: String, color: String },
    Emoji { emoji: String },
    Monogram { text: String, color: String },
    /// A kind this build does not know. It is still an override.
    Unknown,
}

impl ProjectIcon {
    /// `None` for no override. A lucide icon carrying a legacy monogram reads as a monogram.
    pub fn decode(value: Option<&Value>) -> Option<ProjectIcon> {
        let value = value.filter(|value| !value.is_null())?;
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let icon = match value.get("kind").and_then(Value::as_str) {
            Some("lucide") => match (text("monogramText").or_else(|| text("monogram")), text("name"), text("color")) {
                (Some(monogram), _, Some(color)) => Some(ProjectIcon::Monogram { text: monogram, color }),
                (None, Some(name), Some(color)) => Some(ProjectIcon::Lucide { name, color }),
                _ => None,
            },
            Some("emoji") => text("emoji").map(|emoji| ProjectIcon::Emoji { emoji }),
            Some("monogram") => text("text").zip(text("color")).map(|(text, color)| ProjectIcon::Monogram { text, color }),
            _ => None,
        };
        Some(icon.unwrap_or(ProjectIcon::Unknown))
    }
}

/// The Project icon row's description.
pub fn icon_description(icon: Option<&ProjectIcon>, favicon_path: Option<&str>) -> String {
    match icon {
        Some(ProjectIcon::Lucide { name, color }) => format!("{name} \u{b7} {color}"),
        Some(ProjectIcon::Monogram { text, color }) => format!("{text} \u{b7} {color}"),
        Some(ProjectIcon::Emoji { emoji }) => emoji.clone(),
        _ => favicon_path.unwrap_or("Automatic").to_owned(),
    }
}

/// `hasDefaultProjectIcon`: scratch projects default to their dashed chat icon.
pub fn has_default_icon(is_scratch: bool, favicon_path: Option<&str>, icon: Option<&ProjectIcon>) -> bool {
    if favicon_path.is_some() {
        return false;
    }
    if !is_scratch {
        return icon.is_none();
    }
    matches!(icon, Some(ProjectIcon::Lucide { name, color }) if name == SCRATCH_ICON_NAME && color == SCRATCH_ICON_COLOR)
}

/// `ProjectGroupUpdate`, sent as one `project.update` per member.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectUpdate {
    pub title: Option<String>,
    pub favicon_path: Option<Option<String>>,
    pub project_icon: Option<Option<Value>>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
}

impl ProjectUpdate {
    /// `defaultProjectIcon`.
    pub fn default_icon(is_scratch: bool) -> ProjectUpdate {
        let icon = is_scratch.then(|| json!({ "kind": "lucide", "name": SCRATCH_ICON_NAME, "color": SCRATCH_ICON_COLOR }));
        ProjectUpdate { favicon_path: Some(None), project_icon: Some(icon), ..Default::default() }
    }

    pub fn payload(&self, command_id: &str, project_id: &str) -> Value {
        let mut payload = json!({ "type": "project.update", "commandId": command_id, "projectId": project_id });
        if let Some(title) = &self.title {
            payload["title"] = json!(title);
        }
        if let Some(path) = &self.favicon_path {
            payload["faviconPath"] = json!(path);
        }
        if let Some(icon) = &self.project_icon {
            payload["projectIcon"] = icon.clone().unwrap_or(Value::Null);
        }
        if let Some(pinned) = self.pinned {
            payload["pinned"] = json!(pinned);
        }
        if let Some(archived) = self.archived {
            payload["archived"] = json!(archived);
        }
        payload
    }
}

/// `project.delete`, forced so the project's threads go with it.
pub fn delete_project(command_id: &str, project_id: &str) -> Value {
    json!({ "type": "project.delete", "commandId": command_id, "projectId": project_id, "force": true })
}

/// What committing the Name field does (`renameGroup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rename {
    /// Warn "Project title cannot be empty".
    Empty,
    Unchanged,
    Save(String),
}

/// `renameGroup` with `projectGroupTitleNeedsUpdate`: only an edited field that differs from a
/// member's title saves.
pub fn rename(member_titles: &[&str], next: &str, was_edited: bool) -> Rename {
    let title = next.trim();
    if title.is_empty() {
        return Rename::Empty;
    }
    if was_edited && member_titles.iter().any(|member| *member != title) { Rename::Save(title.to_owned()) } else { Rename::Unchanged }
}

/// The Name row's reset: every member is a scratch project and one was renamed.
pub fn can_reset_scratch_title(members: &[(bool, &str)]) -> bool {
    members.iter().all(|(scratch, _)| *scratch) && members.iter().any(|(_, title)| *title != SCRATCH_PROJECT_TITLE)
}

/// The note above a project's rows when this connection cannot change it.
pub fn edit_denied_notice(member_count: usize) -> &'static str {
    if member_count > 1 { "Shared settings require permission to change every checkout in this group." } else { "This connection cannot change this project." }
}

pub fn archive_description(archived: bool) -> &'static str {
    if archived { "Hidden from project pickers. Threads and files are kept." } else { "Hides the project from project pickers. Threads and files are kept." }
}

/// The Danger row's title, description and button for the project's members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DangerCopy {
    pub title: &'static str,
    pub description: String,
    pub button: &'static str,
}

pub fn danger_copy(has_other_members: bool, member_count: usize) -> DangerCopy {
    if has_other_members {
        DangerCopy {
            title: "Remove checkout",
            description: "Deletes the selected machine's checkout entries and their threads. Other machines and files on disk are not touched.".to_owned(),
            button: "Remove checkout",
        }
    } else if member_count > 1 {
        DangerCopy {
            title: "Remove this project everywhere",
            description: format!("Deletes all {member_count} checkout entries and their threads on every machine. Files on disk are not touched."),
            button: "Remove all entries",
        }
    } else {
        DangerCopy {
            title: "Remove project",
            description: "Deletes the project entry and its threads. Files on disk are not touched.".to_owned(),
            button: "Remove project",
        }
    }
}

/// One project entry a removal deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovalMember<'a> {
    pub title: &'a str,
    pub workspace_root: &'a str,
    pub environment_label: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalPlan {
    pub is_whole_group: bool,
    pub confirmation: String,
}

/// `planProjectRemoval`. `thread_count` counts the members' threads, loaded or not.
pub fn plan_removal(members: &[RemovalMember], group_member_count: usize, has_other_members: bool, display_name: &str, thread_count: usize) -> RemovalPlan {
    let is_whole_group = members.len() == group_member_count;
    let target_kind = if has_other_members || !is_whole_group { "checkout" } else { "project" };
    let single = if let [only] = members { Some(only) } else { None };
    let label = single.map_or(display_name, |member| member.title);
    let mut lines = vec![if thread_count > 0 {
        format!("Remove {target_kind} \"{label}\" and delete its {thread_count} thread{}?", if thread_count == 1 { "" } else { "s" })
    } else {
        format!("Remove {target_kind} \"{label}\"?")
    }];
    match single {
        Some(member) => {
            lines.push(format!("Path: {}", member.workspace_root));
            if let Some(environment) = member.environment_label.filter(|label| !label.is_empty()) {
                lines.push(format!("Environment: {environment}"));
            }
        }
        None => lines.push(format!("This removes {} grouped project entries.", members.len())),
    }
    lines.push(
        if thread_count > 0 {
            "Those threads and any archived threads are deleted from T3 Code. Provider sessions are kept."
        } else {
            "Any archived threads are deleted from T3 Code. Provider sessions are kept."
        }
        .to_owned(),
    );
    lines.push(
        if is_whole_group && !has_other_members { "This removes only the project entries, not the files on disk." } else { "Other entries in this grouped project are unaffected." }
            .to_owned(),
    );
    lines.push("This action cannot be undone.".to_owned());
    RemovalPlan { is_whole_group, confirmation: lines.join("\n") }
}

/// `resolveConfirmDialogCopy`: the question line titles the dialog; the rest describes it.
pub fn confirm_copy(message: &str) -> (String, Option<String>) {
    let message = message.trim();
    let lines: Vec<&str> = message.split('\n').collect();
    if let Some(index) = lines.iter().position(|line| line.trim().ends_with('?')) {
        let description = lines.iter().enumerate().filter(|(other, _)| *other != index).map(|(_, line)| *line).collect::<Vec<_>>().join("\n");
        let description = description.trim();
        return (lines[index].trim().to_owned(), (!description.is_empty()).then(|| description.to_owned()));
    }
    if let Some(index) = message.find('?') {
        let rest = message[index + 1..].trim();
        return (message[..=index].trim().to_owned(), (!rest.is_empty()).then(|| rest.to_owned()));
    }
    let description = if message.is_empty() { "This action requires your confirmation." } else { message };
    ("Confirm action".to_owned(), Some(description.to_owned()))
}

/// The Model row's value for a project (`ProjectDefaultsSettings` at project scope over
/// `resolveProjectOverrides`): the override, unless it names an instance the project turned off,
/// else the environment's default; shown as the best available model when none applies.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectModel {
    pub shown: Option<ModelSelection>,
    /// No model is stored, so new threads pick one automatically.
    pub automatic: bool,
    pub source: Source,
}

pub fn project_model(config: &ServerConfig, project_id: &str) -> ProjectModel {
    let settings = &config.settings;
    let enabled = |selection: &ModelSelection| providers::instance_enabled(settings, Some(project_id), &selection.instance_id);
    let overridden = settings.project_settings_overrides.get(project_id).and_then(|entry| entry.default_model_selection.clone());
    let (stored, source) = match overridden {
        Some(Some(selection)) if !enabled(&selection) => (settings.default_model_selection.clone(), Source::Environment),
        Some(selection) => (selection, Source::Project),
        None => (settings.default_model_selection.clone(), Source::Environment),
    };
    let shown = stored.clone().filter(|selection| enabled(selection)).or_else(|| providers::project_default_selection(&providers::entries(config, Some(project_id))));
    ProjectModel { shown, automatic: stored.is_none(), source }
}

/// The Workspace row's value for a project: its override, the environment's setting, the
/// checkout's `t3.json`, then the current checkout.
pub fn project_env_mode(settings: &ServerSettings, project_id: &str, project_file: Option<ThreadEnvMode>) -> (ThreadEnvMode, Source) {
    workspace::configured_env_mode(settings, project_id)
        .or_else(|| project_file.map(|mode| (mode, Source::ProjectFile)))
        .unwrap_or((ThreadEnvMode::Local, Source::Environment))
}

/// `isNullableProjectSettingsOverride`: null stores "no value" for these keys and removes others.
fn nullable_override(key: &str) -> bool {
    matches!(key, "defaultModelSelection" | "sourceControlWriterModelSelection" | "reviewGuideModelSelection" | "pullRequestMergeMethod" | "sidebarAutoSettleAfterDays")
}

fn override_entry(settings: &ServerSettings, project_id: &str) -> Option<Map<String, Value>> {
    settings.project_settings_overrides.get(project_id).map(|entry| entry.raw.clone())
}

/// `projectOverrideWrites` for one key on each member, completed as `planScopedSettingsPatch`
/// completes it: the entry keeps its other keys, and null removes a key that cannot hold null.
pub fn override_set_patch(settings: &ServerSettings, project_ids: &[&str], key: &str, value: Value) -> Value {
    let entries: Map<String, Value> = project_ids
        .iter()
        .map(|project_id| {
            let mut entry = override_entry(settings, project_id).unwrap_or_default();
            if value.is_null() && !nullable_override(key) {
                entry.remove(key);
            } else {
                entry.insert(key.to_owned(), value.clone());
            }
            ((*project_id).to_owned(), Value::Object(entry))
        })
        .collect();
    json!({ "patch": { "projectSettingsOverrides": entries } })
}

/// `clearProjectSettingsOverrides`: the entry without `keys`; null when nothing is left.
pub fn cleared_override(settings: &ServerSettings, project_id: &str, keys: &[&str]) -> Value {
    let Some(mut entry) = override_entry(settings, project_id) else { return Value::Null };
    for key in keys {
        entry.remove(*key);
    }
    if entry.is_empty() { Value::Null } else { Value::Object(entry) }
}

/// `planScopedSettingsClear`: "Reset to inherited value" on each member.
pub fn override_clear_patch(settings: &ServerSettings, project_ids: &[&str], keys: &[&str]) -> Value {
    let entries: Map<String, Value> = project_ids.iter().map(|project_id| ((*project_id).to_owned(), cleared_override(settings, project_id, keys))).collect();
    json!({ "patch": { "projectSettingsOverrides": entries } })
}

#[cfg(test)]
mod tests;
