//! The shared source picker, value field and committed-reference check from SecretSourceInput.tsx.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use gpui::{
    App, ElementId, Entity, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window,
    div, prelude::*,
};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputState},
    v_flex,
};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::settings::{
    providers::form,
    secrets::{self, Classified, SecretProvider, SecretSourceCheckResult},
};

use crate::components::motion::Spinner;
use crate::icons::Icon;
use crate::settings::controls::{
    Button, ButtonSize, ButtonVariant, Select, SelectOption, r, tooltip,
};
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette, theme_slot_clipped_background_layer};

const DIRECT_VALUE: &str = "";

type ChangeSource = Rc<dyn Fn(Option<SharedString>, &mut Window, &mut App)>;
type CheckFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>>>>;
type CheckRpc = Rc<dyn Fn(&'static str, Value, &mut App) -> CheckFuture>;

#[derive(PartialEq, Eq)]
struct CheckKey {
    environment: SharedString,
    provider: SharedString,
    reference: SharedString,
}

enum CheckStatus {
    Idle,
    Checking,
    Resolved,
    Failed(String),
}

/// A committed reference and a permission-checked RPC bound to its environment.
pub struct SecretSourceCheck {
    key: Rc<CheckKey>,
    can_check: bool,
    rpc: CheckRpc,
}

impl SecretSourceCheck {
    /// Supplies the destination and transport for checking this reference without reading its value.
    #[allow(
        dead_code,
        reason = "Shared foundation for the native parity consumers"
    )]
    pub fn new<F>(
        environment: impl Into<SharedString>,
        provider: impl Into<SharedString>,
        reference: impl Into<SharedString>,
        can_check: bool,
        rpc: impl Fn(&'static str, Value, &mut App) -> F + 'static,
    ) -> Self
    where
        F: Future<Output = Result<Value, RpcError>> + 'static,
    {
        Self {
            key: Rc::new(CheckKey {
                environment: environment.into(),
                provider: provider.into(),
                reference: reference.into(),
            }),
            can_check,
            rpc: Rc::new(move |method, payload, cx| Box::pin(rpc(method, payload, cx))),
        }
    }

    fn render(
        self,
        id: ElementId,
        input: Entity<InputState>,
        disabled: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let p = palette(cx);
        let key = self.key;
        let state = window.use_keyed_state((id.clone(), "check"), cx, |_, _| {
            RefCell::new((key.clone(), CheckStatus::Idle))
        });
        let (glyph, color, hint) = {
            let mut current = state.read(cx).borrow_mut();
            if current.0 != key {
                *current = (key.clone(), CheckStatus::Idle);
            }
            if !self.can_check {
                (
                    Icon::RefreshCw,
                    p.muted_foreground,
                    "You cannot check secrets on this environment".into(),
                )
            } else {
                match &current.1 {
                    CheckStatus::Checking => {
                        let hint = crate::i18n::t(cx, "Checking the secret").to_owned().into();
                        return div()
                            .id(id.clone())
                            .size(r(20.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .tooltip(tooltip(hint))
                            .child(Spinner::new(
                                id,
                                r(12.).to_pixels(window.rem_size()),
                                hsla(p.muted_foreground),
                            ))
                            .into_any_element();
                    }
                    CheckStatus::Idle => (
                        Icon::RefreshCw,
                        p.muted_foreground,
                        "Check that the secret resolves".into(),
                    ),
                    CheckStatus::Resolved => {
                        (Icon::Check, p.success_foreground, "Secret resolved".into())
                    }
                    CheckStatus::Failed(detail) => {
                        (Icon::TriangleAlert, p.destructive, detail.clone())
                    }
                }
            }
        };
        Button::new(id, ButtonVariant::GhostMuted, ButtonSize::IconMicro)
            .icon(glyph)
            .icon_color(hsla(color))
            .tooltip(hint)
            .disabled(disabled || !self.can_check)
            .on_click(move |_, window, cx| {
                if input.read(cx).value().trim() != key.reference.as_ref() {
                    return;
                }
                let Some(key) = state.update(cx, |state, cx| {
                    let current = state.get_mut();
                    if current.0 != key || matches!(current.1, CheckStatus::Checking) {
                        return None;
                    }
                    current.1 = CheckStatus::Checking;
                    cx.notify();
                    Some(current.0.clone())
                }) else {
                    return;
                };
                let payload = secrets::check_secret_source_payload(&key.provider, &key.reference);
                let request = (self.rpc)(secrets::CHECK_SECRET_SOURCE, payload, cx);
                let state = state.downgrade();
                window
                    .spawn(cx, async move |cx| {
                        let status = match request.await {
                            Ok(value) => match secrets::decode_check_secret_source_result(&value) {
                                SecretSourceCheckResult::Resolved => CheckStatus::Resolved,
                                SecretSourceCheckResult::Failed(detail) => {
                                    CheckStatus::Failed(detail)
                                }
                            },
                            Err(_) => CheckStatus::Failed("Could not reach the server".to_owned()),
                        };
                        let _ = cx.update(|_, cx| {
                            let Some(state) = state.upgrade() else {
                                return;
                            };
                            state.update(cx, |state, cx| {
                                let current = state.get_mut();
                                if Rc::ptr_eq(&current.0, &key) {
                                    current.1 = status;
                                    cx.notify();
                                }
                            });
                        });
                    })
                    .detach();
            })
            .into_any_element()
    }
}

/// A controlled source picker and secret field for settings backed by an InputState.
#[derive(IntoElement)]
pub struct SecretSourceInput {
    id: ElementId,
    label: SharedString,
    input: Entity<InputState>,
    options: Vec<SelectOption>,
    selected: Option<SharedString>,
    one_password: bool,
    disabled: bool,
    on_source_change: Option<ChangeSource>,
    error: Option<SharedString>,
    check: Option<SecretSourceCheck>,
}

#[allow(
    dead_code,
    reason = "Shared foundation for the native parity consumers"
)]
impl SecretSourceInput {
    /// Uses the caller's input and current source; pass the providers supported by the destination.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        input: &Entity<InputState>,
        providers: &[SecretProvider],
        selected: Option<&str>,
    ) -> Self {
        let mut options = vec![SelectOption::new(DIRECT_VALUE, "Value").icon(Icon::Type)];
        options.extend(
            providers
                .iter()
                .filter(|provider| {
                    matches!(secrets::classify(provider), Classified::OnePassword { .. })
                })
                .map(|provider| {
                    SelectOption::new(provider.id.clone(), provider.label.clone())
                        .icon(Icon::OnePassword)
                }),
        );
        if let Some(selected) =
            selected.filter(|id| !options.iter().any(|option| option.value.as_ref() == *id))
        {
            options.push(
                SelectOption::new(selected.to_owned(), selected.to_owned()).icon(Icon::KeyRound),
            );
        }
        Self {
            id: id.into(),
            label: label.into(),
            input: input.clone(),
            options,
            selected: selected.map(|id| id.to_owned().into()),
            one_password: providers.iter().any(|provider| {
                Some(provider.id.as_str()) == selected && provider.kind == secrets::ONE_PASSWORD
            }),
            disabled: false,
            on_source_change: None,
            error: None,
            check: None,
        }
    }

    /// Disables editing, switching sources and checking the reference.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handles source selection; None means a value typed directly into the field.
    pub fn on_source_change(
        mut self,
        handler: impl Fn(Option<SharedString>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_source_change = Some(Rc::new(handler));
        self
    }

    /// Shows validation feedback from the caller's reference commit handler.
    pub fn error(mut self, error: impl Into<SharedString>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Enables verification for a committed reference on a server that supports the check.
    pub fn check(mut self, check: SecretSourceCheck) -> Self {
        self.check = Some(check);
        self
    }
}

impl RenderOnce for SecretSourceInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let is_reference = self.selected.is_some();
        let label = crate::i18n::t(cx, self.label.as_ref()).to_owned();
        let picker = (self.options.len() > 1).then(|| {
            Select::new((self.id.clone(), "provider"), self.options)
                .selected(self.selected.clone().unwrap_or_default())
                .show_check()
                .disabled(self.disabled || self.on_source_change.is_none())
                .on_change(move |id, window, cx| {
                    if let Some(handler) = &self.on_source_change {
                        handler((id.as_ref() != DIRECT_VALUE).then_some(id), window, cx);
                    }
                })
        });
        let check = self
            .check
            .filter(|check| {
                self.selected.as_ref() == Some(&check.key.provider)
                    && self.error.is_none()
                    && self.input.read(cx).value().trim() == check.key.reference.as_ref()
                    && validate_reference(self.one_password, &check.key.reference).is_ok()
            })
            .map(|check| {
                check.render(
                    (self.id.clone(), "verify").into(),
                    self.input.clone(),
                    self.disabled,
                    window,
                    cx,
                )
            });
        let background = theme_slot_clipped_background_layer(ThemeSlot::Input, r(8.), cx);
        let mut field = Input::new(&self.input)
            .small()
            .disabled(self.disabled)
            .aria_label(label)
            .appearance(background.is_none())
            .when(!is_reference, |field| field.mask_toggle())
            .theme_slot(ThemeSlot::Input, cx);
        if is_reference {
            field = field.font_family(crate::theme::font_family(
                t3_core::client_settings::typography::FontRole::Code,
                cx,
            ));
        }
        h_flex()
            .id(self.id)
            .items_start()
            .min_w_0()
            .flex_1()
            .gap(r(6.))
            .children(picker)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(4.))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(r(6.))
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .children(background)
                                    .child(field),
                            )
                            .children(check),
                    )
                    .children(self.error.map(|error| {
                        div()
                            .text_xs()
                            .text_color(hsla(p.destructive))
                            .child(crate::i18n::t(cx, error.as_ref()).to_owned())
                    })),
            )
    }
}

/// Validates a draft using the same reference rules as the settings models.
#[allow(
    dead_code,
    reason = "Shared foundation for the native parity consumers"
)]
pub fn validate_reference(one_password: bool, text: &str) -> Result<(), &'static str> {
    if one_password && !form::valid_one_password_reference(text) {
        return Err("Use a 1Password reference like op://vault/item/field.");
    }
    if !form::valid_reference(text) {
        return Err("Enter a valid secret reference.");
    }
    Ok(())
}
