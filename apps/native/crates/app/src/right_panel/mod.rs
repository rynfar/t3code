//! The inline right panel beside the thread: per-thread width with an 8px resize handle, a tab
//! bar, and the active surface. Ports `components/RightPanelTabs.tsx`,
//! `components/preview/PreviewPanelShell.tsx`, `components/preview/RightPanelResizeHandle.tsx`
//! and `hooks/usePreviewPanelInlineSize.ts`. New surfaces add a `SurfaceKind` and a content arm.

mod controls;
mod state;
mod tabs;

use std::time::{Duration, Instant};

const REOPEN_GUARD: Duration = Duration::from_millis(250);

use gpui::{
    AnchoredPositionMode, Animation, AnimationExt, App, Context, DragMoveEvent, ElementId, EmptyView, Entity, FocusHandle, Focusable,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, StyleRefinement, Styled, Subscription, Task, Window, actions, div,
    ease_out_quint, point, prelude::*, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::v_flex;
use t3_core::diff::turn_diff_summaries;
use t3_core::pull_requests::PrRef;
use t3_core::pull_requests::thread_badge::{resolve_thread_current_pull_request, visible_thread_pull_requests};
use t3_core::right_panel::{
    DEFAULT_WIDTH, ProactiveEvaluationInput, ProactivePanelCoordinator, PullRequestDetailTab, PullRequestTarget, Surface, SurfaceKind,
    ThreadPanel, clamp_width, is_sheet_mode, sheet_width,
};

pub use controls::{Button, Size, Variant, focusable, segment, segmented, tooltip_view};
pub use state::PanelState;
#[cfg(feature = "snapshot")]
pub use tabs::open_surface;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::diff::DiffPanel;
use crate::files::FilesPanel;
use crate::theme::{hsla, palette};
use crate::workspace::sidebar_inline_width;

actions!(right_panel, [ToggleRightPanel, ToggleDiff, CloseActiveSurface, ReopenClosedView, ToggleMaximized, NewSurface]);

/// The actions behind `rightPanel.toggle`, `rightPanel.toggleMaximized`, `rightPanel.close`,
/// `view.reopenClosed` and `diff.toggle`; `keymap` binds their keys and `when` clauses.
pub fn init(state: Entity<AppState>, cx: &mut App) {
    let bind = |state: &Entity<AppState>, run: fn(&mut AppState, &mut Context<AppState>)| {
        let state = state.clone();
        move |cx: &mut App| state.update(cx, run)
    };
    let toggle = bind(&state, AppState::toggle_right_panel);
    cx.on_action(move |_: &ToggleRightPanel, cx| toggle(cx));
    let diff = bind(&state, AppState::toggle_diff);
    cx.on_action(move |_: &ToggleDiff, cx| diff(cx));
    let close = bind(&state, AppState::close_active_surface);
    cx.on_action(move |_: &CloseActiveSurface, cx| close(cx));
    let reopen = bind(&state, AppState::reopen_closed_view);
    cx.on_action(move |_: &ReopenClosedView, cx| reopen(cx));
    let maximize = bind(&state, AppState::toggle_right_panel_maximized);
    cx.on_action(move |_: &ToggleMaximized, cx| maximize(cx));
}

struct ResizeDrag;

#[derive(Debug, Clone, Copy, PartialEq)]
struct PanelAnimation {
    seq: u64,
    duration_ms: u32,
    opening: bool,
    from_width: f32,
}

pub struct RightPanelView {
    state: Entity<AppState>,
    diff: Entity<DiffPanel>,
    pull_requests: Entity<crate::pull_requests::ThreadPullRequestsPanel>,
    files: Entity<FilesPanel>,
    /// One pane per open side chat tab of the showing thread.
    side_chats: Vec<Entity<crate::side_chat::SideChatPane>>,
    /// One pane per open browser tab of the showing thread; only the active one streams.
    previews: Vec<Entity<crate::preview::PreviewPane>>,
    /// One pane per open device tab of the showing thread; only the active one streams.
    devices: Vec<Entity<crate::device::DevicePane>>,
    /// One pane per open right-panel terminal tab `(thread_id, surface_id, view)`.
    terminals: Vec<(String, String, Entity<crate::terminal::PanelTerminalView>)>,
    /// One embedded detail view per open single-PR tab `(thread_id, surface_id, view)`.
    pull_request_tabs: Vec<(String, String, Entity<crate::pull_requests::DetailView>)>,
    /// Stateful proactive panel coordinator (`useProactivePanels.ts`).
    proactive: ProactivePanelCoordinator,
    /// Last seen `proactive_panels_enabled` setting, so toggling it in Settings re-evaluates.
    last_proactive_enabled: Option<bool>,
    /// Tracks `(thread_id, is_open)` across frames so `panelAnimationDurationMs` animates
    /// same-thread open/close transitions without animating thread navigation (`panelAnimations.ts`).
    last_open_state: Option<(String, bool)>,
    /// Last rendered inline/sheet panel width, used as the start width for close transitions.
    last_rendered_width: f32,
    /// Active open or close width transition; `None` when settled or animations are off (`0 ms`).
    panel_animation: Option<PanelAnimation>,
    animation_seq: u64,
    _close_animation_task: Option<Task<()>>,
    /// Whether the panel is currently in narrow-window sheet mode (`ChatView.tsx:2035`).
    in_sheet_mode: bool,
    /// Inline rename input for a device tab (`RightPanelTabs.tsx`).
    renaming_device: Option<(String, Entity<InputState>, Subscription)>,
    add_menu_open: bool,
    add_menu_dismissed: Option<Instant>,
    add_menu_highlighted: usize,
    add_menu_focus: FocusHandle,
    sheet_focus: FocusHandle,
    _save_width: Option<Task<()>>,
    _details_subscription: Option<Subscription>,
    _git_subscription: Option<Subscription>,
    _subscription: Subscription,
}

#[cfg(feature = "snapshot")]
struct SnapshotDiffPanel(Entity<DiffPanel>);
#[cfg(feature = "snapshot")]
impl gpui::Global for SnapshotDiffPanel {}
#[cfg(feature = "snapshot")]
pub(crate) fn snapshot_diff(cx: &gpui::App) -> Entity<DiffPanel> {
    cx.global::<SnapshotDiffPanel>().0.clone()
}

impl RightPanelView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffPanel::new(state.clone(), window, cx));
        #[cfg(feature = "snapshot")]
        cx.set_global(SnapshotDiffPanel(diff.clone()));
        let pull_requests = cx.new(|cx| crate::pull_requests::ThreadPullRequestsPanel::new(state.clone(), cx));
        let files = cx.new(|cx| FilesPanel::new(state.clone(), window, cx));
        let subscription = cx.subscribe(&state, |this, state, event: &AppEvent, cx| match event {
            AppEvent::RightPanel => {
                this.prune_side_chats(cx);
                let open = state
                    .read(cx)
                    .panel_thread()
                    .as_deref()
                    .is_some_and(|thread| state.read(cx).panels.panels.is_open(thread));
                if !open {
                    this.add_menu_open = false;
                    this.renaming_device = None;
                }
                let proactive_enabled = crate::settings::store::try_client(cx)
                    .is_some_and(|doc| doc.proactive_panels_enabled());
                if this.last_proactive_enabled != Some(proactive_enabled) {
                    this.evaluate_proactive(cx);
                }
                cx.notify();
            }
            AppEvent::Selection => {
                this.prune_side_chats(cx);
                this.add_menu_open = false;
                this.renaming_device = None;
                this.panel_animation = None;
                this._close_animation_task = None;
                // A pending width save belongs to the thread that was showing: flush it now.
                let flush = this._save_width.take().is_some();
                state.update(cx, |state, cx| {
                    if flush {
                        state.save_panels(cx);
                    }
                    state.right_panel_thread_shown(cx);
                });
                this.evaluate_proactive(cx);
                cx.notify();
            }
            AppEvent::Shell | AppEvent::Thread | AppEvent::ServerConfig | AppEvent::Phase => {
                this.evaluate_proactive(cx);
            }
            _ => {}
        });
        let initial_sheet_mode = {
            let viewport = f32::from(window.viewport_size().width);
            let app = state.read(cx);
            let maximized = app.panel_thread().is_some_and(|id| app.panels.panels.panel(&id).maximized);
            is_sheet_mode(viewport) && !maximized
        };
        let mut view = RightPanelView {
            state,
            diff,
            pull_requests,
            files,
            side_chats: Vec::new(),
            previews: Vec::new(),
            devices: Vec::new(),
            terminals: Vec::new(),
            pull_request_tabs: Vec::new(),
            proactive: ProactivePanelCoordinator::default(),
            last_proactive_enabled: None,
            last_open_state: None,
            last_rendered_width: DEFAULT_WIDTH,
            panel_animation: None,
            animation_seq: 0,
            _close_animation_task: None,
            in_sheet_mode: initial_sheet_mode,
            renaming_device: None,
            add_menu_open: false,
            add_menu_dismissed: None,
            add_menu_highlighted: 0,
            add_menu_focus: cx.focus_handle(),
            sheet_focus: cx.focus_handle(),
            _save_width: None,
            _details_subscription: None,
            _git_subscription: None,
            _subscription: subscription,
        };
        view.attach_details_subscription(cx);
        view.evaluate_proactive(cx);
        view
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn open_animation_duration_ms(&self) -> Option<u32> {
        self.panel_animation.filter(|anim| anim.opening).map(|anim| anim.duration_ms)
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn close_animation_duration_ms(&self) -> Option<u32> {
        self.panel_animation.filter(|anim| !anim.opening).map(|anim| anim.duration_ms)
    }

    fn attach_details_subscription(&mut self, cx: &mut Context<Self>) {
        if let Some(details) = crate::thread_details::details(cx) {
            if self._details_subscription.is_none() {
                self._details_subscription = Some(cx.subscribe(&details, |this, _, event: &crate::thread_details::DetailsEvent, cx| {
                    if *event == crate::thread_details::DetailsEvent::Changed {
                        this.evaluate_proactive(cx);
                    }
                }));
            }
            if self._git_subscription.is_none() {
                let git = details.read(cx).git.clone();
                self._git_subscription = Some(cx.subscribe(&git, |this, _, event: &crate::git::GitEvent, cx| {
                    if *event == crate::git::GitEvent::Changed {
                        this.evaluate_proactive(cx);
                    }
                }));
            }
        }
    }

    /// Ports `apps/web/src/hooks/useProactivePanels.ts` + `ChatView.tsx:1286-1287,1997-2051` +
    /// `useThreadLinkedPullRequest.ts:99-104` by reading `AppState`, `ClientSettings`, and
    /// `Details.git` directly inside `RightPanelView`.
    pub fn evaluate_proactive(&mut self, cx: &mut Context<Self>) {
        self.attach_details_subscription(cx);
        let settings_doc = crate::settings::store::try_client(cx);
        let settings_hydrated = settings_doc.is_some();
        let setting_enabled = settings_doc.is_some_and(|doc| doc.proactive_panels_enabled());
        self.last_proactive_enabled = Some(setting_enabled);
        let enabled = setting_enabled && !self.in_sheet_mode;

        let (is_git_repo, branch_pr) = {
            let selected = self.state.read(cx).selected_thread.clone();
            crate::thread_details::details(cx)
                .map(|details| {
                    let git = details.read(cx).git.read(cx);
                    let matches_thread = git.target.as_ref().map(|target| target.thread_id.as_str()) == selected.as_deref();
                    if matches_thread {
                        (
                            git.status.as_ref().map(|status| status.local.is_repo),
                            git.status.as_ref().and_then(|status| status.remote.pr.clone()),
                        )
                    } else {
                        (None, None)
                    }
                })
                .unwrap_or((None, None))
        };

        let (
            thread_id,
            thread_detail_loading,
            running_turn_id,
            latest_run_id,
            latest_run_completed,
            latest_run_settled,
            turn_diff_summaries_vec,
            linked_pull_request,
            has_linked_pull_request_detail,
            pull_requests_key,
            pull_requests_capability_known,
            supports_pull_requests,
            pull_requests_surface_available,
            visible_pull_request_count,
        ) = {
            let state = self.state.read(cx);
            let thread_id = state.selected_thread.clone();
            let projection = state
                .thread
                .projection
                .as_ref()
                .filter(|projection| thread_id.as_deref() == Some(projection.thread.id.as_str()));
            let thread_detail_loading = thread_id.is_some() && projection.is_none();
            let running_turn_id = projection
                .and_then(|p| p.runs.iter().rev().find(|run| matches!(run.status.as_str(), "running" | "starting")))
                .map(|run| run.id.clone());
            let latest_run = projection.and_then(|p| p.runs.last());
            let latest_run_id = latest_run.map(|run| run.id.clone());
            let latest_run_completed = latest_run.is_some_and(|run| run.status == "completed");
            let latest_run_settled = latest_run.is_some_and(|run| run.completed_at.is_some());
            let summaries = projection.map(|p| turn_diff_summaries(&p.checkpoints)).unwrap_or_default();

            let env_id = state.environment().map(str::to_owned);
            let thread_shell = thread_id.as_deref().and_then(|id| state.thread_shell(id));
            let visible_links = thread_shell
                .map(|thread| visible_thread_pull_requests(&thread.pull_requests))
                .unwrap_or_default();
            let visible_count = visible_links.len();
            let current_pr = thread_shell
                .and_then(|thread| resolve_thread_current_pull_request(&thread.pull_requests))
                .map(|current| current.link());
            let project_id = thread_shell.map(|thread| thread.project_id.clone());
            let link_project = project_id
                .as_deref()
                .and_then(|id| state.shell.project(id))
                .and_then(|project| project.repository_identity.as_ref())
                .and_then(t3_core::pull_requests::link::LinkProject::from_identity);
            let persisted_pr = current_pr.zip(project_id.clone()).map(|(link, project_id)| PullRequestTarget {
                environment_id: env_id.clone(),
                project_id,
                host: (!link.host.is_empty()).then(|| link.host.clone()),
                repository: link.repository.clone(),
                number: link.number,
                url: Some(link.url.clone()),
                reveal_url: None,
                detail_tab: None,
            });
            // Match `useThreadLinkedPullRequest.ts:99-104`: branch PR from live git status
            // overrides/supplements when resolvable against the project's repository, while
            // `hasDetail` reflects whether a persisted link exists.
            let branch_pr_target = branch_pr.and_then(|pr| {
                let project_id = project_id.clone()?;
                let parsed = t3_core::pull_requests::link::parse_change_request_url(&pr.url);
                let (host, repository) = match (parsed, &link_project) {
                    (Some(link), _) => (Some(link.host), link.repository),
                    (None, Some(lp)) => (Some(lp.host.clone()), lp.repository.clone()),
                    (None, None) => return None,
                };
                Some(PullRequestTarget {
                    environment_id: env_id.clone(),
                    project_id,
                    host,
                    repository,
                    number: pr.number,
                    url: Some(pr.url),
                    reveal_url: None,
                    detail_tab: None,
                })
            });
            let linked_pr = branch_pr_target.or(persisted_pr);
            let cap_known = state.server_config.config.is_some();
            // Match `ChatView.tsx:1286-1287,1997-2003,2027-2031`:
            let supports_pull_requests = cap_known;
            let pull_requests_surface_available =
                thread_id.is_some() && supports_pull_requests && visible_count > 0;
            let linked_pr_key = linked_pr
                .as_ref()
                .map(|pr| format!("{}:{}#{}", pr.project_id, pr.repository.to_lowercase(), pr.number));
            let pr_key = if pull_requests_surface_available {
                let mut keys: Vec<String> = visible_links
                    .iter()
                    .map(|link| format!("{}/{}#{}", link.host.to_lowercase(), link.repository.to_lowercase(), link.number))
                    .collect();
                keys.sort();
                keys.dedup();
                Some(keys.join("|"))
            } else {
                linked_pr_key
            };
            (
                thread_id,
                thread_detail_loading,
                running_turn_id,
                latest_run_id,
                latest_run_completed,
                latest_run_settled,
                summaries,
                linked_pr,
                current_pr.is_some(),
                pr_key,
                cap_known,
                supports_pull_requests,
                pull_requests_surface_available,
                visible_count,
            )
        };

        let mut proactive = std::mem::take(&mut self.proactive);
        let input = ProactiveEvaluationInput {
            thread_id: thread_id.as_deref(),
            enabled,
            settings_hydrated,
            thread_detail_loading,
            running_turn_id: running_turn_id.as_deref(),
            latest_run_id: latest_run_id.as_deref(),
            latest_run_completed,
            latest_run_settled,
            turn_diff_summaries: &turn_diff_summaries_vec,
            is_git_repo,
            linked_pull_request,
            has_linked_pull_request_detail,
            pull_requests_key,
            pull_requests_capability_known,
            supports_pull_requests,
            pull_requests_surface_available,
            visible_pull_request_count,
        };
        let changed = self.state.update(cx, |state, cx| state.apply_proactive_panels(&mut proactive, input, cx));
        self.proactive = proactive;
        if changed {
            cx.notify();
        }
    }

    fn start_device_rename(&mut self, surface_id: String, current_title: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&current_title, window, cx);
            state
        });
        let sid = surface_id.clone();
        let sub = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                if this.renaming_device.as_ref().is_none_or(|(active_id, _, _)| active_id != &sid) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.renaming_device = None;
                this.state.update(cx, |state, cx| state.rename_device_surface(&sid, &value, cx));
                cx.notify();
            }
        });
        window.focus(&input.focus_handle(cx), cx);
        self.renaming_device = Some((surface_id, input, sub));
        cx.notify();
    }

    fn cancel_device_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming_device.take().is_some() {
            cx.notify();
        }
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub fn is_add_menu_open(&self) -> bool {
        self.add_menu_open
    }

    /// `rightPanel.new` (`RightPanelTabs.tsx`): opens and focuses the add-surface menu while the
    /// right panel is open.
    pub fn open_add_surface_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let is_open = state
            .panel_thread()
            .as_deref()
            .is_some_and(|thread| state.panels.panels.is_open(thread));
        if !is_open {
            self.add_menu_open = false;
            return;
        }
        let entries = tabs::add_menu_entries(state, cx);
        self.add_menu_highlighted = entries.iter().position(|entry| entry.unavailable.is_none()).unwrap_or(0);
        self.add_menu_dismissed = None;
        self.add_menu_open = true;
        window.focus(&self.add_menu_focus, cx);
        cx.notify();
    }

    pub fn toggle_add_surface_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_menu_open {
            self.close_add_surface_menu(cx);
        } else if !self.add_menu_dismissed.take().is_some_and(|at| at.elapsed() < REOPEN_GUARD) {
            self.open_add_surface_menu(window, cx);
        }
    }

    pub fn dismiss_add_surface_menu_outside(&mut self, cx: &mut Context<Self>) {
        if self.add_menu_open {
            self.add_menu_open = false;
            self.add_menu_dismissed = Some(Instant::now());
            cx.notify();
        }
    }

    pub fn close_add_surface_menu(&mut self, cx: &mut Context<Self>) {
        if self.add_menu_open {
            self.add_menu_open = false;
            cx.notify();
        }
    }

    fn handle_add_menu_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.add_menu_open {
            return;
        }
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            cx.stop_propagation();
            self.close_add_surface_menu(cx);
            return;
        }
        let entries = tabs::add_menu_entries(self.state.read(cx), cx);
        if entries.is_empty() {
            return;
        }
        match key {
            "up" | "down" => {
                cx.stop_propagation();
                let step: isize = if key == "down" { 1 } else { -1 };
                let count = entries.len() as isize;
                let mut index = self.add_menu_highlighted as isize;
                for _ in 0..entries.len() {
                    index = (index + step).rem_euclid(count);
                    if entries[index as usize].unavailable.is_none() {
                        self.add_menu_highlighted = index as usize;
                        cx.notify();
                        break;
                    }
                }
            }
            "enter" | "space" => {
                cx.stop_propagation();
                if let Some(entry) = entries.get(self.add_menu_highlighted)
                    && entry.unavailable.is_none()
                {
                    let open = entry.open;
                    self.close_add_surface_menu(cx);
                    self.state.update(cx, open);
                }
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(entry) = entries
                    .iter()
                    .find(|entry| entry.unavailable.is_none() && entry.shortcut.eq_ignore_ascii_case(key))
                {
                    cx.stop_propagation();
                    let open = entry.open;
                    self.close_add_surface_menu(cx);
                    self.state.update(cx, open);
                }
            }
            _ => {}
        }
    }

    fn resize(&mut self, event: &DragMoveEvent<ResizeDrag>, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = f32::from(window.viewport_size().width);
        let row = viewport - sidebar_inline_width(self.state.read(cx), viewport);
        let width = clamp_width(f32::from(event.bounds.right() - event.event.position.x), viewport, row);
        self.state.update(cx, |state, cx| state.set_right_panel_width(width, false, cx));
        // Persist once the drag settles, not on every frame of it.
        self._save_width = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let _ = this.update(cx, |this, cx| this.state.update(cx, |state, cx| state.set_right_panel_width(width, true, cx)));
        }));
    }

    /// Drops the panes whose tab closed, or that belong to a thread no longer showing, with their
    /// transcript caches and subscriptions.
    fn prune_side_chats(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let open: Vec<String> = state
            .panel_thread()
            .as_deref()
            .map(|thread| state.panels.panels.panel(thread).surfaces.iter().filter_map(|surface| surface.side_chat_thread().map(str::to_owned)).collect())
            .unwrap_or_default();
        self.side_chats.retain(|pane| open.iter().any(|id| id == pane.read(cx).thread_id()));
    }

    /// The side chat's pane, kept while its tab stays open so its scroll and draft survive tab switches.
    fn side_chat_pane(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<crate::side_chat::SideChatPane> {
        if let Some(pane) = self.side_chats.iter().find(|pane| pane.read(cx).thread_id() == thread_id) {
            return pane.clone();
        }
        let pane = cx.new(|cx| crate::side_chat::SideChatPane::new(self.state.clone(), thread_id.to_owned(), window, cx));
        self.side_chats.push(pane.clone());
        pane
    }

    /// Keeps a pane per browser tab of the showing thread, and lets only the one in front stream.
    fn sync_previews(&mut self, thread_id: Option<&str>, panel: Option<&ThreadPanel>, window: &mut Window, cx: &mut Context<Self>) {
        let front = panel.filter(|panel| panel.is_open).and_then(|panel| panel.active_surface_id.as_deref());
        self.previews.retain(|pane| {
            let pane = pane.read(cx);
            thread_id == Some(pane.thread_id()) && panel.is_some_and(|panel| panel.surfaces.iter().any(|surface| surface.id == pane.surface_id()))
        });
        for pane in &self.previews {
            let visible = front == Some(pane.read(cx).surface_id());
            pane.update(cx, |pane, cx| pane.set_visible(visible, window, cx));
        }
    }

    /// Keeps a pane per device tab of the showing thread, and lets only the one in front stream.
    fn sync_devices(&mut self, thread_id: Option<&str>, panel: Option<&ThreadPanel>, cx: &mut Context<Self>) {
        let front = panel.filter(|panel| panel.is_open).and_then(|panel| panel.active_surface_id.as_deref());
        self.devices.retain(|pane| {
            let pane = pane.read(cx);
            thread_id == Some(pane.thread_id()) && panel.is_some_and(|panel| panel.surfaces.iter().any(|surface| surface.id == pane.surface_id()))
        });
        for pane in &self.devices {
            let visible = front == Some(pane.read(cx).surface_id());
            pane.update(cx, |pane, cx| pane.set_visible(visible, cx));
        }
    }

    fn device_pane(&mut self, thread_id: &str, surface: &Surface, cx: &mut Context<Self>) -> Option<Entity<crate::device::DevicePane>> {
        if let Some(pane) = self.devices.iter().find(|pane| pane.read(cx).surface_id() == surface.id) {
            return Some(pane.clone());
        }
        let devices = crate::device::devices(cx)?;
        let (state, thread_id, surface_id) = (self.state.clone(), thread_id.to_owned(), surface.id.clone());
        let pane = cx.new(|cx| crate::device::DevicePane::new(state, devices, thread_id, surface_id, cx));
        pane.update(cx, |pane, cx| pane.set_visible(true, cx));
        self.devices.push(pane.clone());
        Some(pane)
    }

    fn preview_pane(&mut self, thread_id: &str, surface: &Surface, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<crate::preview::PreviewPane>> {
        if let Some(pane) = self.previews.iter().find(|pane| pane.read(cx).surface_id() == surface.id) {
            return Some(pane.clone());
        }
        let previews = crate::preview::previews(cx)?;
        let (state, thread_id, surface_id, tab) = (self.state.clone(), thread_id.to_owned(), surface.id.clone(), surface.preview_tab().map(str::to_owned));
        let pane = cx.new(|cx| crate::preview::PreviewPane::new(state, previews, thread_id, surface_id, tab, window, cx));
        pane.update(cx, |pane, cx| pane.set_visible(true, window, cx));
        self.previews.push(pane.clone());
        Some(pane)
    }

    /// Keeps one `PanelTerminalView` per open right-panel terminal tab of the showing thread.
    fn sync_terminals(&mut self, thread_id: Option<&str>, panel: Option<&ThreadPanel>, window: &mut Window, cx: &mut Context<Self>) {
        let front = panel.filter(|panel| panel.is_open).and_then(|panel| panel.active_surface_id.as_deref());
        self.terminals.retain(|(tid, sid, _)| {
            thread_id == Some(tid.as_str()) && panel.is_some_and(|panel| panel.surfaces.iter().any(|surface| surface.id == *sid))
        });
        for (_, sid, view) in &self.terminals {
            let visible = front == Some(sid.as_str());
            view.update(cx, |view, cx| view.set_visible(visible, window, cx));
        }
    }

    fn terminal_pane(&mut self, thread_id: &str, surface: &Surface, window: &mut Window, cx: &mut Context<Self>) -> Entity<crate::terminal::PanelTerminalView> {
        if let Some((_, _, view)) = self.terminals.iter().find(|(tid, sid, _)| tid == thread_id && sid == &surface.id) {
            return view.clone();
        }
        let (state, tid, sid) = (self.state.clone(), thread_id.to_owned(), surface.id.clone());
        let view = cx.new(|cx| crate::terminal::PanelTerminalView::new(state, tid.clone(), sid.clone(), window, cx));
        view.update(cx, |view, cx| view.set_visible(true, window, cx));
        self.terminals.push((tid, sid, view.clone()));
        view
    }

    /// Keeps one embedded `DetailView` per open single-PR tab of the showing thread.
    fn sync_pull_request_tabs(&mut self, thread_id: Option<&str>, panel: Option<&ThreadPanel>, cx: &mut Context<Self>) {
        self.pull_request_tabs.retain(|(tid, sid, _)| {
            thread_id == Some(tid.as_str()) && panel.is_some_and(|panel| panel.surfaces.iter().any(|surface| surface.id == *sid))
        });
        // Persist any detail-tab switches made inside an embedded single-PR view.
        if let (Some(thread_id), Some(panel)) = (thread_id, panel) {
            let mut updates = Vec::new();
            for (tid, sid, view) in &self.pull_request_tabs {
                if tid != thread_id {
                    continue;
                }
                let current = match view.read(cx).current_tab() {
                    crate::pull_requests::DetailTab::Summary => PullRequestDetailTab::Summary,
                    crate::pull_requests::DetailTab::Guide => PullRequestDetailTab::Guide,
                    crate::pull_requests::DetailTab::Timeline => PullRequestDetailTab::Timeline,
                    crate::pull_requests::DetailTab::Code => PullRequestDetailTab::Code,
                };
                if let Some(surface) = panel.surfaces.iter().find(|s| s.id == *sid)
                    && let SurfaceKind::PullRequest { detail_tab, .. } = &surface.kind
                    && detail_tab.unwrap_or_default() != current
                {
                    updates.push((sid.clone(), current));
                }
            }
            if !updates.is_empty() {
                let thread_id = thread_id.to_owned();
                let state = self.state.clone();
                cx.defer(move |cx| {
                    state.update(cx, |state, cx| {
                        for (sid, tab) in updates {
                            state.set_pull_request_surface_detail_tab(&thread_id, &sid, tab, cx);
                        }
                    });
                });
            }
        }
    }

    fn pull_request_tab_pane(&mut self, thread_id: &str, surface: &Surface, cx: &mut Context<Self>) -> Option<Entity<crate::pull_requests::DetailView>> {
        let SurfaceKind::PullRequest { project_id, host, repository, number, reveal_url, detail_tab, .. } = &surface.kind else {
            return None;
        };
        if let Some((_, _, view)) = self.pull_request_tabs.iter().find(|(tid, sid, _)| tid == thread_id && sid == &surface.id) {
            if let Some(url) = reveal_url.clone() {
                view.update(cx, |view, cx| view.reveal(&url, cx));
                let (state, tid, sid) = (self.state.clone(), thread_id.to_owned(), surface.id.clone());
                cx.defer(move |cx| {
                    state.update(cx, |state, cx| state.settle_pull_request_surface_reveal(&tid, &sid, &url, cx));
                });
            }
            return Some(view.clone());
        }
        let reference = PrRef {
            project_id: project_id.clone(),
            host: host.clone(),
            repository: repository.clone(),
            number: *number,
        };
        let state_close = self.state.clone();
        let close_sid = surface.id.clone();
        let on_close = std::rc::Rc::new(move |cx: &mut App| {
            state_close.update(cx, |state, cx| state.close_surface(&close_sid, cx));
        });
        let state_select = self.state.clone();
        let on_select = std::rc::Rc::new(move |pr_ref: PrRef, cx: &mut App| {
            state_select.update(cx, |state, cx| {
                let target = PullRequestTarget {
                    environment_id: state.environment().map(str::to_owned),
                    project_id: pr_ref.project_id,
                    host: pr_ref.host,
                    repository: pr_ref.repository,
                    number: pr_ref.number,
                    url: None,
                    reveal_url: None,
                    detail_tab: None,
                };
                state.open_pull_request_surface(target, cx);
            });
        });
        let initial_tab = detail_tab.unwrap_or_default();
        let initial_reveal = reveal_url.clone();
        let state = self.state.clone();
        let view = cx.new(|cx| {
            let mut detail = crate::pull_requests::DetailView::new_embedded(state, reference, None, on_close, on_select, cx);
            let mapped = match initial_tab {
                PullRequestDetailTab::Summary => crate::pull_requests::DetailTab::Summary,
                PullRequestDetailTab::Guide => crate::pull_requests::DetailTab::Guide,
                PullRequestDetailTab::Timeline => crate::pull_requests::DetailTab::Timeline,
                PullRequestDetailTab::Code => crate::pull_requests::DetailTab::Code,
            };
            if mapped != crate::pull_requests::DetailTab::Summary {
                detail.set_tab(mapped, cx);
            }
            if let Some(url) = &initial_reveal {
                detail.reveal(url, cx);
            }
            detail
        });
        if let Some(url) = initial_reveal {
            let (state, tid, sid) = (self.state.clone(), thread_id.to_owned(), surface.id.clone());
            cx.defer(move |cx| {
                state.update(cx, |state, cx| state.settle_pull_request_surface_reveal(&tid, &sid, &url, cx));
            });
        }
        self.pull_request_tabs.push((thread_id.to_owned(), surface.id.clone(), view.clone()));
        Some(view)
    }

    fn render_resize_handle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let dragging = cx.has_active_drag();
        div()
            .id("right-panel-resize")
            .group("resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-4.))
            .w(px(8.))
            .cursor_col_resize()
            .on_drag(ResizeDrag, |_, _, _, cx| cx.new(|_| EmptyView))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(3.5))
                    .w(px(1.))
                    .when(dragging, |this| this.bg(hsla(p.primary).opacity(0.6)))
                    .when(!dragging, |this| this.group_hover("resize", |style| style.bg(hsla(p.border)))),
            )
    }
}

impl Render for RightPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let thread_id = state.panel_thread();
        let panel = thread_id.as_deref().map(|thread_id| state.panels.panels.panel(thread_id).clone());
        let viewport = f32::from(window.viewport_size().width);
        // Match `ChatView.tsx:1074`: maximized panels render full-width inline (since `workspace.rs`
        // hides the center column when `panel.maximized` is true) and keep the restore button visible.
        // Update `in_sheet_mode` before early-returning for a closed panel so window resizes while
        // closed never leave proactive panel suppression stuck.
        let sheet_mode = is_sheet_mode(viewport) && !panel.as_ref().is_some_and(|panel| panel.maximized);
        let sheet_mode_changed = self.in_sheet_mode != sheet_mode;
        self.in_sheet_mode = sheet_mode;
        let proactive_enabled = crate::settings::store::try_client(cx).is_some_and(|doc| doc.proactive_panels_enabled());
        if self.last_proactive_enabled != Some(proactive_enabled) || sheet_mode_changed {
            let weak = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |this, cx| this.evaluate_proactive(cx));
            });
        }
        self.sync_previews(thread_id.as_deref(), panel.as_ref(), window, cx);
        self.sync_devices(thread_id.as_deref(), panel.as_ref(), cx);
        self.sync_terminals(thread_id.as_deref(), panel.as_ref(), window, cx);
        self.sync_pull_request_tabs(thread_id.as_deref(), panel.as_ref(), cx);
        let state = self.state.read(cx);
        let (Some(thread_id), Some(panel)) = (thread_id, panel) else {
            self.last_open_state = None;
            self.panel_animation = None;
            self._close_animation_task = None;
            return div().into_any_element();
        };
        let animation_duration_ms = crate::settings::store::try_client(cx)
            .map_or(0, t3_core::client_settings::ClientSettingsDoc::panel_animation_duration_ms);
        let same_thread_opened = matches!(
            self.last_open_state.as_ref(),
            Some((prev_thread, false)) if prev_thread == &thread_id
        ) && panel.is_open;
        let same_thread_closed = matches!(
            self.last_open_state.as_ref(),
            Some((prev_thread, true)) if prev_thread == &thread_id
        ) && !panel.is_open;
        let thread_switched = self.last_open_state.as_ref().is_some_and(|(prev, _)| prev != &thread_id);
        self.last_open_state = Some((thread_id.clone(), panel.is_open));
        if animation_duration_ms == 0 || thread_switched {
            self.panel_animation = None;
            self._close_animation_task = None;
        }
        if !panel.is_open {
            if same_thread_closed && animation_duration_ms > 0 && !panel.maximized {
                let from_width = self.last_rendered_width.max(1.);
                self.animation_seq = self.animation_seq.wrapping_add(1);
                let seq = self.animation_seq;
                self.panel_animation = Some(PanelAnimation {
                    seq,
                    duration_ms: animation_duration_ms,
                    opening: false,
                    from_width,
                });
                let wait = Duration::from_millis(u64::from(animation_duration_ms) + 16);
                self._close_animation_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.panel_animation.is_some_and(|a| !a.opening && a.seq == seq) {
                            this.panel_animation = None;
                            this._close_animation_task = None;
                            cx.notify();
                        }
                    });
                }));
            }
            if let Some(anim) = self.panel_animation.filter(|a| !a.opening) {
                let p = palette(cx);
                let closing_el = div()
                    .id("right-panel-closing")
                    .debug_selector(|| "right-panel-closing".to_owned())
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .bg(hsla(p.background))
                    .border_l_1()
                    .border_color(hsla(p.border))
                    .with_animation(
                        ElementId::Name(format!("right-panel-close-{}", anim.seq).into()),
                        Animation::new(Duration::from_millis(u64::from(anim.duration_ms))).with_easing(ease_out_quint()),
                        move |el, delta| el.w(px(anim.from_width * (1. - delta))),
                    );
                return closing_el.into_any_element();
            }
            return div().into_any_element();
        }
        let stored = state.panels.panels.width(&thread_id).unwrap_or(DEFAULT_WIDTH);
        let width = if sheet_mode {
            sheet_width(viewport)
        } else {
            clamp_width(stored, viewport, viewport - sidebar_inline_width(state, viewport))
        };
        self.last_rendered_width = width;
        if same_thread_opened && animation_duration_ms > 0 && (sheet_mode || !panel.maximized) {
            self._close_animation_task = None;
            self.animation_seq = self.animation_seq.wrapping_add(1);
            self.panel_animation = Some(PanelAnimation {
                seq: self.animation_seq,
                duration_ms: animation_duration_ms,
                opening: true,
                from_width: 0.,
            });
        } else if self.panel_animation.is_some_and(|a| !a.opening) {
            self.panel_animation = None;
            self._close_animation_task = None;
        }
        // Maximized beside a hidden sidebar, the tab bar clears the traffic lights and the
        // sidebar toggle as the thread header does (design-system §5).
        let titlebar_inset = !sheet_mode && panel.maximized && sidebar_inline_width(state, viewport) == 0.;
        let p = palette(cx);
        let content = match panel.active().map(|surface| &surface.kind) {
            // Cached: transcript streaming redraws the window, but only diff changes repaint it.
            Some(SurfaceKind::Diff) => self.diff.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            Some(SurfaceKind::PullRequests) => self.pull_requests.clone().into_any_element(),
            Some(SurfaceKind::PullRequest { .. }) => match panel.active().and_then(|surface| self.pull_request_tab_pane(&thread_id, surface, cx)) {
                Some(pane) => pane.into_any_element(),
                None => div().into_any_element(),
            },
            Some(SurfaceKind::Files { .. } | SurfaceKind::File { .. }) => {
                // Markdown selection registrations cannot survive cached scene reuse.
                self.files.clone().into_any_element()
            }
            Some(SurfaceKind::Terminal { .. }) => match panel.active() {
                Some(surface) => self.terminal_pane(&thread_id, surface, window, cx).into_any_element(),
                None => div().into_any_element(),
            },
            Some(SurfaceKind::SideChat { thread_id }) => self.side_chat_pane(thread_id, window, cx).into_any_element(),
            // Cached: frames repaint the page view inside, not the panel around it.
            Some(SurfaceKind::Preview { .. }) => match panel.active().and_then(|surface| self.preview_pane(&thread_id, surface, window, cx)) {
                Some(pane) => pane.cached(StyleRefinement::default().size_full()).into_any_element(),
                None => div().into_any_element(),
            },
            // Cached: frames repaint the device screen inside, not the panel around it.
            Some(SurfaceKind::Device { .. }) => match panel.active().and_then(|surface| self.device_pane(&thread_id, surface, cx)) {
                Some(pane) => pane.cached(StyleRefinement::default().size_full()).into_any_element(),
                None => div().into_any_element(),
            },
            None => tabs::empty_state(self.state.clone(), window, cx).into_any_element(),
        };
        let view = cx.entity();
        let add_menu = self.add_menu_open.then(|| {
            let entries = tabs::add_menu_entries(self.state.read(cx), cx);
            let dismiss_view = view.clone();
            let key_view = view.clone();
            tabs::render_add_menu(
                self.state.clone(),
                &entries,
                self.add_menu_highlighted,
                &self.add_menu_focus,
                move |outside, cx| {
                    dismiss_view.update(cx, |this, cx| {
                        if outside {
                            this.dismiss_add_surface_menu_outside(cx);
                        } else {
                            this.close_add_surface_menu(cx);
                        }
                    });
                },
                move |event, window, cx| {
                    key_view.update(cx, |this, cx| this.handle_add_menu_key_down(event, window, cx));
                },
                cx,
            )
        });
        let toggle_view = view.clone();
        let rename_view = view.clone();
        let cancel_rename_view = view.clone();
        let renaming = self.renaming_device.as_ref().map(|(id, input, _)| (id.as_str(), input));
        let panel_el = v_flex()
            .id("right-panel")
            .relative()
            .h_full()
            .min_h_0()
            .bg(hsla(p.background))
            .when(!sheet_mode && panel.maximized, |this| this.flex_1().min_w_0())
            .when(sheet_mode || !panel.maximized, |this| {
                this.flex_none().w(px(width)).border_l_1().border_color(hsla(p.border))
            })
            .when(sheet_mode, |this| this.shadow_lg().occlude())
            .when(!sheet_mode, |this| this.on_drag_move(cx.listener(Self::resize)))
            .when(!sheet_mode && !panel.maximized, |this| this.child(self.render_resize_handle(cx)))
            .child(tabs::tab_bar(
                self.state.clone(),
                &panel,
                titlebar_inset,
                sheet_mode,
                self.add_menu_open,
                renaming,
                move |window, cx| {
                    toggle_view.update(cx, |this, cx| this.toggle_add_surface_menu(window, cx));
                },
                move |surface_id, current_title, window, cx| {
                    rename_view.update(cx, |this, cx| this.start_device_rename(surface_id, current_title, window, cx));
                },
                move |cx| {
                    cancel_rename_view.update(cx, |this, cx| this.cancel_device_rename(cx));
                },
                window,
                cx,
            ))
            .child(div().flex_1().min_h_0().flex().flex_col().child(content))
            .children(add_menu);

        let panel_el = if sheet_mode {
            panel_el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        } else {
            panel_el
        };
        let animated_panel: gpui::AnyElement = match self.panel_animation {
            Some(anim) if anim.opening && (sheet_mode || !panel.maximized) => panel_el
                .overflow_hidden()
                .with_animation(
                    ElementId::Name(format!("right-panel-open-{}", anim.seq).into()),
                    Animation::new(Duration::from_millis(u64::from(anim.duration_ms))).with_easing(ease_out_quint()),
                    move |el, delta| el.w(px(width * delta)),
                )
                .into_any_element(),
            _ => panel_el.into_any_element(),
        };

        if sheet_mode {
            let dismiss_state = self.state.clone();
            let esc_state = self.state.clone();
            let size = window.viewport_size();
            return div()
                .child(
                    gpui::deferred(
                        gpui::anchored()
                            .position_mode(AnchoredPositionMode::Window)
                            .position(point(px(0.), px(0.)))
                            .child(
                                div()
                                    .id("right-panel-sheet-backdrop")
                                    .track_focus(&self.sheet_focus)
                                    .w(size.width)
                                    .h(size.height)
                                    .flex()
                                    .justify_end()
                                    .bg(hsla(p.background).opacity(0.45))
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                        dismiss_state.update(cx, |state, cx| state.toggle_right_panel(cx));
                                    })
                                    .on_key_down(move |event, _, cx| {
                                        if event.keystroke.key == "escape" {
                                            cx.stop_propagation();
                                            esc_state.update(cx, |state, cx| state.toggle_right_panel(cx));
                                        }
                                    })
                                    .child(animated_panel),
                            ),
                    )
                    .with_priority(40),
                )
                .into_any_element();
        }
        animated_panel
    }
}

/// The window's top-right controls (`PanelLayoutControls.tsx` right panel toggle and
/// `RightPanelMaximizeControl`): in the tab bar while the panel shows, else in the header.
pub fn header_controls(state: &Entity<AppState>, cx: &App) -> impl IntoElement {
    header_controls_with_options(state, true, cx)
}

pub(super) fn header_controls_with_options(state: &Entity<AppState>, show_maximize: bool, cx: &App) -> impl IntoElement {
    let app = state.read(cx);
    let thread = app.panel_thread();
    let open = thread.as_deref().is_some_and(|thread| app.panels.panels.is_open(thread));
    let maximized = thread.as_deref().is_some_and(|thread| app.panels.panels.panel(thread).maximized);
    let toggle_state = state.clone();
    let maximize_state = state.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .when(open && show_maximize, |this| {
            this.child(
                Button::new("right-panel-maximize")
                    .variant(Variant::Toggle)
                    .icon(if maximized { Icon::Minimize2 } else { Icon::Maximize2 })
                    .pressed(maximized)
                    .tooltip(if maximized { "Restore panel size" } else { "Maximize panel" })
                    .on_click(move |_, _, cx| maximize_state.update(cx, |state, cx| state.toggle_right_panel_maximized(cx))),
            )
        })
        .child(
            Button::new("right-panel-toggle")
                .variant(Variant::Toggle)
                .icon(Icon::PanelRight)
                .pressed(open)
                .disabled(thread.is_none())
                .tooltip(crate::keymap::label("rightPanel.toggle", cx).map_or_else(|| "Toggle right panel".to_owned(), |key| format!("Toggle right panel ({key})")))
                .on_click(move |_, _, cx| toggle_state.update(cx, |state, cx| state.toggle_right_panel(cx))),
        )
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::Keystroke;

    #[gpui::test]
    fn right_panel_new_action_opens_add_menu_and_handles_shortcuts(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::store::init(cx);
            installed
        });
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
        });

        let (panel, vcx) = cx.add_window_view(|window, cx| RightPanelView::new(state.clone(), window, cx));

        // While right panel is closed, `open_add_surface_menu` is a no-op.
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
            assert!(!panel.read(cx).is_add_menu_open());
        });

        // Open right panel -> `open_add_surface_menu` opens the add-surface menu and focuses it.
        state.update(vcx, |state, cx| state.toggle_right_panel(cx));
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
            assert!(panel.read(cx).is_add_menu_open());
            window.draw(cx).clear();
        });

        // Pressing Escape dismisses the menu.
        vcx.update(|window, cx| {
            let esc = KeyDownEvent { keystroke: Keystroke::parse("escape").unwrap(), is_held: false, prefer_character_input: false };
            panel.update(cx, |panel, cx| panel.handle_add_menu_key_down(&esc, window, cx));
            assert!(!panel.read(cx).is_add_menu_open());
        });

        // Clicking the `+` trigger while the menu is open fires `on_mouse_down_out` before `on_click`:
        // the reopen guard keeps the menu closed instead of reopening on mouse-up, while `Mod+T` still opens it.
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
            assert!(panel.read(cx).is_add_menu_open());
            panel.update(cx, |panel, cx| {
                panel.dismiss_add_surface_menu_outside(cx);
                panel.toggle_add_surface_menu(window, cx);
            });
            assert!(!panel.read(cx).is_add_menu_open(), "mouse-down-out + trigger click must stay closed");
            panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
            assert!(panel.read(cx).is_add_menu_open(), "keyboard shortcut still opens immediately");
            panel.update(cx, |panel, cx| panel.close_add_surface_menu(cx));
        });

        // Re-open menu and press `d` (Diff shortcut) -> closes menu and opens Diff surface.
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
            assert!(panel.read(cx).is_add_menu_open());
            let diff_key = KeyDownEvent { keystroke: Keystroke::parse("d").unwrap(), is_held: false, prefer_character_input: false };
            panel.update(cx, |panel, cx| panel.handle_add_menu_key_down(&diff_key, window, cx));
            assert!(!panel.read(cx).is_add_menu_open());
        });
        state.read_with(vcx, |state, _| {
            let active = state.panels.panels.active_surface("menu-thread").expect("diff surface opened");
            assert!(matches!(active.kind, SurfaceKind::Diff));
        });

        // Closing the right panel resets an open add menu.
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.open_add_surface_menu(window, cx));
            assert!(panel.read(cx).is_add_menu_open());
        });
        state.update(vcx, |state, cx| state.toggle_right_panel(cx));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(!panel.read(cx).is_add_menu_open());
        });

        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn right_panel_renders_terminal_pr_device_rename_and_proactive_toggle(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::store::init(cx);
            installed
        });
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
        });

        let (panel, vcx) = cx.add_window_view(|window, cx| RightPanelView::new(state.clone(), window, cx));

        // 1. Device tab rename commits trimmed title and falls back to device name on blank.
        let device = t3_core::right_panel::DeviceTarget {
            host_id: "local".into(),
            device_id: "dev-1".into(),
            platform: t3_core::device::Platform::Ios,
            name: "iPhone 16".into(),
        };
        state.update(vcx, |state, cx| {
            state.mutate_panels(cx, |panels, thread| panels.panels.open_device(thread, device));
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            panel.update(cx, |panel, cx| {
                panel.start_device_rename("device:local:dev-1".into(), "iPhone 16".into(), window, cx);
                assert!(panel.renaming_device.is_some());
                panel.renaming_device = None;
            });
        });
        state.update(vcx, |state, cx| state.rename_device_surface("device:local:dev-1", "QA Phone", cx));
        state.read_with(vcx, |state, _| {
            assert_eq!(state.panels.panels.active_surface("menu-thread").map(Surface::title).as_deref(), Some("QA Phone"));
        });
        state.update(vcx, |state, cx| state.rename_device_surface("device:local:dev-1", "   ", cx));
        state.read_with(vcx, |state, _| {
            assert_eq!(state.panels.panels.active_surface("menu-thread").map(Surface::title).as_deref(), Some("iPhone 16"));
        });

        // 2. Right-panel terminal tab & single-PR tab mount, render, and prune cleanly on close.
        let pr_target = PullRequestTarget {
            environment_id: Some("local".into()),
            project_id: "proj-1".into(),
            host: Some("github.com".into()),
            repository: "owner/repo".into(),
            number: 99,
            url: Some("https://github.com/owner/repo/pull/99".into()),
            reveal_url: None,
            detail_tab: Some(PullRequestDetailTab::Code),
        };
        state.update(vcx, |state, cx| {
            state.open_panel_terminal("menu-thread", "term-2", cx);
            state.open_pull_request_surface(pr_target, cx);
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(panel.read(cx).pull_request_tabs.len(), 1);
            // Switch active tab to terminal and draw it.
            state.update(cx, |state, cx| state.activate_surface("terminal:term-2", cx));
            window.draw(cx).clear();
            assert_eq!(panel.read(cx).terminals.len(), 1);
        });
        state.update(vcx, |state, cx| state.close_all_surfaces(cx));
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(panel.read(cx).terminals.is_empty());
            assert!(panel.read(cx).pull_request_tabs.is_empty());
        });

        // 3. Device rename cancel (Escape) drops the inline editor without changing the tab title.
        state.update(vcx, |state, cx| {
            state.mutate_panels(cx, |panels, thread| {
                panels.panels.open_device(
                    thread,
                    t3_core::right_panel::DeviceTarget {
                        host_id: "local".into(),
                        device_id: "dev-2".into(),
                        platform: t3_core::device::Platform::Ios,
                        name: "Original Phone".into(),
                    },
                )
            });
        });
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.start_device_rename("device:local:dev-2".into(), "Original Phone".into(), window, cx);
                if let Some((_, input, _)) = panel.renaming_device.as_ref() {
                    input.update(cx, |input, cx| input.set_value("Discarded", window, cx));
                }
                panel.cancel_device_rename(cx);
                assert!(panel.renaming_device.is_none());
            });
        });
        state.read_with(vcx, |state, _| {
            assert_eq!(state.panels.panels.active_surface("menu-thread").map(Surface::title).as_deref(), Some("Original Phone"));
        });

        // 4. Attachment tab renders in FilesPanel even when no workspace is open.
        state.update(vcx, |state, cx| {
            state.open_attachment(
                t3_core::right_panel::FileAttachment {
                    id: "att-42".into(),
                    name: "notes.txt".into(),
                    mime_type: "text/plain".into(),
                    size_bytes: 256,
                    url: None,
                },
                cx,
            );
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        state.read_with(vcx, |state, _| {
            assert_eq!(state.panels.panels.active_surface("menu-thread").map(Surface::id), Some("attachment:att-42"));
        });

        // 5. Resolved PR links open a single-PR right-panel tab when a thread is selected.
        vcx.update(|_, cx| {
            crate::pull_requests::open_resolved_link(
                &state,
                t3_core::pull_requests::target::Target {
                    environment: state.read(cx).environment().unwrap_or("local").to_owned(),
                    reference: PrRef {
                        project_id: "proj-1".into(),
                        host: Some("github.com".into()),
                        repository: "owner/repo".into(),
                        number: 101,
                    },
                    url: "https://github.com/owner/repo/pull/101".into(),
                },
                cx,
            );
        });
        state.read_with(vcx, |state, _| {
            assert!(!state.pull_requests_open);
            let active = state.panels.panels.active_surface("menu-thread").expect("PR tab opened");
            assert!(matches!(active.kind, SurfaceKind::PullRequest { number: 101, .. }));
        });

        // 6. ProactivePanelsEnabled client setting updates live via AppEvent::RightPanel.
        vcx.update(|_, cx| {
            crate::settings::store::update_client(
                &state,
                t3_core::client_settings::ClientSettingChange::ProactivePanelsEnabled(true),
                cx,
            );
            assert!(crate::settings::store::client(cx).proactive_panels_enabled());
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert_eq!(panel.read(cx).last_proactive_enabled, Some(true));
        });

        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn narrow_window_sheet_mode_respects_maximize_and_escape_dismiss(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::store::init(cx);
            installed
        });
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
            state.add_diff_surface(cx);
        });

        let window = cx.open_window(gpui::size(px(800.), px(700.)), |window, cx| RightPanelView::new(state.clone(), window, cx));
        let panel = window.entity(cx).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        // Narrow window + unmaximized -> sheet mode active.
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(panel.read(cx).in_sheet_mode);
        });

        // Maximizing at narrow width exits sheet mode so the panel fills full width inline
        // with the restore button visible (never blanking center column with a narrow overlay).
        state.update(&mut vcx, |state, cx| state.toggle_right_panel_maximized(cx));
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(!panel.read(cx).in_sheet_mode);
        });

        // Restoring returns to sheet mode; pressing Escape on the sheet closes the panel.
        state.update(&mut vcx, |state, cx| state.toggle_right_panel_maximized(cx));
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(panel.read(cx).in_sheet_mode);
            let focus = panel.read(cx).sheet_focus.clone();
            window.focus(&focus, cx);
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        });
        vcx.run_until_parked();
        state.read_with(&vcx, |state, _| {
            assert!(!state.panels.panels.is_open("menu-thread"), "Escape should dismiss sheet");
        });

        // Widening above 980px while the panel is closed must clear `in_sheet_mode` so proactive
        // opens are not left stuck suppressed; shrinking back while closed must re-enable it.
        vcx.simulate_resize(gpui::size(px(1200.), px(700.)));
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(!panel.read(cx).in_sheet_mode, "widening while closed must clear in_sheet_mode");
        });
        vcx.simulate_resize(gpui::size(px(800.), px(700.)));
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(panel.read(cx).in_sheet_mode, "shrinking while closed must set in_sheet_mode");
        });

        state.update(&mut vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn proactive_panels_opens_on_git_event_after_turn_settle_and_live_setting_toggle(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::store::init(cx);
            let _ = crate::thread_details::init(installed.0.clone(), cx);
            installed
        });
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
        });

        let (panel, vcx) = cx.add_window_view(|window, cx| RightPanelView::new(state.clone(), window, cx));

        // Turn on proactive panels live via ClientSettingChange (emits AppEvent::RightPanel).
        vcx.update(|_, cx| {
            crate::settings::store::update_client(
                &state,
                t3_core::client_settings::ClientSettingChange::ProactivePanelsEnabled(true),
                cx,
            );
        });
        vcx.run_until_parked();
        assert_eq!(panel.read_with(vcx, |p, _| p.last_proactive_enabled), Some(true));

        // Seed a completed settled turn touching 3 files while GitStore status is still None -> defers.
        state.update(vcx, |state, cx| {
            let mut proj: t3_core::thread::ThreadProjection = serde_json::from_value(serde_json::json!({
                "thread": {
                    "id": "menu-thread",
                    "projectId": "menu-project",
                    "title": "Test",
                    "providerInstanceId": "codex",
                    "modelSelection": { "provider": "codex", "model": "gpt-5" },
                    "runtimeMode": "full-access",
                    "interactionMode": "default",
                    "branch": "main",
                    "worktreePath": null,
                    "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "menu-thread" },
                    "createdAt": "2026-11-19T00:00:00Z",
                    "updatedAt": "2026-11-19T00:00:00Z",
                    "archivedAt": null
                },
                "runs": [{
                    "id": "run-1",
                    "ordinal": 1,
                    "providerInstanceId": "codex",
                    "modelSelection": { "provider": "codex", "model": "gpt-5" },
                    "userMessageId": "m1",
                    "rootNodeId": null,
                    "status": "completed",
                    "requestedAt": "2026-11-19T00:00:00Z",
                    "startedAt": "2026-11-19T00:00:01Z",
                    "completedAt": "2026-11-19T00:00:05Z"
                }],
                "attempts": [],
                "nodes": [],
                "subagents": [],
                "providerSessions": [],
                "providerThreads": [],
                "providerTurns": [],
                "runtimeRequests": [],
                "messages": [],
                "plans": [],
                "turnItems": [],
                "checkpointScopes": [],
                "checkpoints": [{
                    "id": "cp-1",
                    "runId": "run-1",
                    "appRunOrdinal": 1,
                    "status": "ready",
                    "capturedAt": "2026-11-19T00:00:05Z",
                    "files": [
                        { "path": "a.rs", "kind": "modified", "additions": 5, "deletions": 1 },
                        { "path": "b.rs", "kind": "modified", "additions": 5, "deletions": 1 },
                        { "path": "c.rs", "kind": "modified", "additions": 5, "deletions": 1 }
                    ]
                }],
                "contextHandoffs": [],
                "contextTransfers": [],
                "visibleTurnItems": [],
                "updatedAt": "2026-11-19T00:00:05Z"
            }))
            .unwrap();
            proj.checkpoints.retain(|_| true);
            state.thread.projection = Some(proj);
            cx.emit(AppEvent::Thread);
        });
        vcx.run_until_parked();
        state.read_with(vcx, |state, _| {
            assert!(!state.panels.panels.is_open("menu-thread"), "must defer while is_git_repo is None");
        });

        // Async GitStore status arrives with `is_repo: true` -> GitEvent::Changed triggers proactive Diff open!
        vcx.update(|_, cx| {
            let details = crate::thread_details::details(cx).expect("details initialized");
            let git = details.read(cx).git.clone();
            git.update(cx, |git, cx| {
                git.target = Some(crate::git::Target {
                    thread_id: "menu-thread".into(),
                    project_id: "menu-project".into(),
                    is_draft: false,
                    cwd: "/tmp".into(),
                    project_cwd: "/tmp".into(),
                    worktree_path: None,
                    branch: Some("main".into()),
                });
                git.status = Some(t3_core::vcs::VcsStatus::default());
                cx.emit(crate::git::GitEvent::Changed);
            });
        });
        vcx.run_until_parked();
        state.read_with(vcx, |state, _| {
            assert!(state.panels.panels.is_open("menu-thread"), "GitEvent::Changed should trigger proactive Diff open");
            assert_eq!(state.panels.panels.active_surface("menu-thread"), Some(&Surface::diff()));
        });

        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn panel_terminal_splits_and_focuses_after_closing_sibling_tab(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::store::init(cx);
            installed
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(serde_json::json!({
                    "authenticated": true,
                    "permissions": ["terminal:read", "terminal:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
        });

        let (panel, vcx) = cx.add_window_view(|window, cx| {
            let _drawer = cx.new(|cx| crate::terminal::TerminalDrawer::new(state.clone(), window, cx));
            RightPanelView::new(state.clone(), window, cx)
        });

        state.update(vcx, |state, cx| {
            state.open_panel_terminal("menu-thread", "term-2", cx);
            state.split_panel_terminal(
                "menu-thread",
                "terminal:term-2",
                "term-3",
                t3_core::terminal::SplitDirection::Horizontal,
                cx,
            );
            // Open a sibling Diff tab on top, then close it -> should queue focus for term-3.
            state.add_diff_surface(cx);
            state.close_surface("diff", cx);
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(panel.read(cx).terminals.len(), 1);
            assert_eq!(panel.read(cx).terminals[0].2.read(cx).pane_count_for_test(), 2);
        });

        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }
}
