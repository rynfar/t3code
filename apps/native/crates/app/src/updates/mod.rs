//! Sidebar update notices and confirmation from the web's `SidebarProviderUpdatePill.tsx`,
//! `SidebarUpdatePill.tsx`, and `UpdateEverythingDialog.tsx`. Indicators are static at idle.
mod dialog;
#[cfg(feature = "snapshot")]
mod fixture;

use crate::{
    app_state::{AppEvent, AppState, Phase},
    icons::Icon,
    theme::{hsla, palette, rem},
};
use gpui::{
    App, Context, Entity, Global, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use std::{collections::HashSet, time::Duration};
use t3_core::{
    settings::search::Section,
    updates::{self, Machine, Pill, Plan, Tone},
};

struct Host {
    control: WeakEntity<UpdateControl>,
    dialog: WeakEntity<dialog::UpdateDialog>,
}
impl Global for Host {}
pub fn overlay(cx: &App) -> Option<Entity<dialog::UpdateDialog>> {
    cx.try_global::<Host>()?.dialog.upgrade()
}

pub struct UpdateControl {
    state: Entity<AppState>,
    dialog: Entity<dialog::UpdateDialog>,
    plan: Plan,
    pill: Option<Pill>,
    dismissed: HashSet<String>,
    visible_after: Option<String>,
    connection: Option<u64>,
    success_timer: Option<Task<()>>,
    busy: Option<String>,
    fixture: bool,
    _subscriptions: Vec<Subscription>,
    _grants: Option<Task<()>>,
}

fn target_version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        // Native's crate version is independent of the server release it was built alongside.
        serde_json::from_str::<serde_json::Value>(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../server/package.json"
        )))
        .ok()
        .and_then(|v| v["version"].as_str().map(str::to_owned))
        .unwrap_or_default()
    });
    &VERSION
}

fn connected(state: &AppState) -> bool {
    matches!(
        state.phase,
        Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
    )
}
pub(super) fn plan(state: &AppState, updating: bool) -> Plan {
    let Some(config) = state.server_config.config.as_ref() else {
        return Plan::default();
    };
    let grants = state
        .client_handle()
        .and_then(|c| c.session_state().borrow().clone());
    updates::collect(
        &[Machine {
            id: &config.environment.environment_id,
            label: &config.environment.label,
            config: Some(config),
            connected: connected(state),
            hosted_by_this_app: false,
            can_maintain: grants
                .as_ref()
                .is_some_and(|s| s.grants("environment:maintain")),
            can_manage_providers: grants
                .as_ref()
                .is_some_and(|s| s.grants("providers:manage")),
            updating,
        }],
        target_version(),
    )
}
impl UpdateControl {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let dialog = cx.new(|cx| dialog::UpdateDialog::new(state.clone(), cx));
        cx.set_global(Host {
            control: cx.weak_entity(),
            dialog: dialog.downgrade(),
        });
        let subscriptions = vec![cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::ServerConfig | AppEvent::Phase) {
                this.sync(cx);
            }
        })];
        let mut this = Self {
            state,
            dialog,
            plan: Plan::default(),
            pill: None,
            dismissed: HashSet::new(),
            visible_after: None,
            connection: None,
            success_timer: None,
            busy: None,
            fixture: false,
            _subscriptions: subscriptions,
            _grants: None,
        };
        this.sync(cx);
        this
    }
    fn sync(&mut self, cx: &mut Context<Self>) {
        if self.fixture {
            return;
        }
        let state = self.state.read(cx);
        if self.connection != state.connection_id() {
            self.connection = state.connection_id();
            self.visible_after = None;
            self.dismissed.clear();
            self._grants = state.client_handle().map(|client| {
                let mut grants = client.session_state();
                cx.spawn(async move |this, cx| {
                    while grants.changed().await.is_ok() {
                        let _ = this.update(cx, |this, cx| this.sync(cx));
                    }
                })
            });
        }
        let state = self.state.read(cx);
        let providers = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.providers.as_slice())
            .unwrap_or_default();
        if self.visible_after.is_none() {
            self.visible_after = providers
                .iter()
                .filter_map(|p| p.checked_at.0.as_ref())
                .max()
                .cloned();
        }
        let next_pill = if connected(state) {
            updates::provider_pill(providers, self.visible_after.as_deref(), &self.dismissed)
        } else {
            None
        };
        let next_plan = plan(state, self.busy.is_some());
        let changed = self.plan != next_plan || self.pill != next_pill;
        self.plan = next_plan;
        if self.pill != next_pill {
            self.pill = next_pill;
            self.success_timer = None;
            if let Some(pill) = &self.pill
                && let Some(ms) = pill.dismiss_after_ms
            {
                let key = pill.key.clone();
                self.success_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.dismissed.insert(key);
                        this.sync(cx);
                    });
                }));
            }
        }
        if changed {
            cx.notify();
        }
    }
    pub(super) fn busy(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        self.busy = message;
        self.sync(cx);
        cx.notify();
    }
}
impl Render for UpdateControl {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let mut body = div().flex_none().flex().flex_col().gap(rem(6.)).px(rem(8.));
        if let Some(view) = &self.pill {
            let (fill, color, icon) = match view.tone {
                Tone::Loading => (
                    hsla(p.sidebar_control_surface),
                    p.sidebar_foreground,
                    Icon::LoaderCircle,
                ),
                Tone::Success => (
                    hsla(p.sidebar_control_surface),
                    p.sidebar_foreground,
                    Icon::CircleCheck,
                ),
                Tone::Warning => (hsla(p.warning).opacity(0.12), p.warning, Icon::Download),
                Tone::Error => (
                    hsla(p.destructive).opacity(0.12),
                    p.destructive,
                    Icon::TriangleAlert,
                ),
            };
            let description = view.description.clone();
            let mut row = div()
                .w_full()
                .flex()
                .items_center()
                .rounded(rem(8.))
                .bg(fill)
                .text_color(hsla(color))
                .text_size(px(11.))
                .line_height(rem(16.));
            row = row.child(
                div()
                    .id("provider-update-main")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(rem(8.))
                    .px(rem(8.))
                    .py(rem(6.))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.8))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(description.clone()).build(window, cx)
                    })
                    .child(icon.sized(px(14.)).text_color(hsla(color)))
                    .child(view.title.clone())
                    .on_click(|_, _, cx| crate::settings::open(Section::Providers, cx)),
            );
            if view.dismissible {
                row = row.child(
                    div()
                        .id("dismiss-provider-update")
                        .size(px(20.))
                        .mr(rem(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(rem(6.))
                        .cursor_pointer()
                        .opacity(0.7)
                        .hover(|s| s.opacity(1.))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                "Dismiss until provider status changes",
                            )
                            .build(window, cx)
                        })
                        .child(Icon::X.sized(px(14.)).text_color(hsla(color)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(pill) = &this.pill {
                                this.dismissed.insert(pill.key.clone());
                            }
                            if this.fixture {
                                this.pill = None;
                                cx.notify();
                            } else {
                                this.sync(cx);
                            }
                        })),
                );
            }
            body = body.child(row);
        }
        if self.plan.has_updates() || self.busy.is_some() {
            let label = self.busy.clone().unwrap_or_else(|| self.plan.summary());
            body = body.child(
                div()
                    .id("update-everything")
                    .w_full()
                    .min_h(rem(28.))
                    .flex()
                    .items_center()
                    .gap(rem(8.))
                    .px(rem(8.))
                    .py(rem(6.))
                    .rounded(rem(8.))
                    .bg(hsla(p.sidebar_control_surface))
                    .text_color(hsla(p.update_foreground))
                    .text_size(px(11.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(p.sidebar_row_hover)))
                    .child(
                        if self.busy.is_some() {
                            Icon::LoaderCircle
                        } else {
                            Icon::CircleArrowUp
                        }
                        .sized(px(14.))
                        .text_color(hsla(p.update_foreground)),
                    )
                    .child(label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.busy.is_none() {
                            let plan = this.plan.clone();
                            this.dialog.update(cx, |dialog, cx| {
                                dialog.open(plan, this.fixture, window, cx)
                            });
                        }
                    })),
            );
        }
        body
    }
}
#[cfg(feature = "snapshot")]
pub fn show_fixture(value: serde_json::Value, window: &mut Window, cx: &mut App) {
    fixture::show(value, window, cx);
}
