//! The entry points into the file surfaces, on `AppState` so any view can call them: the
//! launcher's "Files" (`addFilesSurface`), `openFile` for links, pickers and the explorer, folder
//! navigation from breadcrumbs, and the explorer's mention actions (`FileBrowserPanel.tsx`).

use gpui::{ClipboardItem, Context};
use t3_core::files::{absolute_path, clipboard_path, composer_file_link, file_surface_path};

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
        let thread_id = self.selected_thread.clone()?;
        let thread = self.thread_shell(&thread_id)?;
        let project = self.shell.project(&thread.project_id)?;
        let cwd = thread.worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone());
        (!cwd.is_empty()).then(|| FileWorkspace { thread_id, cwd, project_name: project.title.clone() })
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

    /// A breadcrumb opened a folder: in place of `surface_id`, or beside it when `keep_source`.
    pub fn navigate_file_folder(&mut self, surface_id: &str, folder: &str, keep_source: bool, cx: &mut Context<Self>) {
        self.mutate_panels(cx, |state, thread| state.panels.navigate_folder(thread, surface_id, folder, keep_source));
    }

    /// "Copy mention".
    pub fn copy_file_mention(&mut self, path: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(composer_file_link(path)));
        self.show_toast(Toast::new(ToastKind::Success, "Mention copied").description(path.to_owned()), cx);
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
        cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
        self.show_toast(Toast::new(ToastKind::Success, "Path copied").description(path), cx);
    }
}
