//! Device viewport toolbar for server-streamed preview tabs. Ports
//! `apps/web/src/browser/BrowserDeviceToolbar.tsx` and `browserViewportLayout.ts`:
//! preset selector ("Responsive" + Chrome DevTools standard devices), editable width × height
//! inputs with bounds validation, aspect ratio lock, rotate, and close back to Fill.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString, Styled,
    Subscription, Window, deferred, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::h_flex;
use t3_core::preview::viewport::{
    BROWSER_DEVICE_TOOLBAR_HEIGHT, PREVIEW_VIEWPORT_MAX_DIMENSION, PREVIEW_VIEWPORT_MIN_DIMENSION, VIEWPORT_PRESETS, ViewportSetting,
    find_viewport_preset, preset_setting, resize_freeform_viewport, rotate_viewport, validate_viewport_dimensions,
};

use crate::composer::controls::{MenuItem, popup_surface};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceToolbarEvent {
    Change(ViewportSetting),
}

pub struct DeviceToolbar {
    setting: ViewportSetting,
    panel_width: f32,
    aspect_ratio: Option<f64>,
    preset_menu_open: bool,
    preset_menu_closed_at: Option<Instant>,
    editing_axis: Option<bool>, // Some(true) = width, Some(false) = height
    width_input: Entity<InputState>,
    height_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DeviceToolbarEvent> for DeviceToolbar {}

impl DeviceToolbar {
    pub fn new(setting: ViewportSetting, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (w, h) = setting.dimensions().unwrap_or((1024, 768));
        let width_input = cx.new(|cx| InputState::new(window, cx).default_value(w.to_string()));
        let height_input = cx.new(|cx| InputState::new(window, cx).default_value(h.to_string()));
        let width_sub = cx.subscribe_in(&width_input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Focus => {
                this.editing_axis = Some(true);
            }
            InputEvent::Change => {
                this.on_dimension_edit(true, window, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                this.commit_custom_size(window, cx);
            }
        });
        let height_sub = cx.subscribe_in(&height_input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Focus => {
                this.editing_axis = Some(false);
            }
            InputEvent::Change => {
                this.on_dimension_edit(false, window, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                this.commit_custom_size(window, cx);
            }
        });
        Self {
            setting,
            panel_width: 800.,
            aspect_ratio: None,
            preset_menu_open: false,
            preset_menu_closed_at: None,
            editing_axis: None,
            width_input,
            height_input,
            _subscriptions: vec![width_sub, height_sub],
        }
    }

    pub fn sync_setting(&mut self, setting: ViewportSetting, panel_width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.panel_width = panel_width;
        if self.setting != setting {
            self.setting = setting;
            if let Some((w, h)) = self.setting.dimensions() {
                self.width_input.update(cx, |input, cx| input.set_value(w.to_string(), window, cx));
                self.height_input.update(cx, |input, cx| input.set_value(h.to_string(), window, cx));
            }
            self.editing_axis = None;
            cx.notify();
        }
    }

    pub fn close_menus(&mut self, cx: &mut Context<Self>) -> bool {
        if self.preset_menu_open {
            self.preset_menu_open = false;
            cx.notify();
            return true;
        }
        false
    }

    fn parsed_dimensions(&self, cx: &App) -> Option<(u32, u32)> {
        let w = self.width_input.read(cx).value().trim().parse::<u32>().ok()?;
        let h = self.height_input.read(cx).value().trim().parse::<u32>().ok()?;
        validate_viewport_dimensions(w, h).then_some((w, h))
    }

    fn on_dimension_edit(&mut self, edited_width: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_axis = Some(edited_width);
        let Some(ratio) = self.aspect_ratio else {
            cx.notify();
            return;
        };
        let Some((current_w, current_h)) = self.setting.dimensions() else { return };
        if edited_width {
            if let Ok(numeric) = self.width_input.read(cx).value().trim().parse::<u32>()
                && (PREVIEW_VIEWPORT_MIN_DIMENSION..=PREVIEW_VIEWPORT_MAX_DIMENSION).contains(&numeric)
            {
                let (_, next_h) = resize_freeform_viewport(
                    (f64::from(current_w), f64::from(current_h)),
                    (f64::from(numeric) - f64::from(current_w), 0.),
                    1.,
                    Some(ratio),
                    Some(true),
                );
                let next_str = next_h.to_string();
                if self.height_input.read(cx).value().as_ref() != next_str {
                    self.height_input.update(cx, |input, cx| input.set_value(next_str, window, cx));
                }
            }
        } else if let Ok(numeric) = self.height_input.read(cx).value().trim().parse::<u32>()
            && (PREVIEW_VIEWPORT_MIN_DIMENSION..=PREVIEW_VIEWPORT_MAX_DIMENSION).contains(&numeric)
        {
            let (next_w, _) = resize_freeform_viewport(
                (f64::from(current_w), f64::from(current_h)),
                (0., f64::from(numeric) - f64::from(current_h)),
                1.,
                Some(ratio),
                Some(false),
            );
            let next_str = next_w.to_string();
            if self.width_input.read(cx).value().as_ref() != next_str {
                self.width_input.update(cx, |input, cx| input.set_value(next_str, window, cx));
            }
        }
        cx.notify();
    }

    fn commit_custom_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((w, h)) = self.parsed_dimensions(cx) else {
            if let Some((cur_w, cur_h)) = self.setting.dimensions() {
                self.width_input.update(cx, |input, cx| input.set_value(cur_w.to_string(), window, cx));
                self.height_input.update(cx, |input, cx| input.set_value(cur_h.to_string(), window, cx));
            }
            self.editing_axis = None;
            cx.notify();
            return;
        };
        self.editing_axis = None;
        if self.setting.dimensions() == Some((w, h)) {
            return;
        }
        cx.emit(DeviceToolbarEvent::Change(ViewportSetting::Freeform { width: w, height: h }));
    }

    fn select_responsive(&mut self, cx: &mut Context<Self>) {
        self.preset_menu_open = false;
        if let Some((width, height)) = self.setting.dimensions()
            && !matches!(self.setting, ViewportSetting::Freeform { .. })
        {
            cx.emit(DeviceToolbarEvent::Change(ViewportSetting::Freeform { width, height }));
        }
        cx.notify();
    }

    fn select_preset(&mut self, id: &str, cx: &mut Context<Self>) {
        self.preset_menu_open = false;
        if let Some(setting) = preset_setting(id) {
            if let Some((w, h)) = setting.dimensions()
                && self.aspect_ratio.is_some()
            {
                self.aspect_ratio = Some(f64::from(w) / f64::from(h));
            }
            cx.emit(DeviceToolbarEvent::Change(setting));
        }
        cx.notify();
    }

    fn rotate(&mut self, cx: &mut Context<Self>) {
        let source = match self.parsed_dimensions(cx) {
            Some((w, h)) if self.setting.dimensions() != Some((w, h)) => ViewportSetting::Freeform { width: w, height: h },
            _ => self.setting.clone(),
        };
        let (rotated, next_ratio) = rotate_viewport(&source, self.aspect_ratio);
        self.aspect_ratio = next_ratio;
        cx.emit(DeviceToolbarEvent::Change(rotated));
        cx.notify();
    }

    fn toggle_aspect_ratio(&mut self, cx: &mut Context<Self>) {
        if self.aspect_ratio.is_some() {
            self.aspect_ratio = None;
        } else if let Some((w, h)) = self.parsed_dimensions(cx).or_else(|| self.setting.dimensions()) {
            self.aspect_ratio = Some(f64::from(w) / f64::from(h));
        }
        cx.notify();
    }

    fn close_toolbar(&mut self, cx: &mut Context<Self>) {
        self.preset_menu_open = false;
        self.aspect_ratio = None;
        cx.emit(DeviceToolbarEvent::Change(ViewportSetting::Fill));
        cx.notify();
    }

    fn selected_label(&self, cx: &gpui::App) -> SharedString {
        if let Some(preset_id) = self.setting.preset_id()
            && let Some(preset) = find_viewport_preset(preset_id)
        {
            return SharedString::from(preset.label);
        }
        SharedString::from(crate::i18n::t(cx, "Responsive"))
    }

    fn render_preset_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.preset_menu_open {
            return None;
        }
        let p = palette(cx);
        let active_preset = self.setting.preset_id().map(str::to_owned);
        let is_responsive = active_preset.as_deref().and_then(find_viewport_preset).is_none();
        Some(
            deferred(
                popup_surface(cx)
                    .id("preview-device-preset-menu")
                    .occlude()
                    .absolute()
                    .top(px(BROWSER_DEVICE_TOOLBAR_HEIGHT + 2.))
                    .left(px(6.))
                    .w(px(252.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p(px(4.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.preset_menu_open = false;
                        this.preset_menu_closed_at = Some(Instant::now());
                        cx.notify();
                    }))
                    .child(
                        MenuItem::new("preview-preset-responsive", crate::i18n::t(cx, "Responsive"))
                            .checked(is_responsive)
                            .on_click(cx.listener(|this, _, _, cx| this.select_responsive(cx))),
                    )
                    .child(
                        div()
                            .px(px(8.))
                            .pt(px(6.))
                            .pb(px(2.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(crate::i18n::t(cx, "Standard")),
                    )
                    .children(VIEWPORT_PRESETS.iter().enumerate().map(|(index, preset)| {
                        let id = preset.id;
                        let checked = active_preset.as_deref() == Some(id);
                        h_flex()
                            .id(("preview-preset-item", index))
                            .min_h(px(28.))
                            .px(px(8.))
                            .py(px(4.))
                            .gap(px(8.))
                            .rounded(px(4.))
                            .items_center()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(hsla(p.foreground))
                            .hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                            .on_click(cx.listener(move |this, _, _, cx| this.select_preset(id, cx)))
                            .child(
                                div()
                                    .size(px(16.))
                                    .flex_none()
                                    .children(checked.then(|| Icon::Check.render(px(16.), hsla(p.foreground)))),
                            )
                            .child(div().min_w_0().flex_1().truncate().child(preset.label))
                            .child(div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(preset.detail))
                    })),
            )
            .with_priority(2),
        )
    }
}

impl Render for DeviceToolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let custom_valid = self.parsed_dimensions(cx).is_some();
        let locked = self.aspect_ratio.is_some();
        let show_dimensions_label = self.panel_width >= 560.;
        let trigger_width = if self.panel_width >= 440. { px(144.) } else { px(96.) };
        let input_width = if self.panel_width >= 360. { px(56.) } else { px(52.) };

        div()
            .relative()
            .flex_none()
            .w_full()
            .h(px(BROWSER_DEVICE_TOOLBAR_HEIGHT))
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.7))
            .bg(hsla(p.background))
            .child(
                h_flex()
                    .size_full()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .when(show_dimensions_label, |this| {
                        this.child(div().mr(px(2.)).flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, "Dimensions")))
                    })
                    .child(
                        h_flex()
                            .id("preview-device-preset-trigger")
                            .flex_none()
                            .w(trigger_width)
                            .h(px(24.))
                            .px(px(6.))
                            .justify_between()
                            .items_center()
                            .gap(px(4.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let just_closed = this
                                    .preset_menu_closed_at
                                    .take()
                                    .is_some_and(|at| at.elapsed() < Duration::from_millis(150));
                                if !just_closed {
                                    this.preset_menu_open = !this.preset_menu_open;
                                    cx.notify();
                                }
                            }))
                            .child(div().truncate().text_xs().text_color(hsla(p.foreground)).child(self.selected_label(cx)))
                            .child(Icon::ChevronDown.sized(px(12.)).text_color(hsla(p.muted_foreground))),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(3.))
                            .child(
                                div()
                                    .w(input_width)
                                    .h(px(24.))
                                    .px(px(4.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(hsla(if custom_valid { p.input } else { p.destructive }))
                                    .bg(hsla(p.background))
                                    .flex()
                                    .items_center()
                                    .child(Input::new(&self.width_input).appearance(false).bordered(false)),
                            )
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("×"))
                            .child(
                                div()
                                    .w(input_width)
                                    .h(px(24.))
                                    .px(px(4.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(hsla(if custom_valid { p.input } else { p.destructive }))
                                    .bg(hsla(p.background))
                                    .flex()
                                    .items_center()
                                    .child(Input::new(&self.height_input).appearance(false).bordered(false)),
                            ),
                    )
                    .child(
                        Button::new("preview-device-lock")
                            .variant(if locked { Variant::Secondary } else { Variant::Ghost })
                            .size(Size::IconXs)
                            .icon(if locked { Icon::Link2 } else { Icon::Unlink2 })
                            .tooltip(crate::i18n::t(cx, if locked { "Unlock aspect ratio" } else { "Lock aspect ratio" }))
                            .disabled(!custom_valid)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_aspect_ratio(cx))),
                    )
                    .child(
                        Button::new("preview-device-rotate")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::RotateCcw)
                            .tooltip(crate::i18n::t(cx, "Rotate viewport"))
                            .on_click(cx.listener(|this, _, _, cx| this.rotate(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("preview-device-close")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::X)
                            .tooltip(crate::i18n::t(cx, "Close device toolbar"))
                            .on_click(cx.listener(|this, _, _, cx| this.close_toolbar(cx))),
                    ),
            )
            .children(self.render_preset_menu(cx))
    }
}
