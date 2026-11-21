//! Usage providers settings (`UsageProviderSettings.tsx`, `AddUsageLimitSourceDialog.tsx`,
//! `AddUsageLimitSourceDialog.logic.ts`, and `SecretSourceInput.tsx`, `pingdotgg/t3code#27760`).
//! Manages CLIProxyAPI usage limit hubs with literal or `SecretReference` management keys,
//! reference validation, on-demand `server.checkSecretSource` checks, and stale-draft clearing
//! across environment/connection switches.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, FontWeight, IntoElement, ParentElement, SharedString, Styled, Subscription,
    Window, div, prelude::*,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::commands::method::SERVER_UPDATE_SETTINGS;
use t3_core::device::SETTINGS_WRITE_SCOPE;
use t3_core::settings::providers::form::SecretSourceDraft;
use t3_core::settings::secrets;
use t3_core::usage::PROVIDERS_MANAGE_SCOPE;
use t3_core::usage::limits::sources::{
    ManagementKeyValue, decode_usage_limit_sources, remove_usage_limit_source_patch, source_id_from_url, upsert_usage_limit_source_patch,
};

use super::super::controls::{
    Button, ButtonSize, ButtonVariant, DialogFrame, Row, Select, SelectOption, dialog_footer, group, r, section_title,
};
use super::super::SettingsView;
use super::CloseProviderDialog;
use crate::app_state::AppEvent;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const STORED_SECRET: &str = "__stored__";

#[derive(Clone, PartialEq, Eq)]
struct UsageSourceOwner {
    scoped_saved_id: Option<String>,
    environment: String,
    connection: Option<u64>,
}

impl UsageSourceOwner {
    fn capture(view: &SettingsView, cx: &App) -> Option<Self> {
        let config = view.scoped_raw_representative_config(cx)?;
        let state = view.state.read(cx);
        let scoped_saved_id = view.scoped_primary_owner(cx).map(|owner| owner.saved_id);
        let is_active = scoped_saved_id
            .as_deref()
            .is_none_or(|id| state.environments.active.as_deref() == Some(id));
        Some(Self {
            scoped_saved_id,
            environment: config.environment.environment_id.clone(),
            connection: is_active.then(|| state.connection_id()).flatten(),
        })
    }

    fn current(&self, view: &SettingsView, cx: &App) -> bool {
        Self::capture(view, cx).as_ref() == Some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SecretCheckStatus {
    Checking,
    Resolved,
    Failed(String),
}

struct AddUsageSourceDialog {
    owner: UsageSourceOwner,
    generation: u64,
    environment_label: String,
    url: Entity<InputState>,
    key: Entity<InputState>,
    label: Entity<InputState>,
    source: SecretSourceDraft,
    /// Mirrors `{ "value": "" | { "provider": ..., "reference": ... } }` for `SecretSourceDraft`.
    row: Value,
    rejected_ref_error: Option<String>,
    secret_checks: HashMap<(String, String), SecretCheckStatus>,
    saving: bool,
    error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

struct RemoveUsageSourceDialog {
    owner: UsageSourceOwner,
    id: String,
    label: String,
    focus: FocusHandle,
}

#[derive(Default)]
pub(super) struct UsageSourcesState {
    add: Option<AddUsageSourceDialog>,
    remove: Option<RemoveUsageSourceDialog>,
    generation: u64,
}

impl UsageSourcesState {
    pub(super) fn clear(&mut self) {
        self.generation += 1;
        self.add = None;
        self.remove = None;
    }

    pub(super) fn inputs(&self) -> Vec<&Entity<InputState>> {
        let Some(dialog) = &self.add else {
            return Vec::new();
        };
        vec![&dialog.url, &dialog.key, &dialog.label]
    }
}

impl SettingsView {
    fn usage_sources_may(&self, scope: &str, cx: &App) -> bool {
        self.scoped_session(cx).is_none_or(|session| session.grants(scope))
    }

    fn usage_sources_read_only(&self, cx: &App) -> bool {
        !self.scoped_connected_now(cx)
            || self.is_project_scope(cx)
            || !self.usage_sources_may(PROVIDERS_MANAGE_SCOPE, cx)
    }

    /// Drops stale Add/Remove dialogs when the environment, connection, or write access changes.
    pub(super) fn sync_usage_sources(&mut self, cx: &App) {
        let read_only = self.usage_sources_read_only(cx);
        if self.providers.usage_sources.add.as_ref().is_some_and(|d| read_only || !d.owner.current(self, cx)) {
            self.providers.usage_sources.generation += 1;
            self.providers.usage_sources.add = None;
        }
        if self.providers.usage_sources.remove.as_ref().is_some_and(|d| read_only || !d.owner.current(self, cx)) {
            self.providers.usage_sources.remove = None;
        }
    }

    pub(super) fn close_usage_source_dialogs(&mut self, cx: &mut Context<Self>) {
        if self.providers.usage_sources.add.as_ref().is_some_and(|d| d.saving) {
            return;
        }
        if self.providers.usage_sources.add.is_some() || self.providers.usage_sources.remove.is_some() {
            self.providers.usage_sources.clear();
            cx.notify();
        }
    }

    pub(super) fn open_add_usage_source_dialog(&mut self, environment_label: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.usage_sources_read_only(cx) {
            return;
        }
        let Some(owner) = UsageSourceOwner::capture(self, cx) else {
            return;
        };
        self.providers.usage_sources.generation += 1;
        let generation = self.providers.usage_sources.generation;
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://hub.example.ts.net:8318"));
        let key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("Defaults to the hub's host name"));
        url.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&url, window, move |this, _, event: &InputEvent, _, cx| {
                if this.providers.usage_sources.add.as_ref().is_none_or(|d| d.generation != generation) {
                    return;
                }
                match event {
                    InputEvent::PressEnter { .. } => this.save_usage_source(cx),
                    InputEvent::Change => {
                        if let Some(d) = this.providers.usage_sources.add.as_mut() {
                            d.error = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&key, window, move |this, _, event: &InputEvent, _, cx| {
                if this.providers.usage_sources.add.as_ref().is_none_or(|d| d.generation != generation) {
                    return;
                }
                match event {
                    InputEvent::PressEnter { .. } => {
                        this.commit_usage_source_key(cx);
                        this.save_usage_source(cx);
                    }
                    InputEvent::Blur => {
                        this.commit_usage_source_key(cx);
                    }
                    InputEvent::Change => {
                        this.on_usage_source_key_change(cx);
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&label, window, move |this, _, event: &InputEvent, _, cx| {
                if this.providers.usage_sources.add.as_ref().is_none_or(|d| d.generation != generation) {
                    return;
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_usage_source(cx);
                }
            }),
        ];
        self.providers.usage_sources.remove = None;
        self.providers.usage_sources.add = Some(AddUsageSourceDialog {
            owner,
            generation,
            environment_label,
            url,
            key,
            label,
            source: SecretSourceDraft::default(),
            row: json!({ "value": "" }),
            rejected_ref_error: None,
            secret_checks: HashMap::new(),
            saving: false,
            error: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn usage_source_is_one_password(&self, provider: &str, cx: &App) -> bool {
        self.scoped_raw_representative_config(cx).is_some_and(|c| {
            c.settings
                .secret_providers
                .get(provider)
                .is_some_and(|p| p.kind == secrets::ONE_PASSWORD)
        })
    }

    fn select_usage_source_secret_provider(&mut self, next_provider: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let one_password = next_provider
            .as_deref()
            .is_some_and(|provider| self.usage_source_is_one_password(provider, cx));
        let Some(dialog) = self.providers.usage_sources.add.as_mut() else {
            return;
        };
        let stored = next_provider.is_none();
        dialog.source.select(&mut dialog.row, next_provider);
        dialog.rejected_ref_error = None;
        dialog.error = None;
        let value = dialog.source.input_value(&dialog.row).to_owned();
        let placeholder = if stored {
            ""
        } else if one_password {
            "op://vault/item/field"
        } else {
            "Secret reference"
        };
        dialog.key.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.set_masked(stored, window, cx);
            input.set_placeholder(placeholder, window, cx);
        });
        cx.notify();
    }

    fn on_usage_source_key_change(&mut self, cx: &mut Context<Self>) {
        let (provider, text) = {
            let Some(dialog) = self.providers.usage_sources.add.as_ref() else {
                return;
            };
            (dialog.source.provider(&dialog.row).map(str::to_owned), dialog.key.read(cx).value().to_string())
        };
        let one_password = provider.as_deref().is_some_and(|p| self.usage_source_is_one_password(p, cx));
        let Some(dialog) = self.providers.usage_sources.add.as_mut() else {
            return;
        };
        dialog.error = None;
        if provider.is_some() {
            if text.trim().is_empty() {
                dialog.rejected_ref_error = None;
                let _ = dialog.source.commit(&mut dialog.row, "", one_password);
            } else if dialog.source.validate_draft(&dialog.row, &text, one_password).is_ok() {
                dialog.rejected_ref_error = None;
                let _ = dialog.source.commit(&mut dialog.row, &text, one_password);
            }
        }
        cx.notify();
    }

    fn commit_usage_source_key(&mut self, cx: &mut Context<Self>) {
        let (provider, text) = {
            let Some(dialog) = self.providers.usage_sources.add.as_ref() else {
                return;
            };
            (dialog.source.provider(&dialog.row).map(str::to_owned), dialog.key.read(cx).value().to_string())
        };
        let Some(provider) = provider else {
            return;
        };
        let one_password = self.usage_source_is_one_password(&provider, cx);
        let Some(dialog) = self.providers.usage_sources.add.as_mut() else {
            return;
        };
        if text.trim().is_empty() {
            dialog.rejected_ref_error = None;
            let _ = dialog.source.commit(&mut dialog.row, "", one_password);
            cx.notify();
            return;
        }
        match dialog.source.commit(&mut dialog.row, &text, one_password) {
            Ok(_) => {
                dialog.rejected_ref_error = None;
            }
            Err(message) => {
                dialog.rejected_ref_error = Some(message.to_owned());
            }
        }
        cx.notify();
    }

    fn check_usage_source_secret(&mut self, provider: String, reference: String, cx: &mut Context<Self>) {
        if self
            .scoped_raw_representative_config(cx)
            .is_none_or(|c| c.environment.capabilities.secret_source_check != Some(true))
            || !self.usage_sources_may(SETTINGS_WRITE_SCOPE, cx)
        {
            return;
        }
        let Some(dialog) = self.providers.usage_sources.add.as_mut() else {
            return;
        };
        let key = (provider.clone(), reference.clone());
        if matches!(dialog.secret_checks.get(&key), Some(SecretCheckStatus::Checking)) {
            return;
        }
        let generation = dialog.generation;
        let owner = dialog.owner.clone();
        dialog.secret_checks.insert(key.clone(), SecretCheckStatus::Checking);
        let task = self.scoped_rpc(
            SETTINGS_WRITE_SCOPE,
            secrets::CHECK_SECRET_SOURCE,
            secrets::check_secret_source_payload(&provider, &reference),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let next = match result {
                Ok(reply) => match secrets::decode_check_secret_source_result(&reply) {
                    secrets::SecretSourceCheckResult::Resolved => SecretCheckStatus::Resolved,
                    secrets::SecretSourceCheckResult::Failed(detail) => SecretCheckStatus::Failed(detail),
                },
                Err(_) => SecretCheckStatus::Failed("Could not reach the server".to_owned()),
            };
            let _ = this.update(cx, |this, cx| {
                let current = owner.current(this, cx);
                let Some(dialog) = this.providers.usage_sources.add.as_mut() else {
                    return;
                };
                if !current || dialog.generation != generation {
                    return;
                }
                dialog.secret_checks.insert(key, next);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dispatch_usage_source_patch(
        &self,
        payload: Value,
        on_done: impl FnOnce(Option<String>, &mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let active_saved = self.state.read(cx).environments.active.clone();
        let is_active = self
            .scoped_primary_owner(cx)
            .is_none_or(|owner| active_saved.as_deref() == Some(owner.saved_id.as_str()));
        let sent_at = self.state.read(cx).server_config.settings_revision();
        let task = self.scoped_rpc(PROVIDERS_MANAGE_SCOPE, SERVER_UPDATE_SETTINGS, payload, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(reply) => {
                    if is_active {
                        let candidate = reply.get("settings").unwrap_or(&reply);
                        if let Ok(settings) = serde_json::from_value::<t3_core::server_config::ServerSettings>(candidate.clone()) {
                            this.state.update(cx, |state, cx| {
                                if state.server_config.apply_settings_reply(settings, sent_at) {
                                    cx.emit(AppEvent::ServerConfig);
                                }
                            });
                        }
                    }
                    on_done(None, this, cx);
                }
                Err(error) => {
                    on_done(Some(error.to_string()), this, cx);
                }
            });
        })
        .detach();
    }

    fn usage_source_management_key(&self, dialog: &AddUsageSourceDialog, cx: &App) -> Option<ManagementKeyValue> {
        let text = dialog.key.read(cx).value().to_string();
        if let Some(provider) = dialog.source.provider(&dialog.row) {
            if dialog.rejected_ref_error.is_some() {
                return None;
            }
            let one_password = self.usage_source_is_one_password(provider, cx);
            if text.trim().is_empty() || dialog.source.validate_draft(&dialog.row, &text, one_password).is_err() {
                return None;
            }
            Some(ManagementKeyValue::Reference {
                provider: provider.to_owned(),
                reference: text.trim().to_owned(),
            })
        } else {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| ManagementKeyValue::Literal(trimmed.to_owned()))
        }
    }

    fn save_usage_source(&mut self, cx: &mut Context<Self>) {
        if self.usage_sources_read_only(cx) {
            return;
        }
        let (owner, generation, trimmed_url, trimmed_label, management_key) = {
            let Some(dialog) = self.providers.usage_sources.add.as_ref() else {
                return;
            };
            if dialog.saving || !dialog.owner.current(self, cx) {
                return;
            }
            let trimmed_url = dialog.url.read(cx).value().trim().to_owned();
            if trimmed_url.is_empty() {
                return;
            }
            let Some(management_key) = self.usage_source_management_key(dialog, cx) else {
                return;
            };
            let trimmed_label = dialog.label.read(cx).value().trim().to_owned();
            (
                dialog.owner.clone(),
                dialog.generation,
                trimmed_url,
                (!trimmed_label.is_empty()).then_some(trimmed_label),
                management_key,
            )
        };
        let existing_sources = self
            .scoped_raw_representative_config(cx)
            .and_then(|c| c.settings.usage_limit_sources.as_object().cloned())
            .unwrap_or_default();
        let id = source_id_from_url(&trimmed_url, &existing_sources);
        let payload = upsert_usage_limit_source_patch(&id, &trimmed_url, trimmed_label.as_deref(), &management_key);
        if let Some(dialog) = self.providers.usage_sources.add.as_mut() {
            dialog.saving = true;
            dialog.error = None;
        }
        cx.notify();
        self.dispatch_usage_source_patch(
            payload,
            move |error, this, cx| {
                let current = owner.current(this, cx);
                let Some(dialog) = this.providers.usage_sources.add.as_mut() else {
                    return;
                };
                if !current || dialog.generation != generation {
                    this.providers.usage_sources.add = None;
                    cx.notify();
                    return;
                }
                dialog.saving = false;
                match error {
                    None => {
                        this.providers.usage_sources.add = None;
                    }
                    Some(message) => {
                        dialog.error = Some(message);
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    fn open_remove_usage_source_dialog(&mut self, id: String, label: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.usage_sources_read_only(cx) {
            return;
        }
        let Some(owner) = UsageSourceOwner::capture(self, cx) else {
            return;
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.providers.usage_sources.remove = Some(RemoveUsageSourceDialog { owner, id, label, focus });
        cx.notify();
    }

    fn confirm_remove_usage_source(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.providers.usage_sources.remove.take() else {
            return;
        };
        if self.usage_sources_read_only(cx) || !dialog.owner.current(self, cx) {
            cx.notify();
            return;
        }
        let payload = remove_usage_limit_source_patch(&dialog.id);
        self.dispatch_usage_source_patch(
            payload,
            |error, this, cx| {
                if let Some(message) = error {
                    this.state.update(cx, |state, cx| {
                        state.show_toast(
                            crate::toast::Toast::new(
                                crate::toast::ToastKind::Error,
                                format!("Could not remove usage provider: {message}"),
                            ),
                            cx,
                        );
                    });
                }
            },
            cx,
        );
        cx.notify();
    }

    /// `UsageProviderSettings` (`apps/web/src/components/settings/UsageProviderSettings.tsx`).
    pub(super) fn render_usage_sources_section(&mut self, environment_label: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let read_only = self.usage_sources_read_only(cx);
        let sources = self
            .scoped_raw_representative_config(cx)
            .map(|c| decode_usage_limit_sources(&c.settings.usage_limit_sources))
            .unwrap_or_default();
        let env_label_for_add = environment_label.to_owned();
        let header = h_flex()
            .min_h(r(28.))
            .px(r(16.))
            .gap(r(8.))
            .child(div().flex_1().child(section_title("Usage providers")))
            .when(!read_only, |this| {
                this.child(
                    Button::new("usage-providers-add", ButtonVariant::Outline, ButtonSize::Compact)
                        .icon(Icon::Plus)
                        .label("Add hub")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_add_usage_source_dialog(env_label_for_add.clone(), window, cx);
                        })),
                )
            });

        let rows: Vec<AnyElement> = if sources.is_empty() {
            vec![Row::new("usage-providers-empty", "No hubs configured.").highlighted(self.is_target("usage-providers")).into_any_element()]
        } else {
            sources
                .into_iter()
                .enumerate()
                .map(|(index, source)| {
                    let display_label = source.display_label().to_owned();
                    let description = source.description();
                    let remove_id = source.id.clone();
                    let remove_label = display_label.clone();
                    let mut row = Row::new(SharedString::from(format!("usage-provider-{}", source.id)), display_label)
                        .description(description)
                        .highlighted(self.is_target("usage-providers"));
                    if !read_only {
                        row = row.control(
                            Button::new(("usage-provider-remove", index), ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .label("Remove")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_remove_usage_source_dialog(remove_id.clone(), remove_label.clone(), window, cx);
                                })),
                        );
                    }
                    row.into_any_element()
                })
                .collect()
        };

        v_flex()
            .id("usage-providers-section")
            .gap(r(8.))
            .text_color(hsla(p.foreground))
            .child(header)
            .child(group(rows, cx))
            .into_any_element()
    }

    /// `AddUsageLimitSourceDialog` (`apps/web/src/components/settings/AddUsageLimitSourceDialog.tsx`).
    pub(super) fn render_usage_source_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.providers.usage_sources.add.as_ref()?;
        let p = palette(cx);
        let url_input = dialog.url.clone();
        let key_input = dialog.key.clone();
        let label_input = dialog.label.clone();
        let saving = dialog.saving;
        let error = dialog.error.clone();
        let rejected_ref_error = dialog.rejected_ref_error.clone();
        let active_provider = dialog.source.provider(&dialog.row).map(str::to_owned);
        let key_text = key_input.read(cx).value().to_string();
        let env_label = dialog.environment_label.clone();
        let focus = dialog.focus.clone();
        let trimmed_url = url_input.read(cx).value().trim().to_owned();
        let management_key = self.usage_source_management_key(dialog, cx);
        let can_save = !saving && !trimmed_url.is_empty() && management_key.is_some();

        let secret_select = self.usage_source_secret_select(active_provider.as_deref(), saving, cx);
        let secret_check = active_provider.as_deref().and_then(|provider| {
            let committed_provider = dialog.row["value"]["provider"].as_str()?;
            let committed_ref = dialog.row["value"]["reference"].as_str()?;
            (committed_provider == provider && committed_ref == key_text.trim() && rejected_ref_error.is_none())
                .then(|| self.usage_source_secret_check_button(provider, committed_ref, saving, cx))
                .flatten()
        });

        let field_label = |text: &'static str| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(text)
        };

        let key_control = v_flex()
            .gap(r(4.))
            .child(
                h_flex()
                    .gap(r(6.))
                    .items_center()
                    .children(secret_select)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(active_provider.is_some(), |this| this.font_family(cx.theme().mono_font_family.clone()))
                            .child(Input::new(&key_input).small().disabled(saving)),
                    )
                    .children(secret_check),
            )
            .children(rejected_ref_error.map(|msg| {
                div()
                    .id("usage-source-ref-error")
                    .text_size(r(11.))
                    .text_color(hsla(p.destructive))
                    .child(msg)
            }));

        let body = v_flex()
            .key_context("ProviderDialog")
            .on_action(cx.listener(|this, _: &CloseProviderDialog, _, cx| this.close_usage_source_dialogs(cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                // Enter anywhere in the Add CLIProxyAPI hub dialog submits (`pingdotgg/t3code#36755`).
                if event.keystroke.key == "enter" && !event.keystroke.modifiers.shift {
                    this.commit_usage_source_key(cx);
                    this.save_usage_source(cx);
                    cx.stop_propagation();
                }
            }))
            .gap(r(16.))
            .child(v_flex().gap(r(6.)).child(field_label("Hub URL")).child(Input::new(&url_input).small().disabled(saving)))
            .child(v_flex().gap(r(6.)).child(field_label("Management key")).child(key_control))
            .child(v_flex().gap(r(6.)).child(field_label("Label (optional)")).child(Input::new(&label_input).small().disabled(saving)))
            .children(error.map(|msg| div().text_xs().text_color(hsla(p.destructive)).child(msg)))
            .into_any_element();

        let footer = dialog_footer(
            true,
            vec![
                Button::new("usage-source-cancel", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Cancel")
                    .disabled(saving)
                    .on_click(cx.listener(|this, _, _, cx| this.close_usage_source_dialogs(cx)))
                    .into_any_element(),
                Button::new("usage-source-save", ButtonVariant::Default, ButtonSize::Default)
                    .label("Add hub")
                    .disabled(!can_save)
                    .on_click(cx.listener(|this, _, _, cx| this.save_usage_source(cx)))
                    .into_any_element(),
            ],
            cx,
        )
        .into_any_element();

        let view = cx.entity().downgrade();
        Some(
            DialogFrame {
                id: "add-usage-limit-source-dialog",
                title: "Add a CLIProxyAPI hub".into(),
                description: format!(
                    "Show the quota of every account the hub pools, next to the providers on {env_label}. The key stays on that server."
                )
                .into(),
                max_width: 448.,
                body,
                footer,
                focus,
            }
            .render_modal(
                window,
                4,
                move |_, cx| {
                    let _ = view.update(cx, |this, cx| this.close_usage_source_dialogs(cx));
                },
                cx,
            ),
        )
    }

    /// `RemoveUsageProviderButton` confirm dialog (`UsageProviderSettings.tsx`).
    pub(super) fn render_usage_source_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.providers.usage_sources.remove.as_ref()?;
        let label = dialog.label.clone();
        let focus = dialog.focus.clone();
        let body = div()
            .key_context("ProviderDialog")
            .on_action(cx.listener(|this, _: &CloseProviderDialog, _, cx| this.close_usage_source_dialogs(cx)))
            .into_any_element();
        let footer = dialog_footer(
            false,
            vec![
                Button::new("usage-source-remove-cancel", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.close_usage_source_dialogs(cx)))
                    .into_any_element(),
                Button::new("usage-source-remove-confirm", ButtonVariant::DestructiveOutline, ButtonSize::Default)
                    .label("Remove hub")
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_remove_usage_source(cx)))
                    .into_any_element(),
            ],
            cx,
        )
        .into_any_element();
        let view = cx.entity().downgrade();
        Some(
            DialogFrame {
                id: "remove-usage-limit-source-dialog",
                title: format!("Remove {label}?").into(),
                description: "The hub's management key is deleted from this server. Its accounts leave the Limits view; the hub itself is untouched. Add it again with the URL and key to bring them back.".into(),
                max_width: 448.,
                body,
                footer,
                focus,
            }
            .render(
                window,
                move |_, cx| {
                    let _ = view.update(cx, |this, cx| this.close_usage_source_dialogs(cx));
                },
                cx,
            ),
        )
    }

    fn usage_source_secret_select(&self, current: Option<&str>, disabled: bool, cx: &Context<Self>) -> Option<AnyElement> {
        let config = self.scoped_raw_representative_config(cx)?;
        let mut sources = if config.secret_references {
            config
                .settings
                .secret_providers
                .0
                .iter()
                .filter(|p| !matches!(secrets::classify(p), secrets::Classified::Unsupported))
                .map(|p| (p.id.clone(), p.label.clone(), p.kind == secrets::ONE_PASSWORD))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if sources.is_empty() && current.is_none() {
            return None;
        }
        if let Some(source) = current
            && !sources.iter().any(|(id, _, _)| id == source)
        {
            sources.push((source.to_owned(), source.to_owned(), false));
        }
        let selected = current.unwrap_or(STORED_SECRET).to_owned();
        let options = std::iter::once(SelectOption::new(STORED_SECRET, "Value").icon(Icon::FileText))
            .chain(sources.into_iter().map(|(id, label, one_password)| {
                SelectOption::new(id, label).icon(if one_password { Icon::OnePassword } else { Icon::KeyRound })
            }))
            .collect();
        let view = cx.entity().downgrade();
        Some(
            Select::new("usage-source-key-source", options)
                .selected(selected)
                .label("")
                .disabled(disabled)
                .popup_width(192.)
                .on_change(move |source, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        let next = (source.as_ref() != STORED_SECRET).then(|| source.to_string());
                        this.select_usage_source_secret_provider(next, window, cx);
                    });
                })
                .into_any_element(),
        )
    }

    fn usage_source_secret_check_button(&self, provider: &str, reference: &str, disabled: bool, cx: &Context<Self>) -> Option<AnyElement> {
        if self
            .scoped_raw_representative_config(cx)
            .is_none_or(|c| c.environment.capabilities.secret_source_check != Some(true))
        {
            return None;
        }
        let p = palette(cx);
        let can_check = self.usage_sources_may(SETTINGS_WRITE_SCOPE, cx);
        let status = self
            .providers
            .usage_sources
            .add
            .as_ref()
            .and_then(|d| d.secret_checks.get(&(provider.to_owned(), reference.to_owned())));
        let checking = matches!(status, Some(SecretCheckStatus::Checking));
        let (glyph, color) = match status {
            Some(SecretCheckStatus::Resolved) => (Icon::Check, hsla(p.success_foreground)),
            Some(SecretCheckStatus::Failed(_)) => (Icon::TriangleAlert, hsla(p.destructive)),
            Some(SecretCheckStatus::Checking) | None => (Icon::RefreshCw, hsla(p.muted_foreground)),
        };
        let description: SharedString = if !can_check {
            "You cannot check secrets on this environment".into()
        } else {
            match status {
                None => "Check that the secret resolves".into(),
                Some(SecretCheckStatus::Checking) => "Checking the secret".into(),
                Some(SecretCheckStatus::Resolved) => "Secret resolved".into(),
                Some(SecretCheckStatus::Failed(detail)) => detail.clone().into(),
            }
        };
        let provider = provider.to_owned();
        let reference = reference.to_owned();
        Some(
            div()
                .flex_none()
                .debug_selector(|| "usage-source-secret-check".to_owned())
                .child(
                    Button::new("usage-source-secret-check", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                        .icon(glyph)
                        .icon_color(color)
                        .loading(checking)
                        .tooltip(description)
                        .disabled(disabled || !can_check || checking)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.check_usage_source_secret(provider.clone(), reference.clone(), cx);
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use serde_json::json;

    struct Host(Entity<SettingsView>);
    impl gpui::Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn install_settings_with_secrets(
        cx: &mut gpui::TestAppContext,
        runtime: &tokio::runtime::Runtime,
        sources_json: Value,
    ) -> (Entity<AppState>, t3_client::supervisor::ClientFixture, Entity<SettingsView>, gpui::WindowHandle<Host>) {
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.update(|cx| crate::settings::init(state.clone(), cx));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["settings:write", "providers:manage"]
                }))
                .unwrap(),
            ))
            .unwrap();
        state.update(cx, |state, cx| {
            let config: t3_core::server_config::ServerConfig = serde_json::from_value(json!({
                "environment": {
                    "environmentId": "env-a",
                    "label": "Work Mac",
                    "capabilities": {
                        "secretSourceCheck": true
                    }
                },
                "secretReferences": true,
                "settings": {
                    "secretProviders": {
                        "op-work": {
                            "kind": "one_password",
                            "label": "1Password Work"
                        }
                    },
                    "usageLimitSources": sources_json
                },
                "providers": []
            }))
            .unwrap();
            state.server_config.config = Some(config);
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
        let window = cx.add_window(|window, cx| {
            Host(cx.new(|cx| SettingsView::new(state.clone(), t3_core::settings::search::Section::Providers, None, window, cx)))
        });
        let view = window.read_with(cx, |host, _| host.0.clone()).unwrap();
        (state, fixture, view, window)
    }

    #[gpui::test]
    async fn add_secret_ref_usage_source_validates_checks_saves_and_drops_stale_drafts(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture, view, window) = install_settings_with_secrets(cx, &runtime, json!({}));

        // 1. Open Add hub dialog and select 1Password secret provider.
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |settings, cx| {
                    settings.open_add_usage_source_dialog("Work Mac".into(), window, cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    dialog.url.update(cx, |input, cx| input.set_value("https://hub.example.ts.net:8318", window, cx));
                    dialog.label.update(cx, |input, cx| input.set_value("Team Hub", window, cx));
                    settings.select_usage_source_secret_provider(Some("op-work".into()), window, cx);
                    // Invalid 1Password ref is rejected and blocks save.
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    dialog.key.update(cx, |input, cx| input.set_value("not-an-op-ref", window, cx));
                    settings.commit_usage_source_key(cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    assert!(dialog.rejected_ref_error.is_some());
                    assert!(settings.usage_source_management_key(dialog, cx).is_none());

                    // Valid 1Password ref clears error and enables check + save.
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    dialog.key.update(cx, |input, cx| input.set_value("op://Vault/Hub/key", window, cx));
                    settings.commit_usage_source_key(cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    assert_eq!(dialog.rejected_ref_error, None);
                    assert_eq!(
                        settings.usage_source_management_key(dialog, cx),
                        Some(ManagementKeyValue::Reference {
                            provider: "op-work".into(),
                            reference: "op://Vault/Hub/key".into(),
                        })
                    );
                    settings.check_usage_source_secret("op-work".into(), "op://Vault/Hub/key".into(), cx);
                });
            })
            .unwrap();
        cx.run_until_parked();

        // Answer server.checkSecretSource -> Resolved.
        let (check_tx, check_rx) = tokio::sync::oneshot::channel::<()>();
        let check_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(check_tx)));
        let _check_obs = cx.update(|cx| {
            let check_tx = check_tx.clone();
            cx.observe(&view, move |view, cx| {
                let resolved = view
                    .read(cx)
                    .providers
                    .usage_sources
                    .add
                    .as_ref()
                    .and_then(|d| d.secret_checks.get(&("op-work".into(), "op://Vault/Hub/key".into())))
                    == Some(&SecretCheckStatus::Resolved);
                if resolved
                    && let Some(tx) = check_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, secrets::CHECK_SECRET_SOURCE);
        assert_eq!(
            payload,
            json!({ "source": { "provider": "op-work", "reference": "op://Vault/Hub/key" } })
        );
        let _ = reply.send(Ok(json!({ "_tag": "Resolved" })));
        check_rx.await.expect("check resolved");
        cx.run_until_parked();

        // 2. Save sends server.updateSettings with SecretReference managementKey and closes dialog on success.
        let (save_tx, save_rx) = tokio::sync::oneshot::channel::<()>();
        let save_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(save_tx)));
        let _save_obs = cx.update(|cx| {
            let save_tx = save_tx.clone();
            cx.observe(&view, move |view, cx| {
                if view.read(cx).providers.usage_sources.add.is_none()
                    && let Some(tx) = save_tx.borrow_mut().take()
                {
                    let _ = tx.send(());
                }
            })
        });
        window.update(cx, |host, _, cx| host.0.update(cx, |settings, cx| settings.save_usage_source(cx))).unwrap();
        cx.run_until_parked();
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, "server.updateSettings");
        assert_eq!(
            payload,
            json!({
                "patch": {
                    "usageLimitSources": {
                        "cliproxy-hub.example.ts.net-8318": {
                            "kind": "cliproxy",
                            "label": "Team Hub",
                            "url": "https://hub.example.ts.net:8318",
                            "managementKey": {
                                "provider": "op-work",
                                "reference": "op://Vault/Hub/key"
                            },
                            "enabled": true
                        }
                    }
                }
            })
        );
        let _ = reply.send(Ok(json!({})));
        save_rx.await.expect("save dialog closed");
        cx.run_until_parked();
        view.read_with(cx, |settings, _| assert!(settings.providers.usage_sources.add.is_none()));

        // 3. Stale draft closes when switching environment.
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |settings, cx| {
                    settings.open_add_usage_source_dialog("Work Mac".into(), window, cx);
                    assert!(settings.providers.usage_sources.add.is_some());
                });
            })
            .unwrap();
        state.update(cx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.environment.environment_id = "env-b".into();
            }
            cx.emit(crate::app_state::AppEvent::ServerConfig);
        });
        window
            .update(cx, |host, _, cx| {
                host.0.update(cx, |settings, cx| {
                    settings.sync_usage_sources(cx);
                    assert!(settings.providers.usage_sources.add.is_none(), "stale draft must close on environment switch");
                });
            })
            .unwrap();

        // 4. Remove confirm sends `{ patch: { usageLimitSources: { [id]: null } } }`.
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |settings, cx| {
                    settings.open_remove_usage_source_dialog("cliproxy-hub.example.ts.net-8318".into(), "Team Hub".into(), window, cx);
                    assert!(settings.providers.usage_sources.remove.is_some());
                    settings.confirm_remove_usage_source(cx);
                });
            })
            .unwrap();
        cx.run_until_parked();
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, "server.updateSettings");
        assert_eq!(
            payload,
            json!({ "patch": { "usageLimitSources": { "cliproxy-hub.example.ts.net-8318": null } } })
        );
        let _ = reply.send(Ok(json!({})));
        cx.run_until_parked();

        state.update(cx, |state, _| state.stop_pump());
        runtime.shutdown_background();
    }

    #[gpui::test]
    async fn usage_sources_gate_on_providers_manage_distinct_from_settings_write(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture, view, window) = install_settings_with_secrets(cx, &runtime, json!({}));

        // 1. settings:write alone (no providers:manage) => read-only; Add/Remove blocked.
        fixture
            .grants
            .send(Some(serde_json::from_value(json!({ "authenticated": true, "permissions": ["settings:write"] })).unwrap()))
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |settings, cx| {
                    assert!(settings.usage_sources_read_only(cx));
                    settings.open_add_usage_source_dialog("Work Mac".into(), window, cx);
                    assert!(settings.providers.usage_sources.add.is_none());
                    settings.open_remove_usage_source_dialog("hub-1".into(), "Hub 1".into(), window, cx);
                    assert!(settings.providers.usage_sources.remove.is_none());
                });
            })
            .unwrap();

        // 2. providers:manage alone (no settings:write) => editable, secret check blocked, save succeeds.
        fixture
            .grants
            .send(Some(serde_json::from_value(json!({ "authenticated": true, "permissions": ["providers:manage"] })).unwrap()))
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |settings, cx| {
                    assert!(!settings.usage_sources_read_only(cx));
                    settings.open_add_usage_source_dialog("Work Mac".into(), window, cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    dialog.url.update(cx, |input, cx| input.set_value("https://hub.example.ts.net:8318", window, cx));
                    settings.select_usage_source_secret_provider(Some("op-work".into()), window, cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    dialog.key.update(cx, |input, cx| input.set_value("op://Vault/Hub/key", window, cx));
                    settings.commit_usage_source_key(cx);
                    // Secret check requires settings:write, so it must NOT fire.
                    settings.check_usage_source_secret("op-work".into(), "op://Vault/Hub/key".into(), cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    assert!(dialog.secret_checks.is_empty(), "checkSecretSource must stay gated on settings:write");
                    settings.save_usage_source(cx);
                });
            })
            .unwrap();
        cx.run_until_parked();
        let (method, payload, reply) = fixture.next_request().await;
        assert_eq!(method, "server.updateSettings");
        assert_eq!(
            payload["patch"]["usageLimitSources"]["cliproxy-hub.example.ts.net-8318"]["managementKey"],
            json!({ "provider": "op-work", "reference": "op://Vault/Hub/key" })
        );
        let _ = reply.send(Ok(json!({})));
        cx.run_until_parked();

        // 3. Non-active scoped environment with providers:manage (no settings:write) dispatches to that env.
        let (connections, _output) = t3_client::environments::Environments::start(runtime.handle(), false);
        state.update(cx, |state, _| {
            state.stop_pump();
            state.connections = connections;
            state.environments.active = Some("saved-local".into());
            state.environments.environments = serde_json::from_value(json!([
                {
                    "id": "saved-local", "environmentId": "env-a", "label": "Work Mac",
                    "httpBase": "http://127.0.0.1/saved-local", "accessToken": "", "disabled": false
                },
                {
                    "id": "saved-remote", "environmentId": "env-remote", "label": "Remote Box",
                    "httpBase": "http://127.0.0.1/saved-remote", "accessToken": "", "disabled": false
                }
            ]))
            .unwrap();
        });
        let mut fleet_rx = state.read_with(cx, |s, _| s.connections.view());
        let remote_grants: t3_core::auth::AuthSessionState =
            serde_json::from_value(json!({ "authenticated": true, "permissions": ["providers:manage"] })).unwrap();
        let (remote_client, mut remote_fixture) = t3_client::supervisor::ClientFixture::handle(remote_grants);
        let mut remote_shell = t3_core::shell::ShellState::default();
        remote_shell.synchronized = true;
        let mut remote_config = t3_core::server_config::ServerConfigState::default();
        remote_config.config = Some(
            serde_json::from_value(json!({
                "environment": { "environmentId": "env-remote", "label": "Remote Box", "capabilities": {} },
                "settings": { "usageLimitSources": {} },
                "providers": []
            }))
            .unwrap(),
        );
        let install = state.update(cx, |state, _| {
            state.connections.install_fixture(
                "saved-remote",
                remote_client,
                remote_shell,
                remote_config,
                t3_client::supervisor::ConnectionStatus::Connected,
            )
        });
        let _remote_events = runtime.spawn(install).await.unwrap();
        let fleet = fleet_rx.borrow_and_update().clone();
        state.update(cx, |state, cx| state.apply_fleet_view(fleet, cx));
        cx.run_until_parked();
        window
            .update(cx, |host, window, cx| {
                host.0.update(cx, |settings, cx| {
                    settings.scope.machine = Some("env-remote".into());
                    assert!(!settings.usage_sources_read_only(cx));
                    settings.open_add_usage_source_dialog("Remote Box".into(), window, cx);
                    let dialog = settings.providers.usage_sources.add.as_ref().unwrap();
                    dialog.url.update(cx, |input, cx| input.set_value("http://remote-hub:8318", window, cx));
                    dialog.key.update(cx, |input, cx| input.set_value("secret-key", window, cx));
                    settings.save_usage_source(cx);
                });
            })
            .unwrap();
        cx.run_until_parked();
        let (method, payload, reply) = remote_fixture.next_request().await;
        assert_eq!(method, "server.updateSettings");
        assert_eq!(
            payload["patch"]["usageLimitSources"]["cliproxy-remote-hub-8318"]["managementKey"],
            json!("secret-key")
        );
        let _ = reply.send(Ok(json!({})));
        cx.run_until_parked();
        let _ = view;

        state.update(cx, |state, _| state.stop_pump());
        runtime.shutdown_background();
    }
}
