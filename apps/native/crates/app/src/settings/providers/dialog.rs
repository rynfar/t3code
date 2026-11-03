//! Add provider (`AddProviderInstanceDialog.tsx`, `AddProviderInstanceWizardSteps.tsx`): pick a
//! built-in driver, then name the instance; its id follows the label until the user edits it.
//! The manual path starts from driver defaults; Codex also offers managed ChatGPT accounts.
//! The driver configuration step and ACP Registry search are not ported.

use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, FontWeight, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div,
    point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use t3_core::commands::method;
use t3_core::settings::providers::{self as model, Driver};

use super::super::SettingsView;
use super::super::controls::{Button, ButtonSize, ButtonVariant, Row, icon, r};
use super::{CloseProviderDialog, failure_text};
use crate::components::badge::{BadgeKind, badge};
use crate::icons::Icon;
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::theme::{hsla, palette};

const STEPS: [&str; 2] = ["Provider", "Identity"];

pub(super) struct AddDialog {
    step: usize,
    driver: &'static Driver,
    show_more: bool,
    label: Entity<InputState>,
    instance_id: Entity<InputState>,
    accent: Entity<InputState>,
    /// The user typed their own id, so the label no longer drives it.
    id_overridden: bool,
    attempted: bool,
    saving: bool,
    error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl AddDialog {
    pub(super) fn inputs(&self) -> [&Entity<InputState>; 3] {
        [&self.label, &self.instance_id, &self.accent]
    }
}

impl SettingsView {
    fn add_driver_denial(&self, driver: &Driver, cx: &gpui::App) -> Option<String> {
        let app = self.state.read(cx);
        t3_core::managed_policy::driver_denial(app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()), driver.kind, driver.label)
    }

    fn check_dialog_policy(&mut self, cx: &mut Context<Self>) -> bool {
        let denial = self.providers.dialog.as_ref().and_then(|dialog| self.add_driver_denial(dialog.driver, cx));
        if let Some(denial) = denial {
            if let Some(dialog) = self.providers.dialog.as_mut() { dialog.error = Some(denial); }
            cx.notify();
            return false;
        }
        true
    }

    pub(super) fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let driver = {
            let app = self.state.read(cx);
            let policy = app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref());
            model::offered_drivers(true, policy).next().or_else(|| model::offered_drivers(false, policy).next())
        };
        let Some(driver) = driver else { return; };
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Work"));
        let instance_id = cx.new(|cx| InputState::new(window, cx));
        let accent = cx.new(|cx| InputState::new(window, cx).placeholder("#2563eb"));
        let subscriptions = vec![
            cx.subscribe_in(&label, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.sync_dialog_id(window, cx),
                InputEvent::PressEnter { .. } => this.advance_dialog(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&instance_id, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let typed = input.read(cx).value().to_string();
                    let proposed = this.proposed_dialog_id(cx);
                    if let Some(dialog) = this.providers.dialog.as_mut() {
                        dialog.id_overridden = proposed.as_deref() != Some(typed.as_str());
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => this.advance_dialog(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&accent, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.advance_dialog(window, cx),
                _ => {}
            }),
        ];
        let focus = cx.focus_handle();
        self.providers.dialog = Some(AddDialog {
            step: 0,
            driver,
            show_more: false,
            label,
            instance_id,
            accent,
            id_overridden: false,
            attempted: false,
            saving: false,
            error: None,
            focus: focus.clone(),
            _subscriptions: subscriptions,
        });
        self.select_dialog_driver(driver, window, cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.providers.dialog.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Each driver starts from its own label, as the web keeps one identity draft per driver.
    fn select_dialog_driver(&mut self, driver: &'static Driver, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_driver_denial(driver, cx).is_some() { return; }
        let Some(dialog) = self.providers.dialog.as_mut() else { return };
        dialog.driver = driver;
        dialog.id_overridden = false;
        dialog.attempted = false;
        dialog.error = None;
        let (label, accent) = (dialog.label.clone(), dialog.accent.clone());
        label.update(cx, |input, cx| input.set_value(driver.label, window, cx));
        accent.update(cx, |input, cx| input.set_value("", window, cx));
        let placeholder = format!("{}_work", driver.kind);
        if let Some(dialog) = self.providers.dialog.as_ref() {
            dialog.instance_id.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
        self.sync_dialog_id(window, cx);
        cx.notify();
    }

    fn existing_instance_ids(&self, cx: &gpui::App) -> Vec<String> {
        let app = self.state.read(cx);
        app.server_config.config.as_ref().map(|config| model::reserved_instance_ids(&config.settings)).unwrap_or_default()
    }

    fn proposed_dialog_id(&self, cx: &gpui::App) -> Option<String> {
        let dialog = self.providers.dialog.as_ref()?;
        let label = dialog.label.read(cx).value().to_string();
        Some(model::proposed_instance_id(dialog.driver, &label, &self.existing_instance_ids(cx)))
    }

    /// Keeps the id field on the label's proposal until the user types their own.
    fn sync_dialog_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(proposed) = self.proposed_dialog_id(cx) else { return };
        let Some(dialog) = self.providers.dialog.as_ref() else { return };
        if dialog.id_overridden || dialog.instance_id.read(cx).value().as_ref() == proposed {
            return;
        }
        let input = dialog.instance_id.clone();
        input.update(cx, |input, cx| input.set_value(proposed, window, cx));
        cx.notify();
    }

    fn dialog_id_error(&self, cx: &gpui::App) -> Option<String> {
        let dialog = self.providers.dialog.as_ref()?;
        let id = dialog.instance_id.read(cx).value().trim().to_owned();
        model::validate_instance_id(&id, &self.existing_instance_ids(cx))
    }

    /// Next, or Add instance on the last step. Moving past the identity step needs a valid id
    /// (`resolveWizardNavigation`).
    fn advance_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.check_dialog_policy(cx) { return; }
        let Some(step) = self.providers.dialog.as_ref().filter(|dialog| !dialog.saving).map(|dialog| dialog.step) else { return };
        if step + 1 < STEPS.len() {
            if let Some(dialog) = self.providers.dialog.as_mut() {
                dialog.step = step + 1;
            }
            if let Some(dialog) = self.providers.dialog.as_ref() {
                let label = dialog.label.clone();
                label.update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
            return;
        }
        self.save_dialog(cx);
    }

    fn save_dialog(&mut self, cx: &mut Context<Self>) {
        if !self.check_dialog_policy(cx) { return; }
        let error = self.dialog_id_error(cx);
        let Some(dialog) = self.providers.dialog.as_mut() else { return };
        if dialog.saving {
            return;
        }
        dialog.attempted = true;
        if error.is_some() {
            cx.notify();
            return;
        }
        let instance_id = dialog.instance_id.read(cx).value().trim().to_owned();
        let instance = model::new_instance(dialog.driver.kind, &dialog.label.read(cx).value(), &dialog.accent.read(cx).value());
        dialog.saving = true;
        dialog.error = None;
        cx.notify();
        let (sent_at, task) = {
            let app = self.state.read(cx);
            (app.server_config.settings_revision(), app.rpc(method::SERVER_UPDATE_SETTINGS, model::create_payload(&instance_id, &instance)))
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(reply) => {
                        this.apply_settings_reply(reply, sent_at, cx);
                        this.providers.selected = Some(instance_id.clone());
                        this.providers.dialog = None;
                    }
                    Err(error) => {
                        if let Some(dialog) = this.providers.dialog.as_mut() {
                            dialog.saving = false;
                            dialog.error = Some(format!("Could not add provider instance: {}", failure_text(&error, "The settings update failed.")));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.providers.dialog.as_ref()?;
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let environment = {
            let app = self.state.read(cx);
            app.server_config.config.as_ref().map(|config| config.environment.label.trim().to_owned()).filter(|label| !label.is_empty()).unwrap_or_else(|| "this environment".to_owned())
        };
        let step = dialog.step;
        let driver = dialog.driver;
        let saving = dialog.saving;
        let id_error = self.dialog_id_error(cx).filter(|_| dialog.attempted);
        let denial = self.add_driver_denial(driver, cx);

        let steps = h_flex().gap(r(4.)).p(r(4.)).rounded(r(12.)).bg(if dark { gpui::white().opacity(0.04) } else { hsla(p.background) }).children(STEPS.iter().enumerate().map(|(index, name)| {
            let current = index == step;
            let done = index < step;
            let badge = div()
                .size(r(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .map(|this| {
                    if done {
                        this.bg(hsla(p.primary)).text_color(hsla(p.primary_foreground)).child(icon(Icon::Check, 16., hsla(p.primary_foreground)))
                    } else if current {
                        this.bg(hsla(p.primary).opacity(0.1)).text_color(hsla(p.primary)).border_1().border_color(hsla(p.primary).opacity(0.3)).child((index + 1).to_string())
                    } else {
                        this.bg(hsla(p.card)).text_color(hsla(p.muted_foreground)).border_1().border_color(hsla(p.border)).child((index + 1).to_string())
                    }
                });
            h_flex()
                .id(("add-provider-step", index))
                .flex_1()
                .min_w_0()
                .gap(r(8.))
                .px(r(10.))
                .py(r(8.))
                .rounded(r(8.))
                .when(current, |this| this.bg(hsla(p.card)))
                .when(!saving && index < step, |this| {
                    this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(dialog) = this.providers.dialog.as_mut() {
                            dialog.step = index;
                            cx.notify();
                        }
                    }))
                })
                .child(badge)
                .child(div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(if current { hsla(p.foreground) } else { hsla(p.muted_foreground) }).child(*name))
        }));

        let body = if step == 0 {
            let app = self.state.read(cx);
            let policy = app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref());
            let featured: Vec<&'static Driver> = model::offered_drivers(true, policy).collect();
            let more: Vec<&'static Driver> = model::offered_drivers(false, policy).collect();
            v_flex()
                .gap(r(8.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Provider"))
                .child(self.render_driver_grid(&featured, driver, cx))
                .when(driver.kind == "codex", |v| v.child(Button::new("add-provider-chatgpt",ButtonVariant::Default,ButtonSize::Sm).icon(Icon::OpenAi)
                    .label("Continue with ChatGPT")
                    .on_click(cx.listener(|this,_,window,cx| {
                        let instance=this.state.read(cx).server_config.config.as_ref().and_then(|c|c.providers.iter().find(|p|p.driver=="codex")).map(|p|p.instance_id.clone()).unwrap_or_else(||"codex".into());
                        let view=cx.new(|cx|crate::chatgpt_setup::ChatGptSetup::new(this.state.clone(),instance.clone(),false,window,cx));
                        view.update(cx,|view,cx|view.open_add(cx));
                        this.providers.selected=Some(instance);
                        this.providers.chatgpt=Some((this.state.read(cx).connection_id(),view));
                        this.close_add_dialog(window,cx);
                    })) ))
                .when(!more.is_empty(), |this| {
                    let show_more = dialog.show_more;
                    this.child(
                        h_flex().pt(r(8.)).child(
                            Button::new("add-provider-more", ButtonVariant::GhostMuted, ButtonSize::Compact)
                                .label(if show_more { "Show less" } else { "Show more" })
                                .icon(Icon::ChevronDown)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(dialog) = this.providers.dialog.as_mut() {
                                        dialog.show_more = !dialog.show_more;
                                        cx.notify();
                                    }
                                })),
                        ),
                    )
                    .when(show_more, |this| this.child(div().pt(r(8.)).child(self.render_driver_grid(&more, driver, cx))))
                })
                .into_any_element()
        } else {
            let accent = model::accent_rgba(&dialog.accent.read(cx).value());
            let swatch = div().size(r(16.)).flex_none().rounded_full().border_1().map(|this| match accent {
                Some(accent) => this.border_color(gpui::rgba(accent)).bg(gpui::rgba(accent)),
                None => this.border_color(hsla(p.border)),
            });
            v_flex()
                .child(Row::new("add-provider-label", "Label").description("Shown in the provider list.").control(div().w(r(224.)).child(Input::new(&dialog.label).small())))
                .child(
                    div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(
                        Row::new("add-provider-instance-id", "Instance ID")
                            .description("Letters, digits, '-', or '_'.")
                            .control(div().w(r(224.)).child(Input::new(&dialog.instance_id).small()))
                            .when_some(id_error, |row, error| row.below(div().pb(r(8.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error))),
                    ),
                )
                .child(
                    div().border_t_1().border_color(hsla(p.border).opacity(0.5)).child(
                        Row::new("add-provider-accent", "Accent color")
                            .description("Optional marker shown in the picker.")
                            .control(h_flex().gap(r(8.)).child(swatch).child(div().w(r(112.)).child(Input::new(&dialog.accent).small()))),
                    ),
                )
                .into_any_element()
        };

        let back: SharedString = if step == 0 { "Cancel".into() } else { "Back".into() };
        let primary: SharedString = if step + 1 < STEPS.len() {
            if driver.kind == "codex" { "Configure manually".into() } else { "Next".into() }
        } else if saving {
            "Adding...".into()
        } else {
            "Add instance".into()
        };
        let footer = h_flex()
            .justify_end()
            .gap(r(8.))
            .px(r(24.))
            .py(r(16.))
            .child(
                Button::new("add-provider-back", if step == 0 { ButtonVariant::GhostMuted } else { ButtonVariant::Outline }, ButtonSize::Sm)
                    .label(back)
                    .disabled(saving)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if step == 0 {
                            this.close_add_dialog(window, cx);
                        } else if let Some(dialog) = this.providers.dialog.as_mut() {
                            dialog.step -= 1;
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("add-provider-primary", if step == 0 && driver.kind == "codex" { ButtonVariant::Outline } else { ButtonVariant::Default }, ButtonSize::Sm)
                    .label(primary)
                    .disabled(saving || denial.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.advance_dialog(window, cx))),
            );

        let viewport = window.viewport_size();
        let popup = v_flex()
            .id("add-provider-dialog")
            .key_context("ProviderDialog")
            .track_focus(&dialog.focus)
            .on_action(cx.listener(|this, _: &CloseProviderDialog, window, cx| this.close_add_dialog(window, cx)))
            .occlude()
            .w_full()
            .max_w(r(768.))
            .max_h(px(f32::from(viewport.height) - 32.))
            .relative()
            .rounded(r(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.65),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(-24.),
                inset: false,
            }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .p(r(24.))
                    .gap(r(8.))
                    .child(div().text_xl().line_height(r(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child("Add provider"))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(format!("Add an account or configure a provider on {environment}.")))
                    .child(div().pt(r(8.)).child(steps)),
            )
            .child(div().id("add-provider-panel").flex_1().min_h_0().overflow_y_scroll().px(r(24.)).pt(r(4.)).pb(r(24.)).child(body))
            .children(denial.or_else(|| dialog.error.clone()).map(|error| div().px(r(24.)).pb(r(8.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error)))
            .child(footer);

        let overlay = div()
            .id("add-provider-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| this.close_add_dialog(window, cx)))
            .child(popup);
        Some(
            gpui::deferred(gpui::anchored().position_mode(gpui::AnchoredPositionMode::Window).position(point(px(0.), px(0.))).child(overlay))
                .with_priority(3)
                .into_any_element(),
        )
    }

    fn render_driver_grid(&self, drivers: &[&'static Driver], current: &'static Driver, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let cards: Vec<AnyElement> = drivers
            .iter()
            .map(|driver| {
                let driver: &'static Driver = driver;
                let selected = driver.kind == current.kind;
                h_flex()
                    .id(SharedString::from(format!("add-provider-driver-{}", driver.kind)))
                    .w_full()
                    .gap(r(12.))
                    .px(r(12.))
                    .py(r(12.))
                    .rounded(r(8.))
                    .cursor_pointer()
                    .border_1()
                    .map(|this| {
                        if selected {
                            this.border_color(hsla(p.primary)).bg(hsla(p.primary).opacity(if dark { 0.15 } else { 0.08 }))
                        } else {
                            this.border_color(if dark { gpui::white().opacity(0.05) } else { gpui::black().opacity(0.05) })
                                .bg(if dark { gpui::white().opacity(0.03) } else { hsla(p.card) })
                                .hover(|this| this.bg(hsla(p.muted)))
                        }
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.select_dialog_driver(driver, window, cx)))
                    .child(ProviderIcon::new(ProviderMark::configured(driver.kind, driver.label, None)).size(px(16.)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(driver.label))
                    .when(selected, |this| {
                        this.child(div().size(r(20.)).flex_none().flex().items_center().justify_center().rounded_full().bg(hsla(p.primary)).child(icon(Icon::Check, 14., hsla(p.primary_foreground))))
                    })
                    .children(driver.badge_label.map(|label| badge(label, BadgeKind::Warning, false, cx)))
                    .into_any_element()
            })
            .collect();
        let mut rows = v_flex().gap(r(8.));
        let mut cards = cards.into_iter();
        while let Some(first) = cards.next() {
            let second = cards.next();
            rows = rows.child(h_flex().gap(r(8.)).child(div().flex_1().min_w_0().child(first)).child(div().flex_1().min_w_0().children(second)));
        }
        rows.into_any_element()
    }
}
