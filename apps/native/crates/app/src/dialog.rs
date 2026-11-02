//! A confirm dialog (design-system §6.4): scrim, dialog glass, title, description, and a footer
//! with Cancel and a destructive action. Escape cancels, Enter confirms.

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString,
    Styled, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};

use crate::app_state::AppState;
use crate::theme::{WHITE, hsla, palette};

type OnConfirm = Box<dyn FnOnce(&mut AppState, &mut Context<AppState>)>;

struct Pending {
    title: SharedString,
    description: SharedString,
    confirm_label: SharedString,
    on_confirm: OnConfirm,
}

pub struct ConfirmDialog {
    state: Entity<AppState>,
    pending: Option<Pending>,
    focus: FocusHandle,
}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmDialog {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        ConfirmDialog { state, pending: None, focus: cx.focus_handle() }
    }

    pub fn confirm(
        &mut self,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending = Some(Pending {
            title: title.into(),
            description: description.into(),
            confirm_label: confirm_label.into(),
            on_confirm: Box::new(on_confirm),
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn resolve(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else { return };
        if confirmed {
            self.state.update(cx, |state, cx| (pending.on_confirm)(state, cx));
        }
        cx.notify();
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(pending) = self.pending.as_ref() else { return div().into_any_element() };
        let p = palette(cx);
        let button = |id: &'static str, label: SharedString, destructive: bool| {
            let (fill, text, border) = if destructive { (p.destructive, WHITE, p.destructive) } else { (p.popover, p.foreground, p.input) };
            div()
                .id(id)
                .h(px(32.))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(border))
                .bg(hsla(fill))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(text))
                .cursor_pointer()
                .hover(|this| this.opacity(0.9))
                .child(label)
        };
        deferred(
            div()
                .id("confirm-dialog-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.resolve(false, cx)))
                .child(
                    div()
                        .id("confirm-dialog")
                        .track_focus(&self.focus)
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                            "escape" => {
                                cx.stop_propagation();
                                this.resolve(false, cx);
                            }
                            "enter" => this.resolve(true, cx),
                            _ => {}
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(512.))
                        .relative()
                        .rounded(px(16.))
                        .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
                        .shadow(vec![BoxShadow {
                            color: gpui_hsla(0., 0., 0., 0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .child(
                            div()
                                .p(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(pending.title.clone()))
                                .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(pending.description.clone())),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .px(px(24.))
                                .py(px(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .rounded_b(px(15.))
                                .bg(hsla(p.muted))
                                .child(button("confirm-dialog-cancel", "Cancel".into(), false).on_click(cx.listener(|this, _, _, cx| this.resolve(false, cx))))
                                .child(button("confirm-dialog-confirm", pending.confirm_label.clone(), true).on_click(cx.listener(|this, _, _, cx| this.resolve(true, cx)))),
                        ),
                ),
        )
        .with_priority(50)
        .into_any_element()
    }
}
