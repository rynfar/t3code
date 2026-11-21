//! The palette's Add project flows (`CommandPalette.tsx` browse mode, `filesystem.ts`): a typed
//! path lists the host's folders from `filesystem.browse`, Enter enters a folder or adds the
//! path, Tab completes; and New project names a folder for `projects.createNew`.

use gpui::{AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list};
use gpui_component::{h_flex, v_flex};
use t3_core::auth::{ORCHESTRATION_OPERATE_SCOPE, SOURCE_CONTROL_WRITE_SCOPE};
use t3_core::git_actions::{self, PublishProviderDiscovery};
use t3_core::media::FILESYSTEM_READ_SCOPE;
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::projects::{
    BrowseResult, FILESYSTEM_BROWSE, add_target, append_browse_segment, browse_path, browse_request,
    browse_tab_completion, compute_clone_preview, extract_project_remote_url, filter_browse_entries,
    github_incomplete_owner_repository_message, is_clone_query, CreateNewResult, PROJECTS_CREATE_NEW, create_new_project,
};

use super::{CommandPalette, Mode};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::i18n::{t, t_fmt};
use crate::toast::{Toast, ToastKind};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 32.;

#[derive(Default)]
pub(super) struct ProjectBrowse {
    query: String,
    windows: bool,
    loaded_directory: Option<String>,
    result: Option<BrowseResult>,
    visible: Vec<usize>,
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

    pub fn set_query(&mut self, query: String, state: &Entity<AppState>, environment: Option<&ProjectEnvironment>, cx: &mut Context<CommandPalette>) {
        self.query = query;
        self.highlighted = 0;
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        self.windows = environment.is_some_and(|environment| environment.windows);
        let Some(path) = browse_path(&self.query, self.windows) else {
            self.request = None;
            self.pending = false;
            self.result = None;
            self.error = None;
            self.visible.clear();
            self.loaded_directory = None;
            return;
        };
        if self.loaded_directory.as_deref() == Some(path.directory.as_str()) {
            self.refresh_visible();
            return;
        }
        self.loaded_directory = Some(path.directory.clone());
        self.pending = true;
        self.result = None;
        self.error = None;
        self.visible.clear();
        let owner = environment.and_then(|environment| environment.owner.as_ref());
        let request = state.read(cx).project_request(owner, FILESYSTEM_READ_SCOPE, FILESYSTEM_BROWSE, browse_request(&path.directory, None));
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
                    Err(error) => browse.error = Some(error),
                }
                browse.refresh_visible();
                cx.notify();
            });
        }));
    }

    fn filter(&self) -> String {
        browse_path(&self.query, self.windows).map(|path| path.filter).unwrap_or_default()
    }

    fn refresh_visible(&mut self) {
        self.visible.clear();
        if let Some(result) = &self.result {
            let indices: std::collections::HashMap<_, _> = result.entries.iter().enumerate()
                .map(|(index, entry)| (std::ptr::from_ref(entry), index)).collect();
            self.visible.extend(filter_browse_entries(&result.entries, &self.filter())
                .iter().filter_map(|entry| indices.get(&std::ptr::from_ref(*entry)).copied()));
        }
    }

    fn visible_entry(&self, index: usize) -> Option<&t3_core::projects::BrowseEntry> {
        let index = *self.visible.get(index)?;
        self.result.as_ref()?.entries.get(index)
    }

    fn row_count(&self) -> usize {
        if is_clone_query(&self.query) {
            return 1;
        }
        usize::from(browse_path(&self.query, self.windows).is_some()) + self.visible.len()
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
        let entry = self.visible_entry(self.highlighted - 1)?;
        Some(if add { BrowsePick::Add(entry.full_path.clone()) } else { BrowsePick::Enter(append_browse_segment(&self.query, &entry.name)) })
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
                        if !this.project_destination_available(SOURCE_CONTROL_WRITE_SCOPE, cx) { return; }
                        let owner = this.project_flow.selected.as_ref().and_then(|target| target.owner.clone());
                        this.close(window, cx);
                        this.state.update(cx, |state, cx| state.open_clone_project_on(owner, initial, cx));
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
                let palette = cx.entity().read(cx);
                let app = palette.state.read(cx);
                let cfg = palette.project_flow.selected.as_ref().and_then(|environment| environment.config(app));
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
            let Some(entry) = self.visible_entry(index - 1) else { return div().into_any_element() };
            (Icon::Folder, entry.name.clone(), entry.full_path.clone())
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

#[derive(Clone)]
pub(super) struct ProjectEnvironment {
    pub owner: Option<EnvironmentOwner>,
    connection: Option<u64>,
    label: String,
    machine: Icon,
    pub base_directory: Option<String>,
    root: Option<String>,
    windows: bool,
}

impl ProjectEnvironment {
    fn config<'a>(&self, state: &'a AppState) -> Option<&'a t3_core::server_config::ServerConfig> {
        match &self.owner {
            Some(owner) => state.scoped_environment(&owner.saved_id)?.config.config.as_ref(),
            None => state.server_config.config.as_ref(),
        }
    }
}

#[derive(Clone)]
struct PublishTarget {
    kind: &'static str,
    label: &'static str,
    owner: Option<String>,
}

impl PublishTarget {
    fn repository(&self, path: &str) -> String {
        let name = t3_core::files::basename(path);
        self.owner.as_ref().map_or_else(|| name.to_owned(), |owner| format!("{owner}/{name}"))
    }
}

#[derive(Default)]
pub(super) struct ProjectFlow {
    intent: Option<Mode>,
    pub environments: Vec<ProjectEnvironment>,
    highlighted: usize,
    environment_scroll: UniformListScrollHandle,
    pub selected: Option<ProjectEnvironment>,
    publish_targets: Vec<PublishTarget>,
    publish: Option<usize>,
    discovery: Option<Task<()>>,
    submitting: bool,
    generation: u64,
    error: Option<String>,
}

impl ProjectFlow {
    pub fn is_picking(&self, intent: Mode, mode: Mode) -> bool {
        mode == Mode::ProjectEnvironment && self.intent == Some(intent)
    }

    pub fn reset_new_project(&mut self) {
        self.publish_targets.clear();
        self.publish = None;
        self.discovery = None;
        self.submitting = false;
        self.error = None;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn reset_environment_highlight(&mut self) {
        self.highlighted = 0;
        self.environment_scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
    }

    fn matching(&self, query: &str) -> impl Iterator<Item = (usize, &ProjectEnvironment)> {
        let query = query.trim().to_lowercase();
        self.environments.iter().enumerate().filter(move |(_, environment)| {
            query.is_empty() || environment.label.to_lowercase().contains(&query)
                || environment.owner.as_ref().and_then(|owner| owner.canonical_id.as_deref())
                    .is_some_and(|id| id.to_lowercase().contains(&query))
        })
    }
}

fn project_environments(state: &AppState, mode: Mode, cx: &App) -> Vec<ProjectEnvironment> {
    let capture = |owner: Option<EnvironmentOwner>, config: &t3_core::server_config::ServerConfig| {
        ProjectEnvironment {
            owner,
            connection: state.connection_id(),
            label: config.environment.label.clone(),
            machine: Icon::machine(config),
            base_directory: Some(config.settings.threads.add_project_base_directory.clone()),
            root: config.new_projects_root.clone(),
            windows: config.environment.platform.as_ref().is_some_and(|platform| matches!(platform.os.as_str(), "windows" | "win32")),
        }
    };
    let mut entries: Vec<_> = state.fleet.environments.iter().filter_map(|entry| {
        let owner = &entry.summary.owner;
        let scope = if mode == Mode::Browse { FILESYSTEM_READ_SCOPE } else { ORCHESTRATION_OPERATE_SCOPE };
        if !state.scoped_ready(owner, scope) { return None; }
        let config = entry.config.config.as_ref()?;
        if mode == Mode::NewProject && config.new_projects_root.is_none() { return None; }
        Some(capture(Some(owner.clone()), config))
    }).collect();
    let active_id = state.environments.active.as_deref();
    entries.sort_by_key(|env| env.owner.as_ref().is_none_or(|owner| Some(owner.saved_id.as_str()) != active_id));
    if !state.fleet.environments.iter().any(|entry| Some(entry.summary.owner.saved_id.as_str()) == active_id)
        && let Some(config) = state.server_config.config.as_ref()
        && (mode == Mode::Browse || config.new_projects_root.is_some())
        && active_project_grant(state, if mode == Mode::Browse { FILESYSTEM_READ_SCOPE } else { ORCHESTRATION_OPERATE_SCOPE })
    {
        entries.insert(0, capture(None, config));
    }
    for entry in &mut entries {
        if entry.label.trim().is_empty() {
            entry.label = t(cx, "This machine").to_owned();
        }
    }
    entries
}

fn active_project_grant(state: &AppState, scope: &str) -> bool {
    state.client_handle().is_some_and(|client| {
        client.session_state().borrow().as_ref().is_none_or(|session| session.grants(scope))
    })
}

fn project_name_preview(root: &str, name: &str) -> String {
    let latin = name.replace(['ß', 'ẞ'], "ss").replace(['Æ', 'æ'], "ae")
        .replace(['Œ', 'œ'], "oe").replace(['Þ', 'þ'], "th");
    let folded = t3_core::search::fold_search_query(&latin);
    let normalized = folded.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty()).collect::<Vec<_>>().join("-");
    let name = normalized.get(..normalized.len().min(64)).unwrap_or_default().trim_end_matches('-');
    let reserved = matches!(name, "con" | "prn" | "aux" | "nul")
        || name.strip_prefix("com").or_else(|| name.strip_prefix("lpt"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'));
    let name = if name.is_empty() { "project".to_owned() }
        else if reserved { format!("{name}-project") }
        else { name.to_owned() };
    t3_core::projects::clone_destination_path(root, &name)
}

impl CommandPalette {
    pub(super) fn enter_entry_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(mode, Mode::Browse | Mode::NewProject) {
            self.enter_mode(mode, window, cx);
            return;
        }
        self.project_flow.reset_new_project();
        self.project_flow.intent = Some(mode);
        self.project_flow.reset_environment_highlight();
        self.project_flow.environments = project_environments(self.state.read(cx), mode, cx);
        if let [environment] = self.project_flow.environments.as_slice() {
            self.project_flow.selected = Some(environment.clone());
            self.enter_mode(mode, window, cx);
        } else {
            self.project_flow.selected = None;
            self.enter_mode(Mode::ProjectEnvironment, window, cx);
        }
    }

    pub(super) fn move_project_environment(&mut self, step: isize, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value();
        let count = self.project_flow.matching(&query).count() as isize;
        if count > 0 {
            self.project_flow.highlighted = (self.project_flow.highlighted as isize + step).rem_euclid(count) as usize;
            self.project_flow.environment_scroll.scroll_to_item(self.project_flow.highlighted, gpui::ScrollStrategy::Nearest);
        }
    }

    pub(super) fn pick_project_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value();
        let index = self.project_flow.matching(&query).nth(self.project_flow.highlighted).map(|(index, _)| index);
        if let Some(index) = index {
            self.choose_project_environment(index, window, cx);
        }
    }

    fn choose_project_environment(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.project_flow.intent else { return };
        self.project_flow.reset_new_project();
        self.project_flow.selected = self.project_flow.environments.get(index).cloned();
        self.enter_mode(mode, window, cx);
    }

    pub(super) fn project_destination_available(&self, scope: &str, cx: &App) -> bool {
        let state = self.state.read(cx);
        self.project_flow.selected.as_ref().is_some_and(|environment| match &environment.owner {
            Some(owner) => state.scoped_ready(owner, scope),
            None => state.connection_id() == environment.connection && active_project_grant(state, scope),
        })
    }

    pub(super) fn render_project_environments(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let query = self.input.read(cx).value();
        let entries: Vec<_> = self.project_flow.matching(&query).map(|(index, _)| index).collect();
        if entries.is_empty() {
            let message = if self.project_flow.environments.is_empty() {
                t(cx, "No environments available.")
            } else {
                t(cx, "No matches found.")
            };
            return div().p(px(20.)).text_sm().text_color(hsla(p.muted_foreground)).child(message).into_any_element();
        }
        let count = entries.len();
        div().p(px(8.)).min_h_0().child(
            uniform_list(
                "palette-project-environments",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range.map(|row| this.render_environment_row(row, entries[row], cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.project_flow.environment_scroll)
            .h(px((count as f32 * ROW_HEIGHT).min(300.))),
        ).into_any_element()
    }

    fn render_environment_row(&self, row: usize, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let environment = &self.project_flow.environments[index];
        h_flex().id(("palette-project-environment", index)).h(px(ROW_HEIGHT)).gap(px(8.)).px(px(8.))
            .rounded(px(6.)).cursor_pointer().text_sm()
            .when(row == self.project_flow.highlighted, |row| row.bg(hsla(p.accent)))
            .child(environment.machine.render(px(16.), hsla(p.muted_foreground)))
            .child(div().truncate().child(environment.label.clone()))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.project_flow.highlighted != row {
                    this.project_flow.highlighted = row;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.choose_project_environment(index, window, cx)))
            .into_any_element()
    }

    pub(super) fn discover_new_project_publish_targets(&mut self, cx: &mut Context<Self>) {
        self.project_flow.reset_new_project();
        if !self.project_destination_available(SOURCE_CONTROL_WRITE_SCOPE, cx) { return; }
        let Some(environment) = self.project_flow.selected.as_ref() else { return };
        let state = self.state.read(cx);
        let offered = environment.config(state).map(|config| config.settings.source_control.offered_source_control_providers.clone());
        let request = state.project_request(environment.owner.as_ref(), SOURCE_CONTROL_WRITE_SCOPE, git_actions::method::SOURCE_CONTROL_DISCOVERY, serde_json::json!({}));
        self.project_flow.discovery = Some(cx.spawn(async move |this, cx| {
            let Ok(result) = request.await else { return };
            let discovery: Vec<PublishProviderDiscovery> = git_actions::decode_publish_discovery(&result);
            let targets = git_actions::offered_publish_providers(offered.as_ref(), &discovery)
                .into_iter().filter_map(|(provider, readiness)| {
                    let found = discovery.iter().find(|item| item.kind == provider.kind)?;
                    if !readiness.ready || matches!(provider.kind, "azure-devops" | "bitbucket")
                        || (matches!(provider.kind, "forgejo" | "gitcafe") && found.auth_account.is_none()) {
                        return None;
                    }
                    Some(PublishTarget { kind: provider.kind, label: provider.label, owner: found.auth_account.clone() })
                }).collect();
            let _ = this.update(cx, |this, cx| {
                this.project_flow.publish_targets = targets;
                cx.notify();
            });
        }));
    }

    pub(super) fn submit_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.input.read(cx).value().trim().to_owned();
        if name.is_empty() || self.project_flow.submitting { return; }
        if !self.project_destination_available(ORCHESTRATION_OPERATE_SCOPE, cx) {
            self.project_flow.error = Some(t(cx, "The project environment is no longer available.").to_owned());
            cx.notify();
            return;
        }
        let Some(environment) = self.project_flow.selected.clone() else { return };
        let publish = self.project_flow.publish.and_then(|index| self.project_flow.publish_targets.get(index)).cloned();
        if publish.is_some() && !self.project_destination_available(SOURCE_CONTROL_WRITE_SCOPE, cx) {
            self.project_flow.error = Some(t(cx, "Publishing is no longer available on this environment.").to_owned());
            cx.notify();
            return;
        }
        self.project_flow.submitting = true;
        self.project_flow.error = None;
        let generation = self.project_flow.generation;
        let state = self.state.clone();
        let request = state.read(cx).project_request(environment.owner.as_ref(), ORCHESTRATION_OPERATE_SCOPE, PROJECTS_CREATE_NEW, create_new_project(&name));
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await.and_then(|value| serde_json::from_value::<CreateNewResult>(value).map_err(|error| error.to_string()));
            let created = match result {
                Ok(project) => project,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.project_flow.generation == generation {
                            this.project_flow.submitting = false;
                            this.project_flow.error = Some(error);
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let owner = environment.owner;
            let path = created.workspace_root;
            let _ = this.update_in(cx, |this, window, cx| {
                state.update(cx, |state, cx| {
                    let label = if created.commit_error.is_some() { "Project created without a first commit" } else { "Project created" };
                    let kind = if created.commit_error.is_some() { ToastKind::Warning } else { ToastKind::Success };
                    let detail = created.commit_error.unwrap_or_else(|| path.clone());
                    state.show_toast(Toast::new(kind, t(cx, label)).description(detail), cx);
                });
                if this.project_flow.generation == generation {
                    this.close(window, cx);
                    state.update(cx, |state, cx| {
                        if owner.is_some() || state.connection_id() == environment.connection {
                            state.open_created_project(owner.clone(), created.project_id, cx);
                        }
                    });
                }
            });
            let Some(target) = publish else { return };
            let repository = target.repository(&path);
            let payload = serde_json::json!({
                "cwd": path, "provider": target.kind, "repository": repository, "visibility": "private"
            });
            let request = cx.update(|_, cx| {
                let state = state.read(cx);
                if owner.is_none() && state.connection_id() != environment.connection { return None; }
                Some(state.project_request(owner.as_ref(), SOURCE_CONTROL_WRITE_SCOPE, git_actions::method::SOURCE_CONTROL_PUBLISH_REPOSITORY, payload))
            }).ok().flatten();
            let Some(request) = request else { return };
            let result = request.await.and_then(|value| serde_json::from_value::<git_actions::PublishRepositoryResult>(value).map_err(|error| error.to_string()));
            let _ = cx.update(|_, cx| state.update(cx, |state, cx| {
                let (kind, label, detail) = match result {
                    Ok(repo) => (ToastKind::Success, "Published to {label}", repo.repository.name_with_owner),
                    Err(error) => (ToastKind::Error, "Could not create the {label} repository", error),
                };
                state.show_toast(Toast::new(kind, t_fmt(cx, label, &[("label", target.label.into())])).description(detail), cx);
            }));
        }).detach();
        cx.notify();
    }

    pub(super) fn render_new_project(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(environment) = self.project_flow.selected.as_ref() else { return div().into_any_element() };
        let name = self.input.read(cx).value();
        let path = environment.root.as_deref().map(|root| project_name_preview(root, &name)).unwrap_or_default();
        let submitting = self.project_flow.submitting;
        v_flex().p(px(16.)).gap(px(10.)).text_sm()
            .child(h_flex().gap(px(8.)).text_color(hsla(p.muted_foreground))
                .child(environment.machine.render(px(14.), hsla(p.icon_muted)))
                .child(div().truncate().child(environment.label.clone())))
            .child(div().text_ellipsis_middle().text_color(hsla(p.foreground)).child(path.clone()))
            .children(self.project_flow.publish_targets.iter().enumerate().map(|(index, target)| {
                let selected = self.project_flow.publish == Some(index);
                let label = t_fmt(cx, "Publish to {label}", &[("label", target.label.into())]);
                let repository = target.repository(&path);
                Button::new(("new-project-publish", index), if selected { ButtonVariant::Secondary } else { ButtonVariant::Ghost }, ButtonSize::Sm)
                    .icon(if selected { Icon::Check } else { Icon::Upload })
                    .label(format!("{label} · {repository}"))
                    .disabled(submitting)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.project_flow.publish = (!selected).then_some(index);
                        cx.notify();
                    }))
            }))
            .children(self.project_flow.error.as_ref().map(|error| div().text_color(hsla(p.error_foreground)).child(error.clone())))
            .child(div().flex().justify_end().child(
                Button::new("palette-create-project", ButtonVariant::Default, ButtonSize::Sm)
                    .label(if submitting { "Creating…" } else { "Create project" })
                    .disabled(submitting || name.trim().is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.submit_new_project(window, cx))),
            ))
            .into_any_element()
    }
}
