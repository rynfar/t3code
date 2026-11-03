//! Appearance: color scheme tiles and the theme library (`ThemeSettings.tsx` `ThemeLibrary`), chat
//! width (`AppearanceSettingsPanel.tsx`) and typography (`TypographySettings.tsx`).
//! Choices apply live through `crate::theme`. Theme editing is a separate parity slice.

use gpui::{
    App, AppContext, Context, ElementId, Entity, FontWeight, Hsla, IntoElement, ParentElement, PathPromptOptions, Styled, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use t3_core::client_settings::{AppearanceMode, ChatWidth, ClientSettingChange, ThemePreference};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, r, section, section_title};
use super::{Block, SettingsView, store};
use crate::theme::{self, hsla, palette, swatch};
use gpui_component::slider::{SliderEvent, SliderState};
use t3_core::themes::{self, ThemeDefinition};

pub(super) struct AppearanceState {
    sliders: [Entity<SliderState>; 2],
    _subscriptions: Vec<Subscription>,
    removal: Option<ThemeDefinition>,
    removal_ids: Vec<String>,
    variants: std::collections::BTreeMap<String, String>,
}
impl AppearanceState {
    pub fn new(cx: &mut Context<SettingsView>) -> Self {
        let values = [store::client(cx).appearance_contrast(), store::client(cx).glass_opacity()];
        let sliders = std::array::from_fn(|i| {
            cx.new(|_| {
                SliderState::new()
                    .min(if i == 0 { 50. } else { 40. })
                    .max(if i == 0 { 200. } else { 100. })
                    .step(5.)
                    .default_value(values[i] as f32)
            })
        });
        let subscriptions = sliders
            .iter()
            .enumerate()
            .map(|(i, slider)| {
                cx.subscribe(slider, move |view, _, event: &SliderEvent, cx| {
                    if let SliderEvent::Change(value) = event {
                        let value = value.start().round() as u32;
                        store::update_client(
                            &view.state,
                            if i == 0 { ClientSettingChange::AppearanceContrast(value) } else { ClientSettingChange::GlassOpacity(value) },
                            cx,
                        );
                    }
                })
            })
            .collect();
        Self { sliders, _subscriptions: subscriptions, removal: None, removal_ids: Vec::new(), variants: std::collections::BTreeMap::new() }
    }
}

impl SettingsView {
    pub(super) fn appearance(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
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
                    self.appearance_ui.variants.get(&collection.id).and_then(|id| custom.iter().find(|t| t.id == *id)).unwrap_or(t).clone(),
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
        let grid = div().grid().grid_cols(3).w_full().gap(r(8.)).children(cards.into_iter().map(|(card, label, definition)| {
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
                                    themes::half_theme(&theme, &halves, true, &custom, theme::environment_themes(cx)).map(|t| t.id.as_str())
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
                    .map(|t| SelectOption::new(t.id.clone(), t.label.clone()))
                    .collect::<Vec<_>>();
                if variants.len() > 1 {
                    body = body.child(
                        div().px(r(8.)).pt(r(4.)).child(
                            Select::new(ElementId::Name(format!("variants-{collection_id}").into()), variants)
                                .selected(card.named_id().unwrap_or("").to_owned())
                                .label(collection_label)
                                .width(220.)
                                .on_change(move |id, _, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.appearance_ui.variants.insert(collection_id.clone(), id.to_string());
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
            let mut actions = h_flex().px(r(8.)).py(r(8.)).gap(r(4.)).child(
                Button::new(
                    ElementId::Name(format!("theme-use-{}", card.named_id().unwrap_or("default")).into()),
                    ButtonVariant::Outline,
                    ButtonSize::Xs,
                )
                .label("Use")
                .on_click(move |_, _, cx| {
                    if let Some(t) = &use_definition
                        && t.variants.is_empty()
                    {
                        let patch = themes::assign_half(store::client(cx), t.appearance == "dark", Some(&t.id));
                        store::update_client(&state, ClientSettingChange::ThemePatch(patch), cx);
                    } else {
                        store::update_client(&state, ClientSettingChange::Theme(chosen.clone()), cx);
                    }
                }),
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let copy = themes::duplicate(store::client(cx), &duplicate);
                        this.install_theme(copy, false, cx);
                    })),
                );
                if custom.iter().any(|t| t.id == definition.id) {
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
                        .on_click(cx.listener(|this, _, _, cx| {
                            let seed = ThemeDefinition {
                                id: "t3-code".into(),
                                label: "T3 Code".into(),
                                appearance: "light".into(),
                                colors: themes::standard(false),
                                variants: std::collections::BTreeMap::from([("dark".into(), themes::standard(true))]),
                                collection: None,
                                managed: false,
                            };
                            this.install_theme(themes::duplicate(store::client(cx), &seed), false, cx);
                        })),
                );
            }
            body = body.child(actions);
            body
        }));

        let colors = v_flex()
            .gap(r(12.))
            .child(section_title("Color scheme"))
            .child(tiles)
            .child(
                h_flex().pt(r(20.)).justify_between().child(section_title("Themes")).child(
                    Button::new("import-theme", ButtonVariant::Outline, ButtonSize::Xs)
                        .label("Add theme")
                        .on_click(cx.listener(|this, _, window, cx| this.pick_theme(window, cx))),
                ),
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
        let interface = section(
            "Interface",
            vec![
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
            ],
            cx,
        );

        let typography = self.typography_rows(cx);

        vec![
            Block::new(&["appearance", "color-scheme", "theme"], colors),
            Block::new(&["appearance-interface", "chat-width", "setting-appearance-contrast", "setting-glass-opacity"], interface),
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
    fn pick_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths =
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Add theme".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    std::fs::metadata(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|m| {
                            if m.len() > 256 * 1024 {
                                Err("Theme files are only a few KB, so this one was not read (limit 256 KB).".to_owned())
                            } else {
                                std::fs::read_to_string(path).map_err(|e| e.to_string())
                            }
                        })
                        .map_err(|e| e.to_string())
                        .and_then(|s| themes::import(&s))
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(t) => this.install_theme(t, true, cx),
                Err(e) => store::show_error(e, cx),
            });
        })
        .detach();
    }
    fn install_theme(&mut self, theme: ThemeDefinition, activate: bool, cx: &mut Context<Self>) {
        import_theme(&self.state, theme, activate, cx);
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
        let prefs = theme::preferences(cx);
        let owner = themes::half_theme(theme, &prefs.halves, dark, &prefs.custom, theme::environment_themes(cx))
            .map(|t| ThemePreference::Named(t.id.clone()))
            .unwrap_or_else(|| theme.clone());
        let [background, card, primary, foreground] = swatch(&owner, dark, cx);
        h_flex().flex_1().h_full().bg(hsla(background)).child(div().w(r(28.)).h_full().bg(hsla(card))).child(
            v_flex()
                .flex_1()
                .p(r(10.))
                .gap(r(6.))
                .child(div().h(r(6.)).w(r(56.)).rounded_full().bg(hsla(foreground).opacity(0.35)))
                .child(div().h(r(6.)).w(r(40.)).rounded_full().bg(hsla(foreground).opacity(0.2)))
                .child(div().mt(r(8.)).h(r(14.)).w(r(48.)).rounded(r(4.)).bg(hsla(primary))),
        )
    };
    let frame = h_flex().h(r(112.)).w_full().rounded(r(8.)).overflow_hidden();
    match mode {
        AppearanceMode::System => frame.child(half(false)).child(half(true)),
        AppearanceMode::Light => frame.child(half(false)),
        AppearanceMode::Dark => frame.child(half(true)),
    }
}

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
