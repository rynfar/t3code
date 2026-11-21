//! The chat header (ledger TV-HEADER): the breadcrumb and the panel controls at its right edge.
//! Ports `components/chat/ChatHeader.tsx` (project, parent agent and title, the title's thread
//! menu and double-click rename) and `PanelLayoutControls.tsx` (§7.4). A draft keeps the
//! new-thread breadcrumb.

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, Point, Render, SharedString, Styled,
    Subscription, TextRun, Window, div, point, prelude::*, px,
};
use gpui_component::ActiveTheme;
use gpui_component::h_flex;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use t3_core::thread_details::{header_project_title, link_origin_badge, resolve_rename_commit, should_collapse_parent_crumb};

use crate::app_state::{AppEvent, AppState};
use crate::components::badge::{BadgeKind, badge};
use crate::context_menu::ContextMenu;
use crate::icons::Icon;
use crate::keymap;
use crate::right_panel::{self, Button, Variant};
use crate::theme::{
    ThemeSlot, ThemeSlotExt as _, ThemeSlotHoverExt as _, hsla, palette, theme_slot_background_layer,
};
use crate::thread_details::{self, ToggleThreadPanel, canvas_width};
use crate::toast::{Toast, ToastKind};
use crate::workspace::{sidebar_inline_width, traffic_light_inset};

const PROJECT_CRUMB_MAX_WIDTH: f32 = 160.;
const PARENT_CRUMB_MAX_WIDTH: f32 = 320.;

fn measure_text_width(text: &str, font_size: Pixels, window: &Window, cx: &App) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let font = gpui::font(cx.theme().font_family.clone());
    let run = TextRun { len: text.len(), font, color: gpui::black(), background_color: None, underline: None, strikethrough: None };
    f32::from(window.text_system().shape_line(SharedString::from(text.to_owned()), font_size, &[run], None).width)
}

pub struct ChatHeader {
    state: Entity<AppState>,
    menu: Entity<ContextMenu>,
    rename: Entity<InputState>,
    renaming: Option<String>,
    rename_initial: Option<String>,
    rename_committed: bool,
    rename_owner: Option<crate::context_menu::ThreadMenuOwner>,
    rename_previous_focus: Option<gpui::FocusHandle>,
    guest: Entity<crate::guest_share::GuestIndicator>,
    _subscriptions: Vec<Subscription>,
}

impl ChatHeader {
    pub fn new(state: Entity<AppState>, menu: Entity<ContextMenu>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rename = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
                AppEvent::Selection => {
                    this.cancel_rename(cx);
                    this.rename_committed = false;
                    cx.notify();
                }
                AppEvent::Shell => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&rename, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    if crate::sidebar::is_composing(&this.rename, window, cx) {
                        return;
                    }
                    this.rename_committed = true;
                    this.commit_rename(window, cx);
                }
                InputEvent::Blur => {
                    if this.rename_committed {
                        return;
                    }
                    this.commit_rename(window, cx);
                }
                _ => {}
            }),
        ];
        let guest = cx.new(|cx| crate::guest_share::GuestIndicator::new(state.clone(), cx));
        ChatHeader {
            state,
            menu,
            rename,
            renaming: None,
            rename_initial: None,
            rename_committed: false,
            rename_owner: None,
            rename_previous_focus: None,
            guest,
            _subscriptions: subscriptions,
        }
    }

    fn start_rename(&mut self, thread_id: String, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(owner) = crate::context_menu::ThreadMenuOwner::capture(self.state.read(cx), &thread_id) else { return };
        if !owner.allows(self.state.read(cx), &t3_core::thread_actions::ThreadAction::Rename) { return; }
        self.rename_owner = Some(owner);
        self.rename_initial = Some(title.clone());
        self.rename_committed = false;
        self.rename_previous_focus = window.focused(cx);
        self.rename.update(cx, |input, cx| {
            input.set_value(title, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(thread_id);
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.renaming.take() else { return };
        let initial = self.rename_initial.take().unwrap_or_default();
        let draft = self.rename.read(cx).value().to_string();
        if self.rename.read(cx).focus_handle(cx).is_focused(window) && let Some(focus) = self.rename_previous_focus.take() { window.focus(&focus, cx); }
        let owner = self.rename_owner.take();
        if draft.trim().is_empty() {
            self.state.update(cx, |state, cx| {
                state.show_toast(Toast::new(ToastKind::Warning, "Thread title cannot be empty"), cx);
            });
            cx.notify();
            return;
        }
        if let Some(title) = resolve_rename_commit(&initial, &draft) {
            self.state.update(cx, |state, cx| {
                if owner.is_some_and(|owner| owner.allows(state, &t3_core::thread_actions::ThreadAction::Rename)) {
                    state.rename_thread(&thread_id, &title, cx);
                }
            });
        }
        cx.notify();
    }

    fn open_menu(&mut self, thread_id: String, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let header = cx.weak_entity();
        let window_handle = window.window_handle();
        self.menu.update(cx, |menu, cx| {
            menu.open_for_header(&thread_id, position, window, cx);
            menu.on_header_rename(move |cx| {
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = header.update(cx, |this, cx| {
                        let state = this.state.read(cx);
                        if state.selected_thread.as_deref() == Some(thread_id.as_str())
                            && let Some(title) = state.thread_shell(&thread_id).map(|thread| thread.title.clone())
                        {
                            this.start_rename(thread_id.clone(), title, window, cx);
                        }
                    });
                });
            });
        });
    }

    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            self.rename_initial = None;
            self.rename_owner = None;
            self.rename_previous_focus = None;
            cx.notify();
        }
    }

    fn title_clicked(&mut self, event: &ClickEvent, thread_id: String, title: String, window: &mut Window, cx: &mut Context<Self>) {
        if event.click_count() >= 2 {
            self.menu.update(cx, |menu, cx| menu.close(cx));
            self.start_rename(thread_id, title, window, cx);
        } else {
            let position = point(event.position().x, event.position().y + px(4.));
            self.open_menu(thread_id, position, window, cx);
        }
    }

}

impl Render for ChatHeader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.state.read(cx);
        let p = palette(cx);
        let separator = || div().flex_none().text_color(hsla(p.icon_muted)).child("/");
        let Some(thread) = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) else {
            let project_id = app.draft_project.clone();
            let project = project_id.as_deref().and_then(|id| app.shell.project(id)).map(|project| {
                let scratch = app.is_scratch_project(&project.id);
                let title = if scratch {
                    crate::i18n::t(cx, "No project").to_owned()
                } else {
                    header_project_title(&project.title, scratch).to_owned()
                };
                let favicon = crate::project_icon::render(app, &project.id, crate::project_icon::size(14., cx), hsla(p.icon_muted), cx);
                (project.id.clone(), title, scratch, favicon)
            });
            let draft_label = app.draft_project.is_some().then(|| crate::i18n::t(cx, "New thread"));
            let state = self.state.clone();
            let settings_project = project.as_ref().filter(|(_, _, scratch, _)| !*scratch).map(|(id, _, _, _)| id.clone());
            return h_flex()
                .id("chat-header-breadcrumb")
                .flex_1()
                .min_w_0()
                .gap(px(12.))
                .text_sm()
                .children(project.map(|(project_id, name, _, favicon)| {
                    let state = state.clone();
                    let tooltip: SharedString = crate::i18n::t_fmt(cx, "New thread in {project}", &[("project", name.as_str().into())]).into();
                    h_flex()
                        .id("chat-header-project")
                        .flex_shrink(1.)
                        .min_w_0()
                        .gap(px(6.))
                        .cursor_pointer()
                        .text_color(hsla(p.muted_foreground))
                        .hover(|this| this.text_color(hsla(p.foreground)))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .child(favicon)
                        .child(div().max_w(px(160.)).truncate().child(name))
                        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.start_draft(&project_id, cx)))
                }))
                .when(draft_label.is_some(), |this| this.child(separator()))
                .children(draft_label.map(|label| div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(label)))
                .map(|this| match settings_project {
                    Some(project_id) => this
                        .context_menu(move |menu, _, cx| {
                            let project_id = project_id.clone();
                            menu.item(PopupMenuItem::new(crate::i18n::t(cx, "Project settings")).on_click(move |_, _, cx| crate::settings::open_project(&project_id, cx)))
                        })
                        .into_any_element(),
                    None => this.into_any_element(),
                });
        };
        let thread_id = thread.id.clone();
        let title = thread.title.clone();
        let project = app.shell.project(&thread.project_id).map(|project| {
            let scratch = app.is_scratch_project(&project.id);
            let name = if scratch {
                crate::i18n::t(cx, "No project").to_owned()
            } else {
                header_project_title(&project.title, scratch).to_owned()
            };
            (project.id.clone(), name)
        });
        let favicon = crate::project_icon::render(app, &thread.project_id, crate::project_icon::size(14., cx), hsla(p.icon_muted), cx);
        let parent = (thread.lineage.relationship_to_parent.as_deref() == Some("subagent"))
            .then(|| thread.lineage.parent_thread_id.clone())
            .flatten()
            .map(|id| {
                let title = app.thread_shell(&id).map_or_else(|| crate::i18n::t(cx, "Parent thread").to_owned(), |parent| parent.title.clone());
                (id, title)
            });
        let link_badge = thread.link_origin.as_ref().and_then(link_origin_badge);
        let has_guest = app.guest_share(&thread.id).is_some();
        let viewport_width = f32::from(window.viewport_size().width);
        let sidebar_w = sidebar_inline_width(app, viewport_width);
        let panel_open = app.panels.panels.is_open(&thread.id);
        let titlebar_left = f32::from(traffic_light_inset(window, sidebar_w == 0.));
        let controls_reserved = if panel_open { 40. } else { 104. };
        let available_header = (canvas_width(app, viewport_width) - titlebar_left - 12. - controls_reserved - 48.).max(0.);
        let font_sm = px(14.);
        let font_xs = px(10.);
        let project_w = project
            .as_ref()
            .map_or(0., |(_, name)| measure_text_width(name, font_sm, window, cx).min(PROJECT_CRUMB_MAX_WIDTH) + 20.);
        let parent_w = parent.as_ref().map_or(0., |(_, parent_title)| {
            measure_text_width(parent_title, font_sm, window, cx).min(PARENT_CRUMB_MAX_WIDTH) + 32.
        });
        let title_w = measure_text_width(&title, font_sm, window, cx).max(40.);
        let link_w = link_badge.as_ref().map_or(0., |(text, _)| measure_text_width(text, font_xs, window, cx).min(160.) + 18.);
        let guest_w = if has_guest { 120. } else { 0. };
        let measured_content = project_w + 32. + parent_w + title_w + 18. + link_w + guest_w;
        let parent_collapsed = should_collapse_parent_crumb(available_header, measured_content);
        let crumb = |id: &'static str, icon: Option<gpui::AnyElement>, text: String, max_w: f32, tooltip: String| {
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
                .child(div().max_w(px(max_w)).truncate().child(text))
        };
        let state = self.state.clone();
        let menu_thread = thread_id.clone();
        let title_element = match self.renaming.as_deref() {
            Some(renaming) if renaming == thread_id => div()
                .id("chat-header-rename")
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .rounded(px(4.))
                .border_1()
                .border_color(hsla(p.ring).opacity(if self.rename.read(cx).focus_handle(cx).is_focused(window) { 1. } else { 0.5 }))
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    let previous = this.rename_previous_focus.take();
                    this.cancel_rename(cx);
                    if let Some(focus) = previous { window.focus(&focus, cx); }
                    cx.stop_propagation();
                }))
                .child(Input::new(&self.rename).appearance(false).bordered(false).theme_slot(ThemeSlot::Input, cx))
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
                            .tooltip(|window, cx| Tooltip::new("Thread actions").build(window, cx))
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
                h_flex()
                    .flex_shrink(1.)
                    .min_w_0()
                    .gap(px(12.))
                    .child(
                        crumb("chat-header-project", Some(favicon), name.clone(), PROJECT_CRUMB_MAX_WIDTH, format!("New thread in {name}"))
                            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.start_draft(&project_id, cx))),
                    )
                    .child(separator())
            }))
            .children(parent.map(|(parent_id, parent_title)| {
                let state = self.state.clone();
                let label = if parent_collapsed { "...".to_owned() } else { parent_title.clone() };
                h_flex()
                    .flex_shrink(1.)
                    .min_w_0()
                    .gap(px(12.))
                    .child(crumb("chat-header-parent", None, label, PARENT_CRUMB_MAX_WIDTH, parent_title).on_click(move |_, _, cx| state.update(cx, |state, cx| state.select_thread(&parent_id, cx))))
                    .child(separator())
            }))
            .child(title_element)
            .children(link_badge.map(|(text, tip)| {
                let tip: SharedString = tip.into();
                div()
                    .id("chat-header-link-origin")
                    .flex_none()
                    .child(badge(text, BadgeKind::Info, false, cx))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            }))
            .child(self.guest.clone())
            .into_any_element()
    }
}

#[cfg(test)]
pub(crate) fn container(collapsed: bool, cx: &App) -> gpui::Div {
    container_with_layout(px(if collapsed { 52. } else { 20. }), true, cx)
}

/// Outer chat header bar (`[data-chat-header]`): full-width titlebar strip carrying compiled
/// `ThemeSlot::ChatHeader` overrides and any scoped `var(--theme-background-image)` layer painted
/// behind the breadcrumb and panel controls.
pub(crate) fn container_with_layout(left: Pixels, border: bool, cx: &App) -> gpui::Div {
    h_flex()
        .relative()
        .overflow_hidden()
        .window_control_area(gpui::WindowControlArea::Drag)
        .h(px(crate::workspace::TITLEBAR_HEIGHT))
        .flex_none()
        .pl(left)
        .pr(px(12.))
        .gap_3()
        .when(border, |bar| bar.border_b_1())
        .border_color(cx.theme().border)
        .theme_slot_hover(ThemeSlot::ChatHeader, cx)
        .children(theme_slot_background_layer(ThemeSlot::ChatHeader, cx))
}

/// `PanelLayoutControls`: Thread details, Terminal (the drawer's toggle) and Right panel toggles
/// (ghost `sm` toggles, 16px icons). While the right panel is open its tab bar holds the panel
/// toggles, so the header keeps Thread details (`ThreadPanelHeaderControl`).
pub fn controls(state: &Entity<AppState>, terminal_toggle: Option<AnyElement>, window: &Window, cx: &App) -> impl IntoElement {
    let app = state.read(cx);
    let panel_open = app.panel_thread().is_some_and(|thread| app.panels.panels.is_open(&thread));
    let details_available = thread_details::panel_key(app).is_some();
    let layout = thread_details::layout(state, window, cx);
    let label = keymap::label("threadPanel.toggle", cx).map_or_else(|| "Toggle thread details".to_owned(), |key| format!("Toggle thread details ({key})"));
    h_flex()
        .flex_none()
        .gap(px(4.))
        .when(details_available, |this| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn chat_header_container_applies_theme_slot_and_restores_on_clear(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);

            let colors = t3_core::themes::standard(true);
            crate::theme::preview_with_css(
                Some((
                    &colors,
                    Some(
                        r#"
                        [data-chat-header] {
                            background: #121826;
                            border-bottom: 2px solid #ff44aa;
                            padding: 4px 16px;
                        }
                        [data-chat-header]:hover {
                            opacity: 0.95;
                        }
                    "#,
                    ),
                    true,
                )),
                cx,
            );

            let mut styled = container(false, cx);
            let style = styled.style();
            assert_eq!(
                style.background,
                Some(gpui::Fill::Color(gpui::solid_background(hsla(0x1218_26ff))))
            );
            assert_eq!(style.border_color, Some(hsla(0xff44_aaff)));
            assert_eq!(style.border_widths.bottom, Some(px(2.0).into()));
            assert_eq!(style.padding.top, Some(px(4.0).into()));
            assert_eq!(style.padding.right, Some(px(16.0).into()));

            crate::theme::preview(None, cx);
            let mut restored = container(false, cx);
            let restored_style = restored.style();
            assert_eq!(restored_style.border_color, Some(cx.theme().border));
            assert_eq!(restored_style.background, None);
            assert_eq!(restored_style.padding.top, None);
            assert_ne!(restored_style.border_color, Some(hsla(0xff44_aaff)));
        });
    }
}
