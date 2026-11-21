//! The tab bar (`RightPanelTab`, the add-surface menu, `PanelTabCloseButton`) and the empty
//! "Open a surface" launcher (`RightPanelEmptyState`) from `components/RightPanelTabs.tsx`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ElementId, EmptyView, Entity, FontWeight, Hsla, Image, ImageFormat, IntoElement,
    MouseButton, ParentElement, RenderOnce, ScrollHandle, SharedString, Styled, Task, Window, WindowControlArea, div,
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
use crate::theme::{ThemeSlot, ThemeSlotExt as _, hsla, palette};
use crate::workspace::{TITLEBAR_HEIGHT, traffic_light_inset};

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

fn pull_request_unavailable(state: &AppState, cx: &App) -> Option<&'static str> {
    server_thread_unavailable(state, cx).or_else(|| {
        super::launcher_pull_request_target(state, cx).is_none().then_some("No pull request on this branch yet.")
    })
}

fn open_pull_request(state: &mut AppState, cx: &mut gpui::Context<AppState>) {
    if let Some(target) = super::launcher_pull_request_target(state, cx) {
        state.open_pull_request_surface(target, cx);
    }
}

const SURFACE_ACTIONS: &[SurfaceAction] = &[
    SurfaceAction { label: "Browser", icon: Icon::Earth, shortcut: "B", open: crate::preview::add_browser_surface, unavailable: browser_unavailable },
    SurfaceAction { label: "Terminal", icon: Icon::SquareTerminal, shortcut: "T", open: AppState::add_terminal_surface, unavailable: terminal_unavailable },
    SurfaceAction { label: "Files", icon: Icon::Files, shortcut: "F", open: AppState::add_files_surface, unavailable: files_unavailable },
    SurfaceAction { label: "Diff", icon: Icon::FileDiff, shortcut: "D", open: AppState::add_diff_surface, unavailable: server_thread_unavailable },
    SurfaceAction { label: "Pull request", icon: Icon::GitPullRequest, shortcut: "P", open: open_pull_request, unavailable: pull_request_unavailable },
    SurfaceAction { label: "Linked items", icon: Icon::Link2, shortcut: "L", open: AppState::add_pull_requests_surface, unavailable: server_thread_unavailable },
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
    let label = if label.eq_ignore_ascii_case("Linked pull requests") { "Linked items" } else { label };
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

#[derive(Default)]
struct DirtyTabs(HashMap<(gpui::EntityId, Option<u64>, String, String), bool>);
impl gpui::Global for DirtyTabs {}

#[allow(dead_code, reason = "The file editor publishes dirty state through this hook at integration")]
pub(crate) fn publish_file_tab_dirty(state: &Entity<AppState>, path: Option<&str>, dirty: bool, cx: &mut App) {
    let app = state.read(cx);
    let Some((thread, path)) = app.panel_thread().zip(path) else { return };
    let scope = (state.entity_id(), app.connection_id(), thread, path.to_owned());
    let tabs = &mut cx.default_global::<DirtyTabs>().0;
    let changed = if dirty { tabs.insert(scope, true).is_none() } else { tabs.remove(&scope).is_some() };
    if changed {
        let state = state.clone();
        cx.defer(move |cx| state.update(cx, |_, cx| cx.emit(crate::app_state::AppEvent::RightPanel)));
    }
}

fn browser_favicon_url(raw_url: &str) -> Option<SharedString> {
    let url = (raw_url.len() <= 4096).then(|| gpui::http_client::Url::parse(raw_url).ok()).flatten()?;
    let host = url.domain()?.strip_suffix('.').unwrap_or(url.domain()?);
    let (domain, tld) = host.rsplit_once('.')?;
    if !matches!(url.scheme(), "https" | "http")
        || domain.is_empty()
        || !tld.bytes().all(|byte| byte.is_ascii_alphabetic())
        || matches!(tld, "alt" | "corp" | "example" | "home" | "internal" | "intranet" | "invalid" | "lan" | "local" | "localhost" | "localdomain" | "onion" | "private" | "test")
        || host.ends_with(".ts.net")
        || host.split('.').any(|label| label.is_empty() || matches!(label, "corp" | "internal" | "intranet" | "localhost"))
    {
        return None;
    }
    Some(format!("https://www.google.com/s2/favicons?domain={host}&sz=32").into())
}

struct BrowserFavicon {
    image: Option<Arc<Image>>,
    _load: Task<()>,
}

fn load_browser_favicon(
    url: SharedString,
    surface: &Surface,
    state: &Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<Image>> {
    const PROVIDER: &str = "https://www.google.com";
    let path = url.strip_prefix(PROVIDER)?.to_owned();
    let runtime = state.read(cx).runtime().clone();
    let key = ElementId::Name(format!("browser-tab-favicon:{}:{path}", surface.id).into());
    window.use_keyed_state::<BrowserFavicon>(key, cx, |_, cx| BrowserFavicon {
        image: None,
        _load: cx.spawn(async move |this, cx| {
            let result = runtime.spawn(async move {
                let bytes = t3_client::asset::fetch_asset(PROVIDER, &path, 64 * 1024).await.ok()?;
                let decoder = image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png);
                let (width, height) = decoder.into_dimensions().ok()?;
                (width <= 128 && height <= 128).then(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
            }).await;
            if let Ok(Some(image)) = result {
                let _ = this.update(cx, |load, cx| {
                    load.image = Some(image);
                    cx.notify();
                });
            }
        }),
    }).read(cx).image.clone()
}

fn render_tab_surface_icon(surface: &Surface, favicon: Option<Arc<Image>>, icon_color: Hsla, group: SharedString) -> AnyElement {
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
    if let Some(favicon) = favicon {
        return div()
            .absolute().top(px(2.)).left(px(2.)).size(px(12.))
            .group_hover(group, |style| style.invisible())
            .child(img(favicon).size(px(12.)))
            .into_any_element();
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

#[derive(Default)]
struct TabScroll {
    handle: ScrollHandle,
    active: RefCell<Option<String>>,
    edges: Cell<(bool, bool)>,
}

#[derive(Clone)]
struct DraggedTab {
    thread_id: String,
    surface_id: String,
}

fn scroll_tabs(handle: &ScrollHandle, direction: f32, window: &mut Window) {
    let mut offset = handle.offset();
    offset.x = (offset.x + handle.bounds().size.width * 0.75 * direction)
        .clamp(-handle.max_offset().x, px(0.));
    handle.set_offset(offset);
    window.refresh();
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
    let scroll = window.use_keyed_state("right-panel-tab-scroll", cx, |_, _| TabScroll::default());
    let strip = scroll.read(cx);
    let active_index = panel.active_surface_id.as_ref().and_then(|id| panel.surfaces.iter().position(|tab| &tab.id == id));
    if *strip.active.borrow() != panel.active_surface_id {
        *strip.active.borrow_mut() = panel.active_surface_id.clone();
        if let Some(index) = active_index {
            strip.handle.scroll_to_item(index);
        }
    }
    let handle = strip.handle.clone();
    let left_handle = handle.clone();
    let right_handle = handle.clone();
    let edge_handle = handle.clone();
    let (can_scroll_left, can_scroll_right) = strip.edges.get();
    let overflow = can_scroll_left || can_scroll_right;
    h_flex()
        .h(px(TITLEBAR_HEIGHT))
        .flex_none()
        .pl(if titlebar_inset { traffic_light_inset(window, true) } else { px(8.) })
        .pr(px(12.))
        .gap(px(4.))
        .window_control_area(WindowControlArea::Drag)
        .child(
            h_flex()
                .on_children_prepainted(move |_, window, cx| {
                    let offset = edge_handle.offset().x;
                    let max = edge_handle.max_offset().x;
                    let next = if max > px(if overflow { 57. } else { 1. }) {
                        (offset < -px(1.), offset > -max + px(1.))
                    } else {
                        (false, false)
                    };
                    let strip = scroll.read(cx);
                    let previous = strip.edges.replace(next);
                    if previous != next {
                        if !previous.0 && !previous.1 && let Some(index) = active_index {
                            edge_handle.scroll_to_item(index);
                        }
                        window.request_animation_frame();
                    }
                })
                .id("right-panel-tab-strip")
                .flex_shrink(1.)
                .min_w_0()
                .gap(px(4.))
                .overflow_x_scroll()
                .track_scroll(&handle)
                .children(tabs),
        )
        .when(overflow, |bar| {
            bar.child(
                Button::new("right-panel-tabs-left")
                    .variant(Variant::GhostMuted).size(Size::IconXs)
                    .icon(Icon::ChevronLeft).tooltip(crate::i18n::t(cx, "Scroll tabs left"))
                    .disabled(!can_scroll_left)
                    .on_click(move |_, window, _| scroll_tabs(&left_handle, 1., window)),
            ).child(
                Button::new("right-panel-tabs-right")
                    .variant(Variant::GhostMuted).size(Size::IconXs)
                    .icon(Icon::ChevronRight).tooltip(crate::i18n::t(cx, "Scroll tabs right"))
                    .disabled(!can_scroll_right)
                    .on_click(move |_, window, _| scroll_tabs(&right_handle, -1., window)),
            )
        })
        .child(
            Button::new("right-panel-add")
                .variant(if add_menu_open { Variant::Toggle } else { Variant::GhostMuted })
                .size(Size::IconXs)
                .pressed(add_menu_open)
                .icon(Icon::Plus)
                .tooltip(add_tooltip)
                .on_click(move |_, window, cx| on_toggle_add(window, cx)),
        )
        .child(div().flex_1().min_w_0())
        .child(header_controls_with_options(&state, !sheet_mode, cx))
        .into_any_element()
}

type DismissHandler = Rc<dyn Fn(bool, &mut App)>;
type KeyDownHandler = Rc<dyn Fn(&gpui::KeyDownEvent, &mut Window, &mut App)>;

const EMPTY_PROFILE_REOPEN_GUARD: Duration = Duration::from_millis(250);

#[derive(Default)]
struct EmptyProfileMenuState {
    open: bool,
    dismissed_at: Option<Instant>,
}

struct OwnedAddMenuEntry {
    label: &'static str,
    icon: Icon,
    shortcut: &'static str,
    open: fn(&mut AppState, &mut gpui::Context<AppState>),
    unavailable: Option<&'static str>,
}

#[derive(IntoElement)]
struct AddMenuPopup {
    state: Entity<AppState>,
    entries: Vec<OwnedAddMenuEntry>,
    highlighted: usize,
    focus: gpui::FocusHandle,
    profiles: Vec<BrowserProfile>,
    on_dismiss: DismissHandler,
    on_key_down: KeyDownHandler,
}

impl RenderOnce for AddMenuPopup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let sub_open = window.use_keyed_state("right-panel-add-browser-sub", cx, |_, _| false);
        let is_sub_open = *sub_open.read(cx);
        let dismiss_out = self.on_dismiss.clone();
        let sub_dismiss_out = sub_open.clone();
        let on_key = self.on_key_down.clone();
        let sub_key = sub_open.clone();

        let rows = self.entries.iter().enumerate().map(|(index, entry)| {
            let active = index == self.highlighted && entry.unavailable.is_none();
            if entry.label == "Browser" && entry.unavailable.is_none() {
                let state = self.state.clone();
                let open = entry.open;
                let dismiss = self.on_dismiss.clone();
                let sub_row_click = sub_open.clone();
                let sub_chevron = sub_open.clone();
                let sub_hover = sub_open.clone();
                let profiles_list = is_sub_open.then(|| {
                    let items = self.profiles.iter().enumerate().map(|(pi, profile)| {
                        let state = self.state.clone();
                        let dismiss = self.on_dismiss.clone();
                        let sub_close = sub_open.clone();
                        let profile_id = profile.id.clone();
                        let profile_name = profile.name.clone();
                        let debug_id = format!("right-panel-browser-profile-{}", profile.id);
                        h_flex()
                            .id(("right-panel-browser-profile", pi))
                            .debug_selector(move || debug_id.clone())
                            .h(px(26.))
                            .w_full()
                            .px(px(8.))
                            .rounded(px(5.))
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
                    // Rendered inside `right-panel-add-menu`'s hitbox so clicks never trip
                    // Capture-phase `on_mouse_down_out`, and constrained within the 216px menu
                    // width so it never clips inside 360px narrow panels.
                    v_flex()
                        .id("right-panel-browser-profiles-submenu")
                        .debug_selector(|| "right-panel-browser-profiles-submenu".to_owned())
                        .ml(px(14.))
                        .mt(px(2.))
                        .mb(px(2.))
                        .pl(px(6.))
                        .border_l_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .gap(px(1.))
                        .children(items)
                });
                return v_flex()
                    .id(("right-panel-add-browser-wrap", index))
                    .w_full()
                    .on_hover(move |hovered, _, cx| {
                        if *hovered {
                            sub_hover.update(cx, |open, cx| {
                                if !*open {
                                    *open = true;
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .child(
                        h_flex()
                            .id(("right-panel-add-item", index))
                            .h(px(30.))
                            .w_full()
                            .px(px(8.))
                            .gap(px(8.))
                            .rounded(px(6.))
                            .text_xs()
                            .text_color(hsla(p.foreground))
                            .when(active || is_sub_open, |this| this.bg(hsla(p.accent)))
                            .cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent)))
                            .on_click(move |_, _, cx| {
                                sub_row_click.update(cx, |open, _| *open = false);
                                dismiss(false, cx);
                                state.update(cx, open);
                            })
                            .child(entry.icon.svg().size(px(14.)).text_color(hsla(p.foreground)))
                            .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, entry.label)))
                            .child(kbd(entry.shortcut, cx))
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
                                        sub_chevron.update(cx, |open, cx| {
                                            *open = !*open;
                                            cx.notify();
                                        });
                                    })
                                    .child(
                                        if is_sub_open {
                                            Icon::ChevronDown
                                        } else {
                                            Icon::ChevronRight
                                        }
                                        .svg()
                                        .size(px(12.))
                                        .text_color(hsla(p.muted_foreground)),
                                    ),
                            ),
                    )
                    .children(profiles_list)
                    .into_any_element();
            }
            let state = self.state.clone();
            let open = entry.open;
            let dismiss = self.on_dismiss.clone();
            let sub_reset = sub_open.clone();
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
                            sub_reset.update(cx, |open, _| *open = false);
                            dismiss(false, cx);
                            state.update(cx, open);
                        })
                })
                .when_some(entry.unavailable, |this, reason| {
                    this.tooltip(move |window, cx| super::tooltip_view(crate::i18n::t(cx, reason).into(), window, cx))
                })
                .child(entry.icon.svg().size(px(14.)).text_color(hsla(p.foreground)))
                .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, entry.label)))
                .child(kbd(entry.shortcut, cx))
                .into_any_element()
        });

        crate::composer::controls::popup_surface(cx)
            .id("right-panel-add-menu")
            .track_focus(&self.focus)
            .occlude()
            .w(px(216.))
            .p(px(4.))
            .on_mouse_down_out(move |_, _, cx| {
                sub_dismiss_out.update(cx, |open, _| *open = false);
                dismiss_out(true, cx);
            })
            .on_key_down(move |event, window, cx| {
                if !matches!(event.keystroke.key.as_str(), "up" | "down") {
                    sub_key.update(cx, |open, _| *open = false);
                }
                on_key(event, window, cx);
            })
            .child(v_flex().gap(px(2.)).children(rows))
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
    let menu = AddMenuPopup {
        state,
        entries: entries
            .iter()
            .map(|entry| OwnedAddMenuEntry {
                label: entry.label,
                icon: entry.icon,
                shortcut: entry.shortcut,
                open: entry.open,
                unavailable: entry.unavailable,
            })
            .collect(),
        highlighted,
        focus: focus.clone(),
        profiles: browser_profiles_for_menu(cx),
        on_dismiss: Rc::new(on_dismiss),
        on_key_down: Rc::new(on_key_down),
    };
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
    let mut favicon = None;
    let title: SharedString = match &surface.kind {
        SurfaceKind::SideChat { .. } => side_chat.map_or_else(|| surface.title(), |thread| thread.title.clone()).into(),
        SurfaceKind::Preview { resource_id: Some(tab) } => {
            let previews = crate::preview::previews(cx);
            let thread_key = state.read(cx).panel_thread();
            let snapshot = previews.as_ref().zip(thread_key.as_deref()).and_then(|(previews, thread)| previews.read(cx).session(thread, tab));
            favicon = snapshot.and_then(|snapshot| snapshot.nav_status.url()).and_then(browser_favicon_url);
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
    let favicon = favicon.and_then(|url| load_browser_favicon(url, surface, &state, window, cx));
    let id = surface.id.clone();
    let file_path = match &surface.kind {
        SurfaceKind::File { relative_path, attachment: None, .. } => Some(relative_path.clone()),
        _ => None,
    };
    let dirty = file_path.as_deref().is_some_and(|path| {
        let app = state.read(cx);
        app.panel_thread().is_some_and(|thread| {
            cx.try_global::<DirtyTabs>().is_some_and(|tabs| {
                tabs.0.contains_key(&(state.entity_id(), app.connection_id(), thread, path.to_owned()))
            })
        })
    });
    let (activate, close, middle) = (state.clone(), state.clone(), state.clone());
    let (activate_id, close_id, middle_id) = (id.clone(), id.clone(), id.clone());
    let rename_id_click = id.clone();
    let rename_title_click = title.to_string();
    let on_rename_click = on_start_rename.clone();
    let rename_id_menu = id.clone();
    let rename_title_menu = title.to_string();
    let on_rename_menu = on_start_rename;
    let group: SharedString = format!("tab-{id}").into();
    let tip = title.clone();
    let drag = state.read(cx).panel_thread().map(|thread_id| DraggedTab { thread_id, surface_id: id.clone() });
    let drop_state = state.clone();
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
                .child(Input::new(input).xsmall().theme_slot(ThemeSlot::Input, cx))
                .into_any_element()
        }
        None => div().min_w_0().truncate().child(title.clone()).into_any_element(),
    };
    focusable(div().id(tab_id.clone()), tab_id, true, window, cx)
        .group(group.clone())
        .tooltip(move |window, cx| super::tooltip_view(tip.clone(), window, cx))
        .when_some(drag.filter(|_| count > 1 && rename_input.is_none()), |tab, drag| {
            tab.on_drag(drag, |_, _, _, cx| cx.new(|_| EmptyView))
        })
        .on_drop(move |drag: &DraggedTab, _, cx| {
            drop_state.update(cx, |state, cx| {
                state.mutate_panels(cx, |panels, thread| {
                    thread == drag.thread_id && panels.panels.move_surface(thread, &drag.surface_id, index)
                });
            });
        })
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
                .child(render_tab_surface_icon(surface, favicon, icon_color, group.clone()))
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
        .when(dirty, |tab| tab.child(div().size(px(6.)).flex_none().rounded_full().bg(hsla(p.muted_foreground))))
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
    let profile_menu_state =
        window.use_keyed_state("right-panel-empty-browser-profiles", cx, |_, _| EmptyProfileMenuState::default());
    let is_profile_menu_open = profile_menu_state.read(cx).open;
    let focus = window.use_keyed_state("right-panel-launcher-focus", cx, |window, cx| {
        let focus = cx.focus_handle().tab_stop(true);
        let initial = focus.clone();
        window.on_next_frame(move |window, cx| window.focus(&initial, cx));
        focus
    }).read(cx).clone();
    let selection = window.use_keyed_state("right-panel-launcher-selection", cx, |_, _| Cell::new(0_usize));
    let entries = add_menu_entries(state.read(cx), cx);
    let enabled = entries.iter().enumerate().filter_map(|(index, entry)| entry.unavailable.is_none().then_some(index)).collect::<Vec<_>>();
    let selected = enabled.get(selection.read(cx).get().min(enabled.len().saturating_sub(1))).copied();
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
            .when(selected == Some(index) && focus.is_focused(window), |row| row.bg(hsla(p.accent)))
            .when(reason.is_some(), |this| this.opacity(0.64))
            .when(reason.is_none(), |this| {
                this.cursor_pointer().hover(|style| style.bg(hsla(p.accent).opacity(0.6))).on_click(move |_, _, cx| state_click.update(cx, open))
            })
            .when_some(reason, |this, reason| this.tooltip(move |window, cx| super::tooltip_view(crate::i18n::t(cx, reason).into(), window, cx)))
            .child(action.icon.svg().size(px(16.)).text_color(hsla(p.foreground)))
            .child(div().flex_1().min_w_0().truncate().child(crate::i18n::t(cx, action.label)))
            .when(has_profile_menu, |this| {
                let toggle = profile_menu_state.clone();
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
                        .tooltip(|window, cx| {
                            super::tooltip_view(crate::i18n::t(cx, "Open browser in a profile").into(), window, cx)
                        })
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            toggle.update(cx, |menu, cx| {
                                let just_dismissed = menu
                                    .dismissed_at
                                    .take()
                                    .is_some_and(|at| at.elapsed() < EMPTY_PROFILE_REOPEN_GUARD);
                                menu.open = !just_dismissed && !menu.open;
                                cx.notify();
                            });
                        })
                        .child(Icon::ChevronDown.svg().size(px(14.)).text_color(hsla(p.muted_foreground))),
                )
            })
            .child(kbd(action.shortcut, cx));
        if has_profile_menu && is_profile_menu_open {
            let dismiss_out = profile_menu_state.clone();
            let items = browser_profiles.iter().enumerate().map(|(pi, profile)| {
                let state = state.clone();
                let close = profile_menu_state.clone();
                let profile_id = profile.id.clone();
                let profile_name = profile.name.clone();
                let debug_id = format!("empty-browser-profile-{}", profile.id);
                h_flex()
                    .id(("empty-browser-profile-item", pi))
                    .debug_selector(move || debug_id.clone())
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
                        close.update(cx, |menu, cx| {
                            menu.open = false;
                            menu.dismissed_at = None;
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
                                    .debug_selector(|| "empty-browser-profiles-popup".to_owned())
                                    .occlude()
                                    .w(px(196.))
                                    .p(px(4.))
                                    .on_mouse_down_out(move |_, _, cx| {
                                        dismiss_out.update(cx, |menu, cx| {
                                            if menu.open {
                                                menu.open = false;
                                                menu.dismissed_at = Some(Instant::now());
                                                cx.notify();
                                            }
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
                .when(selected == Some(SURFACE_ACTIONS.len()) && focus.is_focused(window), |row| row.bg(hsla(p.accent)))
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
    let key_focus = focus.clone();
    div()
        .id("right-panel-launcher")
        .track_focus(&focus)
        .aria_label(crate::i18n::t(cx, "Open a surface"))
        .on_key_down(move |event, window, cx| {
            let key = &event.keystroke;
            if is_profile_menu_open || enabled.is_empty()
                || key.modifiers.control || key.modifiers.alt || key.modifiers.platform
            {
                return;
            }
            let index = selection.read(cx).get().min(enabled.len() - 1);
            match key.key.as_str() {
                "down" | "right" | "up" | "left" => {
                    let next = if key.key == "down" || key.key == "right" {
                        (index + 1) % enabled.len()
                    } else {
                        (index + enabled.len() - 1) % enabled.len()
                    };
                    selection.read(cx).set(next);
                    window.focus(&key_focus, cx);
                    window.refresh();
                }
                "enter" if key_focus.is_focused(window) => {
                    state.update(cx, entries[enabled[index]].open);
                }
                key => {
                    let Some(entry) = enabled.iter().map(|index| &entries[*index]).find(|entry| entry.shortcut.eq_ignore_ascii_case(key)) else { return };
                    if !event.is_held {
                        state.update(cx, entry.open);
                    }
                }
            }
            cx.stop_propagation();
        })
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
    fn browser_favicon_sends_only_public_dns_hosts() {
        assert_eq!(
            browser_favicon_url("https://github.com:8443/private?q=secret").unwrap().as_ref(),
            "https://www.google.com/s2/favicons?domain=github.com&sz=32",
        );
        for url in ["http://127.0.0.1:3000/", "http://portal.corp.com/", "https://box.example.ts.net/", "http://[::1]/"] {
            assert!(browser_favicon_url(url).is_none(), "{url}");
        }
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    fn panel_chrome_launcher_opens_surfaces_with_shortcuts_and_arrows(cx: &mut gpui::TestAppContext) {
        use gpui::Render;

        struct Launcher(Entity<AppState>);
        impl Render for Launcher {
            fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
                empty_state(self.0.clone(), window, cx)
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        state.update(cx, |state, cx| {
            let thread = state.shell.snapshot.as_mut().unwrap().threads.iter_mut().find(|thread| thread.id == "menu-thread").unwrap();
            thread.pull_requests.push(serde_json::from_value(serde_json::json!({
                "host": "github.com", "repository": "fixture/repo", "number": 17, "url": "https://github.com/fixture/repo/pull/17",
                "source": "manual", "linkedAt": "2026-01-01T00:00:00Z", "snapshot": null, "stack": null
            })).unwrap());
            state.select_thread("menu-thread", cx);
        });
        let (_, vcx) = cx.add_window_view(|_, _| Launcher(state.clone()));
        vcx.refresh().unwrap();
        vcx.update(|window, cx| { window.simulate_next_frame(cx); });
        vcx.refresh().unwrap();
        vcx.simulate_keystrokes("f");
        state.read_with(vcx, |state, _| {
            assert!(matches!(state.panels.panels.active_surface("menu-thread").unwrap().kind, SurfaceKind::Files { .. }));
        });

        vcx.simulate_keystrokes("down down enter");
        state.read_with(vcx, |state, _| {
            assert!(matches!(state.panels.panels.active_surface("menu-thread").unwrap().kind, SurfaceKind::Diff));
        });
        vcx.simulate_keystrokes("p");
        state.read_with(vcx, |state, _| {
            assert!(matches!(state.panels.panels.active_surface("menu-thread").unwrap().kind, SurfaceKind::PullRequest { .. }));
        });
        vcx.simulate_keystrokes("l");
        state.read_with(vcx, |state, _| {
            assert!(matches!(state.panels.panels.active_surface("menu-thread").unwrap().kind, SurfaceKind::PullRequests));
        });
        state.update(vcx, |state, _| state.stop_pump());
    }

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
            add_menu_open: bool,
            show_empty: bool,
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
                let weak_dismiss = cx.entity().downgrade();
                let weak_key = cx.entity().downgrade();
                div()
                    .relative()
                    .w(px(360.))
                    .h(px(480.))
                    .flex()
                    .flex_col()
                    .child(tab_bar(
                        self.state.clone(),
                        &panel,
                        false,
                        false,
                        self.add_menu_open,
                        None,
                        |_, _| {},
                        |_, _, _, _| {},
                        |_| {},
                        window,
                        cx,
                    ))
                    .when(self.show_empty, |this| {
                        this.child(empty_state(self.state.clone(), window, cx))
                    })
                    .when(self.add_menu_open, |this| {
                        this.child(render_add_menu(
                            self.state.clone(),
                            &entries,
                            0,
                            &self.focus,
                            move |_, cx| {
                                let _ = weak_dismiss.update(cx, |this, cx| {
                                    this.add_menu_open = false;
                                    cx.notify();
                                });
                            },
                            move |event, _, cx| {
                                if event.keystroke.key == "escape" {
                                    let _ = weak_key.update(cx, |this, cx| {
                                        this.add_menu_open = false;
                                        cx.notify();
                                    });
                                }
                            },
                            cx,
                        ))
                    })
            }
        }

        let window = cx.add_window(|_, cx| TabsHarness {
            state: state.clone(),
            focus: cx.focus_handle(),
            add_menu_open: true,
            show_empty: false,
        });
        let harness = window.entity(cx).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        // Distinct Device platform tab icons render for iOS, Android, and Generic.
        assert!(vcx.debug_bounds("device-tab-icon-ios").is_some());
        assert!(vcx.debug_bounds("device-tab-icon-android").is_some());
        assert!(vcx.debug_bounds("device-tab-icon-generic").is_some());

        // Clicking Browser submenu chevron reveals Default, Work, and Incognito profiles within
        // the 360px narrow panel bounds, and keeps the real stateful add_menu open.
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
        harness.read_with(&vcx, |h, _| assert!(h.add_menu_open));
        let submenu_bounds = vcx
            .debug_bounds("right-panel-browser-profiles-submenu")
            .expect("submenu bounds");
        assert!(
            f32::from(submenu_bounds.right()) <= 360.0,
            "submenu must fit within 360px narrow panel, got right={:?}",
            submenu_bounds.right()
        );
        assert!(vcx.debug_bounds("right-panel-browser-profile-default").is_some());
        assert!(vcx.debug_bounds("right-panel-browser-profile-work").is_some());
        assert!(vcx.debug_bounds("right-panel-browser-profile-incognito").is_some());

        // Clicking `work` with a real stateful `on_dismiss` must NOT be eaten by Capture-phase
        // `on_mouse_down_out`: it dispatches `preview.open` with `profileId: "work"` and closes menu.
        let work_center = vcx
            .debug_bounds("right-panel-browser-profile-work")
            .unwrap()
            .center();
        vcx.simulate_click(work_center, Default::default());
        vcx.run_until_parked();
        harness.read_with(&vcx, |h, _| assert!(!h.add_menu_open));

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

        // Reopening the `+` menu after dismissal starts with the Browser submenu closed,
        // and dismissing outside while submenu is open also resets submenu state on next reopen.
        vcx.update(|window, cx| {
            harness.update(cx, |h, cx| {
                h.add_menu_open = true;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("right-panel-browser-profiles-submenu").is_none());
        let chevron = vcx.debug_bounds("right-panel-add-browser-chevron").unwrap().center();
        vcx.simulate_click(chevron, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("right-panel-browser-profiles-submenu").is_some());
        // Click outside the menu (at x=340, y=450) -> closes menu and resets submenu state.
        vcx.simulate_click(gpui::point(px(340.), px(450.)), Default::default());
        vcx.run_until_parked();
        harness.read_with(&vcx, |h, _| assert!(!h.add_menu_open));
        vcx.update(|window, cx| {
            harness.update(cx, |h, cx| {
                h.add_menu_open = true;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("right-panel-browser-profiles-submenu").is_none(),
            "submenu must reset closed after outside-click dismissal"
        );

        // Empty-state Browser profile trigger: open -> click trigger again stays closed (reopen guard)
        // -> open -> click Incognito dispatches `preview.open` with `profileId: "incognito"`.
        vcx.update(|window, cx| {
            harness.update(cx, |h, cx| {
                h.add_menu_open = false;
                h.show_empty = true;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        });
        let trigger = vcx
            .debug_bounds("empty-browser-profile-trigger")
            .expect("empty-state browser profile trigger")
            .center();
        vcx.simulate_click(trigger, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("empty-browser-profiles-popup").is_some());

        // Second click on the open trigger closes instead of immediately reopening on mouse-up.
        vcx.simulate_click(trigger, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("empty-browser-profiles-popup").is_none(),
            "second click on empty-browser-profile-trigger must stay closed"
        );

        // Re-open (after guard clears) and click Incognito -> dispatches preview.open(incognito).
        vcx.simulate_click(trigger, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let incognito_center = vcx
            .debug_bounds("empty-browser-profile-incognito")
            .expect("empty-state incognito profile item")
            .center();
        vcx.simulate_click(incognito_center, Default::default());
        vcx.run_until_parked();
        let mut saw_incognito_open = false;
        for _ in 0..8 {
            let (method, params, reply) = fixture.next_request().await;
            let _ = reply.send(Err(t3_client::session::RpcError::Closed));
            if method == t3_core::preview::method::OPEN {
                assert_eq!(params["profileId"], "incognito");
                saw_incognito_open = true;
                break;
            }
        }
        assert!(saw_incognito_open, "clicking empty-state Incognito must dispatch preview.open with profileId=incognito");

        state.update(&mut vcx, |s, _| s.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }
}
