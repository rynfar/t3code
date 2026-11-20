//! The tab bar (`RightPanelTab`, the add-surface menu, `PanelTabCloseButton`) and the empty
//! "Open a surface" launcher (`RightPanelEmptyState`) from `components/RightPanelTabs.tsx`.

use gpui::{AnyElement, App, ElementId, Entity, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, WindowControlArea, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::right_panel::{Surface, SurfaceKind, ThreadPanel};

use super::{Button, Size, Variant, focusable, header_controls_with_options};
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
    /// Why the entry is unavailable, if it is.
    unavailable: fn(&AppState, &App) -> Option<&'static str>,
}

fn always(_: &AppState, _: &App) -> Option<&'static str> {
    None
}

/// `browserAvailable` with `SURFACE_UNAVAILABLE_HINTS.browser`.
fn browser_unavailable(_: &AppState, cx: &App) -> Option<&'static str> {
    crate::preview::previews(cx).and_then(|previews| previews.read(cx).availability().launcher_hint())
}

fn files_unavailable(state: &AppState, _: &App) -> Option<&'static str> {
    state.file_workspace().is_none().then_some("Available when a project is open.")
}

fn server_thread_unavailable(state: &AppState, _: &App) -> Option<&'static str> {
    state.selected_thread.is_none().then_some("Available for server threads.")
}

fn terminal_unavailable(state: &AppState, cx: &App) -> Option<&'static str> {
    if state.file_workspace().is_none() {
        return Some("Available when a project is open.");
    }
    (!crate::terminal::can_operate_terminals(cx)).then_some("Pair this client again with terminal access to open terminals.")
}

const SURFACE_ACTIONS: &[SurfaceAction] = &[
    SurfaceAction { label: "Browser", icon: Icon::Earth, shortcut: "B", open: crate::preview::add_browser_surface, unavailable: browser_unavailable },
    SurfaceAction { label: "Files", icon: Icon::Files, shortcut: "F", open: AppState::add_files_surface, unavailable: files_unavailable },
    SurfaceAction { label: "Diff", icon: Icon::FileDiff, shortcut: "D", open: AppState::add_diff_surface, unavailable: server_thread_unavailable },
    SurfaceAction { label: "Terminal", icon: Icon::SquareTerminal, shortcut: "T", open: AppState::add_terminal_surface, unavailable: terminal_unavailable },
    SurfaceAction { label: "Linked pull requests", icon: Icon::Link2, shortcut: "L", open: AppState::add_pull_requests_surface, unavailable: server_thread_unavailable },
    SurfaceAction { label: "Device", icon: Icon::Smartphone, shortcut: "M", open: crate::device::add_device_surface, unavailable: always },
];

/// Opens the launcher entry named `label` (case-insensitive), as clicking it would.
#[cfg(feature = "snapshot")]
pub fn open_surface(state: &mut AppState, label: &str, cx: &mut gpui::Context<AppState>) -> bool {
    // An existing side chat of the open thread, by `side-chat:<thread>`; starting one would write.
    if let Some(thread_id) = label.strip_prefix("side-chat:") {
        let is_side_chat = state.selected_thread.as_deref().zip(state.thread_shell(thread_id)).is_some_and(|(parent, thread)| t3_core::side_chat::is_side_chat_of(thread, parent));
        if is_side_chat {
            state.open_related_thread(thread_id, cx);
        }
        return is_side_chat;
    }
    // An existing browser session by `browser:<tab>`, or `browser:new` for the empty state;
    // the "Browser" entry would open a session on the server.
    if let Some(tab) = label.strip_prefix("browser:") {
        let tab = (tab != "new").then_some(tab);
        state.mutate_panels(cx, |panels, thread| panels.panels.open_browser(thread, tab));
        return true;
    }
    // `launcher`: the panel open with no tab; `device:list`: the device tab without the
    // launcher's setup gate, so setup shows over it while devices are off.
    match label {
        "launcher" => {
            state.mutate_panels(cx, |panels, thread| {
                panels.panels.close_all_surfaces(thread);
                panels.panels.toggle_visibility(thread)
            });
            return true;
        }
        "device:list" => {
            state.mutate_panels(cx, |panels, thread| panels.panels.open(thread, Surface::device(None)));
            return true;
        }
        _ => {}
    }
    let Some(action) = SURFACE_ACTIONS.iter().find(|action| action.label.eq_ignore_ascii_case(label)) else { return false };
    (action.open)(state, cx);
    true
}

fn surface_icon(surface: &Surface) -> Icon {
    match &surface.kind {
        SurfaceKind::Diff => Icon::FileDiff,
        SurfaceKind::PullRequests => Icon::Link2,
        SurfaceKind::PullRequest { .. } => Icon::GitPullRequest,
        SurfaceKind::Issue(_) => Icon::CircleDot,
        SurfaceKind::Files { .. } => Icon::Files,
        SurfaceKind::File { attachment: Some(_), .. } => Icon::Paperclip,
        SurfaceKind::File { .. } => Icon::File,
        SurfaceKind::Terminal { .. } => Icon::SquareTerminal,
        SurfaceKind::SideChat { .. } => Icon::MessagesSquare,
        SurfaceKind::Preview { .. } => Icon::Earth,
        SurfaceKind::Device { .. } => Icon::Smartphone,
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

pub(super) struct AddMenuEntry {
    pub label: &'static str,
    pub icon: Icon,
    pub shortcut: &'static str,
    pub open: fn(&mut AppState, &mut gpui::Context<AppState>),
    pub unavailable: Option<&'static str>,
}

pub(super) fn add_menu_entries(state: &AppState, cx: &App) -> Vec<AddMenuEntry> {
    let mut entries: Vec<AddMenuEntry> = SURFACE_ACTIONS
        .iter()
        .map(|action| AddMenuEntry {
            label: action.label,
            icon: action.icon,
            shortcut: action.shortcut,
            open: action.open,
            unavailable: (action.unavailable)(state, cx),
        })
        .collect();
    if let Some(reason) = side_chat_entry(state) {
        entries.push(AddMenuEntry {
            label: "Side chat",
            icon: Icon::MessagesSquare,
            shortcut: "S",
            open: open_side_chat,
            unavailable: reason,
        });
    }
    entries
}

#[allow(clippy::too_many_arguments)]
pub fn tab_bar(
    state: Entity<AppState>,
    panel: &ThreadPanel,
    titlebar_inset: bool,
    sheet_mode: bool,
    add_menu_open: bool,
    renaming: Option<(&str, &Entity<InputState>)>,
    on_toggle_add: impl Fn(&mut Window, &mut App) + 'static,
    on_start_rename: impl Fn(String, String, &mut Window, &mut App) + Clone + 'static,
    on_cancel_rename: impl Fn(&mut App) + Clone + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let count = panel.surfaces.len();
    let tabs: Vec<_> = panel
        .surfaces
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            let active = panel.active_surface_id.as_deref() == Some(surface.id.as_str());
            let rename_input = renaming.and_then(|(id, input)| (id == surface.id).then_some(input));
            tab(state.clone(), surface, index, count, active, rename_input, on_start_rename.clone(), on_cancel_rename.clone(), window, cx)
        })
        .collect();
    let add_tooltip = crate::keymap::label("rightPanel.new", cx)
        .map_or_else(|| "Add panel surface".to_owned(), |key| format!("Add panel surface ({key})"));
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
                        .variant(if add_menu_open { Variant::Toggle } else { Variant::GhostMuted })
                        .size(Size::IconXs)
                        .pressed(add_menu_open)
                        .icon(Icon::Plus)
                        .tooltip(add_tooltip)
                        .on_click(move |_, window, cx| on_toggle_add(window, cx)),
                ),
        )
        .child(header_controls_with_options(&state, !sheet_mode, cx))
        .into_any_element()
}

pub(super) fn render_add_menu(
    state: Entity<AppState>,
    entries: &[AddMenuEntry],
    highlighted: usize,
    focus: &gpui::FocusHandle,
    on_dismiss: impl Fn(bool, &mut App) + Clone + 'static,
    on_key_down: impl Fn(&gpui::KeyDownEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let dismiss_out = on_dismiss.clone();
    let rows = entries.iter().enumerate().map(|(index, entry)| {
        let state = state.clone();
        let open = entry.open;
        let dismiss = on_dismiss.clone();
        let active = index == highlighted && entry.unavailable.is_none();
        h_flex()
            .id(("right-panel-add-item", index))
            .h(px(30.))
            .w_full()
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(6.))
            .text_xs()
            .text_color(hsla(p.foreground))
            .when(active, |this| this.bg(hsla(p.accent)))
            .when(entry.unavailable.is_some(), |this| this.opacity(0.5))
            .when(entry.unavailable.is_none(), |this| {
                this.cursor_pointer()
                    .hover(|style| style.bg(hsla(p.accent)))
                    .on_click(move |_, _, cx| {
                        dismiss(false, cx);
                        state.update(cx, open);
                    })
            })
            .when_some(entry.unavailable, |this, reason| {
                this.tooltip(move |window, cx| super::tooltip_view(reason.into(), window, cx))
            })
            .child(entry.icon.svg().size(px(14.)).text_color(hsla(p.foreground)))
            .child(div().flex_1().min_w_0().truncate().child(entry.label))
            .child(kbd(entry.shortcut, cx))
    });
    let menu = crate::composer::controls::popup_surface(cx)
        .id("right-panel-add-menu")
        .track_focus(focus)
        .occlude()
        .w(px(216.))
        .p(px(4.))
        .on_mouse_down_out(move |_, _, cx| dismiss_out(true, cx))
        .on_key_down(move |event, window, cx| on_key_down(event, window, cx))
        .child(v_flex().gap(px(2.)).children(rows));
    gpui::deferred(
        div()
            .absolute()
            .top(px(TITLEBAR_HEIGHT))
            .left(px(8.))
            .child(menu),
    )
    .with_priority(50)
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn tab(
    state: Entity<AppState>,
    surface: &Surface,
    index: usize,
    count: usize,
    active: bool,
    rename_input: Option<&Entity<InputState>>,
    on_start_rename: impl Fn(String, String, &mut Window, &mut App) + Clone + 'static,
    on_cancel_rename: impl Fn(&mut App) + Clone + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let side_chat = surface.side_chat_thread().and_then(|id| state.read(cx).thread_shell(id));
    let title: SharedString = match &surface.kind {
        SurfaceKind::SideChat { .. } => side_chat.map_or_else(|| surface.title(), |thread| thread.title.clone()).into(),
        SurfaceKind::Preview { resource_id: Some(tab) } => {
            let previews = crate::preview::previews(cx);
            let thread_key = state.read(cx).panel_thread();
            let snapshot = previews.as_ref().zip(thread_key.as_deref()).and_then(|(previews, thread)| previews.read(cx).session(thread, tab));
            t3_core::preview::tab_title(snapshot).into()
        }
        SurfaceKind::Terminal { active_terminal_id, .. } => {
            let thread_key = state.read(cx).panel_thread().unwrap_or_default();
            crate::terminal::terminal_label(&thread_key, active_terminal_id, cx).into()
        }
        _ => surface.title().into(),
    };
    let is_device = matches!(surface.kind, SurfaceKind::Device { .. });
    // Static dot: a side chat tab can stay open for a whole run.
    let dot = side_chat.and_then(t3_core::side_chat::TabState::of).map(|tab_state| match tab_state {
        t3_core::side_chat::TabState::Running => hsla(p.muted_foreground),
        _ => hsla(p.info),
    });
    let id = surface.id.clone();
    let file_path = match &surface.kind {
        SurfaceKind::File { relative_path, attachment: None, .. } => Some(relative_path.clone()),
        _ => None,
    };
    let (activate, close, middle) = (state.clone(), state.clone(), state.clone());
    let (activate_id, close_id, middle_id) = (id.clone(), id.clone(), id.clone());
    let rename_id_click = id.clone();
    let rename_title_click = title.to_string();
    let on_rename_click = on_start_rename.clone();
    let rename_id_menu = id.clone();
    let rename_title_menu = title.to_string();
    let on_rename_menu = on_start_rename;
    let group: SharedString = format!("tab-{id}").into();
    let icon_color = hsla(if active { p.foreground } else { p.muted_foreground });
    let tab_id = ElementId::from(SharedString::from(format!("right-panel-tab-{id}")));
    let label_el = match rename_input {
        Some(input) => {
            let cancel_action = on_cancel_rename.clone();
            let cancel_key = on_cancel_rename;
            div()
                .w(px(96.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_action(move |_: &gpui_component::input::Escape, _, cx| {
                    cx.stop_propagation();
                    cancel_action(cx);
                })
                .on_key_down(move |event, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        cancel_key(cx);
                    }
                })
                .child(Input::new(input).xsmall())
                .into_any_element()
        }
        None => div().min_w_0().truncate().child(title.clone()).into_any_element(),
    };
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
        .on_click(move |event, window, cx| {
            if is_device && event.click_count() == 2 {
                on_rename_click(rename_id_click.clone(), rename_title_click.clone(), window, cx);
                return;
            }
            activate.update(cx, |state, cx| state.activate_surface(&activate_id, cx));
        })
        .on_mouse_up(MouseButton::Middle, move |_, window, cx| {
            middle.update(cx, |state, cx| state.close_surface_with_confirm(&middle_id, window, cx));
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
                    let label: SharedString = crate::i18n::t_fmt(cx, "Close {title}", &[("title", title.as_str().into())]).into();
                    move |window, cx| super::tooltip_view(label.clone(), window, cx)
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    close.update(cx, |state, cx| state.close_surface_with_confirm(&close_id, window, cx));
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
        .child(label_el)
        .context_menu(move |menu, _, cx| {
            let item = |label: &'static str, disabled: bool, run: fn(&mut AppState, &str, &mut Window, &mut gpui::Context<AppState>)| {
                let state = state.clone();
                let id = id.clone();
                PopupMenuItem::new(crate::i18n::t(cx, label))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| state.update(cx, |state, cx| run(state, &id, window, cx)))
            };
            let menu = match file_path.clone() {
                Some(path) => {
                    let state = state.clone();
                    menu.item(PopupMenuItem::new(crate::i18n::t(cx, "Copy path")).on_click(move |_, _, cx| state.update(cx, |state, cx| state.copy_file_path(&path, cx))))
                }
                None => menu,
            };
            let menu = if is_device {
                let on_rename = on_rename_menu.clone();
                let rename_id = rename_id_menu.clone();
                let rename_title = rename_title_menu.clone();
                menu.item(PopupMenuItem::new(crate::i18n::t(cx, "Rename")).on_click(move |_, window, cx| {
                    on_rename(rename_id.clone(), rename_title.clone(), window, cx);
                }))
            } else {
                menu
            };
            menu.item(item("Close", false, |state, id, window, cx| state.close_surface_with_confirm(id, window, cx)))
                .item(item("Close others", count <= 1, |state, id, window, cx| state.close_other_surfaces_with_confirm(id, window, cx)))
                .item(item("Close to the right", index + 1 >= count, |state, id, window, cx| state.close_surfaces_to_right_with_confirm(id, window, cx)))
                .item(item("Close all", false, |state, _, window, cx| state.close_all_surfaces_with_confirm(window, cx)))
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
        let reason = (action.unavailable)(state.read(cx), cx);
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
                this.cursor_pointer().hover(|style| style.bg(hsla(p.accent).opacity(0.6))).on_click(move |_, _, cx| state.update(cx, open))
            })
            .when_some(reason, |this, reason| this.tooltip(move |window, cx| super::tooltip_view(crate::i18n::t(cx, reason).into(), window, cx)))
            .child(action.icon.svg().size(px(16.)).text_color(hsla(p.foreground)))
            .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, action.label)))
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
                .when_some(reason, |this, reason| this.tooltip(move |window, cx| super::tooltip_view(crate::i18n::t(cx, reason).into(), window, cx)))
                .child(Icon::MessagesSquare.svg().size(px(16.)).text_color(hsla(p.foreground)))
                .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, "Side chat")))
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
                        .child(crate::i18n::t(cx, "Open a surface")),
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
