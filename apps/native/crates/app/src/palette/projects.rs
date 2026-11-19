//! The palette's Add project flows (`CommandPalette.tsx` browse mode, `filesystem.ts`): a typed
//! path lists the host's folders from `filesystem.browse`, Enter enters a folder or adds the
//! path, Tab completes; and New project names a folder for `projects.createNew`.

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Task, UniformListScrollHandle, div, prelude::*, px, uniform_list};
use gpui_component::h_flex;
use t3_core::projects::{
    BrowseResult, FILESYSTEM_BROWSE, add_target, append_browse_segment, browse_path, browse_request,
    browse_tab_completion, compute_clone_preview, extract_project_remote_url, filter_browse_entries,
    github_incomplete_owner_repository_message, is_clone_query,
};

use super::CommandPalette;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 32.;

#[derive(Default)]
pub(super) struct ProjectBrowse {
    query: String,
    windows: bool,
    loaded_directory: Option<String>,
    result: Option<BrowseResult>,
    pending: bool,
    error: Option<String>,
    /// Row 0 adds the typed path (or clones a pasted git URL / `owner/repo`); rows after it are folders.
    pub highlighted: usize,
    pub scroll: UniformListScrollHandle,
    request: Option<Task<()>>,
}

/// What Enter does in the browse list.
pub(super) enum BrowsePick {
    Add(String),
    Enter(String),
}

impl ProjectBrowse {
    pub fn reset(&mut self) {
        *self = ProjectBrowse::default();
    }

    pub fn set_query(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<CommandPalette>) {
        self.query = query;
        self.highlighted = 0;
        self.windows = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|config| config.environment.platform.as_ref())
            .is_some_and(|platform| matches!(platform.os.as_str(), "windows" | "win32"));
        let Some(path) = browse_path(&self.query, self.windows) else {
            self.result = None;
            self.loaded_directory = None;
            return;
        };
        if self.loaded_directory.as_deref() == Some(path.directory.as_str()) {
            return;
        }
        self.loaded_directory = Some(path.directory.clone());
        self.pending = true;
        let request = state.read(cx).rpc(FILESYSTEM_BROWSE, browse_request(&path.directory, None));
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |palette, cx| {
                let browse = &mut palette.browse;
                browse.pending = false;
                match result.map(serde_json::from_value::<BrowseResult>) {
                    Ok(Ok(result)) => {
                        browse.result = Some(result);
                        browse.error = None;
                    }
                    Ok(Err(error)) => browse.error = Some(error.to_string()),
                    Err(error) => browse.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    fn filter(&self) -> String {
        browse_path(&self.query, self.windows).map(|path| path.filter).unwrap_or_default()
    }

    fn visible_names(&self) -> Vec<(String, String)> {
        let filter = self.filter();
        self.result.as_ref().map(|result| filter_browse_entries(&result.entries, &filter).into_iter().map(|entry| (entry.name.clone(), entry.full_path.clone())).collect()).unwrap_or_default()
    }

    fn row_count(&self) -> usize {
        if is_clone_query(&self.query) {
            return 1;
        }
        usize::from(browse_path(&self.query, self.windows).is_some()) + self.visible_names().len()
    }

    pub fn move_highlight(&mut self, step: isize) {
        let count = self.row_count() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + step).rem_euclid(count) as usize;
            self.scroll.scroll_to_item(self.highlighted, gpui::ScrollStrategy::Top);
        }
    }

    /// Enter: add the typed path (or clone URL) from the first row, else enter the highlighted folder.
    /// With `add`, a folder row is added instead of entered.
    pub fn pick(&self, add: bool) -> Option<BrowsePick> {
        if is_clone_query(&self.query) {
            return Some(BrowsePick::Add(self.query.trim().to_owned()));
        }
        browse_path(&self.query, self.windows)?;
        if self.highlighted == 0 {
            return Some(BrowsePick::Add(add_target(&self.query)));
        }
        let (name, full_path) = self.visible_names().into_iter().nth(self.highlighted - 1)?;
        Some(if add { BrowsePick::Add(full_path) } else { BrowsePick::Enter(append_browse_segment(&self.query, &name)) })
    }

    /// Tab: the new query, when completion applies.
    pub fn complete(&self) -> Option<String> {
        let result = self.result.as_ref()?;
        let filter = self.filter();
        let visible = filter_browse_entries(&result.entries, &filter);
        let (name, complete) = browse_tab_completion(&visible, &filter)?;
        Some(if complete { append_browse_segment(&self.query, &name) } else { format!("{}{name}", t3_core::projects::browse_directory_path(&self.query)) })
    }

    fn render_footer_actions(&self, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let add_to_machine_target: Option<(String, String, String)> = {
            let app = cx.entity().read(cx).state.read(cx);
            let active_saved = app.environments.active.clone().unwrap_or_default();
            let project_id = crate::new_thread::default_project(app);
            project_id.and_then(|pid| {
                let project = app.shell.project(&pid)?;
                extract_project_remote_url(project.repository_identity.as_ref())?;
                let targets = app.add_project_to_machine_targets(&active_saved, &pid);
                (!targets.is_empty()).then(|| (active_saved, pid, project.title.clone()))
            })
        };
        let initial_clone = if is_clone_query(&self.query) {
            self.query.trim().to_owned()
        } else {
            String::new()
        };
        h_flex()
            .mt(px(8.))
            .pt(px(8.))
            .px(px(8.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .justify_between()
            .items_center()
            .gap(px(8.))
            .child(
                Button::new("palette-open-clone-modal", ButtonVariant::Ghost, ButtonSize::Xs)
                    .label("Clone repository…")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let initial = initial_clone.clone();
                        this.close(window, cx);
                        this.state.update(cx, |state, cx| state.open_clone_project_modal(initial, cx));
                    })),
            )
            .when_some(add_to_machine_target, |row, (saved_id, project_id, title)| {
                row.child(
                    Button::new("palette-open-add-to-machine", ButtonVariant::Ghost, ButtonSize::Xs)
                        .label(format!("Add {title} to another machine…"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close(window, cx);
                            this.state.update(cx, |state, cx| {
                                state.open_add_project_to_machine(saved_id.clone(), project_id.clone(), None, cx);
                            });
                        })),
                )
            })
            .into_any_element()
    }

    pub fn render(&self, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        if is_clone_query(&self.query) {
            return div()
                .flex()
                .flex_col()
                .min_h_0()
                .p(px(8.))
                .child(self.render_row(0, cx))
                .child(self.render_footer_actions(cx))
                .into_any_element();
        }
        if browse_path(&self.query, self.windows).is_none() {
            let hint = github_incomplete_owner_repository_message("github", &self.query)
                .unwrap_or("Enter project path (e.g. ~/projects/my-app) or repository URL (owner/repo)");
            return div()
                .flex()
                .flex_col()
                .p(px(8.))
                .child(
                    div()
                        .py(px(28.))
                        .px(px(16.))
                        .flex()
                        .justify_center()
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child(hint),
                )
                .child(self.render_footer_actions(cx))
                .into_any_element();
        }
        let count = self.row_count();
        let path_missing = self.result.as_ref().is_some_and(|r| r.path_missing);
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .p(px(8.))
            .children(self.error.clone().map(|error| div().px(px(8.)).py(px(4.)).text_xs().text_color(hsla(p.destructive)).child(error)))
            .when(path_missing && self.error.is_none(), |this| {
                this.child(div().px(px(8.)).py(px(4.)).text_xs().text_color(hsla(p.muted_foreground)).child("Folder does not exist yet — it will be created."))
            })
            .when(self.pending && self.result.is_none(), |this| this.child(div().px(px(8.)).py(px(4.)).text_xs().text_color(hsla(p.muted_foreground)).child("Loading folders…")))
            .child(
                uniform_list("palette-browse", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.browse.render_row(index, cx)).collect::<Vec<_>>()))
                    .track_scroll(&self.scroll)
                    .h(px((count as f32 * ROW_HEIGHT).min(380.))),
            )
            .child(self.render_footer_actions(cx))
            .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let (icon, title, detail) = if index == 0 && is_clone_query(&self.query) {
            let (base_dir, fallback_root, ghq_layout) = {
                let app = cx.entity().read(cx).state.read(cx);
                let cfg = app.server_config.config.as_ref();
                (
                    cfg.map(|c| c.settings.threads.add_project_base_directory.clone())
                        .filter(|s| !s.trim().is_empty()),
                    cfg.and_then(|c| c.new_projects_root.clone()),
                    cfg.is_some_and(|c| c.settings.threads.ghq_clone_layout),
                )
            };
            let preview = compute_clone_preview(
                &self.query,
                base_dir.as_deref(),
                fallback_root.as_deref(),
                ghq_layout,
                None,
            );
            (
                Icon::FolderTree,
                format!("Clone {}…", self.query.trim()),
                format!("{} → {}", preview.remote_url, preview.destination_path),
            )
        } else if index == 0 {
            let missing = self.result.as_ref().is_some_and(|r| r.path_missing);
            (
                Icon::FolderTree,
                format!("Add {}", add_target(&self.query)),
                if missing { "Will create folder".to_owned() } else { String::new() },
            )
        } else {
            let Some((name, full_path)) = self.visible_names().into_iter().nth(index - 1) else { return div().into_any_element() };
            (Icon::Folder, name, full_path)
        };
        div()
            .id(("palette-browse-row", index))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_sm()
            .when(index == self.highlighted, |this| this.bg(hsla(p.foreground).opacity(0.09)))
            .child(icon.render(px(16.), hsla(p.icon_muted)))
            .child(div().flex_none().text_color(hsla(p.foreground)).font_weight(if index == 0 { FontWeight::MEDIUM } else { FontWeight::NORMAL }).child(title))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(detail))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.browse.highlighted != index {
                    this.browse.highlighted = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.browse.highlighted = index;
                this.browse_pick(event.modifiers().platform, window, cx);
            }))
            .into_any_element()
    }
}
