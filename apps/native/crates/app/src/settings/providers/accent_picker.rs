//! Compact HSV popover for a provider instance's accent.

use std::{cell::Cell, rc::Rc};

use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, App, Bounds, Context, Corners, FocusHandle, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Point, Styled, Window, anchored,
    black, canvas, deferred, div, fill, linear_color_stop, linear_gradient, point, px, quad,
    relative, size, transparent_black,
};
use gpui_component::{Sizable, h_flex, input::Input, v_flex};
use t3_core::{
    settings::providers::{self as model, InstanceEdit},
    sidebar::grouping::EnvironmentOwner,
    themes::editor::picker::{self, Axis, Drag, Hsv, Surface},
    usage::PROVIDERS_MANAGE_SCOPE,
};

use super::{CloseProviderDialog, Selected, SettingsView};
use crate::{
    composer::controls::popup_surface,
    i18n::t,
    icons::Icon,
    settings::controls::{Button, ButtonSize, ButtonVariant, focus_ring, r},
    theme::{ThemeSlot, ThemeSlotExt, hsla, palette, theme_slot_clipped_background_layer},
};

const INITIAL_COLOR: &str = "#2563eb";

pub(super) struct AccentPicker {
    instance: String,
    owner: Option<EnvironmentOwner>,
    connection: Option<u64>,
    base: Option<u32>,
    hsv: Hsv,
    bounds: [Rc<Cell<Bounds<Pixels>>>; 2],
    focus: [FocusHandle; 2],
    trigger: FocusHandle,
    drag: Drag,
}

impl SettingsView {
    fn accent_allowed(&self, instance: &str, cx: &App) -> bool {
        ((self.scope.machine.is_none() && self.scope.project.is_none())
            || self.scoped_primary_owner(cx).is_some())
            && self
                .scoped_session(cx)
                .is_none_or(|session| session.grants(PROVIDERS_MANAGE_SCOPE))
            && self
                .scoped_raw_representative_config(cx)
                .is_some_and(|config| {
                    !t3_core::managed_policy::setting_locked(
                        config.managed_policy.as_ref(),
                        &format!("providerInstances.{instance}.accentColor"),
                    )
                })
    }

    fn accent_target_current(&self, picker: &AccentPicker, cx: &App) -> bool {
        self.section == t3_core::settings::search::Section::Providers
            && self.selected_instance_id(cx).as_deref() == Some(picker.instance.as_str())
            && self.scoped_primary_owner(cx) == picker.owner
            && self.state.read(cx).connection_id() == picker.connection
    }

    fn dismiss_accent_picker(
        &mut self,
        window: &mut Window,
        restore_focus: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = self.providers.accent_picker.take() {
            if restore_focus {
                picker.trigger.focus(window, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn save_accent_picker(
        &mut self,
        clear: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.providers.accent_picker.as_ref() else {
            return;
        };
        if self.accent_target_current(picker, cx) && self.accent_allowed(&picker.instance, cx) {
            let instance = picker.instance.clone();
            let typed = self.providers.accent.read(cx).value();
            let value = if clear {
                String::new()
            } else {
                let Some(value) = model::normalize_accent(typed.trim()) else {
                    return;
                };
                value
            };
            self.dismiss_accent_picker(window, true, cx);
            self.update_instance_then(&instance, InstanceEdit::Accent(value), |_, _| {}, cx);
        } else {
            self.dismiss_accent_picker(window, true, cx);
        }
    }

    fn preview_accent(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.providers.accent_picker.as_mut() else {
            return;
        };
        let Some(surface) = picker.drag.surface() else {
            return;
        };
        let bounds = picker.bounds[surface.index()].get();
        let x = f64::from((at.x - bounds.left()) / bounds.size.width.max(px(1.)));
        let y = f64::from((at.y - bounds.top()) / bounds.size.height.max(px(1.)));
        if let Some(hsv) = picker.drag.sample(picker.hsv, x, y) {
            self.preview_accent_hsv(hsv, window, cx);
        }
    }

    fn preview_accent_hsv(&mut self, hsv: Hsv, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.providers.accent_picker.as_mut() {
            picker.hsv = hsv;
            self.providers.accent.update(cx, |input, cx| {
                input.set_value(picker::from_hsv(hsv, ""), window, cx)
            });
            cx.notify();
        }
    }

    pub(super) fn render_accent_picker(
        &mut self,
        selected: &Selected,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let instance = selected.row.instance_id.clone();
        let saved = selected.accent;
        let allowed = self.accent_allowed(&instance, cx);
        if self.providers.accent_picker.as_ref().is_some_and(|picker| {
            picker.instance != instance
                || picker.base != saved
                || picker.owner != self.scoped_primary_owner(cx)
                || picker.connection != self.state.read(cx).connection_id()
                || !allowed
        }) {
            self.providers.accent_picker = None;
        }
        let p = palette(cx);
        let picked = self
            .providers
            .accent_picker
            .as_ref()
            .map(|picker| hsv_color(picker.hsv))
            .or_else(|| saved.map(hsla));
        let trigger_focus = self.providers.accent_trigger.clone();
        let color_label = t(
            cx,
            if saved.is_some() {
                "Change accent color"
            } else {
                "Add accent color"
            },
        );
        let trigger = Button::new(
            "provider-accent-trigger",
            ButtonVariant::GhostMuted,
            ButtonSize::IconSm,
        )
        .focus_handle(trigger_focus.clone())
        .tooltip(color_label)
        .disabled(!allowed)
        .when(picked.is_none(), |button| button.icon(Icon::Plus))
        .on_click(cx.listener(move |this, _, window, cx| {
            if this.providers.accent_picker.is_some() {
                this.dismiss_accent_picker(window, true, cx);
                return;
            }
            if !this.accent_allowed(&instance, cx) {
                return;
            }
            let value = saved
                .map(|color| format!("#{:06x}", color >> 8))
                .unwrap_or_else(|| INITIAL_COLOR.to_owned());
            let focus = std::array::from_fn(|_| cx.focus_handle().tab_stop(true));
            this.providers.accent_picker = Some(AccentPicker {
                instance: instance.clone(),
                owner: this.scoped_primary_owner(cx),
                connection: this.state.read(cx).connection_id(),
                base: saved,
                hsv: picker::hsv(&value),
                bounds: std::array::from_fn(|_| Rc::default()),
                focus: focus.clone(),
                trigger: trigger_focus.clone(),
                drag: Drag::default(),
            });
            this.providers
                .accent
                .update(cx, |input, cx| input.set_value(value, window, cx));
            window.defer(cx, move |window, cx| focus[0].focus(window, cx));
            cx.notify();
        }));
        let trigger_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
        div()
            .relative()
            .child(trigger)
            .children(picked.map(|color| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(r(18.))
                            .rounded(r(4.))
                            .bg(color)
                            .border_1()
                            .border_color(hsla(p.border)),
                    )
            }))
            .children(
                self.providers
                    .accent_picker
                    .as_ref()
                    .map(|picker| self.accent_popup(picker, trigger_bounds.clone(), window, cx)),
            )
            .child(crate::settings::appearance::measure_popover_trigger(
                trigger_bounds,
            ))
            .into_any_element()
    }

    fn accent_popup(
        &self,
        picker: &AccentPicker,
        trigger_bounds: Rc<Cell<Bounds<Pixels>>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let hsv = picker.hsv;
        let radius = window.rem_size() * (6. / 16.);
        let saturation = linear_gradient(
            90.,
            linear_color_stop(
                hsv_color(Hsv {
                    h: 0.,
                    s: 0.,
                    v: 1.,
                }),
                0.,
            ),
            linear_color_stop(
                hsv_color(Hsv {
                    h: hsv.h,
                    s: 1.,
                    v: 1.,
                }),
                1.,
            ),
        );
        let value = linear_gradient(
            180.,
            linear_color_stop(black().opacity(0.), 0.),
            linear_color_stop(black(), 1.),
        );
        let plane = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                window.paint_quad(fill(bounds, saturation).corner_radii(radius));
                window.paint_quad(fill(bounds, value).corner_radii(radius));
            },
        )
        .absolute()
        .inset_0();
        let hue_radius = window.rem_size() * (5. / 16.);
        let hue_strip = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let width = bounds.size.width / 6.;
                for i in 0..6 {
                    let hue = |i| {
                        hsv_color(Hsv {
                            h: f64::from(i) * 60.,
                            s: 1.,
                            v: 1.,
                        })
                    };
                    let left = if i == 0 { hue_radius } else { px(0.) };
                    let right = if i == 5 { hue_radius } else { px(0.) };
                    let corners = Corners {
                        top_left: left,
                        bottom_left: left,
                        top_right: right,
                        bottom_right: right,
                    };
                    let segment = Bounds::new(
                        point(bounds.left() + width * i as f32, bounds.top()),
                        size(width, bounds.size.height),
                    );
                    let gradient = linear_gradient(
                        90.,
                        linear_color_stop(hue(i), 0.),
                        linear_color_stop(hue(i + 1), 1.),
                    );
                    window.paint_quad(quad(
                        segment,
                        corners,
                        gradient,
                        px(0.),
                        transparent_black(),
                        gpui::BorderStyle::Solid,
                    ));
                }
            },
        )
        .absolute()
        .inset_0();
        let thumb = |color, x, y| {
            div()
                .absolute()
                .size(r(12.))
                .left(relative(x))
                .top(relative(y))
                .ml(r(-6.))
                .mt(r(-6.))
                .rounded_full()
                .bg(color)
                .border_2()
                .border_color(hsla(p.picker_thumb_border))
        };
        let surfaces = [
            div().relative().h(r(144.)).child(plane).child(
                div().absolute().inset(r(5.)).child(thumb(
                    hsv_color(hsv),
                    hsv.s as f32,
                    1. - hsv.v as f32,
                )),
            ),
            div().relative().h(r(10.)).child(hue_strip).child(
                div()
                    .absolute()
                    .left(r(5.))
                    .right(r(5.))
                    .top_0()
                    .bottom_0()
                    .child(thumb(
                        hsv_color(Hsv {
                            h: hsv.h,
                            s: 1.,
                            v: 1.,
                        }),
                        (hsv.h / 360.) as f32,
                        0.5,
                    )),
            ),
        ];
        let surfaces = surfaces
            .into_iter()
            .zip([Surface::SaturationValue, Surface::Hue])
            .map(|(body, surface)| {
                let index = surface.index();
                let bounds = picker.bounds[index].clone();
                let focus = picker.focus[index].clone();
                div()
                    .id(("provider-hsv-surface", index))
                    .relative()
                    .track_focus(&focus)
                    .cursor_crosshair()
                    .when(focus.is_focused(window), |this| {
                        this.rounded(r(6.)).shadow(focus_ring(hsla(p.ring)))
                    })
                    .child(body)
                    .child(
                        canvas(move |rect, _, _| bounds.set(rect), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            if let Some(picker) = this.providers.accent_picker.as_mut() {
                                picker.drag.start(surface);
                                picker.focus[index].focus(window, cx);
                            }
                            this.preview_accent(event.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let Some(picker) = this.providers.accent_picker.as_ref() else {
                            return;
                        };
                        let axis = match surface {
                            Surface::Hue => Axis::Hue,
                            Surface::SaturationValue
                                if matches!(event.keystroke.key.as_str(), "up" | "down") =>
                            {
                                Axis::Brightness
                            }
                            Surface::SaturationValue => Axis::Saturation,
                        };
                        if let Some(next) = picker::key(
                            picker.hsv,
                            axis,
                            &event.keystroke.key,
                            event.keystroke.modifiers.shift,
                        ) {
                            this.preview_accent_hsv(next, window, cx);
                            cx.stop_propagation();
                        }
                    }))
            });
        let input = &self.providers.accent;
        let valid = model::normalize_accent(input.read(cx).value().trim()).is_some();
        let popup = popup_surface(cx)
            .id("provider-accent-popup")
            .key_context("ProviderDialog")
            .w(r(224.))
            .occlude()
            .gap(r(8.))
            .on_action(cx.listener(|this, _: &CloseProviderDialog, window, cx| {
                this.dismiss_accent_picker(window, true, cx)
            }))
            .on_mouse_down_out(cx.listener(
                move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if !trigger_bounds.get().contains(&event.position) {
                        this.dismiss_accent_picker(window, false, cx);
                    }
                },
            ))
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.preview_accent(event.position, window, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    if let Some(picker) = this.providers.accent_picker.as_mut() {
                        picker.drag.end();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    if let Some(picker) = this.providers.accent_picker.as_mut() {
                        picker.drag.end();
                    }
                }),
            )
            .child(
                v_flex()
                    .gap(r(10.))
                    .p(r(8.))
                    .children(surfaces)
                    .child(
                        div()
                            .relative()
                            .children(theme_slot_clipped_background_layer(
                                ThemeSlot::Input,
                                r(6.),
                                cx,
                            ))
                            .child(Input::new(input).small().theme_slot(ThemeSlot::Input, cx)),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(r(8.))
                            .children(picker.base.is_some().then(|| {
                                Button::new(
                                    "clear-provider-accent",
                                    ButtonVariant::GhostMuted,
                                    ButtonSize::Xs,
                                )
                                .label(t(cx, "Clear color"))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.save_accent_picker(true, window, cx),
                                ))
                            }))
                            .child(
                                Button::new(
                                    "save-provider-accent",
                                    ButtonVariant::Default,
                                    ButtonSize::Xs,
                                )
                                .label(t(
                                    cx,
                                    if picker.base.is_some() {
                                        "Save"
                                    } else {
                                        "Add color"
                                    },
                                ))
                                .disabled(!valid)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.save_accent_picker(false, window, cx)
                                    },
                                )),
                            ),
                    ),
            );
        div()
            .absolute()
            .bottom_0()
            .right_0()
            .child(
                deferred(
                    anchored()
                        .anchor(Anchor::TopRight)
                        .snap_to_window_with_margin(px(8.))
                        .child(div().pt(r(6.)).child(popup)),
                )
                .with_priority(5),
            )
            .into_any_element()
    }

    pub(super) fn accent_input_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = self.providers.accent_picker.as_mut() {
            let typed = self.providers.accent.read(cx).value();
            if let Some(valid) = model::normalize_accent(typed.trim())
                && picker::from_hsv(picker.hsv, "") != valid
            {
                picker.hsv = picker::hsv(&valid);
            }
            cx.notify();
        }
    }
}

fn hsv_color(hsv: Hsv) -> Hsla {
    hsla(picker::swatch(&picker::from_hsv(hsv, "")).expect("valid HSV color"))
}
