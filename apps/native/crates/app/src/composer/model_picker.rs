//! The model picker (design-system §7.10): provider rail, search, model list, the effort column for
//! the selected model, and a footer with speed and permissions. Ports `ProviderModelPicker.tsx`,
//! `ModelPickerContent.tsx`, `ModelListRow.tsx`, and the column and footer in
//! `ComposerModelEffort.tsx`. Choices go to the composer as `PickerEvent`s.

use std::collections::HashSet;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, ParentElement,
    Render, ScrollHandle, ScrollStrategy, SharedString, Styled, Subscription, UniformListScrollHandle, Window, actions, div,
    prelude::*, px, uniform_list,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveRight};
use gpui_component::{h_flex, v_flex};
use t3_core::composer::{runtime_mode_description, runtime_mode_label};
use t3_core::model::ModelSelection;
use t3_core::providers::{self, ProviderEntry, RefreshProviders};
use t3_core::server_config::{OptionDescriptor, RuntimeMode, ServerProvider, WorkspaceSnapshot};
use t3_core::traits::{self, OptionSelection, OptionValue, Speed};

use super::controls::{Float, Segment, floating, kbd, menu_surface, segmented_track};
use super::runtime_mode_icon;
use crate::app_state::AppState;
use crate::provider_icon::{BadgeSize, ProviderIcon, ProviderMark};
use crate::icons::Icon;
use crate::theme::{ThemeSlot, ThemeSlotExt, hsla, palette};

actions!(
    model_picker,
    [
        HighlightPrevious,
        HighlightNext,
        PickHighlighted,
        ToggleHighlighted,
        PreviousProvider,
        NextProvider,
        JumpToModel1,
        JumpToModel2,
        JumpToModel3,
        JumpToModel4,
        JumpToModel5,
        JumpToModel6,
        JumpToModel7,
        JumpToModel8,
        JumpToModel9,
    ]
);

const CONTEXT: &str = "ModelPicker";
const RAIL_WIDTH: f32 = 44.;
const LIST_WIDTH: f32 = 360.;
const LIST_HEIGHT: f32 = 346.;
const EFFORT_WIDTH: f32 = 192.;
const ROW_HEIGHT: f32 = 28.;

pub fn init(cx: &mut App) {
    let context = Some("ModelPicker > Input");
    cx.bind_keys([
        KeyBinding::new("up", HighlightPrevious, context),
        KeyBinding::new("down", HighlightNext, context),
        KeyBinding::new("enter", PickHighlighted, context),
        KeyBinding::new("shift-enter", ToggleHighlighted, context),
    ]);
}

/// What the composer shows for its target, handed to the picker when it opens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PickerInput {
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub workspace_snapshots: Vec<(String, WorkspaceSnapshot)>,
    pub workspace_scanning: Vec<String>,
    pub selection: Option<ModelSelection>,
    pub multi_models_enabled: bool,
    pub selected_models: Vec<ModelSelection>,
    pub driver: String,
    pub descriptors: Vec<OptionDescriptor>,
    pub runtime_mode: RuntimeMode,
    pub runtime_modes: Vec<RuntimeMode>,
    /// Active environment's managed policy (`useManagedPolicy`), enforcing `maxRuntimeMode`.
    pub managed_policy: Option<serde_json::Value>,
    /// A started thread that cannot switch providers keeps this instance.
    pub locked_instance: Option<String>,
}

/// Active composer environment's managed policy for `permissions_section` without changing
/// `composer/view.rs`'s call signature.
#[derive(Default)]
struct ActiveComposerManagedPolicy {
    policy: Option<serde_json::Value>,
}
impl gpui::Global for ActiveComposerManagedPolicy {}

pub(super) fn set_active_managed_policy(policy: Option<serde_json::Value>, cx: &mut App) {
    if cx.try_global::<ActiveComposerManagedPolicy>().is_some_and(|g| g.policy == policy) {
        return;
    }
    cx.set_global(ActiveComposerManagedPolicy { policy });
}

pub enum PickerEvent {
    Model { instance_id: String, model: String },
    ToggleModel { instance_id: String, model: String },
    InspectInstance(String),
    Options(Vec<OptionSelection>),
    RuntimeMode(RuntimeMode),
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tab {
    Favorites,
    Instance(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Model {
        instance_id: String,
        slug: String,
        name: SharedString,
        sub_provider: Option<SharedString>,
        mark: ProviderMark,
        badge_new: bool,
        favorite: bool,
        selected: bool,
        disabled: Option<SharedString>,
        unavailable: bool,
    },
    SubProvider {
        instance_id: String,
        name: SharedString,
        unavailable_reason: Option<SharedString>,
        expanded: bool,
        model_count: usize,
    },
    Legacy { expanded: bool },
}

pub struct ModelPicker {
    state: Entity<AppState>,
    search: Entity<InputState>,
    focus: FocusHandle,
    input: PickerInput,
    tab: Tab,
    legacy_expanded: bool,
    expanded_sub_providers: HashSet<(String, String)>,
    rows: Vec<Row>,
    highlighted: Option<usize>,
    permissions_open: bool,
    permission_preview: Option<RuntimeMode>,
    effort_focus: FocusHandle,
    effort_index: usize,
    effort_scroll: ScrollHandle,
    option_focus: FocusHandle,
    option_menu: Option<(String, usize)>,
    option_scroll: ScrollHandle,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for ModelPicker {}

impl Focusable for ModelPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ModelPicker {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_rows(cx);
                    this.highlighted = this.first_enabled();
                    cx.notify();
                }
            }),
        ];
        ModelPicker {
            state,
            search,
            focus: cx.focus_handle(),
            input: PickerInput::default(),
            tab: Tab::Favorites,
            legacy_expanded: false,
            expanded_sub_providers: HashSet::new(),
            rows: Vec::new(),
            highlighted: None,
            permissions_open: false,
            permission_preview: None,
            effort_focus: cx.focus_handle(),
            effort_index: 0,
            effort_scroll: ScrollHandle::new(),
            option_focus: cx.focus_handle(),
            option_menu: None,
            option_scroll: ScrollHandle::new(),
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Resets the picker for a fresh open: search cleared, Favorites first when there are some.
    pub fn open(&mut self, input: PickerInput, window: &mut Window, cx: &mut Context<Self>) {
        let (has_favorites, refresh_catalog) = {
            let state = self.state.read(cx);
            let config = state.server_config.config.as_ref();
            let has_favorites = config.is_some_and(|config| !providers::favorites(&config.settings).is_empty());
            let should_refresh = config.is_some_and(|config| config.environment.capabilities.model_catalog_refresh == Some(true));
            let refresh_catalog = should_refresh.then(|| {
                let conn = state.connection_id();
                let rpc = state.rpc(t3_core::composer::menu::REFRESH_PROVIDERS, RefreshProviders::ModelCatalog.payload());
                (conn, rpc)
            });
            (has_favorites, refresh_catalog)
        };
        if let Some((connection, rpc)) = refresh_catalog {
            cx.spawn(async move |this, cx| {
                let Ok(reply) = rpc.await else { return };
                let Some(providers) = reply
                    .get("providers")
                    .and_then(|value| serde_json::from_value::<Vec<ServerProvider>>(value.clone()).ok())
                else {
                    return;
                };
                let _ = this.update(cx, |this, cx| {
                    // `modelCatalogOnly: true` pushes updates on the `ServerConfig` stream and returns `providers: []`.
                    if !providers.is_empty() {
                        this.state.update(cx, |state, cx| {
                            if state.connection_id() == connection && state.server_config.apply_providers(providers) {
                                cx.emit(crate::app_state::AppEvent::ServerConfig);
                            }
                        });
                    }
                });
            })
            .detach();
        }
        let active = input.selection.as_ref().map(|selection| selection.instance_id.clone()).unwrap_or_default();
        self.tab = if has_favorites && input.locked_instance.is_none() { Tab::Favorites } else { Tab::Instance(active) };
        self.input = input;
        self.legacy_expanded = false;
        self.expanded_sub_providers.clear();
        // The thread's active sub-provider opens expanded so its models show without a click (`pingdotgg/t3code#29388`).
        if let Some(selection) = self.input.selection.as_ref()
            && let Some(config) = self.state.read(cx).server_config.config.as_ref()
            && let Some(provider) = config.provider(&selection.instance_id)
        {
            let models = providers::models_for_cwd(provider, self.snapshot_for(provider));
            if let Some(sub) = models
                .iter()
                .find(|m| m.slug == selection.model)
                .and_then(|m| m.sub_provider.as_ref())
            {
                self.expanded_sub_providers.insert((selection.instance_id.clone(), sub.clone()));
            }
        }
        self.permissions_open = false;
        self.permission_preview = None;
        self.effort_index = 0;
        self.option_menu = None;
        self.search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        self.refresh_rows(cx);
        self.highlighted = self.rows.iter().position(|row| matches!(row, Row::Model { selected: true, .. })).or_else(|| self.first_enabled());
        if let Some(index) = self.highlighted {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
        if let Tab::Instance(id) = &self.tab
            && !id.is_empty()
        {
            cx.emit(PickerEvent::InspectInstance(id.clone()));
        }
        cx.notify();
    }

    /// Keeps an open picker in step with the composer, such as after a level was picked or the
    /// server config changed. A closed picker rebuilds its rows when it opens.
    pub fn update_input(&mut self, input: PickerInput, config_changed: bool, cx: &mut Context<Self>) {
        if self.input != input || config_changed {
            if self.input.selection.as_ref().map(|selection| (&selection.instance_id, &selection.model))
                != input.selection.as_ref().map(|selection| (&selection.instance_id, &selection.model))
            {
                self.option_menu = None;
                self.effort_index = initial_effort_index(&input.driver, &input.descriptors, cx).unwrap_or_default();
            }
            self.input = input;
            self.refresh_rows(cx);
            cx.notify();
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_owned()
    }

    fn first_enabled(&self) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(
                row,
                Row::Model { disabled: None, .. }
                    | Row::SubProvider { unavailable_reason: None, .. }
                    | Row::Legacy { .. }
            )
        })
    }

    fn snapshot_for<'a>(&'a self, provider: &'a ServerProvider) -> Option<&'a WorkspaceSnapshot> {
        let fetched = self.input.workspace_snapshots.iter().find(|(id, _)| id == &provider.instance_id).map(|(_, snapshot)| snapshot);
        t3_core::composer::menu::workspace_snapshot(provider, fetched, self.input.cwd.as_deref())
    }

    fn unavailable_active_row(
        &self,
        entry: &ProviderEntry,
        entries: &[ProviderEntry],
        settings: &t3_core::server_config::ServerSettings,
        models: &[&t3_core::server_config::ProviderModel],
    ) -> Option<Row> {
        let selected = self.input.selection.as_ref()?;
        if !entry.enabled || selected.instance_id != entry.instance_id() {
            return None;
        }
        let hidden = settings
            .provider_model_preferences
            .as_ref()
            .and_then(|prefs| prefs.get(entry.instance_id()))
            .map(|p| p.hidden_models.as_slice())
            .unwrap_or(&[]);
        let raw_models: Vec<t3_core::server_config::ProviderModel> =
            models.iter().map(|m| (*m).clone()).collect();
        if !providers::should_preserve_unavailable_selection(
            entry.driver(),
            &selected.model,
            &raw_models,
            hidden,
        ) {
            return None;
        }
        let disabled = self
            .input
            .locked_instance
            .as_deref()
            .filter(|locked| *locked != entry.instance_id())
            .map(|_| SharedString::from(LOCKED_REASON));
        Some(Row::Model {
            instance_id: entry.instance_id().to_owned(),
            slug: selected.model.clone(),
            name: selected.model.clone().into(),
            sub_provider: None,
            mark: ProviderMark::of(entry, entries),
            badge_new: false,
            favorite: providers::is_favorite(settings, entry.instance_id(), &selected.model),
            selected: true,
            disabled,
            unavailable: true,
        })
    }

    fn refresh_rows(&mut self, cx: &App) {
        let query = self.query(cx);
        let state = self.state.read(cx);
        let Some(config) = state.server_config.config.as_ref() else {
            self.rows.clear();
            return;
        };
        let entries = providers::entries(config, self.input.project_id.as_deref());
        let selected = self.input.selection.as_ref();
        let multi = &self.input.selected_models;
        let row = |entry: &ProviderEntry, model: &t3_core::server_config::ProviderModel| {
            let favorite = providers::is_favorite(&config.settings, entry.instance_id(), &model.slug);
            let disabled = self
                .input
                .locked_instance
                .as_deref()
                .filter(|locked| *locked != entry.instance_id())
                .map(|_| SharedString::from(LOCKED_REASON));
            let is_selected = if !multi.is_empty() {
                multi.iter().any(|s| s.instance_id == entry.instance_id() && s.model == model.slug)
            } else {
                selected.is_some_and(|s| s.instance_id == entry.instance_id() && s.model == model.slug)
            };
            Row::Model {
                instance_id: entry.instance_id().to_owned(),
                slug: model.slug.clone(),
                name: providers::model_name(model).to_owned().into(),
                sub_provider: model.sub_provider.clone().map(SharedString::from),
                mark: ProviderMark::of(entry, &entries),
                badge_new: model.badge.as_deref() == Some("new"),
                favorite,
                selected: is_selected,
                disabled,
                unavailable: false,
            }
        };
        let mut rows = Vec::new();
        let mut seen_keys: HashSet<(String, String)> = HashSet::new();
        if !query.is_empty() {
            let mut ranked: Vec<(i64, usize, Row)> = Vec::new();
            for entry in entries.iter().filter(|entry| entry.is_ready()) {
                for model in providers::model_options_for_cwd(&config.settings, entry.provider, self.snapshot_for(entry.provider)) {
                    if !seen_keys.insert((entry.instance_id().to_owned(), model.slug.clone())) {
                        continue;
                    }
                    let favorite = providers::is_favorite(&config.settings, entry.instance_id(), &model.slug);
                    if let Some(score) = providers::search_score(model, entry, favorite, &query) {
                        ranked.push((score, ranked.len(), row(entry, model)));
                    }
                }
            }
            ranked.sort_by_key(|(score, order, _)| (*score, *order));
            rows.extend(ranked.into_iter().map(|(_, _, row)| row));
        } else {
            match &self.tab {
                Tab::Favorites => {
                    for favorite in providers::favorites(&config.settings) {
                        let Some(entry) = entries.iter().find(|entry| entry.instance_id() == favorite.provider && entry.is_ready()) else {
                            continue;
                        };
                        let models = providers::models_for_cwd(entry.provider, self.snapshot_for(entry.provider));
                        if let Some(model) = models.iter().find(|model| model.slug == favorite.model)
                            && seen_keys.insert((entry.instance_id().to_owned(), model.slug.clone()))
                        {
                            rows.push(row(entry, model));
                        }
                    }
                }
                Tab::Instance(instance_id) => {
                    if let Some(entry) = entries.iter().find(|entry| entry.instance_id() == instance_id) {
                        let models = if entry.is_ready() {
                            providers::model_options_for_cwd(&config.settings, entry.provider, self.snapshot_for(entry.provider))
                        } else {
                            Vec::new()
                        };
                        let unavailable_row =
                            self.unavailable_active_row(entry, &entries, &config.settings, &models);
                        let favorite = |slug: &str| providers::is_favorite(&config.settings, entry.instance_id(), slug);
                        if let Some(sub_providers) = entry
                            .provider
                            .sub_providers
                            .as_deref()
                            .filter(|subs| !subs.is_empty() && entry.is_ready())
                        {
                            let (ungrouped, groups) = providers::group_models_by_sub_provider(
                                &models,
                                sub_providers,
                                |model| model.sub_provider.as_deref(),
                            );
                            let mut ungrouped_current = ungrouped;
                            ungrouped_current.sort_by_key(|model| !favorite(&model.slug));
                            for model in ungrouped_current {
                                if seen_keys.insert((entry.instance_id().to_owned(), model.slug.clone())) {
                                    rows.push(row(entry, model));
                                }
                            }
                            for group in groups {
                                let key = (entry.instance_id().to_owned(), group.sub_provider.name.clone());
                                let unavailable_reason = group
                                    .sub_provider
                                    .unavailable_reason
                                    .clone()
                                    .map(SharedString::from);
                                let expanded = unavailable_reason.is_none()
                                    && self.expanded_sub_providers.contains(&key);
                                rows.push(Row::SubProvider {
                                    instance_id: entry.instance_id().to_owned(),
                                    name: group.sub_provider.name.clone().into(),
                                    unavailable_reason,
                                    expanded,
                                    model_count: group.models.len(),
                                });
                                if expanded {
                                    let mut group_models = group.models;
                                    group_models.sort_by_key(|model| !favorite(&model.slug));
                                    for model in group_models {
                                        if seen_keys.insert((entry.instance_id().to_owned(), model.slug.clone())) {
                                            rows.push(row(entry, model));
                                        }
                                    }
                                }
                            }
                        } else if entry.is_ready() {
                            let (mut current, legacy): (Vec<_>, Vec<_>) =
                                models.into_iter().partition(|model| !model.is_legacy || favorite(&model.slug));
                            current.sort_by_key(|model| !favorite(&model.slug));
                            for model in current {
                                if seen_keys.insert((entry.instance_id().to_owned(), model.slug.clone())) {
                                    rows.push(row(entry, model));
                                }
                            }
                            if !legacy.is_empty() {
                                if self.legacy_expanded {
                                    for model in legacy {
                                        if seen_keys.insert((entry.instance_id().to_owned(), model.slug.clone())) {
                                            rows.push(row(entry, model));
                                        }
                                    }
                                }
                                rows.push(Row::Legacy { expanded: self.legacy_expanded });
                            }
                        }
                        if let Some(unavailable) = unavailable_row {
                            rows.push(unavailable);
                        }
                    }
                }
            }
        }
        self.rows = rows;
        if self.highlighted.is_some_and(|index| index >= self.rows.len()) {
            self.highlighted = self.first_enabled();
        }
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let message = match &tab {
            Tab::Favorites => "Favorites".to_owned(),
            Tab::Instance(id) => self.state.read(cx).server_config.config.as_ref().and_then(|c| c.provider(id)).map(providers::display_name).unwrap_or_else(|| id.clone()),
        };
        self.state.update(cx, |state, cx| state.announce(message, Some("model-picker"), cx));
        self.tab = tab.clone();
        self.legacy_expanded = false;
        self.refresh_rows(cx);
        self.highlighted = self.first_enabled();
        if let Tab::Instance(id) = tab
            && !id.is_empty()
        {
            cx.emit(PickerEvent::InspectInstance(id));
        }
        cx.notify();
    }

    fn selectable_tabs(&self, cx: &App) -> Vec<Tab> {
        let state = self.state.read(cx);
        let Some(config) = state.server_config.config.as_ref() else {
            return Vec::new();
        };
        let mut tabs = Vec::new();
        if !providers::favorites(&config.settings).is_empty() {
            tabs.push(Tab::Favorites);
        }
        for entry in providers::entries(config, self.input.project_id.as_deref()) {
            let locked = self.input.locked_instance.as_deref().is_some_and(|locked| locked != entry.instance_id());
            if entry.enabled && entry.is_ready() && !locked {
                tabs.push(Tab::Instance(entry.instance_id().to_owned()));
            }
        }
        tabs
    }

    /// `modelPicker.previousProvider` / `modelPicker.nextProvider` (`adjacentModelPickerProvider`).
    pub(crate) fn step_provider(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.selectable_tabs(cx);
        if tabs.is_empty() {
            return;
        }
        let next_index = match tabs.iter().position(|tab| *tab == self.tab) {
            Some(index) => (index as isize + direction).rem_euclid(tabs.len() as isize) as usize,
            None if direction > 0 => 0,
            None => tabs.len() - 1,
        };
        let next = tabs[next_index].clone();
        self.search.update(cx, |search, cx| {
            if !search.value().is_empty() {
                search.set_value("", window, cx);
            }
        });
        self.select_tab(next, cx);
    }

    fn model_jump_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.iter().enumerate().filter_map(|(index, row)| {
            matches!(row, Row::Model { disabled: None, .. }).then_some(index)
        })
    }

    /// `modelPicker.jump.1..9`: activates the Nth enabled model row (`0`-based).
    pub(crate) fn jump_to_model(&mut self, index: usize, cx: &mut Context<Self>) {
        let row_index = self.model_jump_indices().nth(index);
        if let Some(row_index) = row_index {
            self.activate(row_index, false, cx);
        }
    }

    fn activate(&mut self, index: usize, toggle: bool, cx: &mut Context<Self>) {
        match self.rows.get(index).cloned() {
            Some(Row::Model { instance_id, slug, name, disabled: None, .. }) => {
                if toggle && self.input.multi_models_enabled {
                    cx.emit(PickerEvent::ToggleModel { instance_id, model: slug });
                } else {
                    self.state.update(cx, |state, cx| state.announce(format!("Model: {name}"), Some("model-picker"), cx));
                    cx.emit(PickerEvent::Model { instance_id, model: slug });
                }
            }
            Some(Row::SubProvider { instance_id, name, unavailable_reason: None, .. }) => {
                let key = (instance_id, name.to_string());
                if !self.expanded_sub_providers.remove(&key) {
                    self.expanded_sub_providers.insert(key);
                }
                self.refresh_rows(cx);
                cx.notify();
            }
            Some(Row::Legacy { expanded }) => {
                self.legacy_expanded = !expanded;
                self.refresh_rows(cx);
                cx.notify();
            }
            _ => {}
        }
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.rows.len();
        if count == 0 {
            return;
        }
        let mut index = self.highlighted.map(|i| i as isize).unwrap_or(-1);
        for _ in 0..count {
            index = (index + step).rem_euclid(count as isize);
            if !matches!(
                self.rows[index as usize],
                Row::Model { disabled: Some(_), .. }
                    | Row::SubProvider { unavailable_reason: Some(_), .. }
            ) {
                break;
            }
        }
        self.highlighted = Some(index as usize);
        self.scroll.scroll_to_item(index as usize, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn toggle_favorite(&mut self, instance_id: String, slug: String, favorite: bool, cx: &mut Context<Self>) {
        let request = self.state.read(cx).rpc(
            t3_core::commands::method::SERVER_UPDATE_SETTINGS,
            t3_core::commands::set_model_favorite(&instance_id, &slug, !favorite),
        );
        cx.spawn(async move |_, _| {
            if let Err(error) = request.await {
                tracing::warn!(%error, "could not save the favorite");
            }
        })
        .detach();
    }

    fn change_option(&mut self, id: &str, value: OptionValue, cx: &mut Context<Self>) {
        if let Some(selections) = traits::with_value(&self.input.descriptors, id, value) {
            cx.emit(PickerEvent::Options(selections));
        }
    }

    fn render_rail(&self, entries: &[(String, SharedString, ProviderMark, bool)], has_favorites: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let button = |id: SharedString, active: bool, disabled: bool, content: gpui::AnyElement, tooltip: SharedString| {
            div()
                .id(id)
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .when(active, |this| this.bg(hsla(p.foreground).opacity(0.09)))
                .when(disabled, |this| this.opacity(0.4))
                .when(!disabled && !active, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.accent))))
                .child(content)
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
        };
        v_flex()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .py(px(6.))
            .bg(hsla(p.muted).opacity(0.3))
            .border_r_1()
            .border_color(hsla(p.border))
            .when(has_favorites, |this| {
                let active = self.tab == Tab::Favorites;
                this.child(
                    button("rail-favorites".into(), active, false, Icon::Star.render(px(16.), hsla(p.muted_foreground)).into_any_element(), "Favorites".into())
                        .on_click(cx.listener(|this, _, _, cx| this.select_tab(Tab::Favorites, cx))),
                )
            })
            .children(entries.iter().map(|(instance_id, name, mark, locked)| {
                let active = self.tab == Tab::Instance(instance_id.clone());
                let id = instance_id.clone();
                let ring = if active { hsla(p.background) } else { hsla(p.popover) };
                let icon = ProviderIcon::new(mark.clone()).size(px(20.)).badge(BadgeSize::Default, ring);
                button(format!("rail-{instance_id}").into(), active, *locked, icon.into_any_element(), name.clone())
                    .when(!*locked, |this| this.on_click(cx.listener(move |this, _, _, cx| this.select_tab(Tab::Instance(id.clone()), cx))))
            }))
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let highlighted = self.highlighted == Some(index);
        let base = h_flex()
            .id(("model-row", index))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(4.))
            .when(highlighted, |this| this.bg(hsla(p.foreground).opacity(0.09)));
        match &self.rows[index] {
            Row::SubProvider { name, unavailable_reason, expanded, model_count, .. } => {
                let available = unavailable_reason.is_none();
                let chevron = if *expanded { Icon::ChevronDown } else { Icon::ChevronRight };
                base.when(available, |this| {
                    this.cursor_pointer()
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.highlighted != Some(index) {
                                this.highlighted = Some(index);
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.activate(index, false, cx)))
                })
                .when(!available, |this| this.opacity(0.64))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(chevron.render(px(12.), hsla(p.muted_foreground)))
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                .when_some(unavailable_reason.clone(), |this, reason| {
                    this.child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(hsla(p.border))
                            .text_size(px(10.))
                            .text_color(hsla(p.muted_foreground))
                            .child(crate::i18n::t(cx, "Unavailable")),
                    )
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx)
                    })
                })
                .when(available && *model_count > 0, |this| {
                    this.child(
                        div()
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(hsla(p.muted_foreground).opacity(0.7))
                            .child(format!("{model_count}")),
                    )
                })
                .into_any_element()
            }
            Row::Legacy { expanded } => base
                .cursor_pointer()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(div().flex_1().child(crate::i18n::t(cx, if *expanded { "Show less" } else { "Show more" })))
                .child(if *expanded { Icon::ChevronUp } else { Icon::ChevronDown }.render(px(14.), hsla(p.muted_foreground)))
                .on_click(cx.listener(move |this, _, _, cx| this.activate(index, false, cx)))
                .into_any_element(),
            Row::Model { instance_id, slug, name, sub_provider, mark, badge_new, favorite, selected, disabled, unavailable } => {
                let star_id = (instance_id.clone(), slug.clone(), *favorite);
                let shortcut = self.model_jump_indices().take(9).position(|row_index| row_index == index)
                    .and_then(|slot| crate::keymap::label_in(
                        &format!("modelPicker.jump.{}", slot + 1),
                        &t3_core::keybindings::ShortcutMatchContext::default().with("modelPickerOpen", true),
                        cx,
                    ));
                base.when(disabled.is_none(), |this| this.cursor_pointer())
                    .when(disabled.is_some() || *unavailable, |this| this.opacity(0.64))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlighted != Some(index) {
                            this.highlighted = Some(index);
                            cx.notify();
                        }
                    }))
                    .child(ProviderIcon::new(mark.clone()).badge(BadgeSize::Compact, hsla(p.popover)))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().text_sm().line_height(px(19.25)).text_color(hsla(p.foreground)).child(name.clone()))
                            .children(sub_provider.clone().map(|sub| {
                                div().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground).opacity(0.7)).child(sub)
                            }))
                            .when(*unavailable, |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px(px(4.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(hsla(p.border))
                                        .text_size(px(10.))
                                        .text_color(hsla(p.muted_foreground))
                                        .child(crate::i18n::t(cx, "Unavailable")),
                                )
                            })
                            .when(*badge_new, |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px(px(2.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(hsla(p.update).opacity(0.35))
                                        .bg(hsla(p.update).opacity(0.15))
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(hsla(p.update_foreground))
                                        .child("NEW"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id(("favorite", index))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.accent)))
                            .when(*favorite || highlighted, |this| {
                                this.child(if *favorite {
                                    Icon::StarFill.render(px(12.), hsla(p.warning))
                                } else {
                                    Icon::Star.render(px(12.), hsla(p.muted_foreground))
                                })
                            })
                            .tooltip(move |window, cx| {
                                let label = crate::i18n::t(cx, if star_id.2 { "Remove from favorites" } else { "Add to favorites" });
                                gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                            })
                            .on_click({
                                let (instance_id, slug, favorite) = (instance_id.clone(), slug.clone(), *favorite);
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_favorite(instance_id.clone(), slug.clone(), favorite, cx);
                                })
                            }),
                    )
                    .children(shortcut.map(|key| kbd(key, cx)))
                    .child(div().size(px(14.)).flex_none().when(*selected, |this| this.child(Icon::Check.render(px(14.), hsla(p.foreground)))))
                    .when_some(disabled.clone(), |this, reason| {
                        this.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx))
                    })
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| this.activate(index, event.modifiers().shift, cx)))
                    .into_any_element()
            }
        }
    }

    fn render_effort(&self, model_label: SharedString, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let layout = traits::layout(&self.input.driver, &self.input.descriptors);
        let header = layout.effort.map(|d| crate::i18n::t(cx, d.label()).to_owned())
            .unwrap_or_else(|| crate::i18n::t(cx, if layout.others.is_empty() { "Effort" } else { "Options" }).to_owned());
        let mut rows = layout.effort.map(|effort| option_rows(effort, cx)).unwrap_or_default();
        let effort_count = rows.len();
        rows.extend(layout.others.iter().map(|descriptor| {
            let choices = option_rows(descriptor, cx);
            let current = choices.iter().find(|choice| choice.selected).map(|choice| choice.label.clone()).unwrap_or_default();
            match descriptor {
                OptionDescriptor::Boolean(option) => OptionRow {
                    id: option.id.clone(),
                    label: current,
                    descriptor: Some(crate::i18n::t(cx, &option.label).into()),
                    value: Some(OptionValue::Bool(option.current_value != Some(true))),
                    selected: option.current_value == Some(true),
                    default: false,
                    description: None,
                    choices: Vec::new(),
                },
                OptionDescriptor::Select(option) => OptionRow {
                    id: option.id.clone(),
                    label: current,
                    descriptor: Some(crate::i18n::t(cx, &option.label).into()),
                    value: None,
                    selected: false,
                    default: false,
                    description: None,
                    choices,
                },
            }
        }));
        let effort_index = self.effort_index.min(rows.len().saturating_sub(1));
        let column_active = self.effort_focus.is_focused(window);
        let submenu_active = self.option_focus.is_focused(window);
        let rows = std::rc::Rc::new(rows);
        let column = v_flex().id("effort-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.effort_scroll)
            .w_full().gap(px(2.)).px(px(8.)).pt(px(10.)).pb(px(6.))
            .when(rows.is_empty(), |this| this.child(div().px(px(8.)).py(px(6.)).text_sm()
                .text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, "No effort levels"))))
            .children(rows.iter().enumerate().map(|(index, row)| {
                let open = self.option_menu.as_ref().filter(|(id, _)| id == &row.id && !row.choices.is_empty());
                let activate = rows.clone();
                let button = render_option_row(row, column_active && effort_index == index || open.is_some(), cx)
                    .on_click(cx.listener(move |picker, _, window, cx| {
                        cx.stop_propagation();
                        picker.effort_index = index;
                        picker.activate_option(&activate[index], window, cx);
                    }));
                div().relative()
                    .when(index == effort_count && effort_count > 0, |this| this.mt(px(4.)).pt(px(4.)).border_t_1().border_color(hsla(p.border)))
                    .child(button)
                    .when_some(open, |this, (_, selected)| {
                        let selected = (*selected).min(row.choices.len().saturating_sub(1));
                        let choices = row.choices.clone();
                        let popup = menu_surface(cx)
                            .id("model-option-menu")
                            .track_focus(&self.option_focus)
                            .occlude()
                            .w(px(240.))
                            .on_mouse_down_out(cx.listener(|picker, _, _, cx| {
                                picker.option_menu = None;
                                cx.notify();
                            }))
                            .on_action(cx.listener(|picker, _: &Escape, window, cx| picker.close_option_menu(window, cx)))
                            .on_key_down(cx.listener(move |picker, event: &gpui::KeyDownEvent, window, cx| {
                                let key = event.keystroke.key.as_str();
                                match key {
                                    "left" | "escape" => picker.close_option_menu(window, cx),
                                    "up" | "down" => {
                                        let offset = if key == "up" { -1 } else { 1 };
                                        let next = selected.saturating_add_signed(offset).min(choices.len() - 1);
                                        if let Some((_, index)) = &mut picker.option_menu {
                                            *index = next;
                                        }
                                        picker.option_scroll.scroll_to_item(next);
                                        cx.notify();
                                    }
                                    "enter" | "space" => picker.activate_option(&choices[selected], window, cx),
                                    _ => return,
                                }
                                cx.stop_propagation();
                            }))
                            .child(v_flex().id("model-option-menu-scroll").max_h(px(272.)).overflow_y_scroll().track_scroll(&self.option_scroll)
                                .children(row.choices.iter().enumerate().map(|(index, choice)| {
                                    let choice = choice.clone();
                                    render_option_row(&choice, submenu_active && index == selected, cx)
                                        .on_click(cx.listener(move |picker, _, window, cx| picker.activate_option(&choice, window, cx)))
                                })));
                        this.child(div().absolute().top_0().right(px(-6.)).child(
                            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopLeft).snap_to_window_with_margin(px(8.)).child(popup)).with_priority(2),
                        ))
                    })
            }));
        let navigation = rows.clone();
        v_flex()
            .id("model-effort-column")
            .track_focus(&self.effort_focus)
            .tab_index(0)
            .w(px(EFFORT_WIDTH))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.muted).opacity(0.4))
            .on_key_down(cx.listener(move |picker, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() { return; }
                let key = event.keystroke.key.as_str();
                match key {
                    "left" => {
                        picker.search.update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    }
                    "up" | "down" if !navigation.is_empty() => {
                        let offset = if key == "up" { -1 } else { 1 };
                        picker.effort_index = effort_index.saturating_add_signed(offset).min(navigation.len() - 1);
                        picker.effort_scroll.scroll_to_item(picker.effort_index);
                        cx.notify();
                    }
                    "enter" | "space" | "right" => {
                        let Some(row) = navigation.get(effort_index) else { return };
                        if key == "right" && row.choices.is_empty() { return; }
                        picker.activate_option(row, window, cx);
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(
                div().px(px(12.)).pt(px(10.)).child(
                    h_flex()
                        .h(px(33.))
                        .gap(px(8.))
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .text_sm()
                        .text_color(hsla(p.muted_foreground))
                        .child(header)
                        .child(div().ml_auto().min_w_0().truncate().text_xs().child(model_label)),
                ),
            )
            .child(column)

    }

    fn activate_option(&mut self, row: &OptionRow, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(value) = &row.value {
            self.change_option(&row.id, value.clone(), cx);
            if self.option_menu.is_some() {
                self.close_option_menu(window, cx);
            }
        } else if !row.choices.is_empty() {
            if self.option_menu.as_ref().is_some_and(|(id, _)| id == &row.id) {
                self.close_option_menu(window, cx);
            } else {
                let checked = row.choices.iter().position(|choice| choice.selected).unwrap_or_default();
                self.option_menu = Some((row.id.clone(), checked));
                self.option_scroll.scroll_to_item(checked);
                let focus = self.option_focus.clone();
                window.on_next_frame(move |window, cx| window.focus(&focus, cx));
                cx.notify();
            }
        }
    }

    fn move_from_search_to_effort(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let search = self.search.read(cx);
        let end = search.value().len();
        if !search.focus_handle(cx).is_focused(window) || search.selected_range() != (end..end) {
            return false;
        }
        let new_model = self.highlighted.filter(|index| {
            matches!(self.rows.get(*index), Some(Row::Model { selected: false, disabled: None, .. }))
        });
        let effort = initial_effort_index(&self.input.driver, &self.input.descriptors, cx);
        if new_model.is_none() && effort.is_none() {
            return false;
        }
        if let Some(index) = new_model {
            self.activate(index, false, cx);
        }
        self.effort_index = effort.unwrap_or_default();
        self.effort_scroll.scroll_to_item(self.effort_index);
        window.focus(&self.effort_focus, cx);
        cx.notify();
        true
    }

    fn close_option_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.option_menu = None;
        window.focus(&self.effort_focus, cx);
        cx.notify();
    }

    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let layout = traits::layout(&self.input.driver, &self.input.descriptors);
        let speed = layout.speed.map(|speed| match speed {
            Speed::Tier(select) => {
                let current = traits::select_value(select);
                segmented_track(cx).debug_selector(|| "speed-track".to_owned()).children(traits::selectable_choices(select).map(|choice| {
                    let (id, value) = (select.id.clone(), choice.id.clone());
                    let mut segment = Segment::new(SharedString::from(format!("speed-{}", choice.id)), current.as_deref() == Some(&choice.id))
                        .label(choice.label.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.change_option(&id, OptionValue::Str(value.clone()), cx)));
                    if matches!(choice.label.as_str(), "Fast" | "Ultrafast") {
                        segment = segment.icon(Icon::ZapFill);
                    }
                    if let Some(description) = &choice.description {
                        segment = segment.tooltip(description.clone());
                    }
                    segment
                }))
            }
            Speed::FastMode(boolean) => {
                let current = boolean.current_value == Some(true);
                segmented_track(cx).children([false, true].map(|value| {
                    let id = boolean.id.clone();
                    let segment = Segment::new(SharedString::from(format!("fast-{value}")), current == value)
                        .label(if value { "Fast" } else { "Standard" })
                        .on_click(cx.listener(move |this, _, _, cx| this.change_option(&id, OptionValue::Bool(value), cx)));
                    if value { segment.icon(Icon::ZapFill) } else { segment }
                }))
            }
        });
        let ultracode = layout.ultracode.map(|boolean| {
            let on = boolean.current_value == Some(true);
            let id = boolean.id.clone();
            segmented_track(cx).debug_selector(|| "ultracode-toggle".to_owned()).child(
                Segment::new("ultracode-toggle", on)
                    .icon(Icon::Workflow)
                    .label(boolean.label.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.change_option(&id, OptionValue::Bool(!on), cx))),
            )
        });
        let mode = self.input.runtime_mode;
        h_flex()
            .flex_wrap()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.7))
            .bg(hsla(p.muted).opacity(0.4))
            .children(speed)
            .children(ultracode)
            .child(
                div().ml_auto().relative().child(
                    h_flex()
                        .id("picker-permissions")
                        .h(px(28.))
                        .px(px(8.))
                        .gap(px(6.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .when(self.permissions_open, |this| this.bg(hsla(p.foreground).opacity(0.09)).text_color(hsla(p.foreground)))
                        .hover(|this| this.bg(hsla(p.foreground).opacity(0.09)).text_color(hsla(p.foreground)))
                        .child(runtime_mode_icon(mode).render(px(14.), hsla(p.muted_foreground)))
                        .child(runtime_mode_label(mode))
                        .child(Icon::ChevronUp.render(px(12.), hsla(p.muted_foreground)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.permissions_open = !this.permissions_open;
                            this.permission_preview = None;
                            cx.notify();
                        }))
                        .when(self.permissions_open, |this| {
                            this.child(floating(
                                Float::AboveEnd,
                                menu_surface(cx)
                                    .occlude()
                                    .mb(px(4.))
                                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                        this.permissions_open = false;
                                        this.permission_preview = None;
                                        cx.notify();
                                    }))
                                    .child(permissions_section_with_preview(
                                        mode,
                                        &self.input.runtime_modes,
                                        self.input.managed_policy.as_ref(),
                                        self.permission_preview,
                                        Some(|option, hover, this: &mut Self, cx: &mut Context<Self>| {
                                            if hover || this.permission_preview == Some(option) {
                                                this.permission_preview = hover.then_some(option);
                                                cx.notify();
                                            }
                                        }),
                                        cx,
                                        |mode, this: &mut Self, cx: &mut Context<Self>| {
                                            this.permissions_open = false;
                                            cx.emit(PickerEvent::RuntimeMode(mode));
                                            cx.notify();
                                        },
                                    )),
                            ))
                        }),
                ),
            )
    }
}

#[derive(Clone)]
struct OptionRow {
    id: String,
    label: SharedString,
    descriptor: Option<SharedString>,
    value: Option<OptionValue>,
    selected: bool,
    default: bool,
    description: Option<String>,
    choices: Vec<OptionRow>,
}

fn initial_effort_index(driver: &str, descriptors: &[OptionDescriptor], cx: &App) -> Option<usize> {
    let layout = traits::layout(driver, descriptors);
    let choices = layout.effort.map(|effort| option_rows(effort, cx)).unwrap_or_default();
    (!choices.is_empty() || !layout.others.is_empty())
        .then(|| choices.iter().position(|choice| choice.selected).unwrap_or_default())
}

fn option_rows(descriptor: &OptionDescriptor, cx: &App) -> Vec<OptionRow> {
    match descriptor {
        OptionDescriptor::Select(select) => {
            let current = traits::select_value(select);
            traits::selectable_choices(select).map(|choice| OptionRow {
                id: select.id.clone(),
                label: choice.label.clone().into(),
                descriptor: None,
                value: Some(OptionValue::Str(choice.id.clone())),
                selected: current.as_deref() == Some(&choice.id),
                default: choice.is_default,
                description: choice.description.clone(),
                choices: Vec::new(),
            }).collect()
        }
        OptionDescriptor::Boolean(option) => [true, false].into_iter().map(|value| OptionRow {
            id: option.id.clone(),
            label: crate::i18n::t(cx, if value { "On" } else { "Off" }).into(),
            descriptor: None,
            value: Some(OptionValue::Bool(value)),
            selected: (option.current_value == Some(true)) == value,
            default: false,
            description: None,
            choices: Vec::new(),
        }).collect(),
    }
}

fn render_option_row(row: &OptionRow, focused: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    let boolean = row.descriptor.is_some() && matches!(row.value, Some(OptionValue::Bool(_)));
    let value = match &row.value {
        Some(OptionValue::Str(value)) => value.clone(),
        Some(OptionValue::Bool(value)) => value.to_string(),
        _ => String::new(),
    };
    h_flex()
        .id(SharedString::from(format!("model-option:{}:{value}", row.id)))
        .min_h(px(32.))
        .w_full()
        .px(px(8.))
        .py(px(4.))
        .gap(px(6.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_sm()
        .text_color(hsla(p.foreground))
        .when(focused, |this| this.bg(hsla(p.foreground).opacity(0.09)))
        .hover(|this| this.bg(hsla(p.foreground).opacity(0.09)))
        .child(v_flex().flex_1().min_w_0()
            .children(row.descriptor.clone().map(|label| div().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(label)))
            .child(div().truncate().child(row.label.clone())))
        .when(row.default, |this| this.child(div().flex_none().text_xs().text_color(hsla(p.muted_foreground)).child(crate::i18n::t(cx, "Default"))))
        .when(boolean, |this| {
            this.child(crate::settings::controls::Switch::new(SharedString::from(format!("option-toggle:{}", row.id)), row.selected))
        })
        .when(!boolean, |this| this.child(div().size(px(14.)).flex_none()
            .when(!row.choices.is_empty(), |this| this.child(Icon::ChevronRight.render(px(14.), hsla(p.muted_foreground))))
            .when(row.selected, |this| this.child(Icon::Check.render(px(14.), hsla(p.foreground))))))
        .when_some(row.description.clone(), |this, description| this.tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(description.clone()).build(window, cx)
        }))
}

const LOCKED_REASON: &str =
    "This thread does not support switching providers after it has started. Start a new thread to use this model.";

/// `ComposerPermissionsSection`: the modes as one segmented row, the picked one named below.
/// Modes above an administrator's `maxRuntimeMode` ceiling stay listed, disabled with the policy's
/// denial reason (`resolveRuntimeModeOptions` in `runtimeModeConfig.ts`).
pub fn permissions_section<V: 'static>(
    mode: RuntimeMode,
    modes: &[RuntimeMode],
    cx: &mut Context<V>,
    on_pick: impl Fn(RuntimeMode, &mut V, &mut Context<V>) + Clone + 'static,
) -> impl IntoElement {
    let policy = cx
        .try_global::<ActiveComposerManagedPolicy>()
        .and_then(|g| g.policy.clone());
    permissions_section_with_policy(mode, modes, policy.as_ref(), cx, on_pick)
}

/// `ComposerPermissionsSection` with an explicit managed policy.
pub fn permissions_section_with_policy<V: 'static>(
    mode: RuntimeMode,
    modes: &[RuntimeMode],
    policy: Option<&serde_json::Value>,
    cx: &mut Context<V>,
    on_pick: impl Fn(RuntimeMode, &mut V, &mut Context<V>) + Clone + 'static,
) -> gpui::Div {
    permissions_section_with_preview(mode, modes, policy, None, None::<fn(RuntimeMode, bool, &mut V, &mut Context<V>)>, cx, on_pick)
}

fn permissions_section_with_preview<V: 'static>(
    mode: RuntimeMode,
    modes: &[RuntimeMode],
    policy: Option<&serde_json::Value>,
    preview: Option<RuntimeMode>,
    on_preview: Option<impl Fn(RuntimeMode, bool, &mut V, &mut Context<V>) + Clone + 'static>,
    cx: &mut Context<V>,
    on_pick: impl Fn(RuntimeMode, &mut V, &mut Context<V>) + Clone + 'static,
) -> gpui::Div {
    let p = palette(cx);
    let preview_mode = preview.unwrap_or(mode);
    let preview_denial = t3_core::managed_policy::runtime_mode_denial(policy, preview_mode.as_str());
    v_flex()
        .w(px(288.))
        .gap(px(6.))
        .px(px(8.))
        .py(px(6.))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Permissions"))
        .child(segmented_track(cx).children(RuntimeMode::ALL.into_iter().map(|option| {
            let supported = modes.contains(&option);
            let denial = t3_core::managed_policy::runtime_mode_denial(policy, option.as_str());
            let disabled = !supported || denial.is_some();
            let tooltip = denial.unwrap_or_else(|| {
                format!("{}. {}", runtime_mode_label(option), runtime_mode_description(option))
            });
            let on_pick = on_pick.clone();
            Segment::new(SharedString::from(format!("permission-{}", option.as_str())), option == mode)
                .icon(runtime_mode_icon(option))
                .disabled(disabled)
                .tooltip(tooltip)
                .when_some(on_preview.clone(), |segment, on_preview| {
                    segment.on_hover(cx.listener(move |this, &hover, _, cx| on_preview(option, hover, this, cx)))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !disabled {
                        on_pick(option, this, cx);
                    }
                }))
        })))
        .child(
            v_flex()
                .gap(px(2.))
                .child(div().text_sm().text_color(hsla(p.foreground)).child(runtime_mode_label(preview_mode)))
                .child(
                    div()
                        .min_h(px(32.))
                        .text_xs()
                        .line_height(px(16.))
                        .text_color(hsla(p.muted_foreground))
                        .child(preview_denial.unwrap_or_else(|| runtime_mode_description(preview_mode).to_owned())),
                ),
        )
}

impl Render for ModelPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let searching = !self.query(cx).is_empty();
        let (rail, has_favorites, model_label, empty_message) = {
            let state = self.state.read(cx);
            let config = state.server_config.config.as_ref();
            let entries = config.map(|config| providers::entries(config, self.input.project_id.as_deref())).unwrap_or_default();
            let rail: Vec<(String, SharedString, ProviderMark, bool)> = entries
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| {
                    let locked = self.input.locked_instance.as_deref().is_some_and(|locked| locked != entry.instance_id());
                    (entry.instance_id().to_owned(), entry.display_name.clone().into(), ProviderMark::of(entry, &entries), locked)
                })
                .collect();
            let has_favorites = config.is_some_and(|config| !providers::favorites(&config.settings).is_empty());
            let model_label: SharedString = self
                .input
                .selection
                .as_ref()
                .and_then(|selection| {
                    let provider = config?.provider(&selection.instance_id)?;
                    let model = providers::models_for_cwd(provider, self.snapshot_for(provider)).iter().find(|m| m.slug == selection.model)?;
                    Some(providers::trigger_model_name(model).to_owned())
                })
                .unwrap_or_default()
                .into();
            let empty_message: SharedString = match (&self.tab, config) {
                (_, None) => "Loading providers…".into(),
                (Tab::Favorites, Some(_)) if !searching => "No favorites yet".into(),
                (Tab::Instance(id), Some(config)) if !searching => entries
                    .iter()
                    .find(|entry| entry.instance_id() == id && !entry.is_ready())
                    .map(|entry| entry.provider.message.clone().unwrap_or_else(|| format!("{} is not ready.", entry.display_name)))
                    .unwrap_or_else(|| {
                        let scanning = self.input.workspace_scanning.iter().any(|s| s == id)
                            || config
                                .provider(id)
                                .and_then(|provider| self.snapshot_for(provider))
                                .is_some_and(|snapshot| snapshot.models_pending == Some(true));
                        if scanning { "Searching project models...".to_owned() } else { "No models found".to_owned() }
                    })
                    .into(),
                _ => "No models found".into(),
            };
            (rail, has_favorites, model_label, empty_message)
        };
        let rows = self.rows.len();
        menu_surface(cx)
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .occlude()
            .capture_action(cx.listener(|picker, _: &MoveRight, window, cx| {
                if picker.move_from_search_to_effort(window, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &HighlightPrevious, _, cx| this.move_highlight(-1, cx)))
            .on_action(cx.listener(|this, _: &HighlightNext, _, cx| this.move_highlight(1, cx)))
            .on_action(cx.listener(|this, _: &PickHighlighted, _, cx| {
                if let Some(index) = this.highlighted {
                    this.activate(index, false, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleHighlighted, _, cx| {
                if let Some(index) = this.highlighted {
                    this.activate(index, true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PreviousProvider, window, cx| this.step_provider(-1, window, cx)))
            .on_action(cx.listener(|this, _: &NextProvider, window, cx| this.step_provider(1, window, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel1, _, cx| this.jump_to_model(0, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel2, _, cx| this.jump_to_model(1, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel3, _, cx| this.jump_to_model(2, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel4, _, cx| this.jump_to_model(3, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel5, _, cx| this.jump_to_model(4, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel6, _, cx| this.jump_to_model(5, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel7, _, cx| this.jump_to_model(6, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel8, _, cx| this.jump_to_model(7, cx)))
            .on_action(cx.listener(|this, _: &JumpToModel9, _, cx| this.jump_to_model(8, cx)))
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(PickerEvent::Dismiss)))
            .child(
                h_flex()
                    .h(px(LIST_HEIGHT))
                    .items_start()
                    .when(!searching, |this| this.child(self.render_rail(&rail, has_favorites, cx)))
                    .child(
                        v_flex()
                            .w(px(LIST_WIDTH))
                            .h_full()
                            .child(
                                div().px(px(12.)).pt(px(10.)).child(
                                    h_flex()
                                        .h(px(33.))
                                        .gap(px(4.))
                                        .pb(px(6.))
                                        .border_b_1()
                                        .border_color(hsla(p.border).opacity(0.7))
                                        .child(Icon::Search.render(px(16.), hsla(p.muted_foreground).opacity(0.55)))
                                        .child(div().flex_1().h(px(26.)).theme_slot(ThemeSlot::Input, cx).child(Input::new(&self.search).appearance(false))),
                                ),
                            )
                            .when(rows == 0, |this| {
                                this.child(div().px(px(16.)).py(px(12.)).text_sm().text_color(hsla(p.muted_foreground)).child(empty_message))
                            })
                            .when(rows > 0, |this| {
                                this.child(
                                    uniform_list(
                                        "model-rows",
                                        rows,
                                        cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                            range.map(|index| this.render_row(index, cx)).collect::<Vec<_>>()
                                        }),
                                    )
                                    .track_scroll(&self.scroll)
                                    .flex_1()
                                    .px(px(8.))
                                    .pt(px(8.))
                                    .pb(px(4.)),
                                )
                            }),
                    )
                    .child(self.render_effort(model_label, window, cx)),
            )
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host;
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn right_leaves_search_only_when_the_caret_reaches_the_end(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (_, window_cx) = cx.add_window_view(|_, _| Host);
        window_cx.update(|window, cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let app = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            let picker = cx.new(|cx| ModelPicker::new(app, window, cx));
            picker.update(cx, |picker, cx| {
                let effort = serde_json::from_value(serde_json::json!({
                    "type": "select",
                    "id": "reasoningEffort",
                    "role": "reasoningEffort",
                    "label": "Reasoning",
                    "options": [{"id": "low", "label": "Low", "isDefault": true}],
                })).unwrap();
                picker.input = PickerInput { driver: "codex".into(), descriptors: vec![effort], ..PickerInput::default() };
                picker.search.update(cx, |search, cx| {
                    search.set_value("é", window, cx);
                    search.set_selected_range(0..0, cx);
                    search.focus(window, cx);
                });
                assert!(!picker.move_from_search_to_effort(window, cx));

                picker.search.update(cx, |search, cx| search.set_selected_range(2..2, cx));
                assert!(picker.move_from_search_to_effort(window, cx));
                assert!(picker.effort_focus.is_focused(window));
            });
        });
    }
}
