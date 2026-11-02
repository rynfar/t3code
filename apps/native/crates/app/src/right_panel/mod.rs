//! The inline right panel beside the thread: per-thread width with an 8px resize handle, a tab
//! bar, and the active surface. Ports `components/RightPanelTabs.tsx`,
//! `components/preview/PreviewPanelShell.tsx`, `components/preview/RightPanelResizeHandle.tsx`
//! and `hooks/usePreviewPanelInlineSize.ts`. New surfaces add a `SurfaceKind` and a content arm.

mod controls;
mod state;
mod tabs;

use std::time::Duration;

use gpui::{
    App, Context, DragMoveEvent, Entity, EmptyView, IntoElement, KeyBinding, ParentElement, Render, StyleRefinement,
    Styled, Subscription, Task, Window, actions, div, prelude::*, px,
};
use gpui_component::v_flex;
use t3_core::right_panel::{DEFAULT_WIDTH, SurfaceKind, clamp_width};

pub use controls::{Button, Size, Variant, focusable, segment, segmented, tooltip_view};
pub use state::PanelState;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::diff::DiffPanel;
use crate::theme::{hsla, palette};
use crate::workspace::SIDEBAR_WIDTH;

actions!(right_panel, [ToggleRightPanel, ToggleDiff, CloseActiveSurface, ReopenClosedView, ToggleMaximized]);

/// Default bindings from `packages/shared/src/keybindings.ts`.
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
    cx.bind_keys([
        KeyBinding::new("cmd-alt-b", ToggleRightPanel, None),
        KeyBinding::new("cmd-d", ToggleDiff, None),
        KeyBinding::new("cmd-w", CloseActiveSurface, None),
        KeyBinding::new("cmd-shift-t", ReopenClosedView, None),
    ]);
}

struct ResizeDrag;

pub struct RightPanelView {
    state: Entity<AppState>,
    diff: Entity<DiffPanel>,
    _save_width: Option<Task<()>>,
    _subscription: Subscription,
}

impl RightPanelView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffPanel::new(state.clone(), window, cx));
        let subscription = cx.subscribe(&state, |this, state, event: &AppEvent, cx| match event {
            AppEvent::RightPanel => cx.notify(),
            AppEvent::Selection => {
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
        RightPanelView { state, diff, _save_width: None, _subscription: subscription }
    }

    fn resize(&mut self, event: &DragMoveEvent<ResizeDrag>, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = f32::from(window.viewport_size().width);
        let width = clamp_width(f32::from(event.bounds.right() - event.event.position.x), viewport, viewport - SIDEBAR_WIDTH);
        self.state.update(cx, |state, cx| state.set_right_panel_width(width, false, cx));
        // Persist once the drag settles, not on every frame of it.
        self._save_width = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let _ = this.update(cx, |this, cx| this.state.update(cx, |state, cx| state.set_right_panel_width(width, true, cx)));
        }));
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
        let Some(thread_id) = state.selected_thread.clone() else { return div().into_any_element() };
        let panel = state.panels.panels.panel(&thread_id).clone();
        if !panel.is_open {
            return div().into_any_element();
        }
        let viewport = f32::from(window.viewport_size().width);
        let stored = state.panels.panels.width(&thread_id).unwrap_or(DEFAULT_WIDTH);
        let width = clamp_width(stored, viewport, viewport - SIDEBAR_WIDTH);
        let p = palette(cx);
        let content = match panel.active().map(|surface| &surface.kind) {
            // Cached: transcript streaming redraws the window, but only diff changes repaint it.
            Some(SurfaceKind::Diff) => self.diff.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
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
            .child(tabs::tab_bar(self.state.clone(), &panel, window, cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(content))
            .into_any_element()
    }
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
                .tooltip("Toggle right panel (⌥⌘B)")
                .on_click(move |_, _, cx| toggle_state.update(cx, |state, cx| state.toggle_right_panel(cx))),
        )
}
