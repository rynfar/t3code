//! Settings: a modal over the current thread (`web/src/routes/_chat.settings.tsx`,
//! `AppRouteModal.tsx`) with section navigation and settings search (`SettingsSidebarNav.tsx`,
//! `settingsSearch.ts`). Opened with Cmd+, or the app menu; Esc or the backdrop closes it. The
//! view exists only while open, so a closed modal costs nothing. Tab stays inside the modal,
//! sections take arrow keys, and closing commits the focused field first.

mod about;
mod access;
mod appearance;
pub(crate) mod theme_import;
mod archive;
pub(crate) mod controls;
mod devices;
pub(crate) mod diagnostics;
mod diffs_browser;
mod general;
mod keybindings;
mod maintenance;
mod project_actions;
mod project_icon_picker;
mod projects;
mod providers;
mod scheduled_tasks;
mod secrets;
mod source_control;
pub(crate) mod store;
mod threads;
mod typography;
mod worktrees;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, KeyBinding,
    KeyDownEvent, MouseButton, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Window, actions,
    div, point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::settings::search::{self, Features, SearchItem, Section};

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use controls::{Button, ButtonSize, ButtonVariant, ModalLayout, focus_ring, icon, r};

actions!(settings, [OpenSettings, CloseSettings, FocusSettingsSearch, FocusNextInSettings, FocusPreviousInSettings]);

struct Host {
    state: Entity<AppState>,
    view: Option<Entity<SettingsView>>,
    pending: Option<(Section, Option<String>)>,
    /// The project a "Project settings" entry point opens the Projects page on.
    pending_project: Option<String>,
    pending_provider: Option<providers::ProviderSettingsTarget>,
    /// What had focus when the modal opened, focused again when it closes.
    previous_focus: Option<FocusHandle>,
    /// Features the web build reads from its environment, read once at launch.
    features: Features,
    _provider_navigation: Subscription,
}

impl Global for Host {}

pub fn init(state: Entity<AppState>, cx: &mut App) {
    store::init(cx);
    controls::init(cx);
    typography::init(cx);
    providers::init(cx);
    let features = Features::from_secrets_flag(std::env::var("T3CODE_FEATURE_SECRETS").ok().as_deref());
    let provider_navigation = cx.subscribe(&state, |state, event: &AppEvent, cx| {
        if matches!(event, AppEvent::Phase | AppEvent::ServerConfig)
            && cx
                .global::<Host>()
                .pending_provider
                .as_ref()
                .is_some_and(|target| !target.setup_current(state.read(cx)))
        {
            let host = cx.global_mut::<Host>();
            host.pending_provider = None;
            host.pending = None;
        }
    });
    cx.set_global(Host {
        _provider_navigation: provider_navigation,
        state,
        view: None,
        pending: None,
        pending_project: None,
        pending_provider: None,
        previous_focus: None,
        features,
    });
    cx.on_action(|_: &OpenSettings, cx| open(Section::General, cx));
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("escape", CloseSettings, Some("SettingsModal")),
        KeyBinding::new("cmd-f", FocusSettingsSearch, Some("SettingsModal")),
        KeyBinding::new("tab", FocusNextInSettings, Some("SettingsModal")),
        KeyBinding::new("shift-tab", FocusPreviousInSettings, Some("SettingsModal")),
    ]);
}

/// Opens settings at `section`, or moves an open modal there.
pub fn open(section: Section, cx: &mut App) {
    open_at(section, None, cx);
}

pub(crate) fn open_provider_setup(target: crate::composer::ProviderTarget, cx: &mut App) {
    let state = cx.global::<Host>().state.read(cx);
    let Some(instance) = target.instance.as_deref() else {
        return;
    };
    if target.environment != crate::composer::ProviderTarget::environment(state)
        || target.connection != state.connection_id()
    {
        return;
    }
    let Some(target) = providers::ProviderSettingsTarget::capture(state, instance) else {
        return;
    };
    if !target.setup_current(state) {
        return;
    }
    open_at(Section::Providers, None, cx);
    cx.global_mut::<Host>().pending_provider = Some(target);
}

#[cfg(all(test, feature = "snapshot"))]
pub(crate) fn setup_selection_for_test(window: &mut Window, cx: &mut App) -> Option<String> {
    apply_pending(window, cx);
    let view = cx.global::<Host>().view.clone()?;
    view.update(cx, |view, cx| {
        if view.section != Section::Providers {
            return None;
        }
        view.invalidate_provider_setup(cx);
        view.selected_instance_id(cx)
    })
}

#[cfg(all(test, feature = "snapshot"))]
pub(crate) fn test_setup_target_lifetime(
    target: crate::composer::ProviderTarget,
    window: &mut Window,
    cx: &mut App,
) {
    let state = cx.global::<Host>().state.clone();
    finish_close(window, cx);
    open_provider_setup(target.clone(), cx);
    open(Section::General, cx);
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "generic opening cancels the pending target"
    );
    finish_close(window, cx);
    open_provider_setup(target.clone(), cx);
    finish_close(window, cx);
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "close cancels even an unconsumed target"
    );
    let mut stale = target.clone();
    stale.connection = Some(u64::MAX);
    open_provider_setup(stale, cx);
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "a stale connection cannot activate setup"
    );
    open_provider_setup(target.clone(), cx);
    cx.global_mut::<Host>()
        .pending_provider
        .as_mut()
        .unwrap()
        .connection = Some(u64::MAX);
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "deferred consumption revalidates the connection"
    );
    open_provider_setup(target.clone(), cx);
    let environment = target.environment.clone();
    state.update(cx, |state, cx| {
        state
            .server_config
            .config
            .as_mut()
            .unwrap()
            .environment
            .environment_id = "stale-environment".into();
        cx.emit(AppEvent::ServerConfig);
    });
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "deferred consumption rejects a different environment"
    );
    state.update(cx, |state, cx| {
        state
            .server_config
            .config
            .as_mut()
            .unwrap()
            .environment
            .environment_id = environment;
        cx.emit(AppEvent::ServerConfig);
    });
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "discarded pending targets cannot reactivate"
    );
    open_provider_setup(target.clone(), cx);
    assert_eq!(setup_selection_for_test(window, cx), target.instance);
    let providers = state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .unwrap()
        .providers
        .clone();
    let provider = providers
        .iter()
        .find(|provider| Some(&provider.instance_id) == target.instance.as_ref())
        .unwrap()
        .clone();
    state.update(cx, |state, cx| {
        state
            .server_config
            .config
            .as_mut()
            .unwrap()
            .providers
            .retain(|p| p.instance_id != provider.instance_id);
        cx.emit(AppEvent::ServerConfig);
    });
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "removed targets never fall back to the first row"
    );
    state.update(cx, |state, cx| {
        state.server_config.config.as_mut().unwrap().providers = providers;
        cx.emit(AppEvent::ServerConfig);
    });
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "restoring the row must not reactivate the explicit target"
    );
    let view = cx.global::<Host>().view.clone().unwrap();
    view.update(cx, |view, cx| view.navigate(Section::Providers, None, cx));
    assert!(
        setup_selection_for_test(window, cx).is_some(),
        "generic navigation restores normal selection"
    );
    finish_close(window, cx);
    let mut missing = target.clone();
    missing.instance = Some("removed-provider".into());
    open_provider_setup(missing, cx);
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "missing targets cannot open the first row"
    );
    open_provider_setup(target, cx);
    let _ = setup_selection_for_test(window, cx);
    let environment = state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .unwrap()
        .environment
        .environment_id
        .clone();
    state.update(cx, |state, cx| {
        state
            .server_config
            .config
            .as_mut()
            .unwrap()
            .environment
            .environment_id = "other-environment".into();
        cx.emit(AppEvent::ServerConfig);
    });
    assert_eq!(setup_selection_for_test(window, cx), None);
    state.update(cx, |state, cx| {
        state
            .server_config
            .config
            .as_mut()
            .unwrap()
            .environment
            .environment_id = environment;
        cx.emit(AppEvent::ServerConfig);
    });
    assert_eq!(
        setup_selection_for_test(window, cx),
        None,
        "consumed targets cannot reactivate across an environment round trip"
    );
    finish_close(window, cx);
}

fn features(cx: &App) -> Features {
    cx.global::<Host>().features
}

/// Opens settings at the row a settings search result names, as the palette's results do.
pub fn open_setting(id: &str, cx: &mut App) {
    if let Some(item) = search::items(features(cx)).into_iter().find(|item| item.id == id) {
        open_at(item.section, Some(item.anchor().to_owned()), cx);
    }
}

fn open_at(section: Section, anchor: Option<String>, cx: &mut App) {
    cx.global_mut::<Host>().pending_provider = None;
    cx.global_mut::<Host>().pending = Some((section, anchor));
    cx.refresh_windows();
}

/// Opens Settings → Projects on the project holding `project_id`, from every "Project settings"
/// entry point (`openProjectSettings` in `Sidebar.tsx`, the command palette).
pub fn open_project(project_id: &str, cx: &mut App) {
    cx.global_mut::<Host>().pending_project = Some(project_id.to_owned());
    open_at(Section::Projects, None, cx);
}

/// Opens the Add action editor on the shown project (snapshots).
#[cfg(feature = "snapshot")]
pub fn open_add_action(cx: &mut App) {
    if let Some(view) = cx.global::<Host>().view.clone() {
        view.update(cx, |view, cx| view.queue_add_action(cx));
        cx.refresh_windows();
    }
}

/// Back to the project list once the shown project is removed.
pub fn show_project_list(cx: &mut App) {
    if let Some(view) = cx.global::<Host>().view.clone() {
        view.update(cx, |view, cx| view.select_project(None, cx));
    }
}

/// The settings search results the command palette lists (`CommandPalette.tsx`).
pub fn palette_items(cx: &App) -> Vec<t3_core::palette::CommandPaletteItem> {
    search::items(features(cx))
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

/// The per-device settings every view reads (`useClientSettings`).
pub fn client_settings(cx: &App) -> &t3_core::client_settings::ClientSettingsDoc {
    store::client(cx)
}

/// Closes from outside the view: commits the focused field first, as dropping the view
/// unmounts it without a blur (`AppRouteModal.tsx` `onOpenChange`).
fn close(window: &mut Window, cx: &mut App) {
    if let Some(view) = cx.global::<Host>().view.clone() {
        view.update(cx, |view, cx| view.commit_focused_field(window, cx));
    }
    finish_close(window, cx);
}

fn finish_close(window: &mut Window, cx: &mut App) {
    let host = cx.global_mut::<Host>();
    host.view = None;
    host.pending = None;
    host.pending_project = None;
    host.pending_provider = None;
    let previous = host.previous_focus.take();
    store::clear_error(cx);
    if let Some(previous) = previous {
        window.focus(&previous, cx);
    }
    cx.refresh_windows();
}

fn apply_pending(window: &mut Window, cx: &mut App) {
    let pending = cx.global_mut::<Host>().pending.take();
    let provider = cx.global_mut::<Host>().pending_provider.take();
    if let Some((section, anchor)) = pending.filter(|_| {
        provider
            .as_ref()
            .is_none_or(|target| target.setup_current(cx.global::<Host>().state.read(cx)))
    }) {
        let view = match cx.global::<Host>().view.clone() {
            Some(view) => {
                view.update(cx, |view, cx| {
                    if provider.is_none() {
                        view.commit_focused_provider_field(window, cx);
                    }
                    view.navigate(section, anchor, cx);
                });
                view
            }
            None => {
                let state = cx.global::<Host>().state.clone();
                let previous = window.focused(cx);
                let view = cx.new(|cx| SettingsView::new(state, section, anchor, window, cx));
                view.read(cx).focus.clone().focus(window, cx);
                let host = cx.global_mut::<Host>();
                host.view = Some(view.clone());
                host.previous_focus = previous;
                view
            }
        };
        if let Some(provider) = provider {
            view.update(cx, |view, cx| {
                view.select_provider_setup(provider, window, cx)
            });
        }
        if let Some(project_id) = cx.global_mut::<Host>().pending_project.take() {
            view.update(cx, |view, cx| view.select_project_id(&project_id, cx));
        }
    }
}

/// The modal layer, drawn by the workspace above everything else while settings are open.
pub fn overlay(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    apply_pending(window, cx);
    let view = cx.global::<Host>().view.clone()?;
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let layout = ModalLayout::of(window);
    let modal = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(layout.inset)
        .bg(hsla(p.dialog_backdrop))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, cx| close(window, cx))
        .child(
            div()
                .size_full()
                .flex()
                .when(!layout.full_bleed, |this| this.max_w(r(1280.)).max_h(r(960.)).rounded(r(16.)).border_1())
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
    /// One focus handle per nav section; only one is a Tab stop (`useSettingsSectionNavigation`).
    section_focus: Vec<(Section, FocusHandle)>,
    /// The section arrows last moved to, and the page open when they did.
    focused_section: Option<(Section, Section)>,
    /// The compact (icon rail) nav shows the search field over the page while this is set.
    compact_search_open: bool,
    pub(crate) threads: threads::ThreadsState,
    pub(crate) keybindings: keybindings::KeybindingsState,
    pub(crate) archive: archive::ArchiveState,
    pub(crate) providers: providers::ProvidersState,
    pub(crate) worktrees: worktrees::WorktreesState,
    pub(crate) source_control: source_control::SourceControlState,
    pub(crate) scheduled: scheduled_tasks::ScheduledTasksState,
    pub(crate) access: access::AccessPageState,
    pub(crate) projects: projects::ProjectsState,
    pub(crate) secrets: secrets::SecretsState,
    pub(crate) maintenance: maintenance::MaintenanceState,
    pub(crate) diagnostics: diagnostics::DiagnosticsState,
    pub(crate) devices: devices::DevicesState,
    browser: diffs_browser::BrowserState,
    typography: typography::TypographyState,
    appearance_ui: appearance::AppearanceState,
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
                InputEvent::Focus => {
                    this.compact_search_open = true;
                    cx.notify();
                }
                InputEvent::Blur if this.query(cx).trim().is_empty() => {
                    this.compact_search_open = false;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::ServerConfig | AppEvent::Phase | AppEvent::Shell) {
                    this.invalidate_provider_setup(cx);
                    cx.notify();
                }
            }),
            Self::intercept_result_keys(cx),
        ];
        let mut view = SettingsView {
            threads: threads::ThreadsState::new(window, cx),
            keybindings: keybindings::KeybindingsState::new(window, cx),
            archive: archive::ArchiveState::default(),
            providers: providers::ProvidersState::new(window, cx),
            worktrees: worktrees::WorktreesState::new(window, cx),
            source_control: source_control::SourceControlState::new(window, cx),
            scheduled: scheduled_tasks::ScheduledTasksState::new(window, cx),
            access: access::AccessPageState::new(window, cx),
            projects: projects::ProjectsState::new(window, cx),
            secrets: secrets::SecretsState::new(window, cx),
            maintenance: maintenance::MaintenanceState::new(window, cx),
            diagnostics: diagnostics::DiagnosticsState::new(window, cx),
            devices: devices::DevicesState::new(cx),
            browser: diffs_browser::BrowserState::new(window, cx),
            typography: typography::TypographyState::new(window, cx),
            appearance_ui: appearance::AppearanceState::new(cx),
            state,
            focus: cx.focus_handle(),
            section,
            target: None,
            scroll_to_target: false,
            search,
            active_result: 0,
            scroll: ScrollHandle::new(),
            section_focus: search::nav_groups(features(cx))
                .into_iter()
                .flat_map(|(_, sections)| sections)
                .map(|section| (section, cx.focus_handle().tab_stop(true)))
                .collect(),
            focused_section: None,
            compact_search_open: false,
            _subscriptions: subscriptions,
        };
        view.navigate(section, anchor, cx);
        view
    }

    fn navigate(&mut self, section: Section, target: Option<String>, cx: &mut Context<Self>) {
        self.providers.clear_setup_target();
        if section != Section::Providers { self.providers.leave(); }
        let changed = self.section != section;
        self.section = section;
        self.scroll_to_target = target.is_some();
        if let Some(target) = target.as_deref() {
            if matches!(target, "prompt-font" | "terminal-font") && !store::client(cx).separate_fonts() {
                store::update_client(&self.state, t3_core::client_settings::ClientSettingChange::SeparateFonts(true), cx);
            }
            self.worktrees.reveal(target);
            self.source_control.reveal(target);
        }
        self.target = target;
        if changed || self.target.is_none() {
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        if section == Section::Archived {
            self.load_archive(cx);
        }
        if section != Section::ScheduledTasks {
            self.scheduled.stream = None;
        }
        if section != Section::Access {
            self.access.close();
        }
        if section != Section::Projects {
            self.projects.actions.close();
        }
        if section != Section::Secrets {
            self.secrets.dismiss();
        }
        if section != Section::Maintenance {
            self.maintenance.leave();
        }
        if section == Section::Diagnostics {
            if changed {
                self.diagnostics.reset();
            }
        } else {
            self.diagnostics.leave();
        }
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    fn search_items(&self, cx: &App) -> Vec<SearchItem> {
        let mut items = search::items(features(cx));
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
        #[cfg(feature = "snapshot")]
        self.removal_route_audit("route-escape", window, cx);
        if self.dismiss_provider_removal(window, cx) {
            return;
        }
        if self.dismiss_project_icon_picker(window, cx) {
            return;
        }
        if self.dismiss_keybinding_overlay(window, cx) {
            return;
        }
        if let Some(dialog)=self.appearance_ui.import.clone() && dialog.read(cx).is_open() {
            dialog.update(cx,|dialog,cx|dialog.close(window,cx));
            return;
        }
        if self.projects.actions.dismiss() || self.secrets.dismiss() || self.maintenance.dismiss() {
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        if !self.query(cx).trim().is_empty() {
            self.clear_search(window, cx);
            cx.notify();
            return;
        }
        self.commit_focused_field(window, cx);
        finish_close(window, cx);
    }

    /// Saves what a commit-on-blur field holds, before the modal unmounts it.
    fn commit_focused_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_browser_url(window, cx);
        self.commit_focused_thread_field(window, cx);
        self.commit_focused_provider_field(window, cx);
        self.commit_focused_worktree_field(window, cx);
        self.commit_focused_source_control_field(window, cx);
        self.commit_focused_project_field(window, cx);
        self.commit_focused_maintenance_field(window, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSettingsSearch, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        self.removal_route_audit("route-search", window, cx);
        if self.provider_removal_open() {
            return;
        }
        if self.projects.picker.is_some() {
            return;
        }
        self.compact_search_open = true;
        self.search.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Tab and Shift+Tab move through the modal's controls only (the dialog's focus trap).
    fn cycle_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        self.removal_route_audit("route-tab", window, cx);
        if self.cycle_provider_removal_focus(forward, window, cx) {
            return;
        }
        if self.cycle_icon_focus(forward, window, cx) {
            return;
        }
        // Bounded: every control in the window is at most one step each.
        for _ in 0..512 {
            if forward {
                window.focus_next(cx);
            } else {
                window.focus_prev(cx);
            }
            if self.focus.contains_focused(window, cx) && !self.focus.is_focused(window) {
                return;
            }
        }
        self.focus.focus(window, cx);
    }

    /// Up, Down, Home and End move between sections without opening them; Enter or Space opens.
    fn on_nav_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.alt || modifiers.control || modifiers.platform {
            return;
        }
        let Some(index) = self.section_focus.iter().position(|(_, handle)| handle.is_focused(window)) else { return };
        let last = self.section_focus.len() - 1;
        let next = match event.keystroke.key.as_str() {
            "down" => (index + 1).min(last),
            "up" => index.saturating_sub(1),
            "home" => 0,
            "end" => last,
            "left" | "right" => return cx.stop_propagation(),
            _ => return,
        };
        cx.stop_propagation();
        let (section, handle) = self.section_focus[next].clone();
        self.focused_section = Some((section, self.section));
        handle.focus(window, cx);
        cx.notify();
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
                if crate::sidebar::is_composing(&this.search, window, cx) {
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
        if self.typography.text_field_focused(window, cx) || self.projects.picker.as_ref().is_some_and(|picker| picker.text_field_focused(window, cx)) {
            return true;
        }
        [&self.search, &self.threads.base_directory, &self.threads.auto_settle_days, &self.keybindings.filter, &self.keybindings.when_input]
            .into_iter()
            .chain(self.threads.time_inputs())
            .chain(self.providers.inputs())
            .chain(self.worktrees.inputs())
            .chain(self.scheduled.inputs())
            .chain([&self.access.label, &self.projects.name])
            .chain(self.projects.actions.inputs())
            .chain(self.secrets.inputs())
            .chain(self.maintenance.inputs())
            .chain(self.diagnostics.inputs())
            .any(|input| input.focus_handle(cx).is_focused(window))
            || [&self.worktrees.instructions, self.scheduled.textarea(), &self.projects.actions.command]
                .into_iter()
                .chain(self.source_control.textareas())
                .any(|input| input.focus_handle(cx).is_focused(window))
    }

    fn render_nav(&self, layout: ModalLayout, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let query = self.query(cx);
        let searching = !query.trim().is_empty();
        // The compact rail shows icons only until its search opens over the page.
        let overlay = layout.nav_compact && self.compact_search_open;
        let labels = !layout.nav_compact || overlay;
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
        let body = if searching && labels {
            self.render_results(&query, cx).into_any_element()
        } else {
            self.render_sections(labels, layout.nav_compact && !overlay, window, cx).into_any_element()
        };
        v_flex()
            .h_full()
            .flex_none()
            .map(|this| if overlay { this.absolute().top_0().bottom_0().left_0().w_full() } else { this.w(r(if layout.nav_compact { 56. } else { 240. })) })
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
                    .overflow_x_hidden()
                    .p(r(8.))
                    .gap(r(8.))
                    .on_key_down(cx.listener(Self::on_nav_key))
                    .when(labels, |this| this.child(search_row))
                    .child(body),
            )
    }

    fn render_sections(&self, labels: bool, rail: bool, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let keyboard = window.last_input_was_keyboard();
        let tab_stop = match self.focused_section {
            Some((section, page)) if page == self.section => section,
            _ => self.section,
        };
        let open_search = rail.then(|| {
            h_flex()
                .id("settings-compact-search")
                .h(r(32.))
                .justify_center()
                .rounded(r(8.))
                .cursor_pointer()
                .hover(|this| this.bg(hsla(p.sidebar_row_hover)))
                .child(icon(Icon::Search, 16., hsla(p.sidebar_icon)))
                .on_click(cx.listener(|this, _, window, cx| this.on_focus_search(&FocusSettingsSearch, window, cx)))
        });
        let current = self.section.nav_parent();
        v_flex().gap(r(16.)).children(open_search).children(search::nav_groups(features(cx)).into_iter().enumerate().map(|(group_index, (heading, sections))| {
            v_flex()
                .gap(r(4.))
                .when(rail && group_index > 0, |this| this.pt(r(16.)).border_t_1().border_color(hsla(p.sidebar_border)))
                .children(heading.filter(|_| labels).map(|heading| {
                    div().px(r(8.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_muted_foreground).opacity(0.8)).child(heading)
                }))
                .children(sections.into_iter().map(|section| {
                    let active = section == current;
                    let color = if active { hsla(p.sidebar_foreground) } else { hsla(p.sidebar_muted_foreground).opacity(0.8) };
                    let focus = self.section_focus.iter().find(|(candidate, _)| *candidate == section).map(|(_, handle)| handle.clone().tab_stop(section == tab_stop));
                    let ring = keyboard && focus.as_ref().is_some_and(|focus| focus.is_focused(window));
                    h_flex()
                        .id(("settings-section", group_index * 100 + section as usize))
                        .when_some(focus, |this, focus| this.track_focus(&focus))
                        .h(r(32.))
                        .px(r(8.))
                        .gap(r(8.))
                        .when(!labels, |this| this.justify_center())
                        .rounded(r(8.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .cursor_pointer()
                        .when(active, |this| this.bg(hsla(p.sidebar_row_selected)))
                        .when(!active, |this| this.hover(|this| this.bg(hsla(p.sidebar_row_hover))))
                        .when(ring, |this| this.shadow(focus_ring(hsla(p.ring))))
                        .child(icon(section_icon(section), 16., if active { color } else { hsla(p.sidebar_icon) }))
                        .when(labels, |this| this.child(div().truncate().child(section.label())))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.focused_section = None;
                            this.navigate(section, None, cx)
                        }))
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
                    .on_click(|_, window, cx| close(window, cx)),
            )
    }

    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        match self.section {
            Section::General => self.general(cx),
            Section::Diffs => self.diffs_page(cx),
            Section::Browser => self.browser_page(window, cx),
            Section::SnapShot => self.snapshots_page(cx),
            Section::Appearance => self.appearance(cx),
            Section::Chat => self.chat(cx),
            Section::Labs => self.labs(cx),
            Section::Threads => self.threads_page(window, cx),
            Section::Keybindings => self.keybindings_page(window, cx),
            Section::Providers => self.providers_page(window, cx),
            Section::Worktrees => self.worktrees_page(window, cx),
            Section::SourceControl => self.source_control_page(window, cx),
            Section::ScheduledTasks => self.scheduled_tasks_page(cx),
            Section::Projects => self.projects_page(window, cx),
            Section::Access => self.access_page(window, cx),
            Section::Secrets => self.secrets_page(cx),
            Section::Maintenance => self.maintenance_page(window, cx),
            Section::Diagnostics => self.diagnostics_page(window, cx),
            Section::Connections => self.environments(cx),
            Section::About => self.about(cx),
            Section::Archived => self.archive_page(cx),
            Section::Devices => self.devices_page(cx),
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
        Section::Devices => Icon::Smartphone,
        Section::ScheduledTasks => Icon::CalendarClock,
        Section::Projects => Icon::PanelsTopLeft,
        Section::SourceControl => Icon::GitBranch,
        Section::Worktrees => Icon::FolderTree,
        Section::Access => Icon::ShieldCheck,
        Section::Connections => Icon::Server,
        Section::Secrets => Icon::KeyRound,
        Section::Maintenance | Section::Diagnostics => Icon::Wrench,
        Section::Labs => Icon::FlaskConical,
        Section::About => Icon::Info,
        Section::Archived => Icon::Archive,
        _ => Icon::Settings2,
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = ModalLayout::of(window);
        let blocks = self.render_page(window, cx);
        let archive_list = match self.section {
            Section::Archived => self.archive_list(self.scroll_to_target, cx),
            Section::Projects => self.project_list_view(cx),
            _ => None,
        };
        if self.scroll_to_target {
            self.scroll_to_target = false;
            if let Some(index) = self.target.as_deref().and_then(|target| blocks.iter().position(|block| block.anchors.iter().any(|anchor| anchor.as_ref() == target))) {
                self.scroll.scroll_to_item(index + usize::from(store::error(cx).is_some()));
            }
        }
        let p = palette(cx);
        // `SettingsPageContainer width="expanded"`: Diagnostics' tables take `max-w-6xl`.
        let page_width = if self.section == Section::Diagnostics { 1152. } else { 896. };
        let error = store::error(cx).map(|error| {
            div().w_full().max_w(r(page_width)).px(r(24.)).child(
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
        let content = match archive_list {
            Some(list) => v_flex().flex_1().min_h_0().items_center().when_some(error, |this, error| this.child(div().w_full().flex().justify_center().pt(r(24.)).child(error))).child(list).into_any_element(),
            None => v_flex()
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
                .children(blocks.into_iter().map(|block| div().w_full().max_w(r(page_width)).px(r(24.)).child(block.element)))
                .into_any_element(),
        };
        let icon_picker = self.project_icon_picker_overlay(window, cx);
        let action_editor = self.action_editor_overlay(window, cx);
        let secret_dialog = self.secret_dialog_overlay(window, cx);
        let background_dialog = self.background_dialog_overlay(window, cx);
        h_flex()
            .relative()
            .size_full()
            .key_context("SettingsModal")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(|this, _: &crate::thread_view::FindInThread, window, cx| {
                cx.stop_propagation();
                this.on_focus_search(&FocusSettingsSearch, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNextInSettings, window, cx| this.cycle_focus(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPreviousInSettings, window, cx| this.cycle_focus(false, window, cx)))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(self.render_nav(layout, window, cx))
            .child(v_flex().flex_1().min_w_0().h_full().child(self.render_header(cx)).child(content))
            .children(icon_picker)
            .children(action_editor)
            .children(secret_dialog)
            .children(background_dialog)
            .children(self.appearance_ui.import.clone())
    }
}

#[cfg(feature="snapshot")]
pub(crate) fn import_theme_fixture(path:&std::path::Path,state:&Entity<AppState>,cx:&mut App)->Result<(),String> {
    let text=std::fs::read_to_string(path).map_err(|e|e.to_string())?;
    let theme=t3_core::themes::import(&text)?;
    appearance::import_theme(state,theme,true,cx);Ok(())
}
