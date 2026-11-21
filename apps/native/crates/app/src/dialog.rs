//! A confirm dialog (design-system §6.4, `ConfirmDialogHost.tsx`, `confirmDialog.ts`): scrim,
//! dialog glass, title (`leading-snug`), description, FIFO queueing, `default`/`destructive`
//! variants, keyboard focus navigation (`Tab`/arrows/`Enter`/`Space`/`Escape`), and a footer with
//! Cancel and Confirm actions.

use std::collections::VecDeque;

use gpui::{
    App, BoxShadow, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    SharedString, Styled, WeakEntity, Window, deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use t3_core::thread_actions::{archive_confirmation, unpin_confirmation};

use crate::app_state::AppState;
use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
use crate::theme::{
    SlotState, ThemeSlot, ThemeSlotExt, hsla, palette, slot_border_color, slot_has_background,
    theme_slot_state_background_layer,
};

struct Host(WeakEntity<ConfirmDialog>);

impl Global for Host {}

/// Confirm button visual variant (`ConfirmDialogVariant`).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfirmVariant {
    Default,
    #[default]
    Destructive,
}

/// Which button starts focused (`ConfirmDialogOptions["initialFocus"]`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InitialFocus {
    Cancel,
    #[default]
    Confirm,
}

/// Splits a single confirmation message into `(title, description)` (`resolveConfirmDialogCopy`
/// in `ConfirmDialogHost.tsx`).
pub fn resolve_confirm_dialog_copy(message: &str) -> (String, String) {
    let normalized = message.trim();
    let lines: Vec<&str> = normalized.split('\n').collect();
    if let Some(question_index) = lines.iter().position(|line| line.trim().ends_with(['?', '？'])) {
        let title = lines[question_index].trim().to_owned();
        let description = lines
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != question_index)
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
        return (title, description);
    }
    if let Some((index, mark)) = normalized.char_indices().find(|(_, mark)| matches!(mark, '?' | '？')) {
        let after_question = index + mark.len_utf8();
        let title = normalized[..after_question].trim().to_owned();
        let description = normalized[after_question..].trim().to_owned();
        return (title, description);
    }
    let description = if normalized.is_empty() {
        "This action requires your confirmation.".to_owned()
    } else {
        normalized.to_owned()
    };
    ("Confirm action".to_owned(), description)
}

/// Asks before running `on_confirm`, from code that has no window at hand. Without a dialog
/// nothing runs: a confirmation that cannot be shown is a "no". Multiple concurrent requests
/// queue in FIFO order (`confirmDialog.ts`).
pub(crate) fn ask(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    cx: &mut App,
) {
    ask_inner(
        title,
        description,
        confirm_label,
        ConfirmVariant::Destructive,
        InitialFocus::Confirm,
        on_confirm,
        None,
        cx,
    );
}

#[allow(dead_code)]
pub(crate) fn ask_with_options(
    message: &str,
    confirm_label: impl Into<SharedString>,
    variant: ConfirmVariant,
    initial_focus: InitialFocus,
    on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    on_cancel: Option<OnConfirm>,
    cx: &mut App,
) {
    let (title, description) = resolve_confirm_dialog_copy(message);
    ask_inner(title, description, confirm_label, variant, initial_focus, on_confirm, on_cancel, cx);
}

#[allow(clippy::too_many_arguments)]
fn ask_inner(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    variant: ConfirmVariant,
    initial_focus: InitialFocus,
    on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    on_cancel: Option<OnConfirm>,
    cx: &mut App,
) {
    let Some(dialog) = cx.try_global::<Host>().and_then(|host| host.0.upgrade()) else { return };
    let entry = Pending {
        title: title.into(),
        description: description.into(),
        confirm_label: confirm_label.into(),
        variant,
        initial_focus,
        on_confirm: Box::new(on_confirm),
        on_cancel,
    };
    let focus = dialog.update(cx, |dialog, cx| {
        if dialog.pending.is_some() {
            dialog.queued.push_back(entry);
            return None;
        }
        let entry_focus = entry.initial_focus;
        dialog.focused_button = entry_focus;
        dialog.pending = Some(entry);
        cx.notify();
        Some(dialog.button_focus(entry_focus).clone())
    });
    let Some(focus) = focus else { return };
    // The caller may be inside a window update; focus once it returns.
    cx.defer(move |cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| {
                let previous = window.focused(cx);
                dialog.update(cx, |dialog, _| dialog.previous_focus = previous);
                window.focus(&focus, cx);
            });
        }
    });
}

pub(crate) fn ask_available(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    cx: &mut App,
) -> bool {
    let Some(dialog) = cx.try_global::<Host>().and_then(|host| host.0.upgrade()) else { return false; };
    if dialog.read(cx).pending.is_some() { return false; }
    ask_inner(title, description, confirm_label, ConfirmVariant::Destructive, InitialFocus::Confirm, on_confirm, None, cx);
    true
}

pub(crate) fn ask_available_cancellable(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    on_cancel: impl FnOnce(&mut AppState, &mut Context<AppState>) + 'static,
    cx: &mut App,
) -> bool {
    let Some(dialog) = cx.try_global::<Host>().and_then(|host| host.0.upgrade()) else { return false; };
    if dialog.read(cx).pending.is_some() { return false; }
    ask_inner(
        title,
        description,
        confirm_label,
        ConfirmVariant::Destructive,
        InitialFocus::Confirm,
        on_confirm,
        Some(Box::new(on_cancel)),
        cx,
    );
    true
}

pub(crate) fn is_open(cx: &App) -> bool {
    cx.try_global::<Host>().and_then(|host| host.0.upgrade()).is_some_and(|dialog| dialog.read(cx).is_open())
}

impl AppState {
    fn thread_title(&self, thread_id: &str) -> String {
        self.snapshot_thread(thread_id).map_or_else(|| "this thread".to_owned(), |thread| thread.title)
    }

    /// Unpins, asking first while "Confirm thread unpin" is on.
    pub fn request_unpin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if !crate::settings::client_settings(cx).confirm_thread_unpin() {
            return self.unpin_thread(thread_id, cx);
        }
        let (title, description) = unpin_confirmation(&self.thread_title(thread_id));
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture(self, thread_id) else { return };
        ask(title, description, "Unpin", move |state, cx| { if owner.allows(state, &t3_core::thread_actions::ThreadAction::Unpin) { state.unpin_thread(&owner.thread, cx); } }, cx);
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
        let archivable = self.snapshot_thread(thread_id).is_some_and(|thread| thread.can_archive());
        if !archivable || !crate::settings::client_settings(cx).confirm_thread_archive() {
            return self.archive_thread(thread_id, cx);
        }
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture(self, thread_id) else { return };
        ask(archive_confirmation(&self.thread_title(thread_id)), "", "Archive", move |state, cx| { if owner.allows(state, &t3_core::thread_actions::ThreadAction::Archive) { state.archive_thread(&owner.thread, cx); } }, cx);
    }
}

type OnConfirm = Box<dyn FnOnce(&mut AppState, &mut Context<AppState>)>;

struct Pending {
    title: SharedString,
    description: SharedString,
    confirm_label: SharedString,
    variant: ConfirmVariant,
    initial_focus: InitialFocus,
    on_confirm: OnConfirm,
    on_cancel: Option<OnConfirm>,
}

pub struct ConfirmDialog {
    state: Entity<AppState>,
    pending: Option<Pending>,
    queued: VecDeque<Pending>,
    focused_button: InitialFocus,
    focus: FocusHandle,
    cancel_focus: FocusHandle,
    confirm_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmDialog {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.set_global(Host(cx.weak_entity()));
        ConfirmDialog {
            state,
            pending: None,
            queued: VecDeque::new(),
            focused_button: InitialFocus::Confirm,
            focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle().tab_stop(true),
            confirm_focus: cx.focus_handle().tab_stop(true),
            previous_focus: None,
        }
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
        let entry = Pending {
            title: title.into(),
            description: description.into(),
            confirm_label: confirm_label.into(),
            variant: ConfirmVariant::Destructive,
            initial_focus: InitialFocus::Confirm,
            on_confirm: Box::new(on_confirm),
            on_cancel: None,
        };
        if self.pending.is_some() {
            self.queued.push_back(entry);
            return;
        }
        self.focused_button = entry.initial_focus;
        self.pending = Some(entry);
        self.previous_focus = window.focused(cx);
        window.focus(self.button_focus(self.focused_button), cx);
        cx.notify();
    }

    fn button_focus(&self, button: InitialFocus) -> &FocusHandle {
        match button {
            InitialFocus::Cancel => &self.cancel_focus,
            InitialFocus::Confirm => &self.confirm_focus,
        }
    }

    pub(crate) fn set_previous_focus(&mut self, focus: Option<FocusHandle>) { self.previous_focus = focus; }

    pub(crate) fn is_open(&self) -> bool { self.pending.is_some() || !self.queued.is_empty() }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn resolve_for_test(&mut self, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.resolve(confirmed, window, cx);
    }

    fn resolve(&mut self, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else { return };
        if confirmed {
            self.state.update(cx, |state, cx| (pending.on_confirm)(state, cx));
        } else if let Some(on_cancel) = pending.on_cancel {
            self.state.update(cx, |state, cx| on_cancel(state, cx));
        }
        if let Some(next) = self.queued.pop_front() {
            self.focused_button = next.initial_focus;
            self.pending = Some(next);
            window.focus(self.button_focus(self.focused_button), cx);
        } else if self.focus.contains_focused(window, cx)
            && let Some(focus) = self.previous_focus.take()
        {
            window.focus(&focus, cx);
        }
        cx.notify();
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(pending) = self.pending.as_ref() else { return div().into_any_element() };
        let p = palette(cx);
        let confirm_variant = match pending.variant {
            ConfirmVariant::Destructive => ButtonVariant::Destructive,
            ConfirmVariant::Default => ButtonVariant::Default,
        };
        let cancel = Button::new("confirm-dialog-cancel", ButtonVariant::Outline, ButtonSize::Default)
            .label(crate::i18n::t(cx, "Cancel"))
            .focus_handle(self.cancel_focus.clone())
            .on_click(cx.listener(|this, _, window, cx| this.resolve(false, window, cx)));
        let confirm = Button::new("confirm-dialog-confirm", confirm_variant, ButtonSize::Default)
            .label(crate::i18n::t(cx, pending.confirm_label.as_ref()).to_owned())
            .focus_handle(self.confirm_focus.clone())
            .on_click(cx.listener(|this, _, window, cx| this.resolve(true, window, cx)));
        let has_custom_bg = slot_has_background(ThemeSlot::DialogPopup, cx);
        let backdrop_border = slot_border_color(ThemeSlot::DialogPopup, cx).unwrap_or_else(|| hsla(p.dialog_border));
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
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.resolve(false, window, cx)))
                .child(
                    div()
                        .id("confirm-dialog")
                        .track_focus(&self.focus)
                        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                            "escape" => {
                                cx.stop_propagation();
                                this.resolve(false, window, cx);
                            }
                            "tab" | "left" | "right" => {
                                cx.stop_propagation();
                                this.focused_button = if this.cancel_focus.is_focused(window) {
                                    InitialFocus::Confirm
                                } else {
                                    InitialFocus::Cancel
                                };
                                window.focus(this.button_focus(this.focused_button), cx);
                                cx.notify();
                            }
                            _ => {}
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w_full()
                        .max_w(px(512.))
                        .relative()
                        .overflow_hidden()
                        .rounded(px(16.))
                        .when(!has_custom_bg, |this| {
                            this.child(crate::glass::backdrop(16., hsla(p.dialog_surface), backdrop_border, cx))
                        })
                        .when(has_custom_bg, |this| {
                            this.bg(hsla(p.dialog_surface)).border_1().border_color(backdrop_border)
                        })
                        .children(theme_slot_state_background_layer(ThemeSlot::DialogPopup, SlotState::Base, px(16.), cx))
                        .shadow(vec![BoxShadow {
                            color: gpui_hsla(0., 0., 0., 0.65),
                            offset: point(px(0.), px(24.)),
                            blur_radius: px(64.),
                            spread_radius: px(-24.),
                            inset: false,
                        }])
                        .theme_slot(ThemeSlot::DialogPopup, cx)
                        .child(
                            div()
                                .p(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                // `leading-snug` (`pingdotgg/t3code#30527`): keeps multi-line titles readable.
                                .child(div().text_xl().line_height(px(27.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(crate::i18n::t(cx, pending.title.as_ref()).to_owned()))
                                .when(!pending.description.is_empty(), |this| {
                                    this.child(div().text_sm().text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, pending.description.as_ref()).to_owned()))
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
                                .child(cancel)
                                .child(confirm),
                        ),
                ),
        )
        .with_priority(50)
        .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::cell::RefCell;

    #[test]
    fn resolves_confirm_dialog_copy_from_single_message() {
        assert_eq!(
            resolve_confirm_dialog_copy("Delete thread?\nThis cannot be undone."),
            ("Delete thread?".to_owned(), "This cannot be undone.".to_owned())
        );
        assert_eq!(
            resolve_confirm_dialog_copy("Remove environment? It will disconnect."),
            ("Remove environment?".to_owned(), "It will disconnect.".to_owned())
        );
        assert_eq!(
            resolve_confirm_dialog_copy("移除环境？连接将断开。"),
            ("移除环境？".to_owned(), "连接将断开。".to_owned())
        );
        assert_eq!(
            resolve_confirm_dialog_copy("Archive this item"),
            ("Confirm action".to_owned(), "Archive this item".to_owned())
        );
    }

    #[gpui::test]
    async fn confirm_dialog_queues_in_fifo_order_and_supports_initial_focus(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        let window = cx.open_window(gpui::size(px(800.), px(600.)), |_, cx| ConfirmDialog::new(state.clone(), cx));
        let dialog = window.entity(cx).unwrap();
        let log = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        let l1 = log.clone();
        let l1_cancel = log.clone();
        let l2 = log.clone();
        cx.update(|cx| {
            ask_with_options(
                "First question?\nDetails 1",
                "Confirm",
                ConfirmVariant::Default,
                InitialFocus::Cancel,
                move |_, _| l1.borrow_mut().push("first-confirm"),
                Some(Box::new(move |_, _| l1_cancel.borrow_mut().push("first-cancel"))),
                cx,
            );
            ask(
                "Second question?",
                "Details 2",
                "Delete",
                move |_, _| l2.borrow_mut().push("second-confirm"),
                cx,
            );
        });
        cx.run_until_parked();

        // First dialog is shown with initial focus on Cancel; second is queued.
        cx.update_window(window.into(), |_, window, cx| {
            dialog.update(cx, |d, cx| {
                assert!(d.is_open());
                assert_eq!(d.focused_button, InitialFocus::Cancel);
                assert_eq!(d.pending.as_ref().unwrap().title.as_ref(), "First question?");
                assert_eq!(d.queued.len(), 1);
                d.resolve(false, window, cx);
                // Second dialog opens immediately with focus on Confirm.
                assert!(d.is_open());
                assert_eq!(d.focused_button, InitialFocus::Confirm);
                assert_eq!(d.pending.as_ref().unwrap().title.as_ref(), "Second question?");
                d.resolve(true, window, cx);
                assert!(!d.is_open());
            });
        })
        .unwrap();

        assert_eq!(*log.borrow(), vec!["first-cancel", "second-confirm"]);

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn confirm_dialog_renders_with_dialog_popup_and_button_slot_overrides(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.update(|cx| {
            let colors = t3_core::themes::standard(true);
            crate::theme::preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        [data-slot="dialog-popup"] { background: #141622; border: 2px solid #7aa2f7; border-radius: 20px; }
                        [data-slot="button"] { background: #223344; border-radius: 10px; }
                        [data-slot="button"]:hover { background: #334455; }
                        [data-slot="button"]:active { background: #112233; }
                        "#,
                    ),
                    true,
                )),
                cx,
            );
        });
        let (_dialog, visual_cx) = cx.add_window_view(|_, cx| ConfirmDialog::new(state.clone(), cx));
        visual_cx.update(|_, cx| {
            ask("Delete thread?", "This cannot be undone.", "Delete", |_, _| {}, cx);
        });
        visual_cx.refresh().unwrap();

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            crate::theme::preview(None, cx);
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
