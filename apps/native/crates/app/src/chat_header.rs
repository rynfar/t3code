//! The chat header (ledger TV-HEADER): the breadcrumb and the panel controls at its right edge.
//! Ports `components/chat/ChatHeader.tsx` (project, parent agent and title, the title's thread
//! menu and double-click rename) and `PanelLayoutControls.tsx` (§7.4). A draft keeps the
//! new-thread breadcrumb.

use std::time::Duration;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, Point, Render, SharedString, Styled,
    Subscription, Task, Window, div, point, prelude::*, px,
};
use gpui_component::h_flex;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;

use crate::app_state::{AppEvent, AppState};
use crate::context_menu::ContextMenu;
use crate::icons::Icon;
use crate::keymap;
use crate::right_panel::{self, Button, Variant};
use crate::theme::{hsla, palette};
use crate::thread_details::{self, ToggleThreadPanel};

/// `TITLE_MENU_OPEN_DELAY_MS`: the title's menu waits out a double-click that would rename.
const TITLE_MENU_DELAY: Duration = Duration::from_millis(500);

pub struct ChatHeader {
    state: Entity<AppState>,
    menu: Entity<ContextMenu>,
    rename: Entity<InputState>,
    renaming: Option<String>,
    pending_menu: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ChatHeader {
    pub fn new(state: Entity<AppState>, menu: Entity<ContextMenu>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rename = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
                AppEvent::Selection => {
                    this.renaming = None;
                    this.pending_menu = None;
                    cx.notify();
                }
                AppEvent::Shell => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&rename, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(cx),
                _ => {}
            }),
        ];
        ChatHeader { state, menu, rename, renaming: None, pending_menu: None, _subscriptions: subscriptions }
    }

    fn start_rename(&mut self, thread_id: String, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_menu = None;
        self.rename.update(cx, |input, cx| {
            input.set_value(title, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(thread_id);
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.renaming.take() else { return };
        let title = self.rename.read(cx).value().to_string();
        self.state.update(cx, |state, cx| state.rename_thread(&thread_id, &title, cx));
        cx.notify();
    }

    fn open_menu(&mut self, thread_id: String, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_menu = None;
        self.menu.update(cx, |menu, cx| menu.open_for_thread(&thread_id, position, window, cx));
    }

    /// A single click opens the thread menu after the double-click window; a double click renames.
    fn title_clicked(&mut self, event: &ClickEvent, thread_id: String, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let position = point(event.position().x, event.position().y + px(4.));
        if event.click_count() >= 2 {
            return self.start_rename(thread_id, title, window, cx);
        }
        let window_handle = window.window_handle();
        self.pending_menu = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TITLE_MENU_DELAY).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.open_menu(thread_id, position, window, cx));
            });
        }));
    }
}

impl Render for ChatHeader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.state.read(cx);
        let Some(thread) = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) else {
            return crate::new_thread::breadcrumb(&self.state, cx).into_any_element();
        };
        let p = palette(cx);
        let thread_id = thread.id.clone();
        let title = thread.title.clone();
        let project = app.shell.project(&thread.project_id).map(|project| (project.id.clone(), project.title.clone()));
        let favicon = crate::project_icon::render(app, &thread.project_id, crate::project_icon::size(14., cx), hsla(p.icon_muted), cx);
        let parent = (thread.lineage.relationship_to_parent.as_deref() == Some("subagent"))
            .then(|| thread.lineage.parent_thread_id.clone())
            .flatten()
            .map(|id| {
                let title = app.thread_shell(&id).map_or_else(|| "Parent thread".to_owned(), |parent| parent.title.clone());
                (id, title)
            });
        let separator = || div().flex_none().text_color(hsla(p.icon_muted)).child("/");
        let crumb = |id: &'static str, icon: Option<gpui::AnyElement>, text: String, tooltip: String| {
            let tooltip: SharedString = tooltip.into();
            h_flex()
                .id(id)
                .flex_shrink(1.)
                .min_w_0()
                .gap(px(6.))
                .cursor_pointer()
                .text_color(hsla(p.muted_foreground))
                .hover(|this| this.text_color(hsla(p.foreground)))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .children(icon)
                .child(div().max_w(px(160.)).truncate().child(text))
        };
        let state = self.state.clone();
        let menu_thread = thread_id.clone();
        let title_element = match self.renaming.as_deref() {
            Some(renaming) if renaming == thread_id => div()
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                    this.renaming = None;
                    cx.notify();
                    cx.stop_propagation();
                }))
                .child(Input::new(&self.rename).appearance(false).bordered(false))
                .into_any_element(),
            _ => {
                let (click_id, click_title) = (thread_id.clone(), title.clone());
                let chevron_thread = thread_id.clone();
                let tooltip: SharedString = title.clone().into();
                h_flex()
                    .id("chat-header-title")
                    .group("chat-header-title")
                    .min_w_0()
                    .gap(px(4.))
                    .cursor_pointer()
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.title_clicked(event, click_id.clone(), click_title.clone(), window, cx)))
                    .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                    .child(
                        div()
                            .id("chat-header-title-menu")
                            .flex_none()
                            .invisible()
                            .group_hover("chat-header-title", |style| style.visible())
                            .child(Icon::ChevronDown.sized(px(14.)).text_color(hsla(p.muted_foreground)))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                let position = point(event.position().x, event.position().y + px(4.));
                                this.open_menu(chevron_thread.clone(), position, window, cx);
                            })),
                    )
                    .into_any_element()
            }
        };
        h_flex()
            .id("chat-header-breadcrumb")
            .flex_1()
            .min_w_0()
            .gap(px(12.))
            .text_sm()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if this.renaming.is_none() {
                        this.open_menu(menu_thread.clone(), event.position, window, cx);
                    }
                }),
            )
            .children(project.map(|(project_id, name)| {
                let state = state.clone();
                crumb("chat-header-project", Some(favicon), name.clone(), format!("New thread in {name}"))
                    .on_click(move |_, _, cx| state.update(cx, |state, cx| state.start_draft(&project_id, cx)))
            }))
            .child(separator())
            .children(parent.map(|(parent_id, parent_title)| {
                let state = self.state.clone();
                h_flex()
                    .flex_shrink(1.)
                    .min_w_0()
                    .gap(px(12.))
                    .child(crumb("chat-header-parent", None, parent_title.clone(), parent_title).on_click(move |_, _, cx| state.update(cx, |state, cx| state.select_thread(&parent_id, cx))))
                    .child(separator())
            }))
            .child(title_element)
            .into_any_element()
    }
}

/// `PanelLayoutControls`: Thread details, Terminal (the drawer's toggle) and Right panel toggles
/// (ghost `sm` toggles, 16px icons). While the right panel is open its tab bar holds the panel
/// toggles, so the header keeps Thread details (`ThreadPanelHeaderControl`).
pub fn controls(state: &Entity<AppState>, terminal_toggle: Option<AnyElement>, window: &Window, cx: &App) -> impl IntoElement {
    let app = state.read(cx);
    let thread = app.selected_thread.clone();
    let panel_open = thread.as_deref().is_some_and(|thread| app.panels.panels.is_open(thread));
    let layout = thread_details::layout(state, window, cx);
    let label = keymap::label("threadPanel.toggle", cx).map_or_else(|| "Toggle thread details".to_owned(), |key| format!("Toggle thread details ({key})"));
    h_flex()
        .flex_none()
        .gap(px(4.))
        .when(thread.is_some(), |this| {
            this.child(
                Button::new("thread-details-toggle")
                    .variant(Variant::Toggle)
                    .icon(Icon::SquareMenu)
                    .pressed(layout.card_open)
                    .tooltip(label)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleThreadPanel), cx)),
            )
        })
        .children(terminal_toggle)
        .when(!panel_open, |this| this.child(right_panel::header_controls(state, cx)))
}
