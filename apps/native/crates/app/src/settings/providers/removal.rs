//! In-app confirmation for `ProviderSettingsPanel.tsx`'s remove/reset actions.

use gpui::{
    AnyElement, App, Context, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Window, div,
    prelude::*,
};
use t3_client::supervisor::ConnectionStatus;
#[cfg(not(feature = "snapshot"))]
use t3_core::commands::method;
#[cfg(not(feature = "snapshot"))]
use t3_core::settings::providers as model;
use t3_core::settings::providers::removal::{RemovalContext, RemovalPlan, RemovalState};

use super::super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, DialogFrame, dialog_footer},
};
#[cfg(not(feature = "snapshot"))]
use super::failure_text;
use crate::app_state::Phase;

pub(super) struct Removal {
    pub(super) model: RemovalState,
    pub(super) action_focus: FocusHandle,
    focus: FocusHandle,
    cancel_focus: FocusHandle,
    confirm_focus: FocusHandle,
    #[cfg(feature = "snapshot")]
    fixture_denied: bool,
}

impl Removal {
    pub(super) fn new(cx: &mut Context<SettingsView>) -> Self {
        Self {
            model: RemovalState::default(),
            action_focus: cx.focus_handle().tab_stop(true),
            focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle().tab_stop(true),
            confirm_focus: cx.focus_handle().tab_stop(true),
            #[cfg(feature = "snapshot")]
            fixture_denied: false,
        }
    }
}

impl SettingsView {
    pub(in crate::settings) fn provider_removal_open(&self) -> bool {
        self.providers.removal.model.confirmation().is_some()
    }

    pub(super) fn removal_allowed(&self, cx: &App) -> bool {
        #[cfg(feature = "snapshot")]
        if self.providers.removal.fixture_denied {
            return false;
        }
        matches!(
            self.state.read(cx).phase,
            Phase::Online(ConnectionStatus::Connected)
        ) && self
            .session(cx)
            .is_some_and(|session| session.grants(t3_core::usage::PROVIDERS_MANAGE_SCOPE))
    }

    pub(super) fn request_provider_removal(
        &mut self,
        instance_id: &str,
        reset: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let allowed = self.removal_allowed(cx);
        let app = self.state.read(cx);
        let context = RemovalContext {
            connection: app.connection_id(),
            revision: app.server_config.settings_revision(),
            settings: app.server_config.config.as_ref().map(|c| &c.settings),
            allowed,
        };
        if self
            .providers
            .removal
            .model
            .request(instance_id, reset, &context)
        {
            #[cfg(feature = "snapshot")]
            audit("open", self.providers.removal.model.confirmation(), false);
            self.providers.removal.cancel_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(in crate::settings) fn dismiss_provider_removal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.provider_removal_open() {
            return false;
        }
        let target = self
            .providers
            .removal
            .model
            .confirmation()
            .map(|plan| plan.instance_id.clone());
        #[cfg(feature = "snapshot")]
        let cancelled = self.providers.removal.model.confirmation().cloned();
        self.providers.removal.model.cancel();
        self.restore_provider_removal_focus(target.as_deref(), window, cx);
        #[cfg(feature = "snapshot")]
        audit(
            "cancel",
            cancelled.as_ref(),
            self.focus.contains_focused(window, cx),
        );
        cx.notify();
        true
    }

    fn restore_provider_removal_focus(
        &self,
        target: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let trigger_available = !self.providers.removal.model.busy()
            && self.removal_allowed(cx)
            && self
                .page_data(cx)
                .and_then(|page| page.selected)
                .is_some_and(|selected| {
                    Some(selected.row.instance_id.as_str()) == target
                        && (!selected.row.is_default || selected.row.is_dirty)
                })
            && self
                .focus
                .contains(&self.providers.removal.action_focus, window);
        if trigger_available {
            self.providers.removal.action_focus.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }

    pub(in crate::settings) fn cycle_provider_removal_focus(
        &self,
        _forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.provider_removal_open() {
            return false;
        }
        let removal = &self.providers.removal;
        if removal.cancel_focus.is_focused(window) {
            removal.confirm_focus.focus(window, cx);
        } else {
            removal.cancel_focus.focus(window, cx);
        }
        true
    }

    pub(super) fn sync_provider_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let allowed = self.removal_allowed(cx);
        let app = self.state.read(cx);
        let context = RemovalContext {
            connection: app.connection_id(),
            revision: app.server_config.settings_revision(),
            settings: app.server_config.config.as_ref().map(|c| &c.settings),
            allowed,
        };
        if self.providers.removal.model.stale(&context) {
            self.dismiss_provider_removal(window, cx);
        }
    }

    fn confirm_provider_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let allowed = self.removal_allowed(cx);
        let app = self.state.read(cx);
        let context = RemovalContext {
            connection: app.connection_id(),
            revision: app.server_config.settings_revision(),
            settings: app.server_config.config.as_ref().map(|c| &c.settings),
            allowed,
        };
        let Some(plan) = self.providers.removal.model.confirm(&context) else {
            self.restore_provider_removal_focus(None, window, cx);
            cx.notify();
            return;
        };
        self.restore_provider_removal_focus(None, window, cx);
        cx.notify();
        // Snapshot confirmation records the plan before constructing any store or RPC future.
        #[cfg(feature = "snapshot")]
        {
            audit(
                "confirmed-before-rpc",
                Some(&plan),
                self.focus.contains_focused(window, cx),
            );
            match std::env::var("T3_NATIVE_PROVIDER_REMOVAL_LIFECYCLE").as_deref() {
                Ok("pending") => return,
                Ok("success") => self.project_removal_fixture(&plan.instance_id, cx),
                _ => {}
            }
            self.providers.removal.model.finish(&plan);
            if std::env::var_os("T3_NATIVE_PROVIDER_REMOVAL_FAILURE").is_some() {
                self.providers.notice =
                    Some((plan.failure_title(), "Synthetic settings failure.".into()));
            }
            return;
        }
        #[cfg(not(feature = "snapshot"))]
        self.send_provider_removal(plan, cx);
    }

    #[cfg(not(feature = "snapshot"))]
    fn send_provider_removal(&mut self, plan: RemovalPlan, cx: &mut Context<Self>) {
        let app = self.state.read(cx);
        let task = app.rpc(method::SERVER_UPDATE_SETTINGS, plan.payload());
        let cleanup = plan.managed_agent().map(|agent| {
            app.rpc(
                model::SERVER_UNINSTALL_ACP_REGISTRY_BINARY,
                model::uninstall_acp_payload(&agent),
            )
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let should_clean = this
                .update(cx, |this, cx| {
                    if this.state.read(cx).connection_id() != Some(plan.connection) {
                        return false;
                    }
                    match result {
                        Ok(reply) => {
                            this.apply_settings_reply(reply, plan.revision, cx);
                            this.removal_allowed(cx)
                        }
                        Err(error) => {
                            this.providers.notice = Some((
                                plan.failure_title(),
                                failure_text(&error, "The settings update failed."),
                            ));
                            cx.notify();
                            false
                        }
                    }
                })
                .unwrap_or(false);
            if should_clean
                && let Some(cleanup) = cleanup
                && let Err(error) = cleanup.await
            {
                let _ = this.update(cx, |this, cx| {
                    if this.state.read(cx).connection_id() != Some(plan.connection) {
                        return;
                    }
                    this.providers.notice = Some((
                        "Provider deleted, but managed files remain",
                        failure_text(&error, "Managed binary cleanup failed."),
                    ));
                    cx.notify();
                });
            }
            let _ = this.update(cx, |this, cx| {
                if this.providers.removal.model.finish(&plan) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    #[cfg(feature = "snapshot")]
    fn project_removal_fixture(&mut self, id: &str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.settings.provider_instances = config
                    .settings
                    .provider_instances
                    .iter()
                    .filter(|(key, _)| *key != id)
                    .map(|(key, instance)| (key.to_owned(), instance.clone()))
                    .collect();
            }
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
    }

    #[cfg(feature = "snapshot")]
    fn stale_removal_fixture(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(id) = self
            .providers
            .removal
            .model
            .confirmation()
            .map(|plan| plan.instance_id.clone())
        else {
            return false;
        };
        match std::env::var("T3_NATIVE_PROVIDER_REMOVAL_LIFECYCLE").as_deref() {
            Ok("stale-remove") => self.project_removal_fixture(&id, cx),
            Ok("stale-deny") => self.providers.removal.fixture_denied = true,
            _ => return false,
        }
        cx.notify();
        true
    }

    #[cfg(feature = "snapshot")]
    pub(in crate::settings) fn removal_route_audit(&self, event: &str, window: &Window, cx: &App) {
        if std::env::var_os("T3_NATIVE_PROVIDER_REMOVAL_LIFECYCLE").is_none() {
            return;
        }
        let selected = self.page_data(cx).and_then(|page| page.selected);
        audit_record(
            serde_json::json!({"event": event, "focusWithinSettings": self.focus.contains_focused(window, cx),
            "busy": self.providers.removal.model.busy(), "allowed": self.removal_allowed(cx),
            "selected": selected.as_ref().map(|selected| &selected.row.instance_id),
            "dirty": selected.as_ref().map(|selected| selected.row.is_dirty)}),
        );
    }

    pub(super) fn render_provider_removal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let plan = self.providers.removal.model.confirmation()?;
        let cancel = Button::new(
            "provider-removal-cancel",
            ButtonVariant::Outline,
            ButtonSize::Default,
        )
        .focus_handle(self.providers.removal.cancel_focus.clone())
        .label("Cancel")
        .on_click(cx.listener(|this, _, window, cx| {
            this.dismiss_provider_removal(window, cx);
        }))
        .into_any_element();
        let confirm = Button::new(
            "provider-removal-confirm",
            ButtonVariant::DestructiveOutline,
            ButtonSize::Default,
        )
        .focus_handle(self.providers.removal.confirm_focus.clone())
        .label(if plan.reset { "Reset" } else { "Remove" })
        .on_click(cx.listener(|this, _, window, cx| this.confirm_provider_removal(window, cx)))
        .into_any_element();
        let dismiss = cx.listener(|this, _: &(), window, cx| {
            this.dismiss_provider_removal(window, cx);
        });
        let frame = DialogFrame {
            id: "provider-removal",
            title: plan.question().into(),
            description: plan.warning().into(),
            max_width: 512.,
            body: div().into_any_element(),
            footer: dialog_footer(false, vec![cancel, confirm], cx).into_any_element(),
            focus: self.providers.removal.focus.clone(),
        }
        .render_modal(window, 4, move |window, cx| dismiss(&(), window, cx), cx);
        Some(
            div()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    #[cfg(feature = "snapshot")]
                    if event.keystroke.key == "f8" && this.stale_removal_fixture(cx) {
                        return;
                    }
                    if event.keystroke.modifiers.modified()
                        || !matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        return;
                    }
                    if this.providers.removal.cancel_focus.is_focused(window) {
                        cx.stop_propagation();
                        this.dismiss_provider_removal(window, cx);
                    } else if this.providers.removal.confirm_focus.is_focused(window) {
                        cx.stop_propagation();
                        this.confirm_provider_removal(window, cx);
                    }
                }))
                .child(frame)
                .into_any_element(),
        )
    }
}

#[cfg(feature = "snapshot")]
fn audit(event: &str, plan: Option<&RemovalPlan>, focus_restored: bool) {
    audit_record(
        serde_json::json!({"event":event,"instanceId":plan.map(|p| &p.instance_id),"connection":plan.map(|p| p.connection),"payload":plan.map(RemovalPlan::payload),"focusRestored":focus_restored}),
    );
}

#[cfg(feature = "snapshot")]
fn audit_record(record: serde_json::Value) {
    use std::io::Write;
    let Some(path) = std::env::var_os("T3_NATIVE_PROVIDER_REMOVAL_AUDIT") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{record}");
    }
}
