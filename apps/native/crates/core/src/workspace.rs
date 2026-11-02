//! Where a new thread works: the project checkout or a fresh worktree. Ports the project-scoped
//! `defaultThreadEnvMode` resolution (`shared/src/projectSettings.ts` `resolveProjectSettings`,
//! `resolveNewThreadEnvMode`, `resolveSendEnvMode`), the `t3.json` fallback
//! (`web/src/lib/t3ProjectFileDefaults.ts`), `resolveNewDraftStartFromOrigin`
//! (`web/src/lib/chatThreadActions.ts`), and the default base branch the branch toolbar picks
//! (`BranchToolbarBranchSelector.tsx` `worktreeBaseBranchCandidate`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::server_config::{ServerSettings, ThreadEnvMode};

pub const PROJECT_FILE: &str = "t3.json";

/// Where the resolved env mode came from (`ProjectSettingSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Environment,
    Project,
    ProjectFile,
}

/// The project override, else the environment's setting. `None` inherits from `t3.json`.
pub fn configured_env_mode(settings: &ServerSettings, project_id: &str) -> Option<(ThreadEnvMode, Source)> {
    let project = settings.project_settings_overrides.get(project_id).and_then(|overrides| overrides.default_thread_env_mode);
    match project {
        Some(mode) => Some((mode, Source::Project)),
        None => settings.default_thread_env_mode.map(|mode| (mode, Source::Environment)),
    }
}

/// `t3.json`'s `defaultThreadEnvMode`, when the file holds a valid one.
pub fn project_file_env_mode(contents: &str) -> Option<ThreadEnvMode> {
    let file: Value = serde_json::from_str(contents).ok()?;
    serde_json::from_value(file.get("defaultThreadEnvMode")?.clone()).ok()
}

/// `normalizeProjectPathForComparison`: Windows drive and UNC paths compare case-insensitively.
fn normalize_path(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let trimmed = unified.trim_end_matches('/');
    let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
    let windows = trimmed.starts_with("//") || trimmed.as_bytes().get(1) == Some(&b':');
    if windows { trimmed.to_lowercase() } else { trimmed.to_owned() }
}

/// `resolveNewThreadEnvMode`: new name-only projects use their checkout until their first
/// thread, even when the environment's default is a worktree.
pub fn new_thread_env_mode(
    configured: Option<(ThreadEnvMode, Source)>,
    project_file: Option<ThreadEnvMode>,
    workspace_root: Option<&str>,
    new_projects_root: Option<&str>,
    project_has_threads: bool,
) -> ThreadEnvMode {
    let (mode, source) = configured
        .or_else(|| project_file.map(|mode| (mode, Source::ProjectFile)))
        .unwrap_or((ThreadEnvMode::Local, Source::Environment));
    let (Some(workspace_root), Some(new_projects_root)) = (workspace_root, new_projects_root) else { return mode };
    if mode == ThreadEnvMode::Local || project_has_threads || source != Source::Environment {
        return mode;
    }
    let root = normalize_path(new_projects_root);
    let prefix = if root.ends_with('/') { root } else { format!("{root}/") };
    let folder = normalize_path(workspace_root);
    let is_new_project_folder =
        folder.len() > prefix.len() && folder.starts_with(&prefix) && !folder[prefix.len()..].contains('/');
    if is_new_project_folder { ThreadEnvMode::Local } else { mode }
}

/// `resolveNewDraftStartFromOrigin`.
pub fn start_from_origin(settings: &ServerSettings, project_id: &str, mode: ThreadEnvMode) -> bool {
    let setting = settings
        .project_settings_overrides
        .get(project_id)
        .and_then(|overrides| overrides.new_worktrees_start_from_origin)
        .or(settings.new_worktrees_start_from_origin)
        .unwrap_or(true);
    mode == ThreadEnvMode::Worktree && setting
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRef {
    pub name: String,
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub is_default: bool,
}

/// `VcsListRefsResult`, the parts a launch reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRefs {
    #[serde(default)]
    pub is_repo: bool,
    #[serde(default, deserialize_with = "crate::server_config::lenient_vec")]
    pub refs: Vec<VcsRef>,
}

/// `vcs.listRefs` for the first page, as the branch toolbar asks for it.
pub fn list_refs_request(cwd: &str) -> Value {
    json!({ "cwd": cwd, "limit": 100 })
}

/// `projects.readFile` for the project's `t3.json`, absent being fine.
pub fn project_file_request(cwd: &str) -> Value {
    json!({ "cwd": cwd, "relativePath": PROJECT_FILE, "allowMissing": true })
}

/// The `projects.readFile` result's contents, unless the file is missing or truncated.
pub fn project_file_contents(result: &Value) -> Option<&str> {
    if result.get("exists") == Some(&Value::Bool(false)) || result.get("truncated") == Some(&Value::Bool(true)) {
        return None;
    }
    result.get("contents")?.as_str()
}

/// `worktreeBaseBranchCandidate`: the default branch, else the checked-out one.
pub fn base_branch(refs: &VcsRefs) -> Option<String> {
    refs.refs
        .iter()
        .find(|reference| reference.is_default)
        .or_else(|| refs.refs.iter().find(|reference| reference.current))
        .map(|reference| reference.name.clone())
}

/// What a draft's first send asks the server to prepare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    Root,
    Worktree { base_ref: String, start_from_origin: bool },
}

impl Strategy {
    /// `OrchestrationV2ThreadLaunchWorkspaceStrategy`, as `startThreadTurn` encodes it.
    pub fn to_json(&self) -> Value {
        match self {
            Strategy::Root => json!({ "type": "root" }),
            Strategy::Worktree { base_ref, start_from_origin } => {
                let mut strategy = json!({ "type": "worktree", "baseRef": base_ref });
                if *start_from_origin {
                    strategy["startFromOrigin"] = json!(true);
                }
                strategy
            }
        }
    }
}

/// `planComposerTurn` for a draft: a worktree when the mode asks for one in a git repository
/// (`resolveSendEnvMode`), which needs a base branch to start from.
pub fn launch_strategy(mode: ThreadEnvMode, refs: Option<&VcsRefs>, start_from_origin: bool) -> Result<Strategy, &'static str> {
    if mode == ThreadEnvMode::Local || refs.is_some_and(|refs| !refs.is_repo) {
        return Ok(Strategy::Root);
    }
    match refs.and_then(base_branch) {
        Some(base_ref) => Ok(Strategy::Worktree { base_ref, start_from_origin }),
        None => Err("Select a base branch before sending in New worktree mode."),
    }
}

#[cfg(test)]
mod tests;
