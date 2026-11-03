//! Project file actions shared by go-to-file and content search: the active project target
//! (`apps/web/src/hooks/useActiveProjectTarget.ts`), the composer mention hook, and opening a
//! file in the preferred editor (`apps/web/src/editorPreferences.ts`).

use gpui::Context;
use t3_core::project_search::{OPEN_IN_EDITOR, editor_target, open_in_editor_request, preferred_editor};

use crate::app_state::{AppEvent, AppState};
use crate::toast::{Toast, ToastKind};

const MENTION_QUEUE_LIMIT: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTarget {
    pub cwd: String,
    pub project_name: String,
}

/// A file the user picked to mention in the composer; the composer drains `composer_mentions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerMention {
    pub path: String,
    pub line: Option<u64>,
}

impl AppState {
    /// The open thread's worktree or project root, else the new-thread draft's project.
    pub fn active_project_target(&self) -> Option<ProjectTarget> {
        let thread = self.selected_thread.as_deref().and_then(|id| self.thread_shell(id));
        let project_id = thread.map(|thread| thread.project_id.clone()).or_else(|| self.draft_project.clone())?;
        let project = self.shell.project(&project_id)?;
        let cwd = thread.and_then(|thread| thread.worktree_path.clone()).unwrap_or_else(|| project.workspace_root.clone());
        Some(ProjectTarget { cwd, project_name: project.title.clone() })
    }

    /// Hands a file to the composer, which inserts `[name](path) ` at the end of the prompt.
    pub fn request_composer_mention(&mut self, mention: ComposerMention, cx: &mut Context<Self>) {
        // Bounded: a composer that never drains the queue must not grow it forever.
        if self.composer_mentions.len() >= MENTION_QUEUE_LIMIT {
            self.composer_mentions.remove(0);
        }
        self.composer_mentions.push(mention);
        cx.emit(AppEvent::ComposerMention);
    }

    /// The editor `shell.openInEditor` would use, by label, or `None` when the server has none.
    pub fn preferred_editor_label(&self) -> Option<&'static str> {
        let available = self.server_config.config.as_ref().map(|config| config.available_editors.as_slice()).unwrap_or_default();
        preferred_editor(available, None).map(|(_, label)| label)
    }

    /// Opens `relative_path` under `cwd` in the preferred editor. Returns false when the server
    /// offers no editor, so callers can fall back to a mention.
    pub fn open_in_editor(&mut self, cwd: &str, relative_path: &str, line: Option<u64>, cx: &mut Context<Self>) -> bool {
        let available = self.server_config.config.as_ref().map(|config| config.available_editors.clone()).unwrap_or_default();
        let Some((editor, label)) = preferred_editor(&available, None) else { return false };
        let request = self.rpc(OPEN_IN_EDITOR, open_in_editor_request(&editor_target(cwd, relative_path, line), editor));
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |state, cx| match result {
                Ok(_) => state.announce(format!("Opened in {label}"), None, cx),
                Err(error) => state.show_toast(Toast::new(ToastKind::Error, format!("Could not open in {label}")).description(error.to_string()), cx),
            });
        })
        .detach();
        true
    }
}
