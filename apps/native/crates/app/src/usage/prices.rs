//! Model prices dialog (`UsagePriceOverrides.tsx`): reads `usagePriceOverrides` and
//! `usageModelAliases` via `server.getConfig`, validates edits with `t3_core::usage::price_overrides`,
//! and writes patches through `server.updateSettings`.

use std::collections::BTreeMap;

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputState};
use gpui_component::{h_flex, v_flex};
use t3_core::commands::method::SERVER_UPDATE_SETTINGS;
use t3_core::usage::price_overrides::{
    GET_CONFIG, UsagePriceForm, UsagePriceRowDraft, UsagePriceSettings, decode_usage_price_settings,
    usage_price_patch_payload, usage_price_table_changes,
};

use super::UsageView;
use super::controls::{ActionButton, ButtonSize, ButtonVariant, r};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

struct PriceRowInputs {
    id: String,
    is_new: bool,
    removed: bool,
    /// Immutable model ID for existing (`!is_new`) rows so saved server keys are never orphaned.
    fixed_model: String,
    model: Option<Entity<InputState>>,
    input: Entity<InputState>,
    output: Entity<InputState>,
    cache_read: Entity<InputState>,
    cache_write: Entity<InputState>,
    alias: Entity<InputState>,
}

pub(super) struct PriceOverridesDialog {
    loading: bool,
    saving: bool,
    error: Option<String>,
    row_errors: BTreeMap<String, String>,
    settings: UsagePriceSettings,
    rows: Vec<PriceRowInputs>,
    next_row_id: usize,
    seed_model: Option<String>,
}

impl PriceOverridesDialog {
    pub(super) fn is_saving(&self) -> bool {
        self.saving
    }

    fn new(seed_model: Option<String>) -> Self {
        Self {
            loading: true,
            saving: false,
            error: None,
            row_errors: BTreeMap::new(),
            settings: UsagePriceSettings::default(),
            rows: Vec::new(),
            next_row_id: 1,
            seed_model,
        }
    }

    fn push_row(
        &mut self,
        model: &str,
        form: &UsagePriceForm,
        alias: &str,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<UsageView>,
    ) {
        let id = format!("price-row-{}", self.next_row_id);
        self.next_row_id += 1;
        let mk = |value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<UsageView>| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                if !value.is_empty() {
                    state.set_value(value, window, cx);
                }
                state
            })
        };
        self.rows.push(PriceRowInputs {
            id,
            is_new,
            removed: false,
            fixed_model: model.to_owned(),
            model: is_new.then(|| mk(model, "model-id", window, cx)),
            input: mk(&form.input, "0.00", window, cx),
            output: mk(&form.output, "0.00", window, cx),
            cache_read: mk(&form.cache_read, "Input rate", window, cx),
            cache_write: mk(&form.cache_write, "Input rate", window, cx),
            alias: mk(alias, "None", window, cx),
        });
    }

    fn drafts(&self, cx: &gpui::App) -> Vec<UsagePriceRowDraft> {
        self.rows
            .iter()
            .map(|row| {
                let model = match &row.model {
                    Some(input) if row.is_new => input.read(cx).value().to_string(),
                    _ => row.fixed_model.clone(),
                };
                UsagePriceRowDraft {
                    id: row.id.clone(),
                    model: model.clone(),
                    is_new: row.is_new,
                    form: UsagePriceForm {
                        model,
                        input: row.input.read(cx).value().to_string(),
                        output: row.output.read(cx).value().to_string(),
                        cache_read: row.cache_read.read(cx).value().to_string(),
                        cache_write: row.cache_write.read(cx).value().to_string(),
                    },
                    alias: row.alias.read(cx).value().to_string(),
                    removed: row.removed,
                }
            })
            .collect()
    }
}

impl UsageView {
    pub(super) fn open_price_overrides(&mut self, seed_model: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.prices = Some(PriceOverridesDialog::new(seed_model));
        let request = self.state.read(cx).rpc(GET_CONFIG, serde_json::json!({}));
        cx.spawn_in(window, async move |this, cx| {
            let reply = request.await;
            let _ = this.update_in(cx, |view, window, cx| {
                let Some(mut dialog) = view.prices.take() else { return };
                dialog.loading = false;
                match reply {
                    Ok(config) => {
                        let settings = decode_usage_price_settings(&config);
                        let mut models = std::collections::BTreeSet::new();
                        models.extend(settings.prices.keys().cloned());
                        models.extend(settings.aliases.keys().cloned());
                        for model in &models {
                            let form = UsagePriceForm::from_override(model.as_str(), settings.prices.get(model));
                            let alias = settings.aliases.get(model).map(String::as_str).unwrap_or("");
                            dialog.push_row(model, &form, alias, false, window, cx);
                        }
                        if let Some(seed) = dialog.seed_model.clone()
                            && !models.contains(&seed)
                        {
                            let form = UsagePriceForm::from_override(seed.as_str(), None);
                            dialog.push_row(&seed, &form, "", true, window, cx);
                        }
                        if dialog.rows.is_empty() {
                            dialog.push_row("", &UsagePriceForm::default(), "", true, window, cx);
                        }
                        dialog.settings = settings;
                    }
                    Err(error) => {
                        dialog.error = Some(format!("Could not load model prices: {error}"));
                    }
                }
                view.prices = Some(dialog);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn add_price_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prices.as_ref().is_none_or(|d| d.saving) {
            return;
        }
        let Some(mut dialog) = self.prices.take() else { return };
        dialog.push_row("", &UsagePriceForm::default(), "", true, window, cx);
        self.prices = Some(dialog);
        cx.notify();
    }

    fn save_price_overrides(&mut self, cx: &mut Context<Self>) {
        let label = self.data.label.clone();
        let (changes, errors, aliases_supported) = {
            let Some(dialog) = self.prices.as_ref() else { return };
            if dialog.saving || dialog.loading {
                return;
            }
            let drafts = dialog.drafts(cx);
            let (changes, errors) = usage_price_table_changes(&label, &dialog.settings, &drafts);
            (changes, errors, dialog.settings.aliases_supported)
        };
        if !errors.is_empty() {
            if let Some(dialog) = self.prices.as_mut() {
                dialog.row_errors = errors;
            }
            cx.notify();
            return;
        }
        let has_alias_change = changes.iter().any(|c| matches!(c, t3_core::usage::price_overrides::UsagePriceChange::Alias { .. }));
        if has_alias_change && !aliases_supported {
            if let Some(dialog) = self.prices.as_mut() {
                dialog.error = Some("Update server to map models.".to_owned());
            }
            cx.notify();
            return;
        }
        let Some(payload) = usage_price_patch_payload(&changes) else {
            self.prices = None;
            cx.notify();
            return;
        };
        if let Some(dialog) = self.prices.as_mut() {
            dialog.saving = true;
            dialog.error = None;
            dialog.row_errors.clear();
        }
        let request = self.state.read(cx).rpc(SERVER_UPDATE_SETTINGS, payload);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(_) => {
                    view.prices = None;
                    view.load(true, cx);
                    cx.notify();
                }
                Err(error) => {
                    if let Some(dialog) = view.prices.as_mut() {
                        dialog.saving = false;
                        dialog.error = Some(format!("Could not save: {error}"));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_price_overrides_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.prices.as_ref()?;
        let p = palette(cx);
        let loading = dialog.loading;
        let saving = dialog.saving;
        let error = dialog.error.clone();
        let row_views: Vec<AnyElement> = dialog
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.removed)
            .map(|(index, row)| {
                let row_err = dialog.row_errors.get(&row.id).cloned();
                v_flex()
                    .gap(r(4.))
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .items_center()
                            .child(match &row.model {
                                Some(input) if row.is_new => {
                                    div().flex_1().min_w(r(140.)).child(Input::new(input).small()).into_any_element()
                                }
                                _ => div()
                                    .flex_1()
                                    .min_w(r(140.))
                                    .px(r(8.))
                                    .py(r(5.))
                                    .rounded(r(6.))
                                    .bg(hsla(p.muted).opacity(0.45))
                                    .text_size(r(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .truncate()
                                    .child(row.fixed_model.clone())
                                    .into_any_element(),
                            })
                            .child(div().w(r(76.)).child(Input::new(&row.input).small()))
                            .child(div().w(r(76.)).child(Input::new(&row.output).small()))
                            .child(div().w(r(84.)).child(Input::new(&row.cache_read).small()))
                            .child(div().w(r(84.)).child(Input::new(&row.cache_write).small()))
                            .child(div().w(r(116.)).child(Input::new(&row.alias).small()))
                            .child(
                                Button::new(SharedString::from(format!("usage-price-remove-{index}")))
                                    .variant(Variant::Ghost)
                                    .size(Size::IconSm)
                                    .icon(Icon::X)
                                    .disabled(saving)
                                    .tooltip("Remove override")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(d) = this.prices.as_mut()
                                            && !d.saving
                                            && let Some(r) = d.rows.get_mut(index)
                                        {
                                            r.removed = true;
                                            d.row_errors.remove(&r.id);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(row_err, |this, msg| {
                        this.child(div().text_size(r(11.)).text_color(hsla(p.destructive)).child(msg))
                    })
                    .into_any_element()
            })
            .collect();

        let card = v_flex()
            .id("usage-prices-dialog")
            .relative()
            .w_full()
            .max_w(r(860.))
            .max_h_full()
            .rounded(r(16.))
            .overflow_hidden()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.65),
                offset: gpui::point(gpui::px(0.), gpui::px(24.)),
                blur_radius: gpui::px(64.),
                spread_radius: gpui::px(-24.),
                inset: false,
            }])
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .child(
                v_flex()
                    .relative()
                    .gap(r(4.))
                    .p(r(20.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(div().text_size(r(18.)).font_weight(FontWeight::SEMIBOLD).child("Model prices"))
                    .child(
                        div()
                            .text_size(r(12.))
                            .text_color(hsla(p.muted_foreground))
                            .child("Override USD per 1M tokens or map a model ID to another model's rates. Blank cache fields inherit Input."),
                    ),
            )
            .child(
                v_flex()
                    .id("usage-prices-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(r(10.))
                    .p(r(20.))
                    .when_some(error, |this, msg| {
                        this.child(div().text_size(r(12.)).text_color(hsla(p.destructive)).child(msg))
                    })
                    .when(loading, |this| {
                        this.child(div().text_size(r(13.)).text_color(hsla(p.muted_foreground)).child("Loading model prices…"))
                    })
                    .when(!loading, |this| {
                        this.child(
                            h_flex()
                                .gap(r(6.))
                                .text_size(r(11.))
                                .text_color(hsla(p.muted_foreground))
                                .child(div().flex_1().min_w(r(140.)).child("Model"))
                                .child(div().w(r(76.)).child("Input"))
                                .child(div().w(r(76.)).child("Output"))
                                .child(div().w(r(84.)).child("Cache read"))
                                .child(div().w(r(84.)).child("Cache write"))
                                .child(div().w(r(116.)).child("Map to"))
                                .child(div().w(r(28.))),
                        )
                        .children(row_views)
                        .child(
                            h_flex().pt(r(4.)).child(
                                ActionButton::new("usage-prices-add", "Add model", ButtonVariant::Outline, ButtonSize::Xs)
                                    .disabled(saving)
                                    .on_click(cx.listener(|this, _, window, cx| this.add_price_row(window, cx))),
                            ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .relative()
                    .justify_end()
                    .gap(r(8.))
                    .px(r(20.))
                    .py(r(14.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.muted))
                    .child(
                        ActionButton::new("usage-prices-cancel", "Cancel", ButtonVariant::Outline, ButtonSize::Default)
                            .disabled(saving)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.prices.as_ref().is_some_and(|d| d.saving) {
                                    return;
                                }
                                this.prices = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        ActionButton::new(
                            "usage-prices-save",
                            if saving { "Saving…" } else { "Save" },
                            ButtonVariant::Default,
                            ButtonSize::Default,
                        )
                        .disabled(loading || saving)
                        .on_click(cx.listener(|this, _, _, cx| this.save_price_overrides(cx))),
                    ),
            );

        Some(
            div()
                .id("usage-prices-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(r(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if this.prices.as_ref().is_some_and(|d| d.saving) {
                            return;
                        }
                        this.prices = None;
                        cx.notify();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }
}
