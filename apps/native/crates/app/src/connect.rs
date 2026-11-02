//! First run: paste a pairing link from any T3 Code server.

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Disableable, v_flex};

use crate::app_state::{AppEvent, AppState, Phase};

pub struct ConnectView {
    state: Entity<AppState>,
    link: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| InputState::new(window, cx).placeholder("https://your-server:3773/pair#token=…"));
        let subscriptions = vec![
            cx.subscribe_in(&link, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.connect(cx);
                }
            }),
            cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::PhaseChanged) {
                    cx.notify();
                }
            }),
        ];
        ConnectView { state, link, _subscriptions: subscriptions }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().to_string();
        self.state.update(cx, |state, cx| state.pair(&link, cx));
    }
}

impl Render for ConnectView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (pairing, error) = match &self.state.read(cx).phase {
            Phase::Pairing => (true, None),
            Phase::Unpaired { error } => (false, error.clone()),
            Phase::Online(_) => (false, None),
        };
        let theme = cx.theme();
        div().size_full().flex().items_center().justify_center().child(
            v_flex()
                .w(px(440.))
                .gap_3()
                .child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.foreground).child("Connect to T3 Code"))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("On the machine running T3 Code, open Settings → Sharing & access and create a pairing link. Paste it here."),
                )
                .child(Input::new(&self.link))
                .children(error.map(|error| div().text_sm().text_color(theme.danger).child(error)))
                .child(
                    Button::new("connect")
                        .primary()
                        .label(if pairing { "Connecting…" } else { "Connect" })
                        .disabled(pairing)
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                ),
        )
    }
}
