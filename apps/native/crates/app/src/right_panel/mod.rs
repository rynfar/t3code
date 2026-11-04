//! The inline right panel beside the thread: per-thread width with an 8px resize handle, a tab
//! bar, and the active surface. Ports `components/RightPanelTabs.tsx`,
//! `components/preview/PreviewPanelShell.tsx`, `components/preview/RightPanelResizeHandle.tsx`
//! and `hooks/usePreviewPanelInlineSize.ts`. New surfaces add a `SurfaceKind` and a content arm.

mod controls;
mod state;
mod tabs;

use std::time::Duration;

use gpui::{
    App, Context, DragMoveEvent, Entity, EmptyView, IntoElement, ParentElement, Render, StyleRefinement,
    Styled, Subscription, Task, Window, actions, div, prelude::*, px,
};
use gpui_component::v_flex;
use t3_core::right_panel::{DEFAULT_WIDTH, Surface, SurfaceKind, ThreadPanel, clamp_width};

pub use controls::{Button, Size, Variant, focusable, segment, segmented, tooltip_view};
pub use state::PanelState;
#[cfg(feature = "snapshot")]
pub use tabs::open_surface;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::diff::DiffPanel;
use crate::files::FilesPanel;
use crate::theme::{hsla, palette};
use crate::workspace::SIDEBAR_WIDTH;

actions!(right_panel, [ToggleRightPanel, ToggleDiff, CloseActiveSurface, ReopenClosedView, ToggleMaximized]);

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
    _save_width: Option<Task<()>>,
    _subscription: Subscription,
}

impl RightPanelView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffPanel::new(state.clone(), window, cx));
        let pull_requests = cx.new(|cx| crate::pull_requests::ThreadPullRequestsPanel::new(state.clone(), cx));
        let files = cx.new(|cx| FilesPanel::new(state.clone(), window, cx));
        let subscription = cx.subscribe(&state, |this, state, event: &AppEvent, cx| match event {
            AppEvent::RightPanel => {
                this.prune_side_chats(cx);
                cx.notify();
            }
            AppEvent::Selection => {
                this.prune_side_chats(cx);
                // A pending width save belongs to the thread that was showing: flush it now.
                let flush = this._save_width.take().is_some();
                state.update(cx, |state, cx| {
                    if flush {
                        state.save_panels(cx);
                    }
                    state.right_panel_thread_shown(cx);
                });
                cx.notify();
            }
            _ => {}
        });
        RightPanelView { state, diff, pull_requests, files, side_chats: Vec::new(), previews: Vec::new(), devices: Vec::new(), _save_width: None, _subscription: subscription }
    }

    fn resize(&mut self, event: &DragMoveEvent<ResizeDrag>, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = f32::from(window.viewport_size().width);
        let row = viewport - sidebar_width(self.state.read(cx));
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
            .selected_thread
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
        let thread_id = state.selected_thread.clone();
        let panel = thread_id.as_deref().map(|thread_id| state.panels.panels.panel(thread_id).clone());
        self.sync_previews(thread_id.as_deref(), panel.as_ref(), window, cx);
        self.sync_devices(thread_id.as_deref(), panel.as_ref(), cx);
        let state = self.state.read(cx);
        let (Some(thread_id), Some(panel)) = (thread_id, panel) else { return div().into_any_element() };
        if !panel.is_open {
            return div().into_any_element();
        }
        let viewport = f32::from(window.viewport_size().width);
        let stored = state.panels.panels.width(&thread_id).unwrap_or(DEFAULT_WIDTH);
        let width = clamp_width(stored, viewport, viewport - sidebar_width(state));
        // Maximized beside a hidden sidebar, the tab bar clears the traffic lights and the
        // sidebar toggle as the thread header does (design-system §5).
        let titlebar_inset = panel.maximized && state.sidebar.collapsed;
        let p = palette(cx);
        let content = match panel.active().map(|surface| &surface.kind) {
            // Cached: transcript streaming redraws the window, but only diff changes repaint it.
            Some(SurfaceKind::Diff) => self.diff.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            Some(SurfaceKind::PullRequests) => self.pull_requests.clone().into_any_element(),
            Some(SurfaceKind::Files | SurfaceKind::File { .. }) => {
                // Markdown selection registrations cannot survive cached scene reuse.
                self.files.clone().into_any_element()
            }
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
        v_flex()
            .id("right-panel")
            .relative()
            .h_full()
            .min_h_0()
            .bg(hsla(p.background))
            .when(panel.maximized, |this| this.flex_1().min_w_0())
            .when(!panel.maximized, |this| {
                this.flex_none().w(px(width)).border_l_1().border_color(hsla(p.border))
            })
            .on_drag_move(cx.listener(Self::resize))
            .when(!panel.maximized, |this| this.child(self.render_resize_handle(cx)))
            .child(tabs::tab_bar(self.state.clone(), &panel, titlebar_inset, window, cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(content))
            .into_any_element()
    }
}

fn sidebar_width(state: &AppState) -> f32 {
    if state.sidebar.collapsed { 0. } else { SIDEBAR_WIDTH }
}

/// The window's top-right controls (`PanelLayoutControls.tsx` right panel toggle and
/// `RightPanelMaximizeControl`): in the tab bar while the panel shows, else in the header.
pub fn header_controls(state: &Entity<AppState>, cx: &App) -> impl IntoElement {
    let app = state.read(cx);
    let thread = app.selected_thread.as_deref();
    let open = thread.is_some_and(|thread| app.panels.panels.is_open(thread));
    let maximized = thread.is_some_and(|thread| app.panels.panels.panel(thread).maximized);
    let toggle_state = state.clone();
    let maximize_state = state.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .when(open, |this| {
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
