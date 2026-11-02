//! Window root: titlebar strip, sidebar, and the thread area, or the connect screen.

use gpui::{App, ClipboardItem, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, WindowControlArea, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::supervisor::ConnectionStatus;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::composer::{Composer, OpenPermissions, ToggleModelPicker};
use crate::connect::ConnectView;
use crate::new_thread::{self, DraftHero};
use crate::context_menu::ContextMenu;
use crate::dialog::ConfirmDialog;
use crate::icons::Icon;
use crate::keymap::{
    self, CopyThreadReference, JumpToThread1, JumpToThread2, JumpToThread3, JumpToThread4, JumpToThread5, JumpToThread6, JumpToThread7, JumpToThread8,
    JumpToThread9, NextThread, PinThread, PreviousThread, SettleThread, ToggleCommandPalette, ToggleSidebar, UndoThreadAction, Unimplemented,
};
use crate::palette::CommandPalette;
use crate::sidebar::SidebarPane;
use crate::theme::{hsla, palette};
use crate::thread_view::ThreadView;

pub const TITLEBAR_HEIGHT: f32 = 52.;
pub const SIDEBAR_WIDTH: f32 = 256.;

pub struct Workspace {
    state: Entity<AppState>,
    sidebar: Entity<SidebarPane>,
    thread: Entity<ThreadView>,
    composer: Entity<Composer>,
    draft: Entity<DraftHero>,
    connect: Entity<ConnectView>,
    menu: Entity<ContextMenu>,
    dialog: Entity<ConfirmDialog>,
    palette: Entity<CommandPalette>,
    _subscription: Subscription,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let menu = cx.new(|cx| ContextMenu::new(state.clone(), cx));
        let dialog = cx.new(|cx| ConfirmDialog::new(state.clone(), cx));
        let palette = cx.new(|cx| CommandPalette::new(state.clone(), window, cx));
        let sidebar = cx.new(|cx| SidebarPane::new(state.clone(), menu.clone(), dialog.clone(), window, cx));
        let thread = cx.new(|cx| ThreadView::new(state.clone(), window, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), window, cx));
        let draft = cx.new(|cx| DraftHero::new(state.clone(), composer.clone(), cx));
        let connect = cx.new(|cx| ConnectView::new(state.clone(), window, cx));
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Phase | AppEvent::Selection | AppEvent::Notice) {
                cx.notify();
            }
        });
        Workspace { state, sidebar, thread, composer, draft, connect, menu, dialog, palette, _subscription: subscription }
    }

    fn render_titlebar(&self, cx: &App) -> impl IntoElement {
        let state = self.state.read(cx);
        let status = match &state.phase {
            Phase::Online(ConnectionStatus::Connected) => None,
            Phase::Online(ConnectionStatus::Connecting) => Some("Connecting…".to_owned()),
            Phase::Online(ConnectionStatus::Reconnecting { reason, .. }) => Some(reason.clone()),
            Phase::Online(ConnectionStatus::NeedsPairing { reason }) => Some(reason.clone()),
            Phase::Pairing | Phase::Unpaired { .. } => None,
        };
        let theme = cx.theme();
        let collapsed = state.sidebar.collapsed;
        h_flex()
            .window_control_area(WindowControlArea::Drag)
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(if collapsed { 130. } else { 20. }))
            .pr(px(12.))
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(new_thread::breadcrumb(&self.state, cx))
            .children(status.map(|status| div().text_xs().text_color(theme.muted_foreground).child(status)))
            .child(new_thread::new_thread_button(cx))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paired = matches!(self.state.read(cx).phase, Phase::Online(_));
        let theme = cx.theme();
        if !paired {
            return div().size_full().bg(theme.background).child(self.connect.clone()).into_any_element();
        }
        let has_thread = self.state.read(cx).selected_thread.is_some();
        let has_draft = !has_thread && self.state.read(cx).draft_project.is_some();
        let collapsed = self.state.read(cx).sidebar.collapsed;
        let toast = self.state.read(cx).toast.clone();
        let running = self.state.read(cx).is_selected_thread_running();
        h_flex()
            .relative()
            .size_full()
            .on_action(cx.listener(|this, _: &ToggleModelPicker, window, cx| {
                this.composer.update(cx, |composer, cx| composer.toggle_picker(window, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenPermissions, window, cx| {
                this.composer.update(cx, |composer, cx| composer.open_permissions(window, cx));
            }))
            .key_context(keymap::root_context(false, running))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.state.update(cx, |state, cx| state.toggle_sidebar(cx))))
            .on_action(cx.listener(|this, _: &PreviousThread, _, cx| this.traverse(false, cx)))
            .on_action(cx.listener(|this, _: &NextThread, _, cx| this.traverse(true, cx)))
            .on_action(cx.listener(|this, _: &SettleThread, _, cx| this.settle_selected(cx)))
            .on_action(cx.listener(|this, _: &PinThread, _, cx| this.pin_selected(cx)))
            .on_action(cx.listener(|this, _: &CopyThreadReference, _, cx| this.copy_selected_reference(cx)))
            .on_action(cx.listener(|this, _: &UndoThreadAction, _, cx| {
                if !this.state.update(cx, |state, cx| state.undo_latest(cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| this.palette.update(cx, |palette, cx| palette.toggle(window, cx))))
            .on_action(|_: &Unimplemented, _, cx| cx.propagate())
            .on_action(cx.listener(|this, _: &JumpToThread1, _, cx| this.jump(0, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread2, _, cx| this.jump(1, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread3, _, cx| this.jump(2, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread4, _, cx| this.jump(3, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread5, _, cx| this.jump(4, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread6, _, cx| this.jump(5, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread7, _, cx| this.jump(6, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread8, _, cx| this.jump(7, cx)))
            .on_action(cx.listener(|this, _: &JumpToThread9, _, cx| this.jump(8, cx)))
            .bg(theme.background)
            .text_color(theme.foreground)
            .when(!collapsed, |this| {
                this.child(
                    div()
                        .w(px(SIDEBAR_WIDTH))
                        .h_full()
                        .flex_none()
                        .bg(theme.sidebar)
                        .border_r_1()
                        .border_color(hsla(palette(cx).sidebar_edge))
                        .child(self.sidebar.clone().cached(gpui::StyleRefinement::default().size_full())),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.render_titlebar(cx))
                    .when(has_thread, |this| {
                        this.child(div().flex_1().min_h_0().child(self.thread.clone()))
                            .child(self.composer.clone())
                    })
                    .when(has_draft, |this| this.child(div().flex_1().min_h_0().child(self.draft.clone())))
                    .when(!has_thread && !has_draft, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Select a thread"),
                        )
                    }),
            )
            .child(self.render_sidebar_toggle(cx))
            .child(self.menu.clone())
            .child(self.dialog.clone())
            .child(self.palette.clone())
            .children(toast.map(|toast| toast.render(cx)))
            .into_any_element()
    }
}

impl Workspace {
    /// The workspace control at x = 91 (design-system §5), present whether or not the sidebar shows.
    fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let label = keymap::label("sidebar.toggle", cx).map_or_else(|| "Toggle sidebar".to_owned(), |key| format!("Toggle sidebar ({key})"));
        div()
            .id("sidebar-toggle")
            .absolute()
            .left(px(91.))
            .top(px(12.))
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.sidebar_accent)))
            .child(Icon::PanelLeft.sized(px(16.)).text_color(hsla(p.sidebar_icon)))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |state, cx| state.toggle_sidebar(cx))))
    }

    fn traverse(&mut self, next: bool, cx: &mut Context<Self>) {
        let target = {
            let state = self.state.read(cx);
            state.sidebar.model.adjacent_thread(state.selected_thread.as_deref(), next).map(str::to_owned)
        };
        if let Some(id) = target {
            self.state.update(cx, |state, cx| state.select_thread(&id, cx));
        }
    }

    fn jump(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.state.read(cx).sidebar.model.jump_target(index).map(str::to_owned) {
            self.state.update(cx, |state, cx| state.select_thread(&id, cx));
        }
    }

    /// `thread.settle`: settles the open thread and moves on, or un-settles a settled one.
    fn settle_selected(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(id) = state.selected_thread.clone() else { return };
            let Some(thread) = state.sidebar.model.thread(&id) else { return };
            if thread.is_settled() {
                return state.unsettle_thread(&id, cx);
            }
            let next = state.sidebar.model.adjacent_thread(Some(&id), true).or_else(|| state.sidebar.model.adjacent_thread(Some(&id), false)).map(str::to_owned);
            state.settle_thread(&id, cx);
            if let Some(next) = next {
                state.select_thread(&next, cx);
            }
        });
    }

    fn pin_selected(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(id) = state.selected_thread.clone() else { return };
            match state.sidebar.model.thread(&id).map(|thread| thread.pinned) {
                Some(true) => state.unpin_thread(&id, cx),
                Some(false) => state.pin_thread(&id, None, cx),
                None => {}
            }
        });
    }

    fn copy_selected_reference(&mut self, cx: &mut Context<Self>) {
        let reference = {
            let state = self.state.read(cx);
            state.selected_thread.as_deref().and_then(|id| state.sidebar.model.thread(id)).map(|thread| (t3_core::thread_actions::thread_reference(&thread.title, &thread.id), thread.title.clone()))
        };
        if let Some((text, title)) = reference {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.state.update(cx, |state, cx| state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Success, "Thread copied").description(title), cx));
        }
    }
}
