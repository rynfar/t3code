//! The tab bar (`RightPanelTab`, the add-surface menu, `PanelTabCloseButton`) and the empty
//! "Open a surface" launcher (`RightPanelEmptyState`) from `components/RightPanelTabs.tsx`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, Hsla, Image, ImageFormat, IntoElement,
    MouseButton, ParentElement, RenderOnce, SharedString, Styled, Window, WindowControlArea, div,
    img, prelude::*, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{Sizable, h_flex, v_flex};
use t3_core::device::Platform;
use t3_core::preview::viewport::{
    BrowserProfile, BrowserProfileKind, OpenPreviewOverrides, built_in_browser_profiles,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceTabIcon {
    Ios,
    Android,
    Generic,
}

fn device_tab_icon(surface: &Surface) -> DeviceTabIcon {
    match &surface.kind {
        SurfaceKind::Device { target: Some(target), .. } => match target.platform {
            Platform::Ios => DeviceTabIcon::Ios,
            Platform::Android => DeviceTabIcon::Android,
        },
        _ => DeviceTabIcon::Generic,
    }
}

/// Renders the exact CC0 `AppleIcon` / `AndroidIcon` SVG from `apps/web/src/components/Icons.tsx`
/// tinted to `color`, cached per `(Platform, rgba)` like `project_icon.rs`.
fn device_platform_svg(platform: Platform, hex: &str, alpha: f32) -> Vec<u8> {
    let opacity = format!("{:.3}", alpha.clamp(0.0, 1.0));
    match platform {
        Platform::Ios => format!(
            concat!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="{hex}" fill-opacity="{opacity}">"#,
                r#"<path d="M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701"/>"#,
                r#"</svg>"#,
            ),
            hex = hex,
            opacity = opacity,
        )
        .into_bytes(),
        Platform::Android => format!(
            concat!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="{hex}" fill-opacity="{opacity}">"#,
                r#"<path d="m8 3-1-2m9 2 1-2" fill="none" stroke="{hex}" stroke-opacity="{opacity}" stroke-width="1.4" stroke-linecap="round"/>"#,
                r#"<path fill-rule="evenodd" d="M5 10a7 7 0 0 1 14 0H5Zm4-4a.8.8 0 1 0 0 1.6A.8.8 0 0 0 9 6Zm6 0a.8.8 0 1 0 0 1.6A.8.8 0 0 0 15 6Z"/>"#,
                r#"<path d="M5 11h14v7a2 2 0 0 1-2 2v2a1.5 1.5 0 0 1-3 0v-2h-4v2a1.5 1.5 0 0 1-3 0v-2a2 2 0 0 1-2-2v-7Z"/>"#,
                r#"<rect x="1" y="11" width="3" height="8" rx="1.5"/>"#,
                r#"<rect x="20" y="11" width="3" height="8" rx="1.5"/>"#,
                r#"</svg>"#,
            ),
            hex = hex,
            opacity = opacity,
        )
        .into_bytes(),
    }
}

thread_local! {
    static DEVICE_ICON_CACHE: RefCell<HashMap<(Platform, u32), Arc<Image>>> = RefCell::new(HashMap::new());
}

fn device_platform_image(platform: Platform, color: Hsla) -> Arc<Image> {
    let rgba = color.to_rgb();
    let r = (rgba.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (rgba.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (rgba.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = (rgba.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    let key = (platform, u32::from_be_bytes([r, g, b, a]));
    DEVICE_ICON_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(existing) = cache.get(&key) {
            return existing.clone();
        }
        let hex = format!("#{r:02x}{g:02x}{b:02x}");
        let bytes = device_platform_svg(platform, &hex, rgba.a);
        let image = Arc::new(Image::from_bytes(ImageFormat::Svg, bytes));
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    })
}

/// Ordered profiles for the Browser launcher submenu: `Default`, configured named profiles from
/// `ClientSettingsDoc::browser_profiles()`, then `Incognito`.
fn browser_profiles_for_menu(cx: &App) -> Vec<BrowserProfile> {
    let profiles = crate::settings::store::try_client(cx)
        .map(t3_core::client_settings::ClientSettingsDoc::browser_profiles)
        .unwrap_or_else(built_in_browser_profiles);
    order_browser_profiles(profiles)
}

fn order_browser_profiles(profiles: Vec<BrowserProfile>) -> Vec<BrowserProfile> {
    let mut persistent = Vec::new();
    let mut incognito = Vec::new();
    for profile in profiles {
        if profile.kind == BrowserProfileKind::Incognito {
            incognito.push(profile);
        } else {
            persistent.push(profile);
        }
    }
    persistent.extend(incognito);
    persistent
}

fn open_browser_in_profile(
    state: &mut AppState,
    profile_id: String,
    cx: &mut gpui::Context<AppState>,
) {
    let Some(thread_id) = state.panel_thread() else { return };
    cx.defer(move |cx| {
        if let Some(previews) = crate::preview::previews(cx) {
            previews.update(cx, |previews, cx| {
                previews.open_session_with(
                    thread_id,
                    OpenPreviewOverrides {
                        profile_id: Some(profile_id),
                        ..Default::default()
                    },
                    false,
                    cx,
                );
            });
        }
    });
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

fn render_tab_surface_icon(surface: &Surface, icon_color: Hsla, group: SharedString) -> AnyElement {
    if matches!(surface.kind, SurfaceKind::Device { .. }) {
        match device_tab_icon(surface) {
            DeviceTabIcon::Ios => {
                return div()
                    .debug_selector(|| "device-tab-icon-ios".to_owned())
                    .absolute()
                    .top(px(2.))
                    .left(px(2.))
                    .size(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .group_hover(group, |style| style.invisible())
                    .child(img(device_platform_image(Platform::Ios, icon_color)).size(px(12.)))
                    .into_any_element();
            }
            DeviceTabIcon::Android => {
                return div()
                    .debug_selector(|| "device-tab-icon-android".to_owned())
                    .absolute()
                    .top(px(2.))
                    .left(px(2.))
                    .size(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .group_hover(group, |style| style.invisible())
                    .child(img(device_platform_image(Platform::Android, icon_color)).size(px(12.)))
                    .into_any_element();
            }
            DeviceTabIcon::Generic => {
                return div()
                    .debug_selector(|| "device-tab-icon-generic".to_owned())
                    .absolute()
                    .top(px(2.))
                    .left(px(2.))
                    .size(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .group_hover(group, |style| style.invisible())
                    .child(Icon::Smartphone.svg().size(px(12.)).text_color(icon_color))
                    .into_any_element();
            }
        }
    }
    surface_icon(surface)
        .svg()
        .absolute()
        .top(px(2.))
        .left(px(2.))
        .size(px(12.))
        .text_color(icon_color)
        .group_hover(group, |style| style.invisible())
        .into_any_element()
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

type DismissHandler = Rc<dyn Fn(bool, &mut App)>;

#[derive(IntoElement)]
struct BrowserAddMenuRow {
    index: usize,
    state: Entity<AppState>,
    label: &'static str,
    icon: Icon,
    shortcut: &'static str,
    open: fn(&mut AppState, &mut gpui::Context<AppState>),
    active: bool,
    profiles: Vec<BrowserProfile>,
    dismiss: DismissHandler,
}

impl RenderOnce for BrowserAddMenuRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let sub_open = window.use_keyed_state("right-panel-add-browser-sub", cx, |_, _| false);
        let is_open = *sub_open.read(cx);
        let hover_state = sub_open.clone();
        let chevron_state = sub_open.clone();
        let state = self.state.clone();
        let open = self.open;
        let dismiss = self.dismiss.clone();

        let submenu = is_open.then(|| {
            let items = self.profiles.iter().enumerate().map(|(pi, profile)| {
                let state = self.state.clone();
                let dismiss = self.dismiss.clone();
                let sub_close = sub_open.clone();
                let profile_id = profile.id.clone();
                let profile_name = profile.name.clone();
                let debug_id = format!("right-panel-browser-profile-{}", profile.id);
                h_flex()
                    .id(("right-panel-browser-profile", pi))
                    .debug_selector(move || debug_id.clone())
                    .h(px(28.))
                    .w_full()
                    .px(px(8.))
                    .gap(px(6.))
                    .rounded(px(6.))
                    .text_xs()
                    .text_color(hsla(p.foreground))
                    .cursor_pointer()
                    .hover(|style| style.bg(hsla(p.accent)))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        sub_close.update(cx, |open, _| *open = false);
                        dismiss(false, cx);
                        let id = profile_id.clone();
                        state.update(cx, move |state, cx| open_browser_in_profile(state, id, cx));
                    })
                    .child(div().flex_1().min_w_0().truncate().child(profile_name))
            });
            div()
                .absolute()
                .top_0()
                .left_full()
                .pl(px(4.))
                .child(
                    crate::composer::controls::popup_surface(cx)
                        .id("right-panel-browser-profiles-submenu")
                        .debug_selector(|| "right-panel-browser-profiles-submenu".to_owned())
                        .occlude()
                        .w(px(184.))
                        .p(px(4.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(v_flex().gap(px(2.)).children(items)),
                )
        });

        div()
            .id(("right-panel-add-browser-wrap", self.index))
            .relative()
            .w_full()
            .on_hover(move |hovered, _, cx| {
                hover_state.update(cx, |open, cx| {
                    if *open != *hovered {
                        *open = *hovered;
                        cx.notify();
                    }
                });
            })
            .child(
                h_flex()
                    .id(("right-panel-add-item", self.index))
                    .h(px(30.))
                    .w_full()
                    .px(px(8.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .text_xs()
                    .text_color(hsla(p.foreground))
                    .when(self.active || is_open, |this| this.bg(hsla(p.accent)))
                    .cursor_pointer()
                    .hover(|style| style.bg(hsla(p.accent)))
                    .on_click(move |_, _, cx| {
                        sub_open.update(cx, |open, _| *open = false);
                        dismiss(false, cx);
                        state.update(cx, open);
                    })
                    .child(self.icon.svg().size(px(14.)).text_color(hsla(p.foreground)))
                    .child(div().flex_1().min_w_0().truncate().child(self.label))
                    .child(kbd(self.shortcut, cx))
                    .child(
                        div()
                            .id("right-panel-add-browser-chevron")
                            .debug_selector(|| "right-panel-add-browser-chevron".to_owned())
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(4.))
                            .hover(|style| style.bg(hsla(p.muted)))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                chevron_state.update(cx, |open, cx| {
                                    *open = !*open;
                                    cx.notify();
                                });
                            })
                            .child(
                                Icon::ChevronRight
                                    .svg()
                                    .size(px(12.))
                                    .text_color(hsla(p.muted_foreground)),
                            ),
                    ),
            )
            .children(submenu)
    }
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
    let dismiss_rc: DismissHandler = Rc::new(on_dismiss);
    let browser_profiles = browser_profiles_for_menu(cx);
    let rows = entries.iter().enumerate().map(|(index, entry)| {
        let active = index == highlighted && entry.unavailable.is_none();
        if entry.label == "Browser" && entry.unavailable.is_none() {
            return BrowserAddMenuRow {
                index,
                state: state.clone(),
                label: entry.label,
                icon: entry.icon,
                shortcut: entry.shortcut,
                open: entry.open,
                active,
                profiles: browser_profiles.clone(),
                dismiss: dismiss_rc.clone(),
            }
            .into_any_element();
        }
        let state = state.clone();
        let open = entry.open;
        let dismiss = dismiss_rc.clone();
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
            .into_any_element()
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
                .child(render_tab_surface_icon(surface, icon_color, group.clone()))
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
    let browser_profiles = browser_profiles_for_menu(cx);
    let profile_menu_open = window.use_keyed_state("right-panel-empty-browser-profiles", cx, |_, _| false);
    let is_profile_menu_open = *profile_menu_open.read(cx);
    let mut rows: Vec<AnyElement> = SURFACE_ACTIONS.iter().enumerate().map(|(index, action)| {
        let state_click = state.clone();
        let open = action.open;
        let row_id = ElementId::from(("surface-launcher", index));
        let reason = (action.unavailable)(state.read(cx), cx);
        let has_profile_menu = action.label == "Browser" && reason.is_none() && browser_profiles.len() > 1;
        let row = focusable(h_flex().id(row_id.clone()), row_id, reason.is_none(), window, cx)
            .h(px(32.))
            .w_full()
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(8.))
            .text_sm()
            .text_color(hsla(p.foreground))
            .when(reason.is_some(), |this| this.opacity(0.64))
            .when(reason.is_none(), |this| {
                this.cursor_pointer().hover(|style| style.bg(hsla(p.accent).opacity(0.6))).on_click(move |_, _, cx| state_click.update(cx, open))
            })
            .when_some(reason, |this, reason| this.tooltip(move |window, cx| super::tooltip_view(crate::i18n::t(cx, reason).into(), window, cx)))
            .child(action.icon.svg().size(px(16.)).text_color(hsla(p.foreground)))
            .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, action.label)))
            .when(has_profile_menu, |this| {
                let toggle = profile_menu_open.clone();
                this.child(
                    div()
                        .id("empty-browser-profile-trigger")
                        .debug_selector(|| "empty-browser-profile-trigger".to_owned())
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(22.))
                        .rounded(px(6.))
                        .hover(|style| style.bg(hsla(p.accent)))
                        .tooltip(|window, cx| super::tooltip_view("Open browser in a profile".into(), window, cx))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            toggle.update(cx, |open, cx| {
                                *open = !*open;
                                cx.notify();
                            });
                        })
                        .child(Icon::ChevronDown.svg().size(px(14.)).text_color(hsla(p.muted_foreground))),
                )
            })
            .child(kbd(action.shortcut, cx));
        if has_profile_menu && is_profile_menu_open {
            let dismiss_out = profile_menu_open.clone();
            let items = browser_profiles.iter().enumerate().map(|(pi, profile)| {
                let state = state.clone();
                let close = profile_menu_open.clone();
                let profile_id = profile.id.clone();
                let profile_name = profile.name.clone();
                h_flex()
                    .id(("empty-browser-profile-item", pi))
                    .h(px(28.))
                    .w_full()
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_xs()
                    .text_color(hsla(p.foreground))
                    .cursor_pointer()
                    .hover(|style| style.bg(hsla(p.accent)))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        close.update(cx, |open, cx| {
                            *open = false;
                            cx.notify();
                        });
                        let id = profile_id.clone();
                        state.update(cx, move |state, cx| open_browser_in_profile(state, id, cx));
                    })
                    .child(div().flex_1().min_w_0().truncate().child(profile_name))
            });
            return div()
                .relative()
                .w_full()
                .child(row)
                .child(
                    gpui::deferred(
                        div()
                            .absolute()
                            .top_full()
                            .right_0()
                            .mt(px(4.))
                            .child(
                                crate::composer::controls::popup_surface(cx)
                                    .id("empty-browser-profiles-popup")
                                    .occlude()
                                    .w(px(196.))
                                    .p(px(4.))
                                    .on_mouse_down_out(move |_, _, cx| {
                                        dismiss_out.update(cx, |open, cx| {
                                            *open = false;
                                            cx.notify();
                                        });
                                    })
                                    .child(v_flex().gap(px(2.)).children(items)),
                            ),
                    )
                    .with_priority(50),
                )
                .into_any_element();
        }
        row.into_any_element()
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
                .child(kbd("S", cx))
                .into_any_element(),
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

#[cfg(test)]
mod unit_tests {
    use super::*;
    use t3_core::right_panel::DeviceTarget;

    #[test]
    fn device_tab_icon_distinguishes_ios_android_and_generic() {
        let ios = Surface::device(Some(DeviceTarget {
            host_id: "h".into(),
            device_id: "sim".into(),
            platform: Platform::Ios,
            name: "iPhone 16".into(),
        }));
        let android = Surface::device(Some(DeviceTarget {
            host_id: "h".into(),
            device_id: "emu".into(),
            platform: Platform::Android,
            name: "Pixel 9".into(),
        }));
        let generic = Surface::device(None);

        assert_eq!(device_tab_icon(&ios), DeviceTabIcon::Ios);
        assert_eq!(device_tab_icon(&android), DeviceTabIcon::Android);
        assert_eq!(device_tab_icon(&generic), DeviceTabIcon::Generic);

        let ios_svg = String::from_utf8(device_platform_svg(Platform::Ios, "#ffffff", 1.0)).unwrap();
        let android_svg = String::from_utf8(device_platform_svg(Platform::Android, "#ffffff", 1.0)).unwrap();
        assert!(ios_svg.contains("<svg") && ios_svg.contains("M12.152"));
        assert!(android_svg.contains("<svg") && android_svg.contains("m8 3-1-2"));
    }

    #[test]
    fn browser_profile_menu_orders_default_named_then_incognito() {
        let ordered = order_browser_profiles(vec![
            BrowserProfile { id: "default".into(), name: "Default".into(), kind: BrowserProfileKind::Persistent },
            BrowserProfile { id: "incognito".into(), name: "Incognito".into(), kind: BrowserProfileKind::Incognito },
            BrowserProfile { id: "work".into(), name: "Work".into(), kind: BrowserProfileKind::Persistent },
        ]);
        let ids: Vec<&str> = ordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "work", "incognito"]);
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod snapshot_tests {
    use super::*;
    use gpui::{Context, Render};
    use serde_json::json;
    use t3_core::client_settings::ClientSettingChange;
    use t3_core::right_panel::DeviceTarget;

    #[gpui::test]
    async fn browser_profile_submenu_and_device_platform_tab_icons_render_and_dispatch(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "preview:operate"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::preview::init(state.clone(), cx);
            state.update(cx, |s, cx| {
                s.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "env-1",
                            "label": "Mac",
                            "capabilities": { "serverBrowser": true }
                        }
                    }))
                    .unwrap(),
                );
                cx.emit(crate::app_state::AppEvent::ServerConfig);
                s.mutate_panels(cx, |panels, thread| {
                    let mut changed = panels.panels.open_device(
                        thread,
                        DeviceTarget {
                            host_id: "h".into(),
                            device_id: "ios-1".into(),
                            platform: Platform::Ios,
                            name: "iPhone 16".into(),
                        },
                    );
                    changed |= panels.panels.open_device(
                        thread,
                        DeviceTarget {
                            host_id: "h".into(),
                            device_id: "and-1".into(),
                            platform: Platform::Android,
                            name: "Pixel 9".into(),
                        },
                    );
                    changed |= panels.panels.open(thread, Surface::device(None));
                    changed
                });
            });
            crate::settings::store::update_client(
                &state,
                ClientSettingChange::BrowserProfiles(vec![BrowserProfile {
                    id: "work".into(),
                    name: "Work".into(),
                    kind: BrowserProfileKind::Persistent,
                }]),
                cx,
            );
        });
        // Drain any initial preview.reportProfiles / preview.list requests.
        cx.run_until_parked();

        struct TabsHarness {
            state: Entity<AppState>,
            focus: gpui::FocusHandle,
        }
        impl Render for TabsHarness {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let panel = {
                    let s = self.state.read(cx);
                    s.panel_thread()
                        .as_deref()
                        .map(|t| s.panels.panels.panel(t).clone())
                        .unwrap_or_default()
                };
                let entries = add_menu_entries(self.state.read(cx), cx);
                div()
                    .relative()
                    .size_full()
                    .child(tab_bar(
                        self.state.clone(),
                        &panel,
                        false,
                        false,
                        true,
                        None,
                        |_, _| {},
                        |_, _, _, _| {},
                        |_| {},
                        window,
                        cx,
                    ))
                    .child(render_add_menu(
                        self.state.clone(),
                        &entries,
                        0,
                        &self.focus,
                        |_, _| {},
                        |_, _, _| {},
                        cx,
                    ))
            }
        }

        let window = cx.add_window(|_, cx| TabsHarness {
            state: state.clone(),
            focus: cx.focus_handle(),
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        // Distinct Device platform tab icons render for iOS, Android, and Generic.
        assert!(vcx.debug_bounds("device-tab-icon-ios").is_some());
        assert!(vcx.debug_bounds("device-tab-icon-android").is_some());
        assert!(vcx.debug_bounds("device-tab-icon-generic").is_some());

        // Clicking Browser submenu chevron reveals Default, Work, and Incognito profiles.
        let chevron = vcx
            .debug_bounds("right-panel-add-browser-chevron")
            .expect("browser profile submenu chevron")
            .center();
        vcx.simulate_click(chevron, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("right-panel-browser-profile-default").is_some());
        assert!(vcx.debug_bounds("right-panel-browser-profile-work").is_some());
        assert!(vcx.debug_bounds("right-panel-browser-profile-incognito").is_some());

        // Clicking the `work` profile dispatches `preview.open` with `profileId: "work"`.
        let work_center = vcx
            .debug_bounds("right-panel-browser-profile-work")
            .unwrap()
            .center();
        vcx.simulate_click(work_center, Default::default());
        vcx.run_until_parked();

        let mut saw_work_open = false;
        for _ in 0..8 {
            let (method, params, reply) = fixture.next_request().await;
            let _ = reply.send(Err(t3_client::session::RpcError::Closed));
            if method == t3_core::preview::method::OPEN {
                assert_eq!(params["profileId"], "work");
                saw_work_open = true;
                break;
            }
        }
        assert!(saw_work_open, "clicking Work profile must dispatch preview.open with profileId=work");

        state.update(&mut vcx, |s, _| s.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }
}
