//! Custom snooze (`apps/web/src/components/CustomSnoozeDialog.tsx`): a date and time, or a
//! duration, validated by `resolve_custom_snooze`. The web's calendar popover is a typed
//! `YYYY-MM-DD` field here.

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Render, SharedString, Styled,
    Subscription, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use t3_core::sidebar::settled::{CustomSnoozeInput, DurationUnit, local_snooze_fields, resolve_custom_snooze};
use t3_core::sidebar::time::HOUR_MS;

use crate::app_state::AppState;
use crate::sidebar::controller::{SystemZone, now_ms};
use crate::theme::{hsla, palette};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Date,
    Duration,
}

pub struct SnoozeDialog {
    state: Entity<AppState>,
    thread_id: Option<String>,
    mode: Mode,
    unit: DurationUnit,
    date: Entity<InputState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    error: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SnoozeDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.date.read(cx).focus_handle(cx)
    }
}

impl SnoozeDialog {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD"));
        let time = cx.new(|cx| InputState::new(window, cx).placeholder("HH:MM"));
        let amount = cx.new(|cx| InputState::new(window, cx).default_value("2"));
        let subscriptions = [&date, &time, &amount]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.error = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    _ => {}
                })
            })
            .collect();
        SnoozeDialog { state, thread_id: None, mode: Mode::Date, unit: DurationUnit::Hours, date, time, amount, error: None, _subscriptions: subscriptions }
    }

    pub fn open(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (date, time) = local_snooze_fields(now_ms() + HOUR_MS, &SystemZone);
        self.date.update(cx, |input, cx| input.set_value(date, window, cx));
        self.time.update(cx, |input, cx| input.set_value(time, window, cx));
        self.amount.update(cx, |input, cx| input.set_value("2", window, cx));
        self.thread_id = Some(thread_id.to_owned());
        self.mode = Mode::Date;
        self.unit = DurationUnit::Hours;
        self.error = None;
        self.date.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.thread_id = None;
        cx.notify();
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        let (date, time, amount) = (self.date.read(cx).value(), self.time.read(cx).value(), self.amount.read(cx).value());
        let input = match self.mode {
            Mode::Date => CustomSnoozeInput::Date { date: date.as_ref(), time: time.as_ref() },
            Mode::Duration => CustomSnoozeInput::Duration { amount: amount.as_ref(), unit: self.unit },
        };
        let Some(until) = resolve_custom_snooze(&input, now_ms(), &SystemZone) else {
            self.error = Some(if self.mode == Mode::Date { "Choose a valid date and time in the future." } else { "Enter a positive duration." });
            return cx.notify();
        };
        self.close(cx);
        self.state.update(cx, |state, cx| state.snooze_thread(&thread_id, until, cx));
    }

    /// A segmented toggle group (design-system §6.7).
    fn segmented<T: Copy + PartialEq + 'static>(&self, id: &'static str, options: &[(T, &'static str)], value: T, pick: fn(&mut Self, T), cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let dark = gpui_component::ActiveTheme::theme(&**cx).is_dark();
        div()
            .id(id)
            .flex()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(8.))
            .bg(hsla(p.input).opacity(0.4))
            .children(options.iter().enumerate().map(|(index, (option, label))| {
                let option = *option;
                let pressed = option == value;
                div()
                    .id(SharedString::from(format!("{id}-{index}")))
                    .flex_1()
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .text_color(hsla(if pressed { p.foreground } else { p.muted_foreground }))
                    .when(pressed, |this| this.bg(if dark { hsla(p.input).opacity(0.72) } else { hsla(p.background) }))
                    .child(*label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        pick(this, option);
                        this.error = None;
                        cx.notify();
                    }))
            }))
    }

    fn field(label: &'static str, input: &Entity<InputState>, cx: &App) -> impl IntoElement {
        let p = palette(cx);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(label))
            .child(Input::new(input))
    }
}

impl Render for SnoozeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.thread_id.is_none() {
            return div().into_any_element();
        }
        let p = palette(cx);
        let body = match self.mode {
            Mode::Date => div().flex().gap(px(12.)).child(Self::field("Date", &self.date, cx)).child(Self::field("Time", &self.time, cx)).into_any_element(),
            Mode::Duration => div()
                .flex()
                .gap(px(12.))
                .items_end()
                .child(Self::field("Snooze for", &self.amount, cx))
                .child(div().flex_1().child(self.segmented(
                    "snooze-unit",
                    &[(DurationUnit::Minutes, "Minutes"), (DurationUnit::Hours, "Hours"), (DurationUnit::Days, "Days")],
                    self.unit,
                    |this, unit| this.unit = unit,
                    cx,
                )))
                .into_any_element(),
        };
        let button = |id: &'static str, label: &'static str, primary: bool| {
            let (fill, text, border) = if primary { (p.primary, p.primary_foreground, p.primary) } else { (p.popover, p.foreground, p.input) };
            div()
                .id(id)
                .h(px(32.))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(border))
                .bg(hsla(fill))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(text))
                .cursor_pointer()
                .hover(|this| this.opacity(0.9))
                .child(label)
        };
        deferred(
            div()
                .id("snooze-dialog-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close(cx)))
                .child(
                    div()
                        .id("snooze-dialog")
                        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                            this.close(cx);
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(384.))
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
                        .child(
                            div()
                                .p(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Custom snooze"))
                                .child(div().text_sm().text_color(hsla(p.muted_foreground)).child("Choose when snoozed threads return to your inbox.")),
                        )
                        .child(
                            div()
                                .px(px(24.))
                                .pb(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(16.))
                                .child(self.segmented("snooze-mode", &[(Mode::Date, "Date and time"), (Mode::Duration, "Duration")], self.mode, |this, mode| this.mode = mode, cx))
                                .child(body)
                                .children(self.error.map(|error| div().text_sm().text_color(hsla(p.destructive)).child(error))),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .px(px(24.))
                                .py(px(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .rounded_b(px(15.))
                                .bg(hsla(p.muted))
                                .child(button("snooze-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                                .child(button("snooze-confirm", "Snooze", true).on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))),
                        ),
                ),
        )
        .with_priority(50)
        .into_any_element()
    }
}
