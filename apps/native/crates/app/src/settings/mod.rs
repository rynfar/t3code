//! Settings: a modal over the current thread (`web/src/routes/_chat.settings.tsx`,
//! `AppRouteModal.tsx`) with section navigation and settings search (`SettingsSidebarNav.tsx`,
//! `settingsSearch.ts`). Opened with Cmd+, or the app menu; Esc or the backdrop closes it. The
//! view exists only while open, so a closed modal costs nothing.

mod about;
mod appearance;
mod archive;
mod controls;
mod general;
mod keybindings;
mod store;
mod threads;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, KeyBinding,
    MouseButton, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Window, actions, div, point,
    prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::settings::search::{self, SearchItem, Section};

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use controls::{Button, ButtonSize, ButtonVariant, icon, r};

actions!(settings, [OpenSettings, CloseSettings, FocusSettingsSearch]);

struct Host {
    state: Entity<AppState>,
    view: Option<Entity<SettingsView>>,
    pending: Option<(Section, Option<String>)>,
}

impl Global for Host {}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    store::init(cx);
    controls::init(cx);
    cx.set_global(Host { state, view: None, pending: None });
    cx.on_action(|_: &OpenSettings, cx| open(Section::General, cx));
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("escape", CloseSettings, Some("SettingsModal")),
        KeyBinding::new("cmd-f", FocusSettingsSearch, Some("SettingsModal")),
    ]);
}

/// Opens settings at `section`, or moves an open modal there.
pub fn open(section: Section, cx: &mut App) {
    open_at(section, None, cx);
}

/// Opens settings at the row a settings search result names, as the palette's results do.
pub fn open_setting(id: &str, cx: &mut App) {
    if let Some(item) = search::items().into_iter().find(|item| item.id == id) {
        open_at(item.section, Some(item.anchor().to_owned()), cx);
    }
}

fn open_at(section: Section, anchor: Option<String>, cx: &mut App) {
    cx.global_mut::<Host>().pending = Some((section, anchor));
    cx.refresh_windows();
}

/// The settings search results the command palette lists (`CommandPalette.tsx`).
pub fn palette_items() -> Vec<t3_core::palette::CommandPaletteItem> {
    search::items()
        .into_iter()
        .map(|item| t3_core::palette::CommandPaletteItem {
            value: format!("setting:{}", item.id),
            search_terms: [item.title.clone(), item.section.label().to_owned()].into_iter().chain(item.terms.iter().cloned()).collect(),
            description: Some(format!("Settings \u{00b7} {}", item.section.label())),
            secondary: item.secondary,
            title: item.title,
            ..Default::default()
        })
        .collect()
}

fn close(cx: &mut App) {
    cx.global_mut::<Host>().view = None;
    store::clear_error(cx);
    cx.refresh_windows();
}

/// The modal layer, drawn by the workspace above everything else while settings are open.
pub fn overlay(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if let Some((section, anchor)) = cx.global_mut::<Host>().pending.take() {
        match cx.global::<Host>().view.clone() {
            Some(view) => view.update(cx, |view, cx| view.navigate(section, anchor, cx)),
            None => {
                let state = cx.global::<Host>().state.clone();
                let view = cx.new(|cx| SettingsView::new(state, section, anchor, window, cx));
                view.read(cx).focus.clone().focus(window, cx);
                cx.global_mut::<Host>().view = Some(view);
            }
        }
    }
    let view = cx.global::<Host>().view.clone()?;
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let inset = if window.viewport_size().width >= px(1024.) { 40. } else { 24. };
    let modal = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(inset))
        .bg(hsla(p.dialog_backdrop))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| close(cx))
        .child(
            div()
                .size_full()
                .max_w(px(1280.))
                .max_h(px(960.))
                .flex()
                .rounded(px(16.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.background))
                .text_color(hsla(p.foreground))
                .overflow_hidden()
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::black().opacity(if dark { 0.6 } else { 0.4 }),
                    offset: point(px(0.), px(25.)),
                    blur_radius: px(50.),
                    spread_radius: px(-12.),
                    inset: false,
                }])
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(view),
        );
    Some(gpui::deferred(modal).with_priority(1).into_any_element())
}

/// One top-level block of a page and the row ids inside it, so a search jump can scroll to it.
pub(crate) struct Block {
    anchors: Vec<SharedString>,
    element: AnyElement,
}

impl Block {
    fn new(anchors: &[&'static str], element: impl IntoElement) -> Self {
        Block { anchors: anchors.iter().map(|anchor| SharedString::new_static(anchor)).collect(), element: element.into_any_element() }
    }

    fn with_anchors(anchors: Vec<SharedString>, element: impl IntoElement) -> Self {
        Block { anchors, element: element.into_any_element() }
    }
}

pub struct SettingsView {
    state: Entity<AppState>,
    focus: FocusHandle,
    section: Section,
    /// The row a search jump landed on, tinted until the next navigation.
    target: Option<String>,
    scroll_to_target: bool,
    search: Entity<InputState>,
    active_result: usize,
    scroll: ScrollHandle,
    server_address: String,
    pub(crate) threads: threads::ThreadsState,
    pub(crate) keybindings: keybindings::KeybindingsState,
    pub(crate) archive: archive::ArchiveState,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    fn new(state: Entity<AppState>, section: Section, anchor: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.active_result = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.open_active_result(window, cx),
                _ => {}
            }),
            cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::ServerConfig | AppEvent::Phase | AppEvent::Shell) {
                    cx.notify();
                }
            }),
            Self::intercept_result_keys(cx),
        ];
        let mut view = SettingsView {
            threads: threads::ThreadsState::new(window, cx),
            keybindings: keybindings::KeybindingsState::new(window, cx),
            archive: archive::ArchiveState::default(),
            state,
            focus: cx.focus_handle(),
            section,
            target: None,
            scroll_to_target: false,
            search,
            active_result: 0,
            scroll: ScrollHandle::new(),
            server_address: t3_client::store::load().map(|credentials| credentials.http_base).unwrap_or_default(),
            _subscriptions: subscriptions,
        };
        view.navigate(section, anchor, cx);
        view
    }

    fn navigate(&mut self, section: Section, target: Option<String>, cx: &mut Context<Self>) {
        let changed = self.section != section;
        self.section = section;
        self.scroll_to_target = target.is_some();
        self.target = target;
        if changed || self.target.is_none() {
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        if section == Section::Archived {
            self.load_archive(cx);
        }
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    fn search_items(&self, cx: &App) -> Vec<SearchItem> {
        let mut items = search::items();
        let mac = cfg!(target_os = "macos");
        if let Some(config) = self.state.read(cx).server_config.config.as_ref() {
            for row in t3_core::settings::keybindings::rows(&config.keybindings, mac) {
                items.push(search::keybinding_item(&row.command, &row.label, &row.shortcuts));
            }
        }
        items
    }

    fn open_active_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.search_items(cx);
        let query = self.query(cx);
        let results = search::search(&query, &items);
        if let Some(item) = results.get(self.active_result.min(results.len().saturating_sub(1))) {
            let (section, anchor) = (item.section, item.anchor().to_owned());
            self.clear_search(window, cx);
            self.navigate(section, Some(anchor), cx);
            self.focus.focus(window, cx);
        }
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.set_value("", window, cx));
        self.active_result = 0;
    }

    fn on_close(&mut self, _: &CloseSettings, window: &mut Window, cx: &mut Context<Self>) {
        if !self.query(cx).trim().is_empty() {
            self.clear_search(window, cx);
            cx.notify();
            return;
        }
        close(cx);
    }

    fn on_focus_search(&mut self, _: &FocusSettingsSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_key_down(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "/" && !keystroke.modifiers.modified() && !self.text_field_focused(window, cx) {
            cx.stop_propagation();
            self.on_focus_search(&FocusSettingsSearch, window, cx);
        }
    }

    /// The search field is single-line and swallows Up and Down, so they are taken before it
    /// sees them to move through the results (`handleSearchKeyDown`).
    fn intercept_result_keys(cx: &mut Context<Self>) -> Subscription {
        let view = cx.entity().downgrade();
        cx.intercept_keystrokes(move |event, window, cx| {
            let key = event.keystroke.key.as_str();
            if !matches!(key, "up" | "down") || event.keystroke.modifiers.modified() {
                return;
            }
            let _ = view.update(cx, |this, cx| {
                if this.query(cx).trim().is_empty() || !this.search.focus_handle(cx).is_focused(window) {
                    return;
                }
                cx.stop_propagation();
                let count = search::search(&this.query(cx), &this.search_items(cx)).len().max(1);
                this.active_result = if key == "down" { (this.active_result + 1) % count } else { (this.active_result + count - 1) % count };
                cx.notify();
            });
        })
    }

    fn text_field_focused(&self, window: &Window, cx: &App) -> bool {
        [&self.search, &self.threads.base_directory, &self.threads.auto_settle_days, &self.keybindings.filter]
            .into_iter()
            .chain(self.threads.time_inputs())
            .any(|input| input.focus_handle(cx).is_focused(window))
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let query = self.query(cx);
        let searching = !query.trim().is_empty();
        let search_row = h_flex()
            .h(r(32.))
            .px(r(8.))
            .gap(r(8.))
            .rounded(r(6.))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(hsla(p.sidebar_muted_foreground))
            .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
            .child(icon(Icon::Search, 16., hsla(p.sidebar_muted_foreground).opacity(0.8)))
            .child(div().flex_1().min_w_0().child(Input::new(&self.search).appearance(false).bordered(false).cleanable(false)))
            .child(if searching {
                Button::new("clear-settings-search", ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                    .icon(Icon::X)
                    .tooltip("Clear settings search")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_search(window, cx);
                        this.search.update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    }))
                    .into_any_element()
            } else {
                crate::composer::controls::kbd("/", cx).into_any_element()
            });
        let body = if searching {
            self.render_results(&query, cx).into_any_element()
        } else {
            self.render_sections(cx).into_any_element()
        };
        v_flex()
            .w(r(240.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(hsla(p.border))
            .bg(if dark { hsla(p.card) } else { hsla(p.sidebar) })
            .text_color(hsla(p.sidebar_foreground))
            .child(
                v_flex()
                    .id("settings-nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(r(8.))
                    .gap(r(8.))
                    .child(search_row)
                    .child(body),
            )
    }

    fn render_sections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        v_flex().gap(r(16.)).children(search::nav_groups().into_iter().enumerate().map(|(group_index, (heading, sections))| {
            v_flex()
                .gap(r(4.))
                .children(heading.map(|heading| {
                    div().px(r(8.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_muted_foreground).opacity(0.8)).child(heading)
                }))
                .children(sections.into_iter().map(|section| {
                    let active = section == self.section;
                    let color = if active { hsla(p.sidebar_foreground) } else { hsla(p.sidebar_muted_foreground).opacity(0.8) };
                    h_flex()
                        .id(("settings-section", group_index * 100 + section as usize))
                        .h(r(32.))
                        .px(r(8.))
                        .gap(r(8.))
                        .rounded(r(8.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .cursor_pointer()
                        .when(active, |this| this.bg(hsla(p.sidebar_row_selected)))
                        .when(!active, |this| this.hover(|this| this.bg(hsla(p.sidebar_row_hover))))
                        .child(icon(section_icon(section), 16., if active { color } else { hsla(p.sidebar_icon) }))
                        .child(div().truncate().child(section.label()))
                        .on_click(cx.listener(move |this, _, _, cx| this.navigate(section, None, cx)))
                }))
        }))
    }

    fn render_results(&self, query: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let items = self.search_items(cx);
        let results = search::search(query, &items);
        if results.is_empty() {
            return div()
                .px(r(8.))
                .py(r(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.sidebar_muted_foreground))
                .child("No settings found")
                .into_any_element();
        }
        let active = self.active_result.min(results.len() - 1);
        v_flex()
            .gap(r(2.))
            .children(results.into_iter().enumerate().map(|(index, item)| {
                let (section, anchor) = (item.section, item.anchor().to_owned());
                let highlighted = index == active;
                h_flex()
                    .id(SharedString::from(format!("settings-search-result-{}", item.id)))
                    .min_h(r(40.))
                    .px(r(8.))
                    .py(r(4.))
                    .gap(r(8.))
                    .items_start()
                    .rounded(r(8.))
                    .cursor_pointer()
                    .when(highlighted, |this| this.bg(hsla(p.sidebar_row_selected)))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.active_result != index {
                            this.active_result = index;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.clear_search(window, cx);
                        this.navigate(section, Some(anchor.clone()), cx);
                    }))
                    .child(div().pt(r(2.)).child(icon(section_icon(section), 16., hsla(p.sidebar_icon))))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_foreground)).child(item.title.clone()))
                            .child(div().truncate().text_size(r(11.)).text_color(hsla(p.sidebar_muted_foreground).opacity(0.75)).child(section.label())),
                    )
            }))
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        h_flex()
            .h(r(48.))
            .flex_none()
            .pl(r(20.))
            .pr(r(8.))
            .gap(r(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(6.))
                    .text_sm()
                    .child(div().text_color(hsla(p.muted_foreground)).child("Settings"))
                    .child(div().text_color(hsla(p.icon_muted)).child("/"))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(self.section.label())),
            )
            .child(
                Button::new("close-settings", ButtonVariant::Ghost, ButtonSize::IconSm)
                    .icon(Icon::X)
                    .tooltip("Close (Esc)")
                    .on_click(|_, _, cx| close(cx)),
            )
    }

    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        match self.section {
            Section::General => self.general(cx),
            Section::Appearance => self.appearance(cx),
            Section::Chat => self.chat(cx),
            Section::Labs => self.labs(cx),
            Section::Threads => self.threads_page(window, cx),
            Section::Keybindings => self.keybindings_page(cx),
            Section::Connections => self.environments(cx),
            Section::About => self.about(cx),
            Section::Archived => self.archive_page(cx),
            _ => Vec::new(),
        }
    }

    pub(crate) fn is_target(&self, id: &str) -> bool {
        self.target.as_deref() == Some(id)
    }
}

fn section_icon(section: Section) -> Icon {
    match section {
        Section::General => Icon::Settings2,
        Section::Appearance => Icon::Palette,
        Section::Chat => Icon::MessageSquareText,
        Section::Keybindings => Icon::Keyboard,
        Section::Providers => Icon::Bot,
        Section::Threads => Icon::MessagesSquare,
        Section::Connections => Icon::Server,
        Section::Maintenance => Icon::Wrench,
        Section::Labs => Icon::FlaskConical,
        Section::About => Icon::Info,
        Section::Archived => Icon::Archive,
        _ => Icon::Settings2,
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let blocks = self.render_page(window, cx);
        if self.scroll_to_target {
            self.scroll_to_target = false;
            if let Some(index) = self.target.as_deref().and_then(|target| blocks.iter().position(|block| block.anchors.iter().any(|anchor| anchor.as_ref() == target))) {
                self.scroll.scroll_to_item(index + usize::from(store::error(cx).is_some()));
            }
        }
        let p = palette(cx);
        let error = store::error(cx).map(|error| {
            div().w_full().max_w(r(896.)).px(r(24.)).child(
                v_flex()
                    .px(r(14.))
                    .py(r(12.))
                    .gap(r(2.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.error).opacity(0.32))
                    .bg(hsla(p.error_surface))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Setting not saved"))
                    .child(div().opacity(0.8).child(error)),
            )
        });
        h_flex()
            .size_full()
            .key_context("SettingsModal")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_focus_search))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(self.render_nav(cx))
            .child(
                v_flex().flex_1().min_w_0().h_full().child(self.render_header(cx)).child(
                    v_flex()
                        .id("settings-content")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .items_center()
                        .pt(r(24.))
                        .pb(r(48.))
                        .gap(r(24.))
                        .children(error)
                        .children(blocks.into_iter().map(|block| div().w_full().max_w(r(896.)).px(r(24.)).child(block.element))),
                ),
            )
    }
}
