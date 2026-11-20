//! The device tools panel beside the screen, as `DeviceToolsPanel.tsx` lays it out: the host's
//! discovery diagnostics, the live foreground app (Terminate, Relaunch), Open URL and Launch, the
//! simulator or emulator settings (appearance, text size, Liquid Glass and color filter on iOS,
//! orientation and fold posture on Android, and the switches each platform offers), Accessibility
//! frame overlay, location, permissions, a push notification on iOS, and the collapsible iOS event
//! log. Every control is off while an action runs or before the first detail; forms clear only
//! once their action succeeds.

use gpui::{AnyElement, App, AppContext, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::device::Platform;
use t3_core::device::hub::{FoldActionPosture, FoldPosture};
use t3_core::device::tools::{
    Action, Appearance, ColorFilter, Decision, LOCATION_PRESETS, LiquidGlass, ORIENTATIONS, Permission, TextSize, Toggle, parse_location, parse_orientation,
    resolve_app_id, submit_value,
};

use super::DeviceScreen;
use crate::icons::Icon;
use crate::right_panel::{Button as PanelButton, Size, Variant};
use crate::settings::controls::{Button, ButtonSize, ButtonVariant, Select, SelectOption, Switch};
use crate::theme::{hsla, palette};

/// `max-w-72` / `w-72`.
const PANEL_WIDTH: f32 = 288.;

/// The panel's fields, kept while the device's tab lives.
pub(super) struct Form {
    url: Entity<InputState>,
    launch: Entity<InputState>,
    latitude: Entity<InputState>,
    longitude: Entity<InputState>,
    app_id: Entity<InputState>,
    push: Entity<InputState>,
    permission: Permission,
    _subscriptions: Vec<Subscription>,
}

/// What Enter in a field submits.
#[derive(Clone, Copy)]
enum Submit {
    Url,
    Launch,
    Push,
}

/// The action a submit row sends for its field's text: nothing while empty, and a push only to
/// an app in the foreground.
fn submit_action(submit: Submit, text: &str, foreground: Option<&str>) -> Option<Action> {
    let text = submit_value(text)?.to_owned();
    Some(match submit {
        Submit::Url => Action::OpenUrl(text),
        Submit::Launch => Action::LaunchApp(text),
        Submit::Push => Action::SendPush { app_id: foreground?.to_owned(), payload: text },
    })
}

/// `DeviceWorkspace`'s alert: a failed screenshot, else a failed action while the tools panel,
/// which shows its own, is closed.
pub(super) fn alert(screenshot_error: Option<&str>, action_error: Option<&str>, tools_open: bool) -> Option<String> {
    screenshot_error.or(action_error.filter(|_| !tools_open)).map(str::to_owned)
}

impl Form {
    pub(super) fn new(platform: Platform, window: &mut Window, cx: &mut Context<DeviceScreen>) -> Form {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<DeviceScreen>| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let url = input("https://… or myapp://", window, cx);
        let launch = input(if platform == Platform::Ios { "Bundle ID to launch" } else { "Package name to launch" }, window, cx);
        let latitude = input("Latitude", window, cx);
        let longitude = input("Longitude", window, cx);
        let app_id = input("App ID", window, cx);
        let push = input("Alert text", window, cx);
        let mut subscriptions = Vec::new();
        for (field, submit) in [(&url, Some(Submit::Url)), (&launch, Some(Submit::Launch)), (&push, Some(Submit::Push)), (&latitude, None), (&longitude, None), (&app_id, None)] {
            subscriptions.push(cx.subscribe_in(field, window, move |this: &mut DeviceScreen, _, event: &InputEvent, window, cx| match event {
                // Buttons enable as their fields fill.
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    if let Some(submit) = submit {
                        this.submit(submit, window, cx);
                    }
                }
                _ => {}
            }));
        }
        Form { url, launch, latitude, longitude, app_id, push, permission: Permission::Camera, _subscriptions: subscriptions }
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> SharedString {
    input.read(cx).value()
}

fn section(title: &'static str, children: Vec<AnyElement>, cx: &App) -> AnyElement {
    let p = palette(cx);
    v_flex()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .border_b_1()
        .border_color(hsla(p.border))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(title))
        .children(children)
        .into_any_element()
}

fn row(label: impl Into<SharedString>, control: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .min_h(px(28.))
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(div().flex_shrink_0().text_xs().text_color(hsla(palette(cx).muted_foreground)).child(label.into()))
        .child(h_flex().min_w_0().items_center().justify_end().child(control))
        .into_any_element()
}

impl DeviceScreen {
    /// Sends what a field's row submits; the field clears once it succeeds.
    fn submit(&mut self, submit: Submit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let input = match submit {
            Submit::Url => form.url.clone(),
            Submit::Launch => form.launch.clone(),
            Submit::Push => form.push.clone(),
        };
        let foreground = self.effective_foreground();
        let Some(action) = submit_action(submit, &value(&input, cx), foreground.as_deref()) else { return };
        self.act(action, Some(input), window, cx);
    }

    fn submit_row(&self, id: &'static str, input: &Entity<InputState>, label: &'static str, submit: Submit, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let empty = submit_value(&value(input, cx)).is_none();
        h_flex()
            .gap(px(6.))
            .child(div().flex_1().min_w_0().child(Input::new(input).small().disabled(disabled)))
            .child(Button::new(id, ButtonVariant::Outline, ButtonSize::Xs).label(label).disabled(disabled || empty).on_click(cx.listener(move |this, _, window, cx| this.submit(submit, window, cx))))
            .into_any_element()
    }

    pub(super) fn render_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(form) = &self.form else { return div().into_any_element() };
        let ios = self.device.platform == Platform::Ios;
        let disabled = self.controls.disabled(self.controls_visible(cx));
        let settings = self.controls.settings().cloned().unwrap_or_default();
        let foreground = self.effective_foreground();
        let mut body: Vec<AnyElement> = Vec::new();
        if let Some(error) = self.controls.error() {
            body.push(div().px(px(12.)).py(px(8.)).border_b_1().border_color(hsla(p.border)).bg(hsla(p.destructive).opacity(0.1)).text_xs().text_color(hsla(p.destructive)).child(error.to_owned()).into_any_element());
        } else if self.controls.detail().is_none() {
            body.push(div().px(px(12.)).py(px(12.)).text_xs().text_color(hsla(p.muted_foreground)).child("Reading device settings…").into_any_element());
        }
        let diagnostics: Vec<String> = self
            .devices
            .read(cx)
            .service()
            .map(|service| service.panel_diagnostics().into_iter().filter(|diagnostic| diagnostic.host_id == self.device.host_id).map(|diagnostic| diagnostic.detail).collect())
            .unwrap_or_default();
        if !diagnostics.is_empty() {
            let text = div().text_xs().text_color(hsla(p.muted_foreground)).child(diagnostics.join("\n")).into_any_element();
            body.push(section("Host diagnostics", vec![text], cx));
        }

        // App.
        let mut app = vec![row("Foreground", div().truncate().font_family(crate::theme::font_family(t3_core::client_settings::typography::FontRole::Code, cx)).text_xs().child(foreground.clone().unwrap_or_else(|| "-".to_owned())), cx)];
        if let Some(id) = foreground.clone() {
            let (terminate, relaunch) = (id.clone(), id);
            app.push(
                h_flex()
                    .gap(px(6.))
                    .child(Button::new("device-terminate", ButtonVariant::Outline, ButtonSize::Xs).label("Terminate").disabled(disabled).on_click(cx.listener(move |this, _, window, cx| {
                        this.act(Action::TerminateApp(terminate.clone()), None, window, cx);
                    })))
                    .child(Button::new("device-relaunch", ButtonVariant::Outline, ButtonSize::Xs).label("Relaunch").disabled(disabled).on_click(cx.listener(move |this, _, window, cx| {
                        this.act(Action::LaunchApp(relaunch.clone()), None, window, cx);
                    })))
                    .into_any_element(),
            );
        }
        app.push(self.submit_row("device-open-url", &form.url, "Open", Submit::Url, disabled, cx));
        app.push(self.submit_row("device-launch", &form.launch, "Launch", Submit::Launch, disabled, cx));
        body.push(section("App", app, cx));

        // Simulator or emulator.
        let appearance = h_flex()
            .gap(px(2.))
            .children([Appearance::Light, Appearance::Dark].map(|appearance| {
                PanelButton::new(SharedString::from(format!("device-appearance-{}", appearance.as_str())))
                    .variant(Variant::Toggle)
                    .size(Size::Xs)
                    .label(appearance.label())
                    .pressed(settings.appearance == Some(appearance))
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.controls.settings().and_then(|settings| settings.appearance) != Some(appearance) {
                            this.act(Action::SetAppearance(appearance), None, window, cx);
                        }
                    }))
            }));
        let mut device = vec![row("Appearance", appearance, cx)];
        let text_size = Select::new("device-text-size-select", TextSize::ALL.iter().map(|size| SelectOption::new(size.as_str(), size.label())).collect())
            .width(160.)
            .disabled(disabled)
            .label(settings.text_size.map_or("Unknown", TextSize::label))
            .when_some(settings.text_size, |select, size| select.selected(size.as_str()))
            .on_change(self.select_handler(settings.text_size.map(TextSize::as_str), |value| TextSize::parse(value).map(Action::SetTextSize), cx));
        device.push(row("Text size", text_size, cx));
        if ios {
            let glass = h_flex().gap(px(2.)).children([LiquidGlass::Clear, LiquidGlass::Tinted].map(|glass| {
                PanelButton::new(SharedString::from(format!("device-glass-{}", glass.as_str())))
                    .variant(Variant::Toggle)
                    .size(Size::Xs)
                    .label(glass.label())
                    .pressed(settings.liquid_glass == Some(glass))
                    .disabled(disabled || settings.liquid_glass.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.controls.settings().and_then(|settings| settings.liquid_glass) != Some(glass) {
                            this.act(Action::SetLiquidGlass(glass), None, window, cx);
                        }
                    }))
            }));
            device.push(row("Liquid Glass", glass, cx));
            let filter = Select::new("device-color-filter", ColorFilter::ALL.iter().map(|filter| SelectOption::new(filter.as_str(), filter.label())).collect())
                .width(160.)
                .disabled(disabled)
                .label(settings.color_filter.map_or("Unknown", ColorFilter::label))
                .when_some(settings.color_filter, |select, filter| select.selected(filter.as_str()))
                .on_change(self.select_handler(settings.color_filter.map(ColorFilter::as_str), |value| ColorFilter::parse(value).map(Action::SetColorFilter), cx));
            device.push(row("Color filter", filter, cx));
        } else {
            let orientation = Select::new("device-orientation", ORIENTATIONS.iter().map(|(orientation, label)| SelectOption::new(orientation.as_str(), *label)).collect())
                .width(160.)
                .disabled(disabled)
                .label("Rotate to…")
                .on_change(self.select_handler(None, |value| parse_orientation(value).map(Action::SetOrientation), cx));
            device.push(row("Orientation", orientation, cx));
        }
        for toggle in Toggle::for_platform(self.device.platform).iter().copied() {
            let checked = toggle.value(&settings);
            let view = cx.entity().downgrade();
            let switch = Switch::new(SharedString::from(format!("device-toggle-{}", toggle.as_str())), checked.unwrap_or(false))
                .disabled(disabled || checked.is_none())
                .on_change(move |value, window, cx| {
                    let _ = view.update(cx, |this, cx| this.act(Action::SetToggle(toggle, value), None, window, cx));
                });
            device.push(row(toggle.label(), switch, cx));
        }
        body.push(section(if ios { "Simulator" } else { "Emulator" }, device, cx));

        // Android foldable posture (`DeviceAndroidFoldControls.tsx`).
        if !ios
            && let Some(fold) = &self.android_fold
            && fold.supported
        {
            let posture = fold.posture;
            let fold_buttons = h_flex().gap(px(2.)).children([
                (FoldActionPosture::Opened, FoldPosture::Opened, "Opened"),
                (FoldActionPosture::Closed, FoldPosture::Closed, "Closed"),
            ].map(|(action_posture, state_posture, label)| {
                PanelButton::new(SharedString::from(format!("device-fold-{}", action_posture.as_str())))
                    .variant(Variant::Toggle)
                    .size(Size::Xs)
                    .label(label)
                    .pressed(posture == Some(state_posture))
                    .disabled(self.fold_pending)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_fold(action_posture, cx)))
            }));
            let mut fold_rows = vec![row("Posture", fold_buttons, cx)];
            if let Some(error) = &self.fold_error {
                fold_rows.push(div().text_xs().text_color(hsla(p.destructive)).child(error.clone()).into_any_element());
            }
            body.push(section("Fold", fold_rows, cx));
        }

        // Accessibility (`DeviceToolsPanel.tsx`: "Overlay element frames").
        let ax_view = cx.entity().downgrade();
        let ax_switch = Switch::new("device-ax-overlay", self.ax_overlay)
            .disabled(!self.has_hub_access(cx))
            .on_change(move |value, _, cx| {
                let _ = ax_view.update(cx, |this, cx| this.set_ax_overlay(value, cx));
            });
        body.push(section("Accessibility", vec![row("Overlay element frames", ax_switch, cx)], cx));

        // Location.
        let location = parse_location(&value(&form.latitude, cx), &value(&form.longitude, cx));
        let presets = Select::new("device-location-preset", LOCATION_PRESETS.iter().map(|(label, _, _)| SelectOption::new(*label, *label)).collect())
            .width(128.)
            .disabled(disabled)
            .label("Preset…")
            .on_change({
                let view = cx.entity().downgrade();
                move |value, window, cx| {
                    let Some((_, latitude, longitude)) = LOCATION_PRESETS.iter().find(|(label, _, _)| *label == value.as_ref()) else { return };
                    let _ = view.update(cx, |this, cx| {
                        if let Some(form) = &this.form {
                            let (lat, lon) = (form.latitude.clone(), form.longitude.clone());
                            lat.update(cx, |input, cx| input.set_value(latitude.to_string(), window, cx));
                            lon.update(cx, |input, cx| input.set_value(longitude.to_string(), window, cx));
                        }
                        this.act(Action::SetLocation { latitude: *latitude, longitude: *longitude }, None, window, cx);
                    });
                }
            });
        let location_actions = h_flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .child(presets)
            .child(Button::new("device-location-set", ButtonVariant::Outline, ButtonSize::Xs).label("Set").disabled(disabled || location.is_none()).on_click(cx.listener(
                move |this, _, window, cx| {
                    if let Some((latitude, longitude)) = location {
                        this.act(Action::SetLocation { latitude, longitude }, None, window, cx);
                    }
                },
            )))
            .when(ios, |this| {
                this.child(Button::new("device-location-clear", ButtonVariant::Ghost, ButtonSize::Xs).label("Clear").disabled(disabled).on_click(cx.listener(|this, _, window, cx| {
                    if let Some(form) = &this.form {
                        let (lat, lon) = (form.latitude.clone(), form.longitude.clone());
                        lat.update(cx, |input, cx| input.set_value("", window, cx));
                        lon.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    this.act(Action::ClearLocation, None, window, cx);
                })))
            });
        let coordinates = h_flex()
            .gap(px(6.))
            .child(div().flex_1().min_w_0().child(Input::new(&form.latitude).small().disabled(disabled)))
            .child(div().flex_1().min_w_0().child(Input::new(&form.longitude).small().disabled(disabled)));
        body.push(section("Location", vec![coordinates.into_any_element(), location_actions.into_any_element()], cx));

        // Permissions.
        let typed = value(&form.app_id, cx);
        let app_id = resolve_app_id(&typed, foreground.as_deref()).map(str::to_owned);
        let permission = Select::new("device-permission", Permission::for_platform(self.device.platform).iter().map(|(permission, label)| SelectOption::new(permission.as_str(), *label)).collect())
            .width(160.)
            .disabled(disabled)
            .selected(form.permission.as_str())
            .on_change({
                let view = cx.entity().downgrade();
                move |value, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if let (Some(form), Some(permission)) = (this.form.as_mut(), Permission::parse(&value)) {
                            form.permission = permission;
                            cx.notify();
                        }
                    });
                }
            });
        let decisions: &[(Decision, &str, ButtonVariant)] =
            if ios { &[(Decision::Grant, "Grant", ButtonVariant::Outline), (Decision::Revoke, "Revoke", ButtonVariant::Outline), (Decision::Reset, "Reset", ButtonVariant::Ghost)] } else { &[(Decision::Grant, "Grant", ButtonVariant::Outline), (Decision::Revoke, "Revoke", ButtonVariant::Outline)] };
        let chosen = form.permission;
        let permission_actions = h_flex().flex_wrap().items_center().gap(px(6.)).child(permission).children(decisions.iter().map(|(decision, label, variant)| {
            let (decision, app_id) = (*decision, app_id.clone());
            Button::new(SharedString::from(format!("device-permission-{}", decision.as_str())), *variant, ButtonSize::Xs)
                .label(*label)
                .disabled(disabled || app_id.is_none())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(app_id) = app_id.clone() {
                        this.act(Action::SetPermission { app_id, permission: chosen, decision }, None, window, cx);
                    }
                }))
        }));
        body.push(section("Permissions", vec![Input::new(&form.app_id).small().disabled(disabled).into_any_element(), permission_actions.into_any_element()], cx));

        if ios {
            let mut push = vec![self.submit_row("device-push", &form.push, "Send", Submit::Push, disabled || foreground.is_none(), cx)];
            if foreground.is_none() {
                push.push(div().text_xs().text_color(hsla(p.muted_foreground)).child("Open an app first.").into_any_element());
            }
            body.push(section("Push notification", push, cx));

            // Collapsible Event log (`EventLogSection` in `DeviceToolsPanel.tsx`).
            if self.has_hub_access(cx) {
                let mono = crate::theme::font_family(t3_core::client_settings::typography::FontRole::Code, cx);
                let mut log_section = v_flex().border_b_1().border_color(hsla(p.border)).child(
                    h_flex()
                        .id("device-event-log-toggle")
                        .w_full()
                        .px(px(12.))
                        .py(px(10.))
                        .gap(px(6.))
                        .items_center()
                        .cursor_pointer()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.muted_foreground))
                        .hover(|s| s.text_color(hsla(p.foreground)))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_event_log(cx)))
                        .child("Event log")
                        .child(div().flex_1())
                        .child(
                            (if self.event_log_open { Icon::ChevronDown } else { Icon::ChevronRight })
                                .svg()
                                .size(px(14.))
                                .text_color(hsla(p.muted_foreground)),
                        ),
                );
                if self.event_log_open {
                    let entries: Vec<AnyElement> = if self.event_log.is_empty() {
                        vec![div().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child("No events yet.").into_any_element()]
                    } else {
                        self.event_log
                            .iter()
                            .map(|entry| {
                                h_flex()
                                    .gap(px(8.))
                                    .font_family(mono.clone())
                                    .text_size(px(11.))
                                    .child(div().flex_none().text_color(hsla(p.muted_foreground)).child(SharedString::from(entry.short_time().to_owned())))
                                    .child(div().min_w_0().truncate().text_color(hsla(p.foreground)).child(SharedString::from(entry.summary.clone())))
                                    .into_any_element()
                            })
                            .collect()
                    };
                    log_section = log_section.child(
                        div()
                            .id("device-event-log-entries")
                            .max_h(px(256.))
                            .overflow_y_scroll()
                            .px(px(12.))
                            .pb(px(8.))
                            .child(v_flex().gap(px(3.)).children(entries)),
                    );
                }
                body.push(log_section.into_any_element());
            }
        }

        v_flex()
            .w(px(PANEL_WIDTH))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .text_sm()
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Tools"))
                    .when(self.controls.pending(), |this| this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Applying…")))
                    .child(div().flex_1())
                    .child(
                        PanelButton::new("device-tools-close")
                            .variant(Variant::GhostMuted)
                            .size(Size::IconXs)
                            .icon(Icon::X)
                            .tooltip("Close tools")
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_tools(window, cx))),
                    ),
            )
            .child(div().id("device-tools-body").flex_1().min_h_0().overflow_y_scroll().child(v_flex().children(body)))
            .into_any_element()
    }

    /// A Select's change, as the action it names; picking the current value sends nothing.
    fn select_handler(&self, current: Option<&'static str>, action: impl Fn(&str) -> Option<Action> + 'static, cx: &mut Context<Self>) -> impl Fn(SharedString, &mut Window, &mut App) + 'static {
        let view = cx.entity().downgrade();
        move |value, window, cx| {
            if current == Some(value.as_ref()) {
                return;
            }
            if let Some(action) = action(&value) {
                let _ = view.update(cx, |this, cx| this.act(action, None, window, cx));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_tools_submit_rows_and_alert_follow_the_web() {
        assert_eq!(submit_action(Submit::Url, "  myapp://home ", None), Some(Action::OpenUrl("myapp://home".into())));
        assert_eq!(submit_action(Submit::Launch, "   ", Some("com.fg")), None, "an empty field submits nothing");
        assert_eq!(submit_action(Submit::Push, "Hi", None), None, "a push needs an app in the foreground");
        assert_eq!(submit_action(Submit::Push, "Hi", Some("com.fg")), Some(Action::SendPush { app_id: "com.fg".into(), payload: "Hi".into() }));
        assert_eq!(alert(Some("Screenshot capture failed (500). Try again."), Some("Device action failed"), false).as_deref(), Some("Screenshot capture failed (500). Try again."));
        assert_eq!(alert(None, Some("Device action failed"), true), None, "the open tools panel shows the action error itself");
        assert_eq!(alert(None, Some("Device action failed"), false).as_deref(), Some("Device action failed"));
    }
}
