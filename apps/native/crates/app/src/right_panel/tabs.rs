//! The tab bar (`RightPanelTab`, the add-surface menu, `PanelTabCloseButton`) and the empty
//! "Open a surface" launcher (`RightPanelEmptyState`) from `components/RightPanelTabs.tsx`.

use gpui::{AnyElement, App, ElementId, Entity, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, WindowControlArea, div, prelude::*, px};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::right_panel::{Surface, SurfaceKind, ThreadPanel};

use super::{Button, Size, Variant, focusable, header_controls};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::workspace::TITLEBAR_HEIGHT;

/// What the launcher and the add menu offer. Native ships the diff; other surfaces join here.
struct SurfaceAction {
    label: &'static str,
    icon: Icon,
    shortcut: &'static str,
    open: fn(&mut AppState, &mut gpui::Context<AppState>),
}

const SURFACE_ACTIONS: &[SurfaceAction] = &[
    SurfaceAction { label: "Files", icon: Icon::Files, shortcut: "F", open: AppState::add_files_surface },
    SurfaceAction { label: "Diff", icon: Icon::FileDiff, shortcut: "D", open: AppState::add_diff_surface },
    SurfaceAction { label: "Linked pull requests", icon: Icon::Link2, shortcut: "L", open: AppState::add_pull_requests_surface },
];

/// Opens the launcher entry named `label` (case-insensitive), as clicking it would.
#[cfg(feature = "snapshot")]
pub fn open_surface(state: &mut AppState, label: &str, cx: &mut gpui::Context<AppState>) -> bool {
    let Some(action) = SURFACE_ACTIONS.iter().find(|action| action.label.eq_ignore_ascii_case(label)) else { return false };
    (action.open)(state, cx);
    true
}

fn surface_icon(surface: &Surface) -> Icon {
    match surface.kind {
        SurfaceKind::Diff => Icon::FileDiff,
        SurfaceKind::PullRequests => Icon::Link2,
        SurfaceKind::Files => Icon::Files,
        SurfaceKind::File { .. } => Icon::File,
        SurfaceKind::SideChat { .. } => Icon::MessagesSquare,
    }
}

/// The launcher's "Side chat" entry while the open thread offers side chats, with why it is
/// unavailable (`SURFACE_UNAVAILABLE_HINTS.sideChat` when nothing more specific applies).
fn side_chat_entry(state: &AppState) -> Option<Option<&'static str>> {
    state.side_chats_offered().then(|| state.side_chat_disabled_reason().or(state.side_threads.is_starting().then_some("Starting a side chat…")))
}

fn open_side_chat(state: &mut AppState, cx: &mut gpui::Context<AppState>) {
    state.open_side_chat(None, cx);
}

pub fn tab_bar(state: Entity<AppState>, panel: &ThreadPanel, titlebar_inset: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let count = panel.surfaces.len();
    let tabs: Vec<_> = panel
        .surfaces
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            let active = panel.active_surface_id.as_deref() == Some(surface.id.as_str());
            tab(state.clone(), surface, index, count, active, window, cx)
        })
        .collect();
    let add_state = state.clone();
    let side_chat = side_chat_entry(state.read(cx));
    h_flex()
        .h(px(TITLEBAR_HEIGHT))
        .flex_none()
        .pl(px(if titlebar_inset { 130. } else { 8. }))
        .pr(px(12.))
        .gap(px(4.))
        .window_control_area(WindowControlArea::Drag)
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(4.))
                .overflow_hidden()
                .children(tabs)
                .child(
                    Button::new("right-panel-add")
                        .variant(Variant::GhostMuted)
                        .size(Size::IconXs)
                        .icon(Icon::Plus)
                        .tooltip("Add panel surface")
                        .dropdown_menu(move |menu, _, _| {
                            let menu = SURFACE_ACTIONS.iter().fold(menu, |menu, action| {
                                let state = add_state.clone();
                                let open = action.open;
                                menu.item(
                                    PopupMenuItem::new(action.label)
                                        .on_click(move |_, _, cx| state.update(cx, open)),
                                )
                            });
                            match side_chat {
                                Some(reason) => {
                                    let state = add_state.clone();
                                    menu.item(PopupMenuItem::new("Side chat").disabled(reason.is_some()).on_click(move |_, _, cx| state.update(cx, open_side_chat)))
                                }
                                None => menu,
                            }
                        }),
                ),
        )
        .child(header_controls(&state, cx))
        .into_any_element()
}

fn tab(state: Entity<AppState>, surface: &Surface, index: usize, count: usize, active: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let side_chat = surface.side_chat_thread().and_then(|id| state.read(cx).thread_shell(id));
    let title: SharedString = side_chat.map_or_else(|| surface.title().into(), |thread| thread.title.clone().into());
    // Static dot: a side chat tab can stay open for a whole run.
    let dot = side_chat.and_then(t3_core::side_chat::TabState::of).map(|tab_state| match tab_state {
        t3_core::side_chat::TabState::Running => hsla(p.muted_foreground),
        _ => hsla(p.info),
    });
    let id = surface.id.clone();
    let file_path = match &surface.kind {
        SurfaceKind::File { relative_path, .. } => Some(relative_path.clone()),
        _ => None,
    };
    let (activate, close, middle) = (state.clone(), state.clone(), state.clone());
    let (activate_id, close_id, middle_id) = (id.clone(), id.clone(), id.clone());
    let group: SharedString = format!("tab-{id}").into();
    let icon_color = hsla(if active { p.foreground } else { p.muted_foreground });
    let tab_id = ElementId::from(SharedString::from(format!("right-panel-tab-{id}")));
    focusable(div().id(tab_id.clone()), tab_id, true, window, cx)
        .group(group.clone())
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .max_w(px(144.))
        .gap(px(2.))
        .pl(px(6.))
        .pr(px(8.))
        .rounded(px(6.))
        .text_xs()
        .cursor_pointer()
        .when(active, |this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
        .when(!active, |this| {
            this.text_color(hsla(p.muted_foreground))
                .hover(|style| style.bg(hsla(p.accent).opacity(0.6)).text_color(hsla(p.foreground)))
        })
        .on_click(move |_, _, cx| activate.update(cx, |state, cx| state.activate_surface(&activate_id, cx)))
        .on_mouse_up(MouseButton::Middle, move |_, _, cx| {
            middle.update(cx, |state, cx| state.close_surface(&middle_id, cx));
        })
        .child(
            div()
                .id(SharedString::from(format!("right-panel-tab-close-{id}")))
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded(px(4.))
                .hover(|style| style.bg(hsla(p.muted)))
                .tooltip({
                    let label: SharedString = format!("Close {title}").into();
                    move |window, cx| super::tooltip_view(label.clone(), window, cx)
                })
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    close.update(cx, |state, cx| state.close_surface(&close_id, cx));
                })
                .child(
                    surface_icon(surface)
                        .svg()
                        .absolute()
                        .top(px(2.))
                        .left(px(2.))
                        .size(px(12.))
                        .text_color(icon_color)
                        .group_hover(group.clone(), |style| style.invisible()),
                )
                .child(
                    Icon::X
                        .svg()
                        .absolute()
                        .top(px(2.))
                        .left(px(2.))
                        .size(px(12.))
                        .text_color(hsla(p.foreground))
                        .invisible()
                        .group_hover(group, |style| style.visible()),
                )
                .children(dot.map(|dot| div().absolute().top(px(0.)).right(px(0.)).size(px(6.)).rounded_full().bg(dot))),
        )
        .child(div().min_w_0().truncate().child(title))
        .context_menu(move |menu, _, _| {
            let item = |label: &'static str, disabled: bool, run: fn(&mut AppState, &str, &mut gpui::Context<AppState>)| {
                let state = state.clone();
                let id = id.clone();
                PopupMenuItem::new(label)
                    .disabled(disabled)
                    .on_click(move |_, _, cx| state.update(cx, |state, cx| run(state, &id, cx)))
            };
            let menu = match file_path.clone() {
                Some(path) => {
                    let state = state.clone();
                    menu.item(PopupMenuItem::new("Copy path").on_click(move |_, _, cx| state.update(cx, |state, cx| state.copy_file_path(&path, cx))))
                }
                None => menu,
            };
            menu.item(item("Close", false, |state, id, cx| state.close_surface(id, cx)))
                .item(item("Close others", count <= 1, |state, id, cx| state.close_other_surfaces(id, cx)))
                .item(item("Close to the right", index + 1 >= count, |state, id, cx| state.close_surfaces_to_right(id, cx)))
                .item(item("Close all", false, |state, _, cx| state.close_all_surfaces(cx)))
        })
        .into_any_element()
}

/// `RightPanelEmptyState`: shown while the panel has no tab.
pub fn empty_state(state: Entity<AppState>, window: &mut Window, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let side_chat = side_chat_entry(state.read(cx));
    let mut rows: Vec<_> = SURFACE_ACTIONS.iter().enumerate().map(|(index, action)| {
        let state = state.clone();
        let open = action.open;
        let row_id = ElementId::from(("surface-launcher", index));
        focusable(h_flex().id(row_id.clone()), row_id, true, window, cx)
            .h(px(32.))
            .w_full()
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(8.))
            .text_sm()
            .text_color(hsla(p.foreground))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
            .on_click(move |_, _, cx| state.update(cx, open))
            .child(action.icon.svg().size(px(16.)).text_color(hsla(p.foreground)))
            .child(div().flex_1().min_w_0().truncate().child(action.label))
            .child(kbd(action.shortcut, cx))
    }).collect();
    if let Some(reason) = side_chat {
        let row_id = ElementId::from(("surface-launcher", SURFACE_ACTIONS.len()));
        let open_state = state.clone();
        rows.push(
            focusable(h_flex().id(row_id.clone()), row_id, reason.is_none(), window, cx)
                .h(px(32.))
                .w_full()
                .px(px(10.))
                .gap(px(10.))
                .rounded(px(8.))
                .text_sm()
                .text_color(hsla(p.foreground))
                .when(reason.is_some(), |this| this.opacity(0.64))
                .when(reason.is_none(), |this| {
                    this.cursor_pointer()
                        .hover(|style| style.bg(hsla(p.accent).opacity(0.6)))
                        .on_click(move |_, _, cx| open_state.update(cx, open_side_chat))
                })
                .when_some(reason, |this, reason| this.tooltip(move |window, cx| super::tooltip_view(reason.into(), window, cx)))
                .child(Icon::MessagesSquare.svg().size(px(16.)).text_color(hsla(p.foreground)))
                .child(div().flex_1().min_w_0().truncate().child("Side chat"))
                .child(kbd("S", cx)),
        );
    }
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(24.))
        .pb(px(TITLEBAR_HEIGHT))
        .child(
            v_flex()
                .w_full()
                .max_w(px(320.))
                .py(px(24.))
                .child(
                    div()
                        .mb(px(12.))
                        .text_center()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child("Open a surface"),
                )
                .child(v_flex().gap(px(2.)).children(rows)),
        )
        .into_any_element()
}

/// `components/ui/kbd.tsx`.
fn kbd(text: &'static str, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(4.))
        .rounded(px(4.))
        .bg(hsla(p.foreground).opacity(0.07))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(text)
}
