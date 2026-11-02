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

pub(crate) struct SecretsState {
    name: Entity<InputState>,
    account: Entity<InputState>,
    dialog: Option<SecretDialog>,
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
        SecretsState { name, account, dialog: None, revealed: HashSet::new(), _subscriptions: subscriptions }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        [&self.name, &self.account].into_iter()
    }

    /// Escape closes the dialog before it closes settings (`SettingsView::on_close`).
    pub(super) fn dismiss(&mut self) -> bool {
        self.dialog.take().is_some()
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
        let existing = self.thread_settings(cx).map(|config| config.settings.secret_providers.ids()).unwrap_or_default();
        let draft = self.secret_draft(cx);
        if let Ok((id, config)) = secrets::provider_from_draft(&draft, &existing, &target) {
            store::update_server(&self.state, ServerSettingChange::Prepared(secrets::save_payload(&id, config)), cx);
            self.secrets.dialog = None;
        }
        cx.notify();
    }

    /// `RemoveSecretProviderButton`: asks first; the secrets themselves are untouched.
    fn request_remove_secret_provider(&mut self, id: String, label: &str, cx: &mut Context<Self>) {
        let (title, description) = secrets::remove_confirmation(label);
        let state = self.state.clone();
        crate::dialog::ask(
            title,
            description,
            "Remove provider",
            move |_, cx| {
                store::update_server(&state, ServerSettingChange::Prepared(secrets::remove_payload(&id)), cx);
            },
            cx,
        );
    }

    pub(super) fn secrets_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
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
                                .on_click(cx.listener(move |this, _, _, cx| this.request_remove_secret_provider(id.clone(), &label, cx))),
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
        let dialog = self.secrets.dialog.as_ref()?;
        let p = palette(cx);
        let config = self.thread_settings(cx);
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
