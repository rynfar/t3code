//! Composer: multiline input (IME-capable gpui-component textarea), Enter sends, Shift+Enter
//! breaks a line. Sends queue behind a running turn, matching the web's default follow-up mode.

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};

use crate::app_state::{AppEvent, AppState};
use crate::theme::{hsla, palette};
use crate::thread_view::CONTENT_MAX_WIDTH;

pub struct Composer {
    state: Entity<AppState>,
    input: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 12)
                .placeholder("Ask anything…")
                .submit_on_enter(true)
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    this.send(window, cx);
                }
            }),
            cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::Thread | AppEvent::Selection) {
                    cx.notify();
                }
            }),
        ];
        Composer { state, input, _subscriptions: subscriptions }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.state.update(cx, |state, cx| state.send_message(text, cx));
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.state.read(cx).is_selected_thread_running();
        let error = self.state.read(cx).last_error.clone();
        let theme = cx.theme();
        let p = palette(cx);
        let shell = if theme.is_dark() { p.surface_raised } else { p.card };
        let round = |id: &'static str, fill: u32, icon: IconName| {
            div()
                .id(id)
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .bg(hsla(fill))
                .child(Icon::new(icon).small().text_color(hsla(p.message_action_foreground)))
        };
        div().w_full().flex().justify_center().px(px(48.)).pb_4().pt_2().child(
            v_flex()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .gap_1()
                .children(error.map(|error| div().text_xs().text_color(hsla(p.error_foreground)).child(error)))
                .child(
                    v_flex()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(shell))
                        .child(div().px_4().pt_4().pb_2().child(Textarea::new(&self.input).appearance(false)))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .px_2()
                                .pb_2()
                                .when(running, |this| {
                                    this.child(round("stop", p.destructive, IconName::Pause).on_click(cx.listener(
                                        |this, _, _, cx| this.state.update(cx, |state, cx| state.stop_selected_thread(cx)),
                                    )))
                                })
                                .child(
                                    round("send", p.message_action, IconName::ArrowUp)
                                        .hover(|this| this.bg(hsla(p.message_action_hover)))
                                        .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                                ),
                        ),
                ),
        )
    }
}
