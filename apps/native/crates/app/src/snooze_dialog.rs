//! Custom snooze (`apps/web/src/components/CustomSnoozeDialog.tsx`): a date and time, or a
//! duration, validated by `resolve_custom_snooze`. Requests retain their destination until picked.

use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use chrono::NaiveDate;
use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, deferred,
    div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_component::calendar::{Calendar, CalendarEvent, CalendarState, Date};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{Sizable, Size};
use t3_core::sidebar::settled::{
    CustomSnoozeInput, DurationUnit, local_snooze_fields, resolve_custom_snooze,
};
use t3_core::sidebar::time::HOUR_MS;
use t3_core::sidebar::time::{MINUTE_MS, Zone, to_local};

use crate::app_state::{AppState, Phase};
use crate::sidebar::controller::{SystemZone, now_ms};
use crate::theme::{hsla, palette};
use t3_client::supervisor::ConnectionStatus;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnoozeOwner {
    environment: String,
    connection: u64,
    generation: u64,
}

impl SnoozeOwner {
    pub(crate) fn capture(state: &AppState) -> Option<Self> {
        if state.phase != Phase::Online(ConnectionStatus::Connected) {
            return None;
        }
        let environment = &state
            .server_config
            .config
            .as_ref()?
            .environment
            .environment_id;
        if environment.is_empty() {
            return None;
        }
        let client = state.client_handle()?;
        Some(Self {
            environment: environment.clone(),
            connection: state.connection_id()?,
            generation: *client.connection_generation().borrow(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct SnoozeRequest {
    ids: Vec<String>,
    owner: Option<SnoozeOwner>,
    bulk: bool,
    previous_focus: Option<FocusHandle>,
}

impl SnoozeRequest {
    pub(crate) fn capture(
        state: &AppState,
        ids: Vec<String>,
        bulk: bool,
        previous_focus: Option<FocusHandle>,
    ) -> Self {
        Self {
            ids,
            owner: SnoozeOwner::capture(state),
            bulk,
            previous_focus,
        }
    }

    fn plan(
        &self,
        current: Option<&SnoozeOwner>,
        loaded: bool,
        can_operate: bool,
        grants: Option<&t3_core::auth::AuthSessionState>,
        eligible: impl Fn(&str) -> bool,
    ) -> Option<Vec<String>> {
        (self.owner.is_some()
            && self.owner.as_ref() == current
            && loaded
            && can_operate
            && grants
                .is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
            && !self.ids.is_empty()
            && self.ids.iter().all(|id| eligible(id)))
        .then(|| self.ids.clone())
    }

    pub(crate) fn pick_preset(
        &self,
        id: &str,
        shown: &[t3_core::sidebar::settled::SnoozePreset],
        state: &mut AppState,
        cx: &mut Context<AppState>,
    ) {
        let now = now_ms();
        let Some(ids) = self.plan_for(state, now) else {
            return;
        };
        let Some(preset) = t3_core::sidebar::settled::pick_snooze_preset(
            shown,
            id,
            now,
            &state.snooze_settings(),
            &SystemZone,
            &crate::sidebar::controller::time_label,
        ) else {
            state.show_toast(
                crate::toast::Toast::new(
                    crate::toast::ToastKind::Warning,
                    "Could not snooze thread",
                )
                .description("That snooze time has passed. Choose another time."),
                cx,
            );
            return;
        };
        if self.bulk {
            state.clear_thread_selection(cx);
        }
        for id in ids {
            state.snooze_thread(&id, preset.snoozed_until, cx);
        }
    }

    pub(crate) fn plan_for(&self, state: &AppState, now: i64) -> Option<Vec<String>> {
        self.plan_for_action(state, now, true)
    }

    pub(crate) fn plan_for_action(
        &self,
        state: &AppState,
        now: i64,
        snoozing: bool,
    ) -> Option<Vec<String>> {
        let current = SnoozeOwner::capture(state);
        let grants = state
            .client_handle()
            .and_then(|client| client.session_state().borrow().clone());
        self.plan(
            current.as_ref(),
            state.sidebar.capabilities.config_loaded && state.server_config.config.is_some(),
            state.sidebar.capabilities.can_operate,
            grants.as_ref(),
            |id| {
                state.thread_shell(id).is_some()
                    && state.sidebar.model.thread(id).is_some_and(|thread| {
                        !snoozing || t3_core::sidebar::settled::can_snooze(thread, now)
                    })
            },
        )
    }
}

fn calendar_disabled(date: NaiveDate, now: i64, zone: &dyn Zone) -> bool {
    let local = to_local(now, zone);
    NaiveDate::from_ymd_opt(local.year as i32, local.month as u32, local.day as u32)
        .is_none_or(|today| date < today)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Date,
    Duration,
}

pub struct SnoozeDialog {
    state: Entity<AppState>,
    pending: Option<SnoozeRequest>,
    mode: Mode,
    unit: DurationUnit,
    date: Entity<InputState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    error: Option<&'static str>,
    calendar: Entity<CalendarState>,
    calendar_open: bool,
    calendar_trigger: FocusHandle,
    mode_focus: [FocusHandle; 2],
    unit_focus: [FocusHandle; 3],
    footer_focus: [FocusHandle; 3],
    clock: Option<Task<()>>,
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
        let calendar = cx.new(|cx| {
            CalendarState::new(window, cx).disabled_matcher(|date: &NaiveDate| {
                calendar_disabled(*date, now_ms(), &SystemZone)
            })
        });
        let mut subscriptions: Vec<_> =
            [&date, &time, &amount]
                .into_iter()
                .map(|input| {
                    cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                        match event {
                            InputEvent::Change => {
                                this.error = None;
                                cx.notify();
                            }
                            InputEvent::PressEnter { .. } => this.submit(window, cx),
                            _ => {}
                        }
                    })
                })
                .collect();
        subscriptions.push(cx.subscribe_in(
            &calendar,
            window,
            |this, _, event: &CalendarEvent, window, cx| {
                let CalendarEvent::Selected(Date::Single(Some(date))) = event else {
                    return;
                };
                this.date.update(cx, |input, cx| {
                    input.set_value(date.to_string(), window, cx)
                });
                this.error = None;
                this.calendar_open = false;
                window.focus(&this.calendar_trigger, cx);
                cx.notify();
            },
        ));
        subscriptions.push(
            cx.subscribe_in(&state, window, |this, state, event, window, cx| {
                if matches!(
                    event,
                    crate::app_state::AppEvent::Phase | crate::app_state::AppEvent::ServerConfig
                ) && this.pending.as_ref().is_some_and(|request| {
                    request.owner.is_none() || request.owner != SnoozeOwner::capture(state.read(cx))
                }) {
                    this.close(window, cx);
                }
            }),
        );
        SnoozeDialog {
            state,
            pending: None,
            mode: Mode::Date,
            unit: DurationUnit::Hours,
            date,
            time,
            amount,
            error: None,
            calendar,
            calendar_open: false,
            calendar_trigger: cx.focus_handle().tab_stop(true),
            mode_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            unit_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            footer_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            clock: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn open(
        &mut self,
        mut request: SnoozeRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.pending.take() {
            request.previous_focus = previous.previous_focus;
        }
        self.clock = None;
        self.calendar_open = false;
        if request.plan_for(self.state.read(cx), now_ms()).is_none() {
            if let Some(focus) = request.previous_focus {
                window.focus(&focus, cx);
            }
            return cx.notify();
        }
        let (date, time) = local_snooze_fields(now_ms() + HOUR_MS, &SystemZone);
        self.date
            .update(cx, |input, cx| input.set_value(date.clone(), window, cx));
        self.time
            .update(cx, |input, cx| input.set_value(time, window, cx));
        self.amount
            .update(cx, |input, cx| input.set_value("2", window, cx));
        self.calendar.update(cx, |calendar, cx| {
            calendar.set_date(
                NaiveDate::parse_from_str(&date, "%Y-%m-%d").expect("local date"),
                window,
                cx,
            )
        });
        self.pending = Some(request);
        self.mode = Mode::Date;
        self.unit = DurationUnit::Hours;
        self.error = None;
        self.date.update(cx, |input, cx| input.focus(window, cx));
        self.clock = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = (MINUTE_MS - now_ms().rem_euclid(MINUTE_MS)) as u64;
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(wait))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self
            .pending
            .take()
            .and_then(|request| request.previous_focus);
        self.calendar_open = false;
        self.clock = None;
        self.error = None;
        if let Some(focus) = previous {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn toggle_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.calendar_open = !self.calendar_open;
        if self.calendar_open {
            if let Ok(date) = NaiveDate::parse_from_str(&self.date.read(cx).value(), "%Y-%m-%d") {
                self.calendar
                    .update(cx, |calendar, cx| calendar.set_date(date, window, cx));
            }
            let focus = self.calendar.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
        } else {
            window.focus(&self.calendar_trigger, cx);
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            if self.calendar_open {
                self.toggle_calendar(window, cx);
            } else {
                self.close(window, cx);
            }
        } else if key == "tab" {
            self.calendar_open = false;
            cx.notify();
            let mut stops = self.mode_focus.to_vec();
            match self.mode {
                Mode::Date => stops.extend([
                    self.date.read(cx).focus_handle(cx),
                    self.calendar_trigger.clone(),
                    self.time.read(cx).focus_handle(cx),
                ]),
                Mode::Duration => {
                    stops.push(self.amount.read(cx).focus_handle(cx));
                    stops.extend(self.unit_focus.iter().cloned());
                }
            }
            stops.extend(self.footer_focus.iter().cloned());
            let index = stops.iter().position(|focus| focus.is_focused(window));
            let next = if event.keystroke.modifiers.shift {
                index.unwrap_or(0).checked_sub(1).unwrap_or(stops.len() - 1)
            } else {
                index.map_or(0, |i| (i + 1) % stops.len())
            };
            window.focus(&stops[next], cx);
        } else if self.calendar_open && self.calendar.read(cx).focus_handle.is_focused(window) {
            let current = self.calendar.read(cx).date().start();
            let offset = match key {
                "left" => -1,
                "right" => 1,
                "up" => -7,
                "down" => 7,
                _ => 0,
            };
            if let Some(date) =
                current.and_then(|date| date.checked_add_signed(chrono::Duration::days(offset)))
            {
                if offset != 0 {
                    self.calendar
                        .update(cx, |calendar, cx| calendar.set_date(date, window, cx));
                } else if matches!(key, "enter" | "space") {
                    self.calendar.update(cx, |calendar, cx| {
                        calendar.activate_date(date, cx);
                    });
                } else {
                    return;
                }
            }
        } else if matches!(key, "enter" | "space") {
            if self.calendar_trigger.is_focused(window) {
                self.toggle_calendar(window, cx);
            } else if let Some(index) = self
                .mode_focus
                .iter()
                .position(|focus| focus.is_focused(window))
            {
                self.mode = if index == 0 {
                    Mode::Date
                } else {
                    Mode::Duration
                };
                self.calendar_open = false;
                self.error = None;
                cx.notify();
            } else if let Some(index) = self
                .unit_focus
                .iter()
                .position(|focus| focus.is_focused(window))
            {
                self.unit = [
                    DurationUnit::Minutes,
                    DurationUnit::Hours,
                    DurationUnit::Days,
                ][index];
                self.error = None;
                cx.notify();
            } else if self.footer_focus[0].is_focused(window) {
                self.close(window, cx);
                crate::settings::open_setting("snooze-options", cx);
            } else if self.footer_focus[1].is_focused(window) {
                self.close(window, cx);
            } else if self.footer_focus[2].is_focused(window) {
                self.submit(window, cx);
            } else {
                return;
            }
        } else {
            return;
        }
        cx.stop_propagation();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_none() {
            return;
        }
        let (date, time, amount) = (
            self.date.read(cx).value(),
            self.time.read(cx).value(),
            self.amount.read(cx).value(),
        );
        let input = match self.mode {
            Mode::Date => CustomSnoozeInput::Date {
                date: date.as_ref(),
                time: time.as_ref(),
            },
            Mode::Duration => CustomSnoozeInput::Duration {
                amount: amount.as_ref(),
                unit: self.unit,
            },
        };
        let Some(until) = resolve_custom_snooze(&input, now_ms(), &SystemZone) else {
            self.error = Some(if self.mode == Mode::Date {
                "Choose a valid date and time in the future."
            } else {
                "Enter a positive duration."
            });
            return cx.notify();
        };
        let Some(request) = self.pending.clone() else {
            return;
        };
        if request.plan_for(self.state.read(cx), now_ms()).is_none() {
            self.error =
                Some("These threads can no longer be snoozed. Close this dialog and try again.");
            return cx.notify();
        }
        self.close(window, cx);
        self.state.update(cx, |state, cx| {
            let Some(thread_ids) = request.plan_for(state, now_ms()) else {
                return;
            };
            if request.bulk {
                state.clear_thread_selection(cx);
            }
            for thread_id in &thread_ids {
                state.snooze_thread(thread_id, until, cx);
            }
        });
    }

    /// A segmented toggle group (design-system §6.7).
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: &[(T, &'static str)],
        value: T,
        pick: fn(&mut Self, T),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    .track_focus(if id == "snooze-mode" {
                        &self.mode_focus[index]
                    } else {
                        &self.unit_focus[index]
                    })
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
                    .text_color(hsla(if pressed {
                        p.foreground
                    } else {
                        p.muted_foreground
                    }))
                    .when(pressed, |this| {
                        this.bg(if dark {
                            hsla(p.input).opacity(0.72)
                        } else {
                            hsla(p.background)
                        })
                    })
                    .child(*label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        pick(this, option);
                        this.calendar_open = false;
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
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(p.foreground))
                    .child(label),
            )
            .child(Input::new(input))
    }
}

impl Render for SnoozeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending.is_none() {
            return div().into_any_element();
        }
        let p = palette(cx);
        let body = match self.mode {
            Mode::Date => div()
                .flex()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .relative()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child("Date"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(4.))
                                .child(div().flex_1().min_w_0().child(Input::new(&self.date)))
                                .child(
                                    Button::new(
                                        "snooze-calendar",
                                        ButtonVariant::Outline,
                                        ButtonSize::Icon,
                                    )
                                    .focus_handle(self.calendar_trigger.clone())
                                    .icon(Icon::CalendarClock)
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.toggle_calendar(window, cx),
                                    )),
                                ),
                        )
                        .when(self.calendar_open, |this| {
                            this.child(
                                deferred(
                                    div()
                                        .id("snooze-calendar-popup")
                                        .absolute()
                                        .top(px(64.))
                                        .left_0()
                                        .occlude()
                                        .bg(hsla(p.popover))
                                        .child(Calendar::new(&self.calendar).with_size(Size::Small))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        }),
                                )
                                .with_priority(130),
                            )
                        }),
                )
                .child(Self::field("Time", &self.time, cx))
                .into_any_element(),
            Mode::Duration => div()
                .flex()
                .gap(px(12.))
                .items_end()
                .child(Self::field("Snooze for", &self.amount, cx))
                .child(div().flex_1().child(self.segmented(
                    "snooze-unit",
                    &[
                        (DurationUnit::Minutes, "Minutes"),
                        (DurationUnit::Hours, "Hours"),
                        (DurationUnit::Days, "Days"),
                    ],
                    self.unit,
                    |this, unit| this.unit = unit,
                    cx,
                )))
                .into_any_element(),
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
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close(window, cx)),
                )
                .child(
                    div()
                        .id("snooze-dialog")
                        .capture_key_down(
                            cx.listener(|this, event, window, cx| this.on_key(event, window, cx)),
                        )
                        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                            if this.calendar_open {
                                this.toggle_calendar(window, cx);
                            } else {
                                this.close(window, cx);
                            }
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(384.))
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(
                            16.,
                            hsla(p.dialog_surface),
                            hsla(p.dialog_border),
                            cx,
                        ))
                        .shadow(vec![BoxShadow {
                            color: gpui_hsla(0., 0., 0., 0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .child(
                            div()
                                .p(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_xl()
                                        .line_height(px(20.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(hsla(p.popover_foreground))
                                        .child("Custom snooze"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child("Choose when snoozed threads return to your inbox."),
                                ),
                        )
                        .child(
                            div()
                                .px(px(24.))
                                .pb(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(16.))
                                .child(self.segmented(
                                    "snooze-mode",
                                    &[(Mode::Date, "Date and time"), (Mode::Duration, "Duration")],
                                    self.mode,
                                    |this, mode| this.mode = mode,
                                    cx,
                                ))
                                .child(body)
                                .children(self.error.map(|error| {
                                    div().text_sm().text_color(hsla(p.destructive)).child(error)
                                })),
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
                                .child(
                                    div().flex_1().child(
                                        Button::new(
                                            "snooze-settings",
                                            ButtonVariant::GhostMuted,
                                            ButtonSize::Sm,
                                        )
                                        .focus_handle(self.footer_focus[0].clone())
                                        .label("Edit snooze options")
                                        .icon(Icon::Settings2)
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.close(window, cx);
                                                crate::settings::open_setting("snooze-options", cx);
                                            }),
                                        ),
                                    ),
                                )
                                .child(
                                    Button::new(
                                        "snooze-cancel",
                                        ButtonVariant::Outline,
                                        ButtonSize::Default,
                                    )
                                    .focus_handle(self.footer_focus[1].clone())
                                    .label("Cancel")
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.close(window, cx)),
                                    ),
                                )
                                .child(
                                    Button::new(
                                        "snooze-confirm",
                                        ButtonVariant::Default,
                                        ButtonSize::Default,
                                    )
                                    .focus_handle(self.footer_focus[2].clone())
                                    .label("Snooze")
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                    ),
                                ),
                        ),
                ),
        )
        .with_priority(50)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_snooze_targets_require_the_original_owner_and_current_eligibility() {
        let owner = SnoozeOwner {
            environment: "original".into(),
            connection: 1,
            generation: 1,
        };
        let request = SnoozeRequest {
            ids: vec!["same-id".into(), "second".into()],
            owner: Some(owner.clone()),
            bulk: true,
            previous_focus: None,
        };
        let changed_environment = SnoozeOwner {
            environment: "other".into(),
            ..owner.clone()
        };
        let switched_back = SnoozeOwner {
            connection: 3,
            ..owner.clone()
        };
        let reconnected = SnoozeOwner {
            generation: 2,
            ..owner.clone()
        };
        let granted: t3_core::auth::AuthSessionState = serde_json::from_value(
            serde_json::json!({"authenticated":true,"permissions":["orchestration:operate"]}),
        )
        .unwrap();
        let denied = t3_core::auth::AuthSessionState::default();
        let actual: Vec<_> = [
            (Some(&owner), true, true),
            (Some(&changed_environment), true, true),
            (Some(&switched_back), true, true),
            (Some(&reconnected), true, true),
            (None, true, true),
            (Some(&owner), false, true),
            (Some(&owner), true, false),
        ]
        .into_iter()
        .map(|(current, allowed, second_eligible)| {
            request
                .plan(current, true, allowed, Some(&granted), |id| {
                    id != "second" || second_eligible
                })
                .is_some()
        })
        .collect();
        assert_eq!(actual, [true, false, false, false, false, false, false]);
        for (loaded, can_operate, session) in [
            (false, true, Some(&granted)),
            (true, false, Some(&granted)),
            (true, true, None),
            (true, true, Some(&denied)),
        ] {
            assert!(
                request
                    .plan(Some(&owner), loaded, can_operate, session, |_| true)
                    .is_none()
            );
        }
        assert!(
            SnoozeRequest {
                owner: None,
                ..request.clone()
            }
            .plan(Some(&owner), true, true, Some(&granted), |_| true)
            .is_none()
        );
        assert!(
            SnoozeRequest {
                ids: Vec::new(),
                ..request
            }
            .plan(Some(&owner), true, true, Some(&granted), |_| true)
            .is_none()
        );
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;
    use t3_core::sidebar::time::{FixedZone, parse_iso};

    #[test]
    fn calendar_lower_bound_follows_local_midnight() {
        let yesterday = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let today = yesterday.succ_opt().unwrap();
        let zone = FixedZone(-7 * 60 * 60);
        let before = parse_iso("2026-11-05T06:59:59Z").unwrap();
        let after = before + 1000;
        assert!(!calendar_disabled(yesterday, before, &zone));
        assert!(!calendar_disabled(today, before, &zone));
        assert!(calendar_disabled(yesterday, after, &zone));
        assert!(!calendar_disabled(today, after, &zone));
    }
}
