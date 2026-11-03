//! The model picker (design-system §7.10): provider rail, search, model list, the effort column for
//! the selected model, and a footer with speed and permissions. Ports `ProviderModelPicker.tsx`,
//! `ModelPickerContent.tsx`, `ModelListRow.tsx`, and the column and footer in
//! `ComposerModelEffort.tsx`. Choices go to the composer as `PickerEvent`s.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, ParentElement,
    Render, ScrollStrategy, SharedString, Styled, Subscription, UniformListScrollHandle, Window, actions, div,
    prelude::*, px, uniform_list,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::composer::{runtime_mode_description, runtime_mode_label};
use t3_core::model::ModelSelection;
use t3_core::providers::{self, ProviderEntry};
use t3_core::server_config::{OptionDescriptor, RuntimeMode, SelectDescriptor};
use t3_core::traits::{self, OptionSelection, OptionValue, Speed};

use super::controls::{Float, Segment, floating, menu_surface, segmented_track};
use super::runtime_mode_icon;
use crate::app_state::AppState;
use crate::provider_icon::{BadgeSize, ProviderIcon, ProviderMark};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

actions!(model_picker, [HighlightPrevious, HighlightNext, PickHighlighted]);

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
    ]);
}

/// What the composer shows for its target, handed to the picker when it opens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PickerInput {
    pub project_id: Option<String>,
    pub selection: Option<ModelSelection>,
    pub driver: String,
    pub descriptors: Vec<OptionDescriptor>,
    pub runtime_mode: RuntimeMode,
    pub runtime_modes: Vec<RuntimeMode>,
    /// A started thread that cannot switch providers keeps this instance.
    pub locked_instance: Option<String>,
}

pub enum PickerEvent {
    Model { instance_id: String, model: String },
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
    rows: Vec<Row>,
    highlighted: Option<usize>,
    permissions_open: bool,
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
            rows: Vec::new(),
            highlighted: None,
            permissions_open: false,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Resets the picker for a fresh open: search cleared, Favorites first when there are some.
    pub fn open(&mut self, input: PickerInput, window: &mut Window, cx: &mut Context<Self>) {
        let has_favorites = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|config| !providers::favorites(&config.settings).is_empty());
        let active = input.selection.as_ref().map(|selection| selection.instance_id.clone()).unwrap_or_default();
        self.tab = if has_favorites && input.locked_instance.is_none() { Tab::Favorites } else { Tab::Instance(active) };
        self.input = input;
        self.legacy_expanded = false;
        self.permissions_open = false;
        self.search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        self.refresh_rows(cx);
        self.highlighted = self.rows.iter().position(|row| matches!(row, Row::Model { selected: true, .. })).or_else(|| self.first_enabled());
        if let Some(index) = self.highlighted {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Keeps an open picker in step with the composer, such as after a level was picked or the
    /// server config changed. A closed picker rebuilds its rows when it opens.
    pub fn update_input(&mut self, input: PickerInput, config_changed: bool, cx: &mut Context<Self>) {
        if self.input != input || config_changed {
            self.input = input;
            self.refresh_rows(cx);
            cx.notify();
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_owned()
    }

    fn first_enabled(&self) -> Option<usize> {
        self.rows.iter().position(|row| matches!(row, Row::Model { disabled: None, .. }) || matches!(row, Row::Legacy { .. }))
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
        let row = |entry: &ProviderEntry, model: &t3_core::server_config::ProviderModel| {
            let favorite = providers::is_favorite(&config.settings, entry.instance_id(), &model.slug);
            let disabled = self
                .input
                .locked_instance
                .as_deref()
                .filter(|locked| *locked != entry.instance_id())
                .map(|_| SharedString::from(LOCKED_REASON));
            Row::Model {
                instance_id: entry.instance_id().to_owned(),
                slug: model.slug.clone(),
                name: providers::model_name(model).to_owned().into(),
                sub_provider: model.sub_provider.clone().map(SharedString::from),
                mark: ProviderMark::of(entry, &entries),
                badge_new: model.badge.as_deref() == Some("new"),
                favorite,
                selected: selected.is_some_and(|s| s.instance_id == entry.instance_id() && s.model == model.slug),
                disabled,
            }
        };
        let mut rows = Vec::new();
        if !query.is_empty() {
            let mut ranked: Vec<(i64, usize, Row)> = Vec::new();
            for entry in entries.iter().filter(|entry| entry.is_ready()) {
                for model in providers::model_options(&config.settings, entry.provider) {
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
                        if let Some(model) = entry.provider.models.iter().find(|model| model.slug == favorite.model) {
                            rows.push(row(entry, model));
                        }
                    }
                }
                Tab::Instance(instance_id) => {
                    if let Some(entry) = entries.iter().find(|entry| entry.instance_id() == instance_id).filter(|e| e.is_ready()) {
                        let models = providers::model_options(&config.settings, entry.provider);
                        let favorite = |slug: &str| providers::is_favorite(&config.settings, entry.instance_id(), slug);
                        let (mut current, legacy): (Vec<_>, Vec<_>) =
                            models.into_iter().partition(|model| !model.is_legacy || favorite(&model.slug));
                        current.sort_by_key(|model| !favorite(&model.slug));
                        rows.extend(current.into_iter().map(|model| row(entry, model)));
                        if !legacy.is_empty() {
                            if self.legacy_expanded {
                                rows.extend(legacy.into_iter().map(|model| row(entry, model)));
                            }
                            rows.push(Row::Legacy { expanded: self.legacy_expanded });
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
        self.tab = tab;
        self.legacy_expanded = false;
        self.refresh_rows(cx);
        self.highlighted = self.first_enabled();
        cx.notify();
    }

    fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.rows.get(index).cloned() {
            Some(Row::Model { instance_id, slug, name, disabled: None, .. }) => {
                self.state.update(cx, |state, cx| state.announce(format!("Model: {name}"), Some("model-picker"), cx));
                cx.emit(PickerEvent::Model { instance_id, model: slug });
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
            if !matches!(self.rows[index as usize], Row::Model { disabled: Some(_), .. }) {
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
            Row::Legacy { expanded } => base
                .cursor_pointer()
                .text_sm()
                .text_color(hsla(p.muted_foreground))
                .child(div().flex_1().child(if *expanded { "Show less" } else { "Show more" }))
                .child(if *expanded { Icon::ChevronUp } else { Icon::ChevronDown }.render(px(14.), hsla(p.muted_foreground)))
                .on_click(cx.listener(move |this, _, _, cx| this.activate(index, cx)))
                .into_any_element(),
            Row::Model { instance_id, slug, name, sub_provider, mark, badge_new, favorite, selected, disabled } => {
                let star_id = (instance_id.clone(), slug.clone(), *favorite);
                base.when(disabled.is_none(), |this| this.cursor_pointer())
                    .when(disabled.is_some(), |this| this.opacity(0.64))
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
                                let label = if star_id.2 { "Remove from favorites" } else { "Add to favorites" };
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
                    .child(div().size(px(14.)).flex_none().when(*selected, |this| this.child(Icon::Check.render(px(14.), hsla(p.foreground)))))
                    .when_some(disabled.clone(), |this, reason| {
                        this.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(index, cx)))
                    .into_any_element()
            }
        }
    }

    fn render_effort(&self, model_label: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let layout = traits::layout(&self.input.driver, &self.input.descriptors);
        let header = layout.effort.map(|d| d.label().to_owned()).unwrap_or_else(|| if layout.others.is_empty() { "Effort".into() } else { "Options".into() });
        let mut column = v_flex().flex_1().min_h_0().gap(px(2.)).px(px(8.)).pt(px(10.)).pb(px(6.));
        match layout.effort {
            Some(OptionDescriptor::Select(select)) => {
                column = column.children(self.level_rows(select, cx));
            }
            Some(OptionDescriptor::Boolean(boolean)) => {
                let id = boolean.id.clone();
                let current = boolean.current_value == Some(true);
                column = column.children([false, true].map(|value| {
                    let id = id.clone();
                    self.level_row(format!("effort-{value}"), if value { "On" } else { "Off" }.into(), current == value, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.change_option(&id, OptionValue::Bool(value), cx)))
                }));
            }
            None if layout.others.is_empty() => {
                column = column.child(div().px(px(8.)).py(px(6.)).text_sm().text_color(hsla(p.muted_foreground)).child("No effort levels"));
            }
            None => {}
        }
        for descriptor in layout.others {
            column = column.child(div().px(px(8.)).pt(px(6.)).pb(px(4.)).text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child(descriptor.label().to_owned()));
            match descriptor {
                OptionDescriptor::Select(select) => column = column.children(self.level_rows(select, cx)),
                OptionDescriptor::Boolean(boolean) => {
                    let id = boolean.id.clone();
                    let current = boolean.current_value == Some(true);
                    column = column.children([false, true].map(|value| {
                        let id = id.clone();
                        self.level_row(format!("{}-{value}", boolean.id), if value { "On" } else { "Off" }.into(), current == value, cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.change_option(&id, OptionValue::Bool(value), cx)))
                    }));
                }
            }
        }
        v_flex()
            .w(px(EFFORT_WIDTH))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.muted).opacity(0.4))
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
            .child(div().id("effort-scroll").flex_1().min_h_0().overflow_y_scroll().child(column))
    }

    fn level_rows(&self, select: &SelectDescriptor, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let current = traits::select_value(select);
        traits::selectable_choices(select)
            .map(|choice| {
                let (id, value) = (select.id.clone(), choice.id.clone());
                self.level_row(format!("{}-{}", select.id, choice.id), choice.label.clone().into(), current.as_deref() == Some(&choice.id), cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.change_option(&id, OptionValue::Str(value.clone()), cx)))
            })
            .collect()
    }

    fn level_row(&self, id: String, label: SharedString, checked: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
        let p = palette(cx);
        h_flex()
            .id(SharedString::from(id))
            .h(px(32.))
            .w_full()
            .flex_none()
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_sm()
            .text_color(hsla(p.foreground))
            .hover(|this| this.bg(hsla(p.foreground).opacity(0.09)))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(div().size(px(14.)).flex_none().when(checked, |this| this.child(Icon::Check.render(px(14.), hsla(p.foreground)))))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let layout = traits::layout(&self.input.driver, &self.input.descriptors);
        let speed = layout.speed.map(|speed| match speed {
            Speed::Tier(select) => {
                let current = traits::select_value(select);
                segmented_track(cx).children(traits::selectable_choices(select).map(|choice| {
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
                                        cx.notify();
                                    }))
                                    .child(permissions_section(
                                        mode,
                                        &self.input.runtime_modes,
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

const LOCKED_REASON: &str =
    "This thread does not support switching providers after it has started. Start a new thread to use this model.";

/// `ComposerPermissionsSection`: the modes as one segmented row, the picked one named below.
pub fn permissions_section<V: 'static>(
    mode: RuntimeMode,
    modes: &[RuntimeMode],
    cx: &mut Context<V>,
    on_pick: impl Fn(RuntimeMode, &mut V, &mut Context<V>) + Clone + 'static,
) -> impl IntoElement {
    let p = palette(cx);
    v_flex()
        .w(px(288.))
        .gap(px(6.))
        .px(px(8.))
        .py(px(6.))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.muted_foreground)).child("Permissions"))
        .child(segmented_track(cx).children(RuntimeMode::ALL.into_iter().map(|option| {
            let supported = modes.contains(&option);
            let on_pick = on_pick.clone();
            Segment::new(SharedString::from(format!("permission-{}", option.as_str())), option == mode)
                .icon(runtime_mode_icon(option))
                .disabled(!supported)
                .tooltip(format!("{}. {}", runtime_mode_label(option), runtime_mode_description(option)))
                .on_click(cx.listener(move |this, _, _, cx| on_pick(option, this, cx)))
        })))
        .child(
            v_flex()
                .gap(px(2.))
                .child(div().text_sm().text_color(hsla(p.foreground)).child(runtime_mode_label(mode)))
                .child(div().min_h(px(32.)).text_xs().line_height(px(16.)).text_color(hsla(p.muted_foreground)).child(runtime_mode_description(mode))),
        )
}

impl Render for ModelPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    let model = config?.provider(&selection.instance_id)?.models.iter().find(|m| m.slug == selection.model)?;
                    Some(providers::trigger_model_name(model).to_owned())
                })
                .unwrap_or_default()
                .into();
            let empty_message: SharedString = match (&self.tab, config) {
                (_, None) => "Loading providers…".into(),
                (Tab::Instance(id), Some(config)) if !searching => entries
                    .iter()
                    .find(|entry| entry.instance_id() == id && !entry.is_ready())
                    .map(|entry| entry.provider.message.clone().unwrap_or_else(|| format!("{} is not ready.", entry.display_name)))
                    .unwrap_or_else(|| {
                        let _ = config;
                        "No models found".to_owned()
                    })
                    .into(),
                _ => "No models found".into(),
            };
            (rail, has_favorites, model_label, empty_message)
        };
        let rows = self.rows.len();
        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .occlude()
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.dropdown_border))
            .bg(hsla(p.popover))
            .text_color(hsla(p.popover_foreground))
            .shadow(super::controls::floating_shadow(cx.theme().is_dark()))
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &HighlightPrevious, _, cx| this.move_highlight(-1, cx)))
            .on_action(cx.listener(|this, _: &HighlightNext, _, cx| this.move_highlight(1, cx)))
            .on_action(cx.listener(|this, _: &PickHighlighted, _, cx| {
                if let Some(index) = this.highlighted {
                    this.activate(index, cx);
                }
            }))
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
                                        .child(div().flex_1().h(px(26.)).child(Input::new(&self.search).appearance(false))),
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
                    .child(self.render_effort(model_label, cx)),
            )
            .child(self.render_footer(cx))
    }
}
