//! Appearance: color scheme tiles and the theme library (`ThemeSettings.tsx` `ThemeLibrary`), chat
//! width (`AppearanceSettingsPanel.tsx`) and typography (`TypographySettings.tsx`).
//! Choices apply live through `crate::theme`. Custom, imported and environment themes, contrast,
//! glass opacity are not ported yet.

use gpui::{App, Context, ElementId, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::client_settings::{
    AppearanceMode, ChatWidth, ClientSettingChange,
    ThemePreference,
};

use super::controls::{Row, Select, SelectOption, r, section, section_title};
use super::{Block, SettingsView, store};
use crate::theme::{BUILT_IN_THEMES, hsla, palette, swatch};

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
                    false => this.border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.card).opacity(0.6)).hover(|this| this.bg(hsla(p.accent).opacity(0.1))),
                })
                .child(wireframe(&theme, tile_mode))
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

        let cards: Vec<(ThemePreference, SharedString)> = std::iter::once((ThemePreference::Default(mode), SharedString::from("T3 Code")))
            .chain(BUILT_IN_THEMES.iter().map(|named| (ThemePreference::Named(named.id.to_owned()), SharedString::from(named.label))))
            .collect();
        let grid = div().grid().grid_cols(3).w_full().gap(r(8.)).children(cards.into_iter().map(|(card, label)| {
            let active = card.named_id() == theme.named_id();
            let state = self.state.clone();
            let chosen = match &card {
                ThemePreference::Default(_) => ThemePreference::Default(mode),
                named => named.clone(),
            };
            v_flex()
                .id(ElementId::Name(format!("theme-card-{label}").into()))
                .rounded(r(12.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.7))
                .cursor_pointer()
                .overflow_hidden()
                .map(|this| match active {
                    true => this.bg(hsla(p.accent).opacity(0.3)).shadow(ring(hsla(p.ring))),
                    false => this.bg(hsla(p.card).opacity(0.6)).hover(|this| this.bg(hsla(p.accent).opacity(0.1))),
                })
                .child(
                    h_flex()
                        .h(r(80.))
                        .justify_center()
                        .gap(r(12.))
                        .child(preview_circle(&card, false, active && !is_dark(mode, cx)))
                        .child(preview_circle(&card, true, active && is_dark(mode, cx))),
                )
                .child(div().px(r(12.)).pt(r(8.)).pb(r(12.)).text_sm().font_weight(FontWeight::MEDIUM).truncate().child(label))
                .on_click(move |_, _, cx| store::update_client(&state, ClientSettingChange::Theme(chosen.clone()), cx))
        }));

        let colors = v_flex()
            .gap(r(12.))
            .child(section_title("Color scheme"))
            .child(tiles)
            .child(div().pt(r(20.)).child(section_title("Themes")))
            .child(grid);

        let width_state = self.state.clone();
        let interface = section(
            "Interface",
            vec![
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
            Block::new(&["appearance-interface", "chat-width"], interface),
            Block::new(&["typography", "interface-font", "prompt-font", "code-font", "terminal-font", "word-wrap", "separate-fonts", "font-smoothing"], typography),
        ]
    }
}

fn is_dark(mode: AppearanceMode, cx: &App) -> bool {
    match mode {
        AppearanceMode::System => gpui_component::ActiveTheme::theme(cx).is_dark(),
        AppearanceMode::Light => false,
        AppearanceMode::Dark => true,
    }
}

fn ring(color: Hsla) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow { color, offset: gpui::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: false }]
}

/// A theme half as a 40px swatch: its canvas, a card band and an accent dot. The half in use gets
/// a ring.
fn preview_circle(theme: &ThemePreference, dark: bool, active: bool) -> impl IntoElement {
    let [background, card, primary, foreground] = swatch(theme, dark);
    div()
        .size(r(40.))
        .rounded_full()
        .overflow_hidden()
        .border_1()
        .border_color(hsla(foreground).opacity(0.12))
        .bg(hsla(background))
        .when(active, |this| this.shadow(vec![gpui::BoxShadow {
            color: hsla(foreground).opacity(0.6),
            offset: gpui::point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(2.),
            inset: false,
        }]))
        .flex()
        .flex_col()
        .justify_end()
        .child(h_flex().h(r(16.)).bg(hsla(card)).justify_center().child(div().size(r(8.)).rounded_full().bg(hsla(primary))))
}

/// The mode tile's miniature: a sidebar strip and content lines in the theme's colors; System
/// shows the light half beside the dark half.
fn wireframe(theme: &ThemePreference, mode: AppearanceMode) -> impl IntoElement {
    let half = |dark: bool| {
        let [background, card, primary, foreground] = swatch(theme, dark);
        h_flex()
            .flex_1()
            .h_full()
            .bg(hsla(background))
            .child(div().w(r(28.)).h_full().bg(hsla(card)))
            .child(
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
