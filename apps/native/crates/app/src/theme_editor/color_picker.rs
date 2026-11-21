//! Native controls for `ThemeColorPicker.tsx`; conversion and rollback live in core.
use super::*;
pub(super) struct Picker {
    role: &'static str,
    hsv: picker::Hsv,
    hex: Entity<InputState>,
    rgb: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    bounds: [Rc<Cell<Bounds<Pixels>>>; 2],
    focus: [FocusHandle; 3],
    drag: picker::Drag,
    pending: picker::FrameCommit<picker::Hsv>,
    epoch: usize,
}
impl Editor {
    pub(super) fn open_picker(&mut self, role: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.end_picker(false, window, cx);
        self.selected = Some(role);
        self.session.start_color_edit();
        self.picker_epoch = self.picker_epoch.wrapping_add(1);
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
                            this.flush_picker(window, cx);
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
            drag: picker::Drag::default(),
            pending: picker::FrameCommit::default(),
            epoch: self.picker_epoch,
        });
        self.picker.as_ref().unwrap().focus[0].focus(window, cx);
        cx.notify();
    }
    pub(super) fn end_picker(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !cancel {
            self.flush_picker(window, cx);
        }
        if let Some(picker) = self.picker.take() {
            self.session.end_color_edit(cancel);
            self.sync(window, cx);
            self.swatches[picker.role].focus(window, cx);
        }
    }
    pub(super) fn stop_picker_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.picker.as_mut() {
            p.drag.end();
        }
        self.flush_picker(window, cx);
    }
    fn flush_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = is_current_editor(cx.entity().entity_id(), cx);
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        let Some(hsv) = p.pending.take_if(active) else {
            return;
        };
        let role = p.role;
        self.session.update_color(role, picker::from_hsv(hsv, &self.session.colors()[role]));
        self.sync(window, cx);
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
    fn picker_at(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        let Some(owner) = p.drag.surface() else {
            return;
        };
        let bounds = p.bounds[owner.index()].get();
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return;
        }
        let x = f64::from((position.x - bounds.left()) / bounds.size.width);
        let y = f64::from((position.y - bounds.top()) / bounds.size.height);
        p.hsv = p.drag.sample(p.hsv, x, y).expect("active drag");
        self.schedule_picker_commit(window, cx);
        self.sync_picker(window, cx);
        cx.notify();
    }
    fn schedule_picker_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        let epoch = p.epoch;
        if p.pending.schedule(p.hsv) {
            cx.on_next_frame(window, move |this, window, cx| {
                if this.picker.as_ref().is_some_and(|p| p.epoch == epoch) {
                    this.flush_picker(window, cx);
                }
            });
        }
    }
    #[cfg(feature = "snapshot")]
    pub(super) fn pending_sample_fixture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker("canvas", window, cx);
        self.picker.as_mut().unwrap().hsv = picker::Hsv { h: 125., s: 1., v: 0.4 };
        self.schedule_picker_commit(window, cx);
    }
    pub(super) fn render_picker(
        &self,
        editor_left: Pixels,
        editor_width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let p = palette(cx);
        let hsv = picker.hsv;
        let color = |h, s, v| {
            hsla(picker::swatch(&picker::from_hsv(picker::Hsv { h, s, v }, "")).expect("HSV"))
        };
        let selected_color = color(hsv.h, hsv.s, hsv.v);
        let thumb = |diameter, ink| {
            div()
                .absolute()
                .size(px(diameter))
                .ml(px(-diameter / 2.))
                .mt(px(-diameter / 2.))
                .rounded_full()
                .bg(ink)
                .border_2()
                .border_color(hsla(p.picker_thumb_border))
                .shadow(vec![BoxShadow {
                    color: black().opacity(0.68),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.75),
                    inset: false,
                }])
        };
        let plane_radius = window.rem_size() * (6. / 16.);
        let saturation = linear_gradient(
            90.,
            linear_color_stop(color(0., 0., 1.), 0.),
            linear_color_stop(color(hsv.h, 1., 1.), 1.),
        );
        let value = linear_gradient(
            180.,
            linear_color_stop(black().opacity(0.), 0.),
            linear_color_stop(black(), 1.),
        );
        let plane = div()
            .relative()
            .w_full()
            .h(r(160.))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        window.paint_quad(fill(bounds, saturation).corner_radii(plane_radius));
                        window.paint_quad(fill(bounds, value).corner_radii(plane_radius));
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div().absolute().inset(px(6.)).child(
                    thumb(12., selected_color)
                        .left(relative(hsv.s as f32))
                        .top(relative(1. - hsv.v as f32)),
                ),
            );
        let hue_rim = window.rem_size() * (5. / 16.);
        let hue_gradients = std::array::from_fn::<_, 6, _>(|i| {
            linear_gradient(
                90.,
                linear_color_stop(color(i as f64 * 60., 1., 1.), 0.),
                linear_color_stop(color((i + 1) as f64 * 60., 1., 1.), 1.),
            )
        });
        let hue = div()
            .relative()
            .w_full()
            .h(r(10.))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let width = bounds.size.width / 6.;
                        for (i, gradient) in hue_gradients.into_iter().enumerate() {
                            let left = if i == 0 { hue_rim } else { px(0.) };
                            let right = if i == 5 { hue_rim } else { px(0.) };
                            let radii = Corners {
                                top_left: left,
                                top_right: right,
                                bottom_left: left,
                                bottom_right: right,
                            };
                            let segment = Bounds::new(
                                point(bounds.left() + width * i as f32, bounds.top()),
                                size(width, bounds.size.height),
                            );
                            window.paint_quad(quad(
                                segment,
                                radii,
                                gradient,
                                px(0.),
                                transparent_black(),
                                BorderStyle::Solid,
                            ));
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .left(px(7.))
                    .right(px(7.))
                    .top_0()
                    .h_full()
                    .child(
                        thumb(14., color(hsv.h, 1., 1.))
                            .left(relative((hsv.h / 360.) as f32))
                            .top(relative(0.5)),
                    ),
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
                        this.flush_picker(window, cx);
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
                            if let Some(p) = this.picker.as_mut() {
                                p.drag.start(if index == 0 {
                                    picker::Surface::SaturationValue
                                } else {
                                    picker::Surface::Hue
                                });
                                p.focus[if index == 0 { 0 } else { 2 }].focus(window, cx);
                            }
                            this.picker_at(e.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
            });
        let viewport = window.viewport_size();
        let editor_right = editor_left + editor_width;
        let left_room = editor_left - px(20.);
        let right_room = viewport.width - editor_right - px(20.);
        let preferred_width = window.rem_size() * (288. / 16.);
        let place_left = left_room >= preferred_width || left_room >= right_room;
        let room = if place_left { left_room } else { right_room };
        let width = preferred_width.min(room.max(px(1.)));
        let left = if place_left {
            editor_left - px(12.) - width
        } else {
            editor_right + px(12.)
        };
        let anchor = self.swatch_bounds[picker.role].get();
        let preferred_height = window.rem_size() * (310. / 16.);
        let top = anchor.top().clamp(
            px(8.),
            (viewport.height - preferred_height - px(8.)).max(px(8.)),
        );
        let label = crate::i18n::t(
            cx,
            FAMILIES
                .iter()
                .find(|f| f.role == picker.role)
                .map_or("Color", |f| f.label),
        );
        let swatch = |diameter| {
            div()
                .flex_none()
                .size(r(diameter))
                .rounded_full()
                .bg(selected_color)
                .border_1()
                .border_color(hsla(p.foreground).opacity(0.16))
        };
        let panel = crate::composer::controls::popup_surface(cx)
            .id("theme-color-picker")
            .absolute()
            .left(left)
            .top(top)
            .w(width)
            .max_h((viewport.height - top - px(8.)).max(px(0.)))
            .overflow_y_scroll()
            .occlude()
            .child(
                h_flex()
                    .gap(r(8.))
                    .p(r(8.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(swatch(28.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label.to_owned()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(crate::i18n::t(cx, "Choose a color")),
                            ),
                    )
                    .child(
                        Button::new(
                            "close-color-picker",
                            ButtonVariant::Ghost,
                            ButtonSize::IconXs,
                        )
                        .icon(Icon::X)
                        .tooltip("Close")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.end_picker(false, window, cx)),
                        ),
                    ),
            )
            .child(
                v_flex().p(r(8.)).gap(r(12.)).children(areas).child(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("HEX"))
                                .child(h_flex().gap(r(4.)).child(swatch(14.)).child(
                                    div().flex_1().min_w_0().child(theme_input(
                                        &picker.hex,
                                        true,
                                        cx,
                                    )),
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("RGB"))
                                .child(theme_input(&picker.rgb, true, cx)),
                        ),
                ),
            );
        Some(
            deferred(
                div()
                    .absolute()
                    .inset_0()
                    .child({
                        let view = cx.entity().clone();
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, _cx| {
                                let moves = view.clone();
                                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                                    if phase != DispatchPhase::Capture {
                                        return;
                                    }
                                    moves.update(cx, |this, cx| {
                                        if !this.picker.as_ref().is_some_and(|p| p.drag.surface().is_some()) {
                                            return;
                                        }
                                        if event.pressed_button == Some(MouseButton::Left) {
                                            this.picker_at(event.position, window, cx);
                                        } else {
                                            this.picker.as_mut().unwrap().drag.end();
                                            this.flush_picker(window, cx);
                                        }
                                        cx.stop_propagation();
                                    });
                                });
                                let ups = view.clone();
                                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                                    if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                                        return;
                                    }
                                    ups.update(cx, |this, cx| {
                                        if !this.picker.as_ref().is_some_and(|p| p.drag.surface().is_some()) {
                                            return;
                                        }
                                        this.picker_at(event.position, window, cx);
                                        this.picker.as_mut().unwrap().drag.end();
                                        this.flush_picker(window, cx);
                                        cx.stop_propagation();
                                    });
                                });
                            },
                        )
                        .absolute()
                        .inset_0()
                    })
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
