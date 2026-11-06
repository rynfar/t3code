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
        self.show_toast(Toast::new(ToastKind::Success, "Path copied").description(path.to_owned()), cx);
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
