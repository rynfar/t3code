use super::{Host, connected, plan};
use crate::{
    app_state::{AppEvent, AppState},
    icons::Icon,
    theme::{hsla, palette, rem},
    toast::{Toast, ToastKind},
};
use gpui::{
    App, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    Render, Styled, Subscription, Window, deferred, div, prelude::*, px,
};
use t3_core::updates::{Plan, thread_warning};

pub struct UpdateDialog {
    state: Entity<AppState>,
    pub(super) plan: Option<Plan>,
    connection: Option<u64>,
    pub(super) fixture: bool,
    pub(super) counts: Vec<(String, usize)>,
    expanded: bool,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    skipped_focus: FocusHandle,
    cancel_focus: FocusHandle,
    update_focus: FocusHandle,
    _subscription: Subscription,
}
impl UpdateDialog {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if this.plan.is_some()
                && matches!(
                    event,
                    AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase
                )
                && !this.fixture
            {
                let counts = this.running_counts(cx);
                if counts != this.counts
                    || matches!(event, AppEvent::ServerConfig | AppEvent::Phase)
                {
                    this.counts = counts;
                    cx.notify();
                }
            }
        });
        Self {
            state,
            plan: None,
            connection: None,
            fixture: false,
            counts: Vec::new(),
            expanded: false,
            focus: cx.focus_handle(),
            previous_focus: None,
            skipped_focus: cx.focus_handle().tab_stop(true),
            cancel_focus: cx.focus_handle().tab_stop(true),
            update_focus: cx.focus_handle().tab_stop(true),
            _subscription: subscription,
        }
    }
    pub fn open(&mut self, plan: Plan, fixture: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = Some(plan);
        self.fixture = fixture;
        self.expanded = false;
        self.connection = self.state.read(cx).connection_id();
        self.counts = self.running_counts(cx);
        self.previous_focus = window.focused(cx);
        window.focus(&self.cancel_focus, cx);
        cx.notify();
    }
    fn running_counts(&self, cx: &App) -> Vec<(String, usize)> {
        let Some(plan) = &self.plan else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        let environment = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.as_str())
            .unwrap_or_default();
        t3_core::updates::running_counts(
            &plan.restarts,
            state.shell.snapshot.as_ref().into_iter().flat_map(|shell| {
                shell
                    .threads
                    .iter()
                    .map(|thread| (environment, thread.status.as_str()))
            }),
        )
    }
    fn disabled_reason(&self, cx: &App) -> Option<&'static str> {
        if self.fixture {
            return None;
        }
        let state = self.state.read(cx);
        if self.connection != state.connection_id() || !connected(state) {
            return Some("The environment is not connected.");
        }
        if self.plan.as_ref() != Some(&plan(state, false)) {
            return Some("Updates changed. Close this dialog and review them again.");
        }
        None
    }
    fn cycle_focus(&self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut stops = Vec::new();
        if self
            .plan
            .as_ref()
            .is_some_and(|p| p.rows.iter().any(|r| r.skipped.is_some()))
        {
            stops.push(&self.skipped_focus);
        }
        stops.push(&self.cancel_focus);
        if self.disabled_reason(cx).is_none() {
            stops.push(&self.update_focus);
        }
        let index = stops
            .iter()
            .position(|focus| focus.is_focused(window))
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % stops.len()
        } else {
            (index + stops.len() - 1) % stops.len()
        };
        window.focus(stops[next], cx);
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = None;
        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus, cx);
        } else {
            window.blur();
        }
        cx.notify();
    }
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fixture {
            self.close(window, cx);
            return;
        }
        if self.disabled_reason(cx).is_some() {
            return;
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else {
            return;
        };
        let connection = state.connection_id();
        let runtime = state.runtime().clone();
        let state = self.state.clone();
        let control = cx.global::<Host>().control.clone();
        self.close(window, cx);
        if let Some(control) = control.upgrade() {
            control.update(cx, |control, cx| control.busy(Some("Updating…".into()), cx));
        }
        let task = runtime.spawn(async move { t3_client::updates::run(&client, plan).await });
        cx.spawn(async move |_, cx| {
            let failures = task.await.unwrap_or_else(|e| vec![e.to_string()]);
            if let Some(control) = control.upgrade() {
                control.update(cx, |control, cx| control.busy(None, cx));
            }
            state.update(cx, |state, cx| {
                if state.connection_id() != connection {
                    return;
                }
                let (kind, title) = if failures.is_empty() {
                    (ToastKind::Success, "Updates complete")
                } else {
                    (ToastKind::Error, "Some updates failed")
                };
                state.show_toast(Toast::new(kind, title).description(failures.join("\n")), cx);
            });
        })
        .detach();
    }
}
impl Render for UpdateDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(plan) = self.plan.as_ref() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let title = plan.title();
        let mut description = plan
            .version()
            .map(|v| format!("T3 Code {v}"))
            .unwrap_or_default();
        let n = plan.providers.len();
        if n > 0 {
            if !description.is_empty() {
                description.push_str(" · ");
            }
            description.push_str(&format!(
                "{n} provider {}",
                if n == 1 { "update" } else { "updates" }
            ));
        }
        let mut rows = div()
            .id("update-machine-rows")
            .max_h(rem(360.))
            .overflow_y_scroll()
            .px(rem(24.))
            .pb(rem(16.))
            .flex()
            .flex_col()
            .gap(rem(6.));
        let cells = |label: String, app: String, providers: String, count: usize| {
            div()
                .flex()
                .gap(rem(16.))
                .py(rem(6.))
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.5))
                .text_size(rem(14.))
                .child(
                    div()
                        .w(rem(128.))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
                .child(div().w(rem(84.)).child(app))
                .child(div().flex_1().min_w_0().child(providers))
                .child(
                    div()
                        .w(rem(70.))
                        .text_color(hsla(p.warning))
                        .child(if count > 0 {
                            format!("{count} running")
                        } else {
                            String::new()
                        }),
                )
        };
        rows = rows.child(
            div()
                .flex()
                .gap(rem(16.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .border_b_1()
                .border_color(hsla(p.border))
                .pb(rem(6.))
                .child(div().w(rem(128.)).child("Machine"))
                .child(div().w(rem(84.)).child("T3 Code"))
                .child(div().flex_1().child("Providers"))
                .child(div().w(rem(70.))),
        );
        for row in plan.updating_rows() {
            let count = self
                .counts
                .iter()
                .find(|(id, _)| id == &row.environment_id)
                .map_or(0, |(_, n)| *n);
            rows = rows.child(cells(
                row.label.clone(),
                row.app.clone().unwrap_or_else(|| {
                    if row.skipped.is_some() {
                        "Skipped".into()
                    } else {
                        String::new()
                    }
                }),
                row.providers.join(", "),
                count,
            ));
        }
        let skipped: Vec<_> = plan.rows.iter().filter(|r| r.skipped.is_some()).collect();
        if !skipped.is_empty() {
            rows = rows.child(
                div()
                    .id("update-skipped")
                    .track_focus(&self.skipped_focus)
                    .focus_visible(|s| s.text_color(hsla(p.foreground)))
                    .flex()
                    .items_center()
                    .gap(rem(4.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .cursor_pointer()
                    .child(
                        if self.expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        }
                        .sized(px(14.)),
                    )
                    .child(format!("{} skipped", skipped.len()))
                    .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                        if event.is_keyboard() {
                            return;
                        }
                        this.expanded = !this.expanded;
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if crate::settings::controls::activates(event) {
                            cx.stop_propagation();
                            this.expanded = !this.expanded;
                            cx.notify();
                        }
                    })),
            );
            if self.expanded {
                for row in skipped {
                    rows = rows.child(
                        div()
                            .pl(rem(18.))
                            .flex()
                            .gap(rem(12.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(div().w(rem(128.)).child(row.label.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .child(row.skipped.clone().unwrap_or_default()),
                            ),
                    );
                }
            }
        }
        let mut stopped = 0;
        let mut continuing = 0;
        for restart in &plan.restarts {
            let n = self
                .counts
                .iter()
                .find(|(id, _)| id == &restart.environment_id)
                .map_or(0, |(_, n)| *n);
            if restart.continues_threads {
                continuing += n;
            } else {
                stopped += n;
            }
        }
        let disabled = self.disabled_reason(cx);
        let warning = disabled
            .map(str::to_owned)
            .or_else(|| thread_warning(stopped, continuing));
        let button = |id, label, primary, focus: &FocusHandle| {
            div()
                .id(id)
                .track_focus(focus)
                .h(rem(32.))
                .px(rem(11.))
                .flex()
                .flex_none()
                .items_center()
                .rounded(rem(8.))
                .border_1()
                .border_color(hsla(if primary { p.primary } else { p.input }))
                .bg(hsla(if primary { p.primary } else { p.popover }))
                .text_color(hsla(if primary {
                    p.primary_foreground
                } else {
                    p.foreground
                }))
                .text_sm()
                .cursor_pointer()
                .focus_visible(|s| {
                    s.shadow(vec![gpui::BoxShadow {
                        color: hsla(p.ring),
                        offset: gpui::point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    }])
                })
                .child(label)
        };
        deferred(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(rem(16.))
                .bg(hsla(p.dialog_backdrop))
                .child(
                    div()
                        .id("update-everything-dialog")
                        .key_context(super::DIALOG_CONTEXT)
                        .track_focus(&self.focus)
                        .on_action(cx.listener(|this, _: &super::FocusNextUpdate, window, cx| {
                            this.cycle_focus(true, window, cx)
                        }))
                        .on_action(cx.listener(
                            |this, _: &super::FocusPreviousUpdate, window, cx| {
                                this.cycle_focus(false, window, cx)
                            },
                        ))
                        .relative()
                        .w_full()
                        .max_w(rem(576.))
                        .rounded(rem(16.))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.close(window, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(crate::glass::backdrop(
                            16.,
                            hsla(p.dialog_surface),
                            hsla(p.dialog_border),
                            cx,
                        ))
                        .child(
                            div()
                                .p(rem(24.))
                                .flex()
                                .flex_col()
                                .gap(rem(8.))
                                .child(
                                    div()
                                        .text_xl()
                                        .text_color(hsla(p.popover_foreground))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(description),
                                ),
                        )
                        .child(rows)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(rem(8.))
                                .px(rem(24.))
                                .py(rem(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted))
                                .rounded_b(rem(15.))
                                .children(warning.map(|s| {
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(s)
                                }))
                                .child(
                                    button("updates-cancel", "Cancel", false, &self.cancel_focus)
                                        .on_click(cx.listener(
                                            |this, event: &gpui::ClickEvent, window, cx| {
                                                if !event.is_keyboard() {
                                                    this.close(window, cx);
                                                }
                                            },
                                        ))
                                        .on_key_down(cx.listener(
                                            |this, event: &KeyDownEvent, window, cx| {
                                                if crate::settings::controls::activates(event) {
                                                    cx.stop_propagation();
                                                    this.close(window, cx);
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    button(
                                        "updates-confirm",
                                        "Update",
                                        true,
                                        &self.update_focus.clone().tab_stop(disabled.is_none()),
                                    )
                                    .when(disabled.is_some(), |s| s.opacity(0.5))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.confirm(window, cx)),
                                    )
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if crate::settings::controls::activates(event) {
                                                cx.stop_propagation();
                                                this.confirm(window, cx);
                                            }
                                        },
                                    )),
                                ),
                        ),
                ),
        )
        .into_any_element()
    }
}
