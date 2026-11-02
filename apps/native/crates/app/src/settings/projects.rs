//! Settings → Projects: `ProjectsSettings` and `ProjectSettingsPanel` in
//! `web/src/components/settings/`. Without a chosen project the page lists them
//! (`SettingsScopeNotice`); a chosen one shows its name, icon, pin and archive, its New threads
//! overrides (`ProjectDefaultsSettings` at project scope), its actions (`project_actions.rs`), its
//! checkouts and removal. The page's project picker is the project half of the web's
//! `SettingsScopeSentence`; this client holds one environment, so there is no environment half.
//! Not ported: the icon and image pickers ("Choose icon", "Choose file"), and the info alert about
//! keeping the project picked on other pages, which other native pages do not scope by project.

use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, img, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::commands::method;
use t3_core::projects::PROJECTS_MUTATE;
use t3_core::server_config::{ServerConfig, ThreadEnvMode};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::project_actions::{ProjectFile, project_file_from_read};
use t3_core::settings::projects::{
    self as model, NO_PROJECT_NOTICE, NO_PROJECTS, ProjectIcon, ProjectUpdate, RemovalMember, Rename, archive_description, choice_secondary,
    danger_copy, edit_denied_notice, plan_removal, project_env_mode, project_model, resolve_selection,
};
use t3_core::settings::ENV_MODES;
use t3_core::shell::ProjectShell;
use t3_core::sidebar::ENVIRONMENT_ID;
use t3_core::sidebar::grouping::{ProjectGroup, build_project_groups};
use t3_core::workspace::{self, Source};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, group, icon, muted_text, r, section, tooltip};
use super::project_actions::ActionsState;
use super::threads::{env_mode_value, model_options, parse_selection, selection_label, selection_value};
use super::{Block, SettingsView, store};
use crate::icons::Icon;
use crate::sidebar::controller::uuid;
use crate::theme::{hsla, palette};

pub(super) const SETTINGS_WRITE_SCOPE: &str = "settings:write";

/// The list's "no project" value in the picker.
const ALL_PROJECTS: &str = "";

pub(crate) struct ProjectsState {
    /// The chosen group's key; `None` lists the projects.
    pub(super) selected: Option<String>,
    /// The members the page last showed, so a grouping change follows the project.
    last_members: Vec<String>,
    /// A project an entry point asked for before the shell listed it.
    pending_project: Option<String>,
    pub(super) name: Entity<InputState>,
    /// The Name field was typed in since it last committed.
    name_edited: bool,
    organizing: bool,
    saving_icon: bool,
    /// `t3.json` of the shown project's first checkout: its folder and the read's result, `None`
    /// while it loads.
    pub(super) project_file: Option<(String, Option<ProjectFile>)>,
    pub(crate) actions: ActionsState,
    _subscriptions: Vec<Subscription>,
}

impl ProjectsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.projects.name_edited = true,
            InputEvent::Blur => this.commit_project_name(cx),
            InputEvent::PressEnter { .. } => {
                this.commit_project_name(cx);
                this.focus.focus(window, cx);
            }
            _ => {}
        })];
        ProjectsState {
            selected: None,
            last_members: Vec::new(),
            pending_project: None,
            name,
            name_edited: false,
            organizing: false,
            saving_icon: false,
            project_file: None,
            actions: ActionsState::new(window, cx),
            _subscriptions: subscriptions,
        }
    }
}

/// One project group with its members, as the page reads it.
pub(super) struct Shown {
    pub(super) group: ProjectGroup,
    pub(super) members: Vec<ProjectShell>,
}

impl Shown {
    pub(super) fn member_ids(&self) -> Vec<&str> {
        self.members.iter().map(|member| member.id.as_str()).collect()
    }

    fn display_name(&self) -> &str {
        &self.group.label
    }
}

impl SettingsView {
    /// `useSettingsProjectGroups`.
    pub(super) fn project_groups(&self, cx: &App) -> Vec<ProjectGroup> {
        let state = self.state.read(cx);
        let Some(snapshot) = state.shell.snapshot.as_ref() else { return Vec::new() };
        let mut groups = build_project_groups(ENVIRONMENT_ID, &snapshot.projects, &state.sidebar.settings.grouping);
        model::sort_groups(&mut groups);
        groups
    }

    fn shown_project(&self, cx: &App) -> Option<Shown> {
        let key = self.projects.selected.as_deref()?;
        let groups = self.project_groups(cx);
        let group = resolve_selection(&groups, key, &self.projects.last_members)?.clone();
        let state = self.state.read(cx);
        let members = group.member_project_ids.iter().filter_map(|id| state.shell.project(id).cloned()).collect::<Vec<_>>();
        (!members.is_empty()).then_some(Shown { group, members })
    }

    pub(super) fn shown_members(&self, cx: &App) -> Vec<ProjectShell> {
        self.shown_project(cx).map(|shown| shown.members).unwrap_or_default()
    }

    /// Opens the page on the group holding `project_id` (`openProjectSettings`), once the shell
    /// lists it.
    pub(super) fn select_project_id(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let key = self.project_groups(cx).into_iter().find(|group| group.member_project_ids.iter().any(|id| id == project_id)).map(|group| group.key);
        self.projects.pending_project = if key.is_none() { Some(project_id.to_owned()) } else { None };
        if let Some(key) = key {
            self.select_project(Some(key), cx);
        }
    }

    pub(super) fn select_project(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        self.projects.pending_project = None;
        if self.projects.selected == key {
            return;
        }
        self.projects.selected = key;
        self.projects.last_members.clear();
        self.projects.name_edited = false;
        self.projects.actions.close();
        self.scroll_to_target = self.target.is_some();
        self.scroll.set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        cx.notify();
    }

    fn commit_project_name(&mut self, cx: &mut Context<Self>) {
        let was_edited = std::mem::take(&mut self.projects.name_edited);
        let Some(shown) = self.shown_project(cx) else { return };
        let value = self.projects.name.read(cx).value().to_string();
        let titles: Vec<&str> = shown.members.iter().map(|member| member.title.as_str()).collect();
        match model::rename(&titles, &value, was_edited) {
            Rename::Empty => store::show_error("Project title cannot be empty", cx),
            Rename::Unchanged => {}
            Rename::Save(title) => self.update_project_group(&shown.members, ProjectUpdate { title: Some(title), ..Default::default() }, "Failed to rename project", cx),
        }
    }

    pub(super) fn commit_focused_project_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.name.focus_handle(cx).is_focused(window) {
            self.commit_project_name(cx);
        }
    }

    pub(super) fn can_edit_projects(&self, cx: &App) -> bool {
        self.may(ORCHESTRATION_OPERATE_SCOPE, cx)
    }

    /// `checkProjectAccess`: the first failure says which connection lacks the scope.
    fn project_access_denied(&self, failure_title: &str, cx: &mut App) -> bool {
        if self.can_edit_projects(cx) {
            return false;
        }
        let label = self.environment_label(cx);
        store::show_error(format!("{failure_title}: This connection cannot change projects in {label}."), cx);
        true
    }

    pub(super) fn environment_label(&self, cx: &App) -> String {
        self.thread_settings(cx).map(|config| config.environment.label.clone()).filter(|label| !label.is_empty()).unwrap_or_else(|| "this environment".to_owned())
    }

    /// `updateProjectGroup`: one `project.update` per member, in order, stopping at the first
    /// failure.
    fn update_project_group(&mut self, members: &[ProjectShell], update: ProjectUpdate, failure_title: &'static str, cx: &mut Context<Self>) {
        let updates = members.iter().map(|member| (member.id.clone(), update.clone())).collect();
        self.update_project_members(updates, failure_title, cx);
    }

    /// `updateProjectGroup` with an update per member (`defaultProjectIcon` differs by member).
    fn update_project_members(&mut self, updates: Vec<(String, ProjectUpdate)>, failure_title: &'static str, cx: &mut Context<Self>) {
        if self.project_access_denied(failure_title, cx) {
            self.projects.organizing = false;
            self.projects.saving_icon = false;
            return;
        }
        let label = self.environment_label(cx);
        let many = updates.len() > 1;
        let requests: Vec<_> = updates.iter().map(|(id, update)| self.state.read(cx).rpc(PROJECTS_MUTATE, update.payload(&uuid(), id))).collect();
        cx.spawn(async move |this, cx| {
            for request in requests {
                if let Err(error) = request.await {
                    let title = if many { format!("{failure_title} on {label}") } else { failure_title.to_owned() };
                    cx.update(|cx| store::show_error(format!("{title}: {error}"), cx));
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.projects.organizing = false;
                this.projects.saving_icon = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn organize(&mut self, update: ProjectUpdate, failure_title: &'static str, cx: &mut Context<Self>) {
        let members = self.shown_members(cx);
        self.projects.organizing = true;
        cx.notify();
        self.update_project_group(&members, update, failure_title, cx);
    }

    fn reset_project_icon(&mut self, cx: &mut Context<Self>) {
        if self.projects.saving_icon {
            return;
        }
        let scratch_root = self.scratch_root(cx);
        let updates = self
            .shown_members(cx)
            .into_iter()
            .map(|member| {
                let update = ProjectUpdate::default_icon(model::is_scratch(&member.workspace_root, scratch_root.as_deref()));
                (member.id, update)
            })
            .collect();
        self.projects.saving_icon = true;
        cx.notify();
        self.update_project_members(updates, "Failed to update project icon", cx);
    }

    fn scratch_root(&self, cx: &App) -> Option<String> {
        self.thread_settings(cx).and_then(|config| config.scratch_workspace_root.clone())
    }

    /// `removeMembers`: confirm with `planProjectRemoval`'s message, then delete each member in
    /// order. Removing the whole project returns to the list.
    fn remove_members(&mut self, member_ids: Vec<String>, cx: &mut Context<Self>) {
        let failure = "Failed to remove project";
        if self.project_access_denied(failure, cx) {
            return;
        }
        let Some(shown) = self.shown_project(cx) else { return };
        let members: Vec<ProjectShell> = shown.members.iter().filter(|member| member_ids.contains(&member.id)).cloned().collect();
        if members.is_empty() {
            return;
        }
        let label = self.environment_label(cx);
        let state = self.state.read(cx);
        let ids: std::collections::HashSet<String> = member_ids.iter().cloned().collect();
        let loaded: Vec<&t3_core::shell::ThreadShell> =
            state.shell.snapshot.as_ref().map(|snapshot| snapshot.threads.iter().filter(|thread| ids.contains(&thread.project_id)).collect()).unwrap_or_default();
        let unloaded = state.omitted.older().map_or(0, |older| t3_core::sidebar::older_settled::unloaded_count(&older, Some(&ids), &loaded));
        let removal: Vec<RemovalMember> =
            members.iter().map(|member| RemovalMember { title: &member.title, workspace_root: &member.workspace_root, environment_label: Some(&label) }).collect();
        let plan = plan_removal(&removal, shown.members.len(), false, shown.display_name(), loaded.len() + unloaded);
        let (title, description) = model::confirm_copy(&plan.confirmation);
        let back_to_list = plan.is_whole_group;
        crate::dialog::ask(title, description.unwrap_or_default(), "Confirm", move |state, cx| state.remove_projects(member_ids, back_to_list, cx), cx);
    }

    pub(super) fn projects_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(&[], muted_text("Reconnect to change this environment's settings.", cx))];
        };
        if let Some(project_id) = self.projects.pending_project.clone() {
            self.select_project_id(&project_id, cx);
        }
        let groups = self.project_groups(cx);
        let picker = self.project_picker(&groups, cx);
        if self.projects.selected.is_none() {
            return vec![Block::new(&[], picker), Block::new(&[], self.project_list(&groups, cx))];
        }
        let Some(shown) = self.shown_project(cx) else {
            let message = if groups.is_empty() { NO_PROJECTS } else { "This project is no longer available." };
            return vec![Block::new(&[], picker), Block::new(&[], muted_text(message, cx))];
        };
        self.projects.last_members = shown.group.member_project_ids.clone();
        self.open_pending_action_editor(window, cx);
        self.ensure_project_file(&shown.members[0].workspace_root, cx);
        Self::sync_input(&self.projects.name, shown.display_name(), window, cx);
        let mut blocks = vec![
            Block::new(&[], picker),
            Block::new(&["project-overview"], self.project_overview(&shown, &config, cx)),
            Block::new(&["project-defaults"], self.project_new_threads(&shown, &config, cx)),
            Block::new(&["project-actions"], self.project_actions_section(&shown, &config, window, cx)),
        ];
        if shown.members.len() > 1 {
            blocks.push(Block::new(&[], self.project_checkouts(&shown, cx)));
        }
        blocks.push(Block::new(&[], self.project_danger(&shown, cx)));
        blocks
    }

    /// Reads the shown project's `t3.json` once per checkout (`useT3ProjectFileState`).
    fn ensure_project_file(&mut self, cwd: &str, cx: &mut Context<Self>) {
        if self.projects.project_file.as_ref().is_some_and(|(loaded, _)| loaded == cwd) {
            return;
        }
        self.projects.project_file = Some((cwd.to_owned(), None));
        let request = self.state.read(cx).rpc(method::PROJECTS_READ_FILE, workspace::project_file_request(cwd));
        let cwd = cwd.to_owned();
        cx.spawn(async move |this, cx| {
            let file = match request.await {
                Ok(result) => project_file_from_read(&result),
                Err(_) => ProjectFile::Missing,
            };
            let _ = this.update(cx, |this, cx| {
                if this.projects.project_file.as_ref().is_some_and(|(loaded, _)| *loaded == cwd) {
                    this.projects.project_file = Some((cwd, Some(file)));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn project_file(&self) -> Option<&ProjectFile> {
        self.projects.project_file.as_ref().and_then(|(_, file)| file.as_ref())
    }

    /// "Applying settings for [project]": the project axis of `SettingsScopeSentence`.
    fn project_picker(&self, groups: &[ProjectGroup], cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        let label = self.environment_label(cx);
        let root = |group: &ProjectGroup| state.shell.project(&group.representative_id).map(|project| project.workspace_root.clone()).unwrap_or_default();
        let options = std::iter::once(SelectOption::new(ALL_PROJECTS, "All projects"))
            .chain(groups.iter().map(|group| {
                let secondary = choice_secondary(group, groups, &root(group), &label);
                SelectOption::new(group.key.clone(), group.label.clone()).description(secondary)
            }))
            .collect();
        let selected = self.projects.selected.clone().unwrap_or_default();
        let trigger_label = match &self.projects.selected {
            None => "All projects".to_owned(),
            Some(key) => resolve_selection(groups, key, &self.projects.last_members).map_or_else(|| "Unavailable project".to_owned(), |group| group.label.clone()),
        };
        let view = cx.entity().downgrade();
        h_flex()
            .px(r(16.))
            .gap(r(6.))
            .text_sm()
            .text_color(hsla(p.muted_foreground))
            .child("Applying settings for")
            .child(
                Select::new("project-scope", options).selected(selected).label(trigger_label).popup_width(288.).show_check().on_change(move |value, _, cx| {
                    let key = (!value.is_empty()).then(|| value.to_string());
                    let _ = view.update(cx, |this, cx| this.select_project(key, cx));
                }),
            )
            .into_any_element()
    }

    /// `SettingsScopeNotice` for a project: the status line and one row per project.
    fn project_list(&self, groups: &[ProjectGroup], cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let label = self.environment_label(cx);
        let rows: Vec<AnyElement> = groups
            .iter()
            .map(|group| {
                let state = self.state.read(cx);
                let root = state.shell.project(&group.representative_id).map(|project| project.workspace_root.clone()).unwrap_or_default();
                let secondary = choice_secondary(group, groups, &root, &label);
                let key = group.key.clone();
                h_flex()
                    .id(SharedString::from(format!("project-choice-{}", group.key)))
                    .w_full()
                    .px(r(16.))
                    .py(r(12.))
                    .gap(r(12.))
                    .rounded(r(12.))
                    .cursor_pointer()
                    .hover(|this| this.bg(hsla(p.accent).opacity(0.5)))
                    .child(self.project_favicon(&group.representative_id, 16., cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(group.label.clone()))
                            .child(div().id(SharedString::from(format!("project-choice-path-{}", group.key))).truncate().text_xs().text_color(hsla(p.muted_foreground)).child(secondary.clone()).tooltip(tooltip(secondary.into()))),
                    )
                    .child(icon(Icon::ChevronRight, 16., hsla(p.muted_foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_project(Some(key.clone()), cx)))
                    .into_any_element()
            })
            .collect();
        let status = div().px(r(16.)).text_sm().text_color(hsla(p.muted_foreground)).child(if groups.is_empty() { NO_PROJECTS } else { NO_PROJECT_NOTICE });
        v_flex().gap(r(12.)).child(status).when(!rows.is_empty(), |this| this.child(group(rows, cx))).into_any_element()
    }

    /// `ProjectFavicon`: the project's image once fetched, else the folder glyph.
    pub(super) fn project_favicon(&self, project_id: &str, size: f32, cx: &App) -> AnyElement {
        match self.state.read(cx).sidebar.favicons.image(project_id) {
            Some(image) => img(image).flex_none().size(r(size)).into_any_element(),
            None => icon(Icon::Folder, size, hsla(palette(cx).muted_foreground)).into_any_element(),
        }
    }

    /// The untitled first section: Name, Project icon, Pin and Archive.
    fn project_overview(&self, shown: &Shown, config: &ServerConfig, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let can_edit = self.can_edit_projects(cx);
        let scratch_root = config.scratch_workspace_root.as_deref();
        let mut rows = Vec::new();
        if !can_edit {
            rows.push(div().px(r(16.)).py(r(8.)).text_sm().text_color(hsla(p.muted_foreground)).child(edit_denied_notice(shown.members.len())).into_any_element());
        }
        let titles: Vec<(bool, &str)> = shown.members.iter().map(|member| (model::is_scratch(&member.workspace_root, scratch_root), member.title.as_str())).collect();
        let view = cx.entity().downgrade();
        let members = shown.members.clone();
        rows.push(
            Row::new("project-name", "Name")
                .description("The shared name for this project group in the sidebar and thread lists.")
                .reset(model::can_reset_scratch_title(&titles) && can_edit, move |_, _, cx| {
                    let members = members.clone();
                    let _ = view.update(cx, |this, cx| {
                        let update = ProjectUpdate { title: Some(model::SCRATCH_PROJECT_TITLE.to_owned()), ..Default::default() };
                        this.update_project_group(&members, update, "Failed to rename project", cx);
                    });
                })
                .control(div().w(r(256.)).child(Input::new(&self.projects.name).small().disabled(!can_edit)))
                .into_any_element(),
        );

        let representative = &shown.members[0];
        let project_icon = ProjectIcon::decode(representative.project_icon.as_ref());
        let customized = shown.members.iter().any(|member| {
            !model::has_default_icon(
                model::is_scratch(&member.workspace_root, scratch_root),
                member.favicon_path.as_deref(),
                ProjectIcon::decode(member.project_icon.as_ref()).as_ref(),
            )
        });
        let saving_icon = self.projects.saving_icon;
        rows.push(
            Row::new("project-icon", "Project icon")
                .description(model::icon_description(project_icon.as_ref(), representative.favicon_path.as_deref()))
                .reset(customized && can_edit && !saving_icon, cx.listener(|this, _, _, cx| this.reset_project_icon(cx)))
                .control(div().size(r(24.)).flex().items_center().justify_center().child(self.project_favicon(&representative.id, 24., cx)))
                .into_any_element(),
        );

        let organize = can_edit && self.state.read(cx).can_pin_projects();
        if organize {
            let organizing = self.projects.organizing;
            let view = cx.entity().downgrade();
            rows.push(
                Row::new("project-pin", "Pin")
                    .description("Pinned projects come first in project pickers.")
                    .control(Switch::new("project-pin-switch", shown.group.pinned).disabled(organizing).on_change(move |pinned, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            let title = if pinned { "Failed to pin project" } else { "Failed to unpin project" };
                            this.organize(ProjectUpdate { pinned: Some(pinned), ..Default::default() }, title, cx);
                        });
                    }))
                    .into_any_element(),
            );
            let archived = shown.group.archived;
            rows.push(
                Row::new("project-archive", "Archive")
                    .description(archive_description(archived))
                    .control(
                        Button::new("project-archive-button", ButtonVariant::Outline, ButtonSize::Sm)
                            .label(if archived { "Unarchive" } else { "Archive" })
                            .disabled(organizing)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let title = if archived { "Failed to unarchive project" } else { "Failed to archive project" };
                                this.organize(ProjectUpdate { archived: Some(!archived), ..Default::default() }, title, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        let highlighted = self.is_target("project-overview");
        div().rounded(r(12.)).when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.3))).child(group(rows, cx)).into_any_element()
    }

    /// Saves one project-scoped setting on every member's override.
    fn save_project_override(&self, members: &[String], key: &'static str, value: Value, cx: &mut App) {
        let Some(config) = self.thread_settings(cx) else { return };
        let ids: Vec<&str> = members.iter().map(String::as_str).collect();
        let patch = model::override_set_patch(&config.settings, &ids, key, value);
        store::update_server(&self.state, ServerSettingChange::Prepared(patch), cx);
    }

    fn clear_project_override(&self, members: &[String], key: &'static str, cx: &mut App) {
        let Some(config) = self.thread_settings(cx) else { return };
        let ids: Vec<&str> = members.iter().map(String::as_str).collect();
        let patch = model::override_clear_patch(&config.settings, &ids, &[key]);
        store::update_server(&self.state, ServerSettingChange::Prepared(patch), cx);
    }

    /// A control this connection cannot use: dimmed, inert, and saying why (`SettingsRow`).
    pub(super) fn inert_unless(&self, allowed: bool, id: &'static str, control: impl IntoElement, cx: &App) -> AnyElement {
        if allowed {
            return control.into_any_element();
        }
        let message: SharedString = format!("This connection lacks permission to change settings on {}.", self.environment_label(cx)).into();
        div()
            .id(id)
            .relative()
            .child(div().opacity(0.5).child(control))
            .child(div().absolute().inset_0().occlude())
            .tooltip(tooltip(message))
            .into_any_element()
    }

    /// `ProjectDefaultsSettings` with the "project" category: Model and Workspace overrides.
    fn project_new_threads(&self, shown: &Shown, config: &ServerConfig, cx: &mut Context<Self>) -> AnyElement {
        let ids: Vec<String> = shown.members.iter().map(|member| member.id.clone()).collect();
        let first = ids[0].clone();
        let can_write = self.may(SETTINGS_WRITE_SCOPE, cx);
        let models: Vec<_> = ids.iter().map(|id| project_model(config, id)).collect();
        let model = &models[0];
        let mixed_model = models.iter().any(|other| other.shown != model.shown);
        let options = model_options(config, Some(&first));
        let model_reset = can_write && models.iter().any(|model| model.source == Source::Project);
        let model_row = Row::new("project-default-model", "Model")
            .description("Model for new threads in this project.")
            .reset_tooltip("Reset to inherited value")
            .reset(model_reset, {
                let (view, ids) = (cx.entity().downgrade(), ids.clone());
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.clear_project_override(&ids, "defaultModelSelection", cx));
                }
            });
        let model_row = match &model.shown {
            Some(selection) if !options.is_empty() => {
                let (view, ids) = (cx.entity().downgrade(), ids.clone());
                let label = if mixed_model { if model.automatic { "Automatic (mixed)".to_owned() } else { "Mixed".to_owned() } } else { selection_label(config, selection) };
                let select = Select::new("project-default-model-select", options).selected(selection_value(selection)).label(label).width(220.).popup_width(260.).show_check().on_change(
                    move |value, _, cx| {
                        if let Some(selection) = parse_selection(&value) {
                            let _ = view.update(cx, |this, cx| this.save_project_override(&ids, "defaultModelSelection", json!(selection), cx));
                        }
                    },
                );
                model_row.control(self.inert_unless(can_write, "project-default-model-inert", select, cx))
            }
            _ => model_row.control(muted_text(if mixed_model { "Mixed" } else { "No providers available" }, cx)),
        };

        let file_mode = match self.project_file() {
            Some(ProjectFile::Valid { default_thread_env_mode, .. }) => *default_thread_env_mode,
            _ => None,
        };
        let modes: Vec<(ThreadEnvMode, Source)> = ids.iter().map(|id| project_env_mode(&config.settings, id, file_mode)).collect();
        let mixed_mode = modes.iter().any(|(mode, _)| *mode != modes[0].0);
        let mode_reset = can_write && modes.iter().any(|(_, source)| *source == Source::Project);
        let (view, mode_ids) = (cx.entity().downgrade(), ids.clone());
        let mut workspace_select = Select::new("project-default-workspace-select", ENV_MODES.iter().map(|(mode, label)| SelectOption::new(env_mode_value(*mode), *label)).collect())
            .show_check()
            .on_change(move |value, _, cx| {
                let mode = match value.as_ref() {
                    "local" | "worktree" => value.to_string(),
                    _ => return,
                };
                let _ = view.update(cx, |this, cx| this.save_project_override(&mode_ids, "defaultThreadEnvMode", json!(mode), cx));
            });
        workspace_select = if mixed_mode { workspace_select.label("Mixed") } else { workspace_select.selected(env_mode_value(modes[0].0)) };
        let workspace_row = Row::new("project-default-workspace", "Workspace")
            .description("Where new threads in this project start.")
            .reset_tooltip("Reset to inherited value")
            .reset(mode_reset, {
                let (view, ids) = (cx.entity().downgrade(), ids.clone());
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.clear_project_override(&ids, "defaultThreadEnvMode", cx));
                }
            })
            .control(self.inert_unless(can_write, "project-default-workspace-inert", workspace_select, cx));
        section("New threads", vec![model_row.into_any_element(), workspace_row.into_any_element()], cx).into_any_element()
    }

    /// The Checkouts section: one row per member with its own Remove.
    fn project_checkouts(&self, shown: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let can_edit = self.can_edit_projects(cx);
        let label = self.environment_label(cx);
        let rows = shown
            .members
            .iter()
            .map(|member| {
                let id = member.id.clone();
                Row::new(SharedString::from(format!("project-checkout-{}", member.id)), label.clone())
                    .description(member.workspace_root.clone())
                    .control(
                        Button::new(SharedString::from(format!("project-checkout-remove-{}", member.id)), ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Remove")
                            .tooltip(format!("Remove checkout {}", member.workspace_root))
                            .disabled(!can_edit)
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_members(vec![id.clone()], cx))),
                    )
                    .into_any_element()
            })
            .collect();
        section("Checkouts", rows, cx).into_any_element()
    }

    fn project_danger(&self, shown: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let copy = danger_copy(false, shown.members.len());
        let ids: Vec<String> = shown.member_ids().into_iter().map(str::to_owned).collect();
        let row = Row::new("project-remove", copy.title).description(copy.description).control(
            Button::new("project-remove-button", ButtonVariant::DestructiveOutline, ButtonSize::Sm)
                .icon(Icon::Trash2)
                .label(copy.button)
                .disabled(!self.can_edit_projects(cx))
                .on_click(cx.listener(move |this, _, _, cx| this.remove_members(ids.clone(), cx))),
        );
        section("Danger", vec![row.into_any_element()], cx).into_any_element()
    }
}
