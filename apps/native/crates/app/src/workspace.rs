//! Window root: titlebar strip, sidebar, and the thread area, or the connect screen.

use gpui::{App, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, WindowControlArea, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::supervisor::ConnectionStatus;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::composer::Composer;
use crate::connect::ConnectView;
use crate::sidebar::SidebarPane;
use crate::thread_view::ThreadView;

pub const TITLEBAR_HEIGHT: f32 = 52.;
pub const SIDEBAR_WIDTH: f32 = 256.;

pub struct Workspace {
    state: Entity<AppState>,
    sidebar: Entity<SidebarPane>,
    thread: Entity<ThreadView>,
    composer: Entity<Composer>,
    connect: Entity<ConnectView>,
    _subscription: Subscription,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarPane::new(state.clone(), cx));
        let thread = cx.new(|cx| ThreadView::new(state.clone(), window, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
        let connect = cx.new(|cx| ConnectView::new(state.clone(), window, cx));
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Phase | AppEvent::Selection) {
                cx.notify();
            }
        });
        Workspace { state, sidebar, thread, composer, connect, _subscription: subscription }
    }

    fn render_titlebar(&self, cx: &App) -> impl IntoElement {
        let state = self.state.read(cx);
        let title = state
            .selected_thread
            .as_deref()
            .and_then(|id| state.shell.thread(id))
            .map(|thread| thread.title.clone())
            .unwrap_or_default();
        let status = match &state.phase {
            Phase::Online(ConnectionStatus::Connected) => None,
            Phase::Online(ConnectionStatus::Connecting) => Some("Connecting…".to_owned()),
            Phase::Online(ConnectionStatus::Reconnecting { reason, .. }) => Some(reason.clone()),
            Phase::Online(ConnectionStatus::NeedsPairing { reason }) => Some(reason.clone()),
            Phase::Pairing | Phase::Unpaired { .. } => None,
        };
        let theme = cx.theme();
        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(20.))
            .pr(px(12.))
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().truncate().text_sm().font_weight(gpui::FontWeight::MEDIUM).text_color(theme.foreground).child(title))
            .children(status.map(|status| div().text_xs().text_color(theme.muted_foreground).child(status)))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paired = matches!(self.state.read(cx).phase, Phase::Online(_));
        let theme = cx.theme();
        if !paired {
            return div().size_full().bg(theme.background).child(self.connect.clone()).into_any_element();
        }
        let has_thread = self.state.read(cx).selected_thread.is_some();
        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .w(px(SIDEBAR_WIDTH))
                    .h_full()
                    .flex_none()
                    .bg(theme.sidebar)
                    .border_r_1()
                    .border_color(crate::theme::hsla(crate::theme::palette(cx).sidebar_edge))
                    .child(self.sidebar.clone().cached(gpui::StyleRefinement::default().size_full())),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.render_titlebar(cx))
                    .when(has_thread, |this| {
                        this.child(div().flex_1().min_h_0().child(self.thread.clone()))
                            .child(self.composer.clone())
                    })
                    .when(!has_thread, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Select a thread"),
                        )
                    }),
            )
            .into_any_element()
    }
}
