//! Draft workspace menu and previous entry from `BranchToolbar.tsx`,
//! `BranchToolbarEnvModeSelector.tsx` and `PreviousWorktreeItemContent.tsx`.

use gpui::{AnyElement, App, Context, IntoElement, ParentElement, Styled, Task, Window, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::commands::method;
use t3_core::server_config::ThreadEnvMode;
use t3_core::sidebar::grouping::{EnvironmentOwner, GroupConsumer};
use t3_core::vcs::VcsRef;
use t3_core::workspace::{self as rules, DraftWorkspace, PreviousThread, PreviousWorktreeSeed};

use super::controls::{ComposerControl, Float, MenuItem, floating, menu_surface};
use super::{Composer, Target, drafts};
use super::menu::MenuKey;
use crate::app_state::ScopedRouteAction;
use crate::icons::Icon;
use crate::new_thread::environment_machine_icon;
use crate::theme::{hsla, palette};

#[derive(Clone, PartialEq, Eq)]
struct Scope {
    environment: String,
    project: String,
    root: String,
    connection: u64,
    generation: u64,
    destination: (String, String),
}

#[derive(Default)]
pub(super) struct Workspace {
    scope: Option<Scope>,
    is_repo: bool,
    refs: Vec<VcsRef>,
    default_start_from_origin: bool,
    default_mode: Option<ThreadEnvMode>,
    previous: Option<PreviousWorktreeSeed>,
    pub(super) open: bool,
    highlighted: usize,
    pub(super) branch_open: bool,
    branch_highlighted: usize,
    pub(super) host_open: bool,
    host_highlighted: usize,
    loading: bool,
    error: Option<String>,
    task: Option<Task<()>>,
}

#[derive(Clone)]
struct HostOption {
    owner: Option<EnvironmentOwner>,
    saved_id: String,
    project_id: String,
    label: String,
    is_primary: bool,
}

impl Composer {
    fn workspace_choice(&self) -> Option<&DraftWorkspace> {
        let scope = self.workspace.scope.as_ref()?;
        self.drafts.store.get(&drafts::key(&Target::Draft(scope.project.clone())))?.workspace.as_ref().filter(|choice| choice.in_scope(&scope.environment, &scope.root) && choice.destination.as_ref().is_none_or(|destination| destination == &scope.destination))
    }

    pub(super) fn workspace_is_repo(&self) -> bool {
        self.workspace.is_repo
    }

    pub(super) fn selected_workspace(&self) -> Option<DraftWorkspace> {
        self.workspace_choice().cloned()
    }

    pub(super) fn sync_workspace(&mut self, reload: bool, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let scope = (|| {
            if self.fixed.is_some() {
                return None;
            }
            let Target::Draft(project) = self.snapshot.target.as_ref()? else {
                return None;
            };
            Some(Scope {
                environment: state.environments.active.clone()?,
                project: project.clone(),
                root: state.shell.project(project)?.workspace_root.clone(),
                connection: state.connection_id()?,
                generation: *state.client_handle()?.connection_generation().borrow(),
                destination: (state.server_config.config.as_ref()?.environment.environment_id.clone(), state.shell.project(project)?.created_at.clone()),
            })
        })();
        let changed = scope != self.workspace.scope;
        if changed {
            self.workspace = Workspace { scope: scope.clone(), ..Default::default() };
        }
        let Some(scope) = scope else { if changed {cx.notify();} return };
        let choice = self.workspace_choice();
        let current = choice.and_then(|choice| choice.previous.as_ref()).map(|seed| seed.worktree_path.as_str());
        let previous = rules::previous_worktree_seed(
            state.shell.snapshot.iter().flat_map(|shell| &shell.threads).map(|thread| PreviousThread {
                project_id: &thread.project_id,
                branch: thread.branch.as_deref(),
                worktree_path: thread.worktree_path.as_deref(),
                updated_at: &thread.updated_at,
                archived_at: thread.archived_at.as_deref(),
            }),
            &scope.project,
            current,
        );
        let previous_changed = previous != self.workspace.previous;
        self.workspace.previous = previous;
        if !changed && !reload {
            if previous_changed {cx.notify();}
            return;
        }
        let Some(client) = state.client_handle() else {
            cx.notify();
            return;
        };
        let Some(config) = state.server_config.config.as_ref() else {
            cx.notify();
            return;
        };
        let configured = rules::configured_env_mode(&config.settings, &scope.project);
        let default_start_from_origin = rules::start_from_origin(&config.settings, &scope.project, ThreadEnvMode::Worktree);
        self.workspace.default_start_from_origin = default_start_from_origin;
        let new_root = config.new_projects_root.clone();
        let has_threads = state.shell.snapshot.as_ref().is_some_and(|shell| {
            shell.threads.iter().any(|thread| thread.project_id == scope.project)
                || shell.older_settled.as_ref().is_some_and(|cut| cut.counts.iter().any(|count| count.project_id == scope.project && count.count > 0))
        });
        let runtime = state.runtime().clone();
        cx.notify();
        let origin = scope.clone();
        self.workspace.loading = true;
        let net = runtime.spawn(async move {
            let file = if configured.is_none() {
                client
                    .request(method::PROJECTS_READ_FILE, rules::project_file_request(&origin.root))
                    .await
                    .ok()
                    .and_then(|value| rules::project_file_contents(&value).and_then(rules::project_file_env_mode))
            } else {
                None
            };
            let mode = rules::new_thread_env_mode(configured, file, Some(&origin.root), new_root.as_deref(), has_threads);
            let refs = client.list_refs(rules::list_refs_request(&origin.root)).await;
            (mode, refs)
        });
        self.workspace.task = Some(cx.spawn(async move |this, cx| {
            let Ok((mode, result)) = net.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.workspace.scope.as_ref() != Some(&scope) {
                    return;
                }
                this.workspace.loading = false;
                this.workspace.default_mode = Some(mode);
                match result {
                    Ok(Some(refs)) => {
                        this.workspace.is_repo = refs.is_repo;
                        this.workspace.refs = refs.refs;
                        this.workspace.error = None;
                    }
                    Ok(None) => {
                        this.sync_workspace(true, cx);
                    }
                    Err(error) => this.workspace.error = Some(error),
                }
                if !this.workspace_enabled() {this.workspace.open = false;}
                cx.notify();
            });
        }));
    }

    pub(crate) fn open_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || !self.workspace_enabled() {
            if self.workspace.open {self.workspace.open = false; cx.notify();}
            return;
        }
        self.workspace.open = !self.workspace.open;
        self.workspace.branch_open = false;
        self.workspace.host_open = false;
        if self.workspace.open {
            let choice = self.workspace_choice();
            self.workspace.highlighted = usize::from(choice.map_or(self.workspace.default_mode.unwrap_or(ThreadEnvMode::Local) == ThreadEnvMode::Worktree, |choice| choice.mode == ThreadEnvMode::Worktree && choice.previous.is_none()));
            self.sync_workspace(true, cx);
        }
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn open_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (_, options, can_pick) = self.host_options(cx);
        if self.is_rewinding(cx) || !can_pick || options.is_empty() || !matches!(self.snapshot.target, Some(Target::Draft(_))) {
            if self.workspace.host_open {
                self.workspace.host_open = false;
                cx.notify();
            }
            return;
        }
        self.workspace.host_open = !self.workspace.host_open;
        self.workspace.open = false;
        self.workspace.branch_open = false;
        if self.workspace.host_open {
            let active = self.state.read(cx).environments.active.as_deref();
            self.workspace.host_highlighted = options
                .iter()
                .position(|opt| active == Some(opt.saved_id.as_str()))
                .unwrap_or(0);
        }
        self.focus(window, cx);
        cx.notify();
    }

    pub(super) fn workspace_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.workspace.host_open {
            let (_, options, can_pick) = self.host_options(cx);
            if !can_pick || options.is_empty() || !matches!(self.snapshot.target, Some(Target::Draft(_))) {
                self.workspace.host_open = false;
                cx.notify();
                return false;
            }
            let count = options.len();
            self.workspace.host_highlighted = self.workspace.host_highlighted.min(count - 1);
            match key {
                MenuKey::Up => self.workspace.host_highlighted = (self.workspace.host_highlighted + count - 1) % count,
                MenuKey::Down => self.workspace.host_highlighted = (self.workspace.host_highlighted + 1) % count,
                MenuKey::Dismiss => {
                    self.workspace.host_open = false;
                    self.focus(window, cx);
                }
                MenuKey::Accept => {
                    if let Some(option) = options.get(self.workspace.host_highlighted).cloned() {
                        self.select_host(option, window, cx);
                    }
                }
                MenuKey::Delete => return false,
            }
            cx.notify();
            return true;
        }
        if self.workspace.branch_open {
            if !self.base_branch_picker_visible(cx) {
                self.workspace.branch_open = false;
                cx.notify();
                return false;
            }
            let count = 1 + self.workspace.refs.len();
            self.workspace.branch_highlighted = self.workspace.branch_highlighted.min(count - 1);
            match key {
                MenuKey::Up => self.workspace.branch_highlighted = (self.workspace.branch_highlighted + count - 1) % count,
                MenuKey::Down => self.workspace.branch_highlighted = (self.workspace.branch_highlighted + 1) % count,
                MenuKey::Dismiss => {
                    self.workspace.branch_open = false;
                    self.focus(window, cx);
                }
                MenuKey::Accept => {
                    if self.workspace.branch_highlighted == 0 {
                        self.toggle_start_from_origin(window, cx);
                    } else if let Some(reference) = self.workspace.refs.get(self.workspace.branch_highlighted - 1).cloned() {
                        self.choose_base_branch(reference.name, window, cx);
                    }
                }
                MenuKey::Delete => return false,
            }
            cx.notify();
            return true;
        }
        if !self.workspace.open {return false;}
        self.sync_workspace(false, cx);
        if !self.workspace.open || !self.workspace_enabled() {self.workspace.open = false; cx.notify(); return false;}
        let previous = self.workspace.previous.is_some();
        let count = 2 + usize::from(previous) + usize::from(self.workspace.error.is_some());
        self.workspace.highlighted = self.workspace.highlighted.min(count - 1);
        match key {
            MenuKey::Up => self.workspace.highlighted = (self.workspace.highlighted + count - 1) % count,
            MenuKey::Down => self.workspace.highlighted = (self.workspace.highlighted + 1) % count,
            MenuKey::Dismiss => {self.workspace.open = false; self.focus(window, cx);}
            MenuKey::Accept => match self.workspace.highlighted {
                0 => { let previous = self.workspace_choice().and_then(|choice| choice.previous.clone()); self.choose_workspace(ThreadEnvMode::Local, previous, window, cx); }
                1 => self.choose_workspace(ThreadEnvMode::Worktree, None, window, cx),
                2 if previous => self.use_previous_worktree(window, cx),
                _ => self.sync_workspace(true, cx),
            },
            MenuKey::Delete => return false,
        }
        cx.notify();
        true
    }

    fn workspace_visible(&self) -> bool {
        self.workspace.scope.as_ref().is_some_and(|scope| {
            self.workspace.is_repo || self.workspace.error.is_some()
                || (self.workspace_choice().is_none() && self.drafts.store.get(&drafts::key(&Target::Draft(scope.project.clone()))).is_some_and(|draft| draft.workspace.is_some()))
        })
    }

    pub(super) fn workspace_enabled(&self) -> bool {
        rules::can_use_previous_worktree(!matches!(self.snapshot.target, Some(Target::Draft(_))), self.sending, false) && self.workspace_visible()
    }

    fn choose_workspace(&mut self, mode: ThreadEnvMode, previous: Option<PreviousWorktreeSeed>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.workspace_enabled() || self.has_multiple_models(cx) {
            return;
        }
        let Some(scope) = self.workspace.scope.clone() else {
            return;
        };
        let target = Target::Draft(scope.project);
        self.record_draft(&target, cx);
        let mut draft = self.drafts.store.get(&drafts::key(&target)).cloned().unwrap_or_default();
        draft.updated_at = crate::sidebar::controller::now_ms();
        let existing = draft.workspace.as_ref().filter(|w| w.in_scope(&scope.environment, &scope.root));
        let branch = existing.and_then(|w| w.branch.clone());
        let start_from_origin = existing.and_then(|w| w.start_from_origin);
        draft.workspace = Some(DraftWorkspace {
            destination: Some(scope.destination),
            branch,
            start_from_origin,
            environment_id: scope.environment,
            project_root: scope.root,
            mode,
            previous,
        });
        self.mutate_drafts(cx, |store| store.set(&drafts::key(&target), draft));
        self.schedule_draft_save(&target, cx);
        self.workspace.open = false;
        self.workspace.branch_open = false;
        self.workspace.host_open = false;
        self.error = None;
        self.sync_workspace(false, cx);
        self.sync_menu(true, cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn base_branch_picker_visible(&self, cx: &App) -> bool {
        if !self.workspace_enabled() || self.has_multiple_models(cx) {
            return false;
        }
        let choice = self.workspace_choice();
        let previous = choice.and_then(|choice| choice.previous.as_ref());
        let mode = choice.map_or(self.workspace.default_mode.unwrap_or(ThreadEnvMode::Local), |choice| choice.mode);
        self.workspace.is_repo && mode == ThreadEnvMode::Worktree && previous.is_none()
    }

    fn effective_base_branch(&self) -> (Option<String>, Option<bool>, bool) {
        let choice = self.workspace_choice();
        let branch = choice.and_then(|c| c.branch.clone()).or_else(|| {
            self.workspace
                .refs
                .iter()
                .find(|r| r.is_default)
                .or_else(|| self.workspace.refs.iter().find(|r| r.current))
                .map(|r| r.name.clone())
        });
        let is_remote = branch
            .as_deref()
            .and_then(|name| self.workspace.refs.iter().find(|r| r.name == name))
            .and_then(|r| r.is_remote);
        let start_from_origin = choice
            .and_then(|c| c.start_from_origin)
            .unwrap_or(self.workspace.default_start_from_origin);
        (branch, is_remote, start_from_origin)
    }

    fn open_base_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.base_branch_picker_visible(cx) || self.is_rewinding(cx) {
            self.workspace.branch_open = false;
            cx.notify();
            return;
        }
        self.workspace.branch_open = !self.workspace.branch_open;
        self.workspace.open = false;
        self.workspace.host_open = false;
        if self.workspace.branch_open {
            let (selected, _, _) = self.effective_base_branch();
            self.workspace.branch_highlighted = selected
                .as_deref()
                .and_then(|name| self.workspace.refs.iter().position(|r| r.name == name))
                .map_or(0, |index| index + 1);
            self.sync_workspace(true, cx);
        }
        self.focus(window, cx);
        cx.notify();
    }

    fn mutate_draft_workspace(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut DraftWorkspace)) {
        let Some(scope) = self.workspace.scope.clone() else { return };
        let target = Target::Draft(scope.project);
        self.record_draft(&target, cx);
        let mut draft = self.drafts.store.get(&drafts::key(&target)).cloned().unwrap_or_default();
        draft.updated_at = crate::sidebar::controller::now_ms();
        let default_mode = self.workspace.default_mode.unwrap_or(ThreadEnvMode::Local);
        let mut workspace = draft
            .workspace
            .filter(|w| w.in_scope(&scope.environment, &scope.root))
            .unwrap_or(DraftWorkspace {
                destination: Some(scope.destination.clone()),
                branch: None,
                start_from_origin: None,
                environment_id: scope.environment,
                project_root: scope.root,
                mode: default_mode,
                previous: None,
            });
        workspace.destination = Some(scope.destination);
        edit(&mut workspace);
        draft.workspace = Some(workspace);
        self.mutate_drafts(cx, |store| store.set(&drafts::key(&target), draft));
        self.schedule_draft_save(&target, cx);
    }

    fn choose_base_branch(&mut self, branch: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.base_branch_picker_visible(cx) {
            return;
        }
        self.mutate_draft_workspace(cx, |workspace| {
            workspace.mode = ThreadEnvMode::Worktree;
            workspace.previous = None;
            workspace.branch = Some(branch);
        });
        self.workspace.branch_open = false;
        self.focus(window, cx);
        cx.notify();
    }

    fn toggle_start_from_origin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.base_branch_picker_visible(cx) {
            return;
        }
        let (_, _, current) = self.effective_base_branch();
        self.mutate_draft_workspace(cx, |workspace| {
            workspace.start_from_origin = Some(!current);
        });
        self.focus(window, cx);
        cx.notify();
    }

    fn host_options(&self, cx: &App) -> (Option<HostOption>, Vec<HostOption>, bool) {
        let state = self.state.read(cx);
        let active_env = state.environments.active.as_deref();
        let active_project = self.snapshot.project_id.as_deref();
        let primary_id = state.local_saved_environment().map(|env| env.id.as_str()).or_else(|| {
            let mut local = state.environments.environments.iter().filter(|env| env.local);
            let first = local.next()?;
            local.next().is_none().then_some(first.id.as_str())
        });
        if state.fleet.environments.is_empty() {
            let Some(saved) = state.environments.active() else {
                return (None, Vec::new(), false);
            };
            let is_primary = primary_id == Some(saved.id.as_str()) || saved.local;
            let opt = HostOption {
                owner: None,
                saved_id: saved.id.clone(),
                project_id: active_project.unwrap_or_default().to_owned(),
                label: saved.label.clone(),
                is_primary,
            };
            let show = !is_primary || state.environments.environments.len() > 1;
            return (show.then_some(opt), Vec::new(), false);
        }
        let mut group_options: Vec<HostOption> = Vec::new();
        if let Some(pid) = active_project {
            for group in state.project_groups_for(GroupConsumer::DraftPicker, cx) {
                let matches = group.registrations.iter().any(|reg| {
                    reg.project.id == pid && active_env.is_none_or(|env| env == reg.owner.saved_id)
                });
                if !matches {
                    continue;
                }
                for reg in group.registrations {
                    let is_primary = primary_id == Some(reg.owner.saved_id.as_str())
                        || state.environments.environments.iter().any(|e| e.id == reg.owner.saved_id && e.local);
                    let label = if reg.environment_label.trim().is_empty() {
                        state
                            .environments
                            .environments
                            .iter()
                            .find(|e| e.id == reg.owner.saved_id)
                            .map(|e| e.label.clone())
                            .unwrap_or_else(|| reg.owner.saved_id.clone())
                    } else {
                        reg.environment_label
                    };
                    let saved_id = reg.owner.saved_id.clone();
                    group_options.push(HostOption {
                        owner: Some(reg.owner),
                        saved_id,
                        project_id: reg.project.id,
                        label,
                        is_primary,
                    });
                }
                break;
            }
        }
        let active = group_options
            .iter()
            .find(|opt| active_env == Some(opt.saved_id.as_str()))
            .cloned()
            .or_else(|| {
                let saved = state.environments.active()?;
                let is_primary = primary_id == Some(saved.id.as_str()) || saved.local;
                let owner = state.scoped_environment(&saved.id).map(|entry| entry.summary.owner.clone());
                Some(HostOption {
                    owner,
                    saved_id: saved.id.clone(),
                    project_id: active_project.unwrap_or_default().to_owned(),
                    label: saved.label.clone(),
                    is_primary,
                })
            });
        let can_pick = group_options.len() > 1;
        let show = can_pick || active.as_ref().is_some_and(|opt| !opt.is_primary);
        if !show {
            return (None, Vec::new(), false);
        }
        (active, group_options, can_pick)
    }

    fn select_host(&mut self, option: HostOption, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.host_open = false;
        let Some(owner) = option.owner else {
            self.focus(window, cx);
            cx.notify();
            return;
        };
        let (from_project, same_env) = {
            let state = self.state.read(cx);
            (state.draft_project.clone(), state.environments.active.as_deref() == Some(option.saved_id.as_str()))
        };
        if same_env && from_project.as_deref() == Some(option.project_id.as_str()) {
            self.focus(window, cx);
            cx.notify();
            return;
        }
        if let Some(from) = from_project {
            self.retarget_draft(&from, &option.project_id, same_env, window, cx);
        }
        self.state.update(cx, |state, cx| {
            state.navigate_scoped(owner, ScopedRouteAction::Draft(option.project_id), cx);
        });
        self.focus(window, cx);
        cx.notify();
    }

    fn render_host_selector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (active, options, can_pick) = self.host_options(cx);
        let active = active?;
        let p = palette(cx);
        let state = self.state.read(cx);
        let icon = environment_machine_icon(state, &active.saved_id);
        let is_draft = matches!(self.snapshot.target, Some(Target::Draft(_)));
        let interactive = is_draft && can_pick && !self.is_rewinding(cx);
        if !interactive {
            return Some(
                ComposerControl::new("composer-host")
                    .icon(icon)
                    .label(active.label.clone())
                    .tooltip(active.label)
                    .disabled(true)
                    .into_any_element(),
            );
        }
        let tooltip = crate::keymap::label("composer.host", cx)
            .map_or_else(|| active.label.clone(), |shortcut| format!("{} · {}", active.label, shortcut));
        let mut popup = menu_surface(cx)
            .w(px(260.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.workspace.host_open = false;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Run on"));
        let active_env = state.environments.active.as_deref();
        for (index, option) in options.into_iter().enumerate() {
            let checked = active_env == Some(option.saved_id.as_str());
            let highlighted = self.workspace.host_highlighted == index;
            let opt_icon = environment_machine_icon(state, &option.saved_id);
            let label = option.label.clone();
            popup = popup.child(
                MenuItem::new(("host-opt", index), label)
                    .icon(opt_icon, hsla(p.muted_foreground))
                    .checked(checked)
                    .highlighted(highlighted)
                    .on_click(cx.listener(move |this, _, window, cx| this.select_host(option.clone(), window, cx))),
            );
        }
        Some(
            div()
                .relative()
                .min_w_0()
                .child(
                    ComposerControl::new("composer-host")
                        .icon(icon)
                        .label(active.label)
                        .pressed(self.workspace.host_open)
                        .tooltip(tooltip)
                        .on_click(cx.listener(|this, _, window, cx| this.open_host(window, cx))),
                )
                .when(self.workspace.host_open, |this| this.child(floating(Float::AboveStart, div().mb(px(4.)).child(popup))))
                .into_any_element(),
        )
    }

    fn render_base_branch_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.base_branch_picker_visible(cx) {
            return None;
        }
        let p = palette(cx);
        let (branch, is_remote, start_from_origin) = self.effective_base_branch();
        let label = t3_core::vcs::branch_trigger_label(true, branch.as_deref(), is_remote, start_from_origin);
        let mut popup = menu_surface(cx)
            .w(px(300.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.workspace.branch_open = false;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Base branch"))
            .child(
                MenuItem::new("base-branch-origin", "Start from origin")
                    .checked(start_from_origin)
                    .highlighted(self.workspace.branch_highlighted == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_start_from_origin(window, cx))),
            );
        let mut refs_list = div().id("base-branch-refs").max_h(px(220.)).overflow_y_scroll();
        for (index, reference) in self.workspace.refs.iter().enumerate() {
            let name = reference.name.clone();
            let checked = branch.as_deref() == Some(name.as_str());
            let highlighted = self.workspace.branch_highlighted == index + 1;
            let row_label = if reference.is_remote == Some(true) {
                format!("{name} (remote)")
            } else {
                name.clone()
            };
            refs_list = refs_list.child(
                MenuItem::new(("base-branch-ref", index), row_label)
                    .checked(checked)
                    .highlighted(highlighted)
                    .on_click(cx.listener(move |this, _, window, cx| this.choose_base_branch(name.clone(), window, cx))),
            );
        }
        popup = popup.child(refs_list);
        Some(
            div()
                .relative()
                .min_w_0()
                .child(
                    ComposerControl::new("composer-base-branch")
                        .icon(Icon::GitBranch)
                        .label(label.clone())
                        .max_label_width(220.)
                        .suffix(Icon::ChevronDown.render(px(12.), hsla(p.muted_foreground)))
                        .pressed(self.workspace.branch_open)
                        .tooltip(label)
                        .on_click(cx.listener(|this, _, window, cx| this.open_base_branch_picker(window, cx))),
                )
                .when(self.workspace.branch_open, |this| this.child(floating(Float::AboveStart, div().mb(px(4.)).child(popup))))
                .into_any_element(),
        )
    }

    pub(crate) fn use_previous_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        self.sync_workspace(false, cx);
        if !self.workspace.is_repo {
            return;
        }
        if let Some(seed) = self.workspace.previous.clone() {
            self.choose_workspace(ThreadEnvMode::Worktree, Some(seed), window, cx);
        }
    }

    pub(super) fn render_workspace(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let host = self.render_host_selector(cx);
        let workspace_visible = self.workspace_visible();
        if !workspace_visible && host.is_none() {
            return None;
        }
        let mode_control = workspace_visible.then(|| {
        let p = palette(cx);
        let choice = self.workspace_choice();
        let previous = choice.and_then(|choice| choice.previous.as_ref());
        let multi_models = self.has_multiple_models(cx);
        let mode = if multi_models { ThreadEnvMode::Worktree } else { choice.map_or(self.workspace.default_mode.unwrap_or(ThreadEnvMode::Local), |choice| choice.mode) };
        let label = if previous.is_some() && !multi_models {
            "Current worktree"
        } else if mode == ThreadEnvMode::Worktree {
            "New worktree"
        } else {
            "Current checkout"
        };
        let tooltip = if multi_models {
            "Multiple models each launch in their own new worktree.".to_owned()
        } else {
            let base = previous.map(|seed| seed.branch.as_deref().unwrap_or(&seed.worktree_path)).unwrap_or(label);
            crate::keymap::label("composer.workspace", cx).map_or(base.to_owned(), |shortcut| format!("{base} · {shortcut}"))
        };
        let enabled = self.workspace_enabled() && !multi_models && !self.is_rewinding(cx);
        let mut popup = menu_surface(cx)
            .w(px(336.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.workspace.open = false;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Workspace"))
            .child(
                MenuItem::new("workspace-local", if previous.is_some() { "Current worktree" } else { "Current checkout" })
                    .checked(mode == ThreadEnvMode::Local || previous.is_some())
                    .highlighted(self.workspace.highlighted == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let previous = this.workspace_choice().and_then(|choice| choice.previous.clone());
                        this.choose_workspace(ThreadEnvMode::Local, previous, window, cx);
                    })),
            )
            .child(
                MenuItem::new("workspace-new", "New worktree")
                    .checked(mode == ThreadEnvMode::Worktree && previous.is_none())
                    .highlighted(self.workspace.highlighted == 1)
                    .on_click(cx.listener(|this, _, window, cx| this.choose_workspace(ThreadEnvMode::Worktree, None, window, cx))),
            );
        if let Some(seed) = &self.workspace.previous {
            popup = popup.child(
                div()
                    .id("workspace-previous")
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(self.workspace.highlighted == 2, |this| this.bg(hsla(p.accent)))
                    .hover(|style| style.bg(hsla(p.accent)))
                    .on_click(cx.listener(|this, _, window, cx| this.use_previous_worktree(window, cx)))
                    .child(
                        h_flex().items_start().gap(px(6.)).child(div().pt(px(4.)).child(Icon::History.render(px(12.), hsla(p.muted_foreground)))).child(
                            v_flex()
                                .min_w_0()
                                .text_sm()
                                .child("Previous worktree")
                                .children(seed.branch.clone().map(|branch| div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(branch))),
                        ),
                    ),
            );
        }
        if self.workspace.loading {
            popup = popup.child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Loading refs..."));
        }
        if let Some(error) = &self.workspace.error {
            popup = popup
                .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.error_foreground)).child(error.clone()))
                .child(MenuItem::new("workspace-retry", "Retry").highlighted(self.workspace.highlighted == 2 + usize::from(self.workspace.previous.is_some())).on_click(cx.listener(|this, _, _, cx| this.sync_workspace(true, cx))));
        }
        div()
            .relative()
            .min_w_0()
            .child(
                ComposerControl::new("composer-workspace")
                    .icon(if previous.is_some() {
                        Icon::FolderGit
                    } else if mode == ThreadEnvMode::Worktree {
                        Icon::FolderGit2
                    } else {
                        Icon::Folder
                    })
                    .label(label)
                    .when_some(previous.and_then(|seed| seed.branch.clone()), |control, branch| {
                        control.suffix(div().min_w_0().max_w(px(224.)).truncate().text_xs().text_color(hsla(p.muted_foreground)).child(branch))
                    })
                    .pressed(self.workspace.open)
                    .disabled(!enabled)
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, window, cx| this.open_workspace(window, cx))),
            )
            .when(self.workspace.open && enabled, |this| this.child(floating(Float::AboveStart, div().mb(px(4.)).child(popup))))
            .into_any_element()
        });
        let base_branch = self.render_base_branch_picker(cx);
        Some(
            h_flex()
                .min_w_0()
                .gap(px(4.))
                .children(host)
                .children(mode_control)
                .children(base_branch)
                .into_any_element(),
        )
    }
}

#[cfg(feature = "snapshot")]
impl Composer {
    pub(crate) fn verify_workspace_availability(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if self.workspace.scope.is_none() {return Err("availability fixture requires a connected draft".into());}
        self.workspace.task = None;
        self.workspace.previous = None;
        self.workspace.is_repo = false;
        self.workspace.error = None;
        self.workspace.open = false;
        self.set_prompt("", window, cx);
        self.open_workspace(window, cx);
        if self.workspace.open || self.render_workspace(cx).is_some() {return Err("non-repository shortcut opened an invisible workspace popup".into());}
        if self.workspace_key(MenuKey::Down, window, cx) || self.workspace_key(MenuKey::Accept, window, cx) {return Err("hidden workspace popup captured prompt keys".into());}
        self.on_enter(false, false, window, cx);
        if cx.global::<super::SnapshotAudit>().submissions.replace(0) != 1 {return Err("hidden workspace popup suppressed ordinary empty Enter".into());}
        self.workspace.error = Some("fixture read failed".into());
        self.open_workspace(window, cx);
        if !self.workspace.open || self.render_workspace(cx).is_none() || !self.workspace_key(MenuKey::Down, window, cx) {return Err("visible error/retry popup lost keyboard access while refreshing".into());}
        self.workspace_key(MenuKey::Dismiss, window, cx);
        self.workspace.is_repo = true;
        self.workspace.error = None;
        self.open_workspace(window, cx);
        if !self.workspace.open {return Err("repository popup did not open".into());}
        self.workspace.is_repo = false;
        if self.workspace_key(MenuKey::Accept, window, cx) || self.workspace.open {return Err("workspace popup retained keys after repository availability changed".into());}
        self.workspace.open = true;
        self.on_enter(false, false, window, cx);
        if cx.global::<super::SnapshotAudit>().submissions.replace(0) != 1 {return Err("stale hidden popup suppressed empty Enter fallback".into());}
        self.workspace.open = false;
        self.workspace.task = None;
        eprintln!("workspace availability: hidden non-repo shortcut/keys refused; empty Enter reached submit spy; error/retry and repository menus remained accessible; loss of availability released keys; no launch RPC");
        Ok(())
    }
}

impl Composer {
    pub(super) fn apply_branch_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.state.update(cx, |state, _| state.branch_draft.take());
        let Some((owner, choice)) = pending else { return };
        let state = self.state.read(cx);
        if !owner.current(state) || state.selected_thread.is_some() { return; }
        let Some(project) = state.draft_project.clone() else { return };
        let target = Target::Draft(project);
        if self.snapshot.target.as_ref() != Some(&target) { return; }
        self.record_draft(&target, cx);
        let mut draft = self.drafts.store.get(&drafts::key(&target)).cloned().unwrap_or_default();
        draft.workspace = Some(choice);
        self.mutate_drafts(cx, |store| store.set(&drafts::key(&target), draft));
        self.schedule_draft_save(&target, cx);
        self.sync_workspace(false, cx);
        self.sync_menu(true, cx);
        self.focus(window, cx);
        cx.notify();
    }
}
