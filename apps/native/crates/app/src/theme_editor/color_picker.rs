//! Native controls for `ThemeColorPicker.tsx`; conversion and rollback live in core.
use super::*;
use std::{cell::Cell, rc::Rc};
pub(super) struct Picker {
    role: &'static str,
    hsv: picker::Hsv,
    hex: Entity<InputState>,
    rgb: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    bounds: [Rc<Cell<Bounds<Pixels>>>; 2],
    focus: [FocusHandle; 3],
}
impl Editor {
    pub(super) fn open_picker(&mut self, role: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.end_picker(false, window, cx);
        self.selected = Some(role);
        self.session.start_color_edit();
        let value = &self.session.colors()[role];
        let hex = cx.new(|cx| {
            InputState::new(window, cx).default_value(picker::hex(value).unwrap_or_default().chars().take(7).collect::<String>())
        });
        let rgb = cx.new(|cx| InputState::new(window, cx).default_value(picker::rgb(value).unwrap_or_default()));
        let subscriptions = [(&hex, false), (&rgb, true)]
            .into_iter()
            .map(|(input, rgb)| {
                cx.subscribe_in(input, window, move |this, input, event: &InputEvent, window, cx| {
                    if this.syncing {
                        return;
                    }
                    if matches!(event, InputEvent::Change) {
                        let Some(p) = this.picker.as_ref() else {
                            return;
                        };
                        let role = p.role;
                        let previous = &this.session.colors()[role];
                        let draft = input.read(cx).value();
                        let color = if rgb {
                            picker::from_rgb(&draft, previous)
                        } else {
                            picker::from_hex(&draft, previous)
                        };
                        if let Some(color) = color {
                            let hsv = picker::hsv(&color);
                            this.picker.as_mut().unwrap().hsv = hsv;
                            let hex_value = color.chars().take(7).collect::<String>();
                            let rgb_value = picker::rgb(&color).unwrap_or_default();
                            this.session.update_color(role, color);
                            if let Some(p) = this.picker.as_ref() {
                                if rgb {
                                    p.hex.update(cx, |s, cx| s.set_value(hex_value, window, cx));
                                } else {
                                    p.rgb.update(cx, |s, cx| s.set_value(rgb_value, window, cx));
                                }
                            }
                            this.sync(window, cx);
                        }
                    } else if matches!(event, InputEvent::Blur) {
                        this.sync_picker(window, cx);
                    }
                })
            })
            .collect();
        self.picker = Some(Picker {
            role,
            hsv: picker::hsv(value),
            hex,
            rgb,
            _subscriptions: subscriptions,
            bounds: std::array::from_fn(|_| Rc::new(Cell::new(Bounds::default()))),
            focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
        });
        cx.notify();
    }
    pub(super) fn end_picker(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            self.session.end_color_edit(cancel);
            self.preview = self.session.colors().clone();
            self.sync(window, cx);
        }
    }
    fn sync_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.picker.as_ref() {
            let value = picker::from_hsv(p.hsv, &self.session.colors()[p.role]);
            self.syncing = true;
            p.hex
                .update(cx, |s, cx| s.set_value(value.chars().take(7).collect::<String>(), window, cx));
            p.rgb
                .update(cx, |s, cx| s.set_value(picker::rgb(&value).unwrap_or_default(), window, cx));
            self.syncing = false;
        }
    }
    fn picker_at(&mut self, index: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        let bounds = p.bounds[index].get();
        let x = ((position.x - bounds.left()) / bounds.size.width).clamp(0., 1.);
        if index == 0 {
            p.hsv.s = f64::from(x);
            p.hsv.v = f64::from(1. - ((position.y - bounds.top()) / bounds.size.height).clamp(0., 1.));
        } else {
            p.hsv.h = f64::from(x) * 360.;
        }
        let role = p.role;
        let value = picker::from_hsv(p.hsv, &self.session.colors()[role]);
        self.session.update_color(role, value);
        self.sync_picker(window, cx);
        self.sync(window, cx);
    }
    pub(super) fn render_picker(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let p = palette(cx);
        let hsv = picker.hsv;
        let color = |h, s, v| hsla(picker::swatch(&picker::from_hsv(picker::Hsv { h, s, v }, "")).expect("HSV"));
        let plane = div()
            .relative()
            .w_full()
            .h(px(160.))
            .rounded(r(6.))
            .overflow_hidden()
            .bg(linear_gradient(
                90.,
                linear_color_stop(color(0., 0., 1.), 0.),
                linear_color_stop(color(hsv.h, 1., 1.), 1.),
            ))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(color(0., 0., 0.).opacity(0.), 0.),
                linear_color_stop(color(0., 0., 0.), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .left(relative(hsv.s as f32))
                    .top(relative(1. - hsv.v as f32))
                    .size(px(10.))
                    .rounded_full()
                    .border_2()
                    .border_color(color(0., 0., 1.)),
            );
        let hue = h_flex()
            .relative()
            .w_full()
            .h(px(16.))
            .rounded_full()
            .overflow_hidden()
            .children((0..6).map(|i| {
                div().h_full().flex_1().bg(linear_gradient(
                    90.,
                    linear_color_stop(color(f64::from(i) * 60., 1., 1.), 0.),
                    linear_color_stop(color(f64::from(i + 1) * 60., 1., 1.), 1.),
                ))
            }))
            .child(
                div()
                    .absolute()
                    .left(relative((hsv.h / 360.) as f32))
                    .h_full()
                    .w(px(5.))
                    .border_2()
                    .border_color(color(0., 0., 1.)),
            );
        let areas = [plane.into_any_element(), hue.into_any_element()]
            .into_iter()
            .enumerate()
            .map(|(index, element)| {
                let bounds = picker.bounds[index].clone();
                div()
                    .id(if index == 0 { "picker-plane" } else { "picker-hue" })
                    .track_focus(&picker.focus[if index == 0 { 0 } else { 2 }])
                    .relative()
                    .cursor_crosshair()
                    .child(element)
                    .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().inset_0())
                    .when(index == 0, |d| d.child(div().absolute().size(px(1.)).track_focus(&picker.focus[1])))
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        let Some(p) = this.picker.as_ref() else {
                            return;
                        };
                        let axis = if index == 1 {
                            picker::Axis::Hue
                        } else if p.focus[1].is_focused(window) {
                            picker::Axis::Brightness
                        } else {
                            picker::Axis::Saturation
                        };
                        if let Some(hsv) = picker::key(p.hsv, axis, &e.keystroke.key, e.keystroke.modifiers.shift) {
                            let role = p.role;
                            let value = picker::from_hsv(hsv, &this.session.colors()[role]);
                            this.picker.as_mut().unwrap().hsv = hsv;
                            this.session.update_color(role, value);
                            this.sync_picker(window, cx);
                            this.sync(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            if let Some(p) = this.picker.as_ref() {
                                p.focus[if index == 0 { 0 } else { 2 }].focus(window, cx);
                            }
                            this.picker_at(index, e.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, window, cx| {
                        if e.pressed_button == Some(MouseButton::Left) {
                            this.picker_at(index, e.position, window, cx);
                        }
                    }))
            });
        let viewport = window.viewport_size();
        let pos = self.position.unwrap_or(point(
            viewport.width - px(432.),
            viewport.height - px(if self.session.advanced { 688. } else { 336. }),
        ));
        let label = FAMILIES.iter().find(|f| f.role == picker.role).map_or("Color", |f| f.label);
        let panel = v_flex()
            .absolute()
            .left((pos.x - px(300.)).max(px(8.)))
            .top(pos.y.max(px(8.)).min((viewport.height - px(350.)).max(px(8.))))
            .w(px(288.))
            .rounded(r(8.))
            .overflow_hidden()
            .occlude()
            .text_color(hsla(p.popover_foreground))
            .child(crate::glass::backdrop(8., hsla(p.dropdown_surface), hsla(p.dropdown_border), cx))
            .child(
                h_flex()
                    .justify_between()
                    .p(r(12.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(
                        v_flex()
                            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(label))
                            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("Choose a color")),
                    )
                    .child(
                        Button::new("close-color-picker", ButtonVariant::Ghost, ButtonSize::IconXs)
                            .icon(Icon::X)
                            .on_click(cx.listener(|this, _, window, cx| this.end_picker(false, window, cx))),
                    ),
            )
            .child(
                v_flex().p(r(12.)).gap(r(12.)).children(areas).child(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("HEX"))
                                .child(Input::new(&picker.hex).small()),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("RGB"))
                                .child(Input::new(&picker.rgb).small()),
                        ),
                ),
            );
        Some(
            deferred(
                div()
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.end_picker(false, window, cx)),
                    )
                    .child(
                        panel
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                                if e.keystroke.key == "escape" {
                                    this.end_picker(true, window, cx);
                                    cx.stop_propagation();
                                }
                            })),
                    ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }
}
