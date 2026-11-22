//! Post-sign-in T3 Connect onboarding wizard (`apps/web/src/components/cloud/ConnectOnboardingDialog.tsx`):
//! opens only on a fresh signed-out -> signed-in Clerk transition unless the user opted out.

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, IntoElement, MouseButton, ParentElement,
    Render, SharedString, Styled, Subscription, Window, deferred, div, hsla as gpui_hsla,
    prelude::*, px,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use serde_json::json;
use t3_core::commands::method::SERVER_UPDATE_SETTINGS;
use t3_core::relay::{Environment, EnvironmentHealth};
use t3_core::settings::access::{ACCESS_WRITE, CloudLinkState, can_configure_relay};

use crate::app_state::{AppEvent, AppState};
use crate::cloud::render_tunnel_limit_alert;
use crate::composer::controls::{checkbox, floating_shadow};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, Switch};
use crate::theme::{ThemeSlot, ThemeSlotExt as _, hsla, palette, theme_slot_clipped_background_layer};
use crate::toast::{Toast, ToastKind};

const CONNECT_ONBOARDING_OPT_OUT_KEY: &str = "connect-onboarding-opt-out";
const RELAY_READ: &str = "relay:read";
const RELAY_WRITE: &str = "relay:write";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    Publish,
    Devices,
}

/// Returns `true` unless the user explicitly checked "Don't show this again".
pub fn should_open_on_fresh_sign_in() -> bool {
    if cfg!(test) {
        return true;
    }
    match t3_client::store::load_ui_state(CONNECT_ONBOARDING_OPT_OUT_KEY) {
        Some(serde_json::Value::Bool(opted_out)) => !opted_out,
        Some(value) if value.get("optOut").and_then(|v| v.as_bool()) == Some(true) => false,
        _ => true,
    }
}

pub fn set_connect_onboarding_opt_out(opt_out: bool) {
    if !cfg!(test) {
        t3_client::store::save_ui_state(CONNECT_ONBOARDING_OPT_OUT_KEY, &serde_json::Value::Bool(opt_out));
    }
}

fn can_publish_primary(state: &AppState) -> bool {
    let Some(client) = state.client_handle() else {
        return false;
    };
    let session_state = client.session_state();
    let session = session_state.borrow();
    let Some(session) = session.as_ref() else {
        return false;
    };
    let policy = state
        .server_config
        .config
        .as_ref()
        .and_then(|cfg| cfg.managed_policy.as_ref());
    let policy_ok = t3_core::managed_policy::capability_denial(
        policy,
        t3_core::managed_policy::Capability::Connect,
    )
    .is_none();
    policy_ok
        && session.grants(RELAY_READ)
        && session.grants(RELAY_WRITE)
        && can_configure_relay(session)
}

fn can_require_device_approval(state: &AppState) -> bool {
    state
        .client_handle()
        .is_some_and(|client| {
            client
                .session_state()
                .borrow()
                .as_ref()
                .is_some_and(|s| s.grants(ACCESS_WRITE))
        })
}

pub struct ConnectOnboardingDialog {
    state: Entity<AppState>,
    open: bool,
    step: OnboardingStep,
    expose_environment: bool,
    publish_agent_activity: bool,
    require_device_approval: bool,
    dont_show_again: bool,
    applying: bool,
    error: Option<String>,
    link_state: Option<CloudLinkState>,
    focus: FocusHandle,
    _subscription: Subscription,
}

impl ConnectOnboardingDialog {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if *event == AppEvent::Phase {
                this.sync_with_cloud(cx);
            }
        });
        let mut dialog = Self {
            state,
            open: false,
            step: OnboardingStep::Devices,
            expose_environment: true,
            publish_agent_activity: true,
            require_device_approval: false,
            dont_show_again: false,
            applying: false,
            error: None,
            link_state: None,
            focus: cx.focus_handle(),
            _subscription: subscription,
        };
        dialog.sync_with_cloud(cx);
        dialog
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.open {
            return false;
        }
        if !self.applying {
            self.complete(cx);
        }
        true
    }

    fn sync_with_cloud(&mut self, cx: &mut Context<Self>) {
        let (signed_in, pending, can_publish) = {
            let app = self.state.read(cx);
            (
                app.cloud.has_clerk_session && app.cloud.identity.is_some(),
                app.cloud.pending_onboarding,
                can_publish_primary(app),
            )
        };
        if !signed_in {
            if self.open {
                self.open = false;
                self.applying = false;
                self.error = None;
                cx.notify();
            }
            return;
        }
        if !pending {
            return;
        }
        self.state.update(cx, |state, _| {
            state.cloud.pending_onboarding = false;
        });
        if !should_open_on_fresh_sign_in() {
            return;
        }
        self.open = true;
        self.step = if can_publish {
            OnboardingStep::Publish
        } else {
            OnboardingStep::Devices
        };
        self.expose_environment = true;
        self.publish_agent_activity = true;
        self.require_device_approval = self
            .state
            .read(cx)
            .client_handle()
            .and_then(|c| c.server_config_extras().borrow().require_connect_device_approval)
            .unwrap_or(false);
        self.dont_show_again = false;
        self.applying = false;
        self.error = None;
        self.link_state = None;
        if can_publish {
            self.fetch_link_state(cx);
        }
        cx.notify();
    }

    fn fetch_link_state(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.state.read(cx).client_handle() else {
            return;
        };
        let task = self
            .state
            .read(cx)
            .runtime()
            .spawn(async move { client.get_access("/api/connect/link-state").await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(value)) = task.await else {
                return;
            };
            let parsed: Option<CloudLinkState> = serde_json::from_value(value).ok();
            let _ = this.update(cx, |this, cx| {
                if let Some(link) = parsed {
                    if link.linked && !link.link_revoked {
                        this.expose_environment = link.managed_tunnel_active;
                        this.publish_agent_activity = link.publish_agent_activity;
                    }
                    this.link_state = Some(link);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn complete(&mut self, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        if self.dont_show_again {
            set_connect_onboarding_opt_out(true);
        }
        self.open = false;
        self.error = None;
        cx.notify();
    }

    fn apply_publish_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        let app = self.state.read(cx);
        if !can_publish_primary(app) {
            self.step = OnboardingStep::Devices;
            cx.notify();
            return;
        }
        let approval_locked = app
            .client_handle()
            .and_then(|c| c.server_config_extras().borrow().require_connect_device_approval)
            .unwrap_or(false);
        let save_approval = self.require_device_approval && !approval_locked && can_require_device_approval(app);
        if !self.expose_environment && !self.publish_agent_activity && !save_approval {
            self.step = OnboardingStep::Devices;
            cx.notify();
            return;
        }
        let Some(client) = app.client_handle() else {
            self.step = OnboardingStep::Devices;
            cx.notify();
            return;
        };
        let environment_id = app
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.clone())
            .filter(|id| !id.is_empty());
        let cloud = app.cloud.service.clone();
        let runtime = app.runtime().clone();
        let expose = self.expose_environment;
        let publish = self.publish_agent_activity;
        self.applying = true;
        self.error = None;
        self.state
            .update(cx, |state, cx| state.set_tunnel_limit_reached(None, cx));
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            if save_approval {
                let c = client.clone();
                let res = runtime
                    .spawn(async move {
                        c.request(
                            SERVER_UPDATE_SETTINGS,
                            json!({ "patch": { "requireConnectDeviceApproval": true } }),
                        )
                        .await
                    })
                    .await;
                let failed = match res {
                    Ok(Ok(_)) => {
                        client.set_require_connect_device_approval(true);
                        None
                    }
                    Ok(Err(err)) => Some(err.to_string()),
                    Err(err) => Some(err.to_string()),
                };
                if let Some(msg) = failed {
                    let _ = this.update(cx, |this, cx| {
                        this.applying = false;
                        this.error = Some(msg);
                        cx.notify();
                    });
                    return;
                }
            }

            if (expose || publish)
                && let Some(env_id) = environment_id
            {
                let res = crate::settings::link_access_relay_detailed(
                    &client,
                    cloud,
                    &runtime,
                    &env_id,
                    expose,
                    publish,
                    cx,
                )
                .await;
                match res {
                    Ok(true) => {
                        let _ = this.update(cx, |this, cx| {
                            this.applying = false;
                            this.step = OnboardingStep::Devices;
                            this.state.update(cx, |state, cx| {
                                state.refresh_cloud(cx);
                                let desc = if expose {
                                    "This environment is available to your other devices through T3 Connect."
                                } else {
                                    "This environment publishes agent activity to your mobile clients."
                                };
                                state.show_toast(
                                    Toast::new(ToastKind::Success, "T3 Connect enabled").description(desc),
                                    cx,
                                );
                            });
                            cx.notify();
                        });
                        return;
                    }
                    Ok(false) => {
                        let _ = this.update(cx, |this, cx| {
                            this.applying = false;
                            cx.notify();
                        });
                        return;
                    }
                    Err(err) => {
                        let _ = this.update(cx, |this, cx| {
                            this.applying = false;
                            this.error = Some(err.message);
                            if let Some(limit) = err.tunnel_limit {
                                this.state.update(cx, |state, cx| {
                                    state.set_tunnel_limit_reached(Some(limit), cx);
                                });
                            }
                            cx.notify();
                        });
                        return;
                    }
                }
            }

            let _ = this.update(cx, |this, cx| {
                this.applying = false;
                this.step = OnboardingStep::Devices;
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_row(
        id: &'static str,
        (title, description): (&'static str, &'static str),
        checked: bool,
        disabled: bool,
        divider: bool,
        on_change: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        h_flex()
            .items_start()
            .justify_between()
            .gap(px(16.))
            .px(px(16.))
            .py(px(12.))
            .when(divider, |el| el.border_t_1().border_color(hsla(p.border).opacity(0.6)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(description)),
            )
            .child({
                let weak = cx.entity().downgrade();
                Switch::new(id, checked).disabled(disabled).on_change(move |next, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        on_change(this, next, cx);
                        cx.notify();
                    });
                })
            })
            .into_any_element()
    }

    fn render_publish_step(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let app = self.state.read(cx);
        let can_publish = can_publish_primary(app);
        let show_approval = can_require_device_approval(app);
        let approval_locked = app
            .client_handle()
            .and_then(|c| c.server_config_extras().borrow().require_connect_device_approval)
            .unwrap_or(false);
        let disabled = self.applying || !can_publish;

        let mut rows = vec![
            Self::toggle_row(
                "connect-onboarding-expose",
                (
                    "Publish this environment",
                    "Make this environment available to your other devices through T3 Connect.",
                ),
                self.expose_environment,
                disabled,
                false,
                |this, next, _| this.expose_environment = next,
                cx,
            ),
            Self::toggle_row(
                "connect-onboarding-activity",
                (
                    "Publish agent activity",
                    "Send activity from this environment to your mobile clients for push notifications and Live Activities.",
                ),
                self.publish_agent_activity,
                disabled,
                true,
                |this, next, _| this.publish_agent_activity = next,
                cx,
            ),
        ];
        if show_approval {
            rows.push(Self::toggle_row(
                "connect-onboarding-approval",
                (
                    "Require approval for new devices",
                    "Devices that sign in to your T3 Connect account wait until someone already connected here approves them.",
                ),
                self.require_device_approval || approval_locked,
                disabled || approval_locked,
                true,
                |this, next, _| this.require_device_approval = next,
                cx,
            ));
        }

        v_flex()
            .gap(px(12.))
            .child(
                v_flex()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .overflow_hidden()
                    .children(rows),
            )
            .children(self.error.as_ref().map(|err| {
                div()
                    .text_xs()
                    .text_color(hsla(p.destructive_foreground))
                    .child(err.clone())
            }))
            .children(render_tunnel_limit_alert(&self.state, cx))
            .into_any_element()
    }

    fn render_devices_step(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let state = self.state.read(cx);
        let primary_env_id = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.as_str())
            .filter(|id| !id.is_empty());
        let others: Vec<Environment> = state
            .cloud
            .discovery
            .environments
            .iter()
            .filter(|env| Some(env.environment_id.as_str()) != primary_env_id)
            .cloned()
            .collect();

        if others.is_empty() {
            return div()
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.border))
                .px(px(16.))
                .py(px(24.))
                .text_sm()
                .text_center()
                .text_color(hsla(p.muted_foreground))
                .child(
                    "No other environments are published to your account yet. Publish one from another device and it will show up here.",
                )
                .into_any_element();
        }

        let divider = hsla(p.border).opacity(0.6);
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.border))
            .overflow_hidden()
            .children(others.into_iter().enumerate().map(|(ix, env)| {
                let saved = state.environments.find_by_environment_id(&env.environment_id);
                let connected = saved.is_some_and(|s| s.local || t3_client::store::has_saved_relay_route(&s.id));
                let adding = state.cloud.connecting_env_id.as_deref() == Some(env.environment_id.as_str());
                let (status_label, status_color, live) = match state.cloud.statuses.get(&env.environment_id) {
                    Some(EnvironmentHealth::Online) => ("Online", p.success, false),
                    Some(EnvironmentHealth::Offline { .. }) => ("Offline", p.destructive, false),
                    Some(EnvironmentHealth::Incompatible(_)) => ("Incompatible", p.destructive, false),
                    Some(EnvironmentHealth::Error(_)) => ("Unavailable", p.muted_foreground, false),
                    Some(EnvironmentHealth::Checking) | None => ("Checking…", p.warning, true),
                };
                let online = matches!(state.cloud.statuses.get(&env.environment_id), Some(EnvironmentHealth::Online));
                let action_label = if connected {
                    "Connected"
                } else if adding {
                    "Connecting…"
                } else {
                    "Connect"
                };
                let button_env = env.clone();
                h_flex()
                    .when(ix > 0, |el| el.border_t_1().border_color(divider))
                    .items_center()
                    .gap(px(10.))
                    .px(px(14.))
                    .py(px(10.))
                    .child(crate::components::motion::StatusDot::new(
                        SharedString::from(format!("onboarding-env-dot-{}", env.environment_id)),
                        px(8.),
                        hsla(status_color),
                        live,
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(env.label.clone()))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(status_label)),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("onboarding-env-connect-{}", env.environment_id)),
                            if connected { ButtonVariant::Ghost } else { ButtonVariant::Outline },
                            ButtonSize::Xs,
                        )
                        .label(action_label)
                        .disabled(connected || adding || !online)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let target = button_env.clone();
                            this.state.update(cx, |state, cx| {
                                state.connect_cloud_environment(target, false, cx);
                            });
                        })),
                    )
            }))
            .into_any_element()
    }
}

impl Render for ConnectOnboardingDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let can_publish = can_publish_primary(self.state.read(cx));
        let step = if self.step == OnboardingStep::Publish && !can_publish {
            OnboardingStep::Devices
        } else {
            self.step
        };

        let steps_bar = can_publish.then(|| {
            let pill = |target: OnboardingStep, number: &'static str, label: &'static str| {
                let active = step == target;
                h_flex()
                    .id(("connect-onboarding-step", target as usize))
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .when(active, |el| el.bg(hsla(p.accent)))
                    .when(!self.applying, |el| el.cursor_pointer())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.applying {
                            this.step = target;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if active { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                            .child(format!("{number}. {label}")),
                    )
            };
            h_flex()
                .gap(px(6.))
                .pt(px(4.))
                .child(pill(OnboardingStep::Publish, "1", "Publish"))
                .child(pill(OnboardingStep::Devices, "2", "Connect devices"))
        });

        let body = match step {
            OnboardingStep::Publish => self.render_publish_step(cx),
            OnboardingStep::Devices => self.render_devices_step(cx),
        };

        let footer_buttons = match step {
            OnboardingStep::Publish => h_flex()
                .gap(px(8.))
                .child(
                    Button::new("connect-onboarding-not-now", ButtonVariant::Ghost, ButtonSize::Sm)
                        .label("Not now")
                        .disabled(self.applying)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.step = OnboardingStep::Devices;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("connect-onboarding-continue", ButtonVariant::Default, ButtonSize::Sm)
                        .label(if self.applying { "Enabling…" } else { "Continue" })
                        .disabled(self.applying)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.apply_publish_selection(window, cx);
                        })),
                )
                .into_any_element(),
            OnboardingStep::Devices => Button::new("connect-onboarding-done", ButtonVariant::Default, ButtonSize::Sm)
                .label("Done")
                .disabled(self.applying)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.complete(cx);
                }))
                .into_any_element(),
        };

        let card = v_flex()
            .id("connect-onboarding-dialog")
            .occlude()
            .relative()
            .track_focus(&self.focus)
            .w(px(540.))
            .max_w(px(540.))
            .rounded(px(16.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.popover))
            .text_color(hsla(p.popover_foreground))
            .shadow(floating_shadow(dark))
            .theme_slot(ThemeSlot::DialogPopup, cx)
            .children(theme_slot_clipped_background_layer(ThemeSlot::DialogPopup, px(16.), cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .p(px(24.))
                    .gap(px(8.))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hsla(p.popover_foreground))
                            .child("Set up T3 Connect"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(hsla(p.muted_foreground))
                            .child(
                                "Mesh your devices together. Publish this environment and connect the rest, all in one place.",
                            ),
                    )
                    .children(steps_bar),
            )
            .child(div().px(px(24.)).pb(px(24.)).child(body))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .px(px(24.))
                    .py(px(16.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .rounded_b(px(15.))
                    .bg(hsla(p.muted))
                    .child(
                        h_flex()
                            .id("connect-onboarding-opt-out")
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dont_show_again = !this.dont_show_again;
                                cx.notify();
                            }))
                            .child(checkbox(self.dont_show_again, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child("Don't show this again"),
                            ),
                    )
                    .child(footer_buttons),
            );

        deferred(
            div()
                .id("connect-onboarding-backdrop")
                .occlude()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_hsla(0., 0., 0., 0.55))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.complete(cx);
                }))
                .child(card),
        )
        .with_priority(125)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn onboarding_opens_only_on_fresh_sign_in_and_closes_on_sign_out_or_escape(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let (dialog, vcx) = cx.add_window_view(|window, cx| ConnectOnboardingDialog::new(state.clone(), window, cx));

        // 1. Existing startup / signed-out state stays closed.
        assert!(!dialog.read_with(vcx, |d, _| d.is_open()));

        // 2. Startup restore without `pending_onboarding` does not open the wizard.
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.cloud.identity = Some("alice@example.com".into());
                state.cloud.has_clerk_session = true;
                cx.emit(AppEvent::Phase);
            });
        });
        assert!(!dialog.read_with(vcx, |d, _| d.is_open()));

        // 3. Sign out clears identity, then fresh sign-in sets `pending_onboarding` and opens the wizard.
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.cloud.identity = None;
                state.cloud.has_clerk_session = false;
                cx.emit(AppEvent::Phase);
                state.cloud.identity = Some("alice@example.com".into());
                state.cloud.has_clerk_session = true;
                state.cloud.pending_onboarding = true;
                cx.emit(AppEvent::Phase);
            });
        });
        assert!(dialog.read_with(vcx, |d, _| d.is_open()));

        // Escape dismisses cleanly.
        assert!(dialog.update(vcx, |d, cx| d.handle_escape(cx)));
        assert!(!dialog.read_with(vcx, |d, _| d.is_open()));

        // Signing out while open dismisses the wizard immediately.
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.cloud.pending_onboarding = true;
                cx.emit(AppEvent::Phase);
            });
        });
        assert!(dialog.read_with(vcx, |d, _| d.is_open()));
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.cloud.identity = None;
                state.cloud.has_clerk_session = false;
                cx.emit(AppEvent::Phase);
            });
        });
        assert!(!dialog.read_with(vcx, |d, _| d.is_open()));
    }
}
