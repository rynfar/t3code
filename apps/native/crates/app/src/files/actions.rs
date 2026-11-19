//! The entry points into the file surfaces, on `AppState` so any view can call them: the
//! launcher's "Files" (`addFilesSurface`), `openFile` for links, pickers and the explorer, folder
//! navigation from breadcrumbs, and the explorer's mention actions (`FileBrowserPanel.tsx`).

use gpui::{ClipboardItem, Context};
use t3_core::files::{absolute_path, clipboard_path, composer_file_link, file_surface_path};
use t3_core::markdown::links::open_in_editor_request_with_root;
use t3_core::project_search::{OPEN_IN_EDITOR, editor_target, preferred_editor};

use crate::app_state::AppState;
use crate::toast::{Toast, ToastKind};

/// Where the open thread's files live (`RightPanelFileProps`): its worktree, else its project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWorkspace {
    pub thread_id: String,
    pub cwd: String,
    pub project_name: String,
}

impl AppState {
    pub fn file_workspace(&self) -> Option<FileWorkspace> {
        if let Some(thread_id) = self.selected_thread.clone() {
            let thread = self.thread_shell(&thread_id)?;
            let project = self.shell.project(&thread.project_id)?;
            let cwd = thread.worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone());
            return (!cwd.is_empty()).then(|| FileWorkspace { thread_id, cwd, project_name: project.title.clone() });
        }
        let project_id = self.draft_project.as_deref()?;
        let project = self.shell.project(project_id)?;
        let cwd = project.workspace_root.clone();
        (!self.is_scratch_project(project_id) && !cwd.is_empty()).then(|| FileWorkspace {
            thread_id: t3_core::composer::drafts::new_thread_key(project_id),
            cwd,
            project_name: project.title.clone(),
        })
    }

    /// `openFile`: shows `path` (workspace-relative, or absolute) in a file tab, scrolling to
    /// the 1-based `line` when given. The workspace root itself opens the explorer.
    pub fn open_file(&mut self, path: &str, line: Option<u64>, cx: &mut Context<Self>) {
        let Some(workspace) = self.file_workspace() else { return };
        let target = file_surface_path(path, Some(&workspace.cwd));
        self.mutate_panels(cx, |state, thread| state.panels.open_file(thread, target.as_deref(), line));
    }

    /// The "Files" launcher entry and add-menu item.
    pub fn add_files_surface(&mut self, cx: &mut Context<Self>) {
        if self.file_workspace().is_some() {
            self.mutate_panels(cx, |state, thread| state.panels.open_file(thread, None, None));
        }
    }

    /// A breadcrumb opened a folder while keeping its deeper `folder_trail` (`FileBreadcrumbs.tsx`).
    pub fn navigate_file_folder_with_trail(
        &mut self,
        surface_id: &str,
        folder: &str,
        folder_trail: &str,
        keep_source: bool,
        cx: &mut Context<Self>,
    ) {
        self.mutate_panels(cx, |state, thread| {
            state.panels.navigate_folder_with_trail(thread, surface_id, folder, folder_trail, keep_source)
        });
    }

    /// Opens a chat file attachment in a right-panel attachment tab (`openAttachment`).
    pub fn open_attachment(&mut self, attachment: t3_core::right_panel::FileAttachment, cx: &mut Context<Self>) {
        self.mutate_panels(cx, |state, thread| state.panels.open_attachment(thread, attachment));
    }

    /// "Copy mention".
    pub fn copy_file_mention(&mut self, path: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(composer_file_link(path)));
    }

    /// "Add to chat": queued for the composer like the file picker's mentions.
    pub fn add_file_to_chat(&mut self, path: &str, cx: &mut Context<Self>) {
        self.request_composer_mention(crate::project_files::ComposerMention { path: path.to_owned(), line: None }, cx);
    }

    /// The file tab's "Copy path" (`copyRightPanelFilePath`): the path as the tab names it, with
    /// Windows separators on a Windows host.
    pub fn copy_file_path(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        let cwd = self.file_workspace().map(|workspace| workspace.cwd).unwrap_or_default();
        let path = clipboard_path(relative_path, &absolute_path(relative_path, &cwd));
        cx.write_to_clipboard(ClipboardItem::new_string(path));
    }

    /// Opens a workspace or host path in the preferred editor, passing `projectRoot` (`#33558`).
    pub fn open_file_in_preferred_editor(&mut self, relative_path: &str, line: Option<u64>, cx: &mut Context<Self>) -> bool {
        let Some(workspace) = self.file_workspace() else { return false };
        let available = self
            .server_config
            .config
            .as_ref()
            .map(|config| config.available_editors.clone())
            .unwrap_or_default();
        let Some((editor, label)) = preferred_editor(&available, None) else { return false };
        let target = editor_target(&workspace.cwd, relative_path, line);
        let request = self.rpc(OPEN_IN_EDITOR, open_in_editor_request_with_root(&target, editor, Some(&workspace.cwd)));
        cx.spawn(async move |this, cx| {
            if let Err(error) = request.await {
                let _ = this.update(cx, |state, cx| {
                    state.show_toast(Toast::new(ToastKind::Error, format!("Could not open in {label}")).description(error.to_string()), cx);
                });
            }
        })
        .detach();
        true
    }
}
