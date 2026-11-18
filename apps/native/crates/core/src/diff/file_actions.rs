//! Opening a file from a diff. Ports `resolveDiffPathForWorkspace` from
//! `apps/web/src/diffFileActions.ts`: diff paths are repository-relative, so a project nested in
//! its repository strips its own prefix and refuses anything outside it, and Claude and ACP
//! tool-call diffs that name files by absolute path resolve against the workspace
//! (`pingdotgg/t3code#29309`).

use serde_json::{Value, json};

use crate::files::{absolute_path, workspace_relative_path};
use crate::project_search::{EDITOR_ORDER, OPEN_IN_EDITOR};
use crate::sidebar::grouping::normalize_project_path;

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\')))
}

/// `isAbsolutePath`.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || is_windows_absolute(path)
}

/// `normalizedRelativePathSegments`: `None` for anything absolute, drive-relative (`C:x`), or
/// climbing out with `..`.
fn relative_segments(path: &str) -> Option<Vec<&str>> {
    let bytes = path.as_bytes();
    let drive_prefixed = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if path.starts_with('/') || is_windows_absolute(path) || drive_prefixed {
        return None;
    }
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|segment| !segment.is_empty() && *segment != ".").collect();
    (!segments.is_empty() && !segments.contains(&"..")).then_some(segments)
}

/// `repositoryRelativeWorkspaceSegments`: where the workspace sits inside its repository,
/// lowercased on Windows. Empty when they are the same folder, `None` when unrelated.
fn workspace_segments(workspace_root: Option<&str>, repository_root: Option<&str>) -> Option<Vec<String>> {
    let (workspace, repository) = (workspace_root.filter(|root| !root.is_empty())?, repository_root.filter(|root| !root.is_empty())?);
    let (workspace, repository) = (normalize_project_path(workspace), normalize_project_path(repository));
    if workspace == repository {
        return Some(Vec::new());
    }
    let separator = if repository.contains('\\') { '\\' } else { '/' };
    let prefix = if repository.ends_with(separator) { repository } else { format!("{repository}{separator}") };
    let rest = workspace.strip_prefix(&prefix)?;
    Some(rest.split(['/', '\\']).filter(|segment| !segment.is_empty()).map(str::to_owned).collect())
}

/// `resolveDiffPathForWorkspace`: the workspace-relative path to open for a file a diff names, or
/// `None` when it lies outside the workspace and must not be opened.
pub fn resolve_diff_path_for_workspace(file_path: &str, workspace_root: Option<&str>, repository_root: Option<&str>) -> Option<String> {
    if is_absolute(file_path) {
        let relative = workspace_relative_path(file_path, workspace_root?)?;
        return relative_segments(&relative).map(|segments| segments.join("/"));
    }
    let file_segments = relative_segments(file_path)?;
    let workspace = match workspace_segments(workspace_root, repository_root) {
        Some(workspace) if !workspace.is_empty() => workspace,
        _ => return Some(file_segments.join("/")),
    };
    let case_insensitive = repository_root.is_some_and(is_windows_absolute);
    let belongs = workspace.iter().enumerate().all(|(index, segment)| {
        file_segments.get(index).is_some_and(|candidate| if case_insensitive { candidate.to_lowercase() == *segment } else { candidate == segment })
    });
    if !belongs {
        return None;
    }
    let relative = &file_segments[workspace.len()..];
    (!relative.is_empty()).then(|| relative.join("/"))
}

/// `revealInFileExplorerLabelForOs` (`apps/web/src/components/preview/fileExplorerLabel.ts`).
pub fn reveal_in_file_explorer_label_for_os(os: &str) -> &'static str {
    match os {
        "darwin" => "Reveal in Finder",
        "windows" => "Reveal in File Explorer",
        _ => "Reveal in Files",
    }
}

/// `resolveFileContextMenuAbsolutePath` (`apps/web/src/fileContextMenu.ts`): resolves a diff file
/// path into the host path to pass to `shell.openInEditor`, or `None` outside the workspace.
pub fn resolve_file_context_menu_absolute_path(
    file_path: &str,
    workspace_root: Option<&str>,
    repository_root: Option<&str>,
) -> Option<String> {
    let workspace_file_path = resolve_diff_path_for_workspace(file_path, workspace_root, repository_root)?;
    match workspace_root.filter(|root| !root.is_empty()) {
        None => is_absolute(&workspace_file_path).then_some(workspace_file_path),
        Some(root) => Some(absolute_path(&workspace_file_path, root)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContextMenuAction {
    Open,
    RevealInFolder,
    Editor { id: String, label: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContextMenuCapabilities {
    pub reveal_label: Option<&'static str>,
    pub can_open_default: bool,
    pub editors: Vec<(String, String)>,
}

impl FileContextMenuCapabilities {
    pub fn from_server(available_editors: &[String], os: &str) -> Self {
        let has_file_manager = available_editors.iter().any(|id| id == "file-manager");
        let editors = available_editors
            .iter()
            .filter(|id| id.as_str() != "file-manager")
            .map(|id| {
                let label = EDITOR_ORDER.iter().find(|(known, _)| *known == id).map_or(id.as_str(), |(_, label)| *label);
                (id.clone(), label.to_owned())
            })
            .collect();
        Self {
            reveal_label: has_file_manager.then(|| reveal_in_file_explorer_label_for_os(os)),
            can_open_default: has_file_manager,
            editors,
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.can_open_default && self.reveal_label.is_none() && self.editors.is_empty()
    }
}

/// Payload for `shell.openInEditor` from the diff header context menu (`fileContextMenu.ts`).
pub fn file_context_menu_open_request(
    absolute_path: &str,
    action: &FileContextMenuAction,
) -> (&'static str, Value) {
    match action {
        FileContextMenuAction::Open => (OPEN_IN_EDITOR, json!({ "cwd": absolute_path, "editor": "file-manager" })),
        FileContextMenuAction::RevealInFolder => {
            (OPEN_IN_EDITOR, json!({ "cwd": absolute_path, "editor": "file-manager", "reveal": true }))
        }
        FileContextMenuAction::Editor { id, .. } => (OPEN_IN_EDITOR, json!({ "cwd": absolute_path, "editor": id })),
    }
}

pub fn file_context_menu_error_title(action: &FileContextMenuAction) -> String {
    match action {
        FileContextMenuAction::Open => "Could not open file".to_owned(),
        FileContextMenuAction::RevealInFolder => "Unable to reveal file".to_owned(),
        FileContextMenuAction::Editor { label, .. } => format!("Could not open in {label}"),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_diff_path_for_workspace as resolve;
    use super::*;

    #[test]
    fn nested_projects_strip_their_prefix() {
        assert_eq!(resolve("frontend/Dockerfile", Some("/repo/frontend"), Some("/repo")).as_deref(), Some("Dockerfile"));
        assert_eq!(resolve("frontend/Dockerfile", Some("/worktrees/feature"), Some("/repo")).as_deref(), Some("frontend/Dockerfile"), "a separate worktree keeps repository paths");
        assert_eq!(resolve("Frontend/src\\index.ts", Some("C:\\repo\\frontend"), Some("C:\\repo")).as_deref(), Some("src/index.ts"));
        assert_eq!(resolve("src/a.ts", Some("/repo"), None).as_deref(), Some("src/a.ts"));
    }

    #[test]
    fn filesystem_root_repositories() {
        assert_eq!(resolve("frontend/index.ts", Some("/frontend"), Some("/")).as_deref(), Some("index.ts"));
        assert_eq!(resolve("frontend/index.ts", Some("C:\\frontend"), Some("C:\\")).as_deref(), Some("index.ts"));
    }

    #[test]
    fn absolute_tool_call_paths_resolve_inside_the_workspace() {
        assert_eq!(resolve("/repo/frontend/src/index.ts", Some("/repo/frontend"), Some("/repo/frontend")).as_deref(), Some("src/index.ts"));
        assert_eq!(resolve("c:\\Repo\\src\\index.ts", Some("C:\\repo"), Some("C:\\repo")).as_deref(), Some("src/index.ts"));
        assert_eq!(resolve("/repo/src/index.ts", None, None), None, "an absolute path needs a workspace");
    }

    #[test]
    fn out_of_project_paths_never_open() {
        for path in [
            "backend/server.ts",
            "frontend2/app.ts",
            "frontend/../secret.ts",
            "C:secret.ts",
            "/repo/backend/server.ts",
            "/repo/frontend/../backend/server.ts",
            "/repo/frontend",
            "/etc/passwd",
        ] {
            assert_eq!(resolve(path, Some("/repo/frontend"), Some("/repo")), None, "{path}");
        }
    }

    #[test]
    fn context_menu_resolves_workspace_absolute_path_and_actions() {
        assert_eq!(
            resolve_file_context_menu_absolute_path("frontend/src/a.ts", Some("/repo/frontend"), Some("/repo")).as_deref(),
            Some("/repo/frontend/src/a.ts")
        );
        assert_eq!(
            resolve_file_context_menu_absolute_path("/etc/passwd", Some("/repo/frontend"), Some("/repo")),
            None
        );
        let caps = FileContextMenuCapabilities::from_server(&["file-manager".into(), "cursor".into()], "darwin");
        assert!(caps.can_open_default);
        assert_eq!(caps.reveal_label, Some("Reveal in Finder"));
        assert_eq!(caps.editors, vec![("cursor".into(), "Cursor".into())]);
        assert_eq!(
            file_context_menu_open_request("/repo/a.ts", &FileContextMenuAction::RevealInFolder).1,
            json!({ "cwd": "/repo/a.ts", "editor": "file-manager", "reveal": true })
        );
        assert_eq!(reveal_in_file_explorer_label_for_os("windows"), "Reveal in File Explorer");
        assert_eq!(reveal_in_file_explorer_label_for_os("linux"), "Reveal in Files");
    }
}
