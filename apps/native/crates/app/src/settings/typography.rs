//! `TypographySettings.tsx` and `FontFamilyPicker.tsx`: device typography with installed fonts.

#[path = "font_previews.rs"]
mod previews;

use super::{
    SettingsView,
    appearance::{advanced_trigger, measure_popover_trigger},
    controls::{
        ModalLayout, Row, Select, SelectOption, Switch, focus_handle, icon, muted_text, r, section,
    },
    store,
};
use crate::{
    i18n::t,
    icons::Icon,
    theme::{
        self, SlotState, ThemeSlot, ThemeSlotExt, ThemeSlotInteractiveExt, hsla, palette,
        theme_slot_clipped_background_layer,
    },
};
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, Focusable, FontStyle, FontWeight, IntoElement, ParentElement, Styled,
    Subscription, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use std::{cell::Cell, rc::Rc};
use t3_core::client_settings::{
    ClientSettingChange,
    typography::{FontInventory, FontRole, advances_are_monospace, family_options},
};

#[derive(Default)]
struct CachedFonts(FontInventory);
impl gpui::Global for CachedFonts {}

gpui::actions!(font_family_picker, [CloseFontPicker]);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new("escape", CloseFontPicker, Some("FontFamilyPicker"))]);
}

pub(super) struct TypographyState {
    advanced: bool,
    fonts: Rc<Vec<(String, bool)>>,
    font_task: Option<gpui::Task<()>>,
    open: Option<FontRole>,
    search: Entity<InputState>,
    highlighted: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
    previews: previews::FontPreviews,
}

impl TypographyState {
    pub(super) fn text_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.previews.focused(window, cx)
    }

    pub fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        if !cx.has_global::<CachedFonts>() {
            cx.set_global(CachedFonts::default());
        }

        let search = cx.new(|cx| {
            let placeholder = t(cx, "Search fonts…").to_owned();
            InputState::new(window, cx).placeholder(placeholder)
        });
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_focus("font-picker-search", search.read(cx).focus_handle(cx), cx);
        let subscription = cx.subscribe(&search, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.typography.highlighted = 0;
                view.typography.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                cx.notify();
            }
        });
        Self {
            previews: previews::FontPreviews::new(window, cx),
            advanced: false,
            fonts: Rc::new(cx.global::<CachedFonts>().0.families().unwrap_or_default().to_vec()),
            font_task: None,
            open: None,
            search,
            highlighted: 0,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        }
    }
}

/// Synthetic Apple font labels (`"SF Pro"`, `"SF Mono"`) are not registered as named system font
/// families, so map them to the real UI/monospace stacks instead of falling back to serif
/// (`displayFontFamily` in `appearanceFonts.ts`, `pingdotgg/t3code#24082`).
fn display_font_family(family: &str, role: FontRole, cx: &App) -> gpui::SharedString {
    if family.is_empty() {
        return theme::font_family(role, cx);
    }
    let trimmed = family.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| trimmed.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(trimmed)
        .trim();
    if unquoted.eq_ignore_ascii_case("sf pro") {
        gpui::SharedString::from(".SystemUIFont")
    } else if unquoted.eq_ignore_ascii_case("sf mono") {
        theme::default_mono_family(cx).to_owned().into()
    } else {
        family.to_owned().into()
    }
}

impl SettingsView {
    pub(super) fn typography_rows(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let targeted = ["separate-fonts"].iter().any(|id| self.is_target(id));
        let separate = store::client(cx).separate_fonts();
        let roles: &[FontRole] = if separate {
            &FontRole::ALL
        } else {
            &[FontRole::Interface, FontRole::Code]
        };
        let mut rows: Vec<AnyElement> = roles.iter().map(|role| self.font_row(*role, separate, window, cx)).collect();
        let doc = store::client(cx);
        let wrap = doc.word_wrap();
        let state = self.state.clone();
        let reset = state.clone();
        rows.push(
            Row::new("word-wrap", "Word wrap")
                .description("Wrap long lines in code blocks, diffs, and file previews by default.")
                .highlighted(self.is_target("word-wrap"))
                .reset(!wrap, move |_, _, cx| {
                    store::update_client(&reset, ClientSettingChange::WordWrap(true), cx)
                })
                .control(
                    Switch::new("word-wrap-switch", wrap)
                        .on_change(move |on, _, cx| store::update_client(&state, ClientSettingChange::WordWrap(on), cx)),
                )
                .into_any_element(),
        );
        let smoothing_targeted = cfg!(target_os = "macos") && self.is_target("font-smoothing");
        let advanced = self.typography.advanced || targeted || smoothing_targeted;
        rows.push(advanced_trigger(
            "typography-advanced",
            advanced,
            cx.listener(move |this, _, _, cx| {
                this.typography.advanced = !advanced;
                this.target = None;
                cx.notify();
            }),
        ));
        if advanced {
            let state = self.state.clone();
            rows.push(
                Row::new("separate-fonts", "Separate fonts")
                    .description("Set the prompt, code, and terminal fonts separately instead of one monospace font.")
                    .highlighted(self.is_target("separate-fonts"))
                    .control(
                        Switch::new("separate-fonts-switch", separate)
                            .on_change(move |on, _, cx| store::update_client(&state, ClientSettingChange::SeparateFonts(on), cx)),
                    )
                    .into_any_element(),
            );
            #[cfg(target_os = "macos")]
            rows.push(
                Row::new("font-smoothing", "Font smoothing")
                    .description("Use thinner grayscale text smoothing instead of the macOS default.")
                    .highlighted(self.is_target("font-smoothing"))
                    .status(
                        "GPUI has no live macOS smoothing override matching the web preference.",
                    )
                    .control(muted_text(t(cx, "n/a").to_owned(), cx))
                    .into_any_element(),
            );
        }
        section("Typography", rows, cx).into_any_element()
    }

    fn font_row(
        &mut self,
        role: FontRole,
        separate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let doc = store::client(cx);
        let size = doc.font_size(role);
        let reset = !doc.font_family(role).is_empty() || size != role.default_size();
        let state = self.state.clone();
        let size_state = state.clone();
        let title = if !separate && role == FontRole::Code {
            "Monospace font"
        } else {
            role.title()
        };
        let description = if !separate && role == FontRole::Code {
            "Code blocks, diffs, file previews, and the terminal."
        } else {
            role.description()
        };
        let stacked = ModalLayout::of(window).rows_stacked;
        let control = h_flex()
            .gap(r(8.))
            .when(stacked, |row| row.w_full().min_w_0())
            .child(self.font_picker(role, stacked, window, cx))
            .child(
                Select::new(
                    ElementId::Name(format!("{}-size", role.id()).into()),
                    role.range()
                        .map(|size| SelectOption::new(size.to_string(), format!("{size} px")))
                        .collect(),
                )
                .selected(size.to_string())
                .width(88.)
                .on_change(move |value, _, cx| {
                    if let Ok(size) = value.parse() {
                        store::update_client(
                            &size_state,
                            ClientSettingChange::FontSize(role, size),
                            cx,
                        );
                    }
                }),
            );
        let row = Row::new(role.id(), title)
            .description(description)
            .highlighted(self.is_target(role.id()))
            .reset(reset, move |_, _, cx| {
                store::update_client(&state, ClientSettingChange::ResetFont(role), cx)
            })
            .control(control);
        let preview_role = if !separate && role == FontRole::Interface {
            FontRole::Prompt
        } else {
            role
        };
        if role == FontRole::Interface && separate {
            return row.into_any_element();
        }
        let mut previews = v_flex().gap(r(4.)).child(self.typography.previews.render(preview_role, cx));
        if !separate && role == FontRole::Code {
            previews = previews.child(self.typography.previews.render(FontRole::Terminal, cx));
        }
        row.below(previews).into_any_element()
    }

    fn font_picker(
        &mut self,
        role: FontRole,
        stacked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let doc = store::client(cx);
        let family = doc.font_family(role).to_owned();
        let default = match role {
            FontRole::Interface => "SF Pro".to_owned(),
            FontRole::Prompt => {
                let name = theme::font_family(FontRole::Interface, cx);
                if name.as_ref() == ".SystemUIFont" {
                    "SF Pro".to_owned()
                } else {
                    name.to_string()
                }
            }
            _ => theme::default_mono_family(cx).to_owned(),
        };
        let label = if family.is_empty() {
            default.clone()
        } else {
            family.clone()
        };
        let p = palette(cx);
        let open = self.typography.open == Some(role);
        let id = ElementId::Name(format!("{}-family", role.id()).into());
        let focus = focus_handle(&id, window, cx);
        let return_focus = focus.clone();
        let trigger_bounds = Rc::new(Cell::new(gpui::Bounds::default()));
        let focused = focus.is_focused(window);
        let background = if cx.theme().is_dark() {
            hsla(p.input).opacity(0.32)
        } else {
            hsla(p.background)
        };
        let trigger = h_flex()
            .id(id)
            .relative()
            .w_full()
            .min_w_0()
            .min_h(r(28.))
            .px(r(9.))
            .gap(r(8.))
            .track_focus(&focus)
            .tab_index(0)
            .role(gpui::Role::Button)
            .aria_label(t(cx, role.title()).to_owned())
            .cursor_pointer()
            .rounded(r(8.))
            .border_1()
            .border_color(hsla(if focused || open { p.ring } else { p.input }))
            .bg(background)
            .text_sm()
            .text_color(hsla(p.foreground))
            .theme_slot_interactive(ThemeSlot::SelectTrigger, cx)
            .when(focused || open, |view| {
                view.theme_slot_state(ThemeSlot::SelectTrigger, SlotState::FocusWithin, cx)
            })
            .children(theme_slot_clipped_background_layer(
                ThemeSlot::SelectTrigger,
                r(8.),
                cx,
            ))
            .child(div().relative().flex_1().min_w_0().truncate().child(label))
            .child(icon(Icon::ChevronDown, 14., hsla(p.icon_muted)))
            .child(measure_popover_trigger(trigger_bounds.clone()))
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.typography.open == Some(role) {
                    this.typography.open = None;
                    cx.notify();
                    return;
                }
                if cx.global::<CachedFonts>().0.families().is_none()
                    && this.typography.font_task.is_none()
                {
                    let text = cx.text_system().clone();
                    let names = theme::installed_font_families(cx).to_vec();
                    let task = cx.background_executor().spawn(async move { installed_fonts(text, names) });
                    this.typography.font_task = Some(cx.spawn(async move |view, cx| {
                        let fonts = task.await;
                        let _ = view.update(cx, |view, cx| {
                            cx.global_mut::<CachedFonts>().0.finish(fonts);
                            view.typography.fonts = Rc::new(cx.global::<CachedFonts>().0.families().unwrap().to_vec());
                            view.typography.font_task = None;
                            cx.notify();
                        });
                    }));
                }
                this.typography.open = Some(role);
                this.typography.search.update(cx, |search, cx| {
                    search.set_value("", window, cx);
                    search.focus_handle(cx).focus(window, cx);
                });
                cx.notify();
            }));
        let wrapper = div()
            .relative()
            .min_w_0()
            .map(|view| {
                if stacked {
                    view.flex_1()
                } else {
                    view.w(r(180.))
                }
            })
            .child(trigger);
        if !open {
            return wrapper.into_any_element();
        }
        let query = self
            .typography
            .search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let mut choices = family_options(self.typography.fonts.iter().cloned(), false);
        choices.retain(|name| name.to_lowercase().contains(&query));
        if query.is_empty() {
            choices.insert(0, String::new());
        }
        let choices = Rc::new(choices);
        let count = choices.len();
        let loading = self.typography.font_task.is_some();
        let keyboard_choices = choices.clone();
        let selected = family;
        let highlighted = self.typography.highlighted;
        // Short windows cap the popup; shrink the list viewport so every font stays scrollable
        // (`pingdotgg/t3code#22795`).
        let viewport_height = f32::from(window.viewport_size().height);
        let max_list_height = (viewport_height - 96.).clamp(56., 288.);
        let desired_list_height = (count.clamp(1, 9) * 28) as f32;
        let list_height = desired_list_height.min(max_list_height);
        let list = uniform_list(
            ElementId::Name(format!("{}-font-options", role.id()).into()),
            count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|index| {
                        let family = choices[index].clone();
                        let is_default = family.is_empty();
                        let label = if is_default {
                            default.clone()
                        } else {
                            family.clone()
                        };
                        let selected = family == selected;
                        div()
                            .id(("font", index))
                            .h(r(28.))
                            .px(r(8.))
                            .flex()
                            .items_center()
                            .gap(r(8.))
                            .text_sm()
                            .rounded(r(4.))
                            .text_color(hsla(p.foreground))
                            .when(index == highlighted, |this| this.bg(hsla(p.accent)))
                            .cursor_pointer()
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.typography.highlighted != index {
                                    this.typography.highlighted = index;
                                    cx.notify();
                                }
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .font_family(display_font_family(&family, role, cx))
                                    .child(label),
                            )
                            .when(is_default, |row| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(t(cx, "default").to_owned()),
                                )
                            })
                            .child(div().w(r(14.)).flex_none().children(
                                selected.then(|| icon(Icon::Check, 14., hsla(p.foreground))),
                            ))
                            .on_click(cx.listener({
                                let focus = focus.clone();
                                move |this, _, window, cx| {
                                    this.pick_font(role, family.clone(), cx);
                                    focus.focus(window, cx);
                                }
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.typography.scroll)
        .min_h_0()
        .h(r(list_height));
        let escape_focus = return_focus.clone();
        let popup = crate::composer::controls::popup_surface(cx)
            .id("font-picker-popup")
            .w(r(320.))
            .occlude()
            .key_context("FontFamilyPicker")
            .on_action(cx.listener(move |this, _: &CloseFontPicker, window, cx| {
                this.typography.open = None;
                escape_focus.focus(window, cx);
                cx.notify();
            }))
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                if !trigger_bounds.get().contains(&event.position) {
                    this.typography.open = None;
                    cx.notify();
                }
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            this.typography.open = None;
                            return_focus.focus(window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }
                        "up" | "down" if count > 0 => {
                            let step = if event.keystroke.key == "down" {
                                1
                            } else {
                                count - 1
                            };
                            this.typography.highlighted =
                                (this.typography.highlighted + step) % count;
                            this.typography.scroll.scroll_to_item(
                                this.typography.highlighted,
                                gpui::ScrollStrategy::Nearest,
                            );
                            cx.stop_propagation();
                            cx.notify();
                        }
                        "enter" if count > 0 => {
                            let index = this.typography.highlighted.min(count - 1);
                            this.pick_font(role, keyboard_choices[index].clone(), cx);
                            return_focus.focus(window, cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }),
            )
            .child(
                h_flex()
                    .gap(r(8.))
                    .px(r(8.))
                    .pb(r(4.))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .child(icon(Icon::Search, 16., hsla(p.muted_foreground)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .relative()
                            .children(theme_slot_clipped_background_layer(
                                ThemeSlot::Input,
                                r(8.),
                                cx,
                            ))
                            .child(
                                Input::new(&self.typography.search)
                                    .appearance(false)
                                    .small()
                                    .theme_slot(ThemeSlot::Input, cx),
                            ),
                    ),
            )
            .child(list)
            .when(loading, |this| {
                this.child(muted_text(t(cx, "Loading fonts…").to_owned(), cx))
            })
            .when(count == 0 && !loading, |this| {
                this.child(muted_text(t(cx, "No fonts found").to_owned(), cx))
            });
        wrapper
            .child(
                div().absolute().bottom_0().right_0().child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopRight)
                            .snap_to_window_with_margin(px(8.))
                            .child(div().mt(r(4.)).child(popup)),
                    )
                    .with_priority(2),
                ),
            )
            .into_any_element()
    }

    fn pick_font(&mut self, role: FontRole, family: String, cx: &mut Context<Self>) {
        self.typography.open = None;
        if !cx.global::<CachedFonts>().0.accepts(role, &family) {
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    crate::toast::Toast::new(crate::toast::ToastKind::Error, format!("\"{family}\" isn't monospace"))
                        .description("Code and terminal need a fixed-width font, so the current font was kept."),
                    cx,
                );
            });
            // GPUI can retain blank cached text after closing the multi-font popup.
            cx.refresh_windows();
        } else {
            store::update_client(&self.state, ClientSettingChange::FontFamily(role, family), cx);
        }
        cx.notify();
    }
}

fn installed_fonts(text: std::sync::Arc<gpui::TextSystem>, names: Vec<String>) -> Vec<(String, bool)> {
    names
        .into_iter()
        .map(|family| {
            let mono = [FontWeight::NORMAL, FontWeight::BOLD].into_iter().all(|weight| {
                [FontStyle::Normal, FontStyle::Italic].into_iter().all(|style| {
                    let mut font = gpui::font(family.clone());
                    font.weight = weight;
                    font.style = style;
                    let id = text.resolve_font(&font);
                    let advances: Vec<_> = ['i', 'M', 'W', '0', '@', '#', '.', ' ']
                        .into_iter()
                        .map(|glyph| {
                            text.advance(id, px(32.), glyph)
                                .map(|size| f32::from(size.width))
                                .unwrap_or(f32::NAN)
                        })
                        .collect();
                    advances_are_monospace(&advances)
                })
            });
            (family, mono)
        })
        .collect()
}
