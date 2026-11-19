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
mod environments;
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
mod voice;
mod worktrees;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, KeyBinding,
    KeyDownEvent, MouseButton, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Window, actions,
    div, point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::supervisor::ConnectionStatus;
use t3_core::server_config::ServerConfig;
use t3_core::settings::search::{self, Features, SearchAvailability, SearchItem, SearchScope, Section};
use t3_core::settings::{
    ALL_ENVIRONMENTS_VALUE, ALL_PROJECTS_VALUE, ResolvedScopeKind, ScopeCheckout, ScopeEnvironment,
    ScopeProjectGroup, ServerSettingChange, SettingsScopeSearch, is_device_only_section,
    project_scope_menu_groups, scoped_model_disabled_reason, select_checkout_axis,
    select_environment_axis, select_project_axis, server_change_to_override_edit,
    settings_scope_checkout_choices, settings_scope_checkout_label,
    settings_scope_environment_label,
};
use t3_core::sidebar::grouping::{EnvironmentOwner, ScopedProject};

use crate::app_state::{AppEvent, AppState};
use crate::i18n::{active_locale, t};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use controls::{
    Button, ButtonSize, ButtonVariant, ModalLayout, Select, SelectOption, focus_ring, group, icon,
    r,
};

actions!(settings, [OpenSettings, CloseSettings, FocusSettingsSearch, FocusNextInSettings, FocusPreviousInSettings]);

struct Host {
    state: Entity<AppState>,
    view: Option<Entity<SettingsView>>,
    pending: Option<(Section, Option<String>)>,
    /// The project a "Project settings" entry point opens the Projects page on.
    pending_project: Option<String>,
    /// The scheduled task an "Edit automation" / "Open automation" link opens the editor for once tasks load.
    pending_scheduled_task: Option<String>,
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
        pending_scheduled_task: None,
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

pub fn open_voice(cx: &mut App) {
    open_at(Section::Voice, Some("voice-transcription-destination".to_owned()), cx);
}

/// Opens Settings → Projects on the project holding `project_id`, from every "Project settings"
/// entry point (`openProjectSettings` in `Sidebar.tsx`, the command palette).
pub fn open_project(project_id: &str, cx: &mut App) {
    cx.global_mut::<Host>().pending_project = Some(project_id.to_owned());
    open_at(Section::Projects, None, cx);
}

/// Opens Settings → Scheduled tasks and opens the editor on `task_id` once loaded.
pub fn open_scheduled_task(task_id: &str, cx: &mut App) {
    cx.global_mut::<Host>().pending_scheduled_task = Some(task_id.to_owned());
    open_at(Section::ScheduledTasks, None, cx);
}

/// Opens Settings → Scheduled tasks without selecting a specific task.
pub fn open_scheduled_tasks(cx: &mut App) {
    cx.global_mut::<Host>().pending_scheduled_task = None;
    open_at(Section::ScheduledTasks, None, cx);
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

/// Computes `SearchAvailability` (`useAvailableSettingsSearchItems.ts`) from live app state.
fn search_availability_for(state: &AppState) -> SearchAvailability {
    let active_config = state.server_config.config.as_ref();
    let has_environment = active_config.is_some();
    let has_provider_settings_environment = active_config.is_some();
    let has_thread_auto_settlement = active_config.is_some();
    let has_peer_link_host = active_config.is_some()
        || !state.environments.environments.is_empty();
    let has_fast_worktree_checkout_environment = active_config
        .and_then(|c| c.environment.platform.as_ref())
        .is_some_and(|platform| platform.os == "darwin");
    let has_text_generators_environment = active_config.is_some_and(|c| {
        c.managed_policy
            .as_ref()
            .and_then(|policy| policy.pointer("/limits/providers/allowedDrivers"))
            .is_none()
    });
    let has_multiple_environments = state.environments.environments.len() >= 2;
    SearchAvailability {
        has_environment,
        has_provider_settings_environment,
        has_thread_auto_settlement,
        has_peer_link_host,
        has_fast_worktree_checkout_environment,
        has_text_generators_environment,
        has_multiple_environments,
        is_macos: cfg!(target_os = "macos"),
    }
}

/// The settings search results the command palette lists (`CommandPalette.tsx`).
pub fn palette_items(cx: &App) -> Vec<t3_core::palette::CommandPaletteItem> {
    let availability = search_availability_for(cx.global::<Host>().state.read(cx));
    let settings_label = t(cx, "Settings");
    search::available_items(features(cx), availability)
        .into_iter()
        .map(|item| {
            let localized_title = t(cx, &item.title).to_owned();
            let localized_section = t(cx, item.section.label()).to_owned();
            let mut search_terms = vec![
                item.title.clone(),
                item.section.label().to_owned(),
                localized_title.clone(),
                localized_section.clone(),
            ];
            search_terms.extend(item.terms.iter().cloned());
            t3_core::palette::CommandPaletteItem {
                value: format!("setting:{}", item.id),
                search_terms,
                description: Some(format!("{settings_label} \u{00b7} {localized_section}")),
                secondary: item.secondary,
                title: localized_title,
                ..Default::default()
            }
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
    host.pending_scheduled_task = None;
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
        if let Some(task_id) = cx.global_mut::<Host>().pending_scheduled_task.take() {
            view.update(cx, |view, cx| view.select_scheduled_task(&task_id, window, cx));
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

/// Collapsible settings section (`FoldedSettingsSection.tsx`): chevron toggle, title, muted
/// summary when collapsed, optional trailing control (e.g., master switch), highlight border on
/// search jump, and expanded divided group.
#[allow(clippy::too_many_arguments)]
pub(super) fn folded_section(
    id: &'static str,
    title: &'static str,
    summary: Option<String>,
    open: bool,
    highlighted: bool,
    control: Option<AnyElement>,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    rows: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let chevron = if open { Icon::ChevronDown } else { Icon::ChevronRight };
    let header_trigger = h_flex()
        .id(SharedString::from(format!("folded-section-toggle-{id}")))
        .flex_1()
        .min_w_0()
        .gap(r(8.))
        .items_center()
        .cursor_pointer()
        .on_click(on_toggle)
        .child(icon(chevron, 14., hsla(p.muted_foreground)))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(title),
        )
        .when_some(summary.filter(|s| !open && !s.is_empty()), |this, text| {
            this.child(
                div()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .truncate()
                    .child(text),
            )
        });
    v_flex()
        .id(SharedString::from(format!("folded-section-{id}")))
        .rounded(r(12.))
        .border_1()
        .border_color(if highlighted { hsla(p.primary) } else { hsla(p.border) })
        .bg(if highlighted { hsla(p.primary).opacity(0.05) } else { hsla(p.card) })
        .overflow_hidden()
        .child(
            h_flex()
                .min_h(r(44.))
                .px(r(16.))
                .py(r(10.))
                .gap(r(12.))
                .items_center()
                .justify_between()
                .when(open, |this| this.border_b_1().border_color(hsla(p.border)))
                .child(header_trigger)
                .children(control),
        )
        .when(open, |this| this.child(group(rows, cx)))
        .into_any_element()
}

pub struct SettingsView {
    state: Entity<AppState>,
    focus: FocusHandle,
    section: Section,
    /// The row a search jump landed on, tinted until the next navigation.
    target: Option<String>,
    scroll_to_target: bool,
    /// Scoped settings selection (`SettingsScopeSentence.tsx`, `useScopedSettings.ts`).
    scope: SettingsScopeSearch,
    /// Last active UI locale applied to the search input placeholder.
    last_locale: &'static str,
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
    pub(crate) environments: environments::EnvironmentsState,
    pub(crate) voice: voice::VoiceState,
    chat_ui: general::ChatState,
    about: about::AboutState,
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
        let locale = active_locale(cx);
        let search_placeholder = t(cx, "Search").to_owned();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(search_placeholder));
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
            environments: environments::EnvironmentsState::new(),
            voice: voice::VoiceState::new(window, cx),
            chat_ui: general::ChatState::default(),
            about: about::AboutState::new(window, cx),
            browser: diffs_browser::BrowserState::new(window, cx),
            typography: typography::TypographyState::new(window, cx),
            appearance_ui: appearance::AppearanceState::new(cx),
            state,
            focus: cx.focus_handle(),
            section,
            target: None,
            scroll_to_target: false,
            scope: SettingsScopeSearch::default(),
            last_locale: locale,
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
        let target = if section == Section::Voice {
            target.map(|id| self.resolve_voice_target(&id, cx))
        } else {
            target
        };
        self.scroll_to_target = target.is_some();
        if let Some(target) = target.as_deref() {
            if matches!(target, "prompt-font" | "terminal-font") && !store::client(cx).separate_fonts() {
                store::update_client(&self.state, t3_core::client_settings::ClientSettingChange::SeparateFonts(true), cx);
            }
            self.worktrees.reveal(target);
            self.source_control.reveal(target);
            self.environments.reveal(target);
            self.voice.reveal(target);
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
        if section != Section::Worktrees {
            self.worktrees.leave();
        }
        if section != Section::About {
            self.about.leave();
        }
        if section != Section::Connections {
            self.environments.leave();
        }
        if section != Section::Voice {
            self.leave_voice(cx);
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
        let state = self.state.read(cx);
        let mut items = search::available_items(features(cx), search_availability_for(state));
        let mac = cfg!(target_os = "macos");
        if let Some(config) = state.server_config.config.as_ref() {
            for row in t3_core::settings::keybindings::rows(&config.keybindings, mac) {
                items.push(search::keybinding_item(&row.command, &row.label, &row.shortcuts));
            }
        }
        items
    }

    fn open_active_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.search_items(cx);
        let query = self.query(cx);
        let results = search::search_with_locale(&query, &items, active_locale(cx));
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
        if self.dismiss_about_overlay(window, cx)
            || self.dismiss_auto_settle_days(window, cx)
            || self.dismiss_environments_overlay(window, cx)
            || self.dismiss_voice_overlay(window, cx)
        {
            return;
        }
        if self.archive.dismiss()
            || self.projects.actions.dismiss()
            || self.secrets.dismiss()
            || self.maintenance.dismiss()
            || self.dismiss_maintenance_retention(window, cx)
            || self.dismiss_worktree_retention(window, cx)
        {
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
        self.leave_voice(cx);
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
        self.commit_focused_voice_field(window, cx);
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
                let count = search::search_with_locale(&this.query(cx), &this.search_items(cx), active_locale(cx)).len().max(1);
                this.active_result = if key == "down" { (this.active_result + 1) % count } else { (this.active_result + count - 1) % count };
                cx.notify();
            });
        })
    }

    fn text_field_focused(&self, window: &Window, cx: &App) -> bool {
        if self.typography.text_field_focused(window, cx) || self.projects.picker.as_ref().is_some_and(|picker| picker.text_field_focused(window, cx)) {
            return true;
        }
        [
            &self.search,
            &self.threads.base_directory,
            &self.threads.auto_settle_days,
            &self.browser.custom_url,
            &self.about.licenses_search,
            &self.keybindings.filter,
            &self.keybindings.when_input,
        ]
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
            .chain(self.environments.inputs())
            .chain(self.voice.inputs())
            .any(|input| input.focus_handle(cx).is_focused(window))
            || [&self.threads.title_instructions, &self.worktrees.instructions, self.scheduled.textarea(), &self.projects.actions.command]
                .into_iter()
                .chain(self.source_control.textareas())
                .chain(self.voice.textareas())
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
                    .tooltip(t(cx, "Clear settings search"))
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
                    div().px(r(8.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_muted_foreground).opacity(0.8)).child(t(cx, heading))
                }))
                .children(sections.into_iter().map(|section| {
                    let active = section == current;
                    let color = if active { hsla(p.sidebar_foreground) } else { hsla(p.sidebar_muted_foreground).opacity(0.8) };
                    let focus = self.section_focus.iter().find(|(candidate, _)| *candidate == section).map(|(_, handle)| handle.clone().tab_stop(section == tab_stop));
                    let ring = keyboard && focus.as_ref().is_some_and(|focus| focus.is_focused(window));
                    let section_label = t(cx, section.label());
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
                        .when(labels, |this| this.child(div().truncate().child(section_label)))
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
        let locale = active_locale(cx);
        let results = search::search_with_locale(query, &items, locale);
        if results.is_empty() {
            return div()
                .px(r(8.))
                .py(r(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.sidebar_muted_foreground))
                .child(t(cx, "No settings found"))
                .into_any_element();
        }
        let active = self.active_result.min(results.len() - 1);
        v_flex()
            .gap(r(2.))
            .children(results.into_iter().enumerate().map(|(index, item)| {
                let (section, anchor) = (item.section, item.anchor().to_owned());
                let highlighted = index == active;
                let title = t(cx, &item.title).to_owned();
                let section_label = t(cx, section.label());
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
                            .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.sidebar_foreground)).child(title))
                            .child(div().truncate().text_size(r(11.)).text_color(hsla(p.sidebar_muted_foreground).opacity(0.75)).child(section_label)),
                    )
            }))
            .into_any_element()
    }

    fn single_environment_section(&self) -> bool {
        matches!(self.section, Section::Providers | Section::Secrets)
    }

    fn scope_environments(&self, cx: &App) -> Vec<ScopeEnvironment> {
        let state = self.state.read(cx);
        let mut envs: Vec<ScopeEnvironment> = Vec::new();
        for env in &state.environments.environments {
            let fleet_entry = state.fleet.environments.iter().find(|e| e.summary.owner.saved_id == env.id);
            let connected = fleet_entry
                .is_some_and(|e| e.status == ConnectionStatus::Connected && e.config.config.is_some())
                || (state.environments.active.as_deref() == Some(env.id.as_str())
                    && state.server_config.config.is_some());
            let env_id = fleet_entry
                .and_then(|e| e.summary.owner.canonical_id.clone())
                .or_else(|| env.environment_id.clone())
                .unwrap_or_else(|| env.id.clone());
            let label = fleet_entry
                .and_then(|e| e.config.config.as_ref())
                .map(|c| c.environment.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| env.label.clone());
            envs.push(ScopeEnvironment {
                environment_id: env_id,
                label,
                display_url: env.http_base.clone(),
                connected,
            });
        }
        if envs.is_empty()
            && let Some(cfg) = state.server_config.config.as_ref()
        {
            envs.push(ScopeEnvironment {
                environment_id: cfg.environment.environment_id.clone(),
                label: cfg.environment.label.clone(),
                display_url: String::new(),
                connected: true,
            });
        }
        envs
    }

    fn all_scope_project_groups(&self, cx: &App) -> Vec<ScopeProjectGroup> {
        self.project_groups(cx)
            .into_iter()
            .filter(|group| !group.is_scratch)
            .filter_map(|group| {
                let members: Vec<ScopeCheckout> = group
                    .members
                    .into_iter()
                    .map(|member| ScopeCheckout {
                        environment_id: member.project.owner.identity(),
                        physical_project_key: member.physical_key,
                        workspace_root: member.project.project.workspace_root,
                    })
                    .collect();
                let workspace_root = members.first()?.workspace_root.clone();
                Some(ScopeProjectGroup {
                    project_key: group.key,
                    display_name: group.label,
                    workspace_root,
                    archived: group.archived,
                    members,
                })
            })
            .collect()
    }

    /// Resolves `self.scope` into a `ResolvedScopeKind` (`resolveSettingsScope`).
    pub(super) fn resolved_scope_kind(&self, cx: &App) -> ResolvedScopeKind {
        let envs = self.scope_environments(cx);
        let groups = self.all_scope_project_groups(cx);
        if let Some(project_key) = self.scope.project.as_deref() {
            let Some(group) = groups.iter().find(|g| g.project_key == project_key) else {
                return ResolvedScopeKind::Unavailable;
            };
            if let Some(checkout_key) = self.scope.checkout.as_deref() {
                if group.members.iter().any(|m| m.physical_project_key == checkout_key) {
                    return ResolvedScopeKind::Checkout;
                }
                return ResolvedScopeKind::Unavailable;
            }
            if let Some(machine) = self.scope.machine.as_deref()
                && !group.members.iter().any(|m| m.environment_id == machine)
            {
                return ResolvedScopeKind::Unavailable;
            }
            return ResolvedScopeKind::Project;
        }
        if let Some(machine) = self.scope.machine.as_deref() {
            if envs.iter().any(|e| e.environment_id == machine) {
                return ResolvedScopeKind::Environment;
            }
            return ResolvedScopeKind::Unavailable;
        }
        if self.single_environment_section() || envs.len() <= 1 {
            if envs.is_empty() { ResolvedScopeKind::Unavailable } else { ResolvedScopeKind::Environment }
        } else {
            ResolvedScopeKind::All
        }
    }

    /// Connected fleet environment owners targeted by `self.scope` (`selectScopedSettingsEnvironments`).
    pub(super) fn scoped_environment_owners(&self, cx: &App) -> Vec<EnvironmentOwner> {
        let state = self.state.read(cx);
        let connected: Vec<_> = state
            .fleet
            .environments
            .iter()
            .filter(|e| e.status == ConnectionStatus::Connected && e.config.config.is_some())
            .collect();
        if connected.is_empty() {
            return Vec::new();
        }
        if self.scope.project.is_some() {
            let mut owners: Vec<EnvironmentOwner> = Vec::new();
            for member in self.scoped_project_members(cx) {
                if !owners.contains(&member.owner) {
                    owners.push(member.owner);
                }
            }
            return owners;
        }
        if let Some(machine) = self.scope.machine.as_deref() {
            return connected
                .into_iter()
                .filter(|e| e.summary.owner.identity() == machine || e.summary.owner.saved_id == machine)
                .map(|e| e.summary.owner.clone())
                .collect();
        }
        if self.single_environment_section() {
            let active = state.environments.active.as_deref();
            if let Some(entry) = connected
                .iter()
                .find(|e| active.is_some_and(|id| e.summary.owner.saved_id == id))
                .or_else(|| connected.first())
            {
                return vec![entry.summary.owner.clone()];
            }
            return Vec::new();
        }
        connected.into_iter().map(|e| e.summary.owner.clone()).collect()
    }

    /// Physical checkouts targeted when `self.scope` names a project or checkout.
    pub(super) fn scoped_project_members(&self, cx: &App) -> Vec<ScopedProject> {
        let Some(project_key) = self.scope.project.as_deref() else {
            return Vec::new();
        };
        let groups = self.project_groups(cx);
        let Some(group) = groups.iter().find(|g| g.key == project_key) else {
            return Vec::new();
        };
        group
            .members
            .iter()
            .filter(|member| {
                self.scope.machine.as_deref().is_none_or(|m| {
                    member.project.owner.identity() == m || member.project.owner.saved_id == m
                }) && self
                    .scope
                    .checkout
                    .as_deref()
                    .is_none_or(|c| member.physical_key == c)
            })
            .map(|member| member.project.clone())
            .collect()
    }

    pub(super) fn is_project_scope(&self, cx: &App) -> bool {
        matches!(self.resolved_scope_kind(cx), ResolvedScopeKind::Project | ResolvedScopeKind::Checkout)
    }

    /// Inert notice for environment-wide settings when the user is viewing Project/Checkout scope
    /// (`settingsLayout.tsx:371, 447-448`), or standard connection/permission unavailability.
    pub(super) fn env_only_unavailable(&self, cx: &App) -> Option<String> {
        if let Some(reason) = self.settings_unavailable(cx) {
            return Some(reason);
        }
        self.is_project_scope(cx)
            .then(|| "Environment-wide setting. Select an environment to change it.".to_owned())
    }

    /// Primary single environment owner for single-environment RPCs/pages (prefers selected
    /// machine or active environment in scope).
    pub(super) fn scoped_primary_owner(
        &self,
        cx: &App,
    ) -> Option<t3_core::sidebar::grouping::EnvironmentOwner> {
        let state = self.state.read(cx);
        let owners = self.scoped_environment_owners(cx);
        if owners.is_empty() {
            return None;
        }
        let active_saved = state.environments.active.as_deref();
        let chosen = if self.scope.machine.is_some() {
            &owners[0]
        } else {
            owners
                .iter()
                .find(|owner| active_saved.is_some_and(|id| owner.saved_id == id))
                .unwrap_or(&owners[0])
        };
        Some(chosen.clone())
    }

    /// All `ServerConfig`s targeted by the current scope (for `.all(...)` capability checks).
    pub(super) fn scoped_all_configs(&self, cx: &App) -> Vec<ServerConfig> {
        let state = self.state.read(cx);
        let owners = self.scoped_environment_owners(cx);
        let mut out = Vec::new();
        for owner in owners {
            if let Some(cfg) = state
                .fleet
                .environments
                .iter()
                .find(|e| e.summary.owner == owner)
                .and_then(|e| e.config.config.clone())
            {
                out.push(cfg);
            }
        }
        if out.is_empty()
            && let Some(cfg) = state.server_config.config.clone()
        {
            out.push(cfg);
        }
        out
    }

    /// Raw representative `ServerConfig` for the current scope (without project override overlay).
    pub(super) fn scoped_raw_representative_config(&self, cx: &App) -> Option<ServerConfig> {
        let state = self.state.read(cx);
        if let Some(chosen) = self.scoped_primary_owner(cx)
            && let Some(cfg) = state
                .fleet
                .environments
                .iter()
                .find(|e| e.summary.owner == chosen)
                .and_then(|e| e.config.config.clone())
        {
            return Some(cfg);
        }
        state.server_config.config.clone()
    }

    /// Representative `ServerConfig` for display in scoped panels (`selectScopedSettingsEnvironments`),
    /// resolving `projectSettingsOverrides[projectId]` when in Project/Checkout scope.
    pub(super) fn scoped_representative_config(&self, cx: &App) -> Option<ServerConfig> {
        let mut cfg = self.scoped_raw_representative_config(cx)?;
        if self.is_project_scope(cx)
            && let Some(project_id) = self.scope_project_id(cx)
        {
            cfg.settings = t3_core::settings::resolve_project_settings(&cfg.settings, &project_id);
        }
        Some(cfg)
    }

    /// Whether the active Project/Checkout scope has an explicit override stored for any of `keys`.
    pub(super) fn has_project_override(&self, keys: &[&str], cx: &App) -> bool {
        if !self.is_project_scope(cx) {
            return false;
        }
        let state = self.state.read(cx);
        let members = self.scoped_project_members(cx);
        if !members.is_empty() {
            return members.iter().any(|member| {
                state
                    .scoped_environment(&member.owner.saved_id)
                    .and_then(|e| e.config.config.as_ref())
                    .is_some_and(|cfg| {
                        t3_core::settings::project_override_present(
                            &cfg.settings,
                            &member.project.id,
                            keys,
                        )
                    })
            });
        }
        if let Some(project_id) = self.scope_project_id(cx)
            && let Some(cfg) = self.scoped_raw_representative_config(cx)
        {
            return t3_core::settings::project_override_present(&cfg.settings, &project_id, keys);
        }
        false
    }

    /// Clears `keys` from `projectSettingsOverrides` across every project member in scope
    /// (`useClearScopedSettings` / "Reset to inherited value").
    pub(super) fn clear_project_overrides(&self, keys: &[&str], cx: &mut App) {
        let members = self.scoped_project_members(cx);
        if members.is_empty() {
            return;
        }
        let edit = t3_core::settings::projects::OverrideEdit::Clear {
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        };
        store::update_projects(&self.state, members, edit, |_, _| {}, cx);
    }

    /// Wires `Row`'s reset button for a project-scoped setting: in Project/Checkout scope it
    /// appears when any of `keys` is explicitly overridden and clears the project override
    /// ("Reset to inherited value"); otherwise it resets to the environment default.
    pub(super) fn apply_scoped_reset(
        &self,
        row: controls::Row,
        keys: &'static [&'static str],
        env_customized: bool,
        env_reset: ServerSettingChange,
        cx: &Context<Self>,
    ) -> controls::Row {
        let view = cx.entity().downgrade();
        if self.is_project_scope(cx) {
            row.reset_tooltip("Reset to inherited value").reset(
                self.has_project_override(keys, cx),
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.clear_project_overrides(keys, cx));
                },
            )
        } else {
            row.reset(env_customized, move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.update_scoped_server(env_reset.clone(), cx));
            })
        }
    }

    /// Sends a unary RPC to the single scoped environment (falling back to the active client).
    pub(super) fn scoped_rpc(
        &self,
        scope: &'static str,
        method: &'static str,
        payload: serde_json::Value,
        cx: &App,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, t3_client::session::RpcError>> + use<> {
        let app = self.state.read(cx);
        let owner = self.scoped_primary_owner(cx);
        let active_id = app.environments.active.as_deref();
        let use_fleet = owner
            .as_ref()
            .is_some_and(|o| active_id != Some(o.saved_id.as_str()));
        let lookup = use_fleet
            .then(|| owner.as_ref().map(|o| app.connections.destination(o, scope)))
            .flatten();
        let runtime = app.runtime().clone();
        let fallback = lookup.is_none().then(|| app.rpc(method, payload.clone()));
        async move {
            if let Some(lookup) = lookup {
                let dest = lookup.await.map_err(|_| t3_client::session::RpcError::Closed)?;
                let client = dest.client;
                runtime
                    .spawn(async move { client.request(method, payload).await })
                    .await
                    .unwrap_or(Err(t3_client::session::RpcError::Closed))
            } else if let Some(fallback) = fallback {
                fallback.await
            } else {
                Err(t3_client::session::RpcError::Closed)
            }
        }
    }

    /// Session grants for the scoped primary environment (falls back to active session).
    pub(super) fn scoped_session(&self, cx: &App) -> Option<t3_core::auth::AuthSessionState> {
        let state = self.state.read(cx);
        if let Some(chosen) = self.scoped_primary_owner(cx)
            && let Some(grants) = state
                .fleet
                .environments
                .iter()
                .find(|e| e.summary.owner == chosen)
                .and_then(|e| e.grants.clone())
        {
            return Some(grants);
        }
        self.session(cx)
    }

    /// Whether the scoped primary environment is currently connected.
    pub(super) fn scoped_connected_now(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        if let Some(chosen) = self.scoped_primary_owner(cx)
            && let Some(entry) = state
                .fleet
                .environments
                .iter()
                .find(|e| e.summary.owner == chosen)
        {
            return entry.status == ConnectionStatus::Connected && entry.config.config.is_some();
        }
        self.connected_now(cx)
    }

    /// All `(environment_label, ServerConfig, Option<project_id>)` targets currently in scope, for
    /// `scoped_model_disabled_reason` (`useScopedModelAvailability.ts`).
    pub(super) fn scoped_model_targets(&self, cx: &App) -> Vec<(String, ServerConfig, Option<String>)> {
        let state = self.state.read(cx);
        if self.scope.project.is_some() {
            let members = self.scoped_project_members(cx);
            let mut out = Vec::new();
            for member in members {
                if let Some(cfg) = state
                    .scoped_environment(&member.owner.saved_id)
                    .and_then(|e| e.config.config.clone())
                {
                    let label = if !cfg.environment.label.is_empty() {
                        cfg.environment.label.clone()
                    } else {
                        member.environment_label.clone()
                    };
                    out.push((label, cfg, Some(member.project.id.clone())));
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        let owners = self.scoped_environment_owners(cx);
        if !owners.is_empty() {
            let mut out = Vec::new();
            for owner in owners {
                if let Some(cfg) = state
                    .fleet
                    .environments
                    .iter()
                    .find(|e| e.summary.owner == owner)
                    .and_then(|e| e.config.config.clone())
                {
                    let label = if !cfg.environment.label.is_empty() {
                        cfg.environment.label.clone()
                    } else {
                        owner.saved_id.clone()
                    };
                    out.push((label, cfg, None));
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(cfg) = state.server_config.config.clone() {
            let label = if !cfg.environment.label.is_empty() {
                cfg.environment.label.clone()
            } else {
                "this environment".to_owned()
            };
            return vec![(label, cfg, None)];
        }
        Vec::new()
    }

    /// Checks whether `(instance_id, model)` can be applied across every target in `self.scope`
    /// (`useScopedModelDisabledReason` in `useScopedModelAvailability.ts`).
    pub(super) fn scoped_model_disabled_reason(
        &self,
        shown: &ServerConfig,
        instance_id: &str,
        model: &str,
        text_generation: bool,
        cx: &App,
    ) -> Option<String> {
        let targets = self.scoped_model_targets(cx);
        if targets.len() <= 1 && self.scope.project.is_none() {
            return None;
        }
        let target_refs: Vec<(&str, &ServerConfig, Option<&str>)> = targets
            .iter()
            .map(|(label, cfg, pid)| (label.as_str(), cfg, pid.as_deref()))
            .collect();
        let shown_driver = shown.provider(instance_id).map(|p| p.driver.as_str());
        scoped_model_disabled_reason(
            &shown.settings,
            shown_driver,
            &target_refs,
            instance_id,
            model,
            text_generation,
        )
    }

    /// Saves a server setting across the active settings scope (`useUpdateScopedSettings`):
    /// fans out across every connected environment in `"all"` scope, targets the chosen machine
    /// in `"environment"` scope, or writes project overrides in `"project"` / `"checkout"` scope.
    pub(super) fn update_scoped_server(&self, change: ServerSettingChange, cx: &mut App) {
        self.update_scoped_server_with(change, None, cx);
    }

    pub(super) fn update_scoped_server_with(
        &self,
        change: ServerSettingChange,
        on_failed: Option<store::OnFailed>,
        cx: &mut App,
    ) {
        let kind = self.resolved_scope_kind(cx);
        if matches!(kind, ResolvedScopeKind::Project | ResolvedScopeKind::Checkout) {
            let members = self.scoped_project_members(cx);
            let Some(edit) = server_change_to_override_edit(&change) else {
                store::show_error(
                    "This setting applies at environment scope. Choose an environment to change it.",
                    cx,
                );
                if let Some(on_failed) = on_failed {
                    on_failed(cx);
                }
                return;
            };
            if members.is_empty() {
                store::show_error("The selected project is unavailable.", cx);
                if let Some(on_failed) = on_failed {
                    on_failed(cx);
                }
                return;
            }
            store::update_projects(
                &self.state,
                members,
                edit,
                move |error, cx| {
                    if error.is_some()
                        && let Some(on_failed) = on_failed
                    {
                        on_failed(cx);
                    }
                },
                cx,
            );
            return;
        }
        let owners = self.scoped_environment_owners(cx);
        let active_saved = self.state.read(cx).environments.active.clone();
        let fan_out = owners.len() > 1
            || (owners.len() == 1 && active_saved.as_deref() != Some(owners[0].saved_id.as_str()));
        if fan_out {
            store::update_scoped_environments(&self.state, owners, change, on_failed, cx);
        } else {
            store::update_server_with(&self.state, change, on_failed, cx);
        }
    }

    /// Saves a setting on the single scoped environment (for `Providers`, `Secrets`, `SourceControl`
    /// host setup) and invokes `on_done` once settled.
    pub(super) fn update_scoped_single_env(&self, change: ServerSettingChange, cx: &mut App) {
        self.update_scoped_single_env_then(change, |_, _| {}, cx);
    }

    pub(super) fn update_scoped_single_env_then(
        &self,
        change: ServerSettingChange,
        on_done: impl FnOnce(Option<String>, &mut App) + 'static,
        cx: &mut App,
    ) {
        let active_saved = self.state.read(cx).environments.active.clone();
        if let Some(owner) = self.scoped_primary_owner(cx)
            && active_saved.as_deref() != Some(owner.saved_id.as_str())
        {
            store::update_scoped_environments_then(&self.state, vec![owner], change, on_done, cx);
        } else {
            store::update_server_then(&self.state, change, on_done, cx);
        }
    }

    pub(super) fn scope_project_id(&self, cx: &App) -> Option<String> {
        let project_key = self.scope.project.as_deref()?;
        let groups = self.project_groups(cx);
        let group = groups.iter().find(|g| g.key == project_key)?;
        if let Some(checkout) = self.scope.checkout.as_deref()
            && let Some(member) = group.members.iter().find(|m| m.physical_key == checkout)
        {
            return Some(member.project.project.id.clone());
        }
        // Prefer the scoped primary machine (selected machine or active env), then any member.
        if let Some(primary) = self.scoped_primary_owner(cx)
            && let Some(member) = group
                .members
                .iter()
                .find(|m| m.project.owner == primary || m.project.owner.saved_id == primary.saved_id)
        {
            return Some(member.project.project.id.clone());
        }
        if let Some(machine) = self.scope.machine.as_deref()
            && let Some(member) = group.members.iter().find(|m| {
                m.project.owner.identity() == machine || m.project.owner.saved_id == machine
            })
        {
            return Some(member.project.project.id.clone());
        }
        let state = self.state.read(cx);
        let active_saved_id = state.environments.active.as_deref();
        if let Some(active) = active_saved_id
            && let Some(member) = group.members.iter().find(|m| m.project.owner.saved_id == active)
        {
            return Some(member.project.project.id.clone());
        }
        group.members.first().map(|m| m.project.project.id.clone())
    }

    pub(super) fn scope_project_label(&self, cx: &App) -> Option<String> {
        let project_key = self.scope.project.as_deref()?;
        self.all_scope_project_groups(cx)
            .into_iter()
            .find(|g| g.project_key == project_key)
            .map(|g| g.display_name)
    }

    /// `SettingsScopeSentence.tsx`: rendered on every server-scoped settings page (all
    /// non-device-only sections except Projects, which embeds its own synced project picker).
    fn render_scope_sentence(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if is_device_only_section(self.section) || self.section == Section::Projects {
            return None;
        }
        let envs = self.scope_environments(cx);
        let all_groups = self.all_scope_project_groups(cx);
        if envs.is_empty() && all_groups.is_empty() {
            return None;
        }
        let p = palette(cx);
        let single_env = self.single_environment_section() || envs.len() <= 1;
        let visible_groups = project_scope_menu_groups(&all_groups, self.scope.machine.as_deref());
        let selected_project = self
            .scope
            .project
            .as_deref()
            .and_then(|key| all_groups.iter().find(|g| g.project_key == key));
        let project_value = selected_project
            .map(|g| g.project_key.clone())
            .unwrap_or_else(|| ALL_PROJECTS_VALUE.to_owned());
        let mut project_options = vec![SelectOption::new(ALL_PROJECTS_VALUE, "All projects")];
        for g in &visible_groups {
            let label = if g.archived {
                format!("{} (archived)", g.display_name)
            } else {
                g.display_name.clone()
            };
            project_options.push(SelectOption::new(g.project_key.clone(), label));
        }

        let env_options_source: Vec<&ScopeEnvironment> = if let Some(project) = selected_project {
            envs.iter()
                .filter(|e| project.members.iter().any(|m| m.environment_id == e.environment_id))
                .collect()
        } else {
            envs.iter().collect()
        };
        let env_value = self
            .scope
            .machine
            .as_deref()
            .filter(|id| env_options_source.iter().any(|e| e.environment_id == *id))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if single_env {
                    env_options_source
                        .first()
                        .map(|e| e.environment_id.clone())
                        .unwrap_or_else(|| ALL_ENVIRONMENTS_VALUE.to_owned())
                } else {
                    ALL_ENVIRONMENTS_VALUE.to_owned()
                }
            });
        let mut env_options = Vec::new();
        if !single_env {
            env_options.push(SelectOption::new(ALL_ENVIRONMENTS_VALUE, "All environments"));
        }
        for env in &env_options_source {
            let mut label = settings_scope_environment_label(env, &envs);
            if !env.connected {
                label.push_str(" (Offline)");
            }
            env_options.push(SelectOption::new(env.environment_id.clone(), label));
        }
        let env_preposition = if self.scope.machine.is_some()
            || self.resolved_scope_kind(cx) == ResolvedScopeKind::Checkout
            || single_env
        {
            "on"
        } else {
            "across"
        };

        let checkouts: Vec<ScopeCheckout> = selected_project
            .map(|g| {
                settings_scope_checkout_choices(g, &self.scope)
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let checkout_refs: Vec<&ScopeCheckout> = checkouts.iter().collect();
        let checkout_select = (!checkouts.is_empty()).then(|| {
            let mut opts = vec![SelectOption::new("__all__", "All checkouts")];
            for ch in &checkouts {
                opts.push(SelectOption::new(
                    ch.physical_project_key.clone(),
                    settings_scope_checkout_label(ch, &checkout_refs),
                ));
            }
            let selected_checkout = self
                .scope
                .checkout
                .as_deref()
                .filter(|k| checkouts.iter().any(|c| c.physical_project_key == *k))
                .unwrap_or("__all__")
                .to_owned();
            let checkouts_for_cb = checkouts.clone();
            let weak = cx.entity().downgrade();
            h_flex()
                .gap(r(6.))
                .items_center()
                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child("in"))
                .child(
                    Select::new("settings-scope-checkout", opts)
                        .selected(selected_checkout)
                        .width(160.)
                        .on_change(move |value, _, cx| {
                            let chosen = checkouts_for_cb
                                .iter()
                                .find(|c| c.physical_project_key == value)
                                .cloned();
                            let _ = weak.update(cx, |this, cx| {
                                this.scope = select_checkout_axis(&this.scope, chosen.as_ref());
                                cx.notify();
                            });
                        }),
                )
        });

        let weak_proj = cx.entity().downgrade();
        let weak_env = cx.entity().downgrade();
        Some(
            h_flex()
                .id("settings-scope-sentence")
                .debug_selector(|| "settings-scope-sentence".to_owned())
                .flex_wrap()
                .gap(r(6.))
                .items_center()
                .px(r(12.))
                .py(r(8.))
                .rounded(r(10.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.card))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Applying settings for"),
                )
                .child(
                    Select::new("settings-scope-project", project_options)
                        .selected(project_value)
                        .width(168.)
                        .on_change(move |value, _, cx| {
                            let _ = weak_proj.update(cx, |this, cx| {
                                this.scope = select_project_axis(&this.scope, &value);
                                this.projects.selected = this.scope.project.clone();
                                cx.notify();
                            });
                        }),
                )
                .when(!env_options.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(env_preposition),
                    )
                    .child(
                        Select::new("settings-scope-environment", env_options)
                            .selected(env_value)
                            .width(168.)
                            .on_change(move |value, _, cx| {
                                let _ = weak_env.update(cx, |this, cx| {
                                    this.scope = select_environment_axis(&this.scope, &value);
                                    cx.notify();
                                });
                            }),
                    )
                })
                .children(checkout_select)
                .into_any_element(),
        )
    }

    /// `SettingsScopeBoundary` (`_chat.settings.tsx:71-87`): when a search jump lands on a target
    /// whose required `SearchScope` is unavailable in the current scope, prompt the user to switch
    /// to an eligible scope (`is_search_scope_available`).
    fn render_search_scope_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target_id = self.target.as_deref()?;
        let scope_kind = self.resolved_scope_kind(cx);
        if scope_kind == ResolvedScopeKind::Unavailable {
            return None;
        }
        let items = self.search_items(cx);
        let item = items
            .iter()
            .find(|candidate| candidate.id == target_id)
            .or_else(|| items.iter().find(|candidate| candidate.anchor() == target_id))?;
        if search::is_search_scope_available(item.scope, scope_kind) {
            return None;
        }
        let p = palette(cx);
        let title = t(cx, &item.title).to_owned();
        let message = format!("{title} is not available for the selected target. Choose its owning scope to continue.");
        let mut choice_rows: Vec<AnyElement> = Vec::new();
        let weak = cx.entity().downgrade();
        match item.scope {
            Some(SearchScope::Environment) => {
                let envs = self.scope_environments(cx);
                for env in &envs {
                    let env_id = env.environment_id.clone();
                    let label = settings_scope_environment_label(env, &envs);
                    let weak = weak.clone();
                    choice_rows.push(
                        h_flex()
                            .id(SharedString::from(format!("scope-notice-env-{env_id}")))
                            .w_full()
                            .px(r(16.))
                            .py(r(12.))
                            .gap(r(10.))
                            .items_center()
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent).opacity(0.5)))
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.scope = SettingsScopeSearch {
                                        project: None,
                                        machine: Some(env_id.clone()),
                                        checkout: None,
                                    };
                                    this.scroll_to_target = true;
                                    cx.notify();
                                });
                            })
                            .child(icon(Icon::Server, 16., hsla(p.muted_foreground)))
                            .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(label))
                            .child(icon(Icon::ChevronRight, 16., hsla(p.muted_foreground)))
                            .into_any_element(),
                    );
                }
            }
            Some(SearchScope::Project | SearchScope::Checkout) => {
                for group in self.all_scope_project_groups(cx) {
                    let project_key = group.project_key.clone();
                    let label = group.display_name.clone();
                    let weak = weak.clone();
                    choice_rows.push(
                        h_flex()
                            .id(SharedString::from(format!("scope-notice-project-{project_key}")))
                            .w_full()
                            .px(r(16.))
                            .py(r(12.))
                            .gap(r(10.))
                            .items_center()
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent).opacity(0.5)))
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.scope = SettingsScopeSearch {
                                        project: Some(project_key.clone()),
                                        machine: None,
                                        checkout: None,
                                    };
                                    this.projects.selected = Some(project_key.clone());
                                    this.scroll_to_target = true;
                                    cx.notify();
                                });
                            })
                            .child(icon(Icon::Folder, 16., hsla(p.muted_foreground)))
                            .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(label))
                            .child(icon(Icon::ChevronRight, 16., hsla(p.muted_foreground)))
                            .into_any_element(),
                    );
                }
            }
            _ => {
                choice_rows.push(
                    h_flex()
                        .id("scope-notice-all-environments")
                        .w_full()
                        .px(r(16.))
                        .py(r(12.))
                        .gap(r(10.))
                        .items_center()
                        .cursor_pointer()
                        .hover(|this| this.bg(hsla(p.accent).opacity(0.5)))
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.scope = SettingsScopeSearch::default();
                                this.scroll_to_target = true;
                                cx.notify();
                            });
                        })
                        .child(icon(Icon::Server, 16., hsla(p.muted_foreground)))
                        .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Open all environments"))
                        .child(icon(Icon::ChevronRight, 16., hsla(p.muted_foreground)))
                        .into_any_element(),
                );
            }
        }
        Some(
            v_flex()
                .id("settings-scope-notice")
                .debug_selector(|| "settings-scope-notice".to_owned())
                .gap(r(12.))
                .child(div().px(r(12.)).text_sm().text_color(hsla(p.muted_foreground)).child(message))
                .when(!choice_rows.is_empty(), |this| this.child(group(choice_rows, cx)))
                .into_any_element(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let licenses_open = self.section == Section::About && self.about.licenses_open;
        let nav_parent = self.section.nav_parent();
        let has_detail_parent = nav_parent != self.section;
        let section_label = t(cx, self.section.label());
        let parent_label = t(cx, nav_parent.label());
        let settings_label = t(cx, "Settings");
        let licenses_label = t(cx, "Open source licenses");
        let section_crumb = if licenses_open {
            div()
                .id("settings-breadcrumb-about")
                .cursor_pointer()
                .text_color(hsla(p.muted_foreground))
                .hover(|this| this.text_color(hsla(p.foreground)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.about.leave();
                    cx.notify();
                }))
                .child(section_label)
                .into_any_element()
        } else if has_detail_parent {
            div()
                .id("settings-breadcrumb-parent")
                .cursor_pointer()
                .text_color(hsla(p.muted_foreground))
                .hover(|this| this.text_color(hsla(p.foreground)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.navigate(nav_parent, None, cx);
                }))
                .child(parent_label)
                .into_any_element()
        } else {
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .child(section_label)
                .into_any_element()
        };
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
                    .child(div().text_color(hsla(p.muted_foreground)).child(settings_label))
                    .child(div().text_color(hsla(p.icon_muted)).child("/"))
                    .child(section_crumb)
                    .when(has_detail_parent, |this| {
                        this.child(div().text_color(hsla(p.icon_muted)).child("/"))
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(section_label))
                    })
                    .when(licenses_open, |this| {
                        this.child(div().text_color(hsla(p.icon_muted)).child("/"))
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(licenses_label))
                    }),
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
            Section::ScheduledTasks => self.scheduled_tasks_page(window, cx),
            Section::Projects => self.projects_page(window, cx),
            Section::Access => self.access_page(window, cx),
            Section::Secrets => self.secrets_page(cx),
            Section::Maintenance => self.maintenance_page(window, cx),
            Section::Diagnostics => self.diagnostics_page(window, cx),
            Section::Connections => self.environments(cx),
            Section::About => self.about(cx),
            Section::Archived => self.archive_page(cx),
            Section::Devices => self.devices_page(cx),
            Section::Voice => self.voice_page(window, cx),
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
        Section::Diffs => Icon::FileDiff,
        Section::Browser => Icon::Globe,
        Section::SnapShot => Icon::History,
        Section::Keybindings => Icon::Keyboard,
        Section::Providers => Icon::Bot,
        Section::Threads => Icon::MessagesSquare,
        Section::Devices => Icon::Smartphone,
        Section::Voice => Icon::Mic,
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
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = active_locale(cx);
        if self.last_locale != locale {
            self.last_locale = locale;
            let placeholder = t(cx, "Search").to_owned();
            self.search.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
        let layout = ModalLayout::of(window);
        let search_scope_notice = self.render_search_scope_notice(cx);
        let (blocks, archive_list) = if search_scope_notice.is_some() {
            (Vec::new(), None)
        } else {
            let blocks = self.render_page(window, cx);
            let archive_list = match self.section {
                Section::Archived => self.archive_list(self.scroll_to_target, cx),
                Section::Projects => self.project_list_view(cx),
                _ => None,
            };
            (blocks, archive_list)
        };
        let scope_sentence = self.render_scope_sentence(cx);
        if self.scroll_to_target {
            self.scroll_to_target = false;
            if let Some(index) = self.target.as_deref().and_then(|target| blocks.iter().position(|block| block.anchors.iter().any(|anchor| anchor.as_ref() == target))) {
                let offset = usize::from(store::error(cx).is_some())
                    + usize::from(scope_sentence.is_some());
                self.scroll.scroll_to_item(index + offset);
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
        let scope_banner = scope_sentence.map(|el| {
            div().w_full().max_w(r(page_width)).px(r(24.)).child(el)
        });
        let scope_notice = search_scope_notice.map(|el| {
            div().w_full().max_w(r(page_width)).px(r(24.)).child(el)
        });
        let content = match archive_list {
            Some(list) => v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .when_some(error, |this, error| this.child(div().w_full().flex().justify_center().pt(r(24.)).child(error)))
                .when_some(scope_banner, |this, banner| this.child(div().w_full().flex().justify_center().pt(r(16.)).child(banner)))
                .child(list)
                .into_any_element(),
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
                .children(scope_banner)
                .children(scope_notice)
                .children(blocks.into_iter().map(|block| div().w_full().max_w(r(page_width)).px(r(24.)).child(block.element)))
                .into_any_element(),
        };
        let icon_picker = self.project_icon_picker_overlay(window, cx);
        let action_editor = self.action_editor_overlay(window, cx);
        let secret_dialog = self.secret_dialog_overlay(window, cx);
        let background_dialog = self.background_dialog_overlay(window, cx);
        let environments_dialog = self.environments_dialog_overlay(window, cx);
        let voice_dialog = self.voice_dialog_overlay(window, cx);
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
            .children(environments_dialog)
            .children(voice_dialog)
            .children(self.appearance_ui.import.clone())
    }
}

#[cfg(feature="snapshot")]
pub(crate) fn import_theme_fixture(path:&std::path::Path,state:&Entity<AppState>,cx:&mut App)->Result<(),String> {
    let text=std::fs::read_to_string(path).map_err(|e|e.to_string())?;
    let theme=t3_core::themes::import(&text)?;
    appearance::import_theme(state,theme,true,cx);Ok(())
}

#[cfg(all(test, feature = "snapshot"))]
mod shell_tests {
    use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div};
    use serde_json::json;
    use t3_core::settings::search::Section;

    use super::*;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(overlay(window, cx))
        }
    }

    #[gpui::test]
    fn scope_sentence_search_availability_and_detail_breadcrumb(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "settings:write"]
                }))
                .unwrap(),
            ))
            .unwrap();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::updates::init(cx);
            init(state.clone(), cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": {
                            "environmentId": "env-mac",
                            "label": "Mac Studio",
                            "platform": { "os": "darwin", "arch": "arm64" }
                        },
                        "settings": {}
                    }))
                    .unwrap(),
                );
                state.environments.environments.push(
                    serde_json::from_value(json!({
                        "id": "env-mac",
                        "label": "Mac Studio",
                        "httpBase": "http://127.0.0.1:3773",
                        "local": true,
                        "environmentId": "env-mac"
                    }))
                    .unwrap(),
                );
                state.environments.environments.push(
                    serde_json::from_value(json!({
                        "id": "env-vps",
                        "label": "Build VPS",
                        "httpBase": "https://vps.example.ts.net",
                        "local": false,
                        "environmentId": "env-vps"
                    }))
                    .unwrap(),
                );
                cx.emit(AppEvent::ServerConfig);
                cx.emit(AppEvent::Phase);
            });
            open(Section::Appearance, cx);
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        // Device-only (Appearance) does not render a scope banner; scoped panels (Threads) do.
        assert!(vcx.debug_bounds("settings-scope-sentence").is_none());
        vcx.update(|_, cx| open(Section::Threads, cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("settings-scope-sentence").is_some());

        // Switching UI language updates reactive translations and localized settings search.
        vcx.update(|window, cx| {
            store::update_client(
                &state,
                t3_core::client_settings::ClientSettingChange::Language("de".into()),
                cx,
            );
            assert_eq!(crate::i18n::active_locale(cx), "de");
            assert_eq!(crate::i18n::t(cx, "Settings"), "Einstellungen");
            let view = cx.global::<Host>().view.clone().expect("settings view open");
            let items = view.read(cx).search_items(cx);
            let german_matches = search::search_with_locale("Sprache", &items, "de");
            assert!(german_matches.iter().any(|item| item.id == "language"));
            let english_matches = search::search_with_locale("Language", &items, "de");
            assert!(english_matches.iter().any(|item| item.id == "language"));
            store::update_client(
                &state,
                t3_core::client_settings::ClientSettingChange::Language("en".into()),
                cx,
            );
            window.refresh();
        });

        // Jumping to a project-only target while in all-environments scope renders the scope notice
        // powered by `is_search_scope_available`.
        vcx.update(|_, cx| {
            let view = cx.global::<Host>().view.clone().expect("settings view open");
            view.update(cx, |v, cx| {
                v.scope = SettingsScopeSearch::default();
                v.navigate(Section::Projects, Some("project-actions".to_owned()), cx);
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("settings-scope-notice").is_some());
        vcx.update(|_, cx| {
            let view = cx.global::<Host>().view.clone().expect("settings view open");
            view.update(cx, |v, cx| v.navigate(Section::Threads, None, cx));
        });

        // Search availability includes darwin fast-worktree-checkout, voice, text-generators, and
        // multi-env items, and hides text-generators when managedPolicy restricts allowedDrivers.
        vcx.update(|window, cx| {
            let view = cx.global::<Host>().view.clone().expect("settings view open");
            let items = view.read(cx).search_items(cx);
            assert!(items.iter().any(|i| i.id == "worktree-fast-checkout"));
            assert!(items.iter().any(|i| i.id == "voice-microphone"));
            assert!(items.iter().any(|i| i.id == "load-balancing"));
            assert!(items.iter().any(|i| i.id == "text-generators"));
            assert!(items.iter().any(|i| i.id == "language"));
            assert!(items.iter().any(|i| i.id == "screen-reader-announcements"));
            assert!(items.iter().any(|i| i.id == "screen-reader-window-title"));

            state.update(cx, |state, cx| {
                state.server_config.config.as_mut().unwrap().managed_policy = Some(json!({
                    "organization": "Acme",
                    "limits": { "providers": { "allowedDrivers": ["codex"] } }
                }));
                cx.emit(AppEvent::ServerConfig);
            });
            let restricted = view.read(cx).search_items(cx);
            assert!(!restricted.iter().any(|i| i.id == "text-generators"));

            // Navigating to Diagnostics sets nav_parent to Maintenance, and navigating to
            // load-balancing auto-reveals its folded section.
            view.update(cx, |v, cx| v.navigate(Section::Diagnostics, None, cx));
            assert_eq!(view.read(cx).section.nav_parent(), Section::Maintenance);
            view.update(cx, |v, cx| {
                v.navigate(Section::Connections, Some("load-balancing".to_owned()), cx)
            });
            assert!(view.read(cx).environments.load_balancing_open);

            drop(view);
            finish_close(window, cx);
        });
        state.update(&mut vcx, |state, _| state.stop_pump());
        vcx.run_until_parked();
        drop(vcx);
        drop(runtime);
    }
}
