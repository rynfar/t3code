//! Secrets: `SecretsSettingsPanel` in `web/src/components/settings/SecretsSettings.tsx` for the
//! connected environment, with the add and edit dialog (`SecretProviderDialog.tsx`), the removal
//! confirmation, and the account redacted until clicked (`RedactedSensitiveText.tsx`). Rules
//! live in `t3_core::settings::secrets`.

use std::collections::HashSet;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::supervisor::ConnectionStatus;
use t3_core::settings::ServerSettingChange;
use t3_core::settings::secrets::{self, Classified, Draft, PageState, Target};

use super::controls::{Button, ButtonSize, ButtonVariant, DialogFrame, Row, dialog_footer, group, muted_text, r, section_title, tooltip};
use super::{Block, SettingsView, store};
use crate::app_state::Phase;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

struct SecretDialog {
    target: Target,
    /// Errors show once a save was tried, then follow every edit.
    submitted: bool,
    focus: FocusHandle,
}

struct RemoveSecretDialog {
    id: String,
    label: String,
    focus: FocusHandle,
}

pub(crate) struct SecretsState {
    name: Entity<InputState>,
    account: Entity<InputState>,
    dialog: Option<SecretDialog>,
    remove_dialog: Option<RemoveSecretDialog>,
    /// Providers whose account is shown in the clear.
    revealed: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl SecretsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Personal"));
        let account = cx.new(|cx| InputState::new(window, cx).placeholder("my.1password.com"));
        let on_event = |this: &mut SettingsView, event: &InputEvent, cx: &mut Context<SettingsView>| match event {
            InputEvent::PressEnter { .. } => this.save_secret_provider(cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        };
        let subscriptions = vec![
            cx.subscribe_in(&name, window, move |this, _, event: &InputEvent, _, cx| on_event(this, event, cx)),
            cx.subscribe_in(&account, window, move |this, _, event: &InputEvent, _, cx| on_event(this, event, cx)),
        ];
        SecretsState { name, account, dialog: None, remove_dialog: None, revealed: HashSet::new(), _subscriptions: subscriptions }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        [&self.name, &self.account].into_iter()
    }

    /// Escape closes the dialog before it closes settings (`SettingsView::on_close`).
    pub(super) fn dismiss(&mut self) -> bool {
        self.dialog.take().is_some() | self.remove_dialog.take().is_some()
    }
}

impl SettingsView {
    fn secret_page_state(&self, cx: &App) -> PageState {
        let app = self.state.read(cx);
        let connected = matches!(app.phase, Phase::Online(ConnectionStatus::Connected));
        let config = app.server_config.config.as_ref();
        secrets::page_state(connected, config.is_some(), config.and_then(|config| config.secret_provider_settings), self.session(cx).as_ref())
    }

    fn secret_draft(&self, cx: &App) -> Draft {
        Draft { name: self.secrets.name.read(cx).value().to_string(), account: self.secrets.account.read(cx).value().to_string() }
    }

    fn open_secret_dialog(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let existing = match &target {
            Target::Edit(id) => self.thread_settings(cx).and_then(|config| config.settings.secret_providers.get(id)).map(secrets::draft_from),
            Target::Add => None,
        };
        let draft = existing.unwrap_or_default();
        self.secrets.name.update(cx, |input, cx| {
            input.set_value(draft.name, window, cx);
            input.focus(window, cx);
        });
        self.secrets.account.update(cx, |input, cx| input.set_value(draft.account, window, cx));
        self.secrets.remove_dialog = None;
        self.secrets.dialog = Some(SecretDialog { target, submitted: false, focus: cx.focus_handle() });
        cx.notify();
    }

    fn close_secret_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.secrets.dismiss() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// The form's submit: show every problem, or save and close.
    fn save_secret_provider(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.secrets.dialog.as_mut() else {
            return;
        };
        dialog.submitted = true;
        let target = dialog.target.clone();
        let providers = self.thread_settings(cx).map(|config| config.settings.secret_providers.clone()).unwrap_or_default();
        if !target.is_open_in(&providers) {
            self.secrets.dialog = None;
            return cx.notify();
        }
        let existing = providers.ids();
        let draft = self.secret_draft(cx);
        if let Ok((id, config)) = secrets::provider_from_draft(&draft, &existing, &target) {
            store::update_server(&self.state, ServerSettingChange::Prepared(secrets::save_payload(&id, config)), cx);
            self.secrets.dialog = None;
        }
        cx.notify();
    }

    /// `RemoveSecretProviderButton`: asks first, listing settings reading from it; secrets stay untouched.
    fn request_remove_secret_provider(&mut self, id: String, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.secrets.dialog = None;
        self.secrets.remove_dialog = Some(RemoveSecretDialog { id, label: label.to_owned(), focus });
        cx.notify();
    }

    fn confirm_remove_secret_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.secrets.remove_dialog.take() else {
            return;
        };
        store::update_server(&self.state, ServerSettingChange::Prepared(secrets::remove_payload(&dialog.id)), cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn secrets_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        // The web stops rendering an edit/remove whose provider another client removed.
        let providers = self.thread_settings(cx).map(|config| config.settings.secret_providers.clone()).unwrap_or_default();
        if self.secrets.dialog.as_ref().is_some_and(|dialog| !dialog.target.is_open_in(&providers)) {
            self.secrets.dialog = None;
        }
        if self.secrets.remove_dialog.as_ref().is_some_and(|dialog| providers.get(&dialog.id).is_none()) {
            self.secrets.remove_dialog = None;
        }
        let p = palette(cx);
        let environment = self.environment_label(cx);
        let notice = |text: String| {
            Block::new(
                &["secret-providers"],
                v_flex().gap(r(10.)).child(section_title("Secret providers")).child(group(vec![div().px(r(16.)).py(r(12.)).child(muted_text(text, cx)).into_any_element()], cx)),
            )
        };
        let editable = match self.secret_page_state(cx) {
            PageState::Disconnected => return vec![notice(secrets::DISCONNECTED.to_owned())],
            PageState::UpdateRequired => {
                return vec![notice(secrets::update_required(&environment))];
            }
            PageState::Providers { editable } => editable,
        };
        let providers = self.thread_settings(cx).map(|config| config.settings.secret_providers.0.clone()).unwrap_or_default();
        let rows: Vec<AnyElement> = if providers.is_empty() {
            vec![Row::new("secret-providers-empty", secrets::EMPTY_TITLE).description(secrets::EMPTY_DESCRIPTION).into_any_element()]
        } else {
            providers
                .into_iter()
                .map(|provider| {
                    let classified = secrets::classify(&provider);
                    let id = provider.id.clone();
                    let row = Row::new(SharedString::from(format!("secret-provider-{id}")), provider.label.clone());
                    let row = match &classified {
                        Classified::OnePassword { account } => row.title_icon(Icon::OnePassword).description_content(self.redacted_account(&id, account, cx)),
                        Classified::Unsupported => row.description(secrets::unsupported_description(&provider)),
                    };
                    let edit_target = Target::Edit(id.clone());
                    let label = provider.label.clone();
                    let controls = h_flex()
                        .gap(r(4.))
                        .when(classified != Classified::Unsupported, |this| {
                            this.child(
                                Button::new(SharedString::from(format!("secret-provider-edit-{id}")), ButtonVariant::Ghost, ButtonSize::Xs)
                                    .label("Edit")
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_secret_dialog(edit_target.clone(), window, cx))),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("secret-provider-remove-{id}")), ButtonVariant::Ghost, ButtonSize::Xs)
                                .label("Remove")
                                .on_click(cx.listener(move |this, _, window, cx| this.request_remove_secret_provider(id.clone(), &label, window, cx))),
                        );
                    row.when(editable, |row| row.control(controls)).into_any_element()
                })
                .collect()
        };
        let header = h_flex().justify_between().gap(r(16.)).child(section_title("Secret providers")).when(editable, |this| {
            this.child(
                div().pr(r(16.)).child(
                    Button::new("secret-providers-add", ButtonVariant::Outline, ButtonSize::Xs)
                        .icon(Icon::Plus)
                        .label("Add provider")
                        .on_click(cx.listener(|this, _, window, cx| this.open_secret_dialog(Target::Add, window, cx))),
                ),
            )
        });
        let highlighted = self.is_target("secret-providers");
        let section =
            v_flex().id("secret-providers").gap(r(10.)).rounded(r(12.)).when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.3))).child(header).child(group(rows, cx));
        vec![Block::new(&["secret-providers"], section)]
    }

    /// "1Password · <account>", the account redacted until clicked.
    fn redacted_account(&self, id: &str, account: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let revealed = self.secrets.revealed.contains(id);
        let shown = if revealed { account.to_owned() } else { secrets::redacted_placeholder(account) };
        let toggle_id = id.to_owned();
        h_flex()
            .min_w_0()
            .gap(r(4.))
            .child(div().flex_none().child("1Password \u{b7}"))
            .child(
                div()
                    .id(SharedString::from(format!("secret-provider-account-{id}")))
                    .min_w_0()
                    .truncate()
                    .rounded(r(4.))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(r(10.))
                    .cursor_pointer()
                    .hover(|this| this.text_color(hsla(p.foreground)))
                    .child(shown)
                    .tooltip(tooltip(if revealed { "Click to hide account" } else { "Click to reveal account" }.into()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.secrets.revealed.remove(&toggle_id) {
                            this.secrets.revealed.insert(toggle_id.clone());
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub(super) fn secret_dialog_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(dialog) = self.secrets.remove_dialog.as_ref() {
            let p = palette(cx);
            let config = self.thread_settings(cx)?;
            config.settings.secret_providers.get(&dialog.id)?;
            let uses = secrets::secret_provider_uses(&config.settings, &dialog.id);
            let (title, description) = secrets::remove_confirmation(&dialog.label, uses.is_empty());
            let body = if uses.is_empty() {
                div().into_any_element()
            } else {
                v_flex()
                    .id("secret-provider-remove-uses")
                    .max_h(r(192.))
                    .overflow_y_scroll()
                    .gap(r(4.))
                    .px(r(12.))
                    .py(r(8.))
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.muted).opacity(0.4))
                    .children(uses.into_iter().map(|entry| {
                        h_flex()
                            .gap(r(6.))
                            .items_start()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child("\u{2022}")
                            .child(div().min_w_0().child(entry.label))
                    }))
                    .into_any_element()
            };
            let footer = dialog_footer(
                false,
                vec![
                    Button::new("secret-provider-remove-cancel", ButtonVariant::Outline, ButtonSize::Default)
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.close_secret_dialog(window, cx)))
                        .into_any_element(),
                    Button::new("secret-provider-remove-confirm", ButtonVariant::DestructiveOutline, ButtonSize::Default)
                        .label("Remove provider")
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_remove_secret_provider(window, cx)))
                        .into_any_element(),
                ],
                cx,
            );
            let view = cx.entity().downgrade();
            let frame = DialogFrame {
                id: "secret-provider-remove-dialog",
                title: title.into(),
                description: description.into(),
                max_width: 448.,
                body,
                footer: footer.into_any_element(),
                focus: dialog.focus.clone(),
            };
            return Some(frame.render(
                window,
                move |window, cx| {
                    let _ = view.update(cx, |this, cx| this.close_secret_dialog(window, cx));
                },
                cx,
            ));
        }
        let dialog = self.secrets.dialog.as_ref()?;
        let p = palette(cx);
        let config = self.thread_settings(cx);
        if !dialog.target.is_open_in(&config.map(|config| config.settings.secret_providers.clone()).unwrap_or_default()) {
            return None;
        }
        let existing = config.map(|config| config.settings.secret_providers.ids()).unwrap_or_default();
        let editing = match &dialog.target {
            Target::Edit(id) => config.and_then(|config| config.settings.secret_providers.get(id)).map(|provider| provider.label.clone()),
            Target::Add => None,
        };
        let result = secrets::provider_from_draft(&self.secret_draft(cx), &existing, &dialog.target);
        let issues = match (&result, dialog.submitted) {
            (Err(issues), true) => issues.clone(),
            _ => Default::default(),
        };
        let label = |text: &'static str| div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(text);
        let error = |message: Option<String>| message.map(|message| div().text_xs().text_color(hsla(p.destructive_foreground)).child(message));
        let body = v_flex().gap(r(16.)).child(v_flex().gap(r(6.)).child(label("Name")).child(Input::new(&self.secrets.name)).children(error(issues.name))).child(
            v_flex()
                .gap(r(6.))
                .child(label("Account"))
                .child(Input::new(&self.secrets.account))
                .children(error(issues.account))
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(secrets::ACCOUNT_HINT)),
        );
        let footer = dialog_footer(
            true,
            vec![
                Button::new("secret-provider-cancel", ButtonVariant::Outline, ButtonSize::Default)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.close_secret_dialog(window, cx)))
                    .into_any_element(),
                Button::new("secret-provider-save", ButtonVariant::Default, ButtonSize::Default)
                    .label(if editing.is_some() { "Save changes" } else { "Add provider" })
                    .disabled(dialog.submitted && result.is_err())
                    .on_click(cx.listener(|this, _, _, cx| this.save_secret_provider(cx)))
                    .into_any_element(),
            ],
            cx,
        );
        let view = cx.entity().downgrade();
        let frame = DialogFrame {
            id: "secret-provider-dialog",
            title: secrets::dialog_title(&dialog.target, editing.as_deref()).into(),
            description: secrets::dialog_description(&self.environment_label(cx)).into(),
            max_width: 448.,
            body: body.into_any_element(),
            footer: footer.into_any_element(),
            focus: dialog.focus.clone(),
        };
        Some(frame.render(
            window,
            move |window, cx| {
                let _ = view.update(cx, |this, cx| this.close_secret_dialog(window, cx));
            },
            cx,
        ))
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod parity_tests {
    use super::*;
    use gpui::{AppContext, HeadlessAppContext, IntoElement, ParentElement, Render, Styled, div, px, size};
    use serde_json::json;
    use std::sync::Arc;
    use t3_core::settings::search::Section;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().bg(hsla(palette(cx).background)).children(super::super::overlay(window, cx))
        }
    }

    #[test]
    fn remove_dialog_lists_uses_dismisses_and_renders() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let drive = |cx: &mut HeadlessAppContext| {
            cx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            cx.run_until_parked();
        };
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            Arc::new(crate::icons::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.allow_parking();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "orchestration:operate", "settings:write", "providers:manage", "environment:maintain"]
                }))
                .unwrap(),
            ))
            .unwrap();
        drive(&mut cx);
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            cx.global_mut::<super::super::Host>().features.secrets = true;
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "fixture-env",
                            "label": "Studio Mac",
                            "capabilities": { "secretProviderSettings": true, "secretSourceCheck": true }
                        },
                        "secretProviderSettings": true,
                        "secretReferences": true,
                        "settings": {
                            "secretProviders": {
                                "op-main": { "kind": "one_password", "label": "Work 1Password", "config": { "account": "ops.1password.com" } }
                            },
                            "providerInstances": {
                                "codex": {
                                    "driver": "codex",
                                    "displayName": "Codex Main",
                                    "environment": [
                                        { "name": "OPENAI_API_KEY", "value": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/OpenAI/api-key" }, "sensitive": true }
                                    ]
                                }
                            },
                            "cloudSpeechCredentials": {
                                "openai": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/Speech/openai" }
                            },
                            "textGenerators": {
                                "title": { "label": "Title generator", "apiKey": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/Title/key" } }
                            },
                            "simlockDeviceHosts": [
                                { "id": "sim-1", "label": "Simlock Mac", "agentToken": { "kind": "secret", "provider": "op-main", "reference": "op://Dev/Simlock/token" } }
                            ]
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
            });
        });
        let window = cx
            .open_window(size(px(1280.), px(900.)), |_, cx| cx.new(|_| SettingsFrame))
            .unwrap();
        cx.update(|cx| super::super::open(Section::Secrets, cx));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();

        let view = cx.update(|cx| cx.global::<super::super::Host>().view.clone().expect("settings view open"));
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                let config = view.thread_settings(cx).unwrap();
                let uses = secrets::secret_provider_uses(&config.settings, "op-main");
                assert_eq!(uses.len(), 4);
                view.request_remove_secret_provider("op-main".into(), "Work 1Password", window, cx);
                assert!(view.secrets.remove_dialog.is_some());
            });
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();

        if let Some(out) = std::env::var_os("T3_PARITY_386_SHOTS_DIR") {
            let out = std::path::PathBuf::from(out);
            std::fs::create_dir_all(&out).unwrap();
            cx.capture_screenshot(window.into()).unwrap().save(out.join("secrets-remove-dialog.png")).unwrap();
        }

        // Escape dismisses the remove dialog without closing Settings.
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_action(Box::new(super::super::CloseSettings), cx);
        })
        .unwrap();
        cx.run_until_parked();
        view.read_with(&cx, |view, _| {
            assert!(view.secrets.remove_dialog.is_none());
        });

        state.update(&mut cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
