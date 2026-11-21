//! Draft workspace menu, previous worktree entry, and automatic draft environment routing from
//! `BranchToolbar.tsx`, `BranchToolbarEnvironmentSelector.tsx`, `BranchToolbarEnvModeSelector.tsx`,
//! `PreviousWorktreeItemContent.tsx`, `useLoadBalancedEnvironment.ts`, and
//! `useDraftEnvironmentRouting.ts`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{AnyElement, App, Context, Entity, IntoElement, ParentElement, ScrollHandle, Styled, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use t3_client::load_balancing::{
    DraftEnvironmentCandidate, HOST_RESOURCES_FRESH_MS, HostResourcesCache,
    attachments_block_auto_routing_warning, auto_environment_label,
};
use t3_core::commands::method;
use t3_core::server_config::ThreadEnvMode;
use t3_core::sidebar::grouping::{EnvironmentOwner, GroupConsumer};
use t3_core::vcs::VcsRef;
use t3_core::vcs::refs::RefListing;
use t3_core::workspace::{self as rules, DraftWorkspace, PreviousThread, PreviousWorktreeSeed};

use super::controls::{ComposerControl, Float, MenuItem, branch_copy_menu, floating, menu_surface};
use super::{Composer, Target, drafts};
use super::menu::MenuKey;
use crate::app_state::ScopedRouteAction;
use crate::icons::Icon;
use crate::new_thread::environment_machine_icon;
use crate::sidebar::controller::now_ms;
use crate::i18n::t;
use crate::settings::controls::Switch;
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette};
use crate::transcript::middle_truncate;
use crate::toast::{Toast, ToastKind};

const BASE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const START_FROM_REMOTE_TOOLTIP: &str = "Uses the latest commit of the base branch’s tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.";
const GENERIC_LOCAL_ENVIRONMENT_LABELS: &[&str] = &["this device", "this machine", "local", "localhost"];

#[derive(Clone, PartialEq, Eq)]
struct Scope {
    environment: String,
    project: String,
    draft: t3_core::composer::drafts::DraftRef,
    root: String,
    connection: u64,
    generation: u64,
    destination: (String, String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DraftEnvSelection {
    /// Follow load balancing when enabled and eligible; default for new drafts.
    #[default]
    Implicit,
    /// User explicitly picked `"Auto balance"` in the Run-on menu (overrides a selected branch).
    Auto,
    /// User manually chose a specific machine.
    Manual,
}

#[derive(Default)]
pub(super) struct DraftRouting {
    /// Logical project group key this draft routing state belongs to, so self-initiated
    /// retargeting across machines in the same logical project preserves routing state.
    pub(super) logical_project_key: Option<String>,
    draft_id: Option<t3_core::composer::drafts::DraftId>,
    pub(super) selection: DraftEnvSelection,
    /// Canonical `environment_id` picked by automatic load balancing (`loadBalancedEnvironmentId`).
    pub(super) resolved_environment_id: Option<String>,
    /// `(saved_id, project_id)` of an in-flight `navigate_scoped` initiated by auto-routing or
    /// `refreshBeforeSend`, so the transient `clear_connection_state` gap before the target
    /// environment activates does not wipe `selection` / `resolved_environment_id`.
    pub(super) pending_navigation: Option<(String, String)>,
    /// Bounded in-memory cache of `server.getHostResources` snapshots per canonical `environment_id`.
    pub(super) cache: HostResourcesCache,
    /// Generation counter guarding async host resource queries against stale replies.
    generation: u64,
    /// Canonical env IDs currently being queried to avoid duplicate concurrent fetches.
    in_flight_ids: Vec<String>,
    pub(super) task: Option<Task<()>>,
    expiry_timer: Option<Task<()>>,
}

#[derive(Default)]
pub(super) struct Workspace {
    scope: Option<Scope>,
    pub(super) is_repo: bool,
    pub(super) refs: RefListing,
    default_ref: Option<VcsRef>,
    selected_ref: Option<VcsRef>,
    selected_ref_key: Option<(String, String)>,
    selected_ref_task: Option<Task<()>>,
    pub(super) base_search: Option<Entity<InputState>>,
    base_search_sub: Option<Subscription>,
    base_search_debounce: Option<Task<()>>,
    base_scroll: ScrollHandle,
    refs_task: Option<Task<()>>,
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
    pub(super) routing: DraftRouting,
}

#[derive(Clone)]
pub(super) struct HostOption {
    pub(super) owner: Option<EnvironmentOwner>,
    pub(super) saved_id: String,
    pub(super) canonical_environment_id: String,
    pub(super) project_id: String,
    pub(super) label: String,
    is_primary: bool,
}

#[derive(Default)]
pub(super) struct HostOptions {
    pub(super) logical_project_key: Option<String>,
    pub(super) active: Option<HostOption>,
    pub(super) options: Vec<HostOption>,
    pub(super) add_targets: Vec<HostOption>,
    pub(super) can_pick: bool,
}

impl Composer {
    fn workspace_choice(&self) -> Option<&DraftWorkspace> {
        let scope = self.workspace.scope.as_ref()?;
        self.drafts.store.get(&drafts::key(&Target::Draft(scope.draft.clone())))?.workspace.as_ref().filter(|choice| choice.in_scope(&scope.environment, &scope.root) && choice.destination.as_ref().is_none_or(|destination| destination == &scope.destination))
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
            let Target::Draft(draft) = self.snapshot.target.as_ref()? else {
                return None;
            };
            let project = &draft.project_id;
            Some(Scope {
                draft: draft.clone(),
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
            let routing = std::mem::take(&mut self.workspace.routing);
            self.workspace = Workspace {
                scope: scope.clone(),
                base_search: self.workspace.base_search.take(),
                base_search_sub: self.workspace.base_search_sub.take(),
                routing,
                ..Default::default()
            };
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
        let query = if self.workspace.branch_open { self.base_search_query(cx) } else { String::new() };
        let req = (!self.workspace.branch_open || query.is_empty()).then(|| self.workspace.refs.begin("", false)).flatten();
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
                    Ok(Some(page)) => {
                        this.workspace.is_repo = page.is_repo;
                        this.workspace.default_ref = page
                            .refs
                            .iter()
                            .find(|r| r.is_default)
                            .or_else(|| page.refs.iter().find(|r| r.current))
                            .cloned();
                        if let Some(req) = &req {
                            this.workspace.refs.finish(req, Ok(vec![page]));
                        }
                        this.workspace.error = None;
                        this.sync_selected_base_ref(cx);
                    }
                    Ok(None) => {
                        if let Some(req) = &req {
                            this.workspace.refs.finish(req, Err("Session changed.".into()));
                        }
                        this.sync_workspace(true, cx);
                    }
                    Err(error) => {
                        if let Some(req) = &req {
                            this.workspace.refs.finish(req, Err(error.clone()));
                        }
                        this.workspace.error = Some(error);
                    }
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
        let host = self.host_options(cx);
        if self.is_rewinding(cx) || !host.can_pick || (host.options.is_empty() && host.add_targets.is_empty()) || !matches!(self.snapshot.target, Some(Target::Draft(_))) {
            if self.workspace.host_open {
                self.workspace.host_open = false;
                cx.notify();
            }
            return;
        }
        self.workspace.host_open = !self.workspace.host_open;
        self.workspace.open = false;
        self.close_base_branch_picker(false, window, cx);
        if self.workspace.host_open {
            let offer_auto = self.can_offer_auto_balance(&host, cx);
            let auto_active = self.draft_auto_environment_label(&host, cx).is_some();
            if offer_auto && auto_active {
                self.workspace.host_highlighted = 0;
            } else {
                let offset = usize::from(offer_auto);
                let active = self.state.read(cx).environments.active.as_deref();
                self.workspace.host_highlighted = host
                    .options
                    .iter()
                    .position(|opt| active == Some(opt.saved_id.as_str()))
                    .map_or(0, |idx| idx + offset);
            }
        }
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn cycle_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || !matches!(self.snapshot.target, Some(Target::Draft(_))) {
            return;
        }
        let host = self.host_options(cx);
        if host.options.len() <= 1 {
            return;
        }
        let active = self.state.read(cx).environments.active.as_deref();
        let current = host
            .options
            .iter()
            .position(|opt| active == Some(opt.saved_id.as_str()))
            .unwrap_or(0);
        let next = host.options[(current + 1) % host.options.len()].clone();
        self.select_host(next, window, cx);
    }

    pub(super) fn workspace_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.workspace.host_open {
            let host = self.host_options(cx);
            if !host.can_pick || (host.options.is_empty() && host.add_targets.is_empty()) || !matches!(self.snapshot.target, Some(Target::Draft(_))) {
                self.workspace.host_open = false;
                cx.notify();
                return false;
            }
            let offer_auto = self.can_offer_auto_balance(&host, cx);
            let auto_offset = usize::from(offer_auto);
            let add_count = host.add_targets.len();
            let count = auto_offset + host.options.len() + add_count;
            self.workspace.host_highlighted = self.workspace.host_highlighted.min(count - 1);
            match key {
                MenuKey::Up => self.workspace.host_highlighted = (self.workspace.host_highlighted + count - 1) % count,
                MenuKey::Down => self.workspace.host_highlighted = (self.workspace.host_highlighted + 1) % count,
                MenuKey::Dismiss => {
                    self.workspace.host_open = false;
                    self.focus(window, cx);
                }
                MenuKey::Accept => {
                    if offer_auto && self.workspace.host_highlighted == 0 {
                        self.select_auto_host(window, cx);
                    } else if let Some(option) = self
                        .workspace
                        .host_highlighted
                        .checked_sub(auto_offset)
                        .and_then(|idx| host.options.get(idx))
                        .cloned()
                    {
                        self.select_host(option, window, cx);
                    } else if let Some(target) = self
                        .workspace
                        .host_highlighted
                        .checked_sub(auto_offset + host.options.len())
                        .and_then(|idx| host.add_targets.get(idx))
                    {
                        self.open_add_project_to_machine(Some(target.saved_id.clone()), window, cx);
                    }
                }
                MenuKey::Delete => return false,
            }
            cx.notify();
            return true;
        }
        if self.workspace.branch_open {
            if !self.base_branch_picker_visible(cx) {
                self.close_base_branch_picker(true, window, cx);
                cx.notify();
                return false;
            }
            let ref_count = self.workspace.refs.refs.len();
            let count = ref_count + 1;
            self.workspace.branch_highlighted = self.workspace.branch_highlighted.min(count - 1);
            match key {
                MenuKey::Up => self.workspace.branch_highlighted = (self.workspace.branch_highlighted + count - 1) % count,
                MenuKey::Down => self.workspace.branch_highlighted = (self.workspace.branch_highlighted + 1) % count,
                MenuKey::Dismiss => {
                    self.close_base_branch_picker(true, window, cx);
                }
                MenuKey::Accept => {
                    if let Some(reference) = self.workspace.refs.refs.get(self.workspace.branch_highlighted).cloned() {
                        self.choose_base_branch(reference.name, window, cx);
                    } else if self.workspace.branch_highlighted == ref_count {
                        self.toggle_start_from_origin(window, cx);
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
                || (self.workspace_choice().is_none() && self.drafts.store.get(&drafts::key(&Target::Draft(scope.draft.clone()))).is_some_and(|draft| draft.workspace.is_some()))
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
        let target = Target::Draft(scope.draft);
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

    fn base_search_query(&self, cx: &App) -> String {
        self.workspace
            .base_search
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn ensure_base_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(search) = &self.workspace.base_search {
            return search.clone();
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        let sub = cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) && this.workspace.branch_open {
                let query = input.read(cx).value().to_string();
                this.workspace.base_search_debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(BASE_SEARCH_DEBOUNCE).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.workspace.branch_open {
                            this.load_base_refs(&query, false, cx);
                        }
                    });
                }));
            }
        });
        self.workspace.base_search = Some(search.clone());
        self.workspace.base_search_sub = Some(sub);
        search
    }

    pub(super) fn load_base_refs(&mut self, query: &str, next_page: bool, cx: &mut Context<Self>) {
        let Some(scope) = self.workspace.scope.clone() else { return };
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let sanitized = t3_core::vcs::sanitize_new_ref_name(query.trim());
        let Some(request) = self.workspace.refs.begin(&sanitized, next_page) else {
            return;
        };
        let cursors = request.cursors.clone();
        let cwd = scope.root.clone();
        let runtime = state.runtime().clone();
        let net = runtime.spawn(async move {
            let mut pages = Vec::with_capacity(cursors.len());
            for cursor in cursors {
                let payload = t3_core::vcs::list_refs(&cwd, Some(&sanitized), cursor, t3_core::vcs::REF_PAGE_LIMIT);
                match client.list_refs(payload).await? {
                    Some(page) => pages.push(page),
                    None => return Ok(None),
                }
            }
            Ok(Some(pages))
        });
        cx.notify();
        self.workspace.refs_task = Some(cx.spawn(async move |this, cx| {
            let result = net.await.unwrap_or_else(|_| Err("Failed to load refs.".into()));
            let _ = this.update(cx, |this, cx| {
                if this.workspace.scope.as_ref() != Some(&scope) {
                    return;
                }
                let result = match result {
                    Ok(Some(pages)) => Ok(pages),
                    Ok(None) => Err("Session changed.".into()),
                    Err(error) => Err(error),
                };
                if this.workspace.refs.finish(&request, result) {
                    if this.workspace.refs.query.is_empty()
                        && let Some(default_ref) = this
                            .workspace
                            .refs
                            .refs
                            .iter()
                            .find(|r| r.is_default)
                            .or_else(|| this.workspace.refs.refs.iter().find(|r| r.current))
                            .cloned()
                    {
                        this.workspace.default_ref = Some(default_ref);
                    }
                    let (selected, _, _) = this.effective_base_branch();
                    this.workspace.branch_highlighted = selected
                        .as_deref()
                        .and_then(|name| this.workspace.refs.refs.iter().position(|r| r.name == name))
                        .unwrap_or(0);
                    this.sync_selected_base_ref(cx);
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn load_more_base_refs(&mut self, cx: &mut Context<Self>) {
        let query = self.base_search_query(cx);
        self.load_base_refs(&query, true, cx);
    }

    fn sync_selected_base_ref(&mut self, cx: &mut Context<Self>) {
        let (Some(branch), _, _) = self.effective_base_branch() else {
            self.workspace.selected_ref = None;
            self.workspace.selected_ref_key = None;
            return;
        };
        if let Some(found) = self
            .workspace
            .refs
            .refs
            .iter()
            .find(|r| r.name == branch)
            .or_else(|| self.workspace.default_ref.as_ref().filter(|r| r.name == branch))
        {
            self.workspace.selected_ref = Some(found.clone());
            return;
        }
        let Some(scope) = self.workspace.scope.clone() else { return };
        let key = (scope.root.clone(), branch.clone());
        if self.workspace.selected_ref_key.as_ref() == Some(&key) {
            return;
        }
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let runtime = state.runtime().clone();
        self.workspace.selected_ref_key = Some(key.clone());
        let net = runtime.spawn(async move {
            client.list_refs(t3_core::vcs::list_refs(&scope.root, Some(&branch), None, 10)).await
        });
        self.workspace.selected_ref_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(page))) = net.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.workspace.selected_ref_key.as_ref() == Some(&key)
                    && let Some(found) = page.refs.into_iter().find(|r| r.name == key.1)
                {
                    this.workspace.selected_ref = Some(found);
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn base_branch_status_text(&self) -> Option<String> {
        if let Some(error) = &self.workspace.refs.error {
            return Some(error.clone());
        }
        if self.workspace.refs.loading && !self.workspace.refs.loaded {
            return Some("Loading refs...".to_owned());
        }
        if self.workspace.refs.loading_next_page {
            return Some("Loading more refs...".to_owned());
        }
        if self.workspace.refs.next_cursor.is_some() {
            return Some(format!("Showing {} of {} refs", self.workspace.refs.refs.len(), self.workspace.refs.total));
        }
        None
    }

    fn effective_base_branch(&self) -> (Option<String>, Option<bool>, bool) {
        let choice = self.workspace_choice();
        let branch = choice.and_then(|c| c.branch.clone()).or_else(|| {
            self.workspace
                .default_ref
                .as_ref()
                .map(|r| r.name.clone())
                .or_else(|| {
                    self.workspace
                        .refs
                        .refs
                        .iter()
                        .find(|r| r.is_default)
                        .or_else(|| self.workspace.refs.refs.iter().find(|r| r.current))
                        .map(|r| r.name.clone())
                })
        });
        let is_remote = branch.as_deref().and_then(|name| {
            self.workspace
                .refs
                .refs
                .iter()
                .find(|r| r.name == name)
                .or_else(|| self.workspace.selected_ref.as_ref().filter(|r| r.name == name))
                .or_else(|| self.workspace.default_ref.as_ref().filter(|r| r.name == name))
                .and_then(|r| r.is_remote)
        });
        let start_from_origin = choice
            .and_then(|c| c.start_from_origin)
            .unwrap_or(self.workspace.default_start_from_origin);
        (branch, is_remote, start_from_origin)
    }

    pub(super) fn close_base_branch_picker(&mut self, focus_composer: bool, window: &mut Window, cx: &mut Context<Self>) {
        let was_open = self.workspace.branch_open;
        self.workspace.branch_open = false;
        self.workspace.base_search_debounce = None;
        if let Some(search) = &self.workspace.base_search {
            search.update(cx, |input, cx| {
                if !input.value().is_empty() {
                    input.set_value("", window, cx);
                }
            });
        }
        if was_open && !self.workspace.refs.query.is_empty() {
            self.load_base_refs("", false, cx);
        }
        if focus_composer {
            self.focus(window, cx);
        }
    }

    pub(crate) fn open_base_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.base_branch_picker_visible(cx) || self.is_rewinding(cx) {
            self.close_base_branch_picker(false, window, cx);
            cx.notify();
            return;
        }
        if self.workspace.branch_open {
            self.close_base_branch_picker(true, window, cx);
            cx.notify();
            return;
        }
        self.workspace.branch_open = true;
        self.workspace.open = false;
        self.workspace.host_open = false;
        let search = self.ensure_base_search(window, cx);
        search.update(cx, |input, cx| {
            if !input.value().is_empty() {
                input.set_value("", window, cx);
            }
            input.focus(window, cx);
        });
        let (selected, _, _) = self.effective_base_branch();
        self.workspace.branch_highlighted = selected
            .as_deref()
            .and_then(|name| self.workspace.refs.refs.iter().position(|r| r.name == name))
            .unwrap_or(0);
        if !self.workspace.refs.loaded || !self.workspace.refs.query.is_empty() {
            self.load_base_refs("", false, cx);
        } else {
            self.sync_selected_base_ref(cx);
        }
        cx.notify();
    }

    pub(super) fn mutate_draft_workspace(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut DraftWorkspace)) {
        let Some(scope) = self.workspace.scope.clone() else { return };
        let target = Target::Draft(scope.draft);
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
        self.sync_selected_base_ref(cx);
        self.close_base_branch_picker(true, window, cx);
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

    fn resolved_host_label(is_primary: bool, runtime_label: Option<&str>, saved_label: Option<&str>, saved_id: &str) -> String {
        let runtime = runtime_label.map(str::trim).filter(|label| !label.is_empty());
        let saved = saved_label.map(str::trim).filter(|label| !label.is_empty());
        if is_primary {
            return [runtime, saved]
                .into_iter()
                .flatten()
                .find(|label| !GENERIC_LOCAL_ENVIRONMENT_LABELS.contains(&label.to_lowercase().as_str()))
                .unwrap_or("This device")
                .to_owned();
        }
        runtime.or(saved).unwrap_or(saved_id).to_owned()
    }

    pub(crate) fn open_add_project_to_machine(
        &mut self,
        target_saved_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace.host_open = false;
        let host = self.host_options(cx);
        let source = host
            .active
            .as_ref()
            .filter(|opt| !opt.project_id.is_empty())
            .map(|opt| (opt.saved_id.clone(), opt.project_id.clone()))
            .or_else(|| {
                let project_id = self.snapshot.project_id.clone()?;
                let saved_id = self.state.read(cx).environments.active.clone().unwrap_or_default();
                Some((saved_id, project_id))
            });
        if let Some((source_saved_id, project_id)) = source {
            let target = target_saved_id.or_else(|| host.add_targets.first().map(|t| t.saved_id.clone()));
            self.state.update(cx, |state, cx| {
                state.open_add_project_to_machine(source_saved_id, project_id, target, cx);
            });
        } else {
            window.dispatch_action(Box::new(crate::palette::OpenAddProject), cx);
        }
        cx.notify();
    }

    fn canonical_env_id_for(state: &crate::app_state::AppState, saved_id: &str, owner: Option<&EnvironmentOwner>) -> String {
        state
            .scoped_environment(saved_id)
            .and_then(|entry| entry.config.config.as_ref())
            .map(|cfg| cfg.environment.environment_id.clone())
            .filter(|id| !id.is_empty())
            .or_else(|| owner.and_then(|o| o.canonical_id.clone()).filter(|id| !id.is_empty()))
            .or_else(|| {
                state
                    .environments
                    .get(saved_id)
                    .and_then(|saved| saved.environment_id.clone())
                    .filter(|id| !id.is_empty())
            })
            .or_else(|| {
                (state.environments.active.as_deref() == Some(saved_id))
                    .then(|| state.server_config.config.as_ref().map(|cfg| cfg.environment.environment_id.clone()))
                    .flatten()
                    .filter(|id| !id.is_empty())
            })
            .unwrap_or_else(|| saved_id.to_owned())
    }

    pub(super) fn host_options(&self, cx: &App) -> HostOptions {
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
                return HostOptions::default();
            };
            let is_primary = primary_id == Some(saved.id.as_str()) || saved.local;
            if !is_primary {
                let runtime_label = state.server_config.config.as_ref().map(|cfg| cfg.environment.label.as_str());
                let opt = HostOption {
                    owner: None,
                    saved_id: saved.id.clone(),
                    canonical_environment_id: Self::canonical_env_id_for(state, &saved.id, None),
                    project_id: active_project.unwrap_or_default().to_owned(),
                    label: Self::resolved_host_label(is_primary, runtime_label, Some(&saved.label), &saved.id),
                    is_primary,
                };
                return HostOptions {
                    logical_project_key: None,
                    active: Some(opt),
                    options: Vec::new(),
                    add_targets: Vec::new(),
                    can_pick: false,
                };
            }
            return HostOptions::default();
        }
        let mut logical_project_key: Option<String> = None;
        let mut group_options: Vec<HostOption> = Vec::new();
        let mut active_remote_url = false;
        if let Some(pid) = active_project {
            if let Some(project) = active_env
                .and_then(|env| state.scoped_project(&t3_core::sidebar::grouping::ProjectRef { environment_id: env.to_owned(), project_id: pid.to_owned() }))
                .map(|scoped| &scoped.project)
                .or_else(|| state.shell.project(pid))
            {
                active_remote_url =
                    t3_core::projects::extract_project_remote_url(project.repository_identity.as_ref()).is_some();
            }
            for group in state.project_groups_for(GroupConsumer::DraftPicker, cx) {
                let matches = group.registrations.iter().any(|reg| {
                    reg.project.id == pid && active_env.is_none_or(|env| env == reg.owner.saved_id)
                });
                if !matches {
                    continue;
                }
                logical_project_key = Some(group.key.clone());
                for reg in group.registrations {
                    if !active_remote_url {
                        active_remote_url =
                            t3_core::projects::extract_project_remote_url(reg.project.repository_identity.as_ref())
                                .is_some();
                    }
                    let is_primary = primary_id == Some(reg.owner.saved_id.as_str())
                        || state.environments.environments.iter().any(|e| e.id == reg.owner.saved_id && e.local);
                    let saved_label = state
                        .environments
                        .environments
                        .iter()
                        .find(|e| e.id == reg.owner.saved_id)
                        .map(|e| e.label.as_str());
                    let label = Self::resolved_host_label(is_primary, Some(&reg.environment_label), saved_label, &reg.owner.saved_id);
                    let saved_id = reg.owner.saved_id.clone();
                    let canonical_environment_id = Self::canonical_env_id_for(state, &saved_id, Some(&reg.owner));
                    group_options.push(HostOption {
                        owner: Some(reg.owner),
                        saved_id,
                        canonical_environment_id,
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
                let runtime_label = state.server_config.config.as_ref().map(|cfg| cfg.environment.label.as_str());
                let canonical_environment_id = Self::canonical_env_id_for(state, &saved.id, owner.as_ref());
                Some(HostOption {
                    owner,
                    saved_id: saved.id.clone(),
                    canonical_environment_id,
                    project_id: active_project.unwrap_or_default().to_owned(),
                    label: Self::resolved_host_label(is_primary, runtime_label, Some(&saved.label), &saved.id),
                    is_primary,
                })
            });
        let mut add_targets = Vec::new();
        if active_remote_url {
            let mut has_project: HashSet<String> = group_options.iter().map(|opt| opt.saved_id.clone()).collect();
            if let Some(env_id) = active_env {
                has_project.insert(env_id.to_owned());
            }
            for entry in &state.fleet.environments {
                if has_project.contains(&entry.summary.owner.saved_id)
                    || !entry.summary.config_loaded
                    || !state.scoped_ready(&entry.summary.owner, t3_core::auth::SOURCE_CONTROL_WRITE_SCOPE)
                {
                    continue;
                }
                let is_primary = primary_id == Some(entry.summary.owner.saved_id.as_str())
                    || state.environments.environments.iter().any(|e| e.id == entry.summary.owner.saved_id && e.local);
                let saved_label = state
                    .environments
                    .environments
                    .iter()
                    .find(|e| e.id == entry.summary.owner.saved_id)
                    .map(|e| e.label.as_str());
                let runtime_label = entry.config.config.as_ref().map(|cfg| cfg.environment.label.as_str());
                let label = Self::resolved_host_label(is_primary, runtime_label, saved_label, &entry.summary.owner.saved_id);
                let canonical_environment_id = Self::canonical_env_id_for(state, &entry.summary.owner.saved_id, Some(&entry.summary.owner));
                add_targets.push(HostOption {
                    owner: Some(entry.summary.owner.clone()),
                    saved_id: entry.summary.owner.saved_id.clone(),
                    canonical_environment_id,
                    project_id: String::new(),
                    label,
                    is_primary,
                });
            }
            add_targets.sort_by_key(|a| a.label.to_lowercase());
        }
        let can_pick = group_options.len() > 1 || !add_targets.is_empty();
        if group_options.is_empty()
            && let Some(active_opt) = active.clone()
        {
            group_options.push(active_opt);
        }
        let show = can_pick || active.as_ref().is_some_and(|opt| !opt.is_primary);
        if !show {
            return HostOptions::default();
        }
        HostOptions { logical_project_key, active, options: group_options, add_targets, can_pick }
    }

    pub(super) fn load_balancing_enabled(&self, cx: &App) -> bool {
        crate::settings::store::try_client(cx)
            .is_some_and(t3_core::settings::read_load_balancing_enabled)
    }

    pub(super) fn load_balancing_weights(&self, cx: &App) -> HashMap<String, f64> {
        crate::settings::store::try_client(cx)
            .map(t3_core::settings::read_load_balancing_weights)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, f64::from(v)))
            .collect()
    }

    /// Environment IDs of `@thread` contexts currently referenced in the draft prompt
    /// (`referencedThreadContexts` in `composerSend.logic.ts`).
    pub(super) fn referenced_thread_environment_ids(&self, cx: &App) -> Vec<String> {
        let Some(target) = self.snapshot.target.as_ref() else {
            return Vec::new();
        };
        let prompt = self.draft_prompt(cx);
        let referenced = t3_core::composer::menu::referenced_context_ids(&prompt);
        if referenced.is_empty() {
            return Vec::new();
        }
        let mut ids = Vec::new();
        for mention in self.mentions(target) {
            if referenced.contains(&mention.context_id.as_str())
                && !mention.environment_id.is_empty()
                && !ids.contains(&mention.environment_id)
            {
                ids.push(mention.environment_id.clone());
            }
        }
        for record in self.records_for(target) {
            if t3_core::composer::records::field(record, "kind") != "thread" {
                continue;
            }
            let cid = t3_core::composer::records::field(record, "contextId");
            if !referenced.contains(&cid) {
                continue;
            }
            let env_id = t3_core::composer::records::field(record, "environmentId");
            if !env_id.is_empty() && !ids.iter().any(|existing| existing == env_id) {
                ids.push(env_id.to_owned());
            }
        }
        ids
    }

    /// Whether the Run-on menu offers `"Auto balance"` (`ChatView.tsx:3822-3828`).
    pub(super) fn can_offer_auto_balance(&self, host: &HostOptions, cx: &App) -> bool {
        if self.fixed.is_some() || self.is_rewinding(cx) || host.options.len() <= 1 {
            return false;
        }
        let Some(Target::Draft(project_id)) = self.snapshot.target.as_ref() else {
            return false;
        };
        !self.state.read(cx).is_scratch_project(&project_id.project_id) && self.load_balancing_enabled(cx)
    }

    /// Whether this unsent draft currently runs in automatic load-balancing mode
    /// (`automaticEnvironment` in `ChatView.tsx:1315-1325`).
    pub(super) fn automatic_environment(&self, host: &HostOptions, cx: &App) -> bool {
        if !self.can_offer_auto_balance(host, cx) {
            return false;
        }
        if self.workspace.routing.selection == DraftEnvSelection::Manual {
            return false;
        }
        let has_attachments = !self.staged_attachments().is_empty();
        if has_attachments && self.workspace.routing.resolved_environment_id.is_none() {
            return false;
        }
        let choice = self.workspace_choice();
        if choice.and_then(|c| c.previous.as_ref()).is_some() {
            return false;
        }
        if choice.and_then(|c| c.branch.as_ref()).is_some()
            && self.workspace.routing.selection != DraftEnvSelection::Auto
        {
            return false;
        }
        true
    }

    /// Eligible load-balancing candidates for the current draft's logical project group
    /// (`loadBalancingCandidates` in `useDraftEnvironmentRouting.ts:97-129`).
    pub(super) fn draft_load_balancing_candidates(
        &self,
        host: &HostOptions,
        cx: &App,
    ) -> (Vec<DraftEnvironmentCandidate>, HashMap<String, f64>, Vec<String>) {
        let weights = self.load_balancing_weights(cx);
        let referenced_threads = self.referenced_thread_environment_ids(cx);
        let project_envs: Vec<(String, String)> = host
            .options
            .iter()
            .map(|opt| (opt.saved_id.clone(), opt.project_id.clone()))
            .collect();
        let resolved = self.resolve(cx);
        let selected_driver = (!resolved.driver.is_empty()).then_some(resolved.driver.as_str());
        let active_instance = resolved.model.as_ref().map(|m| m.instance_id.as_str());
        let candidates = self.state.read(cx).load_balancing_candidates_for_project_envs(
            &project_envs,
            selected_driver,
            active_instance,
            &referenced_threads,
            &weights,
        );
        (candidates, weights, referenced_threads)
    }

    /// `autoEnvironmentLabel` (`useDraftEnvironmentRouting.ts:245-253`).
    pub(super) fn draft_auto_environment_label(&self, host: &HostOptions, cx: &App) -> Option<&'static str> {
        let automatic = self.automatic_environment(host, cx);
        if !automatic {
            return None;
        }
        let resolved = self.workspace.routing.resolved_environment_id.as_deref();
        let (pending, failed) = if resolved.is_some() {
            (false, false)
        } else {
            let (candidates, weights, _) = self.draft_load_balancing_candidates(host, cx);
            let ids: Vec<String> = candidates.iter().map(|c| c.environment_id.clone()).collect();
            let eval = self.workspace.routing.cache.evaluate(&ids, &weights, now_ms());
            (eval.pending || self.workspace.routing.task.is_some(), eval.failed)
        };
        auto_environment_label(automatic, resolved, pending, failed)
    }

    /// Schedules a single-shot timer when the earliest fresh host-resource snapshot expires
    /// (`useLoadBalancedEnvironment.ts:30-38`).
    fn schedule_host_resources_expiry(
        &mut self,
        next_expiry_ms: Option<i64>,
        current_ms: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(expiry) = next_expiry_ms else {
            self.workspace.routing.expiry_timer = None;
            return;
        };
        let wait_ms = (expiry.saturating_sub(current_ms) + 1).max(1) as u64;
        self.workspace.routing.expiry_timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(wait_ms))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.workspace.routing.expiry_timer = None;
                this.sync_draft_routing(window, cx);
                cx.notify();
            });
        }));
    }

    fn apply_auto_routed_winner(
        &mut self,
        winner_env_id: &str,
        host: &HostOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = host
            .options
            .iter()
            .find(|opt| opt.canonical_environment_id == winner_env_id || opt.saved_id == winner_env_id)
            .cloned()
        else {
            return;
        };
        self.workspace.routing.resolved_environment_id = Some(target.canonical_environment_id.clone());
        let (from_project, same_env) = {
            let state = self.state.read(cx);
            (
                state.draft_project.clone(),
                state.environments.active.as_deref() == Some(target.saved_id.as_str()),
            )
        };
        if same_env && from_project.as_deref() == Some(target.project_id.as_str()) {
            cx.notify();
            return;
        }
        if let Some(owner) = target.owner
            && self
                .state
                .read(cx)
                .scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE)
        {
            let Some(source) = self.state.read(cx).active_draft() else { return };
            let draft = t3_core::composer::drafts::DraftRef::new(source.id, &target.saved_id, &target.project_id);
            if !self.retarget_session(&draft, same_env, window, cx) {
                return;
            }
            if !same_env {
                self.workspace.routing.pending_navigation =
                    Some((target.saved_id.clone(), target.project_id.clone()));
            }
            self.state.update(cx, |state, cx| {
                state.navigate_scoped(owner, ScopedRouteAction::OpenDraft(draft), cx);
            });
        }
        cx.notify();
    }

    /// Synchronizes automatic draft environment routing on draft open / state updates
    /// (`useLoadBalancedEnvironment.ts` + `useDraftEnvironmentRouting.ts`).
    pub(super) fn sync_draft_routing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.try_finish_pending_routed_send(window, cx);
        if self.fixed.is_some() {
            return;
        }
        let Some(Target::Draft(current_draft)) = self.snapshot.target.clone() else {
            let waiting_on_routed_switch = {
                let state = self.state.read(cx);
                state.selected_thread.is_none()
                    && state.has_pending_scoped_route()
                    && self.workspace.routing.pending_navigation.as_ref().is_some_and(
                        |(saved_id, _)| state.environments.active.as_deref() == Some(saved_id.as_str()),
                    )
            };
            if waiting_on_routed_switch {
                return;
            }
            self.workspace.routing.logical_project_key = None;
            self.workspace.routing.draft_id = None;
            self.workspace.routing.selection = DraftEnvSelection::Implicit;
            self.workspace.routing.resolved_environment_id = None;
            self.workspace.routing.pending_navigation = None;
            self.workspace.routing.task = None;
            self.workspace.routing.expiry_timer = None;
            self.workspace.routing.in_flight_ids.clear();
            return;
        };
        if self.workspace.routing.pending_navigation.as_ref().is_some_and(
            |(saved_id, project_id)| {
                self.state.read(cx).environments.active.as_deref() == Some(saved_id.as_str())
                    && project_id == &current_draft.project_id
            },
        ) {
            self.workspace.routing.pending_navigation = None;
        }
        let host = self.host_options(cx);
        let logical_key = host.logical_project_key.clone().or(Some(current_draft.project_id));
        if logical_key != self.workspace.routing.logical_project_key
            || self.workspace.routing.draft_id.as_ref() != Some(&current_draft.id)
        {
            self.workspace.routing.draft_id = Some(current_draft.id);
            self.workspace.routing.logical_project_key = logical_key;
            self.workspace.routing.selection = DraftEnvSelection::Implicit;
            self.workspace.routing.resolved_environment_id = None;
            self.workspace.routing.generation = self.workspace.routing.generation.wrapping_add(1);
            self.workspace.routing.task = None;
            self.workspace.routing.expiry_timer = None;
            self.workspace.routing.in_flight_ids.clear();
        }
        let automatic = self.automatic_environment(&host, cx);
        let needs_load_balancing = automatic && self.workspace.routing.resolved_environment_id.is_none();
        if !needs_load_balancing || self.sending {
            if !needs_load_balancing {
                self.workspace.routing.task = None;
                self.workspace.routing.in_flight_ids.clear();
            }
            return;
        }
        let (candidates, weights, _) = self.draft_load_balancing_candidates(&host, cx);
        if candidates.is_empty() {
            self.workspace.routing.task = None;
            self.workspace.routing.in_flight_ids.clear();
            return;
        }
        let candidate_ids: Vec<String> = candidates.iter().map(|c| c.environment_id.clone()).collect();
        let current_ms = now_ms();
        let eval = self.workspace.routing.cache.evaluate(&candidate_ids, &weights, current_ms);
        self.schedule_host_resources_expiry(eval.next_expiry_ms, current_ms, window, cx);
        if !eval.pending
            && let Some(winner) = eval.environment_id
        {
            self.apply_auto_routed_winner(&winner, &host, window, cx);
            return;
        }
        let needs_fetch = candidate_ids.iter().any(|env_id| {
            match self.workspace.routing.cache.get(env_id) {
                None => true,
                Some(entry) => {
                    !entry.pending
                        && current_ms.saturating_sub(entry.received_at_ms) > HOST_RESOURCES_FRESH_MS
                }
            }
        });
        if !needs_fetch {
            return;
        }
        if self.workspace.routing.task.is_some() && self.workspace.routing.in_flight_ids == candidate_ids {
            return;
        }
        self.workspace.routing.generation = self.workspace.routing.generation.wrapping_add(1);
        let generation = self.workspace.routing.generation;
        for env_id in &candidate_ids {
            self.workspace.routing.cache.mark_pending(env_id);
        }
        self.workspace.routing.in_flight_ids = candidate_ids.clone();
        let fetch = self.state.read(cx).fetch_load_balancing_resources(&candidates);
        let ids_for_eval = candidate_ids;
        self.workspace.routing.task = Some(cx.spawn_in(window, async move |this, cx| {
            let samples = fetch.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.workspace.routing.generation != generation {
                    return;
                }
                this.workspace.routing.task = None;
                this.workspace.routing.in_flight_ids.clear();
                for (env_id, resources, received_at_ms) in samples {
                    this.workspace.routing.cache.record(&env_id, resources, received_at_ms);
                }
                let host = this.host_options(cx);
                let weights = this.load_balancing_weights(cx);
                let now = now_ms();
                let eval = this.workspace.routing.cache.evaluate(&ids_for_eval, &weights, now);
                this.schedule_host_resources_expiry(eval.next_expiry_ms, now, window, cx);
                if !this.sending
                    && this.automatic_environment(&host, cx)
                    && this.workspace.routing.resolved_environment_id.is_none()
                    && !eval.pending
                    && let Some(winner) = eval.environment_id
                {
                    this.apply_auto_routed_winner(&winner, &host, window, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Triggered when the user selects `"Auto balance"` in the Run-on selector
    /// (`onAutoEnvironment` in `useDraftEnvironmentRouting.ts:222-243`).
    pub(super) fn select_auto_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.host_open = false;
        if self.is_rewinding(cx) || self.sending || !matches!(self.snapshot.target, Some(Target::Draft(_))) {
            return;
        }
        if !self.staged_attachments().is_empty() {
            let warn = attachments_block_auto_routing_warning();
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(ToastKind::Warning, warn.title)
                        .description(warn.description)
                        .key("load-balancing-attachments"),
                    cx,
                );
            });
            self.focus(window, cx);
            cx.notify();
            return;
        }
        if let Some(Target::Draft(project_id)) = self.snapshot.target.clone() {
            let target = Target::Draft(project_id);
            let storage_key = drafts::key(&target);
            if let Some(mut draft) = self.drafts.store.get(&storage_key).cloned()
                && let Some(ws) = draft.workspace.as_mut()
                && (ws.branch.is_some() || ws.previous.is_some())
            {
                ws.branch = None;
                ws.previous = None;
                if self.mutate_drafts(cx, |store| store.set(&storage_key, draft)) {
                    self.schedule_draft_save(&target, cx);
                }
            }
        }
        let host = self.host_options(cx);
        for opt in &host.options {
            self.workspace.routing.cache.record(&opt.canonical_environment_id, None, 0);
        }
        self.workspace.routing.selection = DraftEnvSelection::Auto;
        self.workspace.routing.resolved_environment_id = None;
        self.workspace.routing.generation = self.workspace.routing.generation.wrapping_add(1);
        self.workspace.routing.task = None;
        self.workspace.routing.in_flight_ids.clear();
        self.sync_draft_routing(window, cx);
        self.focus(window, cx);
        cx.notify();
    }

    /// Resets manual machine pinning when the user chooses a project in `DraftHero`
    /// so automatic load balancing evaluates the newly picked project.
    pub(crate) fn prepare_draft_routing_on_project_pick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace.routing.selection = DraftEnvSelection::Implicit;
        self.workspace.routing.resolved_environment_id = None;
        self.workspace.routing.generation = self.workspace.routing.generation.wrapping_add(1);
        self.workspace.routing.task = None;
        self.workspace.routing.in_flight_ids.clear();
        self.sync_draft_routing(window, cx);
    }

    pub(super) fn select_host(&mut self, option: HostOption, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.host_open = false;
        self.workspace.routing.selection = DraftEnvSelection::Manual;
        self.workspace.routing.resolved_environment_id = None;
        self.workspace.routing.generation = self.workspace.routing.generation.wrapping_add(1);
        self.workspace.routing.task = None;
        self.workspace.routing.in_flight_ids.clear();
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
        let Some(source) = self.state.read(cx).active_draft() else { return };
        let draft = t3_core::composer::drafts::DraftRef::new(source.id, &option.saved_id, &option.project_id);
        if !self.retarget_session(&draft, same_env, window, cx) {
            return;
        }
        if !same_env {
            self.workspace.routing.pending_navigation = Some((option.saved_id, option.project_id));
        }
        self.state.update(cx, |state, cx| {
            state.navigate_scoped(owner, ScopedRouteAction::OpenDraft(draft), cx);
        });
        self.focus(window, cx);
        cx.notify();
    }

    fn render_host_selector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let host = self.host_options(cx);
        let active = host.active.clone()?;
        let p = palette(cx);
        let state = self.state.read(cx);
        let is_draft = matches!(self.snapshot.target, Some(Target::Draft(_)));
        let offer_auto = self.can_offer_auto_balance(&host, cx);
        let auto_label = self.draft_auto_environment_label(&host, cx);
        let icon = if auto_label.is_some() {
            Icon::Gauge
        } else {
            environment_machine_icon(state, &active.saved_id)
        };
        let trigger_label = auto_label.map_or_else(|| active.label.clone(), str::to_owned);
        let interactive = is_draft && host.can_pick && !self.is_rewinding(cx);
        if !interactive {
            let muted = hsla(p.muted_foreground).opacity(0.7);
            let tooltip_label = active.label.clone();
            return Some(
                h_flex()
                    .id("composer-host-indicator")
                    .debug_selector(|| "composer-host-indicator".to_owned())
                    .h(px(24.))
                    .px(px(7.))
                    .gap(px(6.))
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
                    .child(icon.render(px(12.), muted))
                    .child(div().max_w(px(180.)).truncate().child(active.label))
                    .into_any_element(),
            );
        }
        let tooltip_base = match (auto_label, self.workspace.routing.resolved_environment_id.as_deref()) {
            (Some(label), Some(_)) => format!("{label} ({})", active.label),
            (Some(label), None) => label.to_owned(),
            (None, _) => active.label.clone(),
        };
        let tooltip = crate::keymap::label("composer.host", cx)
            .map_or_else(|| tooltip_base.clone(), |shortcut| format!("{tooltip_base} · {shortcut}"));
        let mut popup = menu_surface(cx)
            .w(px(260.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.workspace.host_open = false;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("Run on"));
        let auto_offset = usize::from(offer_auto);
        if offer_auto {
            popup = popup.child(
                MenuItem::new("host-opt-auto", auto_label.unwrap_or("Auto balance"))
                    .icon(Icon::Gauge, hsla(p.muted_foreground))
                    .checked(auto_label.is_some())
                    .highlighted(self.workspace.host_highlighted == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.select_auto_host(window, cx))),
            );
        }
        let active_env = state.environments.active.as_deref();
        let add_index = auto_offset + host.options.len();
        for (index, option) in host.options.into_iter().enumerate() {
            let checked = auto_label.is_none() && active_env == Some(option.saved_id.as_str());
            let highlighted = self.workspace.host_highlighted == index + auto_offset;
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
        if !host.add_targets.is_empty() {
            let mut section = div().mt(px(4.)).pt(px(4.)).border_t_1().border_color(hsla(p.border));
            if host.add_targets.len() == 1 {
                let target_id = host.add_targets[0].saved_id.clone();
                section = section.child(
                    MenuItem::new("host-add-machine", "Add to another machine")
                        .icon(Icon::Plus, hsla(p.muted_foreground))
                        .highlighted(self.workspace.host_highlighted == add_index)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_add_project_to_machine(Some(target_id.clone()), window, cx)
                        })),
                );
            } else {
                section = section.child(
                    div()
                        .px(px(8.))
                        .py(px(4.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Add to another machine"),
                );
                for (offset, target) in host.add_targets.iter().enumerate() {
                    let target_id = target.saved_id.clone();
                    let target_icon = environment_machine_icon(state, &target.saved_id);
                    section = section.child(
                        MenuItem::new(("host-add-machine", offset), target.label.clone())
                            .icon(target_icon, hsla(p.muted_foreground))
                            .highlighted(self.workspace.host_highlighted == add_index + offset)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_add_project_to_machine(Some(target_id.clone()), window, cx)
                            })),
                    );
                }
            }
            popup = popup.child(section);
        }
        Some(
            div()
                .relative()
                .min_w_0()
                .debug_selector(|| "composer-host".to_owned())
                .child(
                    ComposerControl::new("composer-host")
                        .icon(icon)
                        .label(trigger_label)
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
        let ref_count = self.workspace.refs.refs.len();
        let status_text = self.base_branch_status_text();
        let empty = !self.workspace.refs.loading && self.workspace.refs.error.is_none() && ref_count == 0;
        let has_next_page = self.workspace.refs.next_cursor.is_some();
        let loading_next = self.workspace.refs.loading_next_page;
        if self.workspace.branch_open && has_next_page && !self.workspace.refs.loading {
            let offset = self.workspace.base_scroll.offset();
            let max = self.workspace.base_scroll.max_offset();
            if max.y > px(0.) && -offset.y + px(36.) >= max.y {
                cx.spawn(async move |this, cx| {
                    let _ = this.update(cx, |this, cx| this.load_more_base_refs(cx));
                })
                .detach();
            }
        }
        let mut popup = menu_surface(cx)
            .w(px(320.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.close_base_branch_picker(false, window, cx);
                cx.notify();
            }))
            .when_some(self.workspace.base_search.as_ref(), |this, search| {
                this.child(
                    h_flex()
                        .px(px(8.))
                        .py(px(6.))
                        .gap(px(6.))
                        .items_center()
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .child(Icon::Search.sized(px(14.)).text_color(hsla(p.muted_foreground).opacity(0.6)))
                        .child(div().flex_1().h(px(24.)).flex().items_center().theme_slot(ThemeSlot::Input, cx).child(Input::new(search).appearance(false).bordered(false))),
                )
            });
        let mut refs_list = div()
            .id("base-branch-refs")
            .max_h(px(220.))
            .overflow_y_scroll()
            .track_scroll(&self.workspace.base_scroll);
        if empty {
            refs_list = refs_list.child(
                div()
                    .py(px(16.))
                    .flex()
                    .justify_center()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child("No refs found."),
            );
        } else {
            for (index, reference) in self.workspace.refs.refs.iter().enumerate() {
                let name = reference.name.clone();
                let checked = branch.as_deref() == Some(name.as_str());
                let highlighted = self.workspace.branch_highlighted == index;
                let name_to_select = name.clone();
                let row = MenuItem::new(("base-branch-ref", index), middle_truncate(&name, 28))
                    .checked(checked)
                    .highlighted(highlighted)
                    .on_click(cx.listener(move |this, _, window, cx| this.choose_base_branch(name_to_select.clone(), window, cx)));
                let row = if reference.is_remote() {
                    row.trailing(div().text_xs().text_color(hsla(p.muted_foreground)).child(t(cx, "remote")))
                } else {
                    row
                };
                refs_list = refs_list.child(branch_copy_menu(div().id(("base-branch-copy", index)).child(row), name));
            }
            if has_next_page && !loading_next {
                refs_list = refs_list.child(
                    div()
                        .id("base-branch-load-more")
                        .px(px(8.))
                        .py(px(5.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(p.accent).opacity(0.45)).text_color(hsla(p.foreground)))
                        .on_click(cx.listener(|this, _, _, cx| this.load_more_base_refs(cx)))
                        .child("Load more refs..."),
                );
            }
        }
        let owner = cx.entity().downgrade();
        popup = popup
            .child(refs_list)
            .when_some(status_text, |this, status| {
                this.child(
                    div()
                        .id("base-branch-status")
                        .debug_selector(|| "base-branch-status".to_owned())
                        .px(px(8.))
                        .py(px(5.))
                        .border_t_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(status),
                )
            })
            .child(
                div()
                    .id("base-branch-origin-wrap")
                    .mt(px(4.))
                    .pt(px(4.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .tooltip(|window, cx| Tooltip::new(t(cx, START_FROM_REMOTE_TOOLTIP)).build(window, cx))
                    .child(
                        h_flex().px(px(8.)).py(px(4.)).justify_between()
                            .when(self.workspace.branch_highlighted == ref_count, |row| row.bg(hsla(p.accent)).rounded(px(4.)))
                            .child(div().text_sm().child(t(cx, "Start from remote")))
                            .child(Switch::new("base-branch-origin", start_from_origin)
                                .on_change(move |_, window, cx| {
                                    let _ = owner.update(cx, |this, cx| this.toggle_start_from_origin(window, cx));
                                })),
                    ),
            );
        Some(
            div()
                .relative()
                .min_w_0()
                .debug_selector(|| "composer-base-branch".to_owned())
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
        let Some(project) = state.active_draft() else { return };
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

#[cfg(all(test, feature = "snapshot"))]
mod add_to_machine_tests {
    use super::*;
    use crate::app_state::AppEvent;
    use gpui::{Render, size};
    use serde_json::json;

    #[gpui::test]
    async fn composer_add_to_another_machine_opens_project_dialog_for_active_host_project(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });

        state.update(cx, |s, cx| {
            let mut snapshot = s.shell.snapshot.clone().unwrap_or_default();
            snapshot.projects.push(
                serde_json::from_value(json!({
                    "id": "proj-src",
                    "title": "t3code",
                    "workspaceRoot": "/repo/t3code",
                    "repositoryIdentity": {
                        "canonicalKey": "github.com/pingdotgg/t3code",
                        "displayName": "pingdotgg/t3code",
                        "rootPath": "/repo/t3code",
                        "locator": { "remoteUrl": "https://github.com/pingdotgg/t3code.git" }
                    },
                    "createdAt": "2026-01-01T00:00:00Z",
                    "updatedAt": "2026-01-02T00:00:00Z"
                }))
                .unwrap(),
            );
            s.shell.snapshot = Some(snapshot);
            s.shell.synchronized = true;
            s.server_config.config = Some(
                serde_json::from_value(json!({
                    "environment": { "environmentId": "fixture-env", "label": "MacBook Pro", "capabilities": {} },
                    "providers": [],
                    "settings": {},
                    "issues": [],
                    "keybindings": []
                }))
                .unwrap(),
            );
            s.environments.environments[0].local = true;
            s.environments.environments[0].label = "MacBook Pro".into();
            s.environments.environments.push(
                serde_json::from_value(json!({
                    "id": "remote-1",
                    "label": "Build Box",
                    "httpBase": "http://remote.invalid",
                    "environmentId": "env-remote"
                }))
                .unwrap(),
            );

            let owner_local = t3_core::sidebar::grouping::EnvironmentOwner {
                saved_id: "saved-fixture".into(),
                canonical_id: Some("fixture-env".into()),
                generation: 1,
                connection_generation: 1,
                shell_generation: 1,
            };
            let owner_remote = t3_core::sidebar::grouping::EnvironmentOwner {
                saved_id: "remote-1".into(),
                canonical_id: Some("env-remote".into()),
                generation: 1,
                connection_generation: 1,
                shell_generation: 1,
            };
            let mut config_remote = t3_core::server_config::ServerConfigState::default();
            config_remote.config = Some(
                serde_json::from_value(json!({
                    "environment": { "environmentId": "env-remote", "label": "Build Box", "capabilities": {} },
                    "providers": [],
                    "settings": {},
                    "issues": [],
                    "keybindings": []
                }))
                .unwrap(),
            );
            let grants: t3_core::auth::AuthSessionState = serde_json::from_value(json!({
                "authenticated": true,
                "scopes": ["source-control:write", "orchestration:read"]
            }))
            .unwrap();
            let mut remote_shell = t3_core::shell::ShellState::default();
            remote_shell.synchronized = true;
            s.fleet = std::sync::Arc::new(t3_client::environments::FleetView {
                revision: 1,
                environments: vec![
                    std::sync::Arc::new(t3_client::environments::EnvironmentView {
                        summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                            owner_local,
                            &s.shell,
                            &s.server_config,
                        )),
                        status: t3_client::supervisor::ConnectionStatus::Connected,
                        config: s.server_config.clone(),
                        grants: Some(grants.clone()),
                        settings_revision: 0,
                        keybindings_revision: 0,
                    }),
                    std::sync::Arc::new(t3_client::environments::EnvironmentView {
                        summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                            owner_remote,
                            &remote_shell,
                            &config_remote,
                        )),
                        status: t3_client::supervisor::ConnectionStatus::Connected,
                        config: config_remote,
                        grants: Some(grants),
                        settings_revision: 0,
                        keybindings_revision: 0,
                    }),
                ],
            });
            s.sidebar.grouping_projects = s
                .fleet
                .environments
                .iter()
                .flat_map(|entry| entry.summary.projects.iter().cloned())
                .collect();
            s.sidebar.grouping_threads = s
                .fleet
                .environments
                .iter()
                .flat_map(|entry| entry.summary.threads.iter().cloned())
                .collect();
            s.rebuild_sidebar(cx);
            s.start_draft("proj-src", cx);
            cx.emit(AppEvent::Shell);
        });

        struct Host {
            composer: Entity<Composer>,
            dialogs: Entity<crate::projects::ProjectDialogs>,
        }
        impl Render for Host {
            fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                // Keep `Composer`'s native macOS `Input` out of `TestWindow::draw` so `set_text_content_type`
                // does not call unimplemented `TestWindow::window_handle`.
                div().size_full().child(self.dialogs.clone())
            }
        }

        let (host, vcx) = cx.add_window_view(|window, cx| Host {
            composer: cx.new(|cx| Composer::new(state.clone(), window, cx)),
            dialogs: cx.new(|cx| crate::projects::ProjectDialogs::new(state.clone(), window, cx)),
        });
        vcx.update(|window, cx| {
            window.resize(size(px(960.), px(640.)));
            let composer = host.read(cx).composer.clone();
            let options = composer.read(cx).host_options(cx);
            assert_eq!(options.add_targets.len(), 1);
            assert_eq!(options.add_targets[0].saved_id, "remote-1");
            composer.update(cx, |c, cx| {
                c.open_host(window, cx);
                assert!(c.workspace_key(MenuKey::Down, window, cx));
                assert!(c.workspace_key(MenuKey::Accept, window, cx));
            });
            window.blur();
        });
        vcx.run_until_parked();

        vcx.read_entity(&host, |h, cx| {
            let dialogs = h.dialogs.read(cx);
            assert!(dialogs.is_open(), "Add to another machine must open ProjectDialogs");
            let summary = dialogs
                .add_to_machine_summary_for_test()
                .expect("AddProjectToMachineDialog must be active");
            assert_eq!(summary.0, "saved-fixture");
            assert_eq!(summary.1, "/repo/t3code");
            assert_eq!(summary.2.as_deref(), Some("remote-1"));
        });

        cx.update(|cx| {
            state.update(cx, |s, _| s.stop_pump());
        });
        runtime.shutdown_background();
    }
}
