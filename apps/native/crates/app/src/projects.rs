//! Project actions: add an existing folder (`project.create` through `projects.mutate`), create a
//! new one (`projects.createNew`), pin and unpin (`useProjectPin`), copy its path, start a thread
//! in it, and remove it (`project.delete` after Settings → Projects confirms). Shared by the
//! palette's Add project flows, the project menu and the Projects settings page.

use gpui::{ClipboardItem, Context};
use t3_core::projects::{CreateNewResult, PROJECTS_CREATE_NEW, PROJECTS_MUTATE, add_target, create_new_project, create_project, find_project_by_path, infer_project_title, set_project_pinned};
use t3_core::settings::projects::delete_project;

use crate::app_state::AppState;
use crate::sidebar::controller::uuid;
use crate::toast::{Toast, ToastKind};

impl AppState {
    pub(crate) fn run_scoped_project_action(&mut self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, action: t3_core::thread_actions::ProjectAction, cx: &mut Context<Self>) {
        let Some(target) = t3_core::sidebar::grouping::pick_project_target(group, None) else { return; };
        match action {
            t3_core::thread_actions::ProjectAction::NewThread => self.navigate_scoped(target.project.owner.clone(), crate::app_state::ScopedRouteAction::Draft(target.project.project.id.clone()), cx),
            t3_core::thread_actions::ProjectAction::Settings => self.navigate_scoped(target.project.owner.clone(), crate::app_state::ScopedRouteAction::ProjectSettings(target.project.project.id.clone()), cx),
            t3_core::thread_actions::ProjectAction::CopyPath => self.copy_project_scoped(&target.project.project.workspace_root, cx),
            t3_core::thread_actions::ProjectAction::Pin => self.pin_scoped_project_group(group, true, cx),
            t3_core::thread_actions::ProjectAction::Unpin => self.pin_scoped_project_group(group, false, cx),
        }
    }

    fn copy_project_scoped(&mut self, path: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(path.to_owned()));
        self.show_toast(Toast::new(ToastKind::Success, "Path copied").description(path.to_owned()), cx);
    }

    pub(crate) fn pin_scoped_project_group(&mut self, group: &t3_core::sidebar::grouping::ScopedProjectGroup, pinned: bool, cx: &mut Context<Self>) {
        let refs = group.mutation_refs();
        let members: Option<Vec<_>> = refs.iter().map(|key| self.scoped_project(key).cloned()).collect();
        let Some(members) = members else { return; };
        if members.iter().any(|project| !self.scoped_ready(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)) { return; }
        let lookups: Vec<_> = members.iter().map(|project| self.connections.destination(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)).collect();
        cx.spawn(async move |this, cx| {
            let mut destinations = Vec::new();
            for lookup in lookups {
                let Ok(destination) = lookup.await else { return; };
                destinations.push(destination);
            }
            let preflight = this.read_with(cx, |state, _| members.iter().zip(&destinations).all(|(project, destination)| destination.is_current(&state.fleet)
                && state.scoped_ready(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
                && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
                && state.scoped_project(&project.reference()).is_some_and(|current| current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root))).unwrap_or(false);
            if !preflight { return; }
            for (index, (project, destination)) in members.iter().zip(destinations).enumerate() {
                let valid = this.read_with(cx, |state, _| destination.is_current(&state.fleet) && state.scoped_ready(&project.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
                    && destination.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
                    && state.scoped_project(&project.reference()).is_some_and(|current| current.project.created_at == project.project.created_at && current.project.workspace_root == project.project.workspace_root)).unwrap_or(false);
                if !valid {
                    let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Project pin stopped").description(format!("{index} checkout(s) changed before this destination became unavailable.")), cx));
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

    /// Adds the folder a browse query names, or opens a draft in the project already there.
    pub fn add_project(&mut self, query: &str, cx: &mut Context<Self>) {
        let path = add_target(query);
        if path.is_empty() {
            return;
        }
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

    /// `projects.createNew`: the server makes the folder under its new-projects root.
    pub fn create_new_project(&mut self, name: &str, cx: &mut Context<Self>) {
        if name.trim().is_empty() {
            return;
        }
        let request = self.rpc(PROJECTS_CREATE_NEW, create_new_project(name));
        cx.spawn(async move |this, cx| {
            let result = request.await.map_err(|error| error.to_string()).and_then(|value| serde_json::from_value::<CreateNewResult>(value).map_err(|error| error.to_string()));
            let _ = this.update(cx, |state, cx| match result {
                Ok(created) => {
                    let toast = Toast::new(ToastKind::Success, "Project created").description(created.workspace_root.clone());
                    state.show_toast(created.commit_error.map_or(toast, |error| Toast::new(ToastKind::Warning, "Project created without a first commit").description(error)), cx);
                    state.start_draft(&created.project_id, cx);
                }
                Err(error) => state.show_toast(Toast::new(ToastKind::Error, "Failed to create project").description(error), cx),
            });
        })
        .detach();
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
        cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
        self.show_toast(Toast::new(ToastKind::Success, "Path copied").description(path), cx);
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
