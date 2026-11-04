//! The selected instance's editor (`ProviderInstanceCard.tsx` in editor mode): its header, the
//! Status and Display name rows, Setup (`ProviderSetupSection.tsx`'s runtime and
//! `ProviderAuthenticationSection.tsx`'s account), and Models (`ProviderModelsSection.tsx`). The
//! Runtime and environment fields use `form`. Managed Codex uses `chatgpt_setup`;
//! existing Codex keeps the generic Account row.

use gpui::{AnyElement, AppContext, Context, FontWeight, IntoElement, ParentElement, PromptLevel, SharedString, Styled, Window, div, list, prelude::*, px, relative};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::{Value, json};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::providers::auth::{self, AccountAction, AccountView, CredentialField, RuntimeView};
use t3_core::settings::providers::{self as model, ACP_REGISTRY, DisplayModel, ModelGroup};

use super::super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, icon, r};
use super::super::{SettingsView, store};
use super::{Selected, failure_text, flat_section, status_dot};
use crate::components::badge::{BadgeKind, badge};
use crate::icons::Icon;
use crate::provider_icon::{BadgeSize, ProviderIcon, ProviderMark};
use crate::theme::{hsla, palette};

const SERVER_ACCEPT_ACP_URL_AUTH: &str = "server.acceptAcpRegistryUrlAuth";
/// `FILTER_THRESHOLD`: the models list offers a filter past this many models.
const MODEL_ROW_HEIGHT: f32 = 28.;
const MODELS_MAX_HEIGHT: f32 = 256.;

/// What the models list draws, kept between frames so the list can build only the rows in view.
#[derive(Default)]
pub(super) struct ModelRows {
    instance_id: String,
    filter: String,
    filtering: bool,
    flag_missing_reasoning: bool,
    rows: Vec<model::ModelRow>,
}

/// A row, plus the group heading above it: tighter for the list's first row.
fn model_entry_height(index: usize, row: &model::ModelRow) -> f32 {
    MODEL_ROW_HEIGHT
        + match row.heading {
            None => 0.,
            Some(_) if index == 0 => 24.,
            Some(_) => 40.,
        }
}

/// Whether the editor shows an Account row, which needs `provider.auth.subscribe`.
pub(super) fn shows_account(selected: &Selected) -> bool {
    let Some(live) = &selected.live else { return false };
    if selected.row.driver == "antigravity" {
        return live.setup.is_some();
    }
    live.setup.as_ref().is_some_and(|setup| setup.can_authenticate) || (live.driver == ACP_REGISTRY && live.installed)
}

impl SettingsView {
    pub(super) fn render_editor(&mut self, selected: &Selected, environment_label: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut sections = vec![self.render_identity(selected, cx).into_any_element()];
        if let Some(setup) = self.render_setup(selected, environment_label, window, cx) {
            sections.push(setup.into_any_element());
        }
        if let Some(runtime)=self.render_runtime_form(selected,cx) {sections.push(runtime);}
        sections.extend(self.render_environment_form(selected,cx));
        if model::find_driver(&selected.row.driver).is_none() {
            let description = format!(
                "This instance uses {}, which is not available in this build. Its configuration is preserved.",
                selected.row.driver
            );
            sections.push(flat_section("Runtime", vec![Row::new("provider-driver", "Driver").description(description).into_any_element()], cx).into_any_element());
        } else {
            sections.push(self.render_models(selected, cx).into_any_element());
        }
        v_flex().py(r(16.)).gap(r(24.)).children(sections).into_any_element()
    }

    fn render_identity(&mut self, selected: &Selected, cx: &mut Context<Self>) -> gpui::Div {
        let p = palette(cx);
        let view = &selected.view;
        let row = &selected.row;
        let reset = (row.is_default && row.is_dirty).then(|| {
            let id = row.instance_id.clone();
            let label = model::find_driver(&row.driver).map_or(row.driver.clone(), |driver| driver.label.to_owned());
            Button::new("reset-provider-instance", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                .icon(Icon::Undo2)
                .focus_handle(self.providers.removal.action_focus.clone())
                .disabled(self.providers.removal.model.busy() || !self.removal_allowed(cx))
                .tooltip(format!("Reset {label} provider settings"))
                .on_click(cx.listener(move |this, _, window, cx| this.request_provider_removal(&id, true, window, cx)))
        });
        let delete = (!row.is_default).then(|| {
            let id = row.instance_id.clone();
            Button::new("delete-provider-instance", ButtonVariant::GhostMuted, ButtonSize::IconSm)
                .icon(Icon::Trash2)
                .focus_handle(self.providers.removal.action_focus.clone())
                .disabled(self.providers.removal.model.busy() || !self.removal_allowed(cx))
                .tooltip(format!("Delete instance {}", row.instance_id))
                .on_click(cx.listener(move |this, _, window, cx| this.request_provider_removal(&id, false, window, cx)))
        });
        let header = h_flex()
            .min_h(r(28.))
            .px(r(16.))
            .gap(r(8.))
            .child(ProviderIcon::new(ProviderMark::configured(&row.driver, &view.display_name, selected.accent)).size(px(20.)).badge(BadgeSize::Compact, hsla(p.background)))
            .child(div().flex_1().min_w_0().truncate().text_base().font_weight(FontWeight::SEMIBOLD).child(view.display_name.clone()))
            .children(model::find_driver(&row.driver).and_then(|driver| driver.badge_label).map(|label| badge(label, BadgeKind::Warning, false, cx)))
            .children(view.version.clone().map(|version| {
                div().flex_none().text_xs().font_family(cx.theme().mono_font_family.clone()).text_color(hsla(p.muted_foreground)).child(version)
            }))
            .children(view.advisory.as_ref().map(|_| self.render_advisory_trigger(&row.instance_id, view, false, cx)))
            .children(reset)
            .children(delete);

        let status = self.render_status_line(selected, cx);
        let swatch = div()
            .size(r(16.))
            .flex_none()
            .rounded_full()
            .border_1()
            .map(|this| match selected.accent {
                Some(accent) => this.border_color(gpui::rgba(accent)).bg(gpui::rgba(accent)),
                None => this.border_color(hsla(p.border)),
            });
        let identity = Row::new("provider-display-name", "Display name").control(
            h_flex()
                .gap(r(8.))
                .child(swatch)
                .child(div().w(r(96.)).child(Input::new(&self.providers.accent).small()))
                .child(div().w(r(224.)).child(Input::new(&self.providers.display_name).small())),
        );
        v_flex()
            .gap(r(4.))
            .child(header)
            .child(Row::new("provider-status", "Status").below(div().pb(r(8.)).child(status)))
            .child(div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(identity))
    }

    fn render_status_line(&mut self, selected: &Selected, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let view = &selected.view;
        let mut line = h_flex().flex_wrap().items_center().gap_x(r(6.)).text_xs().line_height(r(18.)).text_color(hsla(p.muted_foreground));
        match &view.authenticated_email {
            Some(email) => {
                line = line
                    .when(view.needs_attention, |this| this.child(status_dot(view.status, cx)))
                    .child("Authenticated as")
                    .child(self.render_email(email, cx))
                    .children(view.auth_label.clone().map(|label| format!("\u{00b7} {label}")));
            }
            None => {
                line = line.when(view.needs_attention, |this| this.child(status_dot(view.status, cx))).child(view.headline.clone());
            }
        }
        line = line.children(view.inline_detail.clone().map(|detail| div().min_w_0().child(format!("\u{00b7} {detail}"))));
        let url_action = selected.live.as_ref().and_then(|live| live.auth.action.clone()).map(|action| {
            let instance_id = selected.row.instance_id.clone();
            let url = action.url.clone();
            let elicitation = action.elicitation_id.clone();
            v_flex()
                .pt(r(4.))
                .gap(r(6.))
                .max_w(r(576.))
                .text_xs()
                .child(div().text_color(hsla(p.foreground)).child(action.message.clone()))
                .child(div().text_size(r(11.)).font_family(cx.theme().mono_font_family.clone()).child(action.url.clone()))
                .child(
                    Button::new("continue-url-auth", ButtonVariant::Outline, ButtonSize::Compact)
                        .icon(Icon::ExternalLink)
                        .label("Continue authentication")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.open_url(&url);
                            this.accept_url_auth(&instance_id, &elicitation, cx);
                        })),
                )
        });
        v_flex().gap(r(4.)).child(line).children(url_action).into_any_element()
    }

    /// `RedactedSensitiveText`: the account email stays hidden until clicked.
    fn render_email(&self, email: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let revealed = self.providers.email_revealed;
        let text = if revealed { email.to_owned() } else { "\u{2022}".repeat(email.chars().count().clamp(6, 18)) };
        div()
            .id("provider-account-email")
            .min_w_0()
            .truncate()
            .cursor_pointer()
            .text_color(hsla(p.foreground))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(if revealed { "Click to hide email" } else { "Click to reveal email" }).build(window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.providers.email_revealed = !this.providers.email_revealed;
                cx.notify();
            }))
            .child(text)
            .into_any_element()
    }

    fn render_setup(&mut self, selected: &Selected, environment_label: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let mut rows: Vec<AnyElement> = Vec::new();
        let live = selected.live.as_ref();
        if selected.row.driver == "codex" {
            let connection = self.state.read(cx).connection_id();
            if self.providers.chatgpt.as_ref().is_none_or(|(id, view)| *id != connection || view.read(cx).instance() != selected.row.instance_id) {
                let view = cx.new(|cx| crate::chatgpt_setup::ChatGptSetup::new(self.state.clone(), selected.row.instance_id.clone(), false, window, cx));
                self.providers.form.runtime_observer=Some(cx.observe(&view,|_,_,cx|cx.notify()));
                self.providers.chatgpt = Some((connection, view));
            }
            rows.push(div().child(self.providers.chatgpt.as_ref().unwrap().1.clone()).into_any_element());
        }
        if selected.row.driver == "antigravity" {
            let enabled = selected.view.enabled;
            let id = selected.row.instance_id.clone();
            rows.push(
                Row::new("provider-environment", "Environment")
                    .description("Device that runs this provider.")
                    .control(
                        v_flex()
                            .items_end()
                            .gap(r(8.))
                            .child(div().text_xs().text_color(hsla(palette(cx).muted_foreground)).child(environment_label.to_owned()))
                            .when(!enabled, |this| {
                                this.child(
                                    crate::components::managed::ManagedControl::new("managed-antigravity", t3_core::managed_policy::driver_denial(self.state.read(cx).server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()), "antigravity", "Antigravity"), Button::new("enable-antigravity", ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Enable Antigravity")
                                        .on_click(cx.listener(move |this, _, _, cx| this.update_instance(&id, model::InstanceEdit::Enabled(true), cx)))),
                                )
                            }),
                    )
                    .into_any_element(),
            );
            match live.filter(|live| live.setup.is_some()) {
                None => rows.push(Row::new("provider-update-required", "Update required").description("Update this environment to manage Antigravity.").into_any_element()),
                Some(live) => {
                    let binary = model::config_string(selected.row.instance().config.as_ref(), "binaryPath");
                    let install = self.providers.install.as_ref().and_then(|session| session.state.clone());
                    let view = auth::runtime_view(live, install.as_ref(), binary.as_deref(), enabled);
                    rows.push(self.render_runtime(&selected.row.instance_id, &view, environment_label, cx));
                }
            }
        }
        if model::chatgpt::uses_generic_account(&selected.row.driver,selected.row.instance().config.as_ref()) && shows_account(selected) && self.providers.dialog.as_ref().is_none_or(|d|!d.add.created())
            && let Some(live) = live
        {
            rows.push(self.render_account(selected, live, environment_label, window, cx));
        } else if selected.row.driver == "cursor" && live.and_then(|live| live.setup.as_ref()).is_some_and(|setup| !setup.can_authenticate) {
            rows.push(
                Row::new("cursor-account", "Cursor account")
                    .description("Using CURSOR_API_KEY. Remove it from this provider's environment to use browser sign-in.")
                    .into_any_element(),
            );
        }
        if let Some(field)=self.render_dedicated_environment(selected,cx) {rows.push(field);}
        (!rows.is_empty()).then(|| flat_section("Setup", rows, cx))
    }

    fn render_runtime(&self, instance_id: &str, view: &RuntimeView, environment_label: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let session = self.providers.install.as_ref();
        let pending = session.is_some_and(|session| session.pending);
        let auth_active = self.providers.auth.as_ref().and_then(|session| session.state.as_ref()).is_some_and(|state| state.is_active());
        let load_error = session.and_then(|session| session.load_error.clone());
        let disabled = pending || load_error.is_some();
        let progress = view.progress.map(|fraction| {
            div().h(r(4.)).w_full().rounded_full().bg(hsla(p.muted)).overflow_hidden().child(div().h_full().w(relative(fraction)).rounded_full().bg(hsla(p.primary)))
        });
        let id = instance_id.to_owned();
        let action = if view.can_cancel {
            Some(
                Button::new("cancel-install", ButtonVariant::Outline, ButtonSize::Sm)
                    .label("Cancel installation")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| this.cancel_install(&id, cx))),
            )
        } else {
            view.install_label.clone().map(|label| {
                Button::new("start-install", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(label)
                    .disabled(disabled || auth_active)
                    .on_click(cx.listener(move |this, _, _, cx| this.install_command(auth::INSTALL_START, auth::subscribe_payload(&id), cx)))
            })
        };
        let remove = view.can_remove.then(|| {
            let id = instance_id.to_owned();
            let prompt = auth::remove_runtime_prompt(environment_label);
            Button::new("remove-runtime", ButtonVariant::Ghost, ButtonSize::IconSm)
                .icon(Icon::Trash2)
                .tooltip("Remove downloaded runtime")
                .disabled(disabled || auth_active)
                .on_click(cx.listener(move |this, _, window, cx| this.confirm_remove_runtime(&id, &prompt, window, cx)))
        });
        let mut notes = Vec::new();
        if view.uses_custom_binary {
            notes.push("Uses the custom binary path below. Installation keeps that path.");
        }
        if view.cannot_install {
            notes.push("Automatic installation unavailable. Set a binary path or use another environment.");
        }
        let errors: Vec<String> = session.and_then(|session| session.error.clone()).into_iter().chain(load_error).collect();
        let mut row = Row::new("provider-runtime", "Runtime").description("Install and manage Antigravity.").control(
            v_flex()
                .w(r(224.))
                .items_end()
                .gap(r(8.))
                .text_xs()
                .child(div().text_color(hsla(p.muted_foreground)).child(view.status.clone()))
                .children(progress)
                .children(view.message.clone().map(|message| div().text_color(hsla(p.muted_foreground)).child(message)))
                .child(h_flex().gap(r(8.)).children(remove).children(action)),
        );
        if !notes.is_empty() || !errors.is_empty() {
            row = row.below(
                v_flex()
                    .pb(r(8.))
                    .gap(r(4.))
                    .text_xs()
                    .children(notes.into_iter().map(|note| div().text_color(hsla(p.muted_foreground)).child(note)))
                    .children(errors.into_iter().map(|error| div().text_color(hsla(p.destructive_foreground)).child(error))),
            );
        }
        row.into_any_element()
    }

    pub(super) fn render_account(&mut self, selected: &Selected, live: &t3_core::server_config::ServerProvider, environment_label: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let session = self.providers.auth.as_ref();
        let state = session.and_then(|session| session.state.clone());
        let pending = session.is_some_and(|session| session.pending);
        let load_error = session.and_then(|session| session.load_error.clone());
        let command_error = session.and_then(|session| session.error.clone());
        let view: AccountView = auth::account_view(live, state.as_ref(), environment_label, load_error.is_some(), pending);
        let instance_id = selected.row.instance_id.clone();
        self.sync_credential_inputs(state.as_ref(), &view.credentials, window, cx);

        let mut controls = h_flex().flex_wrap().justify_end().gap(r(6.));
        if !view.methods.is_empty() {
            let mut options = vec![SelectOption::new("", "Provider default")];
            options.extend(view.methods.iter().map(|method| SelectOption::new(method.id.clone(), method.name.clone())));
            let current = self.providers.method_id.clone();
            let label = view.methods.iter().find(|method| method.id == current.as_ref()).map_or("Provider default".to_owned(), |method| method.name.clone());
            let pick = cx.listener(|this, value: &SharedString, _, cx| {
                this.providers.method_id = value.clone();
                cx.notify();
            });
            controls = controls.child(Select::new("provider-auth-method", options).selected(current).label(label).width(176.).on_change(move |value, window, cx| pick(&value, window, cx)));
        }
        if view.url.is_some() {
            let open_id = instance_id.clone();
            let copy_id = instance_id.clone();
            controls = controls
                .child(
                    Button::new("provider-open-browser", ButtonVariant::Outline, ButtonSize::Sm)
                        .label("Open browser")
                        .disabled(view.disabled)
                        .on_click(cx.listener(move |this, _, _, cx| this.open_auth_page(&open_id, false, cx))),
                )
                .child(
                    Button::new("provider-copy-link", ButtonVariant::GhostMuted, ButtonSize::IconSm)
                        .icon(Icon::Copy)
                        .tooltip("Copy sign-in link")
                        .disabled(view.disabled)
                        .on_click(cx.listener(move |this, _, _, cx| this.open_auth_page(&copy_id, true, cx))),
                );
        }
        match view.action.clone() {
            Some(AccountAction::OpenDocs(url)) => {
                controls = controls.child(Button::new("provider-open-docs", ButtonVariant::Outline, ButtonSize::Sm).label("Open docs").on_click(move |_, _, cx| {if !super::fixture_active(cx){cx.open_url(&url);}}));
            }
            Some(AccountAction::Cancel) => {
                let id = instance_id.clone();
                controls = controls.child(
                    Button::new("provider-cancel-sign-in", ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .label("Cancel")
                        .disabled(view.disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let payload = this.providers.auth.as_ref().and_then(|session| session.state.as_ref()).and_then(|state| auth::cancel_payload(&id, state));
                            if let Some(payload) = payload {
                                this.auth_command(auth::AUTH_CANCEL, payload, cx);
                            }
                        })),
                );
            }
            Some(AccountAction::Start(label)) => {
                let id = instance_id.clone();
                controls = controls.child(
                    Button::new("provider-sign-in", ButtonVariant::Outline, ButtonSize::Sm)
                        .label(label)
                        .disabled(!view.action_enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let payload = {
                                let state = this.providers.auth.as_ref().and_then(|session| session.state.as_ref());
                                auth::start_payload(&id, Some(this.providers.method_id.as_ref()), state)
                            };
                            this.auth_command(auth::AUTH_START, payload, cx);
                        })),
                );
            }
            None => {}
        }
        if view.can_sign_out {
            let id = instance_id.clone();
            let prompt = auth::sign_out_prompt(live.display_name.as_deref().unwrap_or(&selected.view.display_name), environment_label);
            controls = controls.child(
                Button::new("provider-sign-out", ButtonVariant::Ghost, ButtonSize::Sm)
                    .label("Sign out")
                    .disabled(view.disabled || state.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| this.confirm_sign_out(&id, &prompt, window, cx))),
            );
        }

        let mut below: Vec<AnyElement> = Vec::new();
        if let Some(failure) = &view.failure {
            below.push(div().text_xs().text_color(hsla(p.muted_foreground)).child(failure.clone()).into_any_element());
        }
        if let Some(code) = &view.device_code {
            below.push(
                h_flex()
                    .py(r(4.))
                    .gap(r(4.))
                    .text_sm()
                    .text_color(hsla(p.muted_foreground))
                    .child("Enter code")
                    .child(div().font_family(cx.theme().mono_font_family.clone()).text_color(hsla(p.foreground)).child(code.clone()))
                    .child("in your browser.")
                    .into_any_element(),
            );
        }
        if let Some(output) = &view.terminal_output {
            let tail: Vec<&str> = output.lines().rev().take(16).collect::<Vec<_>>().into_iter().rev().collect();
            below.push(
                v_flex()
                    .py(r(4.))
                    .gap(r(4.))
                    .child(
                        div()
                            .p(r(8.))
                            .rounded(r(6.))
                            .bg(hsla(p.terminal_background))
                            .text_color(hsla(p.terminal_foreground))
                            .text_size(r(11.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(tail.join("\n")),
                    )
                    .child(Input::new(&self.providers.terminal_line).small())
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Sends the line followed by Enter. For prompts that need arrow keys, use the web or desktop app."))
                    .into_any_element(),
            );
        }
        if !view.credentials.is_empty() {
            below.push(self.render_credentials(&instance_id, &view, cx));
        }
        if view.accepts_callback {
            below.push(
                v_flex()
                    .py(r(4.))
                    .gap(r(8.))
                    .text_sm()
                    .child(div().text_color(hsla(p.foreground)).child("If the final localhost page does not load, paste its full URL here."))
                    .child(div().w_full().max_w(r(420.)).child(Input::new(&self.providers.callback_url).small()))
                    .child(
                        Button::new("provider-callback-continue", ButtonVariant::Outline, ButtonSize::Sm)
                            .label("Continue")
                            .disabled(view.disabled)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_callback(cx))),
                    )
                    .into_any_element(),
            );
        }
        for error in command_error.into_iter().chain(load_error) {
            below.push(div().py(r(4.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error).into_any_element());
        }

        let mut row = Row::new("provider-account", "Account").control(controls);
        row = match &view.signed_in_email {
            Some(email) => row.below(
                h_flex()
                    .pb(r(4.))
                    .gap(r(4.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child("Signed in as")
                    .child(self.render_email(email, cx)),
            ),
            None => row.description(view.description.clone()),
        };
        if !below.is_empty() {
            row = row.below(v_flex().pb(r(8.)).gap(r(4.)).children(below));
        }
        row.into_any_element()
    }

    fn render_credentials(&self, instance_id: &str, view: &AccountView, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let fields = self.providers.credentials.as_ref().map(|(_, fields)| fields.clone()).unwrap_or_default();
        let id = instance_id.to_owned();
        v_flex()
            .py(r(4.))
            .gap(r(8.))
            .text_sm()
            .children(view.credentials.iter().filter_map(|field: &CredentialField| {
                let input = fields.iter().find(|(name, _)| *name == field.name).map(|(_, input)| input.clone())?;
                Some(v_flex().gap(r(4.)).child(div().text_color(hsla(p.foreground)).child(field.label.clone())).child(div().max_w(r(420.)).child(Input::new(&input).small())))
            }))
            .child(
                Button::new("provider-credentials-connect", ButtonVariant::Outline, ButtonSize::Sm)
                    .label("Connect")
                    .disabled(view.disabled)
                    .on_click(cx.listener(move |this, _, _, cx| this.submit_credentials(&id, cx))),
            )
            .into_any_element()
    }

    /// One input per credential field, recreated for each new flow or interaction.
    fn sync_credential_inputs(&mut self, state: Option<&auth::AuthState>, fields: &[CredentialField], window: &mut Window, cx: &mut Context<Self>) {
        if fields.is_empty() {
            self.providers.credentials = None;
            return;
        }
        let key = state.map(|state| format!("{}:{}", state.flow_id.as_deref().unwrap_or(""), state.interaction.as_ref().map_or("", |interaction| interaction.id()))).unwrap_or_default();
        if self.providers.credentials.as_ref().is_some_and(|(current, _)| *current == key) {
            return;
        }
        let inputs = fields
            .iter()
            .map(|field| {
                let secret = field.secret;
                (field.name.clone(), cx.new(|cx| InputState::new(window, cx).masked(secret)))
            })
            .collect();
        self.providers.credentials = Some((key, inputs));
    }

    fn render_models(&mut self, selected: &Selected, cx: &mut Context<Self>) -> gpui::Div {
        let p = palette(cx);
        let instance_id = selected.row.instance_id.clone();
        let driver = selected.row.driver.clone();
        let filter = self.providers.model_filter.read(cx).value().trim().to_lowercase();
        let filtering = selected.models.len() > model::MODEL_FILTER_THRESHOLD && !filter.is_empty();
        self.sync_model_rows(selected, if filtering { filter } else { String::new() });
        let display: Vec<DisplayModel> = selected.models.iter().map(|(model, _)| model.clone()).collect();
        let built_in = display.iter().any(|model| !model.is_custom);
        let all_hidden = model::all_built_in_hidden(&display, &selected.hidden);

        let mut toolbar = h_flex().flex_wrap().gap(r(8.));
        if selected.models.len() > model::MODEL_FILTER_THRESHOLD {
            toolbar = toolbar.child(div().w(r(224.)).child(Input::new(&self.providers.model_filter).small()));
        }
        let mut counts = h_flex().gap(r(8.));
        if built_in {
            let id = instance_id.clone();
            let hidden = selected.hidden.clone();
            let models = display.clone();
            counts = counts.child(
                Button::new("models-bulk-toggle", ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .label(if all_hidden { "Enable all" } else { "Disable all" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(payload) = model::bulk_hidden_payload(&id, &models, &hidden) {
                            store::update_server(&this.state, ServerSettingChange::Prepared(payload), cx);
                        }
                    })),
            );
        }
        counts = counts.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(model::models_summary(&selected.models, &selected.hidden)));
        toolbar = toolbar.child(counts).child(div().flex_1());
        let can_add = driver != "antigravity";
        if can_add && !self.providers.adding_custom_model {
            toolbar = toolbar.child(
                Button::new("add-custom-model", ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .icon(Icon::Plus)
                    .label("Add custom model")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.providers.adding_custom_model = true;
                        this.providers.custom_model_error = None;
                        this.providers.custom_model.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                            input.focus(window, cx);
                        });
                        cx.notify();
                    })),
            );
        }

        let rows = &self.providers.model_rows.rows;
        let list = if rows.is_empty() {
            div()
                .px(r(16.))
                .py(r(8.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(if filtering { "No models match." } else { "No models reported for this provider yet." })
                .into_any_element()
        } else {
            let height: f32 = rows.iter().enumerate().map(|(index, row)| model_entry_height(index, row)).sum();
            div()
                .mt(r(8.))
                .mx(r(-8.))
                .h(r(height.min(MODELS_MAX_HEIGHT)))
                .child(list(self.providers.models_list.clone(), cx.processor(|this, index: usize, _, cx| this.render_model_entry(index, cx))).size_full())
                .into_any_element()
        };

        let add_row = (can_add && self.providers.adding_custom_model).then(|| {
            h_flex()
                .mt(r(12.))
                .gap(r(8.))
                .child(div().flex_1().child(Input::new(&self.providers.custom_model).small()))
                .child(Button::new("add-custom-model-confirm", ButtonVariant::Outline, ButtonSize::Sm).label("Add").on_click(cx.listener(|this, _, window, cx| this.add_custom_model(window, cx))))
                .child(Button::new("add-custom-model-cancel", ButtonVariant::Ghost, ButtonSize::Sm).label("Cancel").on_click(cx.listener(|this, _, window, cx| {
                    this.providers.adding_custom_model = false;
                    this.providers.custom_model_error = None;
                    this.providers.custom_model.update(cx, |input, cx| input.set_value("", window, cx));
                    cx.notify();
                })))
        });
        let error = self.providers.custom_model_error.clone().map(|error| div().mt(r(8.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error));

        v_flex().gap(r(4.)).child(super::super::controls::section_title("Models")).child(
            v_flex()
                .px(r(16.))
                .py(r(12.))
                .child(
                    div()
                        .mb(r(12.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Model preferences and custom models are saved on the selected environment and shared across connected devices."),
                )
                .child(toolbar)
                .child(list)
                .children(add_row)
                .children(error),
        )
    }

    /// Rebuilds the drawn rows when the models, preferences or filter change. A new instance or
    /// filter, or a different row count, starts the list over; otherwise it keeps its scroll.
    fn sync_model_rows(&mut self, selected: &Selected, filter: String) {
        let rows = model::model_rows(&selected.models, &selected.hidden, &filter);
        let current = &self.providers.model_rows;
        let restart = current.instance_id != selected.row.instance_id || current.filter != filter || current.rows.len() != rows.len();
        if !restart && current.rows == rows {
            return;
        }
        if restart {
            self.providers.models_list.reset(rows.len());
        } else {
            self.providers.models_list.remeasure();
        }
        self.providers.model_rows = ModelRows {
            instance_id: selected.row.instance_id.clone(),
            filtering: !filter.is_empty(),
            filter,
            flag_missing_reasoning: selected.models.iter().any(|(model, _)| model::has_reasoning(model)),
            rows,
        };
    }

    fn render_model_entry(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.providers.model_rows.rows.get(index).cloned() else { return div().into_any_element() };
        let heading = row.heading.map(|heading| {
            div()
                .h(r(model_entry_height(index, &row) - MODEL_ROW_HEIGHT))
                .flex()
                .items_end()
                .px(r(8.))
                .pb(r(6.))
                .text_size(r(11.))
                .text_color(hsla(palette(cx).muted_foreground))
                .child(heading)
        });
        v_flex().w_full().children(heading).child(self.render_model_row(index, &row, cx)).into_any_element()
    }

    fn render_model_row(&self, index: usize, row: &model::ModelRow, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let ModelRows { instance_id, filtering, flag_missing_reasoning, .. } = &self.providers.model_rows;
        let (instance_id, filtering, flag_missing_reasoning) = (instance_id.as_str(), *filtering, *flag_missing_reasoning);
        let model = &row.model;
        let (can_up, can_down) = (row.can_move_up, row.can_move_down);
        let hidden = row.hidden;
        let favorite = row.group == ModelGroup::Favorite;
        let labels = model::capability_labels(model, flag_missing_reasoning);
        let star_id = instance_id.to_owned();
        let star_slug = model.slug.clone();
        let star = h_flex()
            .id(("model-favorite", index))
            .size(r(20.))
            .flex_none()
            .justify_center()
            .rounded(r(4.))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.accent)))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(if favorite { "Remove from favorites" } else { "Add to favorites" }).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                store::update_server(&this.state, ServerSettingChange::Prepared(model::favorite_payload(&star_id, &star_slug, !favorite)), cx);
            }))
            .child(icon(if favorite { Icon::StarFill } else { Icon::Star }, 12., if favorite { hsla(p.warning) } else { hsla(p.muted_foreground) }));
        let mut actions = h_flex().flex_none().gap(r(2.)).justify_end().min_w(r(88.));
        if !hidden && !filtering {
            let (up_id, up_slug) = (instance_id.to_owned(), model.slug.clone());
            let (down_id, down_slug) = (instance_id.to_owned(), model.slug.clone());
            actions = actions
                .child(
                    Button::new(("model-up", index), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                        .icon(Icon::ArrowUp)
                        .tooltip("Move up")
                        .disabled(!can_up)
                        .on_click(cx.listener(move |this, _, _, cx| this.move_model(&up_id, &up_slug, false, cx))),
                )
                .child(
                    Button::new(("model-down", index), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                        .icon(Icon::ArrowDown)
                        .tooltip("Move down")
                        .disabled(!can_down)
                        .on_click(cx.listener(move |this, _, _, cx| this.move_model(&down_id, &down_slug, true, cx))),
                );
        }
        if model.is_custom {
            let (id, slug) = (instance_id.to_owned(), model.slug.clone());
            actions = actions.child(
                Button::new(("model-remove", index), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                    .icon(Icon::X)
                    .tooltip("Remove custom model")
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_custom_model(&id, &slug, cx))),
            );
        }
        let show_id = instance_id.to_owned();
        let show_slug = model.slug.clone();
        let picker_tooltip: SharedString = if model.is_custom {
            "Custom models are always shown in the picker".into()
        } else if hidden {
            "Hidden from picker".into()
        } else {
            "Shown in picker".into()
        };
        let toggle = cx.listener(move |this, shown: &bool, _, cx| {
            store::update_server(&this.state, ServerSettingChange::Prepared(model::hidden_payload(&show_id, &[(show_slug.clone(), !*shown)])), cx);
        });
        let switch = Switch::new(("model-shown", index), !hidden);
        let switch = if model.is_custom { switch } else { switch.on_change(move |shown, window, cx| toggle(&shown, window, cx)) };
        h_flex()
            .id(("model-row", index))
            .w_full()
            .h(r(28.))
            .px(r(8.))
            .gap(r(8.))
            .rounded(r(6.))
            .hover(|this| this.bg(hsla(p.muted).opacity(0.3)))
            .child(star)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(8.))
                    .when(hidden, |this| this.opacity(0.5))
                    .child(div().flex_none().max_w_full().truncate().text_xs().text_color(if hidden { hsla(p.muted_foreground) } else { hsla(p.foreground).opacity(0.9) }).child(model.name.clone()))
                    .when(model.name != model.slug, |this| {
                        this.child(div().min_w_0().truncate().text_size(r(11.)).font_family(cx.theme().mono_font_family.clone()).text_color(hsla(p.muted_foreground).opacity(0.7)).child(model.slug.clone()))
                    })
                    .when(model.is_custom, |this| this.child(div().flex_none().text_size(r(11.)).text_color(hsla(p.muted_foreground).opacity(0.7)).child("custom"))),
            )
            .when(!labels.is_empty(), |this| this.child(div().flex_none().text_size(r(11.)).text_color(hsla(p.muted_foreground).opacity(0.7)).child(labels.join(" \u{00b7} "))))
            .child(actions)
            .child(
                div()
                    .id(("model-shown-wrap", index))
                    .flex_none()
                    .when(model.is_custom, |this| this.opacity(0.64))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(picker_tooltip.clone()).build(window, cx))
                    .child(switch),
            )
            .into_any_element()
    }

    pub(super) fn add_custom_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(instance_id) = self.selected_instance_id(cx) else { return };
        let typed = self.providers.custom_model.read(cx).value().to_string();
        let validated = {
            let app = self.state.read(cx);
            let Some(config) = app.server_config.config.as_ref() else { return };
            let instance = config.settings.provider_instances.get(&instance_id).cloned().unwrap_or_else(|| t3_core::server_config::ProviderInstanceConfig { driver: instance_id.clone(), ..Default::default() });
            let live = config.provider(&instance_id).map(|provider| provider.models.as_slice()).unwrap_or_default();
            model::validate_custom_model(&typed, live, &model::custom_models(&instance))
        };
        match validated {
            Ok(slug) => {
                self.update_instance(&instance_id, model::InstanceEdit::AddCustomModel(slug.to_string()), cx);
                self.providers.adding_custom_model = false;
                self.providers.custom_model_error = None;
                self.providers.model_filter.update(cx, |input, cx| input.set_value("", window, cx));
                self.providers.custom_model.update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(error) => self.providers.custom_model_error = Some(error),
        }
        cx.notify();
    }

    /// `handleRemove`: drops the model, and its place in the order and favorites.
    fn remove_custom_model(&mut self, instance_id: &str, slug: &str, cx: &mut Context<Self>) {
        self.update_instance(instance_id, model::InstanceEdit::RemoveCustomModel(slug.to_owned()), cx);
        let (order, favorite) = {
            let app = self.state.read(cx);
            let Some(config) = app.server_config.config.as_ref() else { return };
            let (_, order) = model::instance_preferences(&config.settings, instance_id);
            (order, model::instance_favorites(&config.settings, instance_id).iter().any(|model| model == slug))
        };
        if order.iter().any(|model| model == slug) {
            let next: Vec<String> = order.into_iter().filter(|model| model != slug).collect();
            store::update_server(&self.state, ServerSettingChange::Prepared(model::order_payload(instance_id, &next)), cx);
        }
        if favorite {
            store::update_server(&self.state, ServerSettingChange::Prepared(model::favorite_payload(instance_id, slug, false)), cx);
        }
    }

    fn move_model(&mut self, instance_id: &str, slug: &str, down: bool, cx: &mut Context<Self>) {
        let order = {
            let app = self.state.read(cx);
            let Some(config) = app.server_config.config.as_ref() else { return };
            let instance = config.settings.provider_instances.get(instance_id).cloned().unwrap_or_else(|| t3_core::server_config::ProviderInstanceConfig { driver: instance_id.to_owned(), ..Default::default() });
            let live = config.provider(instance_id).map(|provider| provider.models.as_slice()).unwrap_or_default();
            let favorites = model::instance_favorites(&config.settings, instance_id);
            let (hidden, order) = model::instance_preferences(&config.settings, instance_id);
            let grouped = model::group_models(model::models_for_display(live, &model::custom_models(&instance)), &favorites, &hidden, &order);
            model::moved_order(&grouped, slug, down)
        };
        if let Some(order) = order {
            store::update_server(&self.state, ServerSettingChange::Prepared(model::order_payload(instance_id, &order)), cx);
        }
    }

    fn accept_url_auth(&mut self, instance_id: &str, elicitation_id: &str, cx: &mut Context<Self>) {
        let task = self.state.read(cx).rpc(SERVER_ACCEPT_ACP_URL_AUTH, json!({ "instanceId": instance_id, "elicitationId": elicitation_id }));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let notice = match result {
                    Ok(reply) if reply.get("accepted").and_then(Value::as_bool) == Some(false) => {
                        Some(("Authentication request expired", "Refresh the provider and start the authentication flow again.".to_owned()))
                    }
                    Ok(_) => None,
                    Err(error) => Some(("Could not continue authentication", failure_text(&error, "The authentication request expired."))),
                };
                if let Some(notice) = notice {
                    this.providers.notice = Some(notice);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Runs one `provider.auth.*` call for the selected instance, one at a time, and shows the
    /// state it answers with at once.
    pub(super) fn auth_command(&mut self, method: &'static str, payload: Value, cx: &mut Context<Self>) {
        if super::fixture_active(cx) || !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE,cx) || self.providers.dialog.as_ref().is_some_and(|d|d.connection!=self.state.read(cx).connection_id()){return;}
        let allowed = self.providers.auth.as_ref().is_some_and(|s| {
            let app=self.state.read(cx);
            app.server_config.config.as_ref().is_some_and(|c| c.provider(&s.instance_id).is_some_and(|p| t3_core::managed_policy::driver_allowed(c.managed_policy.as_ref(),&p.driver)))
        });
        if !allowed {return;}
        let Some(session) = self.providers.auth.as_mut() else { return };
        if session.pending {
            return;
        }
        session.pending = true;
        session.error = None;
        let id = session.instance_id.clone();
        cx.notify();
        let connection=self.state.read(cx).connection_id();
        let task = self.state.read(cx).rpc(method, payload);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).connection_id()!=connection {return;}
                if let Some(session) = this.providers.auth.as_mut().filter(|session| session.instance_id == id) {
                    session.pending = false;
                    match result {
                        Ok(reply) => {
                            if let Some(state) = auth::decode_auth_state(reply) {
                                session.state = Some(state);
                            }
                        }
                        Err(error) => session.error = Some(failure_text(&error, "Provider sign-in failed. Try again.")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens (or copies) the sign-in page. A browser interaction that asks for consent records
    /// it first, as the web does before opening the tab.
    fn open_auth_page(&mut self, instance_id: &str, copy: bool, cx: &mut Context<Self>) {
        if super::fixture_active(cx) || self.providers.dialog.as_ref().is_some_and(|d|d.connection!=self.state.read(cx).connection_id()){return;}
        let Some(state) = self.providers.auth.as_ref().and_then(|session| session.state.clone()) else { return };
        let Some(url) = state.url().map(str::to_owned) else { return };
        if copy {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
        }
        if state.requires_consent() && !self.may(t3_core::usage::PROVIDERS_MANAGE_SCOPE,cx) {return;}
        if !state.requires_consent() {
            if !copy {
                cx.open_url(&url);
            }
            return;
        }
        let Some(payload) = auth::accept_browser_payload(instance_id, &state) else { return };
        let Some(session) = self.providers.auth.as_mut() else { return };
        if session.pending {
            return;
        }
        session.pending = true;
        let id = instance_id.to_owned();
        let task = self.state.read(cx).rpc(auth::AUTH_RESPOND, payload);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(session) = this.providers.auth.as_mut().filter(|session| session.instance_id == id) {
                    session.pending = false;
                    match result {
                        Ok(reply) => {
                            if let Some(state) = auth::decode_auth_state(reply) {
                                session.state = Some(state);
                            }
                            if !copy {
                                cx.open_url(&url);
                            }
                        }
                        Err(error) => session.error = Some(failure_text(&error, "Could not open the sign-in page. Copy the link and open it in your browser.")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_sign_out(&mut self, instance_id: &str, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(PromptLevel::Warning, prompt, None, &["Sign out", "Cancel"], cx);
        let id = instance_id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                let _ = this.update(cx, |this, cx| this.auth_command(auth::AUTH_LOGOUT, auth::logout_payload(&id), cx));
            }
        })
        .detach();
    }

    fn submit_credentials(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let values: Vec<(String, String)> = self
            .providers
            .credentials
            .as_ref()
            .map(|(_, fields)| fields.iter().map(|(name, input)| (name.clone(), input.read(cx).value().to_string())).collect())
            .unwrap_or_default();
        let payload = self.providers.auth.as_ref().and_then(|session| session.state.as_ref()).and_then(|state| auth::credentials_payload(instance_id, state, &values));
        if let Some(payload) = payload {
            self.providers.credentials = None;
            self.auth_command(auth::AUTH_RESPOND, payload, cx);
        }
    }

    pub(super) fn submit_terminal_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.providers.terminal_line.read(cx).value().to_string();
        let payload = self.providers.auth.as_ref().and_then(|session| session.state.as_ref().and_then(|state| auth::terminal_line_payload(&session.instance_id, state, &line)));
        if let Some(payload) = payload {
            self.providers.terminal_line.update(cx, |input, cx| input.set_value("", window, cx));
            self.auth_command(auth::AUTH_RESPOND, payload, cx);
        }
    }

    pub(super) fn submit_callback(&mut self, cx: &mut Context<Self>) {
        let typed = self.providers.callback_url.read(cx).value().to_string();
        let payload = self.providers.auth.as_ref().and_then(|session| session.state.as_ref().and_then(|state| auth::complete_payload(&session.instance_id, state, &typed)));
        if let Some(payload) = payload {
            self.auth_command(auth::AUTH_COMPLETE, payload, cx);
        }
    }

    /// Runs one `provider.install.*` call for the selected instance.
    fn install_command(&mut self, method: &'static str, payload: Value, cx: &mut Context<Self>) {
        let Some(session) = self.providers.install.as_mut() else { return };
        if session.pending {
            return;
        }
        session.pending = true;
        session.error = None;
        let id = session.instance_id.clone();
        cx.notify();
        let task = self.state.read(cx).rpc(method, payload);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(session) = this.providers.install.as_mut().filter(|session| session.instance_id == id) {
                    session.pending = false;
                    match result {
                        Ok(reply) => {
                            if let Some(state) = auth::decode_install_state(reply) {
                                session.state = Some(state);
                            }
                        }
                        Err(error) => session.error = Some(failure_text(&error, "Provider setup failed. Try again.")),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_install(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let payload = self.providers.install.as_ref().and_then(|session| session.state.as_ref()).and_then(|state| auth::install_cancel_payload(instance_id, state));
        if let Some(payload) = payload {
            self.install_command(auth::INSTALL_CANCEL, payload, cx);
        }
    }

    fn confirm_remove_runtime(&mut self, instance_id: &str, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(PromptLevel::Warning, prompt, None, &["Remove", "Cancel"], cx);
        let id = instance_id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                let _ = this.update(cx, |this, cx| this.install_command(auth::INSTALL_REMOVE, auth::subscribe_payload(&id), cx));
            }
        })
        .detach();
    }
}
