//! Project actions: add an existing folder (`project.create` through `projects.mutate`), create a
//! new one (`projects.createNew`), pin and unpin (`useProjectPin`), copy its path, start a thread
//! in it, and remove it (`project.delete` after Settings → Projects confirms). Shared by the
//! palette's Add project flows, the project menu and the Projects settings page.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, App, BoxShadow, ClipboardItem, Context, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Task, Window, deferred, div,
    hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::projects::{
    COPIED_FILES_MAX_COUNT, COPYABLE_FILE_MAX_BYTES, CloneProtocol, PROJECT_CLONE_CANCEL,
    PROJECT_CLONE_RETRY, PROJECT_CLONE_START, PROJECTS_LIST_IGNORED_FILES, PROJECTS_MUTATE,
    PROJECTS_READ_FILE, PROJECTS_WRITE_FILE, ProjectClonePhase, ProjectCloneSnapshot, ProjectCloneStartResult,
    ProjectIgnoredFile, ProjectListIgnoredFilesResult, ProjectReadFileResult, SUBSCRIBE_PROJECT_CLONES,
    add_project_initial_query, cancel_project_clone_request, clone_destination_path, clone_directory_name,
    clone_directory_path, clone_url_for_protocol, clone_url_protocol, compute_clone_preview,
    create_project, default_selected_ignored_files, extract_project_remote_url, extract_repository_display_name,
    extract_repository_root_path, find_project_by_path, format_bytes, infer_project_title, is_clone_query,
    list_ignored_files_request, project_clone_display_name, project_clone_progress_summary,
    project_subdirectory, read_project_file_request, resolve_add_project_path, retry_project_clone_request,
    set_project_pinned, start_project_clone, write_project_file_request,
};
use t3_core::settings::projects::delete_project;
use t3_core::sidebar::grouping::EnvironmentOwner;
use t3_core::auth::{ORCHESTRATION_OPERATE_SCOPE, SOURCE_CONTROL_WRITE_SCOPE};

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, Select, SelectOption};
use crate::sidebar::controller::uuid;
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette};
use crate::toast::{Toast, ToastKind};

/// Target connected machine eligible for `AddProjectToMachineDialog`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineTarget {
    pub saved_id: String,
    pub environment_id: String,
    pub label: String,
    pub add_project_base_directory: Option<String>,
    pub ghq_clone_layout: bool,
}

/// Global handoff from palette / menus / `AppState` to `ProjectDialogs` mounted in `Workspace`.
#[derive(Debug, Clone)]
pub enum PendingProjectModal {
    Clone {
        initial_url: String,
        owner: Option<EnvironmentOwner>,
    },
    AddToMachine {
        source_saved_id: String,
        project_id: String,
        target_saved_id: Option<String>,
    },
}

#[derive(Default)]
pub struct PendingProjectModalGlobal(pub Option<PendingProjectModal>);

impl gpui::Global for PendingProjectModalGlobal {}

impl AppState {
    fn validate_scoped_project_group(&self, captured: &t3_core::sidebar::grouping::ScopedProjectGroup, cx: &gpui::App) -> Result<(), &'static str> {
        let groups = self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
        let current = groups.iter().find(|group| group.key == captured.key).ok_or("The selected project group is no longer available.")?;
        if current.representative != captured.representative || current.member_refs.len() != captured.member_refs.len()
            || !captured.member_refs.iter().all(|reference| current.member_refs.contains(reference))
            || current.registrations.len() != captured.registrations.len()
            || !captured.registrations.iter().all(|project| current.registrations.iter().any(|candidate| candidate.reference() == project.reference()
                && candidate.owner == project.owner && candidate.project.created_at == project.project.created_at
                && candidate.project.workspace_root == project.project.workspace_root)) {
            return Err("The selected project group changed. Open its menu again.");
        }
        Ok(())
    }

    pub(crate) fn scoped_project_action_target(&self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, cx: &gpui::App) -> Result<t3_core::sidebar::grouping::ScopedProjectMember, &'static str> {
        self.validate_scoped_project_group(group, cx)?;
        t3_core::sidebar::grouping::pick_project_target(group, None).ok_or("The selected project is no longer available.")
    }

    fn scoped_project_navigation_target(&self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, cx: &gpui::App) -> Result<t3_core::sidebar::grouping::ScopedProjectMember, &'static str> {
        let target = self.scoped_project_action_target(group, cx)?;
        if !self.scoped_ready(&target.project.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
            return Err("The project destination is not connected or read access changed.");
        }
        Ok(target)
    }

    pub(crate) fn run_scoped_project_action(&mut self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, action: t3_core::thread_actions::ProjectAction, cx: &mut Context<Self>) {
        let navigation = matches!(action, t3_core::thread_actions::ProjectAction::NewThread | t3_core::thread_actions::ProjectAction::Settings);
        let target = match if navigation { self.scoped_project_navigation_target(group, cx) } else { self.scoped_project_action_target(group, cx) } {
            Ok(target) => target,
            Err(error) => {
                self.show_toast(Toast::new(ToastKind::Error, "Project action stopped").description(error), cx);
                return;
            }
        };
        match action {
            t3_core::thread_actions::ProjectAction::NewThread | t3_core::thread_actions::ProjectAction::Settings => self.navigate_scoped(target.project.owner.clone(), crate::app_state::ScopedRouteAction::Project { group:Box::new(group.clone()), action }, cx),
            t3_core::thread_actions::ProjectAction::CopyPath => self.copy_project_scoped(&target.project.project.workspace_root, cx),
            t3_core::thread_actions::ProjectAction::Pin => self.pin_scoped_project_group(group, true, cx),
            t3_core::thread_actions::ProjectAction::Unpin => self.pin_scoped_project_group(group, false, cx),
        }
    }

    pub(crate) fn finish_scoped_project_action(&mut self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, action: t3_core::thread_actions::ProjectAction, cx: &mut Context<Self>) {
        let target = match self.scoped_project_navigation_target(group, cx) {
            Ok(target) => target,
            Err(error) => {
                self.show_toast(Toast::new(ToastKind::Error, "Project action stopped").description(error), cx);
                return;
            }
        };
        match action {
            t3_core::thread_actions::ProjectAction::NewThread => self.start_draft(&target.project.project.id, cx),
            t3_core::thread_actions::ProjectAction::Settings => crate::settings::open_project(&target.project.project.id, cx),
            _ => {},
        }
    }

    fn copy_project_scoped(&mut self, path: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(path.to_owned()));
    }

    fn scoped_project_pinnable(&self, project: &t3_core::sidebar::grouping::ScopedProject) -> bool {
        self.scoped_ready(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
            && self.scoped_environment(&project.owner.saved_id).and_then(|entry| entry.config.config.as_ref())
                .is_some_and(|config| config.environment.capabilities.project_organization == Some(true))
            && self.scoped_project(&project.reference()).is_some_and(|current| current.owner == project.owner
                && current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root)
    }

    fn project_pin_refused(&mut self, completed: usize, cx: &mut Context<Self>) {
        self.show_toast(Toast::new(ToastKind::Error, "Project pin stopped")
            .description(format!("{completed} checkout(s) changed before the project, destination, permission or project organization support changed.")), cx);
    }

    pub(crate) fn pin_scoped_project_group(&mut self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, pinned: bool, cx: &mut Context<Self>) {
        if self.validate_scoped_project_group(group, cx).is_err() { self.project_pin_refused(0, cx); return; }
        let group = group.clone();
        let refs = group.mutation_refs();
        let members: Option<Vec<_>> = refs.iter().map(|key| group.registrations.iter().find(|project| project.reference() == *key).cloned()).collect();
        let Some(members) = members else { self.project_pin_refused(0, cx); return; };
        if members.iter().any(|project| !self.scoped_project_pinnable(project)) { self.project_pin_refused(0, cx); return; }
        let lookups: Vec<_> = members.iter().map(|project| self.connections.destination(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)).collect();
        cx.spawn(async move |this, cx| {
            let mut destinations = Vec::new();
            for lookup in lookups {
                let Ok(destination) = lookup.await else {
                    let _ = this.update(cx, |state, cx| state.project_pin_refused(0, cx));
                    return;
                };
                destinations.push(destination);
            }
            let preflight = this.read_with(cx, |state, cx| state.validate_scoped_project_group(&group, cx).is_ok() && members.iter().zip(&destinations).all(|(project, destination)| destination.is_current(&state.fleet)
                && state.scoped_project_pinnable(project)
                && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)))).unwrap_or(false);
            if !preflight {
                let _ = this.update(cx, |state, cx| state.project_pin_refused(0, cx));
                return;
            }
            for (index, (project, destination)) in members.iter().zip(destinations).enumerate() {
                let valid = this.read_with(cx, |state, cx| state.validate_scoped_project_group(&group, cx).is_ok() && destination.is_current(&state.fleet) && state.scoped_project_pinnable(project)
                    && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))).unwrap_or(false);
                if !valid {
                    let _ = this.update(cx, |state, cx| state.project_pin_refused(index, cx));
                    return;
                }
                if let Err(error) = destination.client.request(PROJECTS_MUTATE, set_project_pinned(&uuid(), &project.project.id, pinned)).await {
                    let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Project pin stopped").description(format!("{index} checkout(s) changed; {}: {error}", project.environment_label)), cx));
                    return;
                }
            }
        }).detach();
    }

    /// `useProjectGroupUpdate`'s gate: servers without `projectOrganization` drop pin and archive.
    pub fn can_pin_projects(&self) -> bool {
        self.server_config.config.as_ref().is_some_and(|config| config.environment.capabilities.project_organization == Some(true))
    }

    /// Opens the interactive Clone Repository modal (`AP-PROJECTS`), optionally pre-filled with a URL.
    pub fn open_clone_project_modal(&mut self, initial_url: impl Into<String>, cx: &mut Context<Self>) {
        self.open_clone_project_on(None, initial_url.into(), cx);
    }

    pub(crate) fn open_clone_project_on(&self, owner: Option<EnvironmentOwner>, initial_url: String, cx: &mut Context<Self>) {
        cx.set_global(PendingProjectModalGlobal(Some(PendingProjectModal::Clone {
            initial_url,
            owner,
        })));
        cx.emit(AppEvent::Notice);
    }

    /// Opens `AddProjectToMachineDialog` (`AddProjectToMachineDialog.tsx`) to clone `project_id`
    /// onto another connected machine and relay chosen git-ignored files.
    pub fn open_add_project_to_machine(
        &mut self,
        source_saved_id: impl Into<String>,
        project_id: impl Into<String>,
        target_saved_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        cx.set_global(PendingProjectModalGlobal(Some(PendingProjectModal::AddToMachine {
            source_saved_id: source_saved_id.into(),
            project_id: project_id.into(),
            target_saved_id,
        })));
        cx.emit(AppEvent::Notice);
    }

    /// Connected fleet machines that do not already hold `project_id`'s logical repository.
    pub fn add_project_to_machine_targets(&self, source_saved_id: &str, project_id: &str) -> Vec<MachineTarget> {
        let source_identity = if self.environments.active.as_deref() == Some(source_saved_id) {
            self.shell.project(project_id).and_then(|p| p.repository_identity.as_ref())
        } else {
            self.fleet
                .environments
                .iter()
                .find(|env| env.summary.owner.saved_id == source_saved_id)
                .and_then(|env| env.summary.projects.iter().find(|p| p.project.id == project_id))
                .and_then(|p| p.project.repository_identity.as_ref())
        };
        let source_key = source_identity.and_then(t3_core::prompt_links::repository_key);
        let mut out = Vec::new();
        for env in &self.fleet.environments {
            let saved_id = &env.summary.owner.saved_id;
            if saved_id == source_saved_id
                || !self.scoped_ready(&env.summary.owner, SOURCE_CONTROL_WRITE_SCOPE)
            {
                continue;
            }
            if let Some(key) = source_key {
                let already_present = env.summary.projects.iter().any(|p| {
                    p.project.archived_at.is_none()
                        && p.project
                            .repository_identity
                            .as_ref()
                            .and_then(t3_core::prompt_links::repository_key)
                            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(key))
                });
                if already_present {
                    continue;
                }
            }
            let cfg = env.config.config.as_ref();
            let env_id = cfg
                .map(|c| c.environment.environment_id.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| saved_id.clone());
            let label = self
                .environments
                .get(saved_id)
                .map(|e| e.label.clone())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| cfg.map(|c| c.environment.label.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| saved_id.clone());
            let base_dir = cfg
                .map(|c| c.settings.threads.add_project_base_directory.clone())
                .filter(|s| !s.trim().is_empty());
            let ghq_layout = cfg.is_some_and(|c| c.settings.threads.ghq_clone_layout);
            out.push(MachineTarget {
                saved_id: saved_id.clone(),
                environment_id: env_id,
                label,
                add_project_base_directory: base_dir,
                ghq_clone_layout: ghq_layout,
            });
        }
        out
    }

    /// Adds the folder a browse query names (or opens the interactive Clone Repository modal when
    /// given a repository URL / `owner/repo`), or opens a draft in the project already there.
    pub fn add_project(&mut self, query: &str, cx: &mut Context<Self>) {
        if is_clone_query(query) {
            self.open_clone_project_modal(query.trim(), cx);
            return;
        }
        let platform = self
            .server_config
            .config
            .as_ref()
            .and_then(|c| c.environment.platform.as_ref())
            .map(|p| p.os.as_str())
            .unwrap_or("darwin");
        let current_cwd = self
            .selected_thread
            .as_deref()
            .and_then(|id| self.thread_shell(id))
            .and_then(|t| self.shell.project(&t.project_id))
            .or_else(|| self.draft_project.as_deref().and_then(|id| self.shell.project(id)))
            .map(|p| p.workspace_root.as_str());
        let path = match resolve_add_project_path(query, current_cwd, platform) {
            Ok(path) => path,
            Err(error) => {
                self.show_toast(Toast::new(ToastKind::Error, "Failed to add project").description(error), cx);
                return;
            }
        };
        let existing = self.shell.snapshot.as_ref().and_then(|snapshot| find_project_by_path(&snapshot.projects, &path, |project| &project.workspace_root).map(|project| project.id.clone()));
        if let Some(project_id) = existing {
            self.show_toast(Toast::new(ToastKind::Warning, "Project already added").description(path), cx);
            self.start_draft(&project_id, cx);
            return;
        }
        let project_id = uuid();
        let title = infer_project_title(&path);
        let request = self.rpc(PROJECTS_MUTATE, create_project(&uuid(), &project_id, &title, &path));
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |state, cx| match result {
                Ok(_) => {
                    state.show_toast(Toast::new(ToastKind::Success, "Project added").description(title), cx);
                    state.start_draft(&project_id, cx);
                }
                Err(error) => state.show_toast(Toast::new(ToastKind::Error, "Failed to add project").description(error.to_string()), cx),
            });
        })
        .detach();
    }

    pub(crate) fn project_request(
        &self,
        owner: Option<&EnvironmentOwner>,
        scope: &str,
        method: &'static str,
        payload: serde_json::Value,
    ) -> impl Future<Output = Result<serde_json::Value, String>> + use<> {
        let destination = owner.map(|owner| self.connections.destination(owner, scope));
        let active = destination.is_none().then(|| self.rpc(method, payload.clone()));
        async move {
            if let Some(active) = active {
                return active.await.map_err(|error| error.to_string());
            }
            let dest = destination.expect("scoped project request").await
                .map_err(|error| format!("Project destination is unavailable ({error:?})."))?;
            dest.client.request(method, payload).await.map_err(|error| error.to_string())
        }
    }

    pub(crate) fn open_created_project(&mut self, owner: Option<EnvironmentOwner>, project_id: String, cx: &mut Context<Self>) {
        if let Some(owner) = owner {
            self.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Draft(project_id), cx);
        } else {
            self.start_draft(&project_id, cx);
        }
    }

    pub(crate) fn add_scoped_project(&mut self, owner: EnvironmentOwner, query: &str, cx: &mut Context<Self>) {
        if is_clone_query(query) {
            self.open_clone_project_on(Some(owner), query.trim().to_owned(), cx);
            return;
        }
        let Some(entry) = self.scoped_environment(&owner.saved_id).filter(|entry| entry.summary.owner == owner) else { return };
        let platform = entry.config.config.as_ref().and_then(|config| config.environment.platform.as_ref())
            .map_or("darwin", |platform| platform.os.as_str());
        let current_cwd = (self.environments.active.as_deref() == Some(owner.saved_id.as_str())).then(|| {
            self.selected_thread.as_deref().and_then(|id| self.thread_shell(id)).map(|thread| thread.project_id.as_str())
                .or(self.draft_project.as_deref())
                .and_then(|id| self.shell.project(id))
                .map(|project| project.workspace_root.as_str())
        }).flatten();
        let path = match resolve_add_project_path(query, current_cwd, platform) {
            Ok(path) => path,
            Err(error) => {
                self.show_toast(Toast::new(ToastKind::Error, "Failed to add project").description(error), cx);
                return;
            }
        };
        if let Some(project) = find_project_by_path(&entry.summary.projects, &path, |project| &project.project.workspace_root) {
            self.open_created_project(Some(owner), project.project.id.clone(), cx);
            return;
        }
        let project_id = uuid();
        let title = infer_project_title(&path);
        let req = self.project_request(Some(&owner), ORCHESTRATION_OPERATE_SCOPE, PROJECTS_MUTATE, create_project(&uuid(), &project_id, &title, &path));
        cx.spawn(async move |this, cx| {
            let result = req.await;
            let _ = this.update(cx, |state, cx| match result {
                Ok(_) => {
                    state.show_toast(Toast::new(ToastKind::Success, "Project added").description(title), cx);
                    state.open_created_project(Some(owner), project_id, cx);
                }
                Err(error) => state.show_toast(Toast::new(ToastKind::Error, "Failed to add project").description(error), cx),
            });
        }).detach();
    }

    pub fn set_project_pinned(&mut self, project_id: &str, pinned: bool, cx: &mut Context<Self>) {
        let request = self.rpc(PROJECTS_MUTATE, set_project_pinned(&uuid(), project_id, pinned));
        let title = if pinned { "Failed to pin project" } else { "Failed to unpin project" };
        cx.spawn(async move |this, cx| {
            if let Err(error) = request.await {
                let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(error.to_string()), cx));
            }
        })
        .detach();
    }

    pub fn copy_project_path(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some(path) = self.shell.project(project_id).map(|project| project.workspace_root.clone()) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(path));
    }

    /// `removeMembers` once confirmed: a forced `project.delete` per member, in order, stopping at
    /// the first failure. A draft in a removed project closes. Removing the whole project returns
    /// Settings to the project list.
    pub(crate) fn remove_scoped_projects(&mut self, members: Vec<t3_core::sidebar::grouping::ScopedProject>, back_to_list: bool, cx: &mut Context<Self>) {
        let lookups = members.iter().map(|project| self.connections.destination(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)).collect::<Vec<_>>();
        cx.spawn(async move |this, cx| {
            let mut destinations = Vec::new();
            for lookup in lookups { let Ok(destination) = lookup.await else { return }; destinations.push(destination); }
            let valid = |state: &AppState, project: &t3_core::sidebar::grouping::ScopedProject, destination: &t3_client::environments::Destination| destination.is_current(&state.fleet)
                && state.scoped_ready(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
                && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
                && state.scoped_project(&project.reference()).is_some_and(|current| current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root);
            if !this.read_with(cx, |state, _| members.iter().zip(&destinations).all(|(project, destination)| valid(state, project, destination))).unwrap_or(false) { return; }
            for (index, (project, destination)) in members.into_iter().zip(destinations).enumerate() {
                if !this.read_with(cx, |state, _| valid(state, &project, &destination)).unwrap_or(false) {
                    cx.update(|cx| crate::settings::store::show_error(format!("Removed {index} checkout(s); destination changed before removing {}.", project.project.title), cx)); return;
                }
                if let Err(error) = destination.client.request(PROJECTS_MUTATE, delete_project(&uuid(), &project.project.id)).await {
                    cx.update(|cx| crate::settings::store::show_error(format!("Removed {index} checkout(s); failed to remove {}: {error}", project.project.title), cx)); return;
                }
                let _ = this.update(cx, |state, cx| {
                    if state.environments.active.as_deref() == Some(&project.owner.saved_id) && state.draft_project.as_deref() == Some(&project.project.id) {
                        state.draft_project = None; state.clear_selection(cx);
                    }
                });
            }
            if back_to_list { cx.update(crate::settings::show_project_list); }
        }).detach();
    }
}

/// Interactive project modals mounted on `Workspace`:
/// 1. Full repository clone modal (`projectClone.start` + URL/host/ghq preview + live progress/cancel/retry/error).
/// 2. `AddProjectToMachineDialog` (`AddProjectToMachineDialog.tsx` + `lib/addProjectToMachine.ts` ignored-file relay).
pub struct ProjectDialogs {
    state: Entity<AppState>,
    clone: Option<CloneModalState>,
    add_to_machine: Option<AddToMachineState>,
    composer: Option<gpui::WeakEntity<crate::composer::Composer>>,
    _subscriptions: Vec<Subscription>,
}

struct ActiveCloneState {
    project_id: String,
    remote_url: String,
    destination_path: String,
    snapshot: Option<ProjectCloneSnapshot>,
    action_pending: bool,
    action_error: Option<String>,
    _stream: Option<Task<()>>,
}

struct CloneModalState {
    owner: Option<EnvironmentOwner>,
    url_input: Entity<InputState>,
    dest_input: Entity<InputState>,
    protocol_override: Option<CloneProtocol>,
    ghq_layout: bool,
    path_overridden: bool,
    syncing_dest: bool,
    submitting: bool,
    error: Option<String>,
    active_clone: Option<ActiveCloneState>,
    _input_subs: Vec<Subscription>,
}

struct AddToMachineState {
    source_saved_id: String,
    source_project_id: String,
    source_label: String,
    project_title: String,
    display_name: String,
    workspace_root: String,
    repository_root: Option<String>,
    source_url: String,
    targets: Vec<MachineTarget>,
    selected_target_idx: usize,
    protocol: Option<CloneProtocol>,
    dest_input: Entity<InputState>,
    search_input: Entity<InputState>,
    path_overridden: bool,
    syncing_dest: bool,
    can_list_files: bool,
    loading_files: bool,
    files_error: Option<String>,
    all_files: Vec<ProjectIgnoredFile>,
    shown_files: Vec<ProjectIgnoredFile>,
    truncated: bool,
    file_query: String,
    user_edited_selection: bool,
    selected_files: BTreeMap<String, ProjectIgnoredFile>,
    submitting: bool,
    error: Option<String>,
    search_seq: u64,
    _list_task: Option<Task<()>>,
    _input_subs: Vec<Subscription>,
}

impl ProjectDialogs {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
            if *event == AppEvent::Notice {
                this.consume_pending(window, cx);
            }
        });
        let mut this = Self {
            state,
            clone: None,
            add_to_machine: None,
            composer: None,
            _subscriptions: vec![sub],
        };
        this.consume_pending(window, cx);
        this
    }

    pub(crate) fn with_composer(mut self, composer: &Entity<crate::composer::Composer>) -> Self {
        self.composer = Some(composer.downgrade());
        self
    }

    pub(crate) fn is_open(&self) -> bool {
        self.clone.is_some() || self.add_to_machine.is_some()
    }

    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.is_open() {
            return false;
        }
        self.close(cx);
        true
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn add_to_machine_summary_for_test(&self) -> Option<(String, String, Option<String>)> {
        let modal = self.add_to_machine.as_ref()?;
        let target = modal.targets.get(modal.selected_target_idx).map(|t| t.saved_id.clone());
        Some((modal.source_saved_id.clone(), modal.workspace_root.clone(), target))
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.clone = None;
        self.add_to_machine = None;
        cx.notify();
    }

    fn consume_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = cx
            .try_global::<PendingProjectModalGlobal>()
            .and_then(|g| g.0.clone());
        let Some(request) = pending else { return };
        cx.set_global(PendingProjectModalGlobal(None));
        match request {
            PendingProjectModal::Clone { initial_url, owner } => {
                self.open_clone_modal(initial_url, owner, window, cx);
            }
            PendingProjectModal::AddToMachine {
                source_saved_id,
                project_id,
                target_saved_id,
            } => {
                self.open_add_to_machine_modal(source_saved_id, project_id, target_saved_id, window, cx);
            }
        }
    }

    fn clone_base_dirs(&self, owner: Option<&EnvironmentOwner>, cx: &App) -> (Option<String>, Option<String>, bool, String) {
        let app = self.state.read(cx);
        let cfg = match owner {
            Some(owner) => app.scoped_environment(&owner.saved_id).and_then(|entry| entry.config.config.as_ref()),
            None => app.server_config.config.as_ref(),
        };
        let base_dir = cfg
            .map(|c| c.settings.threads.add_project_base_directory.clone())
            .filter(|s| !s.trim().is_empty());
        let fallback_root = cfg.and_then(|c| c.new_projects_root.clone());
        let ghq_layout = cfg.is_some_and(|c| c.settings.threads.ghq_clone_layout);
        let env_label = owner.and_then(|owner| app.environments.get(&owner.saved_id))
            .or_else(|| owner.is_none().then(|| app.active_environment()).flatten())
            .map(|e| e.label.clone())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| cfg.map(|c| c.environment.label.clone()).filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "this machine".to_owned());
        (base_dir, fallback_root, ghq_layout, env_label)
    }

    fn open_clone_modal(&mut self, initial_url: String, owner: Option<EnvironmentOwner>, window: &mut Window, cx: &mut Context<Self>) {
        self.add_to_machine = None;
        let (base_dir, fallback_root, ghq_layout, _) = self.clone_base_dirs(owner.as_ref(), cx);
        let preview = compute_clone_preview(
            &initial_url,
            base_dir.as_deref(),
            fallback_root.as_deref(),
            ghq_layout,
            None,
        );
        let url_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("owner/repo, https://..., or git@...");
            if !initial_url.is_empty() {
                input.set_value(initial_url.clone(), window, cx);
            }
            input
        });
        let dest_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("~/projects/repo");
            input.set_value(preview.destination_path.clone(), window, cx);
            input
        });
        let url_sub = cx.subscribe_in(&url_input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                this.sync_clone_destination(window, cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit_clone(window, cx),
            _ => {}
        });
        let dest_sub = cx.subscribe_in(&dest_input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                if let Some(modal) = this.clone.as_mut()
                    && !modal.syncing_dest
                {
                    modal.path_overridden = true;
                    modal.error = None;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit_clone(window, cx),
            _ => {}
        });
        // Focusing a native macOS `Input` inside GPUI's headless `TestWindow` panics in
        // `set_text_content_type` (`TestWindow::window_handle` is unimplemented).
        #[cfg(not(test))]
        url_input.update(cx, |input, cx| input.focus(window, cx));
        self.clone = Some(CloneModalState {
            owner,
            url_input,
            dest_input,
            protocol_override: None,
            ghq_layout,
            path_overridden: false,
            syncing_dest: false,
            submitting: false,
            error: None,
            active_clone: None,
            _input_subs: vec![url_sub, dest_sub],
        });
        cx.notify();
    }

    fn sync_clone_destination(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (base_dir, fallback_root, _, _) = self.clone_base_dirs(self.clone.as_ref().and_then(|modal| modal.owner.as_ref()), cx);
        let Some(modal) = self.clone.as_mut() else { return };
        modal.error = None;
        if modal.path_overridden {
            return;
        }
        let raw = modal.url_input.read(cx).value().to_string();
        let preview = compute_clone_preview(
            &raw,
            base_dir.as_deref(),
            fallback_root.as_deref(),
            modal.ghq_layout,
            modal.protocol_override,
        );
        modal.syncing_dest = true;
        modal.dest_input.update(cx, |input, cx| {
            input.set_value(preview.destination_path, window, cx);
        });
        modal.syncing_dest = false;
    }

    fn set_clone_protocol(&mut self, protocol: CloneProtocol, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.clone.as_mut() {
            modal.protocol_override = Some(protocol);
        }
        self.sync_clone_destination(window, cx);
        cx.notify();
    }

    fn set_clone_ghq_layout(&mut self, ghq_layout: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.clone.as_mut() {
            modal.ghq_layout = ghq_layout;
        }
        self.sync_clone_destination(window, cx);
        cx.notify();
    }

    fn reset_clone_destination(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.clone.as_mut() {
            modal.path_overridden = false;
        }
        self.sync_clone_destination(window, cx);
        cx.notify();
    }

    fn submit_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (base_dir, fallback_root, _, _) = self.clone_base_dirs(self.clone.as_ref().and_then(|modal| modal.owner.as_ref()), cx);
        let (raw_url, destination, preview) = {
            let Some(modal) = self.clone.as_ref() else { return };
            if modal.submitting || modal.active_clone.is_some() {
                return;
            }
            let raw_url = modal.url_input.read(cx).value().to_string();
            let destination = modal.dest_input.read(cx).value().trim().to_owned();
            let preview = compute_clone_preview(
                &raw_url,
                base_dir.as_deref(),
                fallback_root.as_deref(),
                modal.ghq_layout,
                modal.protocol_override,
            );
            (raw_url, destination, preview)
        };
        if let Some(hint) = preview.incomplete_hint {
            if let Some(modal) = self.clone.as_mut() {
                modal.error = Some(hint.to_owned());
            }
            cx.notify();
            return;
        }
        if raw_url.trim().is_empty() || preview.title.is_empty() {
            if let Some(modal) = self.clone.as_mut() {
                modal.error = Some("Enter a valid repository URL or owner/repo.".to_owned());
            }
            cx.notify();
            return;
        }
        if destination.is_empty() {
            if let Some(modal) = self.clone.as_mut() {
                modal.error = Some("Enter a destination folder.".to_owned());
            }
            cx.notify();
            return;
        }
        if let Some(modal) = self.clone.as_mut() {
            modal.submitting = true;
            modal.error = None;
        }
        cx.notify();

        let project_id = uuid();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let payload = start_project_clone(&project_id, &preview.title, &created_at, &preview.remote_url, &destination);
        let owner = self.clone.as_ref().and_then(|modal| modal.owner.clone());
        let req = self.state.read(cx).project_request(owner.as_ref(), SOURCE_CONTROL_WRITE_SCOPE, PROJECT_CLONE_START, payload);
        let (remote_client, active_client) = match owner.as_ref() {
            Some(owner) => (Some(self.state.read(cx).connections.client(&owner.saved_id)), None),
            None => (None, self.state.read(cx).client_handle()),
        };
        let runtime = self.state.read(cx).runtime().clone();

        cx.spawn_in(window, async move |this, cx| {
            let result = req
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::from_value::<ProjectCloneStartResult>(value).map_err(|e| e.to_string()));
            let client = match remote_client {
                Some(remote) if result.is_ok() => remote.await,
                _ => active_client,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(modal) = this.clone.as_mut() else { return };
                modal.submitting = false;
                match result {
                    Ok(started) => {
                        let watched_id = started.project_id.clone();
                        let stream_task = client.map(|client| {
                            cx.spawn_in(window, async move |this, cx| {
                                let stream = runtime
                                    .spawn(async move { client.stream(SUBSCRIBE_PROJECT_CLONES, serde_json::json!({})).await })
                                    .await
                                    .ok()
                                    .and_then(Result::ok);
                                let Some(mut handle) = stream else {
                                    // Older servers predate `subscribeProjectClones`: open draft immediately.
                                    let _ = this.update(cx, |this, cx| {
                                        this.clone = None;
                                        this.state.update(cx, |s, cx| s.open_created_project(owner.clone(), watched_id.clone(), cx));
                                        cx.notify();
                                    });
                                    return;
                                };
                                let mut terminal = false;
                                let mut stream_err: Option<String> = None;
                                while let Some(frame) = handle.receiver.recv().await {
                                    let value = match frame {
                                        Ok(value) => value,
                                        Err(error) => {
                                            stream_err = Some(error.to_string());
                                            break;
                                        }
                                    };
                                    let snapshots = serde_json::from_value::<Vec<ProjectCloneSnapshot>>(value).unwrap_or_default();
                                    let Some(matched) = snapshots.into_iter().find(|s| s.project_id == watched_id) else {
                                        continue;
                                    };
                                    let done = matched.phase == ProjectClonePhase::Done;
                                    terminal = matches!(
                                        matched.phase,
                                        ProjectClonePhase::Done | ProjectClonePhase::Failed | ProjectClonePhase::Cancelled
                                    );
                                    let display = project_clone_display_name(&matched);
                                    let dest = matched.destination_path.clone();
                                    let keep_going = this.update(cx, |this, cx| {
                                        let Some(modal) = this.clone.as_mut() else { return false };
                                        let Some(active) = modal.active_clone.as_mut() else { return false };
                                        if active.project_id != watched_id {
                                            return false;
                                        }
                                        active.snapshot = Some(matched);
                                        if done {
                                            this.clone = None;
                                            this.state.update(cx, |s, cx| {
                                                s.show_toast(
                                                    Toast::new(ToastKind::Success, format!("Cloned {display}")).description(dest),
                                                    cx,
                                                );
                                                s.open_created_project(owner.clone(), watched_id.clone(), cx);
                                            });
                                        }
                                        cx.notify();
                                        !done
                                    });
                                    if !matches!(keep_going, Ok(true)) {
                                        break;
                                    }
                                }
                                if !terminal {
                                    let reason = stream_err.unwrap_or_else(|| {
                                        "Lost connection while monitoring clone progress.".to_owned()
                                    });
                                    let _ = this.update(cx, |this, cx| {
                                        if let Some(modal) = this.clone.as_mut()
                                            && let Some(active) = modal.active_clone.as_mut()
                                            && active.project_id == watched_id
                                        {
                                            let already_terminal = active.snapshot.as_ref().is_some_and(|s| {
                                                matches!(
                                                    s.phase,
                                                    ProjectClonePhase::Done
                                                        | ProjectClonePhase::Failed
                                                        | ProjectClonePhase::Cancelled
                                                )
                                            });
                                            if !already_terminal {
                                                active.action_error = Some(reason);
                                                cx.notify();
                                            }
                                        }
                                    });
                                }
                            })
                        });
                        modal.active_clone = Some(ActiveCloneState {
                            project_id: started.project_id,
                            remote_url: started.remote_url,
                            destination_path: started.cwd,
                            snapshot: None,
                            action_pending: false,
                            action_error: None,
                            _stream: stream_task,
                        });
                    }
                    Err(error) => {
                        modal.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_active_clone(&mut self, cx: &mut Context<Self>) {
        let project_id = match self.clone.as_mut().and_then(|m| m.active_clone.as_mut()) {
            Some(active) if !active.action_pending => {
                active.action_pending = true;
                active.action_error = None;
                active.project_id.clone()
            }
            _ => return,
        };
        let owner = self.clone.as_ref().and_then(|modal| modal.owner.as_ref());
        let req = self.state.read(cx).project_request(owner, SOURCE_CONTROL_WRITE_SCOPE, PROJECT_CLONE_CANCEL, cancel_project_clone_request(&project_id));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = req.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(active) = this.clone.as_mut().and_then(|m| m.active_clone.as_mut()) {
                    active.action_pending = false;
                    if let Err(error) = result {
                        active.action_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn retry_active_clone(&mut self, cx: &mut Context<Self>) {
        let project_id = match self.clone.as_mut().and_then(|m| m.active_clone.as_mut()) {
            Some(active) if !active.action_pending => {
                active.action_pending = true;
                active.action_error = None;
                active.project_id.clone()
            }
            _ => return,
        };
        let owner = self.clone.as_ref().and_then(|modal| modal.owner.as_ref());
        let req = self.state.read(cx).project_request(owner, SOURCE_CONTROL_WRITE_SCOPE, PROJECT_CLONE_RETRY, retry_project_clone_request(&project_id));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = req.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(active) = this.clone.as_mut().and_then(|m| m.active_clone.as_mut()) {
                    active.action_pending = false;
                    if let Err(error) = result {
                        active.action_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_active_clone_background(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.clone.as_ref().and_then(|m| m.active_clone.as_ref()).map(|a| a.project_id.clone()) else {
            return;
        };
        let owner = self.clone.take().and_then(|modal| modal.owner);
        self.state.update(cx, |s, cx| s.open_created_project(owner, project_id, cx));
        cx.notify();
    }

    fn open_add_to_machine_modal(
        &mut self,
        source_saved_id: String,
        project_id: String,
        target_saved_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clone = None;
        let (source_label, project, targets) = {
            let app = self.state.read(cx);
            let is_active = app.environments.active.as_deref() == Some(source_saved_id.as_str())
                || source_saved_id.is_empty();
            let effective_saved = if source_saved_id.is_empty() {
                app.environments.active.clone().unwrap_or_default()
            } else {
                source_saved_id.clone()
            };
            let project = if is_active {
                app.shell.project(&project_id).cloned()
            } else {
                app.fleet
                    .environments
                    .iter()
                    .find(|e| e.summary.owner.saved_id == effective_saved)
                    .and_then(|e| e.summary.projects.iter().find(|p| p.project.id == project_id))
                    .map(|p| p.project.clone())
            };
            let source_label = app
                .environments
                .get(&effective_saved)
                .map(|e| e.label.clone())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| app.server_config.config.as_ref().map(|c| c.environment.label.clone()))
                .unwrap_or_else(|| "This machine".to_owned());
            let targets = app.add_project_to_machine_targets(&effective_saved, &project_id);
            (source_label, project, targets)
        };
        let Some(project) = project else { return };
        let source_url = extract_project_remote_url(project.repository_identity.as_ref()).unwrap_or_default();
        let display_name = extract_repository_display_name(project.repository_identity.as_ref())
            .unwrap_or_else(|| project.title.clone());
        let repository_root = extract_repository_root_path(project.repository_identity.as_ref());
        let selected_target_idx = target_saved_id
            .as_deref()
            .and_then(|wanted| targets.iter().position(|t| t.saved_id == wanted || t.environment_id == wanted))
            .unwrap_or(0);
        let default_dest = targets
            .get(selected_target_idx)
            .map(|target| {
                clone_destination_path(
                    &add_project_initial_query(target.add_project_base_directory.as_deref()),
                    &clone_directory_path(&source_url, target.ghq_clone_layout, None),
                )
            })
            .unwrap_or_default();

        let dest_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("~/projects/repo");
            input.set_value(default_dest, window, cx);
            input
        });
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search ignored files"));
        let dest_sub = cx.subscribe_in(&dest_input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(modal) = this.add_to_machine.as_mut()
                && !modal.syncing_dest
            {
                modal.path_overridden = true;
                modal.error = None;
                cx.notify();
            }
        });
        let search_sub = cx.subscribe_in(&search_input, window, |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                this.on_ignored_file_query_changed(query, window, cx);
            }
        });

        let protocol = clone_url_protocol(&source_url);
        let workspace_root = project.workspace_root.clone();
        self.add_to_machine = Some(AddToMachineState {
            source_saved_id: source_saved_id.clone(),
            source_project_id: project_id,
            source_label,
            project_title: project.title,
            display_name,
            workspace_root: workspace_root.clone(),
            repository_root,
            source_url,
            targets,
            selected_target_idx,
            protocol,
            dest_input,
            search_input,
            path_overridden: false,
            syncing_dest: false,
            can_list_files: true,
            loading_files: true,
            files_error: None,
            all_files: Vec::new(),
            shown_files: Vec::new(),
            truncated: false,
            file_query: String::new(),
            user_edited_selection: false,
            selected_files: BTreeMap::new(),
            submitting: false,
            error: None,
            search_seq: 0,
            _list_task: None,
            _input_subs: vec![dest_sub, search_sub],
        });
        self.fetch_ignored_files(None, window, cx);
        cx.notify();
    }

    fn fetch_ignored_files(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (source_saved_id, cwd, seq) = {
            let Some(modal) = self.add_to_machine.as_mut() else { return };
            modal.search_seq = modal.search_seq.wrapping_add(1);
            (modal.source_saved_id.clone(), modal.workspace_root.clone(), modal.search_seq)
        };
        let is_initial = query.is_none();
        let active_saved = self.state.read(cx).environments.active.clone().unwrap_or_default();
        let active_client = self.state.read(cx).client_handle();
        let fleet_fut = (!source_saved_id.is_empty() && source_saved_id != active_saved)
            .then(|| self.state.read(cx).connections.client(&source_saved_id));
        let runtime = self.state.read(cx).runtime().clone();
        let payload = list_ignored_files_request(&cwd, query.as_deref());

        let task = cx.spawn_in(window, async move |this, cx| {
            let client = match fleet_fut {
                Some(fut) => fut.await,
                None => active_client,
            };
            let result = match client {
                Some(client) => runtime
                    .spawn(async move { client.request(PROJECTS_LIST_IGNORED_FILES, payload).await })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
                    .and_then(|v| serde_json::from_value::<ProjectListIgnoredFilesResult>(v).map_err(|e| e.to_string())),
                None => Err("Source environment is not connected.".to_owned()),
            };
            let _ = this.update(cx, |this, cx| {
                let Some(modal) = this.add_to_machine.as_mut() else { return };
                if modal.search_seq != seq {
                    return;
                }
                modal.loading_files = false;
                match result {
                    Ok(list) => {
                        modal.files_error = None;
                        if is_initial {
                            modal.truncated = list.truncated;
                            modal.all_files = list.files.clone();
                            if !modal.user_edited_selection {
                                modal.selected_files = default_selected_ignored_files(&list.files)
                                    .into_iter()
                                    .map(|f| (f.path.clone(), f))
                                    .collect();
                            }
                        }
                        let needle = modal.file_query.trim().to_ascii_lowercase();
                        modal.shown_files = if needle.is_empty() {
                            list.files
                        } else {
                            list.files
                                .into_iter()
                                .filter(|f| f.path.to_ascii_lowercase().contains(&needle))
                                .collect()
                        };
                    }
                    Err(error) => {
                        let unsupported = error.contains("Method not found")
                            || error.contains("Unknown RPC")
                            || error.contains("not implemented");
                        if is_initial && unsupported {
                            modal.can_list_files = false;
                            modal.files_error = None;
                        } else {
                            modal.files_error = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        });
        if let Some(modal) = self.add_to_machine.as_mut() {
            modal.loading_files = true;
            modal._list_task = Some(task);
        }
    }

    fn on_ignored_file_query_changed(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        let should_remote_search = {
            let Some(modal) = self.add_to_machine.as_mut() else { return };
            modal.file_query = query.clone();
            let trimmed = query.trim();
            if trimmed.is_empty() {
                modal.search_seq = modal.search_seq.wrapping_add(1);
                modal._list_task = None;
                modal.loading_files = false;
                modal.shown_files = modal.all_files.clone();
                false
            } else if modal.truncated {
                true
            } else {
                modal.search_seq = modal.search_seq.wrapping_add(1);
                modal._list_task = None;
                modal.loading_files = false;
                let needle = trimmed.to_ascii_lowercase();
                modal.shown_files = modal
                    .all_files
                    .iter()
                    .filter(|f| f.path.to_ascii_lowercase().contains(&needle))
                    .cloned()
                    .collect();
                false
            }
        };
        if should_remote_search {
            self.fetch_ignored_files(Some(query), window, cx);
        }
        cx.notify();
    }

    fn select_machine_target(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.add_to_machine.as_mut() else { return };
        if index >= modal.targets.len() {
            return;
        }
        modal.selected_target_idx = index;
        if !modal.path_overridden {
            let target = &modal.targets[index];
            let default_dest = clone_destination_path(
                &add_project_initial_query(target.add_project_base_directory.as_deref()),
                &clone_directory_path(&modal.source_url, target.ghq_clone_layout, None),
            );
            modal.syncing_dest = true;
            modal.dest_input.update(cx, |input, cx| input.set_value(default_dest, window, cx));
            modal.syncing_dest = false;
        }
        cx.notify();
    }

    fn toggle_ignored_file(&mut self, file: ProjectIgnoredFile, cx: &mut Context<Self>) {
        let Some(modal) = self.add_to_machine.as_mut() else { return };
        if file.byte_length > COPYABLE_FILE_MAX_BYTES {
            return;
        }
        modal.user_edited_selection = true;
        if modal.selected_files.contains_key(&file.path) {
            modal.selected_files.remove(&file.path);
        } else if modal.selected_files.len() < COPIED_FILES_MAX_COUNT {
            modal.selected_files.insert(file.path.clone(), file);
        }
        cx.notify();
    }

    fn toggle_all_shown_ignored_files(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.add_to_machine.as_mut() else { return };
        modal.user_edited_selection = true;
        let copyable: Vec<ProjectIgnoredFile> = modal
            .shown_files
            .iter()
            .filter(|f| f.byte_length <= COPYABLE_FILE_MAX_BYTES)
            .cloned()
            .collect();
        let all_selected = !copyable.is_empty() && copyable.iter().all(|f| modal.selected_files.contains_key(&f.path));
        for file in copyable {
            if all_selected {
                modal.selected_files.remove(&file.path);
            } else if modal.selected_files.len() < COPIED_FILES_MAX_COUNT {
                modal.selected_files.insert(file.path.clone(), file);
            }
        }
        cx.notify();
    }

    fn submit_add_to_machine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (
            source_saved_id,
            source_project_id,
            source_cwd,
            subdirectory,
            target,
            title,
            remote_url,
            destination_path,
            files_to_copy,
        ) = {
            let Some(modal) = self.add_to_machine.as_ref() else { return };
            if modal.submitting || (modal.can_list_files && modal.loading_files) {
                return;
            }
            let Some(target) = modal.targets.get(modal.selected_target_idx).cloned() else {
                return;
            };
            let destination_path = modal.dest_input.read(cx).value().trim().to_owned();
            if destination_path.is_empty() {
                if let Some(m) = self.add_to_machine.as_mut() {
                    m.error = Some("Choose a folder on the other machine.".to_owned());
                }
                cx.notify();
                return;
            }
            let ssh_url = clone_url_for_protocol(&modal.source_url, CloneProtocol::Ssh);
            let https_url = clone_url_for_protocol(&modal.source_url, CloneProtocol::Https);
            let remote_url = match modal.protocol {
                Some(CloneProtocol::Ssh) => ssh_url.unwrap_or_else(|| modal.source_url.clone()),
                Some(CloneProtocol::Https) => https_url.unwrap_or_else(|| modal.source_url.clone()),
                None => modal.source_url.clone(),
            };
            let subdirectory = project_subdirectory(modal.repository_root.as_deref(), &modal.workspace_root);
            let files: Vec<String> = modal.selected_files.keys().cloned().collect();
            (
                modal.source_saved_id.clone(),
                modal.source_project_id.clone(),
                modal.workspace_root.clone(),
                subdirectory,
                target,
                modal.project_title.clone(),
                remote_url,
                destination_path,
                files,
            )
        };

        if let Some(modal) = self.add_to_machine.as_mut() {
            modal.submitting = true;
            modal.error = None;
        }
        cx.notify();

        let active_saved = self.state.read(cx).environments.active.clone().unwrap_or_default();
        let source_route = {
            let state = self.state.read(cx);
            (state.connection_id(), state.selected_thread.clone(), state.active_draft())
        };
        let composer = self.composer.clone();
        let dialog_input = self.add_to_machine.as_ref().expect("submitting dialog").dest_input.downgrade();
        let active_client = self.state.read(cx).client_handle();
        let source_fut = (!source_saved_id.is_empty() && source_saved_id != active_saved)
            .then(|| self.state.read(cx).connections.client(&source_saved_id));
        let target_fut = (target.saved_id != active_saved)
            .then(|| self.state.read(cx).connections.client(&target.saved_id));
        let runtime = self.state.read(cx).runtime().clone();
        let state_entity = self.state.clone();

        cx.spawn_in(window, async move |this, cx| {
            let source_client = match source_fut {
                Some(fut) => fut.await,
                None => active_client.clone(),
            };
            let target_client = match target_fut {
                Some(fut) => fut.await,
                None => active_client,
            };
            let Some(target_client) = target_client else {
                let _ = this.update(cx, |this, cx| {
                    if let Some(modal) = this.add_to_machine.as_mut() {
                        modal.submitting = false;
                        modal.error = Some(format!("{} is not connected.", target.label));
                    }
                    cx.notify();
                });
                return;
            };

            // Step 1: Read chosen ignored files from the source machine before cloning (`addProjectToMachine.ts`).
            let mut reads: Vec<(String, Result<String, String>)> = Vec::with_capacity(files_to_copy.len());
            if !files_to_copy.is_empty() {
                if let Some(source_client) = source_client {
                    for rel_path in files_to_copy {
                        let client = source_client.clone();
                        let req_payload = read_project_file_request(&source_cwd, &rel_path);
                        let res = runtime
                            .spawn(async move { client.request(PROJECTS_READ_FILE, req_payload).await })
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r.map_err(|e| e.to_string()))
                            .and_then(|v| serde_json::from_value::<ProjectReadFileResult>(v).map_err(|e| e.to_string()));
                        match res {
                            Ok(file) if file.truncated => reads.push((rel_path, Err("Too large to copy.".to_owned()))),
                            Ok(file) => reads.push((rel_path, Ok(file.contents))),
                            Err(err) => reads.push((rel_path, Err(err))),
                        }
                    }
                } else {
                    for rel_path in files_to_copy {
                        reads.push((rel_path, Err("Source environment disconnected.".to_owned())));
                    }
                }
            }

            // Step 2: Start the clone on the target machine.
            let project_id = uuid();
            let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let start_payload = start_project_clone(&project_id, &title, &created_at, &remote_url, &destination_path);
            let start_client = target_client.clone();
            let started = runtime
                .spawn(async move { start_client.request(PROJECT_CLONE_START, start_payload).await })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()))
                .and_then(|v| serde_json::from_value::<ProjectCloneStartResult>(v).map_err(|e| e.to_string()));

            let started = match started {
                Ok(started) => started,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(modal) = this.add_to_machine.as_mut() {
                            modal.submitting = false;
                            modal.error = Some(error);
                        }
                        cx.notify();
                    });
                    return;
                }
            };

            let modal_current = this.update(cx, |this, cx| {
                if !this.add_to_machine.as_ref().is_some_and(|modal| {
                    modal.submitting && modal.dest_input.downgrade() == dialog_input
                }) {
                    return false;
                }
                this.add_to_machine = None;
                cx.notify();
                true
            }).unwrap_or(false);
            let total_files = reads.len();
            let clone_toast_desc = if total_files == 0 {
                started.cwd.clone()
            } else {
                let plural = if total_files == 1 { "file" } else { "files" };
                format!("{} — will copy {total_files} ignored {plural} after clone", started.cwd)
            };
            cx.update(|window, cx| {
                state_entity.update(cx, |state, cx| {
                    state.show_toast(
                        Toast::new(ToastKind::Info, format!("Cloning {title} to {}", target.label))
                            .description(clone_toast_desc),
                        cx,
                    );
                });
                let (route_current, source_draft, target_is_active, owner) = {
                    let state = state_entity.read(cx);
                    let route_current = modal_current && state.connection_id() == source_route.0
                        && state.selected_thread == source_route.1 && state.active_draft() == source_route.2;
                    let source_draft = source_route.2.as_ref().is_some_and(|draft| draft.project_id == source_project_id)
                        && (source_saved_id.is_empty() || source_saved_id == active_saved);
                    let owner = state.scoped_environment(&target.saved_id)
                        .map(|entry| entry.summary.owner.clone())
                        .filter(|owner| state.scoped_ready(owner, t3_core::auth::ORCHESTRATION_READ_SCOPE));
                    (route_current, source_draft, target.saved_id == active_saved, owner)
                };
                if !route_current || (!target_is_active && owner.is_none()) {
                    return;
                }
                if source_draft && let Some(composer) = composer.as_ref().and_then(gpui::WeakEntity::upgrade) {
                    composer.update(cx, |composer, cx| {
                        composer.retarget_draft(&source_project_id, &started.project_id, target_is_active, window, cx);
                    });
                }
                state_entity.update(cx, |state, cx| {
                    if target_is_active {
                        state.start_draft(&started.project_id, cx);
                    } else if let Some(owner) = owner {
                        state.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Draft(started.project_id.clone()), cx);
                    }
                });
            }).ok();
            if reads.is_empty() {
                return;
            }

            // Step 4: Wait for the clone to finish on the target machine, then relay each file via `projects.writeFile`.
            #[derive(Debug)]
            enum CloneWaitOutcome {
                Done,
                Failed(String),
                Cancelled,
                Disconnected,
            }

            let relay_project_id = started.project_id.clone();
            let relay_cwd = started.cwd.clone();
            let relay_client = target_client.clone();
            let relay_runtime = runtime.clone();
            let target_label = target.label.clone();

            let outcome = relay_runtime
                .spawn(async move {
                    let Ok(mut stream) = relay_client.stream(SUBSCRIBE_PROJECT_CLONES, serde_json::json!({})).await else {
                        // Older servers that predate `subscribeProjectClones` complete synchronously in `projectClone.start`.
                        return CloneWaitOutcome::Done;
                    };
                    let mut seen = false;
                    while let Some(frame) = stream.receiver.recv().await {
                        let value = match frame {
                            Ok(value) => value,
                            Err(err) => return CloneWaitOutcome::Failed(err.to_string()),
                        };
                        let snapshots = serde_json::from_value::<Vec<ProjectCloneSnapshot>>(value).unwrap_or_default();
                        if let Some(clone) = snapshots.into_iter().find(|s| s.project_id == relay_project_id) {
                            seen = true;
                            match clone.phase {
                                ProjectClonePhase::Done => return CloneWaitOutcome::Done,
                                ProjectClonePhase::Failed => {
                                    return CloneWaitOutcome::Failed(
                                        clone.error.unwrap_or_else(|| "Clone failed on target machine.".to_owned()),
                                    );
                                }
                                ProjectClonePhase::Cancelled => return CloneWaitOutcome::Cancelled,
                                ProjectClonePhase::Running | ProjectClonePhase::Unknown => {}
                            }
                        } else if seen {
                            return CloneWaitOutcome::Failed("Clone stopped before completing.".to_owned());
                        }
                    }
                    CloneWaitOutcome::Disconnected
                })
                .await
                .unwrap_or(CloneWaitOutcome::Disconnected);

            match outcome {
                CloneWaitOutcome::Done => {}
                CloneWaitOutcome::Failed(reason) => {
                    let plural = if total_files == 1 { "file" } else { "files" };
                    state_entity.update(cx, |s, cx| {
                        s.show_toast(
                            Toast::new(ToastKind::Error, format!("Clone failed on {target_label}"))
                                .description(format!("{reason} ({total_files} ignored {plural} were not copied.)")),
                            cx,
                        );
                    });
                    return;
                }
                CloneWaitOutcome::Cancelled => {
                    let plural = if total_files == 1 { "file" } else { "files" };
                    state_entity.update(cx, |s, cx| {
                        s.show_toast(
                            Toast::new(ToastKind::Warning, format!("Clone cancelled on {target_label}"))
                                .description(format!("{total_files} ignored {plural} were not copied.")),
                            cx,
                        );
                    });
                    return;
                }
                CloneWaitOutcome::Disconnected => {
                    let plural = if total_files == 1 { "file" } else { "files" };
                    state_entity.update(cx, |s, cx| {
                        s.show_toast(
                            Toast::new(ToastKind::Error, format!("Could not confirm clone on {target_label}"))
                                .description(format!("Lost clone progress stream; {total_files} ignored {plural} were not copied.")),
                            cx,
                        );
                    });
                    return;
                }
            }

            let mut failed: Vec<String> = Vec::new();
            for (rel_path, content_res) in reads {
                let contents = match content_res {
                    Ok(c) => c,
                    Err(err) => {
                        failed.push(format!("{rel_path}: {err}"));
                        continue;
                    }
                };
                let target_rel = if subdirectory.is_empty() {
                    rel_path.clone()
                } else {
                    format!("{subdirectory}/{rel_path}")
                };
                let write_client = target_client.clone();
                let write_payload = write_project_file_request(&relay_cwd, &target_rel, &contents);
                let written = runtime
                    .spawn(async move { write_client.request(PROJECTS_WRITE_FILE, write_payload).await })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                if let Err(err) = written {
                    failed.push(format!("{rel_path}: {err}"));
                }
            }

            state_entity.update(cx, |s, cx| {
                if failed.is_empty() {
                    let plural = if total_files == 1 { "file" } else { "files" };
                    s.show_toast(
                        Toast::new(ToastKind::Success, format!("Copied {total_files} ignored {plural} to {target_label}")),
                        cx,
                    );
                } else {
                    let count = failed.len();
                    let plural = if count == 1 { "file" } else { "files" };
                    s.show_toast(
                        Toast::new(ToastKind::Error, format!("Could not copy {count} {plural} to {target_label}"))
                            .description(failed.join("\n")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn render_clone_modal(&self, modal: &CloneModalState, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let (base_dir, fallback_root, _, env_label) = self.clone_base_dirs(self.clone.as_ref().and_then(|modal| modal.owner.as_ref()), cx);
        let raw_url = modal.url_input.read(cx).value().to_string();
        let dest_value = modal.dest_input.read(cx).value().to_string();
        let preview = compute_clone_preview(
            &raw_url,
            base_dir.as_deref(),
            fallback_root.as_deref(),
            modal.ghq_layout,
            modal.protocol_override,
        );
        let effective_protocol = modal.protocol_override.or(preview.protocol);
        let can_toggle_protocol = preview.ssh_url.is_some() && preview.https_url.is_some();

        let body = if let Some(active) = &modal.active_clone {
            let phase = active.snapshot.as_ref().map(|s| s.phase).unwrap_or(ProjectClonePhase::Running);
            let display = active
                .snapshot
                .as_ref()
                .map(project_clone_display_name)
                .unwrap_or_else(|| clone_directory_name(&active.remote_url));
            let summary = active
                .snapshot
                .as_ref()
                .map(project_clone_progress_summary)
                .unwrap_or_else(|| "Connecting".to_owned());
            let percent = active.snapshot.as_ref().and_then(|s| s.percent);
            let error_msg = active
                .snapshot
                .as_ref()
                .and_then(|s| s.error.clone())
                .or_else(|| active.action_error.clone());
            let failed_or_cancelled = matches!(phase, ProjectClonePhase::Failed | ProjectClonePhase::Cancelled);
            v_flex()
                .px(px(24.))
                .pb(px(20.))
                .gap(px(12.))
                .child(
                    v_flex()
                        .gap(px(4.))
                        .p(px(12.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.muted).opacity(0.4))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child(if phase == ProjectClonePhase::Cancelled {
                                    format!("Cancelled cloning {display}")
                                } else if phase == ProjectClonePhase::Failed {
                                    format!("Failed to clone {display}")
                                } else {
                                    format!("Cloning {display}")
                                }),
                        )
                        .child(
                            div()
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .truncate()
                                .child(active.destination_path.clone()),
                        )
                        .when(!failed_or_cancelled, |card| {
                            card.child(
                                div()
                                    .pt(px(4.))
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(summary),
                            )
                            .when_some(percent, |card, pct| {
                                let ratio = (pct.min(100) as f32) / 100.0;
                                card.child(
                                    div()
                                        .mt(px(4.))
                                        .w_full()
                                        .h(px(6.))
                                        .rounded_full()
                                        .bg(hsla(p.border))
                                        .overflow_hidden()
                                        .child(div().h_full().w(gpui::relative(ratio)).bg(hsla(p.primary))),
                                )
                            })
                        })
                        .when_some(error_msg, |card, msg| {
                            card.child(div().pt(px(4.)).text_xs().text_color(hsla(p.destructive_foreground)).child(msg))
                        }),
                )
                .into_any_element()
        } else {
            v_flex()
                .px(px(24.))
                .pb(px(20.))
                .gap(px(14.))
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Repository URL or owner/repo"))
                        .child(Input::new(&modal.url_input).theme_slot(ThemeSlot::Input, cx))
                        .when(!preview.remote_url.is_empty(), |col| {
                            col.child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .child(
                                        h_flex()
                                            .gap(px(6.))
                                            .items_center()
                                            .min_w_0()
                                            .flex_1()
                                            .when_some(preview.host.clone(), |row, host| {
                                                row.child(
                                                    div()
                                                        .flex_none()
                                                        .px(px(6.))
                                                        .py(px(2.))
                                                        .rounded(px(4.))
                                                        .bg(hsla(p.muted))
                                                        .border_1()
                                                        .border_color(hsla(p.border))
                                                        .text_xs()
                                                        .font_family(mono.clone())
                                                        .text_color(hsla(p.foreground))
                                                        .child(host),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .flex_1()
                                                    .truncate()
                                                    .font_family(mono.clone())
                                                    .text_xs()
                                                    .text_color(hsla(p.muted_foreground))
                                                    .child(preview.remote_url.clone()),
                                            ),
                                    )
                                    .when(can_toggle_protocol, |row| {
                                        row.child(
                                            h_flex()
                                                .flex_none()
                                                .gap(px(4.))
                                                .child(
                                                    Button::new(
                                                        "clone-proto-ssh",
                                                        if effective_protocol == Some(CloneProtocol::Ssh) {
                                                            ButtonVariant::Secondary
                                                        } else {
                                                            ButtonVariant::Ghost
                                                        },
                                                        ButtonSize::Xs,
                                                    )
                                                    .label("SSH")
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.set_clone_protocol(CloneProtocol::Ssh, window, cx);
                                                    })),
                                                )
                                                .child(
                                                    Button::new(
                                                        "clone-proto-https",
                                                        if effective_protocol == Some(CloneProtocol::Https) {
                                                            ButtonVariant::Secondary
                                                        } else {
                                                            ButtonVariant::Ghost
                                                        },
                                                        ButtonSize::Xs,
                                                    )
                                                    .label("HTTPS")
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.set_clone_protocol(CloneProtocol::Https, window, cx);
                                                    })),
                                                ),
                                        )
                                    }),
                            )
                        })
                        .when_some(preview.incomplete_hint, |col, hint| {
                            col.child(div().text_xs().text_color(hsla(p.warning_foreground)).child(hint))
                        }),
                )
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(
                            h_flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(hsla(p.foreground))
                                        .child(format!("Destination folder on {env_label}")),
                                )
                                .child(
                                    h_flex()
                                        .gap(px(4.))
                                        .child(
                                            Button::new(
                                                "clone-layout-flat",
                                                if !modal.ghq_layout { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                                                ButtonSize::Xs,
                                            )
                                            .label("Flat")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.set_clone_ghq_layout(false, window, cx);
                                            })),
                                        )
                                        .child(
                                            Button::new(
                                                "clone-layout-ghq",
                                                if modal.ghq_layout { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                                                ButtonSize::Xs,
                                            )
                                            .label("ghq")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.set_clone_ghq_layout(true, window, cx);
                                            })),
                                        )
                                        .when(modal.path_overridden, |row| {
                                            row.child(
                                                Button::new("clone-reset-dest", ButtonVariant::Ghost, ButtonSize::Xs)
                                                    .label("Reset")
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.reset_clone_destination(window, cx);
                                                    })),
                                            )
                                        }),
                                ),
                        )
                        .child(Input::new(&modal.dest_input).theme_slot(ThemeSlot::Input, cx))
                        .when(!preview.directory_path.is_empty(), |col| {
                            let layout_note = if modal.ghq_layout {
                                format!("ghq layout: {}", preview.directory_path)
                            } else {
                                format!("Folder: {}", preview.directory_path)
                            };
                            col.child(
                                div()
                                    .font_family(mono)
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .truncate()
                                    .child(layout_note),
                            )
                        }),
                )
                .when_some(modal.error.clone(), |col, err| {
                    col.child(div().text_xs().text_color(hsla(p.destructive_foreground)).child(err))
                })
                .into_any_element()
        };

        let footer = if let Some(active) = &modal.active_clone {
            let phase = active.snapshot.as_ref().map(|s| s.phase).unwrap_or(ProjectClonePhase::Running);
            let failed_or_cancelled = matches!(phase, ProjectClonePhase::Failed | ProjectClonePhase::Cancelled);
            h_flex()
                .justify_end()
                .gap(px(8.))
                .px(px(24.))
                .py(px(16.))
                .border_t_1()
                .border_color(hsla(p.border))
                .when(!failed_or_cancelled, |row| {
                    row.child(
                        Button::new("clone-cancel-active", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Cancel clone")
                            .disabled(active.action_pending)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_active_clone(cx))),
                    )
                    .child(
                        Button::new("clone-open-bg", ButtonVariant::Default, ButtonSize::Sm)
                            .label("Open project")
                            .on_click(cx.listener(|this, _, _, cx| this.open_active_clone_background(cx))),
                    )
                })
                .when(failed_or_cancelled, |row| {
                    row.child(
                        Button::new("clone-close-failed", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("clone-retry-active", ButtonVariant::Default, ButtonSize::Sm)
                            .label("Retry")
                            .disabled(active.action_pending)
                            .on_click(cx.listener(|this, _, _, cx| this.retry_active_clone(cx))),
                    )
                })
        } else {
            let can_submit = !modal.submitting
                && !preview.title.is_empty()
                && preview.incomplete_hint.is_none()
                && !dest_value.trim().is_empty();
            h_flex()
                .justify_end()
                .gap(px(8.))
                .px(px(24.))
                .py(px(16.))
                .border_t_1()
                .border_color(hsla(p.border))
                .child(
                    Button::new("clone-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                )
                .child(
                    Button::new("clone-submit", ButtonVariant::Default, ButtonSize::Sm)
                        .label(if modal.submitting { "Starting…" } else { "Clone repository" })
                        .disabled(!can_submit)
                        .on_click(cx.listener(|this, _, window, cx| this.submit_clone(window, cx))),
                )
        };

        deferred(
            div()
                .id("clone-project-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" && this.dismiss(cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .id("clone-project-dialog")
                        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                            this.close(cx);
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(576.))
                        .relative()
                        .rounded(px(16.))
                        .theme_slot(ThemeSlot::DialogPopup, cx)
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .children(crate::theme::theme_slot_clipped_background_layer(ThemeSlot::DialogPopup, px(16.), cx))
                        .shadow(vec![BoxShadow {
                            color: gpui_hsla(0., 0., 0., 0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .child(
                            v_flex()
                                .relative()
                                .child(
                                    v_flex()
                                        .p(px(24.))
                                        .pb(px(16.))
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .text_xl()
                                                .line_height(px(20.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(hsla(p.popover_foreground))
                                                .child("Clone repository"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(hsla(p.muted_foreground))
                                                .child("Clone a Git repository and open it as a project."),
                                        ),
                                )
                                .child(body)
                                .child(footer),
                        ),
                ),
        )
        .with_priority(55)
        .into_any_element()
    }

    fn render_add_to_machine_modal(&self, modal: &AddToMachineState, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let target = modal.targets.get(modal.selected_target_idx);
        let state = self.state.read(cx);
        let source_id = if modal.source_saved_id.is_empty() {
            state.environments.active.as_deref().unwrap_or_default()
        } else {
            &modal.source_saved_id
        };
        let source_machine = crate::context_chip::environment_machine_icon(state, source_id);
        let target_machine = target.map(|target| crate::context_chip::environment_machine_icon(state, &target.saved_id));
        let machine_options = modal.targets.iter().enumerate().map(|(index, target)| {
            SelectOption::new(index.to_string(), target.label.clone())
                .icon(crate::context_chip::environment_machine_icon(state, &target.saved_id))
        }).collect();
        let dest_path = modal.dest_input.read(cx).value().to_string();
        let ssh_url = clone_url_for_protocol(&modal.source_url, CloneProtocol::Ssh);
        let https_url = clone_url_for_protocol(&modal.source_url, CloneProtocol::Https);
        let remote_url = match modal.protocol {
            Some(CloneProtocol::Ssh) => ssh_url.clone().unwrap_or_else(|| modal.source_url.clone()),
            Some(CloneProtocol::Https) => https_url.clone().unwrap_or_else(|| modal.source_url.clone()),
            None => modal.source_url.clone(),
        };
        let selected_count = modal.selected_files.len();
        let selected_bytes: u64 = modal.selected_files.values().map(|f| f.byte_length).sum();
        let copyable_shown: Vec<&ProjectIgnoredFile> = modal
            .shown_files
            .iter()
            .filter(|f| f.byte_length <= COPYABLE_FILE_MAX_BYTES)
            .collect();
        let all_copyable_selected = !copyable_shown.is_empty()
            && copyable_shown.iter().all(|f| modal.selected_files.contains_key(&f.path));
        let at_limit = selected_count >= COPIED_FILES_MAX_COUNT;
        let unavailable = target.is_none() || (modal.can_list_files && modal.loading_files && modal.all_files.is_empty());

        deferred(
            div()
                .id("add-to-machine-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" && this.dismiss(cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .id("add-to-machine-dialog")
                        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                            this.close(cx);
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(576.))
                        .relative()
                        .rounded(px(16.))
                        .theme_slot(ThemeSlot::DialogPopup, cx)
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .children(crate::theme::theme_slot_clipped_background_layer(ThemeSlot::DialogPopup, px(16.), cx))
                        .shadow(vec![BoxShadow {
                            color: gpui_hsla(0., 0., 0., 0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .child(
                            v_flex()
                                .relative()
                                .child(
                                    v_flex()
                                        .p(px(24.))
                                        .pb(px(16.))
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .text_xl()
                                                .line_height(px(20.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(hsla(p.popover_foreground))
                                                .child("Add to another machine"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(hsla(p.muted_foreground))
                                                .child(format!(
                                                    "Clone {} and bring the local files git does not track.",
                                                    modal.display_name
                                                )),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .px(px(24.))
                                        .pb(px(20.))
                                        .gap(px(14.))
                                        // Source -> git clone (+ N files) -> Target summary
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap(px(12.))
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap(px(2.))
                                                        .child(
                                                            h_flex().gap(px(6.)).min_w_0()
                                                                .child(source_machine.render(px(14.), hsla(p.muted_foreground)))
                                                                .child(div()
                                                                    .min_w_0()
                                                                    .text_sm()
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .text_color(hsla(p.foreground))
                                                                    .truncate()
                                                                    .child(modal.source_label.clone())),
                                                        )
                                                        .child(
                                                            div()
                                                                .font_family(mono.clone())
                                                                .text_xs()
                                                                .text_color(hsla(p.muted_foreground))
                                                                .text_ellipsis_middle()
                                                                .child(modal.workspace_root.clone()),
                                                        ),
                                                )
                                                .child(
                                                    v_flex()
                                                        .w(px(104.))
                                                        .flex_none()
                                                        .items_center()
                                                        .gap(px(2.))
                                                        .text_xs()
                                                        .text_color(hsla(p.muted_foreground))
                                                        .child("git clone")
                                                        .child(
                                                            h_flex()
                                                                .w_full()
                                                                .items_center()
                                                                .child(div().h(px(1.)).flex_1().bg(hsla(p.border)))
                                                                .child(Icon::ArrowRight.sized(px(12.)).text_color(hsla(p.muted_foreground))),
                                                        )
                                                        .child(if selected_count == 0 {
                                                            "no files".to_owned()
                                                        } else {
                                                            format!(
                                                                "+ {selected_count} {}",
                                                                if selected_count == 1 { "file" } else { "files" }
                                                            )
                                                        }),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap(px(2.))
                                                        .child(if modal.targets.len() > 1 {
                                                            Select::new("add-to-machine-target", machine_options)
                                                                .selected(modal.selected_target_idx.to_string())
                                                                .show_check()
                                                                .disabled(modal.submitting)
                                                                .on_change({
                                                                    let view = cx.entity().downgrade();
                                                                    move |value, window, cx| {
                                                                        if let Ok(index) = value.parse() {
                                                                            let _ = view.update(cx, |this, cx| this.select_machine_target(index, window, cx));
                                                                        }
                                                                    }
                                                                })
                                                                .into_any_element()
                                                        } else {
                                                            h_flex().gap(px(6.)).min_w_0()
                                                                .children(target_machine.map(|glyph| glyph.render(px(14.), hsla(p.muted_foreground))))
                                                                .child(div()
                                                                    .min_w_0()
                                                                    .text_sm()
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .text_color(hsla(p.foreground))
                                                                    .truncate()
                                                                    .child(target.map_or_else(|| crate::i18n::t(cx, "No machine available").to_owned(), |target| target.label.clone())))
                                                                .into_any_element()
                                                        })
                                                        .child(
                                                            div()
                                                                .font_family(mono.clone())
                                                                .text_xs()
                                                                .text_color(hsla(p.muted_foreground))
                                                                .text_ellipsis_middle()
                                                                .child(dest_path),
                                                        ),
                                                ),
                                        )
                                        // Clone over (SSH / HTTPS)
                                        .child(
                                            v_flex()
                                                .gap(px(6.))
                                                .child(
                                                    h_flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .font_weight(FontWeight::MEDIUM)
                                                                .text_color(hsla(p.foreground))
                                                                .child("Clone over"),
                                                        )
                                                        .when(ssh_url.is_some() && https_url.is_some(), |row| {
                                                            row.child(
                                                                h_flex()
                                                                    .gap(px(4.))
                                                                    .child(
                                                                        Button::new(
                                                                            "relay-proto-ssh",
                                                                            if modal.protocol == Some(CloneProtocol::Ssh) {
                                                                                ButtonVariant::Secondary
                                                                            } else {
                                                                                ButtonVariant::Ghost
                                                                            },
                                                                            ButtonSize::Xs,
                                                                        )
                                                                        .label("SSH")
                                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                                            if let Some(m) = this.add_to_machine.as_mut() {
                                                                                m.protocol = Some(CloneProtocol::Ssh);
                                                                            }
                                                                            cx.notify();
                                                                        })),
                                                                    )
                                                                    .child(
                                                                        Button::new(
                                                                            "relay-proto-https",
                                                                            if modal.protocol == Some(CloneProtocol::Https) {
                                                                                ButtonVariant::Secondary
                                                                            } else {
                                                                                ButtonVariant::Ghost
                                                                            },
                                                                            ButtonSize::Xs,
                                                                        )
                                                                        .label("HTTPS")
                                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                                            if let Some(m) = this.add_to_machine.as_mut() {
                                                                                m.protocol = Some(CloneProtocol::Https);
                                                                            }
                                                                            cx.notify();
                                                                        })),
                                                                    ),
                                                            )
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .font_family(mono.clone())
                                                        .text_xs()
                                                        .text_color(hsla(p.muted_foreground))
                                                        .truncate()
                                                        .child(remote_url),
                                                ),
                                        )
                                        // Folder on target machine
                                        .child(
                                            v_flex()
                                                .gap(px(6.))
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(hsla(p.foreground))
                                                        .child(format!(
                                                            "Folder on {}",
                                                            target.map(|t| t.label.as_str()).unwrap_or("the other machine")
                                                        )),
                                                )
                                                .child(Input::new(&modal.dest_input).theme_slot(ThemeSlot::Input, cx)),
                                        )
                                        // Copy ignored files
                                        .when(modal.can_list_files, |col| {
                                            col.child(
                                                v_flex()
                                                    .gap(px(6.))
                                                    .child(
                                                        h_flex()
                                                            .items_center()
                                                            .gap(px(8.))
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .text_xs()
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .text_color(hsla(p.foreground))
                                                                    .child("Copy ignored files"),
                                                            )
                                                            .when(!copyable_shown.is_empty(), |row| {
                                                                let summary = if selected_bytes > 0 {
                                                                    format!(
                                                                        "{selected_count} of {} · {}",
                                                                        modal.all_files.len(),
                                                                        format_bytes(selected_bytes)
                                                                    )
                                                                } else {
                                                                    format!("{selected_count} of {}", modal.all_files.len())
                                                                };
                                                                row.child(
                                                                    div()
                                                                        .text_xs()
                                                                        .text_color(hsla(p.muted_foreground))
                                                                        .child(summary),
                                                                )
                                                                .child(
                                                                    Button::new("relay-toggle-all", ButtonVariant::Ghost, ButtonSize::Xs)
                                                                        .label(if all_copyable_selected { "None" } else { "All" })
                                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                                            this.toggle_all_shown_ignored_files(cx);
                                                                        })),
                                                                )
                                                            }),
                                                    )
                                                    .when(modal.truncated || !modal.file_query.is_empty(), |col| {
                                                        col.child(Input::new(&modal.search_input).theme_slot(ThemeSlot::Input, cx))
                                                    })
                                                    .child(if let Some(err) = &modal.files_error {
                                                        div()
                                                            .text_xs()
                                                            .text_color(hsla(p.destructive_foreground))
                                                            .child(err.clone())
                                                            .into_any_element()
                                                    } else if modal.shown_files.is_empty() {
                                                        div()
                                                            .text_xs()
                                                            .text_color(hsla(p.muted_foreground))
                                                            .child(if modal.loading_files {
                                                                "Finding ignored files…"
                                                            } else if !modal.file_query.trim().is_empty() {
                                                                "No ignored files match."
                                                            } else {
                                                                "No ignored files to copy."
                                                            })
                                                            .into_any_element()
                                                    } else {
                                                        let row_count = modal.shown_files.len();
                                                        div()
                                                            .id("ignored-files-list")
                                                            .max_h(px(208.))
                                                            .overflow_y_scroll()
                                                            .rounded(px(6.))
                                                            .border_1()
                                                            .border_color(hsla(p.border))
                                                            .children(modal.shown_files.iter().enumerate().map(|(ix, file)| {
                                                                let checked = modal.selected_files.contains_key(&file.path);
                                                                let too_large = file.byte_length > COPYABLE_FILE_MAX_BYTES;
                                                                let disabled = too_large || (at_limit && !checked);
                                                                let file_clone = file.clone();
                                                                h_flex()
                                                                    .id(("ignored-file-row", ix))
                                                                    .items_center()
                                                                    .gap(px(8.))
                                                                    .px(px(10.))
                                                                    .py(px(6.))
                                                                    .when(ix + 1 < row_count, |r| {
                                                                        r.border_b_1().border_color(hsla(p.border).opacity(0.6))
                                                                    })
                                                                    .when(disabled, |r| r.opacity(0.64))
                                                                    .when(!disabled, |r| {
                                                                        r.cursor_pointer()
                                                                            .hover(|s| s.bg(hsla(p.accent).opacity(0.5)))
                                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                                this.toggle_ignored_file(file_clone.clone(), cx);
                                                                            }))
                                                                    })
                                                                    .child(
                                                                        div()
                                                                            .size(px(14.))
                                                                            .flex_none()
                                                                            .flex()
                                                                            .items_center()
                                                                            .justify_center()
                                                                            .rounded(px(3.))
                                                                            .border_1()
                                                                            .border_color(if checked {
                                                                                hsla(p.primary)
                                                                            } else {
                                                                                hsla(p.input)
                                                                            })
                                                                            .when(checked, |b| b.bg(hsla(p.primary)))
                                                                            .when(checked, |b| {
                                                                                b.child(Icon::Check.sized(px(10.)).text_color(hsla(p.primary_foreground)))
                                                                            }),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .flex_1()
                                                                            .min_w_0()
                                                                            .truncate()
                                                                            .font_family(mono.clone())
                                                                            .text_xs()
                                                                            .text_color(hsla(p.foreground))
                                                                            .child(file.path.clone()),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .flex_none()
                                                                            .text_xs()
                                                                            .text_color(hsla(p.muted_foreground))
                                                                            .child(if too_large {
                                                                                "Over 1 MB".to_owned()
                                                                            } else {
                                                                                format_bytes(file.byte_length)
                                                                            }),
                                                                    )
                                                            }))
                                                            .into_any_element()
                                                    })
                                                    .when(modal.truncated, |col| {
                                                        col.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(hsla(p.muted_foreground))
                                                                .child(format!(
                                                                    "Showing the first {} files. Search to find the rest.",
                                                                    modal.shown_files.len()
                                                                )),
                                                        )
                                                    })
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(hsla(p.muted_foreground))
                                                            .child("Files are copied after the clone finishes. Folders git ignores whole, like node_modules, are left out."),
                                                    ),
                                            )
                                        })
                                        .when_some(modal.error.clone(), |col, err| {
                                            col.child(div().text_sm().text_color(hsla(p.destructive_foreground)).child(err))
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap(px(8.))
                                        .px(px(24.))
                                        .py(px(16.))
                                        .border_t_1()
                                        .border_color(hsla(p.border))
                                        .child(
                                            Button::new("add-to-machine-cancel", ButtonVariant::Outline, ButtonSize::Sm)
                                                .label("Cancel")
                                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                                        )
                                        .child(
                                            Button::new("add-to-machine-submit", ButtonVariant::Default, ButtonSize::Sm)
                                                .label(if modal.submitting {
                                                    "Starting…".to_owned()
                                                } else {
                                                    format!(
                                                        "Clone to {}",
                                                        target.map(|t| t.label.as_str()).unwrap_or("machine")
                                                    )
                                                })
                                                .disabled(unavailable || modal.submitting)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.submit_add_to_machine(window, cx);
                                                })),
                                        ),
                                ),
                        ),
                ),
        )
        // The shared Select defers its popup at priority 2.
        .with_priority(1)
        .into_any_element()
    }
}

impl Render for ProjectDialogs {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(modal) = &self.clone {
            return self.render_clone_modal(modal, cx);
        }
        if let Some(modal) = &self.add_to_machine {
            return self.render_add_to_machine_modal(modal, cx);
        }
        div().into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::size;
    use serde_json::json;

    #[gpui::test]
    async fn interactive_clone_modal_previews_ghq_layout_and_tracks_progress_and_error(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        state.update(cx, |s, _| {
            s.server_config.config = Some(
                serde_json::from_value(json!({
                    "environment": { "environmentId": "fixture-env", "label": "Fixture", "capabilities": {} },
                    "providers": [],
                    "settings": {
                        "addProjectBaseDirectory": "~/src",
                        "ghqCloneLayout": true
                    },
                    "issues": [],
                    "keybindings": []
                }))
                .unwrap(),
            );
        });

        let (dialogs, vcx) = cx.add_window_view(|window, cx| ProjectDialogs::new(state.clone(), window, cx));
        vcx.update(|window, _| {
            window.resize(size(px(960.), px(680.)));
        });

        // Trigger clone modal via AppState::add_project with shorthand URL.
        vcx.update(|window, cx| {
            state.update(cx, |s, cx| s.add_project("pingdotgg/t3code", cx));
            // Blur the native macOS `Input` so `TestWindow` renders without invoking
            // `TestWindow::window_handle` in `set_text_content_type`.
            window.blur();
        });
        vcx.run_until_parked();
        vcx.update(|window, _| window.blur());
        vcx.read_entity(&dialogs, |d, cx| {
            let modal = d.clone.as_ref().expect("clone modal must open");
            assert_eq!(modal.dest_input.read(cx).value().as_str(), "~/src/github.com/pingdotgg/t3code");
            assert!(modal.ghq_layout);
        });

        // Switch protocol to SSH and submit; first fail synchronously to verify inline error state.
        vcx.update(|window, cx| {
            window.blur();
            dialogs.update(cx, |d, cx| {
                d.set_clone_protocol(CloneProtocol::Ssh, window, cx);
                d.submit_clone(window, cx);
            });
        });
        vcx.run_until_parked();
        let (method, payload, reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECT_CLONE_START {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(method, PROJECT_CLONE_START);
        assert_eq!(payload["remoteUrl"], "git@github.com:pingdotgg/t3code.git");
        assert_eq!(payload["destinationPath"], "~/src/github.com/pingdotgg/t3code");
        let (err_tx, err_rx) = futures::channel::oneshot::channel::<()>();
        let err_tx = std::cell::RefCell::new(Some(err_tx));
        let _err_sub = vcx.update(|_, cx| {
            cx.observe(&dialogs, move |d, cx| {
                if d.read(cx).clone.as_ref().and_then(|m| m.error.as_ref()).is_some()
                    && let Some(tx) = err_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        reply
            .send(Err(t3_client::session::RpcError::Failure(t3_core::rpc::RpcFailure {
                tag: None,
                message: "destination already exists".into(),
                error: serde_json::Value::Null,
            })))
            .unwrap();
        err_rx.await.unwrap();
        vcx.run_until_parked();
        vcx.read_entity(&dialogs, |d, _| {
            let modal = d.clone.as_ref().expect("modal stays open on error");
            assert!(modal.error.as_deref().is_some_and(|e| e.contains("destination already exists")));
            assert!(modal.active_clone.is_none());
        });

        // Re-submit and succeed -> enters live progress tracking.
        vcx.update(|window, cx| {
            dialogs.update(cx, |d, cx| d.submit_clone(window, cx));
        });
        vcx.run_until_parked();
        let (_, _, reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECT_CLONE_START {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        let (active_tx, active_rx) = futures::channel::oneshot::channel::<()>();
        let active_tx = std::cell::RefCell::new(Some(active_tx));
        let _active_sub = vcx.update(|_, cx| {
            cx.observe(&dialogs, move |d, cx| {
                if d.read(cx).clone.as_ref().and_then(|m| m.active_clone.as_ref()).is_some()
                    && let Some(tx) = active_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        reply
            .send(Ok(json!({
                "projectId": "proj-cloned-1",
                "cwd": "/Users/me/src/github.com/pingdotgg/t3code",
                "remoteUrl": "git@github.com:pingdotgg/t3code.git"
            })))
            .unwrap();
        active_rx.await.unwrap();
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.blur();
            window.draw(cx).clear();
        });

        // Prematurely closing `subscribeProjectClones` before a terminal phase sets an inline error
        // instead of leaving `active_clone` stuck spinning.
        let (stream_err_tx, stream_err_rx) = futures::channel::oneshot::channel::<()>();
        let stream_err_tx = std::cell::RefCell::new(Some(stream_err_tx));
        let _stream_err_sub = vcx.update(|_, cx| {
            cx.observe(&dialogs, move |d, cx| {
                if d.read(cx)
                    .clone
                    .as_ref()
                    .and_then(|m| m.active_clone.as_ref())
                    .and_then(|a| a.action_error.as_ref())
                    .is_some()
                    && let Some(tx) = stream_err_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        let stream_reply = loop {
            let (m, r) = fixture.next_stream().await;
            if m == SUBSCRIBE_PROJECT_CLONES {
                break r;
            }
        };
        let (stream_handle, stream_tx) = t3_client::session::StreamHandle::fixture();
        assert!(stream_reply.send(Ok(stream_handle)).is_ok());
        drop(stream_tx);
        stream_err_rx.await.unwrap();
        vcx.read_entity(&dialogs, |d, _| {
            let err = d
                .clone
                .as_ref()
                .and_then(|m| m.active_clone.as_ref())
                .and_then(|a| a.action_error.as_deref());
            assert!(
                err.is_some_and(|msg| msg.contains("Lost connection")),
                "premature stream close must surface an error, got {err:?}"
            );
        });

        // Escape dismisses the modal even while in `active_clone` progress view (no `Input` focused).
        vcx.update(|_, cx| {
            assert!(dialogs.update(cx, |d, cx| d.dismiss(cx)));
        });
        vcx.read_entity(&dialogs, |d, _| {
            assert!(!d.is_open(), "dismiss closes active clone progress modal");
        });

        drop(_err_sub);
        drop(_active_sub);
        drop(_stream_err_sub);
        cx.update(|cx| {
            state.update(cx, |s, _| s.stop_pump());
        });
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    async fn add_project_to_machine_dialog_lists_and_relays_ignored_files(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        state.update(cx, |s, _| {
            let mut snapshot = s.shell.snapshot.clone().unwrap_or_default();
            snapshot.projects.push(
                serde_json::from_value(json!({
                    "id": "proj-src",
                    "title": "web",
                    "workspaceRoot": "/repo/apps/web",
                    "repositoryIdentity": {
                        "canonicalKey": "github.com/pingdotgg/t3code",
                        "displayName": "pingdotgg/t3code",
                        "rootPath": "/repo",
                        "origin": { "remoteUrl": "https://github.com/pingdotgg/t3code.git" }
                    },
                    "createdAt": "2026-01-01T00:00:00Z",
                    "updatedAt": "2026-01-02T00:00:00Z"
                }))
                .unwrap(),
            );
            s.shell.snapshot = Some(snapshot);
            s.shell.synchronized = true;
        });

        let (dialogs, vcx) = cx.add_window_view(|window, cx| ProjectDialogs::new(state.clone(), window, cx));
        vcx.update(|window, cx| {
            window.resize(size(px(960.), px(680.)));
            dialogs.update(cx, |d, cx| {
                d.open_add_to_machine_modal(String::new(), "proj-src".into(), None, window, cx);
                // Attach a target machine matching the active fixture for deterministic RPC testing.
                if let Some(m) = d.add_to_machine.as_mut() {
                    m.targets.push(MachineTarget {
                        saved_id: state.read(cx).environments.active.clone().unwrap_or_default(),
                        environment_id: "env-target".into(),
                        label: "Studio".into(),
                        add_project_base_directory: Some("~/code".into()),
                        ghq_clone_layout: true,
                    });
                }
                d.select_machine_target(0, window, cx);
            });
            window.blur();
        });
        vcx.run_until_parked();

        // 1. Answer `projects.listIgnoredFiles` with a default `.env.local`, an over-1MB file, and a regular ignored file.
        let (list_method, list_payload, list_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECTS_LIST_IGNORED_FILES {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(list_method, PROJECTS_LIST_IGNORED_FILES);
        assert_eq!(list_payload["cwd"], "/repo/apps/web");
        let (listed_tx, listed_rx) = futures::channel::oneshot::channel::<()>();
        let listed_tx = std::cell::RefCell::new(Some(listed_tx));
        let _listed_sub = vcx.update(|_, cx| {
            cx.observe(&dialogs, move |d, cx| {
                if d.read(cx).add_to_machine.as_ref().is_some_and(|m| !m.loading_files)
                    && let Some(tx) = listed_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        list_reply
            .send(Ok(json!({
                "files": [
                    { "path": ".env.local", "byteLength": 128 },
                    { "path": "huge.bin", "byteLength": 2097152 },
                    { "path": "local.notes", "byteLength": 64 }
                ],
                "truncated": false
            })))
            .unwrap();
        listed_rx.await.unwrap();
        vcx.run_until_parked();

        vcx.read_entity(&dialogs, |d, cx| {
            let m = d.add_to_machine.as_ref().expect("add-to-machine dialog open");
            assert_eq!(m.dest_input.read(cx).value().as_str(), "~/code/github.com/pingdotgg/t3code");
            assert!(m.selected_files.contains_key(".env.local"), ".env.local auto-selected");
            assert!(!m.selected_files.contains_key("huge.bin"), "over-1MB file excluded");
            assert!(!m.selected_files.contains_key("local.notes"), "non-env file unchecked by default");
        });

        // 2. Render dialog and submit -> reads `.env.local`, starts clone, writes `apps/web/.env.local`.
        vcx.update(|window, cx| {
            window.blur();
            window.draw(cx).clear();
            dialogs.update(cx, |d, cx| d.submit_add_to_machine(window, cx));
        });
        vcx.run_until_parked();

        let (read_method, read_payload, read_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECTS_READ_FILE {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(read_method, PROJECTS_READ_FILE);
        assert_eq!(read_payload["relativePath"], ".env.local");
        read_reply
            .send(Ok(json!({
                "relativePath": ".env.local",
                "contents": "API_KEY=secret\n",
                "byteLength": 15,
                "truncated": false
            })))
            .unwrap();

        let (clone_method, clone_payload, clone_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECT_CLONE_START {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(clone_method, PROJECT_CLONE_START);
        assert_eq!(clone_payload["destinationPath"], "~/code/github.com/pingdotgg/t3code");
        clone_reply
            .send(Ok(json!({
                "projectId": "proj-dst",
                "cwd": "/studio/code/github.com/pingdotgg/t3code",
                "remoteUrl": "https://github.com/pingdotgg/t3code.git"
            })))
            .unwrap();

        // Fixture drops stream commands so `subscribeProjectClones` returns Closed -> relay proceeds directly to `projects.writeFile`.
        let (write_method, write_payload, write_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECTS_WRITE_FILE {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(write_method, PROJECTS_WRITE_FILE);
        assert_eq!(write_payload["cwd"], "/studio/code/github.com/pingdotgg/t3code");
        assert_eq!(write_payload["relativePath"], "apps/web/.env.local");
        assert_eq!(write_payload["contents"], "API_KEY=secret\n");
        let (toast_tx, toast_rx) = futures::channel::oneshot::channel::<()>();
        let toast_tx = std::cell::RefCell::new(Some(toast_tx));
        let _toast_sub = vcx.update(|_, cx| {
            cx.subscribe(&state, move |s, _event: &AppEvent, cx| {
                if s.read(cx).toasts.latest().is_some_and(|t| t.title == "Copied 1 ignored file to Studio")
                    && let Some(tx) = toast_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        write_reply.send(Ok(json!({ "relativePath": "apps/web/.env.local" }))).unwrap();
        toast_rx.await.unwrap();

        vcx.read_entity(&state, |s, _| {
            let latest = s.toasts.latest().expect("relay completion toast");
            assert_eq!(latest.kind, ToastKind::Success);
            assert_eq!(latest.title, "Copied 1 ignored file to Studio");
        });

        drop(_listed_sub);
        drop(_toast_sub);
        cx.update(|cx| {
            state.update(cx, |s, _| s.stop_pump());
        });
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    async fn add_to_machine_stale_remote_search_and_failed_relay_clone_toast(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        state.update(cx, |s, _| {
            let mut snapshot = s.shell.snapshot.clone().unwrap_or_default();
            snapshot.projects.push(
                serde_json::from_value(json!({
                    "id": "proj-src",
                    "title": "web",
                    "workspaceRoot": "/repo/apps/web",
                    "repositoryIdentity": {
                        "canonicalKey": "github.com/pingdotgg/t3code",
                        "displayName": "pingdotgg/t3code",
                        "rootPath": "/repo",
                        "origin": { "remoteUrl": "https://github.com/pingdotgg/t3code.git" }
                    },
                    "createdAt": "2026-01-01T00:00:00Z",
                    "updatedAt": "2026-01-02T00:00:00Z"
                }))
                .unwrap(),
            );
            s.shell.snapshot = Some(snapshot);
            s.shell.synchronized = true;
        });

        let (dialogs, vcx) = cx.add_window_view(|window, cx| ProjectDialogs::new(state.clone(), window, cx));
        vcx.update(|window, cx| {
            window.resize(size(px(960.), px(680.)));
            dialogs.update(cx, |d, cx| {
                d.open_add_to_machine_modal(String::new(), "proj-src".into(), None, window, cx);
                if let Some(m) = d.add_to_machine.as_mut() {
                    m.targets.push(MachineTarget {
                        saved_id: state.read(cx).environments.active.clone().unwrap_or_default(),
                        environment_id: "env-target".into(),
                        label: "Studio".into(),
                        add_project_base_directory: Some("~/code".into()),
                        ghq_clone_layout: true,
                    });
                }
                d.select_machine_target(0, window, cx);
            });
            window.blur();
        });
        vcx.run_until_parked();

        // 1. Answer initial listing with `truncated: true` so queries trigger remote searches.
        let (_, _, initial_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECTS_LIST_IGNORED_FILES {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        let (listed_tx, listed_rx) = futures::channel::oneshot::channel::<()>();
        let listed_tx = std::cell::RefCell::new(Some(listed_tx));
        let _listed_sub = vcx.update(|_, cx| {
            cx.observe(&dialogs, move |d, cx| {
                if d.read(cx).add_to_machine.as_ref().is_some_and(|m| !m.loading_files)
                    && let Some(tx) = listed_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        initial_reply
            .send(Ok(json!({
                "files": [
                    { "path": ".env.local", "byteLength": 128 },
                    { "path": "local.notes", "byteLength": 64 }
                ],
                "truncated": true
            })))
            .unwrap();
        listed_rx.await.unwrap();

        // 2. Start a remote search, then clear the query BEFORE the remote response arrives.
        vcx.update(|window, cx| {
            dialogs.update(cx, |d, cx| d.on_ignored_file_query_changed("secret".into(), window, cx));
        });
        vcx.run_until_parked();
        let (_, remote_payload, stale_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECTS_LIST_IGNORED_FILES {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        assert_eq!(remote_payload["query"], "secret");

        vcx.update(|window, cx| {
            dialogs.update(cx, |d, cx| d.on_ignored_file_query_changed(String::new(), window, cx));
        });
        // Deliver the stale remote response; it must be discarded and must not overwrite `shown_files`.
        let _ = stale_reply.send(Ok(json!({
            "files": [{ "path": "secret.txt", "byteLength": 32 }],
            "truncated": false
        })));
        vcx.run_until_parked();
        vcx.read_entity(&dialogs, |d, _| {
            let m = d.add_to_machine.as_ref().expect("dialog open");
            assert!(!m.loading_files, "clearing search clears loading_files");
            let paths: Vec<&str> = m.shown_files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(paths, vec![".env.local", "local.notes"]);
        });

        // 3. Submit relay, observe "Cloning web to Studio" start toast, then push a Failed clone snapshot
        //    and assert an error toast reports both the clone failure and skipped ignored files.
        let (started_toast_tx, started_toast_rx) = futures::channel::oneshot::channel::<()>();
        let started_toast_tx = std::cell::RefCell::new(Some(started_toast_tx));
        let (failed_toast_tx, failed_toast_rx) = futures::channel::oneshot::channel::<(ToastKind, String, Option<String>)>();
        let failed_toast_tx = std::cell::RefCell::new(Some(failed_toast_tx));
        let _toast_sub = vcx.update(|_, cx| {
            cx.subscribe(&state, move |s, _event: &AppEvent, cx| {
                if let Some(latest) = s.read(cx).toasts.latest() {
                    if latest.title == "Cloning web to Studio"
                        && let Some(tx) = started_toast_tx.borrow_mut().take()
                    {
                        let _ = tx.send(());
                    }
                    if latest.title == "Clone failed on Studio"
                        && let Some(tx) = failed_toast_tx.borrow_mut().take()
                    {
                        let _ = tx.send((
                            latest.kind,
                            latest.title.to_string(),
                            latest.description.as_ref().map(|d| d.to_string()),
                        ));
                    }
                }
            })
        });

        vcx.update(|window, cx| {
            dialogs.update(cx, |d, cx| d.submit_add_to_machine(window, cx));
        });
        vcx.run_until_parked();

        let (_, _, read_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECTS_READ_FILE {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        read_reply
            .send(Ok(json!({
                "relativePath": ".env.local",
                "contents": "SECRET=1\n",
                "byteLength": 9,
                "truncated": false
            })))
            .unwrap();

        let (_, _, clone_reply) = loop {
            let (m, p, r) = fixture.next_request().await;
            if m == PROJECT_CLONE_START {
                break (m, p, r);
            }
            let _ = r.send(Ok(json!({})));
        };
        clone_reply
            .send(Ok(json!({
                "projectId": "proj-fail",
                "cwd": "/studio/code/github.com/pingdotgg/t3code",
                "remoteUrl": "https://github.com/pingdotgg/t3code.git"
            })))
            .unwrap();
        started_toast_rx.await.unwrap();
        vcx.run_until_parked();

        let stream_reply = loop {
            let (m, r) = fixture.next_stream().await;
            if m == SUBSCRIBE_PROJECT_CLONES {
                break r;
            }
        };
        let (stream_handle, stream_tx) = t3_client::session::StreamHandle::fixture();
        assert!(stream_reply.send(Ok(stream_handle)).is_ok());
        stream_tx
            .send(Ok(json!([{
                "projectId": "proj-fail",
                "remoteUrl": "https://github.com/pingdotgg/t3code.git",
                "destinationPath": "/studio/code/github.com/pingdotgg/t3code",
                "phase": "failed",
                "stage": "connecting",
                "error": "auth failed"
            }])))
            .await
            .unwrap();

        let (kind, title, desc) = failed_toast_rx.await.unwrap();
        assert_eq!(kind, ToastKind::Error);
        assert_eq!(title, "Clone failed on Studio");
        assert!(
            desc.as_deref()
                .is_some_and(|d| d.contains("auth failed") && d.contains("1 ignored file")),
            "failed clone toast must report error and skipped files, got {desc:?}"
        );
        let (remaining_reqs, _) = fixture.take_requests();
        assert!(
            !remaining_reqs.iter().any(|(m, _, _)| *m == PROJECTS_WRITE_FILE),
            "must not write ignored files when clone failed"
        );

        drop(_listed_sub);
        drop(_toast_sub);
        cx.update(|cx| {
            state.update(cx, |s, _| s.stop_pump());
        });
        drop(fixture);
        runtime.shutdown_background();
    }
}
