//! Settings → Projects: `ProjectsSettings` and `ProjectSettingsPanel` in
//! `web/src/components/settings/`. Without a chosen project the page lists them
//! (`SettingsScopeNotice`); a chosen one shows its name, icon, pin and archive, its New threads
//! overrides (`ProjectDefaultsSettings` at project scope), its actions (`project_actions.rs`), its
//! checkouts and removal. The project picker follows the watched environments' scoped groups.
//! Not ported: the image picker ("Choose file"), and the info alert about
//! keeping the project picked on other pages, which other native pages do not scope by project.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, list, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::commands::method;
use t3_core::client_settings::{ClientSettingChange, ProjectGroupingMode, PROJECT_GROUPING_MODE_LABELS, project_grouping_label};
use t3_core::projects::PROJECTS_MUTATE;
use t3_core::server_config::{ServerConfig, ThreadEnvMode};
use t3_core::settings::project_actions::{ProjectFile, project_file_from_read};
use t3_core::settings::projects::{
    self as model, NO_PROJECT_NOTICE, NO_PROJECTS, OverrideEdit, ProjectIcon, ProjectUpdate, RemovalMember, Rename, archive_description,
    danger_copy, edit_denied_notice, model_unavailable_reason, plan_removal, project_env_mode, project_model,
};
use t3_core::settings::ENV_MODES;
use t3_core::shell::ProjectShell;
use t3_core::sidebar::grouping::{GroupConsumer, ProjectRef, ScopedProject, ScopedProjectGroup};
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
    last_members: Vec<ProjectRef>,
    /// A project an entry point asked for before the shell listed it.
    pending_project: Option<String>,
    /// The project list, virtualized: a header entry, then one per project.
    list: ListState,
    choices: Rc<Vec<Choice>>,
    pub(super) name: Entity<InputState>,
    /// The Name field was typed in since it last committed.
    name_edited: bool,
    organizing: bool,
    pub(super) saving_icon: bool,
    pub(super) picker: Option<super::project_icon_picker::Picker>,
    /// `t3.json` of the shown project's first checkout: its folder and the read's result, `None`
    /// while it loads.
    pub(super) project_file: Option<(ScopedProject, Option<ProjectFile>)>,
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
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            choices: Rc::default(),
            name,
            name_edited: false,
            organizing: false,
            saving_icon: false,
            picker: None,
            project_file: None,
            actions: ActionsState::new(window, cx),
            _subscriptions: subscriptions,
        }
    }
}

/// A project-list row.
pub(super) struct Choice {
    key: String,
    label: SharedString,
    secondary: SharedString,
    representative: ProjectRef,
}

/// One project group with its members, as the page reads it.
pub(super) struct Shown {
    pub(super) group: ScopedProjectGroup,
    pub(super) members: Vec<ProjectShell>,
    pub(super) targets: Vec<ScopedProject>,
}

impl Shown {
    pub(super) fn member_ids(&self) -> Vec<&str> {
        self.members.iter().map(|member| member.id.as_str()).collect()
    }

    fn display_name(&self) -> &str {
        &self.group.label
    }
}

#[derive(Clone)]
pub(super) struct IconTarget {
    pub(super) members: Vec<ScopedProject>,
}

impl IconTarget {
    pub(super) fn allows(&self, current: &Self, permitted: bool, saving: bool) -> bool {
        permitted && !saving && self.members.len() == current.members.len()
            && self.members.iter().zip(&current.members).all(|(captured, current)| captured.owner == current.owner
                && captured.project.id == current.project.id && captured.project.created_at == current.project.created_at
                && captured.project.workspace_root == current.project.workspace_root)
    }
}

impl SettingsView {
    pub(super) fn project_groups(&self, cx: &App) -> Vec<ScopedProjectGroup> {
        self.state.read(cx).project_groups_for(GroupConsumer::Settings, cx)
    }

    pub(super) fn shown_project(&self, cx: &App) -> Option<Shown> {
        let key = self.projects.selected.as_deref()?;
        let groups = self.project_groups(cx);
        let group = groups.iter().find(|group| group.key == key)
            .or_else(|| groups.iter().find(|group| group.member_refs.iter().any(|reference| self.projects.last_members.contains(reference))))?.clone();
        let state = self.state.read(cx);
        let mut targets = group.mutation_refs().iter().map(|reference| state.scoped_project(reference).cloned()).collect::<Option<Vec<_>>>()?;
        targets.sort_by_key(|project| project.reference() != group.representative);
        let members = targets.iter().map(|project| project.project.clone()).collect();
        (!targets.is_empty()).then_some(Shown { group, members, targets })
    }

    pub(super) fn project_targets(&self, cx: &App) -> Vec<ScopedProject> {
        self.shown_project(cx).map(|shown| shown.targets).unwrap_or_default()
    }

    pub(super) fn projects_permitted(&self, scope: &str, cx: &App) -> bool {
        let targets = self.project_targets(cx);
        !targets.is_empty() && targets.iter().all(|project| store::project_current(self.state.read(cx), project, scope))
    }

    pub(super) fn project_config<'a>(&self, project: &ScopedProject, cx: &'a App) -> Option<&'a ServerConfig> {
        self.state.read(cx).scoped_environment(&project.owner.saved_id)?.config.config.as_ref()
    }

    pub(super) fn select_project_id(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let active = self.state.read(cx).environments.active.as_deref();
        let key = self.project_groups(cx).into_iter().find(|group| group.member_refs.iter().any(|reference| Some(reference.environment_id.as_str()) == active && reference.project_id == project_id)).map(|group| group.key);
        self.projects.pending_project = key.is_none().then(|| project_id.to_owned());
        if let Some(key) = key { self.select_project(Some(key), cx); }
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
            Rename::Save(title) => self.update_project_group(&shown.targets, ProjectUpdate { title: Some(title), ..Default::default() }, "Failed to rename project", cx),
        }
    }

    pub(super) fn commit_focused_project_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.name.focus_handle(cx).is_focused(window) {
            self.commit_project_name(cx);
        }
    }

    pub(super) fn can_edit_projects(&self, cx: &App) -> bool {
        self.projects_permitted(ORCHESTRATION_OPERATE_SCOPE, cx)
    }

    pub(super) fn environment_label(&self, cx: &App) -> String {
        let targets = self.project_targets(cx);
        if targets.is_empty() { return "this environment".to_owned(); }
        let mut labels = targets.into_iter().map(|project| project.environment_label).collect::<Vec<_>>();
        labels.sort();
        labels.dedup();
        labels.join(", ")
    }

    fn update_project_group(&mut self, members: &[ScopedProject], update: ProjectUpdate, failure_title: &'static str, cx: &mut Context<Self>) {
        let updates = members.iter().cloned().map(|project| (project, update.clone())).collect();
        self.update_project_members(updates, failure_title, cx);
    }

    fn update_project_members(&mut self, updates: Vec<(ScopedProject, ProjectUpdate)>, failure_title: &'static str, cx: &mut Context<Self>) {
        if updates.is_empty() || updates.iter().any(|(project, _)| !store::project_current(self.state.read(cx), project, ORCHESTRATION_OPERATE_SCOPE)) {
            self.projects.organizing = false;
            return store::show_error(format!("{failure_title}: a checkout changed or is not permitted."), cx);
        }
        let state = self.state.clone();
        let lookups = updates.iter().map(|(project, _)| state.read(cx).connections.destination(&project.owner, ORCHESTRATION_OPERATE_SCOPE)).collect::<Vec<_>>();
        cx.spawn(async move |this, cx| {
            let mut destinations = Vec::new();
            let mut failure = None;
            for lookup in lookups {
                match lookup.await { Ok(destination) => destinations.push(destination), Err(error) => { failure = Some(format!("{error:?}")); break; } }
            }
            if failure.is_none() && !state.read_with(cx, |app, _| updates.iter().zip(&destinations).all(|((project, _), destination)|
                store::project_current(app, project, ORCHESTRATION_OPERATE_SCOPE) && destination.is_current(&app.fleet)
                    && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(ORCHESTRATION_OPERATE_SCOPE)))) {
                failure = Some("A checkout or permission changed; nothing was sent.".to_owned());
            }
            if failure.is_none() {
                for (changed, ((project, update), destination)) in updates.into_iter().zip(destinations).enumerate() {
                    let valid = state.read_with(cx, |state, _| store::project_current(state, &project, ORCHESTRATION_OPERATE_SCOPE) && destination.is_current(&state.fleet))
                        && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(ORCHESTRATION_OPERATE_SCOPE));
                    if !valid { failure = Some(format!("{changed} checkout(s) changed; destination unavailable.")); break; }
                    let runtime = state.read_with(cx, |state, _| state.runtime().clone());
                    let payload = update.payload(&uuid(), &project.project.id);
                    let reply = runtime.spawn(async move { destination.client.request(PROJECTS_MUTATE, payload).await }).await;
                    match reply { Ok(Ok(_)) => {}, Ok(Err(error)) => { failure = Some(format!("{changed} checkout(s) changed; {}: {error}", project.environment_label)); break; }, Err(error) => { failure = Some(error.to_string()); break; } }
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.projects.organizing = false;
                this.projects.saving_icon = false;
                if let Some(error) = failure { store::show_error(format!("{failure_title}: {error}"), cx); }
                cx.notify();
            });
        }).detach();
    }

    fn organize(&mut self, update: ProjectUpdate, failure_title: &'static str, cx: &mut Context<Self>) {
        let members = self.project_targets(cx);
        self.projects.organizing = true;
        cx.notify();
        self.update_project_group(&members, update, failure_title, cx);
    }

    fn reset_project_icon(&mut self, cx: &mut Context<Self>) {
        if self.projects.saving_icon { return; }
        let target = self.icon_target(cx);
        let updates = target.members.iter().map(|member| (member.clone(), ProjectUpdate::default_icon(member.is_scratch))).collect();
        self.save_icon_updates(target, updates, cx);
    }

    pub(super) fn icon_target(&self, cx: &App) -> IconTarget {
        IconTarget { members: self.project_targets(cx) }
    }

    pub(super) fn save_icon_updates(&mut self, target: IconTarget, updates: Vec<(ScopedProject, ProjectUpdate)>, cx: &mut Context<Self>) {
        if !target.allows(&self.icon_target(cx), self.can_edit_projects(cx), self.projects.saving_icon) { return; }
        #[cfg(feature = "snapshot")]
        if let Some(path) = std::env::var_os("T3_NATIVE_ICON_COMMAND_SPY") {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).expect("icon command spy");
            for (project, update) in updates { writeln!(file, "{}", update.payload("snapshot-icon", &project.project.id)).expect("write icon command spy"); }
            return;
        }
        self.projects.saving_icon = true;
        cx.notify();
        self.update_project_members(updates, "Failed to update project icon", cx);
    }

    /// `removeMembers`: confirm with `planProjectRemoval`'s message, then delete each member in
    /// order. Removing the whole project returns to the list.
    fn remove_members(&mut self, member_refs: Vec<ProjectRef>, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_project(cx) else { return };
        let members = shown.targets.iter().filter(|member| member_refs.contains(&member.reference())).cloned().collect::<Vec<_>>();
        if members.is_empty() { return; }
        if members.iter().any(|project| !store::project_current(self.state.read(cx), project, ORCHESTRATION_OPERATE_SCOPE)) {
            return store::show_error("Failed to remove project: a checkout changed or is not permitted.", cx);
        }
        let state = self.state.read(cx);
        let mut thread_count = 0;
        for project in &members {
            if let Some(environment) = state.scoped_environment(&project.owner.saved_id) {
                thread_count += environment.summary.threads.iter().filter(|thread| thread.thread.project_id == project.project.id).count();
                thread_count += environment.summary.older_settled.as_ref().and_then(|cut| cut.counts.iter().find(|count| count.project_id == project.project.id)).map_or(0, |count| count.count as usize);
            }
        }
        let removal = members.iter().map(|member| RemovalMember { title: &member.project.title, workspace_root: &member.project.workspace_root, environment_label: Some(&member.environment_label) }).collect::<Vec<_>>();
        let plan = plan_removal(&removal, shown.targets.len(), false, shown.display_name(), thread_count);
        let (title, description) = model::confirm_copy(&plan.confirmation);
        let back_to_list = plan.is_whole_group;
        crate::dialog::ask(title, description.unwrap_or_default(), "Confirm", move |state, cx| state.remove_scoped_projects(members, back_to_list, cx), cx);
    }

    pub(super) fn projects_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        if let Some(project_id) = self.projects.pending_project.clone() {
            self.select_project_id(&project_id, cx);
        }
        if self.projects.selected.is_none() {
            return Vec::new();
        }
        let groups = self.project_groups(cx);
        let picker = self.project_picker(&groups, cx);
        let Some(shown) = self.shown_project(cx) else {
            let message = if groups.is_empty() { NO_PROJECTS } else { "This project is no longer available." };
            return vec![Block::new(&[], picker), Block::new(&[], muted_text(message, cx))];
        };
        let Some(config) = self.project_config(&shown.targets[0], cx).cloned() else {
            return vec![Block::new(&[], picker), Block::new(&[], muted_text("Reconnect to change this project's settings.", cx))];
        };
        self.projects.last_members = shown.group.member_refs.clone();
        self.open_pending_action_editor(window, cx);
        self.ensure_project_file(&shown.targets[0], cx);
        Self::sync_input(&self.projects.name, shown.display_name(), window, cx);
        let mut blocks = vec![
            Block::new(&[], picker),
            Block::new(&["project-overview"], self.project_overview(&shown, &config, cx)),
            Block::new(&["project-defaults"], self.project_new_threads(&shown, &config, cx)),
            Block::new(&["project-actions"], self.project_actions_section(&shown, &config, window, cx)),
        ];
        blocks.push(Block::new(&[], self.project_checkouts(&shown, cx)));
        blocks.push(Block::new(&[], self.project_danger(&shown, cx)));
        blocks
    }

    /// Reads the shown project's `t3.json` once per checkout (`useT3ProjectFileState`).
    fn ensure_project_file(&mut self, project: &ScopedProject, cx: &mut Context<Self>) {
        if !store::project_current(self.state.read(cx), project, "filesystem:read") {
            self.projects.project_file = None;
            return;
        }
        if self.projects.project_file.as_ref().is_some_and(|(loaded, _)| loaded.owner == project.owner
            && loaded.project.id == project.project.id && loaded.project.created_at == project.project.created_at
            && loaded.project.workspace_root == project.project.workspace_root) { return; }
        self.projects.project_file = Some((project.clone(), None));
        let project = project.clone();
        let lookup = self.state.read(cx).connections.destination(&project.owner, "filesystem:read");
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            let file = match lookup.await {
                Ok(destination) => {
                    let valid = state.read_with(cx, |state, _| store::project_current(state, &project, "filesystem:read") && destination.is_current(&state.fleet));
                    if !valid {
                        let _ = this.update(cx, |this, _| {
                            if this.projects.project_file.as_ref().is_some_and(|(loaded, _)| loaded == &project) { this.projects.project_file = None; }
                        });
                        return;
                    }
                    let runtime = state.read_with(cx, |state, _| state.runtime().clone());
                    let request = workspace::project_file_request(&project.project.workspace_root);
                    match runtime.spawn(async move { destination.client.request(method::PROJECTS_READ_FILE, request).await }).await {
                        Ok(Ok(reply)) => project_file_from_read(&reply),
                        _ => ProjectFile::Missing,
                    }
                }
                Err(_) => ProjectFile::Missing,
            };
            let _ = this.update(cx, |this, cx| {
                if this.projects.project_file.as_ref().is_some_and(|(loaded, _)| loaded.owner == project.owner && loaded.project.id == project.project.id && loaded.project.created_at == project.project.created_at && loaded.project.workspace_root == project.project.workspace_root)
                    && store::project_current(this.state.read(cx), &project, "filesystem:read") {
                    this.projects.project_file = Some((project, Some(file)));
                    cx.notify();
                } else if this.projects.project_file.as_ref().is_some_and(|(loaded, _)| loaded == &project) {
                    this.projects.project_file = None;
                }
            });
        }).detach();
    }

    pub(super) fn project_file(&self) -> Option<&ProjectFile> {
        self.projects.project_file.as_ref().and_then(|(_, file)| file.as_ref())
    }

    /// "Applying settings for [project]": the project axis of `SettingsScopeSentence`.
    fn project_picker(&self, groups: &[ScopedProjectGroup], cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        let mut seen = std::collections::HashSet::new();
        let shared = groups.iter().filter(|group| !seen.insert(group.label.as_str())).map(|group| group.label.as_str()).collect::<std::collections::HashSet<_>>();
        let options = std::iter::once(SelectOption::new(ALL_PROJECTS, "All projects"))
            .chain(groups.iter().map(|group| {
                let project = state.scoped_project(&group.representative);
                let secondary = [group.archived.then_some("Archived"), shared.contains(group.label.as_str()).then(|| project.map(|project| project.environment_label.as_str()).unwrap_or("")), project.map(|project| project.project.workspace_root.as_str())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                SelectOption::new(group.key.clone(), group.label.clone()).description(secondary)
            }))
            .collect();
        let icon_state = self.state.clone();
        let icon_projects: std::collections::HashMap<_, _> = groups.iter().map(|group| (group.key.clone(), group.representative.clone())).collect();
        let selected = self.projects.selected.clone().unwrap_or_default();
        let trigger_label = match &self.projects.selected {
            None => "All projects".to_owned(),
            Some(key) => groups.iter().find(|group| group.key == *key || group.member_refs.iter().any(|reference| self.projects.last_members.contains(reference))).map_or_else(|| "Unavailable project".to_owned(), |group| group.label.clone()),
        };
        let view = cx.entity().downgrade();
        h_flex()
            .px(r(16.))
            .gap(r(6.))
            .text_sm()
            .text_color(hsla(p.muted_foreground))
            .child("Applying settings for")
            .child(
                Select::new("project-scope", options).selected(selected).label(trigger_label).popup_width(288.).show_check()
                    .render_icon(move |key, cx| {
                        let id = icon_projects.get(key)?;
                        Some(crate::project_icon::ProjectVisual::for_scoped_project(icon_state.read(cx), id).render(crate::project_icon::size(14., cx), hsla(palette(cx).icon_muted), cx))
                    })
                    .on_change(move |value, _, cx| {
                        let key = (!value.is_empty()).then(|| value.to_string());
                        let _ = view.update(cx, |this, cx| this.select_project(key, cx));
                    }),
            )
            .into_any_element()
    }

    /// `SettingsScopeNotice` for a project, while none is chosen: the picker and status line,
    /// then one row per project, as a virtualized list over the page.
    pub(super) fn project_list_view(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.projects.selected.is_some() {
            return None;
        }
        let groups = self.project_groups(cx);
        let mut seen = std::collections::HashSet::new();
        let shared = groups.iter().filter(|group| !seen.insert(group.label.as_str())).map(|group| group.label.as_str()).collect::<std::collections::HashSet<_>>();
        let state = self.state.read(cx);
        let choices: Vec<Choice> = groups
            .iter()
            .map(|group| {
                let project = state.scoped_project(&group.representative);
                let secondary = [group.archived.then_some("Archived"), shared.contains(group.label.as_str()).then(|| project.map(|project| project.environment_label.as_str()).unwrap_or("")), project.map(|project| project.project.workspace_root.as_str())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                Choice {
                    key: group.key.clone(),
                    label: group.label.clone().into(),
                    secondary: secondary.into(),
                    representative: group.representative.clone(),
                }
            })
            .collect();
        if self.projects.list.item_count() != choices.len() + 1 {
            self.projects.list.reset(choices.len() + 1);
        }
        self.projects.choices = Rc::new(choices);
        Some(list(self.projects.list.clone(), cx.processor(|this, index: usize, _, cx| this.render_project_choice(index, cx))).w_full().flex_1().into_any_element())
    }

    fn render_project_choice(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let choices = self.projects.choices.clone();
        let content = match index.checked_sub(1).and_then(|index| choices.get(index).map(|choice| (index, choice))) {
            None => {
                let groups = self.project_groups(cx);
                let status = if choices.is_empty() { NO_PROJECTS } else { NO_PROJECT_NOTICE };
                v_flex()
                    .gap(r(24.))
                    .pb(r(12.))
                    .child(self.project_picker(&groups, cx))
                    .child(div().px(r(16.)).text_sm().text_color(hsla(p.muted_foreground)).child(status))
                    .into_any_element()
            }
            Some((position, choice)) => {
                let key = choice.key.clone();
                // One segment of `SettingsGroup`: the card's sides on every row, its ends on the first and last.
                div()
                    .border_l_1()
                    .border_r_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.card).opacity(0.4))
                    .when(position == 0, |this| this.border_t_1().rounded_t(r(12.)))
                    .when(position + 1 == choices.len(), |this| this.border_b_1().rounded_b(r(12.)))
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("project-choice-{}", choice.key)))
                            .w_full()
                            .px(r(16.))
                            .py(r(12.))
                            .gap(r(12.))
                            .when(position > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.5)))
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent).opacity(0.5)))
                            .child(self.scoped_project_favicon(&choice.representative, 16., cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(choice.label.clone()))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("project-choice-path-{}", choice.key)))
                                            .truncate()
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(choice.secondary.clone())
                                            .tooltip(tooltip(choice.secondary.clone())),
                                    ),
                            )
                            .child(icon(Icon::ChevronRight, 16., hsla(p.muted_foreground)))
                            .on_click(cx.listener(move |this, _, _, cx| this.select_project(Some(key.clone()), cx))),
                    )
                    .into_any_element()
            }
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .when(index == 0, |this| this.pt(r(24.)))
            .when(index == choices.len(), |this| this.pb(r(48.)))
            .child(div().w_full().max_w(r(896.)).px(r(24.)).child(content))
            .into_any_element()
    }

    fn scoped_project_favicon(&self, project: &ProjectRef, size: f32, cx: &App) -> AnyElement {
        crate::project_icon::ProjectVisual::for_scoped_project(self.state.read(cx), project).render(crate::project_icon::size(size, cx), hsla(palette(cx).icon_muted), cx)
    }

    /// The untitled first section: Name, Project icon, Pin and Archive.
    fn project_overview(&self, shown: &Shown, _config: &ServerConfig, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let can_edit = self.can_edit_projects(cx);
        let mut rows = Vec::new();
        if !can_edit {
            rows.push(div().px(r(16.)).py(r(8.)).text_sm().text_color(hsla(p.muted_foreground)).child(edit_denied_notice(shown.members.len())).into_any_element());
        }
        let titles: Vec<(bool, &str)> = shown.targets.iter().map(|member| (member.is_scratch, member.project.title.as_str())).collect();
        let view = cx.entity().downgrade();
        let members = shown.targets.clone();
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
        let customized = shown.targets.iter().any(|project| {
            let member = &project.project;
            !model::has_default_icon(
                project.is_scratch,
                member.favicon_path.as_deref(),
                ProjectIcon::decode(member.project_icon.as_ref()).as_ref(),
            )
        });
        let saving_icon = self.projects.saving_icon;
        rows.push(
            Row::new("project-icon", "Project icon")
                .description(model::icon_description(project_icon.as_ref(), representative.favicon_path.as_deref()))
                .reset(
                    customized && can_edit && !saving_icon,
                    cx.listener(|this, _, _, cx| this.reset_project_icon(cx)),
                )
                .control(
                    h_flex().gap(r(8.)).child(self.scoped_project_favicon(&shown.group.representative, 24., cx)).child(
                        Button::new("project-choose-icon", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Choose icon")
                            .disabled(!can_edit || saving_icon)
                            .on_click(cx.listener(|this, _, window, cx| this.open_project_icon_picker(window, cx))),
                    ),
                )
                .into_any_element(),
        );

        let organize = can_edit && shown.targets.iter().all(|project| self.project_config(project, cx).is_some_and(|config| config.environment.capabilities.project_organization == Some(true)));
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

    /// Saves one project-scoped setting on every member's override, built when the queue sends
    /// it so an earlier queued change to the same entries is kept.
    fn save_project_override(&self, key: &'static str, value: Value, cx: &mut App) {
        store::update_projects(&self.state, self.project_targets(cx), OverrideEdit::Set { key: key.to_owned(), value }, |_, _| {}, cx);
    }

    fn project_model_unavailable(&self, selection: &t3_core::model::ModelSelection, cx: &App) -> Option<String> {
        for project in self.project_targets(cx) {
            let Some(config) = self.project_config(&project, cx) else { return Some(format!("{} is unavailable.", project.environment_label)); };
            if let Some(reason) = model_unavailable_reason(config, &[&project.project.id], selection, &project.environment_label) { return Some(reason); }
        }
        None
    }

    fn set_project_model(&self, selection: t3_core::model::ModelSelection, cx: &mut App) {
        if let Some(reason) = self.project_model_unavailable(&selection, cx) { return store::show_error(format!("Default model not saved: {reason}"), cx); }
        self.save_project_override("defaultModelSelection", json!(selection), cx);
    }

    fn clear_project_override(&self, key: &'static str, cx: &mut App) {
        store::update_projects(&self.state, self.project_targets(cx), OverrideEdit::Clear { keys: vec![key.to_owned()] }, |_, _| {}, cx);
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
        let Some(configs) = shown.targets.iter().map(|project| self.project_config(project, cx)).collect::<Option<Vec<_>>>() else {
            return section("New threads", vec![muted_text("Reconnect every checkout to change shared defaults.", cx).into_any_element()], cx).into_any_element();
        };
        let member_ids = shown.member_ids();
        let can_write = self.projects_permitted(SETTINGS_WRITE_SCOPE, cx);
        let models: Vec<_> = shown.targets.iter().zip(&configs).map(|(project, config)| project_model(config, &project.project.id)).collect();
        let model = &models[0];
        let mixed_model = models.iter().any(|other| other.shown != model.shown);
        // Only models every member can run, as `useScopedModelDisabledReason` disables the rest.
        let options: Vec<SelectOption> = model_options(config, Some(member_ids[0]))
            .into_iter()
            .filter(|option| parse_selection(&option.value).is_some_and(|selection| self.project_model_unavailable(&selection, cx).is_none()))
            .collect();
        let model_reset = can_write && models.iter().any(|model| model.source == Source::Project);
        let unavailable = model.shown.as_ref().filter(|_| !mixed_model).and_then(|selection| self.project_model_unavailable(selection, cx));
        let model_row = Row::new("project-default-model", "Model")
            .description("Model for new threads in this project.")
            .when_some(unavailable, |row, reason| row.status(reason))
            .reset_tooltip("Reset to inherited value")
            .reset(model_reset, {
                let view = cx.entity().downgrade();
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.clear_project_override("defaultModelSelection", cx));
                }
            });
        let model_row = match &model.shown {
            Some(selection) if !options.is_empty() => {
                let view = cx.entity().downgrade();
                let trigger = if mixed_model { if model.automatic { "Automatic (mixed)".to_owned() } else { "Mixed".to_owned() } } else { selection_label(config, selection) };
                let select = Select::new("project-default-model-select", options).selected(selection_value(selection)).label(trigger).width(220.).popup_width(260.).show_check().on_change(
                    move |value, _, cx| {
                        let Some(selection) = parse_selection(&value) else { return };
                        let _ = view.update(cx, |this, cx| this.set_project_model(selection, cx));
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
        let modes: Vec<(ThreadEnvMode, Source)> = shown.targets.iter().zip(&configs).map(|(project, config)| project_env_mode(&config.settings, &project.project.id, (project.reference() == shown.targets[0].reference()).then_some(file_mode).flatten())).collect();
        let mixed_mode = modes.iter().any(|(mode, _)| *mode != modes[0].0);
        let mode_reset = can_write && modes.iter().any(|(_, source)| *source == Source::Project);
        let view = cx.entity().downgrade();
        let mut workspace_select = Select::new("project-default-workspace-select", ENV_MODES.iter().map(|(mode, label)| SelectOption::new(env_mode_value(*mode), *label)).collect())
            .show_check()
            .on_change(move |value, _, cx| {
                let mode = match value.as_ref() {
                    "local" | "worktree" => value.to_string(),
                    _ => return,
                };
                let _ = view.update(cx, |this, cx| this.save_project_override("defaultThreadEnvMode", json!(mode), cx));
            });
        workspace_select = if mixed_mode { workspace_select.label("Mixed") } else { workspace_select.selected(env_mode_value(modes[0].0)) };
        let workspace_row = Row::new("project-default-workspace", "Workspace")
            .description("Where new threads in this project start.")
            .reset_tooltip("Reset to inherited value")
            .reset(mode_reset, {
                let view = cx.entity().downgrade();
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.clear_project_override("defaultThreadEnvMode", cx));
                }
            })
            .control(self.inert_unless(can_write, "project-default-workspace-inert", workspace_select, cx));
        section("New threads", vec![model_row.into_any_element(), workspace_row.into_any_element()], cx).into_any_element()
    }

    fn project_checkouts(&self, shown: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let can_edit = self.can_edit_projects(cx);
        let doc = store::client(cx);
        let grouping = doc.project_grouping_settings();
        let has_multiple = shown.group.members.len() > 1;
        let mut rows = vec![
            div()
                .px(r(16.))
                .py(r(10.))
                .text_xs()
                .text_color(hsla(palette(cx).muted_foreground))
                .child("Grouping rule decides how each checkout joins project groups in the sidebar. Changing it can move a checkout to a different project.")
                .into_any_element(),
        ];
        for checkout in &shown.group.members {
            let project = &checkout.project;
            let member = &project.project;
            let key = checkout.physical_key.clone();
            let selection = grouping.overrides.get(&key).map(|mode| mode.as_str()).unwrap_or("inherit");
            let trigger = if selection == "inherit" {
                format!("Default ({})", project_grouping_label(doc.project_grouping()))
            } else {
                project_grouping_label(ProjectGroupingMode::parse(selection).unwrap_or_default()).to_owned()
            };
            let options = std::iter::once(SelectOption::new("inherit", "Use global default"))
                .chain(PROJECT_GROUPING_MODE_LABELS.iter().map(|(mode, label)| SelectOption::new(mode.as_str(), *label)))
                .collect();
            let state = self.state.clone();
            let select =
                Select::new(SharedString::from(format!("checkout-grouping-{}", checkout.physical_key)), options).selected(selection).label(trigger).width(288.).on_change(
                    move |value, _, cx| {
                        let mode = if value.as_ref() == "inherit" {
                            None
                        } else {
                            let Some(mode) = ProjectGroupingMode::parse(&value) else { return };
                            Some(mode)
                        };
                        store::update_client(&state, ClientSettingChange::ProjectGroupingOverride { key: key.clone(), mode }, cx);
                    },
                );
            let ids = shown.targets.iter().filter(|project| project.physical_key() == checkout.physical_key).map(ScopedProject::reference).collect::<Vec<_>>();
            let controls = h_flex().gap(r(8.)).child(select).when(has_multiple, |controls| {
                controls.child(
                    Button::new(SharedString::from(format!("project-checkout-remove-{}", checkout.physical_key)), ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Remove")
                        .tooltip(format!("Remove checkout {}", member.workspace_root))
                        .disabled(!can_edit)
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_members(ids.clone(), cx))),
                )
            });
            rows.push(
                Row::new(SharedString::from(format!("project-checkout-{}", checkout.physical_key)), project.environment_label.clone())
                    .description(member.workspace_root.clone())
                    .control(controls)
                    .into_any_element(),
            );
        }
        section("Checkouts", rows, cx).into_any_element()
    }

    fn project_danger(&self, shown: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let copy = danger_copy(false, shown.members.len());
        let ids = shown.targets.iter().map(ScopedProject::reference).collect::<Vec<_>>();
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

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::sidebar::grouping::EnvironmentOwner;

    fn target(saved: &str) -> ScopedProject {
        ScopedProject {
            owner: EnvironmentOwner { saved_id: saved.into(), canonical_id: Some(saved.into()), generation: 1, connection_generation: 2, shell_generation: 3 },
            project: serde_json::from_value(json!({ "id": "same", "title": "Project", "workspaceRoot": "/same", "createdAt": "created", "updatedAt": "updated" })).unwrap(),
            is_scratch: false,
            environment_label: saved.into(),
        }
    }

    #[test]
    fn grouped_settings_capture_rejects_other_owners_replacements_and_denied_members() {
        let captured = IconTarget { members: vec![target("a"), target("b")] };
        assert!(captured.allows(&captured.clone(), true, false));
        assert!(!captured.allows(&captured.clone(), false, false));
        assert!(!captured.allows(&captured.clone(), true, true));
        let mut current = captured.clone();
        current.members[1].owner.saved_id = "c".into();
        assert!(!captured.allows(&current, true, false));
        let mut current = captured.clone();
        current.members[1].owner.shell_generation += 1;
        assert!(!captured.allows(&current, true, false));
        let mut current = captured.clone();
        current.members[1].project.created_at = "replacement".into();
        assert!(!captured.allows(&current, true, false));
        let mut current = captured.clone();
        current.members[1].project.workspace_root = "/replacement".into();
        assert!(!captured.allows(&current, true, false));
    }
}
