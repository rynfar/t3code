//! A confirm dialog (design-system §6.4): scrim, dialog glass, title, description, and a footer
//! with Cancel and a destructive action. Escape cancels, Enter confirms. The thread actions the
//! General settings can ask to confirm (unpin, archive) go through here from every entry point
//! (`useThreadActions.ts` `confirmAndUnpinThread`, `useThreadActionMenu.ts` archive).

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    SharedString, Styled, WeakEntity, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use t3_core::thread_actions::{archive_confirmation, unpin_confirmation};

use crate::app_state::AppState;
use crate::theme::{WHITE, hsla, palette};

struct Host(WeakEntity<ConfirmDialog>);

impl Global for Host {}

/// Asks before running `on_confirm`, from code that has no window at hand. Without a dialog
/// nothing runs: a confirmation that cannot be shown is a "no".
fn ask(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    cx: &mut App,
) {
    let Some(dialog) = cx.try_global::<Host>().and_then(|host| host.0.upgrade()) else { return };
    let focus = dialog.update(cx, |dialog, cx| {
        dialog.pending = Some(Pending {
            title: title.into(),
            description: description.into(),
            confirm_label: confirm_label.into(),
            on_confirm: Box::new(on_confirm),
        });
        cx.notify();
        dialog.focus.clone()
    });
    // The caller may be inside a window update; focus once it returns.
    cx.defer(move |cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| window.focus(&focus, cx));
        }
    });
}

impl AppState {
    fn thread_title(&self, thread_id: &str) -> String {
        self.sidebar.model.thread(thread_id).map_or_else(|| "this thread".to_owned(), |thread| thread.title.clone())
    }

    /// Unpins, asking first while "Confirm thread unpin" is on.
    pub fn request_unpin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if !crate::settings::client_settings(cx).confirm_thread_unpin() {
            return self.unpin_thread(thread_id, cx);
        }
        let (title, description) = unpin_confirmation(&self.thread_title(thread_id));
        let id = thread_id.to_owned();
        ask(title, description, "Unpin", move |state, cx| state.unpin_thread(&id, cx), cx);
    }

    /// Unpins a multi-selection, asking once first while "Confirm thread unpin" is on.
    pub fn request_unpin_threads(&mut self, thread_ids: Vec<String>, cx: &mut Context<Self>) {
        let confirm = crate::settings::client_settings(cx).confirm_thread_unpin() && !thread_ids.is_empty();
        let (title, description) = match thread_ids.as_slice() {
            [only] => unpin_confirmation(&self.thread_title(only)),
            ids => (format!("Unpin {} threads?", ids.len()), "This will move the threads out of your pinned section."),
        };
        let unpin = move |state: &mut AppState, cx: &mut Context<AppState>| {
            for id in &thread_ids {
                state.unpin_thread(id, cx);
            }
            state.clear_thread_selection(cx);
        };
        if confirm {
            ask(title, description, "Unpin", unpin, cx);
        } else {
            unpin(self, cx);
        }
    }

    /// Archives, asking first while "Confirm thread archive" is on. A thread that cannot be
    /// archived says why without asking.
    pub fn request_archive_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let archivable = self.sidebar.model.thread(thread_id).is_some_and(|thread| thread.can_archive());
        if !archivable || !crate::settings::client_settings(cx).confirm_thread_archive() {
            return self.archive_thread(thread_id, cx);
        }
        let id = thread_id.to_owned();
        ask(archive_confirmation(&self.thread_title(thread_id)), "", "Archive", move |state, cx| state.archive_thread(&id, cx), cx);
    }
}

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
        cx.set_global(Host(cx.weak_entity()));
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
                                .when(!pending.description.is_empty(), |this| {
                                    this.child(div().text_sm().text_color(hsla(p.muted_foreground)).child(pending.description.clone()))
                                }),
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
