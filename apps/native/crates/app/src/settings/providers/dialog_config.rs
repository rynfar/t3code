//! Draft controls for `AddProviderInstanceDialog.tsx`; uses the editor's `form::fields` metadata.
use super::super::{
    SettingsView,
    controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, focus_handle, r},
};
use crate::{
    icons::Icon,
    theme::{hsla, palette},
};
use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use serde_json::{Value, json};
use t3_core::settings::providers::{form, wizard};

#[derive(IntoElement)]
struct FocusConfigSelect {
    id: gpui::ElementId,
    select: Select,
    focus: bool,
}
impl RenderOnce for FocusConfigSelect {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        // Select's focus handle is keyed in its element scope.
        if self.focus {
            focus_handle(&self.id, window, cx).focus(window, cx);
        }
        self.select.render(window, cx)
    }
}

pub(super) struct DraftInput {
    key: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}
#[derive(Default)]
pub(super) struct ConfigDraft {
    pub(super) model: wizard::ConfigDraft,
    pub(super) fields: Vec<DraftInput>,
}
impl ConfigDraft {
    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        self.fields.iter().map(|f| &f.input)
    }
}

impl SettingsView {
    pub(super) fn sync_dialog_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.providers.dialog.as_mut() else {
            return;
        };
        let driver = dialog.driver().kind;
        let draft = dialog.configs.entry(driver).or_default();
        let fields = form::fields(driver, draft.model.value.as_ref());
        for field in fields.into_iter().filter(|f| f.options.is_empty()) {
            if self.providers.dialog.as_ref().unwrap().configs[driver]
                .fields
                .iter()
                .any(|f| f.key == field.key)
            {
                continue;
            }
            let key = field.key.clone();
            let saved = self.providers.dialog.as_ref().unwrap().configs[driver]
                .model
                .value
                .as_ref()
                .and_then(|v| v[&key].as_str())
                .unwrap_or("")
                .to_owned();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(saved)
                    .placeholder(field.placeholder)
                    .masked(field.secret)
            });
            let subscription = cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.edit_dialog_config(
                            driver,
                            &key,
                            json!(input.read(cx).value().as_ref()),
                            cx,
                        );
                    }
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.advance_dialog(window, cx);
                    }
                },
            );
            self.providers
                .dialog
                .as_mut()
                .unwrap()
                .configs
                .get_mut(driver)
                .unwrap()
                .fields
                .push(DraftInput {
                    key: field.key,
                    input,
                    _subscription: subscription,
                });
        }
    }

    fn edit_dialog_config(
        &mut self,
        driver: &'static str,
        key: &str,
        value: Value,
        cx: &mut Context<Self>,
    ) {
        if self
            .form_denial(driver, &format!("config.{key}"), cx)
            .is_some()
        {
            return;
        }
        let Some(dialog) = self
            .providers
            .dialog
            .as_mut()
            .filter(|d| !d.saving() && d.driver().kind == driver)
        else {
            return;
        };
        let Some(draft) = dialog.configs.get_mut(driver) else {
            return;
        };
        draft.model.edit(driver, key, value);
        if driver == "acpRegistry" && key == "source" {
            draft.fields.clear();
        }
        dialog.error = None;
        cx.notify();
    }

    pub(super) fn render_dialog_config(
        &self,
        focus_key: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dialog = self.providers.dialog.as_ref().unwrap();
        let driver = dialog.driver().kind;
        let draft = &dialog.configs[driver];
        let fields = form::fields(driver, draft.model.value.as_ref());
        if fields.is_empty() {
            return div()
                .text_sm()
                .text_color(hsla(palette(cx).muted_foreground))
                .child("This driver has no required configuration. You can add the instance now.")
                .into_any_element();
        }
        let mut rows = Vec::new();
        for field in fields {
            let key = field.key.clone();
            let denial = self.form_denial(driver, &format!("config.{key}"), cx);
            let disabled = dialog.saving() || denial.is_some();
            let saved = draft
                .model
                .value
                .as_ref()
                .and_then(|c| c.get(&key))
                .and_then(Value::as_str)
                .unwrap_or("");
            let control = if !field.options.is_empty() {
                let fallback = field.options[0].0.clone();
                let current = if saved.is_empty() {
                    fallback.clone()
                } else {
                    saved.into()
                };
                let label = field
                    .options
                    .iter()
                    .find(|(v, _)| v == &current)
                    .map_or(current.clone(), |(_, l)| l.clone());
                if disabled {
                    div()
                        .w(r(224.))
                        .opacity(0.64)
                        .child(label)
                        .into_any_element()
                } else {
                    let edit_key = key.clone();
                    let view = cx.entity().downgrade();
                    let id: gpui::ElementId =
                        SharedString::from(format!("wizard-config-{driver}-{key}")).into();
                    let select = Select::new(
                        id.clone(),
                        field
                            .options
                            .into_iter()
                            .map(|(v, l)| SelectOption::new(v, l))
                            .collect(),
                    )
                    .selected(current)
                    .label(label)
                    .width(224.)
                    .on_change(move |value, window, cx| {
                        let value = if value.as_ref() == fallback {
                            ""
                        } else {
                            value.as_ref()
                        };
                        let _ = view.update(cx, |this, cx| {
                            this.edit_dialog_config(driver, &edit_key, json!(value), cx);
                            this.sync_dialog_config(window, cx);
                        });
                    });
                    FocusConfigSelect {
                        id,
                        select,
                        focus: focus_key == Some(key.as_str()),
                    }
                    .into_any_element()
                }
            } else {
                let input = draft
                    .fields
                    .iter()
                    .find(|f| f.key == key)
                    .unwrap()
                    .input
                    .clone();
                if focus_key == Some(key.as_str()) {
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
                h_flex()
                    .gap(r(8.))
                    .child(
                        div()
                            .w(r(224.))
                            .child(Input::new(&input).small().disabled(disabled)),
                    )
                    .when(field.secret && !saved.is_empty(), |el| {
                        el.child(
                            Button::new(
                                SharedString::from(format!("wizard-clear-{driver}-{key}")),
                                ButtonVariant::GhostMuted,
                                ButtonSize::IconSm,
                            )
                            .icon(Icon::X)
                            .tooltip(format!("Clear {}", field.label))
                            .disabled(disabled)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.edit_dialog_config(driver, &key, json!(""), cx);
                                    input.update(cx, |input, cx| {
                                        input.focus(window, cx);
                                        input.set_value("", window, cx);
                                    });
                                },
                            )),
                        )
                    })
                    .into_any_element()
            };
            rows.push(
                Row::new(
                    SharedString::from(format!("wizard-field-{driver}-{}", field.key)),
                    field.label,
                )
                .description(denial.unwrap_or(field.description))
                .control(control)
                .into_any_element(),
            );
        }
        v_flex()
            .children(rows.into_iter().enumerate().map(|(i, row)| {
                div()
                    .when(i > 0, |el| {
                        el.border_t_1()
                            .border_color(hsla(palette(cx).border).opacity(0.5))
                    })
                    .child(row)
            }))
            .children(draft.model.error().map(|error| {
                div()
                    .text_xs()
                    .text_color(hsla(palette(cx).destructive_foreground))
                    .child(error.to_owned())
            }))
            .into_any_element()
    }
}
