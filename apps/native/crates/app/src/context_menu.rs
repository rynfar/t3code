//! The thread context menu (design-system §6.3), anchored at the pointer. Items come from
//! `t3_core::thread_actions::build_menu`; actions the app can run go to `AppState`, the rest
//! (rename, delete) are emitted for the sidebar and workspace.

use gpui::{
    AnyElement, App, BoxShadow, Context, MouseButton, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent,
    ParentElement, Pixels, Point, Render, SharedString, Styled, Window, anchored, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use t3_core::sidebar::grouping::{GroupingSettings, ProjectGroup, ProjectScope, build_project_groups};
use t3_core::sidebar::selection::{BulkAction, build_bulk_menu};
use t3_core::sidebar::settled::{self, SnoozePreset};
use t3_core::thread_actions::{Available, MenuItem, ProjectAction, Supports, ThreadAction, ThreadActionMenuState, build_menu, build_project_menu};

use crate::app_state::AppState;
use crate::icons::Icon;
use crate::snooze_dialog::SnoozeRequest;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};

const MENU_WIDTH: f32 = 220.;
const ITEM_HEIGHT: f32 = 28.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Thread(ThreadAction),
    Project(ProjectAction),
    Bulk(BulkAction),
    DiscardDraft,
}

pub enum ContextMenuEvent {
    DiscardDraft(String),
    Rename(ThreadMenuOwner, Option<FocusHandle>),
    Delete(ThreadMenuOwner, Option<FocusHandle>),
    CustomSnooze(SnoozeRequest),
    BulkDelete(Vec<String>),
}

/// The exact destination captured by a thread menu or one of its delayed callbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMenuOwner {
    pub thread: String,
    scope: (String, Option<String>, u64),
    generation: u64,
    shell_generation: u64,
    created_at: String,
    project: String,
    project_created: String,
    root: String,
    branch: Option<String>,
    worktree: Option<String>,
}
impl ThreadMenuOwner {
    pub(crate) fn capture(state: &AppState, id: &str) -> Option<Self> {
        if state.phase != crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
            || !state.shell.synchronized { return None; }
        let thread = state.thread_shell(id)?;
        if thread.archived_at.is_some() || thread.deleted_at.is_some() { return None; }
        let project = state.shell.project(&thread.project_id)?;
        if project.archived_at.is_some() { return None; }
        Some(Self { thread:id.into(), scope:state.thread_copy_scope()?,
            generation:*state.client_handle()?.connection_generation().borrow(), shell_generation:state.shell.generation,
            created_at:thread.created_at.clone(), project:thread.project_id.clone(), project_created:project.created_at.clone(),
            root:project.workspace_root.clone(), branch:thread.branch.clone(), worktree:thread.worktree_path.clone() })
    }
    fn response_scope_current(&self, state: &AppState) -> bool {
        state.thread_copy_scope().as_ref() == Some(&self.scope)
            && state.phase == crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
            && state.shell.synchronized && state.shell.generation == self.shell_generation
            && state.client_handle().is_some_and(|client| *client.connection_generation().borrow() == self.generation)
            && state.shell.project(&self.project).is_some_and(|project| project.created_at == self.project_created && project.workspace_root == self.root && project.archived_at.is_none())
    }
    pub(crate) fn response_current(&self, state: &AppState) -> bool {
        self.response_scope_current(state) && state.thread_shell(&self.thread).is_some_and(|thread| thread.created_at == self.created_at && thread.deleted_at.is_none())
    }
    pub(crate) fn response_after_placement(&self, state: &AppState) -> bool {
        // Placement commits can remove a row from the bounded shell before their reply.
        self.response_scope_current(state)
            && !(state.selected_thread.as_deref() == Some(self.thread.as_str()) && (state.thread.deleted || state.thread.projection.as_ref().is_some_and(|projection| projection.thread.created_at != self.created_at || projection.thread.deleted_at.is_some())))
            && state.thread_shell(&self.thread).is_none_or(|thread| thread.created_at == self.created_at && thread.deleted_at.is_none())
    }
    pub(crate) fn current(&self, state: &AppState) -> bool { Self::capture(state, &self.thread).as_ref() == Some(self) }
    pub(crate) fn can_operate(state: &AppState) -> bool {
        state.sidebar.capabilities.config_loaded && state.sidebar.capabilities.can_operate
            && state.client_handle().is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)))
    }
    pub(crate) fn allows(&self, state: &AppState, action: &ThreadAction) -> bool {
        if !self.current(state) || (action.requires_operate() && !Self::can_operate(state)) { return false; }
        match action {
            ThreadAction::ShareGuest => state.can_share_with_guest(),
            ThreadAction::TurnIntoProject => state.can_turn_into_project(&self.thread),
            ThreadAction::Archive => state.sidebar.model.thread(&self.thread).is_some_and(|thread| thread.can_archive()),
            ThreadAction::RegenerateTitle => state.sidebar.capabilities.config_loaded && state.sidebar.model.thread(&self.thread).is_some_and(|thread| !thread.regenerating_title),
            ThreadAction::AutoSettle | ThreadAction::AutoSettleEnabled | ThreadAction::AutoSettleDisabled => state.sidebar.capabilities.auto_settle_opt_out,
            ThreadAction::Pin | ThreadAction::Unpin | ThreadAction::Settle | ThreadAction::Unsettle => state.sidebar.capabilities.config_loaded,
            ThreadAction::NewThreadOnBranch | ThreadAction::CopyBranch => self.branch.as_ref().is_some_and(|branch| !branch.is_empty()),
            _ => true,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ProjectFilterMember {
    id: String,
    root: String,
    created_at: String,
    archived_at: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
struct ProjectFilterOwner {
    environment: String,
    saved_environment: Option<String>,
    connection: u64,
    generation: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct ThreadProjectFilter {
    owner: Option<ProjectFilterOwner>,
    shell_generation: u64,
    thread: String,
    created_at: String,
    project: String,
    group: ProjectGroup,
    grouping: GroupingSettings,
    members: Vec<ProjectFilterMember>,
}

impl ThreadProjectFilter {
    fn capture(state: &AppState, thread_id: &str) -> Option<Self> {
        let thread = state.thread_shell(thread_id)?;
        if thread.archived_at.is_some() || thread.deleted_at.is_some() {
            return None;
        }
        let environment = &state
            .server_config
            .config
            .as_ref()?
            .environment
            .environment_id;
        let snapshot = state.shell.snapshot.as_ref()?;
        let grouping = state.sidebar.settings.grouping.clone();
        let group = build_project_groups(environment, &snapshot.projects, &grouping)
            .into_iter()
            .find(|group| {
                !group.archived && group.member_project_ids.contains(&thread.project_id)
            })?;
        let members = group
            .member_project_ids
            .iter()
            .map(|id| {
                let project = state.shell.project(id)?;
                Some(ProjectFilterMember {
                    id: id.clone(),
                    root: project.workspace_root.clone(),
                    created_at: project.created_at.clone(),
                    archived_at: project.archived_at.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let owner =
            state
                .thread_copy_scope()
                .and_then(|(environment, saved_environment, connection)| {
                    Some(ProjectFilterOwner {
                        environment,
                        saved_environment,
                        connection,
                        generation: *state.client_handle()?.connection_generation().borrow(),
                    })
                });
        Some(Self {
            owner,
            shell_generation: state.shell.generation,
            thread: thread_id.to_owned(),
            created_at: thread.created_at.clone(),
            project: thread.project_id.clone(),
            group,
            grouping,
            members,
        })
    }

    fn can_apply(&self, state: &AppState) -> bool {
        self.owner.is_some()
            && state.sidebar.capabilities.config_loaded
            && state.sidebar.capabilities.can_operate
            && state.client_handle().is_some_and(|client| {
                client
                    .session_state()
                    .borrow()
                    .as_ref()
                    .is_some_and(|session| {
                        session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
                    })
            })
            && Self::capture(state, &self.thread).as_ref() == Some(self)
    }

    fn apply(self, state: &mut AppState, cx: &mut Context<AppState>) {
        if !self.can_apply(state) {
            return;
        }
        let mut scope = ProjectScope::default();
        if !state.sidebar.project_filter_scope.is_selected(&self.group) {
            scope.toggle(&self.group, std::slice::from_ref(&self.group), true);
        }
        state.set_project_scope(scope, cx);
    }
}

struct OpenMenu {
    /// The thread or project the menu acts on.
    target_id: String,
    items: Vec<MenuItem<MenuAction>>,
    position: Point<Pixels>,
    highlighted: Option<usize>,
    submenu: Option<usize>,
    sub_highlighted: Option<usize>,
    presets: Vec<SnoozePreset>,
    /// The selected threads a bulk menu acts on.
    selection: Vec<String>,
    snooze_request: Option<SnoozeRequest>,
    copy_scope: Option<(String, Option<String>, u64)>,
    project_filter: Option<ThreadProjectFilter>,
    owner: Option<ThreadMenuOwner>,
    previous_focus: Option<FocusHandle>,
    window: gpui::AnyWindowHandle,
    header: bool,
}

pub struct ContextMenu {
    state: Entity<AppState>,
    open: Option<OpenMenu>,
    focus: FocusHandle,
}

impl EventEmitter<ContextMenuEvent> for ContextMenu {}
impl EventEmitter<DismissEvent> for ContextMenu {}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ContextMenu {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        ContextMenu { state, open: None, focus: cx.focus_handle() }
    }

    pub fn open_for_thread(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_thread(thread_id, position, true, window, cx);
    }

    pub fn open_for_header(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_thread(thread_id, position, false, window, cx);
    }

    fn open_thread(&mut self, thread_id: &str, position: Point<Pixels>, sidebar: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread) = state.sidebar.model.thread(thread_id) else { return };
        let project_filter = sidebar.then(|| ThreadProjectFilter::capture(state, thread_id)).flatten();
        let now = now_ms();
        let presets = state.snooze_presets();
        let caps = state.sidebar.capabilities;
        let loaded = caps.config_loaded;
        let supports = Supports { settlement: loaded, auto_settle_opt_out: caps.auto_settle_opt_out, snooze: loaded, pinning: loaded, title_regeneration: loaded };
        let items = build_menu(&ThreadActionMenuState {
            can_operate: ThreadMenuOwner::can_operate(state),
            can_find: !sidebar && state.selected_thread.as_deref() == Some(thread_id),
            can_share_with_guest: state.can_share_with_guest(),
            can_turn_into_project: state.can_turn_into_project(thread_id),
            branch: thread.branch.as_deref(),
            project_filter: project_filter.as_ref().map(|filter| (filter.group.label.as_str(), state.sidebar.project_filter_scope.is_selected(&filter.group))),
            is_pinned: thread.pinned,
            is_settled: thread.is_settled(),
            auto_settle_enabled: !thread.auto_settle_disabled,
            is_snoozed: settled::effective_snoozed(thread, now),
            can_snooze_now: settled::can_snooze(thread, now),
            is_regenerating_title: thread.regenerating_title,
            is_running: !thread.can_archive(),
            supports,
            snooze_presets: &presets,
            available: Available { new_thread_on_branch: true, custom_snooze: true, project_settings: true },
        });
        let snooze_request = SnoozeRequest::capture(state, vec![thread_id.to_owned()], false, window.focused(cx));
        let can_snooze = snooze_request.plan_for(state, now).is_some();
        let items = items.into_iter().map(|item| {
            let mut item = item.map(&MenuAction::Thread);
            if item.action == MenuAction::Thread(ThreadAction::FilterByProject) {
                item.disabled = project_filter.as_ref().is_none_or(|filter| !filter.can_apply(state));
            }
            if item.action == MenuAction::Thread(ThreadAction::Unsnooze) && snooze_request.plan_for_action(state, now, false).is_none() {
                item.disabled = true;
            }
            if item.action == MenuAction::Thread(ThreadAction::Snooze) && !can_snooze {
                item.disabled = true;
                for child in &mut item.children { child.disabled = true; }
            }
            item
        }).collect();
        self.open = Some(OpenMenu { target_id: thread_id.to_owned(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets, selection: Vec::new(), snooze_request: Some(snooze_request), copy_scope: state.thread_copy_scope(), project_filter, owner: ThreadMenuOwner::capture(state, thread_id), previous_focus: window.focused(cx), window: window.window_handle().into(), header: !sidebar });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn open_for_draft(&mut self, project: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let mut copy = MenuItem::new(MenuAction::Project(ProjectAction::CopyPath), "Copy", Some("copy"));
        copy.disabled = self.state.read(cx).shell.project(project).is_none();
        if !copy.disabled { copy.children.push(MenuItem::new(MenuAction::Project(ProjectAction::CopyPath), "Path", Some("folder"))); }
        let mut discard = MenuItem::new(MenuAction::DiscardDraft, "Discard draft", Some("trash"));
        discard.destructive = true;
        discard.separator_before = true;
        let mut items = vec![copy];
        if self.state.read(cx).shell.project(project).is_some() {
            items.push(MenuItem::new(MenuAction::Project(ProjectAction::Settings), "Project settings", Some("settings")));
        }
        items.push(discard);
        self.open = Some(OpenMenu {target_id: project.into(), items, position, highlighted:None, submenu:None,sub_highlighted:None,presets:Vec::new(),selection:Vec::new(),snooze_request:None,copy_scope:None,project_filter:None,owner:None,previous_focus:window.focused(cx),window:window.window_handle().into(),header:false});
        window.focus(&self.focus,cx);
        cx.notify();
    }

    /// The multi-select menu (`handleMultiSelectContextMenu`).
    pub fn open_for_selection(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let (selection, summary) = state.bulk_selection();
        if selection.is_empty() {
            return;
        }
        let presets = state.snooze_presets();
        let snooze_request = SnoozeRequest::capture(state, selection.clone(), true, window.focused(cx));
        let can_snooze = snooze_request.plan_for(state, now_ms()).is_some();
        let items = build_bulk_menu(&summary, &presets).into_iter().map(|item| {
            let mut item = item.map(&MenuAction::Bulk);
            if item.action == MenuAction::Bulk(BulkAction::Snooze) && !can_snooze {
                item.disabled = true;
                for child in &mut item.children { child.disabled = true; }
            }
            item
        }).collect();
        self.open = Some(OpenMenu { target_id: String::new(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets, selection, snooze_request: Some(snooze_request), copy_scope: None, project_filter: None, owner: None, previous_focus: window.focused(cx), window: window.window_handle().into(), header: false });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The project menu, from a project row in the project filter.
    pub fn open_for_project(&mut self, project_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(project) = state.shell.project(project_id) else { return };
        let items = build_project_menu(project.pinned_at.is_some(), state.can_pin_projects()).into_iter().map(|item| item.map(&MenuAction::Project)).collect();
        self.open = Some(OpenMenu { target_id: project_id.to_owned(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets: Vec::new(), selection: Vec::new(), snooze_request: None, copy_scope: None, project_filter: None, owner: None, previous_focus: window.focused(cx), window: window.window_handle().into(), header: false });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The row's Snooze button (`SnoozeMenuButton`).
    pub fn open_snooze(&mut self, thread_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let snooze_request = SnoozeRequest::capture(state, vec![thread_id.to_owned()], false, window.focused(cx));
        if snooze_request.plan_for(state, now_ms()).is_none() { return; }
        let presets = state.snooze_presets();
        let mut items: Vec<_> = presets
            .iter()
            .map(|preset| MenuItem {
                action: MenuAction::Thread(ThreadAction::SnoozePreset(preset.id.clone())),
                label: format!("{} ({})", preset.label, preset.when_label),
                icon: None,
                disabled: false,
                destructive: false,
                separator_before: false,
                checked: None,
                children: Vec::new(),
            })
            .collect();
        let mut custom = MenuItem::new(MenuAction::Thread(ThreadAction::SnoozeCustom), "Custom…", None);
        custom.separator_before = !presets.is_empty();
        items.push(custom);
        self.open = Some(OpenMenu { target_id: thread_id.to_owned(), items, position, highlighted: None, submenu: None, sub_highlighted: None, presets, selection: Vec::new(), snooze_request: Some(snooze_request), copy_scope: None, project_filter: None, owner: None, previous_focus: window.focused(cx), window: window.window_handle().into(), header: false });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn custom_snooze_keys(&self) -> Vec<&'static str> {
        let open = self.open.as_ref().unwrap();
        let main = Self::enabled(&open.items);
        let index = main.iter().position(|i| open.items[*i].action == MenuAction::Thread(ThreadAction::SnoozeCustom) || open.items[*i].children.iter().any(|child| matches!(child.action, MenuAction::Thread(ThreadAction::SnoozeCustom) | MenuAction::Bulk(BulkAction::SnoozeCustom)))).unwrap();
        let mut keys = vec!["down"; index + 1];
        if !open.items[main[index]].children.is_empty() { keys.extend(["right", "up"]); }
        keys.push("enter");
        keys
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn restore_focus(&self, previous: Option<FocusHandle>, window: gpui::AnyWindowHandle, cx: &mut Context<Self>) {
        let focus = self.focus.clone();
        let menu = cx.weak_entity();
        cx.defer(move |cx| {
            if menu.upgrade().is_none_or(|menu| menu.read(cx).open.is_some()) { return; }
            let _ = cx.update_window(window, |_, window, cx| {
                if focus.is_focused(window) { if let Some(previous) = previous { window.focus(&previous, cx); } }
            });
        });
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.open.take() {
            self.restore_focus(open.previous_focus, open.window, cx);
            cx.emit(DismissEvent);
            cx.notify();
        }
    }

    fn activate(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        let Some(open) = self.open.take() else { return };
        self.restore_focus(open.previous_focus.clone(), open.window, cx);
        cx.emit(DismissEvent);
        cx.notify();
        let enabled = open.items.iter().any(|item| !item.disabled && (item.action == action && item.children.is_empty() || item.children.iter().any(|child| !child.disabled && child.action == action)));
        if !enabled { return; }
        if let MenuAction::Thread(ref thread_action) = action {
            if let Some(owner) = &open.owner {
                if !owner.allows(self.state.read(cx), thread_action)
                    || (open.header && self.state.read(cx).selected_thread.as_deref() != Some(open.target_id.as_str())) { return; }
            } else if !matches!(thread_action, ThreadAction::SnoozeCustom | ThreadAction::SnoozePreset(_) | ThreadAction::Unsnooze) { return; }
        }
        match action {
            MenuAction::DiscardDraft => cx.emit(ContextMenuEvent::DiscardDraft(open.target_id)),
            MenuAction::Thread(ThreadAction::Rename) => cx.emit(ContextMenuEvent::Rename(open.owner.unwrap(), open.previous_focus)),
            MenuAction::Thread(ThreadAction::Delete) => cx.emit(ContextMenuEvent::Delete(open.owner.unwrap(), open.previous_focus)),
            MenuAction::Thread(ThreadAction::SnoozeCustom) | MenuAction::Bulk(BulkAction::SnoozeCustom) => {
                if let Some(request) = open.snooze_request.filter(|request| request.plan_for(self.state.read(cx), now_ms()).is_some()) {
                    cx.emit(ContextMenuEvent::CustomSnooze(request));
                }
            }
            MenuAction::Thread(ThreadAction::SnoozePreset(id)) | MenuAction::Bulk(BulkAction::SnoozePreset(id)) => {
                if let Some(request) = open.snooze_request {
                    self.state.update(cx, |state, cx| request.pick_preset(&id, &open.presets, state, cx));
                }
            }
            MenuAction::Thread(ThreadAction::Unsnooze) => {
                if let Some(request) = open.snooze_request {
                    self.state.update(cx, |state, cx| {
                        if let Some(ids) = request.plan_for_action(state, now_ms(), false) {
                            for id in ids {
                                state.unsnooze_thread(&id, cx);
                            }
                        }
                    });
                }
            }
            MenuAction::Thread(ThreadAction::FilterByProject) => {
                if let Some(filter) = open.project_filter {
                    self.state.update(cx, |state, cx| filter.apply(state, cx));
                }
            }
            MenuAction::Bulk(BulkAction::Delete) => cx.emit(ContextMenuEvent::BulkDelete(open.selection)),
            MenuAction::Thread(ThreadAction::ShareGuest) => {
                if let Some(owner) = open.owner { crate::guest_share::open(owner, open.window, cx); }
            }
            MenuAction::Thread(ThreadAction::ProjectSettings) => {
                let project_id = self.state.read(cx).sidebar.model.thread(&open.target_id).map(|thread| thread.project_id.clone());
                if let Some(project_id) = project_id {
                    crate::settings::open_project(&project_id, cx);
                }
            }
            MenuAction::Project(ProjectAction::Settings) => crate::settings::open_project(&open.target_id, cx),
            MenuAction::Bulk(action) => self.state.update(cx, |state, cx| state.run_bulk_action(&open.selection, action, &open.presets, cx)),
            MenuAction::Thread(ThreadAction::CopyThread) => self.state.update(cx, |state, cx| {
                if open.copy_scope.is_some() && open.copy_scope == state.thread_copy_scope() {
                    state.run_thread_action(&open.target_id, ThreadAction::CopyThread, &[], cx);
                }
            }),
            MenuAction::Thread(action) => self.state.update(cx, |state, cx| state.run_thread_action(&open.target_id, action, &open.presets, cx)),
            MenuAction::Project(action) => self.state.update(cx, |state, cx| match action {
                ProjectAction::NewThread => state.start_draft(&open.target_id, cx),
                ProjectAction::CopyPath => state.copy_project_path(&open.target_id, cx),
                ProjectAction::Pin => state.set_project_pinned(&open.target_id, true, cx),
                ProjectAction::Unpin => state.set_project_pinned(&open.target_id, false, cx),
                ProjectAction::Settings => {}
            }),
        }
    }

    fn enabled(items: &[MenuItem<MenuAction>]) -> Vec<usize> {
        items.iter().enumerate().filter(|(_, item)| !item.disabled).map(|(index, _)| index).collect()
    }

    fn step(current: Option<usize>, enabled: &[usize], down: bool) -> Option<usize> {
        if enabled.is_empty() { return None; }
        let position = current.and_then(|current| enabled.iter().position(|index| *index == current));
        let next = match (position, down) {
            (None, true) => 0,
            (None, false) => enabled.len().checked_sub(1)?,
            (Some(p), true) => (p + 1) % enabled.len(),
            (Some(p), false) => (p + enabled.len() - 1) % enabled.len(),
        };
        enabled.get(next).copied()
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut() else { return };
        let key = event.keystroke.key.as_str();
        let in_submenu = open.submenu.is_some();
        match key {
            "escape" if in_submenu => {
                cx.stop_propagation();
                open.submenu = None;
            }
            "escape" => {
                cx.stop_propagation();
                return self.close(cx);
            }
            "down" | "up" => {
                if let Some(parent) = open.submenu {
                    let enabled = Self::enabled(&open.items[parent].children);
                    open.sub_highlighted = Self::step(open.sub_highlighted, &enabled, key == "down");
                } else {
                    let enabled = Self::enabled(&open.items);
                    open.highlighted = Self::step(open.highlighted, &enabled, key == "down");
                }
            }
            "right" => {
                if let Some(index) = open.highlighted.filter(|index| !open.items[*index].children.is_empty()) {
                    open.submenu = Some(index);
                    open.sub_highlighted = Self::enabled(&open.items[index].children).first().copied();
                }
            }
            "left" => open.submenu = None,
            "enter" | "space" => {
                let action = match (open.submenu, open.sub_highlighted, open.highlighted) {
                    (Some(parent), Some(child), _) => Some(open.items[parent].children[child].action.clone()),
                    (None, _, Some(index)) if open.items[index].children.is_empty() => Some(open.items[index].action.clone()),
                    (None, _, Some(index)) => {
                        open.submenu = Some(index);
                        open.sub_highlighted = Self::enabled(&open.items[index].children).first().copied();
                        None
                    }
                    _ => None,
                };
                if let Some(action) = action {
                    return self.activate(action, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn render_item(&self, item: &MenuItem<MenuAction>, index: usize, highlighted: bool, submenu_parent: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let text = if item.destructive { p.destructive_foreground } else if highlighted { p.accent_foreground } else { p.popover_foreground };
        let icon_color = if item.destructive { text } else { p.muted_foreground };
        let has_children = !item.children.is_empty();
        let action = item.action.clone();
        let disabled = item.disabled;
        let id = SharedString::from(format!("menu-item-{}-{index}", submenu_parent.map_or(-1, |p| p as i64)));
        div()
            .id(id)
            .flex()
            .items_center()
            .min_h(px(ITEM_HEIGHT))
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .text_color(hsla(text))
            .when(highlighted, |this| this.bg(hsla(p.accent)))
            .when(disabled, |this| this.opacity(0.64))
            .when(item.checked.is_some(), |this| {
                this.child(div().w(px(16.)).flex_none().children(item.checked.filter(|c| *c).map(|_| Icon::Check.sized(px(14.)).text_color(hsla(text)))))
            })
            .children(item.icon.and_then(Icon::from_name).map(|icon| div().opacity(0.8).child(icon.sized(px(16.)).text_color(hsla(icon_color)))))
            .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
            .when(has_children, |this| this.child(Icon::ChevronRight.sized(px(14.)).text_color(hsla(p.muted_foreground))))
            .when(!disabled, |this| {
                this.on_mouse_move(cx.listener(move |this, _, _, cx| {
                    let Some(open) = this.open.as_mut() else { return };
                    match submenu_parent {
                        Some(_) if open.sub_highlighted != Some(index) => open.sub_highlighted = Some(index),
                        None if open.highlighted != Some(index) || (has_children && open.submenu != Some(index)) => {
                            open.highlighted = Some(index);
                            open.submenu = has_children.then_some(index);
                            open.sub_highlighted = None;
                        }
                        _ => return,
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !has_children {
                        this.activate(action.clone(), cx);
                    }
                }))
            })
            .into_any_element()
    }

    fn panel(cx: &App) -> gpui::Div {
        let p = palette(cx);
        div()
            .relative()
            .w(px(MENU_WIDTH))
            .p(px(5.))
            .rounded(px(8.))
            .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .shadow(vec![BoxShadow {
                color: gpui_hsla(0., 0., 0., 0.55),
                offset: point(px(0.), px(16.)),
                blur_radius: px(40.),
                spread_radius: px(-18.),
                inset: false,
            }])
    }

    fn render_list(&self, items: &[MenuItem<MenuAction>], highlighted: Option<usize>, parent: Option<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = palette(cx);
        let mut children = Vec::new();
        for (index, item) in items.iter().enumerate() {
            if item.separator_before && index > 0 {
                children.push(div().h(px(1.)).my(px(4.)).mx(px(-4.)).bg(hsla(p.border)).into_any_element());
            }
            children.push(self.render_item(item, index, highlighted == Some(index), parent, cx));
        }
        children
    }
}

/// The y offset of item `index` inside a panel, counting separators.
fn item_offset(items: &[MenuItem<MenuAction>], index: usize) -> f32 {
    let separators = items.iter().take(index + 1).enumerate().filter(|(i, item)| item.separator_before && *i > 0).count();
    4. + index as f32 * ITEM_HEIGHT + separators as f32 * 9.
}

impl Render for ContextMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = self.open.as_ref() else { return div().into_any_element() };
        let main = self.render_list(&open.items, open.highlighted, None, cx);
        let submenu = open.submenu.map(|parent| {
            let items = &open.items[parent].children;
            let list = self.render_list(items, open.sub_highlighted, Some(parent), cx);
            Self::panel(cx).id("thread-context-submenu").occlude().absolute().left(px(MENU_WIDTH - 4.)).top(px(item_offset(&open.items, parent) - 5.)).children(list)
        });
        let position = open.position;
        div()
            .id("thread-context-menu-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _, cx| this.close(cx)))
            .child(
                deferred(
                    anchored().position(position).snap_to_window_with_margin(px(8.)).child(
                        Self::panel(cx)
                            .id("thread-context-menu")
                            .relative()
                            .occlude()
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .font_weight(FontWeight::NORMAL)
                            .children(main)
                            .children(submenu),
                    ),
                )
                .with_priority(130),
            )
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
pub(crate) mod project_filter_tests {
    use super::*;
    use gpui::AppContext;
    use t3_client::supervisor::{ClientFixture, ConnectionStatus};
    use t3_core::auth::AuthSessionState;
    use t3_core::shell::{ShellSnapshot, ShellState};
    use t3_core::sidebar::grouping::{ProjectScope, build_project_groups};

    pub(crate) fn session(operate: bool) -> AuthSessionState {
        serde_json::from_value(serde_json::json!({
            "authenticated":true,
            "permissions": if operate { vec![t3_core::auth::ORCHESTRATION_OPERATE_SCOPE] } else { vec![] }
        })).unwrap()
    }

    fn install(cx: &mut gpui::TestAppContext) -> (Entity<AppState>, ClientFixture, tokio::runtime::Runtime) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (state, fixture) = cx.update(|cx| install_state(cx, runtime.handle().clone()));
        (state, fixture, runtime)
    }

    pub(crate) fn install_state(cx: &mut App, runtime: tokio::runtime::Handle) -> (Entity<AppState>, ClientFixture) {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let fixture: crate::chatgpt_setup::Fixture = serde_json::from_value(serde_json::json!({
                "provider":{"instanceId":"replay","driver":"replay","enabled":true,"installed":true,"status":"ready"},
                "auth":{"phase":"idle"},"install":{"phase":"idle"}
            })).unwrap();
            cx.set_global(fixture);
        let state = cx.new(|cx| AppState::new(runtime, cx));
        let (client, mut fixture) = ClientFixture::handle(session(true));
        state.update(cx, |state, cx| {
            state.environments = serde_json::from_value(serde_json::json!({"active":"saved-fixture", "environments":[{
                "id":"saved-fixture","label":"Fixture","httpBase":"http://fixture.invalid","environmentId":"fixture-env"
            }]})).unwrap();
            let projects = [("checkout-a", "/fixture/a", "repo"), ("checkout-b", "/fixture/b", "repo"), ("other", "/fixture/other", "other")].into_iter().map(|(id, path, repo)| {
                serde_json::from_value(serde_json::json!({
                    "id":id,"title": if repo == "repo" { "Shared repo" } else { "Other" },"workspaceRoot":path,
                    "createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z",
                    "projectIcon":{"kind":"monogram","text":"P","color":"blue"},
                    "repositoryIdentity":{"canonicalKey":repo}
                })).unwrap()
            }).collect();
            let threads = [("menu-thread", "checkout-b"), ("sibling", "checkout-a"), ("other-thread", "other")].into_iter().map(|(id, project)| {
                serde_json::from_value(serde_json::json!({
                    "id":id,"projectId":project,"title":id,"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z",
                    "providerInstanceId":"replay","modelSelection":{"instanceId":"replay","model":"replay"},
                    "runtimeMode":"auto","interactionMode":"default","lineage":{"rootThreadId":id},"status":"idle",
                    "hasActionableProposedPlan":false,"itemCount":0,"visibleItemCount":0
                })).unwrap()
            }).collect();
            let mut shell = ShellState::default();
            shell.snapshot = Some(ShellSnapshot {projects, threads, ..Default::default()});
            shell.synchronized = true;
            state.install_active_environment(client, shell, state.server_config.clone(), ConnectionStatus::Connected, cx);
        });
        fixture.take_commands();
        (state, fixture)
    }

    fn select_groups(state: &Entity<AppState>, keys: &[&str], cx: &mut gpui::TestAppContext) {
        state.update(cx, |state, cx| {
            let groups = build_project_groups(
                "fixture-env",
                &state.shell.snapshot.as_ref().unwrap().projects,
                &state.sidebar.settings.grouping,
            );
            let mut scope = ProjectScope::default();
            for group in groups
                .iter()
                .filter(|group| keys.contains(&group.key.as_str()))
            {
                scope.toggle(group, &groups, true);
            }
            state.set_project_scope(scope, cx);
        });
    }

    #[gpui::test]
    fn sidebar_menu_activation_replaces_and_clears_logical_scope(cx: &mut gpui::TestAppContext) {
        let (state, mut fixture, _runtime) = install(cx);
        let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
        let draft = state.read_with(cx, |state, _| state.draft_project.clone());
        for clear in [false, true] {
            if clear {
                select_groups(&state, &["repo", "other"], cx);
            }
            state.update(cx, |state, _| {
                state.sidebar.selection.toggle("other-thread")
            });
            let shown = cx.update(|window, cx| {
                menu.update(cx, |menu, cx| {
                    menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
                    menu.open
                        .as_ref()
                        .unwrap()
                        .items
                        .iter()
                        .find(|item| {
                            item.action == MenuAction::Thread(ThreadAction::FilterByProject)
                        })
                        .cloned()
                })
            });
            if !clear {
                select_groups(&state, &["other"], cx);
            }
            cx.update(|_, cx| {
                menu.update(cx, |menu, cx| {
                    menu.activate(MenuAction::Thread(ThreadAction::FilterByProject), cx);
                    assert!(!menu.is_open());
                })
            });
            state.read_with(cx, |state, _| {
                let expected = (!clear).then(|| {
                    ["checkout-a".to_owned(), "checkout-b".to_owned()]
                        .into_iter()
                        .collect()
                });
                assert_eq!(
                    state.sidebar.project_scope, expected,
                    "real menu activation must replace/clear the sidebar scope"
                );
                assert_eq!(state.sidebar.project_filter_scope.is_empty(), clear);
                assert!(state.sidebar.model.thread("sibling").is_some());
                assert_eq!(state.sidebar.model.thread("other-thread").is_some(), clear);
                assert!(state.sidebar.selection.selected().is_empty());
                assert_eq!(state.selected_thread, None);
                assert_eq!(state.draft_project, draft);
            });
            let shown = shown.expect("sidebar menu exposes its logical project filter");
            assert_eq!(
                shown.label,
                if clear {
                    "Show all projects"
                } else {
                    "Filter by Shared repo"
                }
            );
            assert_eq!(shown.icon, Some("folder-tree"));
            assert!(!shown.disabled);
            assert!(
                fixture.take_commands().is_empty(),
                "scope changes must enqueue no RPCs or visits"
            );
            eprintln!(
                "production menu activation: clear={clear}; logical members=checkout-a,checkout-b; selection cleared; route retained; zero client commands"
            );
        }
    }

    #[gpui::test]
    fn sidebar_menu_rechecks_current_grants_and_captured_target(cx: &mut gpui::TestAppContext) {
        for mode in [
            "revoked",
            "unknown-grants",
            "unauthenticated",
            "capability",
            "config-loading",
            "config-missing",
            "disconnected",
            "unsynchronized",
            "reconnected",
            "shell-replaced",
            "client-replaced",
            "environment-replaced",
            "saved-replaced",
            "saved-mismatch",
            "thread-missing",
            "thread-archived",
            "thread-replaced",
            "thread-moved",
            "project-missing",
            "group-archived",
            "member-missing",
            "member-replaced",
            "member-archived",
            "regrouped",
        ] {
            let (state, mut fixture, _runtime) = install(cx);
            let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
            cx.update(|window, cx| {
                menu.update(cx, |menu, cx| {
                    menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
                    assert!(
                        menu.open
                            .as_ref()
                            .unwrap()
                            .items
                            .iter()
                            .any(|item| item.action
                                == MenuAction::Thread(ThreadAction::FilterByProject)
                                && !item.disabled)
                    );
                })
            });
            state.update(cx, |state, _| {
                state.sidebar.selection.toggle("other-thread")
            });
            let mut replacement = None;
            state.update(cx, |state, cx| match mode {
                "revoked" => {
                    fixture.grants.send(Some(session(false))).unwrap();
                }
                "unknown-grants" => {
                    fixture.grants.send(None).unwrap();
                }
                "unauthenticated" => {
                    let mut denied = session(true);
                    denied.authenticated = false;
                    fixture.grants.send(Some(denied)).unwrap();
                }
                "capability" => state.sidebar.capabilities.can_operate = false,
                "config-loading" => state.sidebar.capabilities.config_loaded = false,
                "config-missing" => state.server_config.config = None,
                "disconnected" => {
                    state.phase = crate::app_state::Phase::Online(ConnectionStatus::Connecting)
                }
                "unsynchronized" => state.shell.synchronized = false,
                "reconnected" => {
                    fixture.generation.send(2).unwrap();
                }
                "shell-replaced" => state.shell.generation += 1,
                "client-replaced" => {
                    let (client, observed) = ClientFixture::handle(session(true));
                    state.install_active_environment(
                        client,
                        state.shell.clone(),
                        state.server_config.clone(),
                        ConnectionStatus::Connected,
                        cx,
                    );
                    replacement = Some(observed);
                }
                "environment-replaced" => {
                    state
                        .server_config
                        .config
                        .as_mut()
                        .unwrap()
                        .environment
                        .environment_id = "other-env".into()
                }
                "saved-replaced" => state.environments.active = Some("other-saved".into()),
                "saved-mismatch" => {
                    state.environments.environments[0].environment_id = Some("other-env".into())
                }
                "thread-missing" => state
                    .shell
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .threads
                    .retain(|thread| thread.id != "menu-thread"),
                "thread-archived" => {
                    state.shell.snapshot.as_mut().unwrap().threads[0].archived_at =
                        Some("2026-11-05T00:00:00Z".into())
                }
                "thread-replaced" => {
                    state.shell.snapshot.as_mut().unwrap().threads[0].created_at =
                        "2026-11-05T00:00:00Z".into()
                }
                "thread-moved" => {
                    state.shell.snapshot.as_mut().unwrap().threads[0].project_id = "other".into();
                }
                "member-archived" => {
                    state.shell.snapshot.as_mut().unwrap().projects[0].archived_at =
                        Some("2026-11-05T00:00:00Z".into());
                }
                "project-missing" => state
                    .shell
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .projects
                    .retain(|project| project.id != "checkout-b"),
                "group-archived" => {
                    for project in &mut state.shell.snapshot.as_mut().unwrap().projects {
                        project.archived_at = Some("2026-11-05T00:00:00Z".into());
                    }
                }
                "member-missing" => state
                    .shell
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .projects
                    .retain(|project| project.id != "checkout-a"),
                "member-replaced" => {
                    state.shell.snapshot.as_mut().unwrap().projects[0].workspace_root =
                        "/replaced/a".into()
                }
                "regrouped" => {}
                _ => unreachable!(),
            });
            if mode == "regrouped" {
                cx.update(|_, cx| {
                    crate::settings::store::install_committed_client(
                        &state,
                        serde_json::from_value(
                            serde_json::json!({"sidebarProjectGroupingMode":"separate"}),
                        )
                        .unwrap(),
                        cx,
                    )
                });
            }
            if let Some(replacement) = &mut replacement {
                replacement.take_commands();
            }
            cx.update(|_, cx| {
                menu.update(cx, |menu, cx| {
                    menu.activate(MenuAction::Thread(ThreadAction::FilterByProject), cx)
                })
            });
            state.read_with(cx, |state, _| {
                assert!(state.sidebar.project_filter_scope.is_empty(), "{mode}");
                assert!(state.sidebar.project_scope.is_none(), "{mode}");
                assert_eq!(
                    state.sidebar.selection.selected(),
                    ["other-thread"],
                    "{mode}"
                );
                assert!(state.selected_thread.is_none(), "{mode}");
            });
            assert!(!menu.read_with(cx, |menu, _| menu.is_open()), "{mode}");
            assert!(
                fixture.take_commands().is_empty(),
                "{mode}: original client commands"
            );
            assert!(
                replacement
                    .as_mut()
                    .is_none_or(|fixture| fixture.take_commands().is_empty()),
                "{mode}: replacement client commands"
            );
            eprintln!(
                "production activation rejected: {mode}; unchanged scope/selection; zero commands"
            );
        }
    }

    #[gpui::test]
    fn sidebar_filter_omission_disabled_grants_header_and_keyboard(cx: &mut gpui::TestAppContext) {
        for mode in ["header", "archived", "missing", "denied"] {
            let (state, mut fixture, _runtime) = install(cx);
            let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
            state.update(cx, |state, _| {
                if mode == "archived" {
                    for project in &mut state.shell.snapshot.as_mut().unwrap().projects {
                        project.archived_at = Some("2026-11-05T00:00:00Z".into());
                    }
                } else if mode == "missing" {
                    state.shell.snapshot.as_mut().unwrap().projects.clear();
                }
            });
            if mode == "denied" {
                fixture.grants.send(Some(session(false))).unwrap();
            }
            cx.update(|window, cx| {
                menu.update(cx, |menu, cx| {
                    if mode == "header" {
                        menu.open_for_header("menu-thread", point(px(10.), px(10.)), window, cx);
                    } else {
                        menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
                    }
                    let items = &menu.open.as_ref().unwrap().items;
                    assert!(
                        !items
                            .iter()
                            .any(|item| item.action == MenuAction::Thread(ThreadAction::Find))
                    );
                    let filter = items.iter().find(|item| {
                        item.action == MenuAction::Thread(ThreadAction::FilterByProject)
                    });
                    if mode == "denied" {
                        assert!(filter.unwrap().disabled);
                    } else {
                        assert!(filter.is_none(), "{mode}");
                    }
                    menu.activate(MenuAction::Thread(ThreadAction::FilterByProject), cx);
                })
            });
            assert!(
                state.read_with(cx, |state, _| state.sidebar.project_filter_scope.is_empty()),
                "{mode}"
            );
            assert!(fixture.take_commands().is_empty());
        }
        let (state, mut fixture, _runtime) = install(cx);
        let (menu, cx) = cx.add_window_view(|_, cx| ContextMenu::new(state.clone(), cx));
        fixture.grants.send(Some(session(false))).unwrap();
        fixture.grants.send(Some(session(true))).unwrap();
        cx.update(|window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
                let items = &menu.open.as_ref().unwrap().items;
                let index = items
                    .iter()
                    .position(|item| {
                        item.action == MenuAction::Thread(ThreadAction::FilterByProject)
                    })
                    .unwrap();
                assert_eq!(
                    items[index - 1].action,
                    MenuAction::Thread(ThreadAction::MarkUnread)
                );
                for _ in 0..=index {
                    menu.on_key(
                        &KeyDownEvent {
                            keystroke: gpui::Keystroke::parse("down").unwrap(),
                            is_held: false,
                            prefer_character_input: false,
                        },
                        cx,
                    );
                }
                menu.on_key(
                    &KeyDownEvent {
                        keystroke: gpui::Keystroke::parse("enter").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    },
                    cx,
                );
                assert!(!menu.is_open());
            })
        });
        assert_eq!(
            state.read_with(cx, |state, _| state
                .sidebar
                .project_scope
                .as_ref()
                .unwrap()
                .len()),
            2
        );
        cx.update(|window, cx| {
            menu.update(cx, |menu, cx| {
                menu.open_for_thread("menu-thread", point(px(10.), px(10.)), window, cx);
                menu.on_key(
                    &KeyDownEvent {
                        keystroke: gpui::Keystroke::parse("escape").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    },
                    cx,
                );
                assert!(!menu.is_open());
            })
        });
        assert!(fixture.take_commands().is_empty());
        eprintln!(
            "production menu: missing/archived/header omitted; live grant denial disabled; regrant/keyboard Enter scopes two checkouts; Escape dismisses; no Find; zero commands"
        );
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod thread_menu_tests;
