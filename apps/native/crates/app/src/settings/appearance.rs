//! Appearance: color scheme tiles and the theme library (`ThemeSettings.tsx` `ThemeLibrary`), chat
//! width (`AppearanceSettingsPanel.tsx`) and typography (`TypographySettings.tsx`).
//! Choices apply live through `crate::theme`; editing is hosted by `crate::theme_editor`.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, App, AppContext, BorderStyle, Bounds, BoxShadow, Context, Corners, ElementId,
    Entity, FocusHandle, FontWeight, Hsla, IntoElement, ParentElement, Pixels, RenderImage, Styled,
    Subscription, Window, canvas, div, fill, point, prelude::*, px, quad, size,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::client_settings::{
    AppearanceMode, ChatWidth, ClientSettingChange, DEFAULT_PANEL_ANIMATION_DURATION_MS, EffortDisplay,
    EnvironmentIdentificationMode, MAX_PANEL_ANIMATION_DURATION_MS, MIN_PANEL_ANIMATION_DURATION_MS, ThemePreference,
    environment_identification_options, resolve_sidebar_stage_backdrop_variant,
};

use super::controls::{
    Button, ButtonSize, ButtonVariant, DialogFrame, ModalLayout, Row, Select, SelectOption, Switch,
    dialog_footer, focus_handle, icon, r, section, section_title, tooltip,
};
use super::{Block, SettingsView, store};
use crate::i18n::{t, t_fmt};
use crate::icons::Icon;
use crate::theme::{
    self, ThemeSlot, ThemeSlotExt, hsla, palette, theme_slot_clipped_background_layer,
};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use t3_core::{
    i18n::IcuArg,
    themes::{self, ThemeDefinition},
};

pub(super) struct AppearanceState {
    sliders: [Entity<SliderState>; 3],
    _subscriptions: Vec<Subscription>,
    removal: Option<ThemeDefinition>,
    pub(super) import: Option<Entity<super::theme_import::ThemeImportDialog>>,
    removal_ids: Vec<String>,
    removal_focus: FocusHandle,
    removal_return_focus: Option<FocusHandle>,
    thread_details_menu_open: bool,
    advanced_interface: bool,
    panel_preview_open: bool,
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
            removal_focus: cx.focus_handle(),
            removal_return_focus: None,
            thread_details_menu_open: false,
            advanced_interface: false,
            panel_preview_open: true,
        }
    }
}

impl SettingsView {
    pub(super) fn appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let doc = store::client(cx).clone();
        let mode = doc.appearance_mode();
        let theme = doc.theme();
        let p = palette(cx);
        let stacked = ModalLayout::of(window).rows_stacked;

        let tiles = h_flex()
            .w_full()
            .gap(r(12.))
            .children(AppearanceMode::ALL.iter().map(|&tile_mode| {
                let active = tile_mode == mode;
                let state = self.state.clone();
                let id = ElementId::Name(format!("appearance-{}", tile_mode.as_str()).into());
                let focus = focus_handle(&id, window, cx);
                v_flex()
                    .id(id)
                    .track_focus(&focus)
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_toggled(if active {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .aria_label(t(cx, tile_mode.label()).to_owned())
                    .flex_1()
                    .min_w_0()
                    .gap(r(6.))
                    .p(r(8.))
                    .rounded(r(12.))
                    .border_1()
                    .cursor_pointer()
                    .map(|tile| {
                        if active {
                            tile.border_color(hsla(p.ring))
                                .bg(hsla(p.accent).opacity(0.3))
                                .shadow(inset_ring(hsla(p.ring), 2.))
                        } else {
                            tile.border_color(hsla(p.border).opacity(0.7))
                                .bg(hsla(p.card).opacity(0.6))
                                .hover(|tile| tile.bg(hsla(p.accent).opacity(0.1)))
                        }
                    })
                    .focus_visible(|tile| {
                        tile.shadow(inset_ring(hsla(p.foreground).opacity(0.6), 2.))
                    })
                    .child(wireframe(&theme, tile_mode, window, cx))
                    .child(
                        div()
                            .flex()
                            .justify_center()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if active {
                                hsla(p.foreground)
                            } else {
                                hsla(p.muted_foreground)
                            })
                            .child(t(cx, tile_mode.label()).to_owned()),
                    )
                    .on_click(move |_, _, cx| {
                        store::update_client(
                            &state,
                            ClientSettingChange::AppearanceMode(tile_mode),
                            cx,
                        )
                    })
            }));

        let custom = theme::preferences(cx).custom.clone();
        let environment = theme::environment_themes(cx).to_vec();
        let halves = themes::halves(&doc);
        let owners = [false, true].map(|dark| {
            themes::half_theme(&theme, &halves, dark, &custom, &environment)
                .map(|theme| theme.id.as_str())
        });
        let mut seen = std::collections::BTreeSet::new();
        let custom_cards = custom.iter().filter_map(|definition| {
            if let Some(collection) = &definition.collection {
                if !seen.insert(collection.id.as_str()) {
                    return None;
                }
                return Some(
                    custom
                        .iter()
                        .filter(|candidate| {
                            candidate
                                .collection
                                .as_ref()
                                .is_some_and(|group| group.id == collection.id)
                        })
                        .collect::<Vec<_>>(),
                );
            }
            Some(vec![definition])
        });
        let cards = std::iter::once((Vec::new(), false))
            .chain(
                themes::builtins()
                    .iter()
                    .chain(environment.iter())
                    .filter(|theme| !custom.iter().any(|saved| saved.id == theme.id))
                    .map(|theme| (vec![theme], false)),
            )
            .chain(custom_cards.map(|variants| (variants, true)))
            .map(|(variants, editable)| self.theme_card(window, &variants, owners, editable, cx))
            .collect::<Vec<_>>();
        let columns = theme_grid_columns(window);
        let count = cards.len();
        let mut cards = cards.into_iter();
        let grid = v_flex()
            .w_full()
            .gap(r(8.))
            .children((0..count.div_ceil(columns)).map(|row| {
                let filled = (count - row * columns).min(columns);
                h_flex()
                    .w_full()
                    .items_start()
                    .gap(r(8.))
                    .children(cards.by_ref().take(columns))
                    .children((filled..columns).map(|_| div().flex_1().min_w_0()))
            }));

        let colors = v_flex()
            .gap(r(12.))
            .child(section_title(t(cx, "Color scheme").to_owned()))
            .child(tiles)
            .child(
                h_flex()
                    .pt(r(20.))
                    .pr(r(4.))
                    .gap(r(8.))
                    .flex_wrap()
                    .justify_between()
                    .child(section_title(t(cx, "Themes").to_owned()))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(
                                Button::new("create-theme", ButtonVariant::Outline, ButtonSize::Xs)
                                    .icon(Icon::Palette)
                                    .label("Create theme")
                                    .on_click(|_, _, cx| {
                                        crate::theme_editor::open(None, None, None, cx)
                                    }),
                            )
                            .child(
                                Button::new("import-theme", ButtonVariant::Outline, ButtonSize::Xs)
                                    .icon(Icon::Plus)
                                    .label("Add theme")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_theme_import(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(grid)
            .id("appearance-colors")
            .on_action(cx.listener(|this, _: &super::CloseSettings, window, cx| {
                if this.appearance_ui.removal.is_some() {
                    this.close_theme_removal(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &super::FocusNextInSettings, window, cx| {
                this.cycle_theme_removal_focus(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &super::FocusPreviousInSettings, window, cx| {
                this.cycle_theme_removal_focus(false, window, cx);
            }))
            .children(self.theme_removal_dialog(&custom, window, cx));

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
            self.percent_row(false, stacked, cx),
            self.percent_row(true, stacked, cx),
            Row::new("chat-width", "Chat width")
                .description("Set how wide messages and the composer can grow on large screens.")
                .highlighted(self.is_target("chat-width"))
                .reset(doc.chat_width() != ChatWidth::default(), {
                    let state = self.state.clone();
                    move |_, _, cx| store::update_client(&state, ClientSettingChange::ChatWidth(ChatWidth::default()), cx)
                })
                .control(
                    appearance_control(stacked, 160.).child(Select::new(
                        "chat-width-select",
                        vec![
                            SelectOption::new("comfortable", "Comfortable (default)"),
                            SelectOption::new("wide", "Wide"),
                            SelectOption::new("full", "Full"),
                        ],
                    )
                    .selected(doc.chat_width().as_str())
                    .label(doc.chat_width().label())
                    .show_check()
                    .on_change(move |value, _, cx| {
                        if let Some(width) = ChatWidth::parse(&value) {
                            store::update_client(&width_state, ClientSettingChange::ChatWidth(width), cx);
                        }
                    })),
                )
                .into_any_element(),
            self.thread_details_items_row(&doc, stacked, cx),
            Row::new("effort-display", "Effort display")
                .description("Show reasoning effort in the composer as its name, such as High, or as bars.")
                .highlighted(self.is_target("effort-display"))
                .reset(effort_display != EffortDisplay::default(), move |_, _, cx| {
                    store::update_client(&effort_reset_state, ClientSettingChange::EffortDisplay(EffortDisplay::default()), cx)
                })
                .control(
                    appearance_control(stacked, 160.)
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
            advanced_trigger("appearance-interface-advanced", advanced_open, cx.listener(move |this, _, _, cx| {
                this.appearance_ui.advanced_interface = !advanced_open;
                this.target = None;
                cx.notify();
            })),
        ];
        if advanced_open {
            interface_rows.push(self.panel_animations_row(&doc, stacked, cx));
            interface_rows.push(self.environment_identification_row(&doc, stacked, cx));
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

    fn theme_card(
        &self,
        window: &mut Window,
        variants: &[&ThemeDefinition],
        owners: [Option<&str>; 2],
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let representative = variants
            .iter()
            .copied()
            .find(|theme| owners.contains(&Some(theme.id.as_str())))
            .or_else(|| variants.first().copied());
        let group = representative.and_then(|theme| theme.collection.as_ref());
        let key = group.map_or_else(
            || {
                representative
                    .map_or("default", |theme| theme.id.as_str())
                    .to_owned()
            },
            |group| format!("collection-{}", group.id),
        );
        let label = group
            .map(|group| group.label.as_str())
            .or_else(|| representative.map(|theme| theme.label.as_str()))
            .unwrap_or("T3 Code");
        let active = owners.iter().zip([false, true]).any(|(owner, dark)| {
            if representative.is_none() {
                owner.is_none()
            } else {
                variants
                    .iter()
                    .any(|theme| *owner == Some(theme.id.as_str()) && theme.half(dark).is_some())
            }
        });

        let mut previews = h_flex()
            .w_full()
            .justify_center()
            .items_start()
            .min_h(r(80.))
            .gap(r(10.))
            .pt(r(8.));
        for (index, dark) in [false, true].into_iter().enumerate() {
            let choices: Vec<_> = variants
                .iter()
                .copied()
                .filter(|theme| theme.half(dark).is_some())
                .collect();
            let selected = choices
                .iter()
                .copied()
                .find(|theme| Some(theme.id.as_str()) == owners[index])
                .or_else(|| choices.first().copied());
            if selected.is_none() && representative.is_some() {
                continue;
            }
            let name = selected.map_or(label, |theme| theme.label.as_str());
            let mode = if dark { "dark" } else { "light" };
            let instruction = t_fmt(
                cx,
                if dark {
                    "Use {label} for dark mode"
                } else {
                    "Use {label} for light mode"
                },
                &[("label", IcuArg::Text(name.into()))],
            );
            let theme_id = selected.map(|theme| theme.id.clone());
            let picked = owners[index] == theme_id.as_deref();
            let state = self.state.clone();
            let selector = format!("theme-half-{key}-{mode}");
            let id = ElementId::Name(selector.clone().into());
            let focus = focus_handle(&id, window, cx);
            let orb = h_flex()
                .id(id)
                .debug_selector(move || selector)
                .relative()
                .track_focus(&focus)
                .tab_index(0)
                .role(gpui::Role::Button)
                .aria_toggled(if picked {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .aria_label(instruction.clone())
                .size(r(68.))
                .flex_none()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .tooltip(tooltip(instruction.into()))
                .hover(|orb| orb.bg(hsla(p.accent).opacity(0.35)))
                .when(picked, |orb| orb.shadow(inset_ring(hsla(p.ring), 2.)))
                .focus_visible(|orb| orb.shadow(inset_ring(hsla(p.foreground).opacity(0.6), 2.)))
                .child(preview_orb(
                    selected.and_then(|theme| theme.half(dark)),
                    dark,
                    56.,
                    window,
                    cx,
                ))
                .when(picked, |orb| {
                    orb.child(
                        h_flex()
                            .absolute()
                            .bottom(r(2.))
                            .right(r(2.))
                            .size(r(20.))
                            .justify_center()
                            .rounded_full()
                            .border_1()
                            .border_color(hsla(p.border).opacity(0.7))
                            .bg(hsla(p.background))
                            .child(icon(
                                if dark { Icon::Moon } else { Icon::Sun },
                                12.,
                                hsla(p.foreground),
                            )),
                    )
                })
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let patch = themes::assign_half(store::client(cx), dark, theme_id.as_deref());
                    store::update_client(&state, ClientSettingChange::ThemePatch(patch), cx);
                });
            let switcher = selected.filter(|_| choices.len() > 1).map(|selected| {
                let state = self.state.clone();
                div()
                    .id(ElementId::Name(
                        format!("theme-variant-trigger-{key}-{mode}").into(),
                    ))
                    .w(r(102.))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        Select::new(
                            ElementId::Name(format!("theme-variant-{key}-{mode}").into()),
                            choices
                                .into_iter()
                                .map(|theme| {
                                    SelectOption::new(theme.id.clone(), theme.label.clone())
                                })
                                .collect(),
                        )
                        .selected(selected.id.clone())
                        .popup_width(208.)
                        .show_check()
                        .on_change(move |id, _, cx| {
                            let patch = themes::assign_half(store::client(cx), dark, Some(&id));
                            store::update_client(
                                &state,
                                ClientSettingChange::ThemePatch(patch),
                                cx,
                            );
                        }),
                    )
            });
            previews = previews.child(
                v_flex()
                    .items_center()
                    .gap(r(4.))
                    .child(orb)
                    .children(switcher),
            );
        }

        let selector = format!("theme-card-{key}");
        let id = ElementId::Name(selector.clone().into());
        let focus = focus_handle(&id, window, cx);
        let seed = representative.cloned();
        let copy_name = t_fmt(
            cx,
            "{label} copy",
            &[(
                "label",
                IcuArg::Text(
                    representative
                        .map_or("T3 Code", |theme| &theme.label)
                        .into(),
                ),
            )],
        );
        let mut footer = h_flex()
            .w_full()
            .gap(r(2.))
            .px(r(12.))
            .pt(r(8.))
            .pb(r(12.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(label.to_owned()),
            )
            .child(
                Button::new(
                    (id.clone(), "duplicate-theme"),
                    ButtonVariant::GhostMuted,
                    ButtonSize::IconXs,
                )
                .icon(Icon::Copy)
                .tooltip("Duplicate")
                .on_click(move |_, _, cx| {
                    crate::theme_editor::open(None, seed.clone(), Some(copy_name.clone()), cx)
                }),
            );
        if let Some(theme) = representative.filter(|_| editable).cloned() {
            let editing = theme.clone();
            let export = theme.clone();
            let remove_selector = format!("remove-{}", theme.id);
            let multiple = variants.len() > 1;
            footer = footer
                .child(
                    Button::new(
                        (id.clone(), "edit-theme"),
                        ButtonVariant::GhostMuted,
                        ButtonSize::IconXs,
                    )
                    .icon(Icon::PenLine)
                    .tooltip("Edit theme")
                    .on_click(move |_, _, cx| {
                        crate::theme_editor::open(Some(editing.clone()), None, None, cx)
                    }),
                )
                .child(
                    Button::new(
                        (id.clone(), "export-theme"),
                        ButtonVariant::GhostMuted,
                        ButtonSize::IconXs,
                    )
                    .icon(Icon::Upload)
                    .tooltip("Export theme")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.export_theme(export.clone(), window, cx)
                    })),
                )
                .child(
                    div().debug_selector(|| remove_selector.clone()).child(
                        Button::new(
                            ElementId::Name(remove_selector.into()),
                            ButtonVariant::GhostDestructive,
                            ButtonSize::IconXs,
                        )
                        .icon(Icon::Trash2)
                        .tooltip("Remove")
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.appearance_ui.removal_ids = if multiple {
                                    Vec::new()
                                } else {
                                    vec![theme.id.clone()]
                                };
                                this.appearance_ui.removal = Some(theme.clone());
                                this.appearance_ui.removal_return_focus = window.focused(cx);
                                this.appearance_ui.removal_focus.focus(window, cx);
                                cx.notify();
                            },
                        )),
                    ),
                );
        }
        let state = self.state.clone();
        let variants = variants
            .iter()
            .map(|theme| (*theme).clone())
            .collect::<Vec<_>>();
        v_flex()
            .id(id)
            .debug_selector(move || selector)
            .track_focus(&focus)
            .tab_index(0)
            .role(gpui::Role::Button)
            .aria_label(t_fmt(
                cx,
                "Use {label} theme",
                &[("label", IcuArg::Text(label.into()))],
            ))
            .flex_1()
            .min_w_0()
            .rounded(r(12.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.7))
            .cursor_pointer()
            .bg(hsla(p.card).opacity(0.6))
            .hover(|card| card.bg(hsla(p.accent).opacity(0.1)))
            .when(active, |card| {
                card.bg(hsla(p.accent).opacity(0.3))
                    .shadow(inset_ring(hsla(p.ring), 1.))
            })
            .focus_visible(|card| card.shadow(inset_ring(hsla(p.foreground).opacity(0.6), 2.)))
            .child(previews)
            .child(footer)
            .on_click(move |_, _, cx| {
                let doc = store::client(cx);
                let change = if variants.is_empty() {
                    ClientSettingChange::Theme(ThemePreference::Default(doc.appearance_mode()))
                } else {
                    ClientSettingChange::ThemePatch(themes::select_collection(doc, &variants))
                };
                store::update_client(&state, change, cx);
            })
            .into_any_element()
    }

    fn theme_removal_dialog(
        &self,
        custom: &[ThemeDefinition],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let target = self.appearance_ui.removal.as_ref()?;
        let p = palette(cx);
        let variants: Vec<_> = custom
            .iter()
            .filter(|theme| {
                theme.id == target.id
                    || target.collection.as_ref().is_some_and(|collection| {
                        theme
                            .collection
                            .as_ref()
                            .is_some_and(|other| collection.id == other.id)
                    })
            })
            .collect();
        let ids: Vec<_> = self
            .appearance_ui
            .removal_ids
            .iter()
            .filter(|id| variants.iter().any(|theme| &theme.id == *id))
            .cloned()
            .collect();
        let collection = target.collection.as_ref().filter(|_| variants.len() > 1);
        let count = ids.len();
        let title = if let Some(collection) = collection {
            t_fmt(
                cx,
                "Remove themes from “{label}”?",
                &[("label", IcuArg::Text(collection.label.as_str().into()))],
            )
        } else {
            t_fmt(
                cx,
                "Remove “{label}”?",
                &[("label", IcuArg::Text(target.label.as_str().into()))],
            )
        };
        let description = if collection.is_some() {
            "Select the variants you want to remove. You can restore them by importing the extension again."
        } else {
            "You can bring it back anytime by importing its JSON file."
        };
        let weak = cx.entity().downgrade();
        let body = v_flex()
            .debug_selector(|| "theme-removal-modal".to_owned())
            .gap(r(8.))
            .children(variants.into_iter().map(|theme| {
                let id = theme.id.clone();
                let checked = ids.contains(&id);
                let view = weak.clone();
                h_flex()
                    .w_full()
                    .min_h(r(60.))
                    .px(r(12.))
                    .gap(r(12.))
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(if checked && collection.is_some() {
                        p.ring
                    } else {
                        p.border
                    }))
                    .bg(hsla(p.muted).opacity(0.25))
                    .child(
                        h_flex()
                            .gap(r(4.))
                            .children([false, true].into_iter().filter_map(|dark| {
                                Some(preview_orb(Some(theme.half(dark)?), dark, 36., window, cx))
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(theme.label.clone()),
                    )
                    .when(collection.is_some(), |row| {
                        row.child(
                            gpui_component::checkbox::Checkbox::new(ElementId::Name(
                                format!("remove-variant-{id}").into(),
                            ))
                            .checked(checked)
                            .tooltip(theme.label.clone())
                            .theme_slot(ThemeSlot::Checkbox, cx)
                            .on_click(move |checked, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.appearance_ui.removal_ids.retain(|other| other != &id);
                                    if *checked {
                                        this.appearance_ui.removal_ids.push(id.clone());
                                    }
                                    cx.notify();
                                });
                            }),
                        )
                    })
            }));
        let label = if collection.is_none() {
            t(cx, "Remove theme").to_owned()
        } else if count == 0 {
            t(cx, "Remove selected").to_owned()
        } else {
            t_fmt(
                cx,
                "Remove selected ({count})",
                &[("count", IcuArg::Int(count as i64))],
            )
        };
        let footer = dialog_footer(
            false,
            vec![
                Button::new(
                    "cancel-theme-remove",
                    ButtonVariant::Outline,
                    ButtonSize::Sm,
                )
                .label("Cancel")
                .on_click(cx.listener(|this, _, window, cx| this.close_theme_removal(window, cx)))
                .into_any_element(),
                Button::new(
                    "confirm-theme-remove",
                    ButtonVariant::Destructive,
                    ButtonSize::Sm,
                )
                .label(label)
                .disabled(count == 0)
                .on_click(cx.listener(move |this, _, window, cx| {
                    match themes::remove(store::client(cx), &ids) {
                        Ok(patch) => {
                            store::update_client(
                                &this.state,
                                ClientSettingChange::ThemePatch(patch),
                                cx,
                            );
                            this.appearance_ui.removal_return_focus = None;
                            this.close_theme_removal(window, cx);
                        }
                        Err(error) => store::show_error(error, cx),
                    }
                }))
                .into_any_element(),
            ],
            cx,
        );
        Some(
            DialogFrame {
                id: "theme-removal-dialog",
                title: title.into(),
                description: description.into(),
                max_width: 464.,
                body: body.into_any_element(),
                footer: footer.into_any_element(),
                focus: self.appearance_ui.removal_focus.clone(),
            }
            .render_modal(
                window,
                4,
                move |window, cx| {
                    let _ = weak.update(cx, |this, cx| this.close_theme_removal(window, cx));
                },
                cx,
            ),
        )
    }

    fn cycle_theme_removal_focus(&self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.appearance_ui.removal.is_none() {
            cx.propagate();
            return;
        }
        let focus = &self.appearance_ui.removal_focus;
        for _ in 0..512 {
            if forward {
                window.focus_next(cx);
            } else {
                window.focus_prev(cx);
            }
            if focus.contains_focused(window, cx) && !focus.is_focused(window) {
                return;
            }
        }
        focus.focus(window, cx);
    }

    fn close_theme_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance_ui.removal = None;
        self.appearance_ui.removal_ids.clear();
        if let Some(focus) = self.appearance_ui.removal_return_focus.take() {
            focus.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn dismiss_appearance_popover(&mut self, cx: &mut Context<Self>) -> bool {
        if !std::mem::take(&mut self.appearance_ui.thread_details_menu_open) {
            return false;
        }
        cx.notify();
        true
    }

    fn thread_details_items_row(
        &self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
        stacked: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let enabled = doc.thread_details_enabled_count();
        let total = t3_core::thread_details::ITEMS.len();
        let is_default = doc.thread_details_is_default();
        let reset_state = self.state.clone();
        let menu_open = self.appearance_ui.thread_details_menu_open;
        let trigger_bounds = Rc::new(Cell::new(Bounds::default()));
        let popover = menu_open.then(|| {
            let trigger_bounds = trigger_bounds.clone();
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
                            t(cx, label).to_owned(),
                        )
                        .checked(shown)
                        .on_click(move |_, _, cx| {
                            update_thread_details(
                                &item_state,
                                ClientSettingChange::ThreadDetailsItem {
                                    id: item_id.clone(),
                                    visible: !shown,
                                },
                                cx,
                            );
                        }),
                    )
            });
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored()
                        .anchor(gpui::Anchor::TopRight)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            v_flex()
                                .id("thread-details-items-popover")
                                .debug_selector(|| "thread-details-items-popover".to_owned())
                                .occlude()
                                .relative()
                                .mt(r(4.))
                                .w(r(232.))
                                .p(r(4.))
                                .rounded(r(8.))
                                .border_1()
                                .border_color(hsla(p.dropdown_border))
                                .bg(hsla(p.popover))
                                .text_color(hsla(p.popover_foreground))
                                .shadow(crate::composer::controls::floating_shadow(dark))
                                .theme_slot(ThemeSlot::MenuPopup, cx)
                                .children(theme_slot_clipped_background_layer(
                                    ThemeSlot::MenuPopup,
                                    r(8.),
                                    cx,
                                ))
                                .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                    if !trigger_bounds.get().contains(&event.position) {
                                        this.dismiss_appearance_popover(cx);
                                    }
                                }))
                                .child(
                                    div()
                                        .px(r(8.))
                                        .py(r(6.))
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(hsla(p.muted_foreground))
                                        .child(t(cx, "Show in thread details").to_owned()),
                                )
                                .children(items)
                                .child(div().h(px(1.)).my(r(4.)).bg(hsla(p.border)))
                                .child(
                                    div()
                                        .debug_selector(|| "thread-details-menu-reset".to_owned())
                                        .child(
                                            crate::composer::controls::MenuItem::new(
                                                "thread-details-menu-reset",
                                                t(cx, "Reset to default").to_owned(),
                                            )
                                            .disabled(is_default)
                                            .on_click(move |_, _, cx| {
                                                update_thread_details(&menu_reset_state, ClientSettingChange::ResetThreadDetailsItems, cx);
                                            }),
                                        ),
                                ),
                        ),
                )
                .with_priority(2),
            )
        });
        Row::new("thread-details-items", "Thread details")
            .description("Choose what the thread details card shows. You can also right-click the card.")
            .highlighted(self.is_target("thread-details-items"))
            .reset(!is_default, move |_, _, cx| {
                update_thread_details(&reset_state, ClientSettingChange::ResetThreadDetailsItems, cx);
            })
            .control(
                appearance_control(stacked, 160.)
                    .relative()
                    .debug_selector(|| "thread-details-items-trigger".to_owned())
                    .child(measure_popover_trigger(trigger_bounds))
                    .child(
                        Button::new(
                            "thread-details-items-trigger",
                            ButtonVariant::Outline,
                            ButtonSize::Sm,
                        )
                        .label(t_fmt(
                            cx,
                            "{enabled} of {total} on",
                            &[
                                ("enabled", IcuArg::Int(enabled as i64)),
                                ("total", IcuArg::Int(total as i64)),
                            ],
                        ))
                        .trailing(Icon::ChevronDown)
                        .justify_between()
                        .full_width()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.appearance_ui.thread_details_menu_open =
                                !this.appearance_ui.thread_details_menu_open;
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
        stacked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let duration_ms = doc.panel_animation_duration_ms();
        let slider = self.appearance_ui.sliders[2].clone();
        let reset_slider = slider.clone();
        let reset_state = self.state.clone();
        let weak = cx.entity().downgrade();
        let preview = PanelAnimationPreview {
            open: self.appearance_ui.panel_preview_open,
            duration: Duration::from_millis(u64::from(duration_ms)),
            view: weak,
        };

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
                    .map(|row| {
                        if stacked {
                            row.w_full().min_w_0().flex_col().items_stretch()
                        } else {
                            row
                        }
                    })
                    .child(appearance_control(stacked, 112.).child(preview))
                    .child(
                        appearance_control(stacked, 208.)
                            .child(DurationSlider::new(slider, duration_ms)),
                    ),
            )
            .into_any_element()
    }

    fn environment_identification_row(
        &self,
        doc: &t3_core::client_settings::ClientSettingsDoc,
        stacked: bool,
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
                appearance_control(stacked, 160.)
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
    fn percent_row(&self, glass: bool, stacked: bool, cx: &App) -> gpui::AnyElement {
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
            .control(
                appearance_control(stacked, 208.)
                    .overflow_hidden()
                    .child(super::controls::PercentSlider::new(slider, value)),
            )
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

fn update_thread_details(state: &Entity<crate::app_state::AppState>, change: ClientSettingChange, cx: &mut App) {
    store::update_client(state, change, cx);
    if let Some(details) = crate::thread_details::details(cx) {
        let updated = store::client(cx).clone();
        details.update(cx, |details, cx| details.sync_from_client_settings(&updated, true, cx));
    }
}

pub(super) fn measure_popover_trigger(bounds: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
        .absolute()
        .inset_0()
}

fn appearance_control(stacked: bool, width: f32) -> gpui::Div {
    div().min_w_0().map(|control| {
        if stacked {
            control.w_full()
        } else {
            control.w(r(width))
        }
    })
}

pub(super) fn advanced_trigger(
    id: &'static str,
    open: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .w_full()
        .px(r(8.))
        .py(r(4.))
        .debug_selector(|| id.to_owned())
        .child(
            Button::new(id, ButtonVariant::GhostMuted, ButtonSize::Sm)
                .icon(if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                })
                .label("Advanced")
                .full_width()
                .justify_between()
                .on_click(on_click),
        )
        .into_any_element()
}

fn theme_grid_columns(window: &Window) -> usize {
    let layout = ModalLayout::of(window);
    let rem = f32::from(window.rem_size());
    let nav_rem = if layout.nav_compact { 3.5 } else { 15. };
    let available_rem =
        ((f32::from(window.viewport_size().width) - 2.) / rem - 2. * layout.inset.0).min(80.)
            - nav_rem
            - 1. / rem;
    let content_rem = available_rem.min(56.) - 3.;
    (((content_rem + 0.5) / 16.5).floor() as usize).max(1)
}

fn inset_ring(color: Hsla, width: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(width),
        inset: true,
    }]
}

const ORB_GUTTER: u32 = 2;
const ORB_TEXTURE: u32 = 64;
const ORB_CACHE_LIMIT: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
struct OrbColors {
    dark: bool,
    canvas: u32,
    accent: u32,
    action: u32,
}

impl OrbColors {
    fn new(colors: Option<&themes::Colors>, dark: bool) -> Self {
        let (canvas, accent, action) = if let Some(colors) = colors {
            let accent = colors.get("accent").unwrap_or(&colors["accentSurface"]);
            let roles = [
                ("canvas", &colors["canvas"]),
                ("surface", accent),
                ("messageAction", &colors["messageAction"]),
                ("text", &colors["text"]),
            ]
            .into_iter()
            .map(|(role, color)| (role.to_owned(), color.clone()))
            .collect();
            let [canvas, accent, action, _] = themes::swatch(&roles);
            (canvas, accent, action)
        } else if dark {
            (0x0a0a0aff, 0x1c1c1fff, 0x8b9cffff)
        } else {
            (0xfcfcfcff, 0xf4f4f5ff, 0x4f46e5ff)
        };
        Self {
            dark,
            canvas,
            accent,
            action,
        }
    }
}

thread_local! {
    static ORB_IMAGES: RefCell<VecDeque<(OrbColors, Arc<RenderImage>)>> = const { RefCell::new(VecDeque::new()) };
}

fn preview_orb(
    colors: Option<&themes::Colors>,
    dark: bool,
    diameter: f32,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let colors = OrbColors::new(colors, dark);
    let p = palette(cx);
    let border = hsla(p.background);
    let [_, _, _, edge] =
        theme::swatch(&ThemePreference::Default(AppearanceMode::System), dark, cx);
    let edge = hsla(edge).opacity(if dark { 0.14 } else { 0.10 });
    let rim = window.rem_size() / 8.;
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            if !window.content_mask().bounds.intersects(&bounds) {
                return;
            }
            let (image, evicted) = ORB_IMAGES.with_borrow_mut(|cache| {
                if let Some(index) = cache.iter().position(|(key, _)| *key == colors) {
                    let entry = cache.remove(index).expect("cached orb");
                    let image = entry.1.clone();
                    cache.push_back(entry);
                    return (image, None);
                }
                let image = rasterize_orb(colors);
                let evicted = (cache.len() == ORB_CACHE_LIMIT)
                    .then(|| cache.pop_front())
                    .flatten();
                cache.push_back((colors, image.clone()));
                (image, evicted.map(|(_, image)| image))
            });
            if let Some(image) = evicted {
                let _ = window.drop_image(image);
            }
            let radius = bounds.size.width / 2.;
            window.paint_drop_shadows(
                bounds,
                Corners::all(radius),
                &[BoxShadow {
                    color: gpui::black().opacity(if dark { 0.18 } else { 0.08 }),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(2.),
                    spread_radius: px(0.),
                    inset: false,
                }],
            );
            window.paint_quad(fill(bounds, border).corner_radii(radius));
            let visible = bounds.inset(rim);
            let fitted =
                visible.dilate(visible.size.width * (ORB_GUTTER as f32 / ORB_TEXTURE as f32));
            let _ = window.paint_image_fitted(
                visible,
                fitted,
                Corners::all(radius - rim),
                image,
                0,
                false,
            );
            window.paint_inset_shadows(bounds, Corners::all(radius), &inset_ring(edge, 1.));
        },
    )
    .size(r(diameter))
    .flex_none()
}

fn rasterize_orb(colors: OrbColors) -> Arc<RenderImage> {
    // themePreview.ts: the two circle gradients use the farthest corner as their radius.
    let (center, other, opacity, target) = if colors.dark {
        ((0.28_f32, 0.78_f32), (0.82_f32, 0.18_f32), 0.62, 0x09090bff)
    } else {
        ((0.72, 0.22), (0.18, 0.82), 0.72, 0xffffffff)
    };
    let base = mix_lab(to_lab(target), to_lab(colors.canvas), 0.8);
    let accent = to_lab(colors.accent);
    let action = to_lab(colors.action);
    let opacity_at = |x: f32, y: f32, (cx, cy): (f32, f32)| {
        (x - cx).hypot(y - cy) / cx.max(1. - cx).hypot(cy.max(1. - cy))
    };
    let sample = |x, y| {
        let x = (((x as f32 + 0.5) / ORB_TEXTURE as f32) - 0.5) / 1.1 + 0.5;
        let y = (((y as f32 + 0.5) / ORB_TEXTURE as f32) - 0.5) / 1.1 + 0.5;
        let distance = opacity_at(x, y, center);
        let accent_alpha = if distance < 0.28 {
            1. - (distance / 0.28) * (1. - opacity)
        } else {
            opacity * (1. - (distance - 0.28) / (0.58 - 0.28)).max(0.)
        };
        let action_alpha = 0.45 * (1. - opacity_at(x, y, other) / 0.55).max(0.);
        let action_alpha = action_alpha * (colors.action as u8 as f32 / 255.);
        let accent_alpha = accent_alpha * (colors.accent as u8 as f32 / 255.);
        let [red, green, blue] = from_lab(mix_lab(
            mix_lab(base, action, action_alpha),
            accent,
            accent_alpha,
        ));
        image::Rgba([blue, green, red, 255])
    };
    let interior = image::RgbaImage::from_fn(ORB_TEXTURE, ORB_TEXTURE, sample);
    let blurred = image::imageops::blur(&interior, 3. * ORB_TEXTURE as f32 / 56.);
    // GPUI's Metal atlas has no padding. Duplicate the blurred edge texels before sampling.
    let texture = image::RgbaImage::from_fn(
        ORB_TEXTURE + 2 * ORB_GUTTER,
        ORB_TEXTURE + 2 * ORB_GUTTER,
        |x, y| {
            let interior_index = |index: u32| index.saturating_sub(ORB_GUTTER).min(ORB_TEXTURE - 1);
            *blurred.get_pixel(interior_index(x), interior_index(y))
        },
    );
    Arc::new(RenderImage::new(vec![image::Frame::new(texture)]))
}

fn mix_lab(left: [f32; 3], right: [f32; 3], weight: f32) -> [f32; 3] {
    std::array::from_fn(|index| left[index] + (right[index] - left[index]) * weight)
}

fn to_lab(color: u32) -> [f32; 3] {
    let linear = |byte: u8| {
        let value = f32::from(byte) / 255.;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b, _] = color.to_be_bytes().map(linear);
    let l = (0.41222146 * r + 0.53633255 * g + 0.051445995 * b).cbrt();
    let m = (0.2119035 * r + 0.6806995 * g + 0.10739696 * b).cbrt();
    let s = (0.08830246 * r + 0.28171885 * g + 0.6299787 * b).cbrt();
    [
        0.21045426 * l + 0.7936178 * m - 0.004072047 * s,
        1.9779985 * l - 2.4285922 * m + 0.4505937 * s,
        0.025904037 * l + 0.78277177 * m - 0.80867577 * s,
    ]
}

fn from_lab([l, a, b]: [f32; 3]) -> [u8; 3] {
    let x = (l + 0.39633778 * a + 0.21580376 * b).powi(3);
    let y = (l - 0.105561346 * a - 0.06385417 * b).powi(3);
    let z = (l - 0.08948418 * a - 1.2914855 * b).powi(3);
    [
        4.0767417 * x - 3.3077116 * y + 0.23096994 * z,
        -1.268438 * x + 2.6097574 * y - 0.3413194 * z,
        -0.0041960863 * x - 0.7034186 * y + 1.7076147 * z,
    ]
    .map(|value| {
        let srgb = if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.powf(1. / 2.4) - 0.055
        };
        (srgb.clamp(0., 1.) * 255.).round() as u8
    })
}

fn wireframe(
    theme: &ThemePreference,
    mode: AppearanceMode,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let light = (mode != AppearanceMode::Dark).then(|| theme::wireframe(theme, false, cx));
    let dark = (mode != AppearanceMode::Light).then(|| theme::wireframe(theme, true, cx));
    let palette = palette(cx);
    let border = hsla(palette.border).opacity(0.6);
    let seam = hsla(palette.card);
    let radius = window.rem_size() / 2.;
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let inside = bounds.inset(px(1.));
            if inside.size.width <= px(2.) {
                return;
            }
            let inner_radius = radius - px(1.);
            if let (Some(light), Some(dark)) = (light, dark) {
                let half = inside.size.width / 2.;
                window.paint_quad(fill(inside, seam).corner_radii(inner_radius));
                let clip = size(half - px(1.), inside.size.height);
                for (x, colors) in [(inside.left(), light), (inside.center().x + px(1.), dark)] {
                    let clip_bounds = Bounds::new(point(x, inside.top()), clip);
                    window.with_content_mask(
                        Some(gpui::ContentMask {
                            bounds: clip_bounds,
                        }),
                        |window| {
                            paint_wireframe(window, inside, inner_radius, colors);
                        },
                    );
                }
            } else if let Some(colors) = light.or(dark) {
                paint_wireframe(window, inside, inner_radius, colors);
            }
            window
                .paint_quad(gpui::outline(bounds, border, BorderStyle::Solid).corner_radii(radius));
        },
    )
    .w_full()
    .h(r(140.))
}

fn paint_wireframe(window: &mut Window, bounds: Bounds<Pixels>, rim: Pixels, colors: [u32; 7]) {
    let [canvas, sidebar, surface, accent, message, action, _] = colors.map(hsla);
    let line = gpui::rgba(0x7f7f7f40);
    let percent = |x, y, w, h| {
        Bounds::new(
            point(
                bounds.left() + bounds.size.width * x,
                bounds.top() + bounds.size.height * y,
            ),
            size(bounds.size.width * w, bounds.size.height * h),
        )
    };
    window.paint_quad(fill(bounds, canvas).corner_radii(rim));
    let strip = percent(0., 0., 0.22, 1.);
    window.paint_quad(quad(
        strip,
        Corners {
            top_left: rim,
            bottom_left: rim,
            ..Corners::default()
        },
        sidebar,
        px(0.),
        line,
        BorderStyle::Solid,
    ));
    window.paint_quad(fill(
        Bounds::new(
            point(strip.right() - px(1.), strip.top()),
            size(px(1.), strip.size.height),
        ),
        line,
    ));
    for (x, y, w, h, radius, color, border) in [
        (0.03, 0.08, 0.16, 0.08, 5., surface, 1.),
        (0.03, 0.22, 0.16, 0.07, 5., accent, 0.),
        (0.03, 0.32, 0.16, 0.07, 5., message.opacity(0.7), 0.),
        (0.03, 0.42, 0.16, 0.07, 5., message.opacity(0.5), 0.),
        (0.48, 0.11, 0.24, 0.09, 6., message, 0.),
        (0.27, 0.28, 0.34, 0.05, 3., line.into(), 0.),
        (0.27, 0.38, 0.26, 0.05, 3., line.into(), 0.),
        (0.26, 0.77, 0.68, 0.15, 5., surface, 1.),
        (0.29, 0.82, 0.22, 0.04, 3., line.opacity(0.7).into(), 0.),
    ] {
        window.paint_quad(quad(
            percent(x, y, w, h),
            px(radius),
            color,
            px(border),
            line,
            BorderStyle::Solid,
        ));
    }
    let send = (bounds.size.height * 0.09).min(bounds.size.width * 0.13);
    let send_bounds = Bounds::centered_at(percent(0.89, 0.845, 0., 0.).origin, size(send, send));
    window.paint_quad(fill(send_bounds, action).corner_radii(send / 2.));
    let details = percent(0.75, 0.08, 0.20, 0.46);
    window.paint_drop_shadows(
        details,
        Corners::all(px(6.)),
        &[BoxShadow {
            color: gpui::black().opacity(0.14),
            offset: point(px(0.), px(2.)),
            blur_radius: px(5.),
            spread_radius: px(0.),
            inset: false,
        }],
    );
    window.paint_quad(quad(
        details,
        px(6.),
        surface,
        px(1.),
        line,
        BorderStyle::Solid,
    ));
    let diameter = (details.size.height * 0.052).min(details.size.width * 0.15);
    for (index, ink) in [hsla(0x34d399ff), action, hsla(0xfbbf24ff)]
        .into_iter()
        .enumerate()
    {
        let y = 0.08 + 0.46 * (0.20 + index as f32 * 0.30);
        let dot = Bounds::centered_at(percent(0.784, y, 0., 0.).origin, size(diameter, diameter));
        window.paint_quad(fill(dot, ink.opacity(0.55)).corner_radii(diameter / 2.));
        window.paint_quad(fill(percent(0.82, y - 0.014, 0.083, 0.028), line).corner_radii(px(2.)));
    }
}

#[derive(IntoElement)]
struct PanelAnimationPreview {
    open: bool,
    duration: Duration,
    view: gpui::WeakEntity<SettingsView>,
}

impl RenderOnce for PanelAnimationPreview {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let progress = crate::components::motion::transition(
            "settings-panel-animation-preview",
            if self.open { 1. } else { 0. },
            self.duration,
            window,
            cx,
        );
        let p = palette(cx);
        let label = t(cx, "Replay panel animation preview");
        let focus = focus_handle(&"panel-animations-preview".into(), window, cx);
        h_flex()
            .id("panel-animations-preview")
            .debug_selector(|| "panel-animations-preview".to_owned())
            .w_full()
            .h(r(40.))
            .p(r(4.))
            .overflow_hidden()
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.background))
            .cursor_pointer()
            .track_focus(&focus)
            .tab_index(0)
            .role(gpui::Role::Button)
            .aria_label(label.to_owned())
            .focus_visible(|s| s.shadow(inset_ring(hsla(p.ring), 2.)))
            .tooltip(tooltip(label.to_owned().into()))
            .on_click(move |_, _, cx| {
                let _ = self.view.update(cx, |this, cx| {
                    this.appearance_ui.panel_preview_open = !this.appearance_ui.panel_preview_open;
                    cx.notify();
                });
            })
            .child(
                div()
                    .w(r(16. * progress))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(div().w(r(16.)).h_full().rounded(r(4.)).bg(hsla(p.sidebar))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px(r(4.))
                    .child(
                        v_flex().flex_1().min_h_0().gap(r(4.)).pt(r(4.)).children(
                            [1., 0.8, 0.6]
                                .into_iter()
                                .enumerate()
                                .map(|(index, width)| {
                                    div()
                                        .h(r(2.))
                                        .w(gpui::relative(width))
                                        .rounded_full()
                                        .bg(hsla(p.muted_foreground)
                                            .opacity(0.25 - 0.05 * index as f32))
                                }),
                        ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .h(r(8. * progress))
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .h(r(8.))
                                    .items_center()
                                    .px(r(6.))
                                    .bg(hsla(p.foreground).opacity(0.05))
                                    .border_t_1()
                                    .border_color(hsla(p.border).opacity(0.7))
                                    .child(
                                        div()
                                            .h(px(1.))
                                            .w(gpui::relative(0.66))
                                            .rounded_full()
                                            .bg(hsla(p.muted_foreground).opacity(0.25)),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .w(r(20. * progress))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(div().w(r(20.)).h_full().rounded(r(4.)).bg(hsla(p.muted))),
            )
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
            .w_full()
            .min_w_0()
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
                    .child(format!(
                        "{} ms",
                        crate::i18n::format_integer(cx, self.value.into())
                    )),
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
        let trigger = vcx
            .debug_bounds("thread-details-items-trigger")
            .expect("trigger");
        let menu = vcx
            .debug_bounds("thread-details-items-popover")
            .expect("menu");
        assert!(
            (menu.right() - trigger.right()).abs() <= px(1.),
            "{menu:?} should align to {trigger:?}"
        );
        assert!(menu.top() >= trigger.bottom());
        let usage_item = vcx
            .debug_bounds("thread-details-item-usage")
            .expect("usage checkbox")
            .center();
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
    fn collection_previews_each_mode_and_opens_a_removal_modal(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.executor().allow_parking();
        let (state, _fixture) = cx.update(|cx| {
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        cx.run_until_parked();
        cx.update(|cx| {
            crate::composer::init(cx);
            crate::keymap::init(cx);
            crate::settings::init(state.clone(), cx);
            crate::updates::init(cx);
            let [light, dark] = ["light", "dark"].map(|appearance| ThemeDefinition {
                id: format!("parity-test-{appearance}"),
                label: format!("Example {appearance}"),
                appearance: appearance.to_owned(),
                colors: themes::defaults(appearance == "dark"),
                variants: Default::default(),
                collection: Some(themes::Collection {
                    id: "parity-test".into(),
                    label: "Example collection".into(),
                }),
                managed: false,
                css: None,
                background: None,
            });
            store::install_committed_client(
                &state,
                serde_json::Map::from_iter([(themes::LIBRARY_KEY.into(), json!([light, dark]))]),
                cx,
            );
        });
        let window = cx.add_window(|_, _| SettingsFrame);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
        };
        vcx.update(|_, cx| crate::settings::open(Section::Appearance, cx));
        draw(&mut vcx);
        let light = vcx
            .debug_bounds("theme-half-collection-parity-test-light")
            .expect("light orb");
        let dark = vcx
            .debug_bounds("theme-half-collection-parity-test-dark")
            .expect("dark orb");
        assert!(light.right() < dark.left());
        assert_eq!(light.size, dark.size);

        let removal = "remove-parity-test-light";
        let view = vcx.read(|cx| {
            cx.global::<crate::settings::Host>()
                .view
                .clone()
                .expect("settings view")
        });
        let remove_bounds = vcx.debug_bounds(removal).expect("remove button");
        view.update(&mut vcx, |view, _| {
            view.scroll
                .set_offset(point(px(0.), (px(340.) - remove_bounds.top()).min(px(0.))));
        });
        draw(&mut vcx);
        let remove_center = vcx.debug_bounds(removal).expect("visible remove button").center();
        vcx.simulate_click(remove_center, Default::default());
        draw(&mut vcx);
        assert!(vcx.debug_bounds("theme-removal-modal").is_some());

        vcx.simulate_keystrokes("escape");
        draw(&mut vcx);
        assert!(vcx.debug_bounds("theme-removal-modal").is_none());
        assert!(
            vcx.debug_bounds("theme-card-collection-parity-test")
                .is_some()
        );
        vcx.read(|cx| assert_eq!(themes::library(store::client(cx)).len(), 2));
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
