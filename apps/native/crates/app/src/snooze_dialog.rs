//! Custom snooze (`apps/web/src/components/CustomSnoozeDialog.tsx`): a date and time, or a
//! duration, validated by `resolve_custom_snooze`. Requests retain their destination until picked.

use crate::icons::Icon;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use chrono::{Datelike, NaiveDate};
use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, deferred,
    div, hsla as gpui_hsla, point, prelude::*, px,
};
use gpui_base::CalendarView;
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
    scoped: Vec<crate::context_menu::ThreadMenuOwner>,
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
            scoped: Vec::new(),
            owner: SnoozeOwner::capture(state),
            bulk,
            previous_focus,
        }
    }

    pub(crate) fn capture_scoped(state: &AppState, keys: Vec<t3_core::sidebar::tag_filter::ThreadKey>, bulk: bool, previous_focus: Option<FocusHandle>) -> Self {
        let owners = keys.iter().map(|key| crate::context_menu::ThreadMenuOwner::capture_scoped(state, key)).collect::<Option<Vec<_>>>().unwrap_or_default();
        Self { ids: keys.iter().map(|key| key.thread_id.clone()).collect(), scoped: owners, owner: None, bulk, previous_focus }
    }

    fn scoped_plan(&self, state: &AppState, now: i64, snoozing: bool) -> bool {
        !self.scoped.is_empty() && self.scoped.len() == self.ids.len() && self.scoped.iter().all(|owner| {
            owner.current(state) && owner.can_operate_owner(state) && owner.key().and_then(|key| state.scoped_thread(&key)).is_some_and(|row| !snoozing || t3_core::sidebar::settled::can_snooze(&row.thread, now))
        })
    }

    fn dispatch(&self, state: &mut AppState, until: i64, cx: &mut Context<AppState>) {
        if !self.scoped.is_empty() {
            if !self.scoped_plan(state, now_ms(), true) { return; }
            state.run_scoped_drop_steps(self.scoped.iter().map(|owner| (owner.clone(), t3_core::thread_actions::snooze(&crate::sidebar::controller::uuid(), &owner.thread, until))).collect(), cx);
        } else {
            for id in &self.ids { state.snooze_thread(id, until, cx); }
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
        let settings = self.scoped.first().and_then(|owner| owner.destination.as_ref()).and_then(|owner| state.scoped_environment(&owner.saved_id)).and_then(|entry| entry.config.config.as_ref()).map(|config| t3_core::settings::snooze::menu_settings(&config.settings.threads.snooze_presets)).unwrap_or_else(|| state.snooze_settings());
        let Some(preset) = t3_core::sidebar::settled::pick_snooze_preset(
            shown,
            id,
            now,
            &settings,
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
        let _ = ids;
        self.dispatch(state, preset.snoozed_until, cx);
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
        if !self.scoped.is_empty() { return self.scoped_plan(state, now, snoozing).then(|| self.ids.clone()); }
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

fn refresh_calendar(
    calendar: &mut CalendarState,
    window: &mut Window,
    cx: &mut Context<CalendarState>,
) {
    if calendar.today() == chrono::Local::now().date_naive() {
        return;
    }
    // The pinned state has no today setter; retain the entity, focus and full navigation range.
    let page = calendar.years_on_page().first().copied();
    while calendar.prev_year_page() {}
    let first = calendar
        .years_on_page()
        .first()
        .copied()
        .unwrap_or(calendar.current_year());
    while calendar.next_year_page() {}
    let last = calendar.years_on_page().last().copied().unwrap_or(first);
    let mut fresh = CalendarState::new(window, cx).year_range((first, last + 1));
    fresh.focus_handle = calendar.focus_handle.clone();
    // A selected day can have become disabled since midnight, but must remain selected.
    fresh.apply_date(calendar.date());
    fresh.select_year(calendar.current_year());
    fresh.select_month(calendar.current_month());
    fresh.set_view(calendar.view());
    if let Some(page) = page {
        while fresh
            .years_on_page()
            .first()
            .is_some_and(|first| *first > page)
            && fresh.prev_year_page()
        {}
        while fresh
            .years_on_page()
            .first()
            .is_some_and(|first| *first < page)
            && fresh.next_year_page()
        {}
    }
    fresh.set_number_of_months(calendar.number_of_months(), window, cx);
    fresh.set_disabled_matcher(
        |date: &NaiveDate| calendar_disabled(*date, now_ms(), &SystemZone),
        window,
        cx,
    );
    *calendar = fresh;
    cx.notify();
}

fn reset_calendar(
    calendar: &mut CalendarState,
    date: NaiveDate,
    window: &mut Window,
    cx: &mut Context<CalendarState>,
) {
    calendar.set_view(CalendarView::Day);
    calendar.set_date(date, window, cx);
    let year = calendar.current_year();
    while calendar
        .years_on_page()
        .last()
        .is_some_and(|last| *last < year)
        && calendar.next_year_page()
    {}
    while calendar
        .years_on_page()
        .first()
        .is_some_and(|first| *first > year)
        && calendar.prev_year_page()
    {}
}

fn calendar_day(calendar: &CalendarState) -> Option<NaiveDate> {
    // Header navigation changes the displayed month without changing the selected date.
    let day = calendar.date().start().map_or(1, |date| date.day());
    (1..=day).rev().find_map(|day| {
        NaiveDate::from_ymd_opt(
            calendar.current_year(),
            u32::from(calendar.current_month()),
            day,
        )
    })
}

fn calendar_key(
    calendar: &mut CalendarState,
    key: &str,
    window: &mut Window,
    cx: &mut Context<CalendarState>,
) -> bool {
    let view = calendar.view();
    // The pinned Small facade uses 232px width, p-3, and 30%/16% items with m-1.
    let content = (230. - f32::from(window.rem_size()) * 1.5).max(1.);
    let fraction = if view == CalendarView::Month {
        0.3
    } else {
        0.16
    };
    let columns = (content / (content * fraction + f32::from(window.rem_size()) * 0.5))
        .floor()
        .max(1.) as i64;
    let row = if view == CalendarView::Day {
        7
    } else {
        columns
    };
    let offset = match key {
        "left" => -1,
        "right" => 1,
        "up" => -row,
        "down" => row,
        _ => 0,
    };
    if offset != 0 {
        match view {
            CalendarView::Day => {
                if let Some(date) = calendar_day(calendar)
                    .and_then(|date| date.checked_add_signed(chrono::Duration::days(offset)))
                {
                    calendar.set_date(date, window, cx);
                }
            }
            CalendarView::Month => {
                let month = i64::from(calendar.current_month()) + offset;
                if (1..=12).contains(&month) {
                    calendar.select_month(month as u8);
                    calendar.set_view(CalendarView::Month);
                }
            }
            CalendarView::Year => {
                let years = calendar.years_on_page();
                let mut index = years
                    .iter()
                    .position(|year| *year == calendar.current_year())
                    .unwrap_or(0) as i64
                    + offset;
                let count = years.len() as i64;
                if index < 0 && calendar.prev_year_page() {
                    index += calendar.years_on_page().len() as i64;
                } else if index >= count && calendar.next_year_page() {
                    index -= count;
                }
                if let Some(year) = usize::try_from(index)
                    .ok()
                    .and_then(|index| calendar.years_on_page().get(index))
                    .copied()
                {
                    calendar.select_year(year);
                    calendar.set_view(CalendarView::Year);
                }
            }
        }
    } else if matches!(key, "enter" | "space") {
        match view {
            CalendarView::Day => {
                if let Some(date) = calendar_day(calendar) {
                    calendar.activate_date(date, cx);
                }
            }
            CalendarView::Month => calendar.select_month(calendar.current_month()),
            CalendarView::Year => {
                let years = calendar.years_on_page();
                if let Some(year) = years
                    .iter()
                    .find(|year| **year == calendar.current_year())
                    .or_else(|| years.first())
                    .copied()
                {
                    calendar.select_year(year);
                }
            }
        }
    } else {
        return false;
    }
    cx.notify();
    true
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
                    if request.scoped.is_empty() { request.owner.is_none() || request.owner != SnoozeOwner::capture(state.read(cx)) }
                    else { !request.scoped_plan(state.read(cx), now_ms(), true) }
                }) {
                    let previous = this.pending.as_ref().and_then(|request| request.previous_focus.clone());
                    this.close(window, cx);
                    // Other owner-change subscribers may focus the composer after this one.
                    if let Some(focus) = previous {
                        cx.defer_in(window, move |this, window, cx| {
                            if this.pending.is_none() { window.focus(&focus, cx); }
                        });
                    }
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
            refresh_calendar(calendar, window, cx);
            reset_calendar(
                calendar,
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
        self.clock = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let wait = (MINUTE_MS - now_ms().rem_euclid(MINUTE_MS)) as u64;
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(wait))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        this.calendar
                            .update(cx, |calendar, cx| refresh_calendar(calendar, window, cx));
                        cx.notify();
                    })
                    .is_err()
                {
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
                self.calendar.update(cx, |calendar, cx| {
                    reset_calendar(calendar, date, window, cx)
                });
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
            if !self
                .calendar
                .update(cx, |calendar, cx| calendar_key(calendar, key, window, cx))
            {
                return;
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
            let _ = thread_ids;
            request.dispatch(state, until, cx);
        });
    }

    /// A segmented toggle group (design-system §6.7).
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: &[(T, &'static str)],
        value: T,
        pick: fn(&mut Self, T),
        window: &Window,
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
                let focus = if id == "snooze-mode" {
                    &self.mode_focus[index]
                } else {
                    &self.unit_focus[index]
                };
                let keyboard_focus = focus.is_focused(window) && window.last_input_was_keyboard();
                div()
                    .id(SharedString::from(format!("{id}-{index}")))
                    .track_focus(focus)
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
                    .when(keyboard_focus, |this| {
                        this.shadow(vec![
                            BoxShadow {
                                color: hsla(p.ring),
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(0.),
                                spread_radius: px(3.),
                                inset: true,
                            },
                            BoxShadow {
                                color: hsla(p.background),
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(0.),
                                spread_radius: px(1.),
                                inset: true,
                            },
                        ])
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    window,
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
                                    window,
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
            scoped: Vec::new(),
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

    #[gpui::test]
    fn calendar_keyboard_tracks_visible_views_and_resets_navigation(cx: &mut gpui::TestAppContext) {
        use gpui_base::CalendarView;
        use std::{cell::RefCell, rc::Rc};
        // Optional process-local clock interposition exercises midnight without changing OS time.
        let clock_file = std::env::var_os("N265_CLOCK_FILE");
        let set_clock = |iso: &str| {
            if let Some(path) = &clock_file {
                std::fs::write(path, (parse_iso(iso).unwrap() / 1000).to_string()).unwrap();
            }
        };
        set_clock("2026-11-05T06:59:59Z");
        let (calendar, cx) = cx.add_window_view(CalendarState::new);
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&calendar, move |_, event: &CalendarEvent, _| {
                let CalendarEvent::Selected(date) = event;
                observed.borrow_mut().push(*date);
            })
        });
        let date = NaiveDate::from_ymd_opt(2030, 1, 31).unwrap();
        let outcomes = cx.update(|window, cx| {
            calendar.update(cx, |calendar, cx| {
                window.set_rem_size(px(16.));
                calendar.set_year_range((2000, 2100), cx);
                let mut outcomes = Vec::new();
                for view in [CalendarView::Month, CalendarView::Year] {
                    calendar.set_date(date, window, cx);
                    calendar.set_view(view);
                    calendar_key(calendar, "enter", window, cx);
                    outcomes.push((
                        "caption Enter opens days",
                        calendar.view() == CalendarView::Day
                            && calendar.date().start() == Some(date),
                    ));
                    calendar.set_view(view);
                    calendar.next_year_page();
                    reset_calendar(calendar, date, window, cx);
                    outcomes.push((
                        "fresh request resets view and year page",
                        calendar.view() == CalendarView::Day
                            && calendar.years_on_page().contains(&2030),
                    ));
                }
                calendar.set_view(CalendarView::Day);
                calendar.set_date(date, window, cx);
                calendar.next_month();
                calendar_key(calendar, "right", window, cx);
                outcomes.push((
                    "arrow follows displayed month and clamps day",
                    calendar.date().start() == NaiveDate::from_ymd_opt(2030, 3, 1),
                ));
                calendar.set_date(date, window, cx);
                calendar.next_month();
                calendar_key(calendar, "enter", window, cx);
                outcomes.push((
                    "day Enter activates displayed month",
                    calendar.date().start() == NaiveDate::from_ymd_opt(2030, 2, 28),
                ));
                calendar.set_date(date, window, cx);
                calendar.select_month(5);
                calendar.set_view(CalendarView::Month);
                calendar_key(calendar, "down", window, cx);
                outcomes.push((
                    "month arrow follows two-column grid",
                    calendar.view() == CalendarView::Month
                        && calendar.current_month() == 7
                        && calendar.date().start() == Some(date),
                ));
                calendar_key(calendar, "space", window, cx);
                outcomes.push((
                    "month Space opens its day grid",
                    calendar.view() == CalendarView::Day
                        && calendar.current_month() == 7
                        && calendar.date().start() == Some(date),
                ));
                calendar.set_date(date, window, cx);
                calendar.set_year_range((2000, 2100), cx);
                calendar.next_year_page();
                let first = calendar.years_on_page()[0];
                calendar.set_view(CalendarView::Year);
                calendar_key(calendar, "down", window, cx);
                outcomes.push((
                    "year arrow follows visible page",
                    calendar.view() == CalendarView::Year
                        && calendar.current_year() == first + 5
                        && calendar.date().start() == Some(date),
                ));
                calendar_key(calendar, "enter", window, cx);
                outcomes.push((
                    "year Enter opens its day grid",
                    calendar.view() == CalendarView::Day
                        && calendar.current_year() == first + 5
                        && calendar.date().start() == Some(date),
                ));
                calendar.set_date(NaiveDate::from_ymd_opt(2032, 1, 31).unwrap(), window, cx);
                calendar.next_month();
                calendar_key(calendar, "enter", window, cx);
                outcomes.push((
                    "displayed leap month keeps valid day",
                    calendar.date().start() == NaiveDate::from_ymd_opt(2032, 2, 29),
                ));
                for (before, after) in [
                    ("2026-11-05T06:59:59Z", "2026-11-05T07:00:00Z"),
                    ("2027-01-01T06:59:59Z", "2027-01-01T07:00:00Z"),
                    ("2032-03-01T06:59:59Z", "2032-03-01T07:00:00Z"),
                    ("2026-03-08T08:59:59Z", "2026-03-08T09:00:00Z"),
                    ("2026-11-01T07:59:59Z", "2026-11-01T08:00:00Z"),
                ] {
                    set_clock(before);
                    refresh_calendar(calendar, window, cx);
                    for view in [CalendarView::Day, CalendarView::Month, CalendarView::Year] {
                        calendar.set_disabled_matcher_shared(None);
                        calendar.set_date(date, window, cx);
                        calendar.set_disabled_matcher(
                            |date: &NaiveDate| calendar_disabled(*date, now_ms(), &SystemZone),
                            window,
                            cx,
                        );
                        calendar.next_month();
                        calendar.next_year_page();
                        calendar.set_view(view);
                        let selected = calendar.date();
                        let year_month = (calendar.current_year(), calendar.current_month());
                        let page = calendar.years_on_page().to_vec();
                        let focus = calendar.focus_handle.clone();
                        window.focus(&focus, cx);
                        set_clock(after);
                        refresh_calendar(calendar, window, cx);
                        outcomes.push((
                            "minute refresh follows local day and retains calendar presentation",
                            calendar.today() == chrono::Local::now().date_naive()
                                && calendar.date() == selected
                                && (calendar.current_year(), calendar.current_month())
                                    == year_month
                                && calendar.view() == view
                                && calendar.years_on_page() == page
                                && calendar.focus_handle == focus
                                && focus.is_focused(window),
                        ));
                        let today = chrono::Local::now().date_naive();
                        if clock_file.is_some() {
                            assert_eq!(calendar.today(), today, "{before} -> {after}");
                            assert!(
                                calendar
                                    .disabled_matcher_ref()
                                    .unwrap()
                                    .matched(&today.pred_opt().unwrap())
                            );
                            assert!(!calendar.disabled_matcher_ref().unwrap().matched(&today));
                        }
                        set_clock(before);
                        refresh_calendar(calendar, window, cx);
                    }
                }
                let lower = NaiveDate::from_ymd_opt(2030, 3, 1).unwrap();
                calendar.set_disabled_matcher_shared(None);
                calendar.set_date(lower, window, cx);
                calendar.set_disabled_matcher(move |date: &NaiveDate| *date < lower, window, cx);
                calendar.prev_month();
                calendar_key(calendar, "enter", window, cx);
                outcomes.push((
                    "pinned disabled matcher blocks activation",
                    calendar.date().start() == Some(lower),
                ));
                outcomes
            })
        });
        assert!(outcomes.iter().all(|(_, passed)| *passed), "{outcomes:?}");
        assert_eq!(
            *events.borrow(),
            [
                Date::Single(NaiveDate::from_ymd_opt(2030, 2, 28)),
                Date::Single(NaiveDate::from_ymd_opt(2032, 2, 29))
            ]
        );
    }

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

#[cfg(all(test, feature = "snapshot"))]
mod lifecycle {
    use super::*;
    use gpui::{AppContext, HeadlessAppContext, Keystroke, PlatformInput, size};
    use std::sync::Arc;
    use crate::context_menu::project_filter_tests::{install_state, session};

    struct Flow {
        state: Entity<AppState>,
        rows: Entity<crate::sidebar::SidebarPane>,
        menu: Entity<crate::context_menu::ContextMenu>,
        dialog: Entity<SnoozeDialog>,
        outside: FocusHandle,
    }

    impl Render for Flow {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().relative().bg(hsla(palette(cx).background))
                .child(div().w(px(400.)).h_full().child(self.rows.update(cx, |pane, cx| pane.render_snooze_rows(window, cx))))
                .child(div().track_focus(&self.outside).absolute().right(px(20.)).top(px(20.)).child("Synthetic snooze lifecycle"))
                .child(self.menu.clone()).child(self.dialog.clone())
                .children(crate::settings::overlay(window, cx))
                .children(crate::toast::stack(&self.state, window, cx))
        }
    }

    fn accommodate(dialog: &Entity<SnoozeDialog>, window: &mut Window, cx: &mut App) {
            // Focus assertions run before this test-only NSView raster accommodation.
            dialog.update(cx, |d, cx| {
                if d.pending.is_some() && [&d.date, &d.time, &d.amount].iter().any(|input| input.read(cx).focus_handle(cx).is_focused(window)) {
                    window.focus(&d.footer_focus[1], cx);
                }
            });
    }

    fn key(dialog: &Entity<SnoozeDialog>, window: &mut Window, cx: &mut App, key: &str) {
        let input_focused = dialog.read(cx).pending.is_some() && [&dialog.read(cx).date, &dialog.read(cx).time, &dialog.read(cx).amount].iter().any(|input| input.read(cx).focus_handle(cx).is_focused(window));
        let keystroke = Keystroke::parse(key).unwrap();
        // GPUI's raw key dispatcher draws dirty inputs first; use their action/capture boundaries here.
        if input_focused {
            if key == "enter" {
                window.dispatch_action(Box::new(gpui_component::input::Enter { secondary:false, shift:false }), cx);
            } else {
                dialog.update(cx, |d, cx| d.on_key(&KeyDownEvent { keystroke, is_held:false, prefer_character_input:false }, window, cx));
            }
        } else { window.dispatch_keystroke(keystroke, cx); }
    }

    fn click(window: &mut Window, cx: &mut App, x: f32, y: f32) {
        let position = point(px(x), px(y));
        window.simulate_mouse_move(position, cx);
        window.dispatch_event(PlatformInput::MouseDown(gpui::MouseDownEvent { button: MouseButton::Left, position, modifiers: Default::default(), click_count: 1, first_mouse: false }), cx);
        window.dispatch_event(PlatformInput::MouseUp(gpui::MouseUpEvent { button: MouseButton::Left, position, modifiers: Default::default(), click_count: 1 }), cx);
    }

    #[test]
    fn production_snooze_lifecycle() {
        let Some(out) = std::env::var_os("T3_SNOOZE_CAPTURE") else { return };
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cx = HeadlessAppContext::with_platform(gpui_platform::current_platform(true).text_system(), Arc::new(crate::icons::Assets), gpui_platform::current_headless_renderer);
        let (state, mut fixture) = cx.update(|cx| {
            let (state, fixture) = install_state(cx, runtime.handle().clone());
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            (state, fixture)
        });
        assert!(state.read_with(&cx, |state, _| state.selected_thread.is_none()));
        let mut handles = None;
        let mut flow = None;
        let window = cx.open_window(size(px(1000.), px(740.)), |window, cx| {
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let dialog = cx.new(|cx| SnoozeDialog::new(state.clone(), window, cx));
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let pane = cx.new(|cx| crate::sidebar::SidebarPane::new(state.clone(), composer, menu.clone(), confirm, dialog.clone(), window, cx));
            let rows = pane;
            let outside = cx.focus_handle().tab_stop(true);
            window.focus(&outside, cx);
            handles = Some((menu.clone(), dialog.clone(), outside.clone()));
            let view = cx.new(|_| Flow { state:state.clone(), rows, menu, dialog, outside });
            flow = Some(view.clone());
            view
        }).unwrap();
        let flow = flow.unwrap();
        let (menu, dialog, outside) = handles.unwrap();
        let mut records = Vec::new();
        let draw = |cx: &mut HeadlessAppContext| {
            cx.update_window(window.into(), |_, window, cx| accommodate(&dialog, window, cx)).unwrap();
            flow.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| { window.refresh(); window.draw(cx).clear(); }).unwrap();
        };
        let frame = |cx: &mut HeadlessAppContext, name: &str| {
            draw(cx);
            cx.capture_screenshot(window.into()).unwrap().save(out.join(format!("{name}.png"))).unwrap();
        };
        let edit = |cx: &mut HeadlessAppContext, f: &mut dyn FnMut(&mut Window, &mut App)| {
            let was_pending = dialog.read_with(cx, |d, _| d.pending.is_some());
            let dialog = dialog.clone();
            cx.update_window(window.into(), |_, win, cx| {
                f(win, cx);
                cx.defer(move |cx| {
                    cx.update_window(window.into(), |_, win, cx| {
                        if !was_pending && dialog.read(cx).pending.is_some() {
                            assert!(dialog.read(cx).date.read(cx).focus_handle(cx).is_focused(win));
                        }
                        accommodate(&dialog, win, cx);
                    }).unwrap();
                });
            }).unwrap();
            cx.run_until_parked();
        };
        frame(&mut cx, "00-rows");
        edit(&mut cx, &mut |window, cx| window.simulate_mouse_move(point(px(320.), px(16.)), cx));
        frame(&mut cx, "01-inline-hover");
        fixture.grants.send(Some(session(false))).unwrap();
        frame(&mut cx, "02-inline-revoked");
        edit(&mut cx, &mut |window, cx| click(window, cx, 306., 16.));
        assert!(!menu.read_with(&cx, |m, _| m.is_open()));
        assert_eq!(state.read_with(&cx, |s, _| s.selected_thread.clone()).as_deref(), Some("menu-thread"));
        state.update(&mut cx, |s, cx| s.close_selected_thread(cx));
        let denied_commands = fixture.take_commands();
        records.push(serde_json::json!({"inline":"denied click reaches row, not clock menu", "commands":denied_commands}));
        fixture.grants.send(Some(session(true))).unwrap();
        for denied in ["unsupported", "ineligible"] {
            let caps = state.read_with(&cx, |s, _| s.sidebar.capabilities.clone());
            let model = state.read_with(&cx, |s, _| s.sidebar.model.clone());
            state.update(&mut cx, |s, _| {
                if denied == "unsupported" { s.sidebar.capabilities.config_loaded = false; }
                else { Arc::make_mut(&mut s.sidebar.model).threads.iter_mut().find(|t| t.id == "menu-thread").unwrap().has_pending_approvals = true; }
            });
            draw(&mut cx);
            edit(&mut cx, &mut |window, cx| click(window, cx, 306., 16.));
            assert!(!menu.read_with(&cx, |m, _| m.is_open()));
            assert_eq!(state.read_with(&cx, |s, _| s.selected_thread.clone()).as_deref(), Some("menu-thread"));
            state.update(&mut cx, |s, cx| { s.close_selected_thread(cx); s.sidebar.capabilities = caps.clone(); s.sidebar.model = model.clone(); });
            records.push(serde_json::json!({"inlineDenied":denied,"commands":fixture.take_commands(),"hit":"row, not snooze menu"}));
        }
        frame(&mut cx, "03-inline-restored");
        edit(&mut cx, &mut |window, cx| click(window, cx, 306., 16.));
        assert!(menu.read_with(&cx, |menu, _| menu.is_open()), "actual hover clock click must open inline menu");
        frame(&mut cx, "03-inline-menu");
        // Up from no selection selects the final Custom item.
        edit(&mut cx, &mut |window, cx| {
            key(&dialog, window, cx, "up"); key(&dialog, window, cx, "enter");
        });
        assert!(dialog.read_with(&cx, |dialog, _| dialog.pending.is_some()));
        frame(&mut cx, "04-inline-custom");
        edit(&mut cx, &mut |window, cx| {
            dialog.update(cx, |dialog, cx| { dialog.date.update(cx, |input, cx| { input.set_value("invalid", window, cx); input.focus(window, cx); assert!(input.focus_handle(cx).is_focused(window)); }); });
            key(&dialog, window, cx, "enter");
        });
        assert_eq!(dialog.read_with(&cx, |d, _| d.error), Some("Choose a valid date and time in the future."));
        frame(&mut cx, "05-invalid-date");
        edit(&mut cx, &mut |window, cx| {
            dialog.update(cx, |d, cx| window.focus(&d.mode_focus[1], cx)); key(&dialog, window, cx, "space");
            assert!(dialog.read(cx).error.is_none());
        });
        draw(&mut cx);
        edit(&mut cx, &mut |window, cx| {
            dialog.update(cx, |d, cx| d.amount.update(cx, |input, cx| {
                input.set_value("0", window, cx); input.focus(window, cx);
                assert!(input.focus_handle(cx).is_focused(window));
            }));
            key(&dialog, window, cx, "enter");
        });
        assert_eq!(dialog.read_with(&cx, |d, _| d.error), Some("Enter a positive duration."));
        frame(&mut cx, "06-invalid-duration");
        edit(&mut cx, &mut |window, cx| {
            dialog.update(cx, |d, cx| d.amount.update(cx, |input, cx| {
                input.focus(window, cx);
                assert!(input.focus_handle(cx).is_focused(window));
                gpui::EntityInputHandler::replace_text_in_range(input, Some(0..1), "3", window, cx);
            }));
        });
        assert!(dialog.read_with(&cx, |d, _| d.error.is_none()));
        // The production tab handler wraps through the current mode and footer focus handles.
        edit(&mut cx, &mut |window, cx| {
            dialog.update(cx, |d, cx| window.focus(&d.mode_focus[0], cx));
            let stops = dialog.read(cx).mode_focus.iter().cloned().chain([dialog.read(cx).amount.read(cx).focus_handle(cx)]).chain(dialog.read(cx).unit_focus.iter().cloned()).chain(dialog.read(cx).footer_focus.iter().cloned()).collect::<Vec<_>>();
            for expected in stops.iter().cycle().skip(1).take(stops.len()) {
                key(&dialog, window, cx, "tab"); assert!(expected.is_focused(window));
            }
            dialog.update(cx, |d, _| assert!(d.mode_focus[0].is_focused(window)));
            key(&dialog, window, cx, "shift-tab");
            dialog.update(cx, |d, _| assert!(d.footer_focus[2].is_focused(window)));
            let previous = dialog.read(cx).pending.as_ref().unwrap().previous_focus.clone();
            key(&dialog, window, cx, "escape");
            assert_eq!(window.focused(cx), previous);
        });
        assert!(dialog.read_with(&cx, |d, _| d.pending.is_none() && d.clock.is_none() && !d.calendar_open));
        let invalid_commands = fixture.take_commands();
        assert!(!invalid_commands.contains(&"orchestration.dispatchCommand"));
        records.push(serde_json::json!({"invalidEnter":"date and duration", "commands":invalid_commands}));
        // Actual AppState capture/plan/guarded-submit reads watched grants and generation.
        for mode in ["revoked", "unknown-grants", "unauthenticated", "capability", "config-loading", "config-missing", "disconnected", "generation", "environment", "connection", "missing-shell", "missing-model", "approval", "valid"] {
            let shell = state.read_with(&cx, |s, _| s.shell.clone());
            let config = state.read_with(&cx, |s, _| s.server_config.clone());
            let caps = state.read_with(&cx, |s, _| s.sidebar.capabilities.clone());
            let mut replacement_fixture = None;
            edit(&mut cx, &mut |window, cx| {
                state.update(cx, |s, _| { s.sidebar.selection.toggle("menu-thread"); s.sidebar.selection.toggle("sibling"); });
                let request = SnoozeRequest::capture(state.read(cx), vec!["menu-thread".into(), "sibling".into()], true, Some(outside.clone()));
                assert_eq!(request.plan_for(state.read(cx), now_ms()).unwrap(), ["menu-thread", "sibling"]);
                dialog.update(cx, |d, cx| d.open(request.clone(), window, cx));
                state.update(cx, |s, cx| match mode {
                    "revoked" => { fixture.grants.send(Some(session(false))).unwrap(); },
                    "unknown-grants" => { fixture.grants.send(None).unwrap(); },
                    "unauthenticated" => { let mut grant = session(true); grant.authenticated = false; fixture.grants.send(Some(grant)).unwrap(); },
                    "capability" => s.sidebar.capabilities.can_operate = false,
                    "config-loading" => s.sidebar.capabilities.config_loaded = false,
                    "config-missing" => s.server_config.config = None,
                    "disconnected" => s.phase = Phase::Online(ConnectionStatus::Connecting),
                    "generation" => { fixture.generation.send(2).unwrap(); },
                    "environment" => s.server_config.config.as_mut().unwrap().environment.environment_id = "replacement-env".into(),
                    "connection" => {
                        let (client, observed) = t3_client::supervisor::ClientFixture::handle(session(true));
                        replacement_fixture = Some(observed);
                        s.install_active_environment(client, shell.clone(), config.clone(), ConnectionStatus::Connected, cx);
                    },
                    "missing-shell" => s.shell.snapshot.as_mut().unwrap().threads.retain(|t| t.id != "sibling"),
                    "missing-model" => { s.sidebar.model = Arc::new(Default::default()); },
                    "approval" => { Arc::make_mut(&mut s.sidebar.model).threads.iter_mut().find(|t| t.id == "sibling").unwrap().has_pending_approvals = true; },
                    _ => {}
                });
                assert_eq!(request.plan_for(state.read(cx), now_ms()).is_some(), mode == "valid", "{mode}");
                dialog.update(cx, |d, cx| {
                    d.mode = Mode::Duration;
                    d.amount.update(cx, |input, cx| input.set_value("2", window, cx));
                    d.submit(window, cx);
                });
                if mode != "valid" {
                    assert_eq!(state.read(cx).sidebar.selection.len(), 2, "{mode}");
                } else { assert_eq!(state.read(cx).sidebar.selection.len(), 0); }
                dialog.update(cx, |d, cx| d.close(window, cx));
            });
            let commands = fixture.take_commands();
            let replacement_commands = replacement_fixture.as_mut().map(|fixture| fixture.take_commands()).unwrap_or_default();
            assert!(!replacement_commands.contains(&"orchestration.dispatchCommand"));
            records.push(serde_json::json!({"guard":mode,"commands":commands,"replacementCommands":replacement_commands}));
            println!("guard {mode}: {commands:?}");
            std::fs::write(out.join("actions.json"), serde_json::to_vec_pretty(&records).unwrap()).unwrap();
            assert_eq!(commands.iter().filter(|m| **m == "orchestration.dispatchCommand").count(), if mode == "valid" {2} else {0}, "{mode}: {commands:?}");
            edit(&mut cx, &mut |_, cx| state.update(cx, |s, cx| {
                let _ = s.sidebar.selection.clear();
                let (client, replacement) = t3_client::supervisor::ClientFixture::handle(session(true));
                fixture = replacement;
                s.install_active_environment(client, shell.clone(), config.clone(), ConnectionStatus::Connected, cx);
                s.sidebar.capabilities = caps.clone();
            }));
            fixture.take_commands();
        }
        for changed in ["phase", "generation", "environment"] {
            let config = state.read_with(&cx, |s, _| s.server_config.clone());
            edit(&mut cx, &mut |window, cx| {
                window.focus(&outside, cx);
                let request = SnoozeRequest::capture(state.read(cx), vec!["menu-thread".into()], false, Some(outside.clone()));
                dialog.update(cx, |d, cx| d.open(request, window, cx));
                state.update(cx, |s, cx| {
                    match changed {
                        "phase" => { s.phase = Phase::Online(ConnectionStatus::Connecting); },
                        "generation" => { fixture.generation.send(2).unwrap(); },
                        _ => s.server_config.config.as_mut().unwrap().environment.environment_id = "replacement-env".into(),
                    }
                    cx.emit(if changed == "environment" { crate::app_state::AppEvent::ServerConfig } else { crate::app_state::AppEvent::Phase });
                });
            });
            assert!(dialog.read_with(&cx, |d, _| d.pending.is_none() && d.clock.is_none() && !d.calendar_open));
            cx.update_window(window.into(), |_, win, _| assert!(outside.is_focused(win), "owner event: {changed}")).unwrap();
            state.update(&mut cx, |s, _| { s.server_config = config.clone(); s.phase = Phase::Online(ConnectionStatus::Connected); });
            fixture.generation.send(1).unwrap();
            assert!(!fixture.take_commands().contains(&"orchestration.dispatchCommand"));
            records.push(serde_json::json!({"invalidationEvent":changed,"requestCancelled":true,"timerReleased":true,"focusRestored":true}));
        }
        edit(&mut cx, &mut |window, cx| {
            let first = SnoozeRequest::capture(state.read(cx), vec!["menu-thread".into()], false, Some(outside.clone()));
            dialog.update(cx, |d, cx| {
                d.open(first, window, cx);
                let next = SnoozeRequest::capture(state.read(cx), vec!["sibling".into()], false, Some(d.footer_focus[0].clone()));
                d.mode = Mode::Duration;
                d.amount.update(cx, |input, cx| input.set_value("9", window, cx));
                d.calendar.update(cx, |calendar, _| calendar.set_view(CalendarView::Year));
                d.open(next, window, cx);
                assert!(d.date.read(cx).focus_handle(cx).is_focused(window));
                assert_eq!(d.pending.as_ref().unwrap().ids, ["sibling"]);
                assert!(d.mode == Mode::Date && d.unit == DurationUnit::Hours);
                assert_eq!(d.amount.read(cx).value().as_str(), "2");
                assert_eq!(d.calendar.read(cx).view(), CalendarView::Day);
                d.close(window, cx);
                assert!(outside.is_focused(window));
            });
        });
        edit(&mut cx, &mut |window, cx| {
            window.focus(&outside, cx);
            menu.update(cx, |m, cx| m.open_for_thread("menu-thread", point(px(20.), px(20.)), window, cx));
        });
        draw(&mut cx);
        let keys = menu.read_with(&cx, |m, _| m.custom_snooze_keys());
        for next in keys { edit(&mut cx, &mut |window, cx| key(&dialog, window, cx, next)); }
        assert!(dialog.read_with(&cx, |d, _| d.pending.as_ref().is_some_and(|r| !r.bulk && r.ids == ["menu-thread"])));
        edit(&mut cx, &mut |window, cx| key(&dialog, window, cx, "escape"));
        // The expired fixed-time path keeps bulk selection and queues no command.
        edit(&mut cx, &mut |_, cx| state.update(cx, |s, cx| {
            s.toasts = Default::default();
            s.sidebar.selection.toggle("menu-thread");
            let request = SnoozeRequest::capture(s, vec!["menu-thread".into()], true, None);
            let shown = [t3_core::sidebar::settled::SnoozePreset { id:"at:today:00:00".into(), label:"Expired".into(), when_label:"00:00".into(), snoozed_until:0 }];
            request.pick_preset(&shown[0].id, &shown, s, cx);
            assert_eq!(s.sidebar.selection.len(), 1);
            s.sidebar.selection.clear();
            let actions = t3_core::thread_actions::plan_undo(t3_core::thread_actions::PlanFor::Snooze, false, None, None);
            assert_eq!(actions, [t3_core::thread_actions::UndoStep::Unsnooze]);
            assert!(request.plan_for_action(s, now_ms(), false).is_some());
        }));
        assert!(!fixture.take_commands().contains(&"orchestration.dispatchCommand"));
        frame(&mut cx, "expired-preset-warning");
        edit(&mut cx, &mut |_, cx| state.update(cx, |s, cx| {
            let original = s.server_config.config.as_ref().unwrap().settings.threads.snooze_presets.clone();
            s.server_config.config.as_mut().unwrap().settings.threads.snooze_presets = vec![t3_core::settings::snooze::SnoozePreset::After { minutes:90 }];
            let request = SnoozeRequest::capture(s, vec!["menu-thread".into()], true, None);
            let shown = [t3_core::sidebar::settled::SnoozePreset { id:"after:90".into(), label:"In 90 minutes".into(), when_label:"Expired shown time".into(), snoozed_until:0 }];
            s.sidebar.selection.toggle("menu-thread");
            let fixed = t3_core::sidebar::settled::pick_snooze_preset(&shown, "after:90", 1_000, &s.snooze_settings(), &SystemZone, &crate::sidebar::controller::time_label).unwrap();
            assert_eq!(fixed.snoozed_until, 1_000 + 90 * MINUTE_MS);
            request.pick_preset("after:90", &shown, s, cx);
            assert!(s.sidebar.selection.is_empty());
            s.server_config.config.as_mut().unwrap().settings.threads.snooze_presets = original;
        }));
        let preset_commands = fixture.take_commands();
        assert_eq!(preset_commands.iter().filter(|m| **m == "orchestration.dispatchCommand").count(), 1);
        records.push(serde_json::json!({"preset":"re-resolved from current settings", "commands":preset_commands}));
        edit(&mut cx, &mut |window, cx| {
            window.focus(&outside, cx);
            let request = SnoozeRequest::capture(state.read(cx), vec!["sibling".into()], false, Some(outside.clone()));
            dialog.update(cx, |d, cx| { d.open(request, window, cx); d.mode = Mode::Duration; });
        });
        draw(&mut cx);
        edit(&mut cx, &mut |window, cx| {
            dialog.update(cx, |d, cx| d.amount.update(cx, |input, cx| {
                input.focus(window, cx); assert!(input.focus_handle(cx).is_focused(window));
            }));
            key(&dialog, window, cx, "enter");
        });
        assert!(dialog.read_with(&cx, |d, _| d.pending.is_none() && d.clock.is_none()));
        cx.update_window(window.into(), |_, win, _| assert!(outside.is_focused(win))).unwrap();
        let enter_commands = fixture.take_commands();
        assert_eq!(enter_commands.iter().filter(|m| **m == "orchestration.dispatchCommand").count(), 1);
        records.push(serde_json::json!({"singleValidInputEnter":true,"target":"sibling","commands":enter_commands}));
        // A fresh bulk request is cancelled through each actual event path without selection loss.
        for route in ["cancel", "cancel-key", "backdrop", "escape", "settings-key", "settings-click"] {
            edit(&mut cx, &mut |window, cx| {
                state.update(cx, |s, _| { s.sidebar.selection.toggle("menu-thread"); s.sidebar.selection.toggle("sibling"); });
                window.focus(&outside, cx);
                menu.update(cx, |m, cx| m.open_for_selection(point(px(20.), px(20.)), window, cx));
            });
            draw(&mut cx);
            let keys = menu.read_with(&cx, |m, _| m.custom_snooze_keys());
            for next in keys { edit(&mut cx, &mut |window, cx| key(&dialog, window, cx, next)); }
            assert!(dialog.read_with(&cx, |d, _| d.pending.as_ref().is_some_and(|r| r.bulk && r.ids.len() == 2)));
            if route == "cancel" { frame(&mut cx, "07-bulk-custom"); } else { draw(&mut cx); }
            edit(&mut cx, &mut |window, cx| {
                dialog.update(cx, |d, cx| {
                    assert!(d.mode == Mode::Date);
                    assert_eq!(d.unit, DurationUnit::Hours);
                    assert_eq!(d.amount.read(cx).value().as_str(), "2");
                    assert!(d.error.is_none());
                    window.focus(&d.mode_focus[0], cx);
                });
                let stops = dialog.read(cx).mode_focus.iter().cloned().chain([dialog.read(cx).date.read(cx).focus_handle(cx), dialog.read(cx).calendar_trigger.clone(), dialog.read(cx).time.read(cx).focus_handle(cx)]).chain(dialog.read(cx).footer_focus.iter().cloned()).collect::<Vec<_>>();
                for expected in stops.iter().cycle().skip(1).take(stops.len()) {
                    key(&dialog, window, cx, "tab"); assert!(expected.is_focused(window));
                }
                dialog.update(cx, |d, _| assert!(d.mode_focus[0].is_focused(window)));
            });
            edit(&mut cx, &mut |window, cx| {
                if route.starts_with("settings") {
                    dialog.update(cx, |d, cx| {
                        d.amount.update(cx, |input, cx| input.set_value("9", window, cx));
                        d.calendar.update(cx, |calendar, _| calendar.set_view(CalendarView::Year));
                    });
                }
                match route {
                    "cancel" => click(window, cx, 553., 485.),
                    "cancel-key" => { dialog.update(cx, |d, cx| window.focus(&d.footer_focus[1], cx)); key(&dialog, window, cx, "space"); },
                    "backdrop" => {
                        click(window, cx, 500., 270.);
                        assert!(dialog.read(cx).pending.is_some());
                        click(window, cx, 950., 700.);
                    },
                    "escape" => {
                        dialog.update(cx, |d, cx| window.focus(&d.calendar_trigger, cx)); key(&dialog, window, cx, "space");
                        assert!(dialog.read(cx).calendar_open);
                        key(&dialog, window, cx, "escape");
                        assert!(!dialog.read(cx).calendar_open && dialog.read(cx).calendar_trigger.is_focused(window));
                        key(&dialog, window, cx, "escape");
                    },
                    "settings-key" => {
                        dialog.update(cx, |d, cx| window.focus(&d.calendar_trigger, cx)); key(&dialog, window, cx, "space");
                        assert!(dialog.read(cx).calendar_open);
                        dialog.update(cx, |d, cx| window.focus(&d.footer_focus[0], cx)); key(&dialog, window, cx, "enter");
                    },
                    "settings-click" => click(window, cx, 420., 483.),
                    _ => unreachable!()
                }
            });
            assert!(dialog.read_with(&cx, |d, _| d.pending.is_none() && d.clock.is_none() && !d.calendar_open), "{route}");
            assert_eq!(state.read_with(&cx, |s, _| s.sidebar.selection.len()), 2, "{route}");
            records.push(serde_json::json!({"cancelRoute":route,"pending":false,"timerOwned":false,"selection":2}));
            println!("cancel route {route}: released request/timer, retained selection");
            if route.starts_with("settings") {
                frame(&mut cx, &format!("{route}-navigation"));
                edit(&mut cx, &mut |window, cx| click(window, cx, 5., 5.));
                cx.update_window(window.into(), |_, win, _| assert!(outside.is_focused(win))).unwrap();
                edit(&mut cx, &mut |window, cx| {
                    let request = SnoozeRequest::capture(state.read(cx), vec!["sibling".into()], false, Some(outside.clone()));
                    dialog.update(cx, |d, cx| {
                        d.open(request, window, cx);
                        assert!(d.date.read(cx).focus_handle(cx).is_focused(window));
                        assert_eq!(d.pending.as_ref().unwrap().ids, ["sibling"]);
                        assert_eq!(d.pending.as_ref().unwrap().owner, SnoozeOwner::capture(state.read(cx)));
                        assert!(d.mode == Mode::Date && d.unit == DurationUnit::Hours && d.error.is_none());
                        assert_eq!(d.amount.read(cx).value().as_str(), "2");
                        assert_eq!(d.calendar.read(cx).view(), CalendarView::Day);
                        d.close(window, cx);
                        assert!(outside.is_focused(window));
                    });
                });
            } else { edit(&mut cx, &mut |window, _| assert!(outside.is_focused(window), "{route}")); }
            state.update(&mut cx, |s, _| { s.sidebar.selection.clear(); });
            let commands = fixture.take_commands();
            assert!(!commands.contains(&"orchestration.dispatchCommand") && !commands.contains(&"server.updateSettings"));
        }
        assert!(state.read_with(&cx, |s, _| s.selected_thread.is_none()));
        std::fs::write(out.join("actions.json"), serde_json::to_vec_pretty(&records).unwrap()).unwrap();
        std::fs::write(out.join("isolation.json"), serde_json::to_vec_pretty(&serde_json::json!({"selectedThread":state.read_with(&cx, |s, _| s.selected_thread.clone()),"client":"ClientFixture: in-memory channels, no supervisor", "networkWrites":0,"guards":["grant","capability","config","connection","generation","environment","target"],"validBulkSubmitCommands":2,"validSingleEnterCommands":1,"validPresetCommands":1})).unwrap()).unwrap();
        state.update(&mut cx, |s, _| s.stop_pump());
        cx.update(|cx| { cx.shutdown(); cx.clear_globals(); });
        drop(fixture); drop(runtime); cx.run_until_parked();
    }
}
