//! Appearance: color scheme tiles and the theme library (`ThemeSettings.tsx` `ThemeLibrary`), chat
//! width (`AppearanceSettingsPanel.tsx`) and typography (`TypographySettings.tsx`).
//! Choices apply live through `crate::theme`; editing is hosted by `crate::theme_editor`.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, App, AppContext, Context, ElementId, Entity, FontWeight, Hsla, IntoElement, ParentElement, Styled,
    Subscription, Window, div, ease_out_quint, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::client_settings::{
    AppearanceMode, ChatWidth, ClientSettingChange, DEFAULT_PANEL_ANIMATION_DURATION_MS, EffortDisplay,
    EnvironmentIdentificationMode, MAX_PANEL_ANIMATION_DURATION_MS, MIN_PANEL_ANIMATION_DURATION_MS, ThemePreference,
    environment_identification_options, resolve_sidebar_stage_backdrop_variant,
};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, r, section, section_title};
use super::{Block, SettingsView, store};
use crate::icons::Icon;
use crate::theme::{self, hsla, palette, swatch};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use t3_core::themes::{self, ThemeDefinition};

const THREAD_DETAILS_MENU_REOPEN_GUARD: Duration = Duration::from_millis(250);

pub(super) struct AppearanceState {
    sliders: [Entity<SliderState>; 3],
    _subscriptions: Vec<Subscription>,
    removal: Option<ThemeDefinition>,
    pub(super) import: Option<Entity<super::theme_import::ThemeImportDialog>>,
    removal_ids: Vec<String>,
    variants: std::collections::BTreeMap<String, String>,
    thread_details_menu_open: bool,
    thread_details_menu_dismissed: Option<Instant>,
    advanced_interface: bool,
    panel_preview_open: bool,
    panel_preview_seq: u64,
}
impl AppearanceState {
    pub fn new(cx: &mut Context<SettingsView>) -> Self {
        let client = store::client(cx);
        let values = [
            client.appearance_contrast(),
            client.glass_opacity(),
            client.panel_animation_duration_ms(),
        ];
        let sliders = std::array::from_fn(|i| {
            let (min, max, step) = match i {
                0 => (50., 200., 5.),
                1 => (40., 100., 5.),
                _ => (MIN_PANEL_ANIMATION_DURATION_MS as f32, MAX_PANEL_ANIMATION_DURATION_MS as f32, 25.),
            };
            cx.new(|_| SliderState::new().min(min).max(max).step(step).default_value(values[i] as f32))
        });
        let subscriptions = sliders
            .iter()
            .enumerate()
            .map(|(i, slider)| {
                cx.subscribe(slider, move |view, _, event: &SliderEvent, cx| {
                    if let SliderEvent::Change(value) = event {
                        let value = value.start().round() as u32;
                        let change = match i {
                            0 => ClientSettingChange::AppearanceContrast(value),
                            1 => ClientSettingChange::GlassOpacity(value),
                            _ => ClientSettingChange::PanelAnimationDurationMs(value),
                        };
                        store::update_client(&view.state, change, cx);
                    }
                })
            })
            .collect();
        Self {
            sliders,
            _subscriptions: subscriptions,
            removal: None,
            import: None,
            removal_ids: Vec::new(),
            variants: std::collections::BTreeMap::new(),
            thread_details_menu_open: false,
            thread_details_menu_dismissed: None,
            advanced_interface: false,
            panel_preview_open: true,
            panel_preview_seq: 0,
        }
    }
}

impl SettingsView {
    pub(super) fn appearance(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<Block> {
        let doc = store::client(cx).clone();
        let mode = doc.appearance_mode();
        let theme = doc.theme();
        let p = palette(cx);

        let tiles = h_flex().w_full().gap(r(12.)).children(AppearanceMode::ALL.iter().map(|tile_mode| {
            let active = *tile_mode == mode;
            let state = self.state.clone();
            let tile_mode = *tile_mode;
            v_flex()
                .id(ElementId::Name(format!("appearance-{}", tile_mode.as_str()).into()))
                .flex_1()
                .gap(r(6.))
                .p(r(8.))
                .rounded(r(12.))
                .border_1()
                .cursor_pointer()
                .map(|this| match active {
                    true => this.border_color(hsla(p.ring)).bg(hsla(p.accent).opacity(0.3)).shadow(ring(hsla(p.ring))),
                    false => this
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.card).opacity(0.6))
                        .hover(|this| this.bg(hsla(p.accent).opacity(0.1))),
                })
                .child(wireframe(&theme, tile_mode, cx))
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if active { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                        .child(tile_mode.label()),
                )
                .on_click(move |_, _, cx| store::update_client(&state, ClientSettingChange::AppearanceMode(tile_mode), cx))
        }));

        let custom = theme::preferences(cx).custom.clone();
        let mut seen = std::collections::BTreeSet::new();
        let shown_custom = custom
            .iter()
            .filter_map(|t| {
                let Some(collection) = &t.collection else {
                    return Some(t.clone());
                };
                if !seen.insert(collection.id.clone()) {
                    return None;
                }
                Some(
                    self.appearance_ui
                        .variants
                        .get(&collection.id)
                        .and_then(|id| custom.iter().find(|t| t.id == id.split(':').next().unwrap_or(id)))
                        .or_else(|| {
                            custom.iter().find(|candidate| {
                                candidate.collection.as_ref().is_some_and(|c| c.id == collection.id)
                                    && [false, true].into_iter().any(|dark| {
                                        themes::half_theme(&theme, &themes::halves(&doc), dark, &custom, theme::environment_themes(cx))
                                            .is_some_and(|owner| owner.id == candidate.id)
                                    })
                            })
                        })
                        .unwrap_or(t)
                        .clone(),
                )
            })
            .collect::<Vec<_>>();
        let environment =
            theme::environment_themes(cx).iter().filter(|t| !custom.iter().any(|c| c.id == t.id)).cloned().collect::<Vec<_>>();
        let cards = std::iter::once((ThemePreference::Default(mode), "T3 Code".to_owned(), None))
            .chain(
                themes::builtins()
                    .iter()
                    .chain(environment.iter())
                    .chain(shown_custom.iter())
                    .map(|t| (ThemePreference::Named(t.id.clone()), t.label.clone(), Some(t.clone()))),
            )
            .collect::<Vec<_>>();
        let rendered_cards: Vec<_> = cards
            .into_iter()
            .map(|(card, label, definition)| {
                let halves = themes::halves(&doc);
                let light = themes::half_theme(&theme, &halves, false, &custom, theme::environment_themes(cx)).map(|t| t.id.as_str())
                    == card.named_id();
                let mut body = v_flex()
                    .id(ElementId::Name(format!("theme-card-{label}").into()))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .overflow_hidden()
                    .bg(hsla(p.card).opacity(0.6))
                    .child(h_flex().h(r(80.)).justify_center().gap(r(12.)).children(
                        [false, true].into_iter().filter(|dark| definition.as_ref().is_none_or(|t| t.half(*dark).is_some())).map(|dark| {
                            let state = self.state.clone();
                            let id = card.named_id().map(str::to_owned);
                            div()
                                .id(ElementId::Name(format!("theme-half-{label}-{dark}").into()))
                                .cursor_pointer()
                                .child(preview_circle(
                                    &card,
                                    dark,
                                    if dark {
                                        themes::half_theme(&theme, &halves, true, &custom, theme::environment_themes(cx))
                                            .map(|t| t.id.as_str())
                                            == card.named_id()
                                    } else {
                                        light
                                    },
                                    cx,
                                ))
                                .on_click(move |_, _, cx| {
                                    let patch = themes::assign_half(store::client(cx), dark, id.as_deref());
                                    store::update_client(&state, ClientSettingChange::ThemePatch(patch), cx);
                                })
                        }),
                    ))
                    .child(div().px(r(12.)).text_sm().font_weight(FontWeight::MEDIUM).truncate().child(label));
                if let Some(collection) = definition.as_ref().and_then(|t| t.collection.as_ref()) {
                    let weak = cx.entity().downgrade();
                    let collection_id = collection.id.clone();
                    let collection_label = collection.label.clone();
                    let variants = custom
                        .iter()
                        .filter(|t| t.collection.as_ref().is_some_and(|c| c.id == collection.id))
                        .flat_map(|t| {
                            [false, true].into_iter().filter(|dark| t.half(*dark).is_some()).map(move |dark| {
                                SelectOption::new(
                                    format!("{}:{}", t.id, if dark { "dark" } else { "light" }),
                                    format!("{} ({})", t.label, if dark { "Dark" } else { "Light" }),
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    if variants.len() > 1 {
                        body = body.child(
                            div().px(r(8.)).pt(r(4.)).child(
                                Select::new(ElementId::Name(format!("variants-{collection_id}").into()), variants)
                                    .selected(self.appearance_ui.variants.get(&collection.id).cloned().unwrap_or_else(|| {
                                        format!("{}:{}", card.named_id().unwrap_or(""), definition.as_ref().unwrap().appearance)
                                    }))
                                    .label(collection_label)
                                    .width(220.)
                                    .on_change(move |id, _, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            if let Some((theme_id, mode)) = id.split_once(':') {
                                                let patch = themes::assign_half(store::client(cx), mode == "dark", Some(theme_id));
                                                store::update_client(&this.state, ClientSettingChange::ThemePatch(patch), cx);
                                                this.appearance_ui.variants.insert(collection_id.clone(), id.to_string());
                                            }
                                            cx.notify();
                                        });
                                    }),
                            ),
                        );
                    }
                }
                let state = self.state.clone();
                let chosen = card.clone();
                let use_definition = definition.clone();
                let collection = definition.as_ref().and_then(|t| t.collection.as_ref()).map(|c| c.id.clone());
                let collection_themes = collection
                    .as_ref()
                    .map(|id| custom.iter().filter(|t| t.collection.as_ref().is_some_and(|c| c.id == *id)).cloned().collect::<Vec<_>>())
                    .unwrap_or_default();

                let mut actions = h_flex().px(r(8.)).py(r(8.)).gap(r(4.)).child(
                    Button::new(
                        ElementId::Name(format!("theme-use-{}", card.named_id().unwrap_or("default")).into()),
                        ButtonVariant::Outline,
                        ButtonSize::Xs,
                    )
                    .label("Use")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if collection_themes.len() > 1 {
                            let patch = themes::select_collection(store::client(cx), &collection_themes);
                            store::update_client(&state, ClientSettingChange::ThemePatch(patch), cx);
                            if let Some(id) = &collection {
                                this.appearance_ui.variants.remove(id);
                            }
                            cx.notify();
                        } else if let Some(t) = &use_definition
                            && t.variants.is_empty()
                        {
                            let patch = themes::assign_half(store::client(cx), t.appearance == "dark", Some(&t.id));
                            store::update_client(&state, ClientSettingChange::ThemePatch(patch), cx);
                        } else {
                            store::update_client(&state, ClientSettingChange::Theme(chosen.clone()), cx);
                        }
                    })),
                );
                if let Some(definition) = definition {
                    let duplicate = definition.clone();
                    actions = actions.child(
                        Button::new(
                            ElementId::Name(format!("duplicate-{}", definition.id).into()),
                            ButtonVariant::GhostMuted,
                            ButtonSize::IconXs,
                        )
                        .icon(crate::icons::Icon::Copy)
                        .tooltip("Duplicate")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            crate::theme_editor::open(None, Some(duplicate.clone()), Some(format!("{} copy", duplicate.label)), cx);
                        })),
                    );
                    if custom.iter().any(|t| t.id == definition.id) {
                        let editing=definition.clone();
                        actions=actions.child(Button::new(ElementId::Name(format!("edit-{}",definition.id).into()),ButtonVariant::GhostMuted,ButtonSize::IconXs)
                            .icon(crate::icons::Icon::Pencil).tooltip("Edit theme").on_click(move |_,_,cx|crate::theme_editor::open(Some(editing.clone()),None,None,cx)));
                        let export = definition.clone();
                        actions = actions.child(
                            Button::new(
                                ElementId::Name(format!("export-{}", definition.id).into()),
                                ButtonVariant::GhostMuted,
                                ButtonSize::IconXs,
                            )
                            .icon(crate::icons::Icon::Download)
                            .tooltip("Export theme")
                            .on_click(cx.listener(move |this, _, window, cx| this.export_theme(export.clone(), window, cx))),
                        );
                        actions = actions.child(
                            Button::new(
                                ElementId::Name(format!("remove-{}", definition.id).into()),
                                ButtonVariant::DestructiveOutline,
                                ButtonSize::IconXs,
                            )
                            .icon(crate::icons::Icon::Trash2)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let count = themes::library(store::client(cx))
                                    .iter()
                                    .filter(|t| {
                                        definition
                                            .collection
                                            .as_ref()
                                            .is_some_and(|c| t.collection.as_ref().is_some_and(|other| other.id == c.id))
                                    })
                                    .count();
                                this.appearance_ui.removal_ids = if count > 1 { Vec::new() } else { vec![definition.id.clone()] };
                                this.appearance_ui.removal = Some(definition.clone());
                                cx.notify();
                            })),
                        );
                    }
                }
                if card.named_id().is_none() {
                    actions = actions.child(
                        Button::new("duplicate-default", ButtonVariant::GhostMuted, ButtonSize::IconXs)
                            .icon(crate::icons::Icon::Copy)
                            .tooltip("Duplicate")
                            .on_click(cx.listener(|_, _, _, cx| {
                                crate::theme_editor::open(None, None, Some("T3 Code copy".into()), cx);
                            })),
                    );
                }
                body.child(actions).flex_1().min_w_0().into_any_element()
            })
            .collect();
        let count = rendered_cards.len();
        let mut cards = rendered_cards.into_iter();
        let grid = v_flex().w_full().gap(r(8.)).children((0..count.div_ceil(3)).map(|row| {
            let filled = (count - row * 3).min(3);
            h_flex().w_full().items_start().gap(r(8.)).children(cards.by_ref().take(3)).children((filled..3).map(|_| div().flex_1()))
        }));

        let colors = v_flex()
            .gap(r(12.))
            .child(section_title("Color scheme"))
            .child(tiles)
            .child(
                h_flex().pt(r(20.)).justify_between().child(section_title("Themes")).child(h_flex().gap(r(8.)).child(
                    Button::new("create-theme",ButtonVariant::Outline,ButtonSize::Xs).label("Create theme")
                        .on_click(|_,_,cx|crate::theme_editor::open(None,None,None,cx)),
                ).child(
                    Button::new("import-theme", ButtonVariant::Outline, ButtonSize::Xs)
                        .label("Add theme")
                        .on_click(cx.listener(|this, _, window, cx| this.open_theme_import(window, cx))),
                )),
            )
            .child(grid)
            .when_some(self.appearance_ui.removal.clone(), |this, target| {
                let variants = custom
                    .iter()
                    .filter(|t| target.collection.as_ref().is_some_and(|c| t.collection.as_ref().is_some_and(|other| other.id == c.id)))
                    .cloned()
                    .collect::<Vec<_>>();
                let multiple = variants.len() > 1;
                let title = if multiple {
                    format!("Remove themes from “{}”?", target.collection.as_ref().unwrap().label)
                } else {
                    format!("Remove “{}”?", target.label)
                };
                let mut confirmation = v_flex().p(r(16.)).gap(r(12.)).child(section_title(title)).child(super::controls::muted_text(
                    if multiple {
                        "Select the variants you want to remove. You can restore them by importing the extension again."
                    } else {
                        "You can bring it back anytime by importing its JSON file."
                    },
                    cx,
                ));
                if multiple {
                    confirmation = confirmation.children(variants.into_iter().map(|t| {
                        let checked = self.appearance_ui.removal_ids.contains(&t.id);
                        let id = t.id.clone();
                        let weak = cx.entity().downgrade();
                        h_flex().justify_between().child(super::controls::muted_text(t.label, cx)).child(
                            super::controls::Switch::new(ElementId::Name(format!("remove-variant-{id}").into()), checked).on_change(
                                move |checked, _, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        if checked {
                                            this.appearance_ui.removal_ids.push(id.clone());
                                        } else {
                                            this.appearance_ui.removal_ids.retain(|i| i != &id);
                                        }
                                        cx.notify();
                                    });
                                },
                            ),
                        )
                    }));
                }
                let ids = self.appearance_ui.removal_ids.clone();
                let count = ids.len();
                this.child(
                    confirmation.child(
                        h_flex()
                            .gap(r(8.))
                            .child(Button::new("cancel-theme-remove", ButtonVariant::Outline, ButtonSize::Sm).label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.appearance_ui.removal = None;
                                    cx.notify();
                                }),
                            ))
                            .child(
                                Button::new("confirm-theme-remove", ButtonVariant::DestructiveOutline, ButtonSize::Sm)
                                    .label(if multiple { format!("Remove selected ({count})") } else { "Remove theme".into() })
                                    .disabled(count == 0)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        match themes::remove(store::client(cx), &ids) {
                                            Ok(patch) => {
                                                store::update_client(&this.state, ClientSettingChange::ThemePatch(patch), cx);
                                                this.appearance_ui.removal = None;
                                            }
                                            Err(e) => store::show_error(e, cx),
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
                )
            });

        let width_state = self.state.clone();
        let effort_state = self.state.clone();
        let effort_reset_state = self.state.clone();
        let effort_display = doc.effort_display();
        let math_enabled = doc.math_rendering_enabled();
        let math_state = self.state.clone();
        let math_reset_state = self.state.clone();
        let advanced_targeted = [
            "panel-animations",
            "environment-identification",
            "appearance-interface",
            "composer-context",
        ]
        .iter()
        .any(|id| self.is_target(id));
        let advanced_open = self.appearance_ui.advanced_interface || advanced_targeted;
        let mut interface_rows = vec![
            self.percent_row(false, cx),
            self.percent_row(true, cx),
            Row::new("chat-width", "Chat width")
                .description("Set how wide messages and the composer can grow on large screens.")
                .highlighted(self.is_target("chat-width"))
                .reset(doc.chat_width() != ChatWidth::default(), {
                    let state = self.state.clone();
                    move |_, _, cx| store::update_client(&state, ClientSettingChange::ChatWidth(ChatWidth::default()), cx)
                })
                .control(
                    Select::new(
                        "chat-width-select",
                        vec![
                            SelectOption::new("comfortable", "Comfortable (default)"),
                            SelectOption::new("wide", "Wide"),
                            SelectOption::new("full", "Full"),
                        ],
                    )
                    .selected(doc.chat_width().as_str())
                    .label(doc.chat_width().label())
                    .width(160.)
                    .show_check()
                    .on_change(move |value, _, cx| {
                        if let Some(width) = ChatWidth::parse(&value) {
                            store::update_client(&width_state, ClientSettingChange::ChatWidth(width), cx);
                        }
                    }),
                )
                .into_any_element(),
            self.thread_details_items_row(&doc, cx),
            Row::new("effort-display", "Effort display")
                .description("Show reasoning effort in the composer as its name, such as High, or as bars.")
                .highlighted(self.is_target("effort-display"))
                .reset(effort_display != EffortDisplay::default(), move |_, _, cx| {
                    store::update_client(&effort_reset_state, ClientSettingChange::EffortDisplay(EffortDisplay::default()), cx)
                })
                .control(
                    div()
                        .debug_selector(|| "effort-display-control".to_owned())
                        .child(
                            Select::new(
                                "effort-display-select",
                                vec![
                                    SelectOption::new("label", "Word (default)"),
                                    SelectOption::new("bars", "Bars"),
                                ],
                            )
                            .selected(effort_display.as_str())
                            .label(effort_display.label())
                            .width(160.)
                            .show_check()
                            .on_change(move |value, _, cx| {
                                if let Some(display) = EffortDisplay::parse(&value) {
                                    store::update_client(&effort_state, ClientSettingChange::EffortDisplay(display), cx);
                                }
                            }),
                        ),
                )
                .into_any_element(),
            Row::new("math-rendering", "Render math")
                .description("Typeset LaTeX math, such as $x^2$ or \\[x^2\\], in messages and Markdown previews. The composer keeps TeX source editable.")
                .highlighted(self.is_target("math-rendering"))
                .reset(math_enabled, move |_, _, cx| {
                    store::update_client(&math_reset_state, ClientSettingChange::MathRenderingEnabled(false), cx)
                })
                .control(
                    Switch::new("math-rendering-switch", math_enabled).on_change(move |checked, _, cx| {
                        store::update_client(&math_state, ClientSettingChange::MathRenderingEnabled(checked), cx);
                    }),
                )
                .into_any_element(),
            div()
                .px(r(16.))
                .py(r(8.))
                .debug_selector(|| "appearance-interface-advanced".to_owned())
                .child(
                    Button::new("appearance-interface-advanced", ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .icon(if advanced_open { Icon::ChevronDown } else { Icon::ChevronRight })
                        .label("Advanced")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.appearance_ui.advanced_interface = !advanced_open;
                            this.target = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        ];
        if advanced_open {
            interface_rows.push(self.panel_animations_row(&doc, cx));
            interface_rows.push(self.environment_identification_row(&doc, cx));
            interface_rows.push(self.composer_context_row(&doc));
        }
        let interface = section("Interface", interface_rows, cx);

        let typography = self.typography_rows(window, cx);

        vec![
            Block::new(&["appearance", "color-scheme", "theme"], colors),
            Block::new(
                &[
                    "appearance-interface",
                    "setting-appearance-contrast",
                    "setting-glass-opacity",
                    "chat-width",
                    "thread-details-items",
                    "effort-display",
                    "math-rendering",
                    "panel-animations",
                    "environment-identification",
                    "composer-context",
                ],
                interface,
            ),
            Block::new(
                &[
                    "typography",
                    "interface-font",
                    "prompt-font",
                    "code-font",
                    "terminal-font",
                    "word-wrap",
                    "separate-fonts",
                    "font-smoothing",
                ],
                typography,
            ),
        ]
    }

    fn thread_details_items_row(
        &self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let enabled = doc.thread_details_enabled_count();
        let total = t3_core::thread_details::ITEMS.len();
        let is_default = doc.thread_details_is_default();
        let reset_state = self.state.clone();
        let menu_open = self.appearance_ui.thread_details_menu_open;
        let popover = menu_open.then(|| {
            let menu_reset_state = self.state.clone();
            let items = t3_core::thread_details::ITEMS.iter().map(|(id, label, _)| {
                let item_id = (*id).to_owned();
                let item_state = self.state.clone();
                let shown = doc.thread_details_item_visible(id);
                div()
                    .debug_selector({
                        let id = *id;
                        move || format!("thread-details-item-{id}")
                    })
                    .child(
                        crate::composer::controls::MenuItem::new(
                            ElementId::Name(format!("thread-details-item-{id}").into()),
                            *label,
                        )
                        .checked(shown)
                        .on_click(move |_, _, cx| {
                            store::update_client(
                                &item_state,
                                ClientSettingChange::ThreadDetailsItem { id: item_id.clone(), visible: !shown },
                                cx,
                            );
                            if let Some(details) = crate::thread_details::details(cx) {
                                let updated = store::client(cx).clone();
                                details.update(cx, |details, cx| details.sync_from_client_settings(&updated, true, cx));
                            }
                        }),
                    )
            });
            gpui::deferred(
                gpui::anchored()
                    .anchor(gpui::Anchor::TopRight)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        v_flex()
                            .id("thread-details-items-popover")
                            .debug_selector(|| "thread-details-items-popover".to_owned())
                            .occlude()
                            .mt(r(4.))
                            .w(r(232.))
                            .p(r(4.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .text_color(hsla(p.popover_foreground))
                            .shadow(crate::composer::controls::floating_shadow(dark))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                if this.appearance_ui.thread_details_menu_open {
                                    this.appearance_ui.thread_details_menu_open = false;
                                    this.appearance_ui.thread_details_menu_dismissed = Some(Instant::now());
                                    cx.notify();
                                }
                            }))
                            .child(
                                div()
                                    .px(r(8.))
                                    .py(r(6.))
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.muted_foreground))
                                    .child("Show in thread details"),
                            )
                            .children(items)
                            .child(div().h(px(1.)).my(r(4.)).bg(hsla(p.border)))
                            .child(
                                div().debug_selector(|| "thread-details-menu-reset".to_owned()).child(
                                    crate::composer::controls::MenuItem::new("thread-details-menu-reset", "Reset to default")
                                        .disabled(is_default)
                                        .on_click(move |_, _, cx| {
                                            store::update_client(
                                                &menu_reset_state,
                                                ClientSettingChange::ResetThreadDetailsItems,
                                                cx,
                                            );
                                            if let Some(details) = crate::thread_details::details(cx) {
                                                let updated = store::client(cx).clone();
                                                details.update(cx, |details, cx| details.sync_from_client_settings(&updated, true, cx));
                                            }
                                        }),
                                ),
                            ),
                    ),
            )
            .with_priority(2)
        });
        Row::new("thread-details-items", "Thread details")
            .description("Choose what the thread details card shows. You can also right-click the card.")
            .highlighted(self.is_target("thread-details-items"))
            .reset(!is_default, move |_, _, cx| {
                store::update_client(&reset_state, ClientSettingChange::ResetThreadDetailsItems, cx);
                if let Some(details) = crate::thread_details::details(cx) {
                    let updated = store::client(cx).clone();
                    details.update(cx, |details, cx| details.sync_from_client_settings(&updated, true, cx));
                }
            })
            .control(
                div()
                    .relative()
                    .w(r(160.))
                    .debug_selector(|| "thread-details-items-trigger".to_owned())
                    .child(
                        Button::new("thread-details-items-trigger", ButtonVariant::Outline, ButtonSize::Sm)
                            .label(format!("{enabled} of {total} on"))
                            .trailing(Icon::ChevronDown)
                            .full_width()
                            .on_click(cx.listener(|this, _, _, cx| {
                                let just_dismissed = this
                                    .appearance_ui
                                    .thread_details_menu_dismissed
                                    .take()
                                    .is_some_and(|at| at.elapsed() < THREAD_DETAILS_MENU_REOPEN_GUARD);
                                this.appearance_ui.thread_details_menu_open =
                                    !(just_dismissed || this.appearance_ui.thread_details_menu_open);
                                cx.notify();
                            })),
                    )
                    .children(popover),
            )
            .into_any_element()
    }

    fn panel_animations_row(
        &self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = palette(cx);
        let duration_ms = doc.panel_animation_duration_ms();
        let slider = self.appearance_ui.sliders[2].clone();
        let reset_slider = slider.clone();
        let reset_state = self.state.clone();
        let preview_open = self.appearance_ui.panel_preview_open;
        let preview_seq = self.appearance_ui.panel_preview_seq;
        let animate = duration_ms > 0 && preview_seq > 0;
        let lerp = move |open_size: f32, delta: f32| {
            if preview_open { open_size * delta } else { open_size * (1.0 - delta) }
        };
        let sidebar_strip = div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .rounded(r(4.))
            .bg(hsla(p.sidebar));
        let sidebar_strip: gpui::AnyElement = if animate {
            sidebar_strip
                .with_animation(
                    ElementId::Name(format!("panel-preview-left-{preview_seq}").into()),
                    Animation::new(Duration::from_millis(u64::from(duration_ms))).with_easing(ease_out_quint()),
                    move |el, delta| el.w(px(lerp(16., delta))),
                )
                .into_any_element()
        } else {
            sidebar_strip.w(px(if preview_open { 16. } else { 0. })).into_any_element()
        };
        let drawer_strip = h_flex()
            .flex_none()
            .items_center()
            .overflow_hidden()
            .bg(hsla(p.foreground).opacity(0.05))
            .px(px(6.))
            .child(div().h(px(1.)).w(gpui::relative(0.66)).rounded_full().bg(hsla(p.muted_foreground).opacity(0.25)));
        let drawer_strip: gpui::AnyElement = if animate {
            drawer_strip
                .border_t_1()
                .border_color(hsla(p.border).opacity(0.7))
                .with_animation(
                    ElementId::Name(format!("panel-preview-bottom-{preview_seq}").into()),
                    Animation::new(Duration::from_millis(u64::from(duration_ms))).with_easing(ease_out_quint()),
                    move |el, delta| el.h(px(lerp(8., delta))),
                )
                .into_any_element()
        } else {
            drawer_strip
                .when(preview_open, |this| this.h(px(8.)).border_t_1().border_color(hsla(p.border).opacity(0.7)))
                .when(!preview_open, |this| this.h(px(0.)))
                .into_any_element()
        };
        let right_strip = div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .rounded(r(4.))
            .bg(hsla(p.muted));
        let right_strip: gpui::AnyElement = if animate {
            right_strip
                .with_animation(
                    ElementId::Name(format!("panel-preview-right-{preview_seq}").into()),
                    Animation::new(Duration::from_millis(u64::from(duration_ms))).with_easing(ease_out_quint()),
                    move |el, delta| el.w(px(lerp(20., delta))),
                )
                .into_any_element()
        } else {
            right_strip.w(px(if preview_open { 20. } else { 0. })).into_any_element()
        };
        let preview = h_flex()
            .id("panel-animations-preview")
            .debug_selector(|| "panel-animations-preview".to_owned())
            .w(r(112.))
            .h(r(40.))
            .p(r(4.))
            .overflow_hidden()
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .cursor_pointer()
            .tooltip(super::controls::tooltip("Replay panel animation preview".into()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.appearance_ui.panel_preview_open = !this.appearance_ui.panel_preview_open;
                this.appearance_ui.panel_preview_seq = this.appearance_ui.panel_preview_seq.wrapping_add(1);
                cx.notify();
            }))
            .child(sidebar_strip)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px(px(4.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap(px(4.))
                            .pt(px(4.))
                            .child(div().h(px(2.)).w_full().rounded_full().bg(hsla(p.muted_foreground).opacity(0.25)))
                            .child(div().h(px(2.)).w(gpui::relative(0.8)).rounded_full().bg(hsla(p.muted_foreground).opacity(0.2)))
                            .child(div().h(px(2.)).w(gpui::relative(0.6)).rounded_full().bg(hsla(p.muted_foreground).opacity(0.15))),
                    )
                    .child(drawer_strip),
            )
            .child(right_strip);

        Row::new("panel-animations", "Panel animations")
            .description("Set how fast panels open and close.")
            .highlighted(self.is_target("panel-animations"))
            .reset(duration_ms != DEFAULT_PANEL_ANIMATION_DURATION_MS, move |_, window, cx| {
                reset_slider.update(cx, |s, cx| s.set_value(DEFAULT_PANEL_ANIMATION_DURATION_MS as f32, window, cx));
                store::update_client(
                    &reset_state,
                    ClientSettingChange::PanelAnimationDurationMs(DEFAULT_PANEL_ANIMATION_DURATION_MS),
                    cx,
                );
            })
            .control(
                h_flex()
                    .gap(r(12.))
                    .items_center()
                    .child(preview)
                    .child(DurationSlider::new(slider, duration_ms)),
            )
            .into_any_element()
    }

    fn environment_identification_row(
        &self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let stage_label = crate::sidebar::environment_stage_label(self.state.read(cx));
        let stage_variant = resolve_sidebar_stage_backdrop_variant(stage_label, EnvironmentIdentificationMode::Artwork);
        let options = environment_identification_options(stage_variant);
        let current = doc.environment_identification_mode();
        let effective_mode = crate::sidebar::effective_environment_identification_mode(cx);
        let theme_hides_artwork = effective_mode == EnvironmentIdentificationMode::Pill
            && resolve_sidebar_stage_backdrop_variant(stage_label, current).is_some();
        let description = if theme_hides_artwork {
            if stage_variant.is_some() {
                "Only built-in themes show artwork, so this theme shows the version pill."
            } else {
                "Only built-in themes show artwork."
            }
        } else if stage_variant.is_some() {
            "Choose how Dev and Nightly environments are identified."
        } else {
            "Show Dev or Nightly artwork in the sidebar header."
        };
        let label = options
            .iter()
            .find(|(value, _)| *value == current)
            .map_or_else(|| current.label(), |(_, label)| *label);
        let change_state = self.state.clone();
        let reset_state = self.state.clone();
        Row::new("environment-identification", "Environment identification")
            .description(description)
            .highlighted(self.is_target("environment-identification") || self.is_target("appearance-interface"))
            .reset(current != EnvironmentIdentificationMode::default(), move |_, _, cx| {
                store::update_client(
                    &reset_state,
                    ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::default()),
                    cx,
                );
            })
            .control(
                div()
                    .debug_selector(|| "environment-identification-control".to_owned())
                    .child(
                        Select::new(
                            "environment-identification-select",
                            options
                                .iter()
                                .map(|(value, option_label)| SelectOption::new(value.as_str(), *option_label))
                                .collect(),
                        )
                        .selected(current.as_str())
                        .label(label)
                        .width(160.)
                        .on_change(move |value, _, cx| {
                            if let Some(mode) = EnvironmentIdentificationMode::parse(&value) {
                                store::update_client(
                                    &change_state,
                                    ClientSettingChange::EnvironmentIdentificationMode(mode),
                                    cx,
                                );
                            }
                        }),
                    ),
            )
            .into_any_element()
    }

    fn composer_context_row(
        &self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
    ) -> gpui::AnyElement {
        let persist = doc.persist_composer_context_strip();
        let toggle_state = self.state.clone();
        let reset_state = self.state.clone();
        Row::new("composer-context", "Composer context")
            .description("Keep branch and worktree controls below the composer after a thread starts.")
            .highlighted(self.is_target("composer-context"))
            .reset(persist, move |_, _, cx| {
                store::update_client(&reset_state, ClientSettingChange::PersistComposerContextStrip(false), cx)
            })
            .control(
                div()
                    .debug_selector(|| "composer-context-switch".to_owned())
                    .child(Switch::new("composer-context-switch", persist).on_change(move |checked, _, cx| {
                        store::update_client(&toggle_state, ClientSettingChange::PersistComposerContextStrip(checked), cx);
                    })),
            )
            .into_any_element()
    }
    fn percent_row(&self, glass: bool, cx: &App) -> gpui::AnyElement {
        let doc = store::client(cx);
        let (id, title, description, value, default) = if glass {
            (
                "setting-glass-opacity",
                "Glass opacity",
                "Higher values make menus, dialogs, and the composer more solid.",
                doc.glass_opacity(),
                80,
            )
        } else {
            (
                "setting-appearance-contrast",
                "Contrast",
                "Adjust the contrast of colors and borders across the interface.",
                doc.appearance_contrast(),
                100,
            )
        };
        let state = self.state.clone();
        let slider = self.appearance_ui.sliders[usize::from(glass)].clone();
        let reset_slider = slider.clone();
        Row::new(id, title)
            .description(description)
            .highlighted(self.is_target(id))
            .reset(value != default, move |_, window, cx| {
                reset_slider.update(cx, |s, cx| s.set_value(default as f32, window, cx));
                store::update_client(
                    &state,
                    if glass { ClientSettingChange::GlassOpacity(default) } else { ClientSettingChange::AppearanceContrast(default) },
                    cx,
                );
            })
            .control(super::controls::PercentSlider::new(slider, value))
            .into_any_element()
    }
    fn export_theme(&self, theme: ThemeDefinition, window: &mut Window, cx: &mut Context<Self>) {
        let file = themes::serialize(&theme);
        let path = cx.prompt_for_new_path(std::path::Path::new("/"), Some(&format!("{}.json", theme.id)));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let result = cx.background_executor().spawn(async move { std::fs::write(path, file) }).await;
            if let Err(e) = result {
                let _ = this.update(cx, |_, cx| store::show_error(e.to_string(), cx));
            }
        })
        .detach();
    }

}

fn ring(color: Hsla) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow { color, offset: gpui::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: false }]
}

/// A theme half as a 40px swatch: its canvas, a card band and an accent dot. The half in use gets
/// a ring.
fn preview_circle(theme: &ThemePreference, dark: bool, active: bool, cx: &App) -> impl IntoElement {
    let [background, card, primary, foreground] = swatch(theme, dark, cx);
    div()
        .size(r(40.))
        .rounded_full()
        .overflow_hidden()
        .border_1()
        .border_color(hsla(foreground).opacity(0.12))
        .bg(hsla(background))
        .when(active, |this| {
            this.shadow(vec![gpui::BoxShadow {
                color: hsla(foreground).opacity(0.6),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(2.),
                inset: false,
            }])
        })
        .flex()
        .flex_col()
        .justify_end()
        .child(h_flex().h(r(16.)).bg(hsla(card)).justify_center().child(div().size(r(8.)).rounded_full().bg(hsla(primary))))
}

/// The mode tile's miniature: a sidebar strip and content lines in the theme's colors; System
/// shows the light half beside the dark half.
fn wireframe(theme: &ThemePreference, mode: AppearanceMode, cx: &App) -> impl IntoElement {
    let half = |dark: bool| {
        let [canvas,sidebar,surface,accent,message,action,text]=theme::wireframe(theme,dark,cx);
        let line=hsla(text).opacity(0.25);
        let rect=|x,y,w,h,color|div().absolute().left(gpui::relative(x)).top(gpui::relative(y)).w(gpui::relative(w)).h(gpui::relative(h)).rounded(r(4.)).bg(color);
        div().relative().flex_1().h_full().overflow_hidden().bg(hsla(canvas))
            .child(rect(0.,0.,0.22,1.,hsla(sidebar)))
            .child(rect(0.03,0.08,0.16,0.08,hsla(surface)).border_1().border_color(line))
            .child(rect(0.03,0.22,0.16,0.07,hsla(accent)))
            .child(rect(0.03,0.32,0.16,0.07,hsla(message).opacity(0.7)))
            .child(rect(0.03,0.42,0.16,0.07,hsla(message).opacity(0.5)))
            .child(rect(0.48,0.11,0.24,0.09,hsla(message)))
            .child(rect(0.27,0.28,0.34,0.05,line))
            .child(rect(0.27,0.38,0.26,0.05,line))
            .child(rect(0.26,0.77,0.68,0.15,hsla(surface)).border_1().border_color(line)
                .child(rect(0.05,0.37,0.34,0.26,line))
                .child(rect(0.86,0.21,0.08,0.58,hsla(action)).rounded_full()))
            .child(rect(0.75,0.08,0.2,0.46,hsla(surface)).border_1().border_color(line)
                .children([0,1,2].map(|i|rect(0.12,0.1+i as f32*0.3,0.76,0.2,line.opacity(0.55)))))
    };
    let frame = h_flex().h(r(112.)).w_full().rounded(r(8.)).overflow_hidden();
    match mode {
        AppearanceMode::System => frame.child(
            div().relative().flex_1().h_full().overflow_hidden().child(div().absolute().left_0().top_0().w(gpui::relative(2.)).h_full().flex().child(half(false)))
        ).child(
            div().relative().flex_1().h_full().overflow_hidden().child(div().absolute().right_0().top_0().w(gpui::relative(2.)).h_full().flex().child(half(true)))
        ),
        AppearanceMode::Light => frame.child(half(false)),
        AppearanceMode::Dark => frame.child(half(true)),
    }
}

#[derive(IntoElement)]
struct DurationSlider {
    state: Entity<SliderState>,
    value: u32,
}

impl DurationSlider {
    fn new(state: Entity<SliderState>, value: u32) -> Self {
        Self { state, value }
    }
}

impl RenderOnce for DurationSlider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        if self.state.read(cx).value().start() != self.value as f32 {
            self.state.update(cx, |s, cx| s.set_value(self.value as f32, window, cx));
        }
        h_flex()
            .w(r(208.))
            .gap(r(12.))
            .child(
                div()
                    .min_w(r(64.))
                    .rounded(r(6.))
                    .bg(hsla(p.muted))
                    .px(r(8.))
                    .py(r(4.))
                    .text_center()
                    .font_family(theme::font_family(t3_core::client_settings::typography::FontRole::Code, cx))
                    .text_xs()
                    .text_color(hsla(p.foreground))
                    .child(format!("{} ms", self.value)),
            )
            .child(div().flex_1().child(Slider::new(&self.state)))
    }
}

#[cfg(feature="snapshot")]
pub(super) fn import_theme(state: &Entity<crate::app_state::AppState>, theme: ThemeDefinition, activate: bool, cx: &mut App) {
    match themes::install(store::client(cx), &theme) {
        Ok(library) => {
            store::update_client(state, ClientSettingChange::ThemeLibrary(library), cx);
            if activate {
                if theme.variants.is_empty() {
                    let patch = themes::assign_half(store::client(cx), theme.appearance == "dark", Some(&theme.id));
                    store::update_client(state, ClientSettingChange::ThemePatch(patch), cx);
                } else {
                    store::update_client(state, ClientSettingChange::Theme(ThemePreference::Named(theme.id)), cx);
                }
            }
        }
        Err(e) => store::show_error(e, cx),
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Render, Window, div, prelude::*};
    use serde_json::json;
    use t3_core::client_settings::{ClientSettingChange, EffortDisplay, EnvironmentIdentificationMode};
    use t3_core::settings::search::Section;

    use super::*;

    struct SettingsFrame;
    impl Render for SettingsFrame {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(crate::settings::overlay(window, cx))
        }
    }

    fn reset_interface_settings(state: &Entity<crate::app_state::AppState>, cx: &mut App) {
        store::update_client(state, ClientSettingChange::ResetThreadDetailsItems, cx);
        store::update_client(state, ClientSettingChange::EffortDisplay(EffortDisplay::Label), cx);
        store::update_client(state, ClientSettingChange::PanelAnimationDurationMs(0), cx);
        store::update_client(state, ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::Artwork), cx);
        store::update_client(state, ClientSettingChange::PersistComposerContextStrip(false), cx);
    }

    #[gpui::test]
    fn appearance_interface_controls_resets_and_search_expand_work(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.run_until_parked();
        let details = cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            reset_interface_settings(&state, cx);
            crate::thread_details::init(state.clone(), cx)
        });

        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        // Opening via settings search for "environment-identification" (anchor "appearance-interface")
        // auto-expands the Advanced Interface disclosure.
        vcx.update(|_, cx| crate::settings::open_setting("environment-identification", cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        // Scroll the Interface section into the visible 960px modal viewport before clicking controls.
        let view = vcx.read(|cx| cx.global::<crate::settings::Host>().view.clone().expect("settings view"));
        let scroll_to_bounds = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
            vcx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            let bounds = vcx.debug_bounds(selector).unwrap_or_else(|| panic!("missing {selector}"));
            let current_y = f32::from(view.read_with(vcx, |v, _| v.scroll.offset().y));
            let delta = f32::from(bounds.origin.y) - 360.;
            view.update(vcx, |v, _| {
                v.scroll.set_offset(gpui::point(px(0.), px((current_y - delta).min(0.))));
            });
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            vcx.debug_bounds(selector).unwrap_or_else(|| panic!("missing {selector} after scroll")).center()
        };
        assert!(vcx.debug_bounds("thread-details-items-trigger").is_some());
        assert!(vcx.debug_bounds("effort-display-control").is_some());
        assert!(vcx.debug_bounds("panel-animations-preview").is_some());
        assert!(vcx.debug_bounds("environment-identification-control").is_some());
        assert!(vcx.debug_bounds("composer-context-switch").is_some());

        // Toggle Thread details items via the popover and verify Details entity syncs live.
        let trigger_center = scroll_to_bounds(&mut vcx, "thread-details-items-trigger");
        vcx.simulate_click(trigger_center, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("thread-details-items-popover").is_some());
        let usage_item = vcx.debug_bounds("thread-details-item-usage").expect("usage checkbox").center();
        vcx.simulate_click(usage_item, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.read(|cx| assert!(store::client(cx).thread_details_item_visible("usage")));
        details.read_with(&vcx, |d, _| assert!(d.shows("usage")));

        // Reset via the popover's "Reset to default" restores both ClientSettingsDoc and Details.
        let reset_item = vcx.debug_bounds("thread-details-menu-reset").expect("menu reset").center();
        vcx.simulate_click(reset_item, Default::default());
        vcx.run_until_parked();
        vcx.read(|cx| assert!(store::client(cx).thread_details_is_default()));
        details.read_with(&vcx, |d, _| assert!(!d.shows("usage") && d.items_are_default()));

        // Close Thread details popover before clicking other controls.
        vcx.simulate_click(trigger_center, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });

        // Clicking PanelAnimationsPreview toggles preview_open state.
        view.read_with(&vcx, |v, _| assert!(v.appearance_ui.panel_preview_open));
        let preview_center = scroll_to_bounds(&mut vcx, "panel-animations-preview");
        vcx.simulate_click(preview_center, Default::default());
        vcx.run_until_parked();
        view.read_with(&vcx, |v, _| assert!(!v.appearance_ui.panel_preview_open));

        // Panel animations slider updates duration and resets cleanly to 0.
        vcx.update(|window, cx| {
            let slider = view.read(cx).appearance_ui.sliders[2].clone();
            slider.update(cx, |s, cx| {
                s.set_value(200., window, cx);
                cx.emit(SliderEvent::Change(gpui_component::slider::SliderValue::Single(200.)));
            });
        });
        vcx.run_until_parked();
        vcx.read(|cx| assert_eq!(store::client(cx).panel_animation_duration_ms(), 200));

        // Clicking Composer context switch toggles persistComposerContextStrip.
        let switch_center = scroll_to_bounds(&mut vcx, "composer-context-switch");
        vcx.simulate_click(switch_center, Default::default());
        vcx.run_until_parked();
        vcx.read(|cx| assert!(store::client(cx).persist_composer_context_strip()));

        // Exercise EffortDisplay & EnvironmentIdentificationMode updates + resets via store.
        vcx.update(|_, cx| {
            store::update_client(&state, ClientSettingChange::EffortDisplay(EffortDisplay::Bars), cx);
            store::update_client(&state, ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::Pill), cx);
        });
        vcx.read(|cx| {
            assert_eq!(store::client(cx).effort_display(), EffortDisplay::Bars);
            assert_eq!(store::client(cx).environment_identification_mode(), EnvironmentIdentificationMode::Pill);
        });
        vcx.update(|_, cx| {
            store::update_client(&state, ClientSettingChange::EffortDisplay(EffortDisplay::Label), cx);
            store::update_client(&state, ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::Artwork), cx);
            store::update_client(&state, ClientSettingChange::PanelAnimationDurationMs(0), cx);
            store::update_client(&state, ClientSettingChange::PersistComposerContextStrip(false), cx);
        });
        vcx.read(|cx| {
            assert_eq!(store::client(cx).effort_display(), EffortDisplay::Label);
            assert_eq!(store::client(cx).environment_identification_mode(), EnvironmentIdentificationMode::Artwork);
            assert_eq!(store::client(cx).panel_animation_duration_ms(), 0);
            assert!(!store::client(cx).persist_composer_context_strip());
        });

        // Open Appearance directly (without search target) and verify Advanced starts collapsed until clicked.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.appearance_ui.advanced_interface = false;
                v.navigate(Section::Appearance, None, cx);
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("panel-animations-preview").is_none());
        let adv_center = scroll_to_bounds(&mut vcx, "appearance-interface-advanced");
        vcx.simulate_click(adv_center, Default::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("panel-animations-preview").is_some());

        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }

    #[gpui::test]
    fn appearance_consumers_react_to_effort_bars_context_strip_sidebar_id_and_panel_animation(
        cx: &mut gpui::TestAppContext,
    ) {
        struct ConsumerStage {
            sidebar: Entity<crate::sidebar::SidebarPane>,
            composer: Entity<crate::composer::Composer>,
            right_panel: Entity<crate::right_panel::RightPanelView>,
        }
        impl Render for ConsumerStage {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                h_flex()
                    .size_full()
                    .child(div().w(px(280.)).h_full().child(self.sidebar.clone().cached(gpui::StyleRefinement::default().size_full())))
                    .child(div().flex_1().h_full().child(self.composer.clone()))
                    .child(self.right_panel.clone())
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        cx.executor().allow_parking();
        let (state, fixture) =
            cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            reset_interface_settings(&state, cx);
            crate::thread_details::init(state.clone(), cx);
            state.update(cx, |state, cx| {
                state.server_config.config = Some(
                    serde_json::from_value(json!({
                        "environment": { "environmentId": "fixture-env", "label": "Mac", "serverVersion": "0.1.7" },
                        "providers": [{
                            "driver": "codex",
                            "instanceId": "codex",
                            "enabled": true,
                            "installed": true,
                            "status": "ready",
                            "auth": { "status": "authenticated" },
                            "models": [{
                                "slug": "gpt-5.4",
                                "name": "GPT-5.4",
                                "capabilities": {
                                    "optionDescriptors": [{
                                        "type": "select",
                                        "id": "reasoningEffort",
                                        "label": "Reasoning effort",
                                        "role": "reasoning",
                                        "options": [
                                            { "id": "low", "label": "Low" },
                                            { "id": "medium", "label": "Medium", "isDefault": true },
                                            { "id": "high", "label": "High" }
                                        ]
                                    }]
                                }
                            }]
                        }]
                    }))
                    .unwrap(),
                );
                let snapshot = serde_json::from_value(json!({
                    "kind": "snapshot",
                    "snapshot": {
                        "snapshotSequence": 1,
                        "projects": [{
                            "id": "proj-1",
                            "title": "Repo",
                            "workspaceRoot": "/tmp/repo",
                            "scripts": [],
                            "createdAt": "2026-11-19T00:00:00Z",
                            "updatedAt": "2026-11-19T00:00:00Z"
                        }],
                        "threads": [{
                            "id": "thread-1",
                            "projectId": "proj-1",
                            "title": "Active thread",
                            "providerInstanceId": "codex",
                            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
                            "runtimeMode": "full-access",
                            "interactionMode": "default",
                            "branch": "feat/appearance",
                            "worktreePath": null,
                            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
                            "latestRunId": "run-1",
                            "activeRunId": null,
                            "status": "idle",
                            "pendingRuntimeRequest": null,
                            "latestVisibleMessage": null,
                            "latestUserMessageAt": null,
                            "hasActionableProposedPlan": false,
                            "itemCount": 0,
                            "visibleItemCount": 0,
                            "createdAt": "2026-11-19T00:00:00Z",
                            "updatedAt": "2026-11-19T00:00:00Z",
                            "archivedAt": null,
                            "settledOverride": null,
                            "settledAt": null,
                            "deletedAt": null,
                            "pullRequests": []
                        }],
                        "archivedThreads": []
                    }
                }))
                .unwrap();
                state.shell.apply(snapshot);
                state.selected_thread = Some("thread-1".into());
                cx.emit(crate::app_state::AppEvent::ServerConfig);
                cx.emit(crate::app_state::AppEvent::Shell);
                cx.emit(crate::app_state::AppEvent::Selection);
            });
        });

        let window = cx.add_window(|window, cx| {
            let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), window, cx));
            let menu = cx.new(|cx| crate::context_menu::ContextMenu::new(state.clone(), cx));
            let confirm = cx.new(|cx| crate::dialog::ConfirmDialog::new(state.clone(), cx));
            let snooze = cx.new(|cx| crate::snooze_dialog::SnoozeDialog::new(state.clone(), window, cx));
            let sidebar = cx.new(|cx| crate::sidebar::SidebarPane::new(state.clone(), composer.clone(), menu, confirm, snooze, window, cx));
            let right_panel = cx.new(|cx| crate::right_panel::RightPanelView::new(state.clone(), window, cx));
            ConsumerStage { sidebar, composer, right_panel }
        });
        let right_panel = window.read_with(cx, |stage, _| stage.right_panel.clone()).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            vcx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
        };

        // 1. Defaults: Effort word shown, active-thread context strip hidden, Dev artwork shown in
        // sidebar and on the composer Send button, right panel animation disabled at 0 ms.
        draw(&mut vcx);
        assert!(vcx.debug_bounds("composer-effort-word").is_some(), "default effortDisplay=label shows word");
        assert!(vcx.debug_bounds("composer-effort-bars").is_none());
        assert!(vcx.debug_bounds("composer-context-strip").is_none(), "default persistComposerContextStrip=false hides active strip");
        assert!(vcx.debug_bounds("sidebar-stage-backdrop-dev").is_some(), "default Artwork shows Dev backdrop");
        assert!(vcx.debug_bounds("send-stage-art-dev").is_some(), "default Artwork shows Dev stage art on Send button");
        assert!(vcx.debug_bounds("environment-identification-pill-dev").is_none());

        // 2. Switch effortDisplay -> Bars, persistComposerContextStrip -> true, environmentIdentificationMode -> Pill.
        vcx.update(|_, cx| {
            store::update_client(&state, ClientSettingChange::EffortDisplay(EffortDisplay::Bars), cx);
            store::update_client(&state, ClientSettingChange::PersistComposerContextStrip(true), cx);
            store::update_client(&state, ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::Pill), cx);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("composer-effort-bars").is_some(), "effortDisplay=bars renders bars glyph");
        assert!(vcx.debug_bounds("composer-effort-word").is_none());
        assert!(vcx.debug_bounds("composer-context-strip").is_some(), "persistComposerContextStrip=true shows active thread strip");
        assert!(vcx.debug_bounds("environment-identification-pill-dev").is_some(), "Pill mode renders Dev pill");
        assert!(vcx.debug_bounds("sidebar-stage-backdrop-dev").is_none());
        assert!(vcx.debug_bounds("send-stage-art-dev").is_none(), "Pill mode hides Send button stage art");

        // 3. Nightly artwork & None modes in sidebar + Send button.
        vcx.update(|_, cx| {
            store::update_client(&state, ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::NightlyArtwork), cx);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("sidebar-stage-backdrop-nightly").is_some());
        assert!(vcx.debug_bounds("send-stage-art-nightly").is_some(), "NightlyArtwork renders Nightly stage art on Send button");
        vcx.update(|_, cx| {
            store::update_client(&state, ClientSettingChange::EnvironmentIdentificationMode(EnvironmentIdentificationMode::None), cx);
        });
        draw(&mut vcx);
        assert!(vcx.debug_bounds("sidebar-stage-backdrop-nightly").is_none());
        assert!(vcx.debug_bounds("sidebar-stage-backdrop-dev").is_none());
        assert!(vcx.debug_bounds("send-stage-art-nightly").is_none());
        assert!(vcx.debug_bounds("send-stage-art-dev").is_none());
        assert!(vcx.debug_bounds("environment-identification-pill-dev").is_none());

        // 3b. Stage detection prefers the primary/local environment's serverVersion over an active remote.
        state.update(&mut vcx, |state, _| {
            if let Some(first) = state.environments.environments.first_mut() {
                first.local = true;
                first.server_version = Some("0.1.0-nightly.20260401.1".into());
            }
            let remote_id = "remote-stable".to_owned();
            let mut remote = state.environments.environments[0].clone();
            remote.id = remote_id.clone();
            remote.local = false;
            remote.label = "Remote Stable".into();
            remote.server_version = Some("0.1.0".into());
            state.environments.environments.push(remote);
            state.environments.active = Some(remote_id);
        });
        state.read_with(&vcx, |state, _| {
            assert_eq!(
                crate::sidebar::environment_stage_label(state),
                "Nightly",
                "primary local nightly version must not be overridden by active remote stable config"
            );
        });
        // Reverse check: local stable + active remote nightly does not turn stage into Nightly.
        state.update(&mut vcx, |state, _| {
            state.environments.environments[0].server_version = Some("0.1.0".into());
            if let Some(cfg) = state.server_config.config.as_mut() {
                cfg.environment.server_version = "0.1.0-nightly.20260401.1".into();
            }
        });
        state.read_with(&vcx, |state, _| {
            assert_eq!(
                crate::sidebar::environment_stage_label(state),
                if cfg!(debug_assertions) { "Dev" } else { "Latest" },
                "active remote nightly config must not override local stable primary environment"
            );
        });

        // 3c. Single-project primary thread on a non-git checkout hides context strip even in a multi-env fleet.
        state.update(&mut vcx, |state, cx| {
            let local_id = state.environments.environments[0].id.clone();
            state.environments.active = Some(local_id);
            if let Some(snapshot) = state.shell.snapshot.as_mut() {
                if let Some(thread) = snapshot.threads.first_mut() {
                    thread.branch = None;
                    thread.worktree_path = None;
                }
            }
            cx.emit(crate::app_state::AppEvent::Shell);
        });
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("composer-context-strip").is_none(),
            "non-git primary thread without fleet picker hides context strip"
        );

        // 4. Right panel open and close animations activate only when panelAnimationDurationMs > 0 on same-thread transitions.
        state.update(&mut vcx, |state, cx| {
            state.toggle_right_panel(cx);
        });
        draw(&mut vcx);
        right_panel.read_with(&vcx, |panel, _| {
            assert_eq!(panel.open_animation_duration_ms(), None, "0 ms never animates panel open");
        });
        state.update(&mut vcx, |state, cx| state.toggle_right_panel(cx));
        draw(&mut vcx);
        right_panel.read_with(&vcx, |panel, _| {
            assert_eq!(panel.close_animation_duration_ms(), None, "0 ms never animates panel close");
        });
        vcx.update(|_, cx| {
            store::update_client(&state, ClientSettingChange::PanelAnimationDurationMs(225), cx);
        });
        state.update(&mut vcx, |state, cx| state.toggle_right_panel(cx));
        draw(&mut vcx);
        right_panel.read_with(&vcx, |panel, _| {
            assert_eq!(panel.open_animation_duration_ms(), Some(225), "non-zero duration animates same-thread panel open");
        });
        state.update(&mut vcx, |state, cx| state.toggle_right_panel(cx));
        draw(&mut vcx);
        right_panel.read_with(&vcx, |panel, _| {
            assert_eq!(panel.close_animation_duration_ms(), Some(225), "non-zero duration animates same-thread panel close");
        });
        assert!(vcx.debug_bounds("right-panel-closing").is_some(), "closing shell animates width collapse");
        vcx.executor().advance_clock(std::time::Duration::from_millis(300));
        draw(&mut vcx);
        right_panel.read_with(&vcx, |panel, _| {
            assert_eq!(panel.close_animation_duration_ms(), None, "close animation settles and clears after duration");
        });
        assert!(vcx.debug_bounds("right-panel-closing").is_none());

        vcx.update(|_, cx| reset_interface_settings(&state, cx));
        state.update(&mut vcx, |state, _| state.stop_pump());
        drop(vcx);
        drop(fixture);
        runtime.shutdown_background();
    }
}
