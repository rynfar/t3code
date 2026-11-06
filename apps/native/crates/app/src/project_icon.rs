//! Shared `web/src/components/ProjectFavicon.tsx` / `ProjectMonogram.tsx` presentation.

use std::sync::Arc;

use gpui::{AnyElement, App, FontWeight, Hsla, Image, IntoElement, ObjectFit, ParentElement, Pixels, SharedString, Styled, StyledImage, div, img, prelude::*};
use t3_core::client_settings::typography::FontRole;
use t3_core::settings::projects::{
    ProjectIcon,
    icon::{Presentation, Source, monogram_width},
};

use crate::{
    app_state::AppState,
    icons::Icon,
    theme::{Palette, hsla, palette},
};

#[derive(Clone, Default)]
pub struct ProjectVisual {
    pub presentation: Option<Arc<Presentation>>,
    pub image: Option<Arc<Image>>,
}

impl ProjectVisual {
    pub fn for_project(state: &AppState, id: &str) -> Self {
        state.sidebar.favicons.visual(id)
    }

    pub fn for_scoped_project(state: &AppState, key: &t3_core::sidebar::grouping::ProjectRef) -> Self {
        let Some(project) = state.scoped_project(key) else { return Self::default() };
        state.sidebar.owner_favicons.get(&project.owner).map(|store| store.visual(&key.project_id)).unwrap_or_else(|| Self {
            presentation: Some(Arc::new(Presentation::new(project.project.project_icon.as_ref(), &project.project.title))), image: None,
        })
    }

    pub fn render(&self, size: Pixels, muted: Hsla, cx: &App) -> AnyElement {
        self.render_with_grayscale(size, muted, false, cx)
    }

    pub fn render_receding(&self, size: Pixels, muted: Hsla, cx: &App) -> AnyElement {
        div()
            .flex_none()
            .relative()
            .size(size)
            .child(div().opacity(0.4).group_hover("sidebar-row", |this| this.invisible()).child(self.render_with_grayscale(size, muted, true, cx)))
            .child(div().absolute().top_0().left_0().invisible().group_hover("sidebar-row", |this| this.visible()).child(self.render(size, muted, cx)))
            .into_any_element()
    }

    fn render_with_grayscale(&self, size: Pixels, muted: Hsla, grayscale: bool, cx: &App) -> AnyElement {
        let p = palette(cx);
        let font = crate::theme::font_family(FontRole::Code, cx);
        match self.presentation.as_ref().map(|presentation| presentation.source(self.image.is_some())) {
            Some(Source::Image) => {
                let fallback = self.presentation.clone();
                let loading = fallback.clone();
                let loading_font = font.clone();
                img(self.image.clone().expect("image source"))
                    .flex_none()
                    .size(size)
                    .rounded(size * 0.25)
                    .object_fit(ObjectFit::Contain)
                    .grayscale(grayscale)
                    .with_fallback(move || render_source(fallback.as_ref().map(|p| p.source(false)), size, muted, p, grayscale, font.clone()))
                    .with_loading(move || render_source(loading.as_ref().map(|p| p.source(false)), size, muted, p, grayscale, loading_font.clone()))
                    .into_any_element()
            }
            source => render_source(source, size, muted, p, grayscale, font),
        }
    }
}

fn render_source(source: Option<Source<'_>>, size: Pixels, muted: Hsla, p: Palette, grayscale: bool, font: SharedString) -> AnyElement {
    let ink = |color: &str| {
        let mut ink = color_token(color, p);
        if grayscale {
            // Match the web's CSS grayscale and GPUI's image grayscale shader.
            let rgb = ink.to_rgb();
            ink.s = 0.;
            ink.l = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
        }
        ink
    };
    match source {
        Some(Source::Icon(ProjectIcon::Emoji { emoji })) => div()
            .flex_none()
            .size(size)
            .flex()
            .items_center()
            .justify_center()
            .text_size(size * 0.8)
            .line_height(size)
            .font_weight(FontWeight::NORMAL)
            .child(SharedString::from(emoji.clone()))
            .into_any_element(),
        Some(Source::Icon(ProjectIcon::Monogram { text, color })) => {
            let ink = ink(color);
            // GPUI lacks SVG textLength: keep the 8.25/16 type and 6/12-unit target, without stretching glyphs.
            div()
                .flex_none()
                .size(size)
                .rounded(size * 0.25)
                .overflow_hidden()
                .bg(ink.opacity(0.14))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .max_w(size * (monogram_width(text) / 16.))
                        .text_size(size * (8.25 / 16.))
                        .line_height(size)
                        .font_family(font)
                        .font_weight(FontWeight::BOLD)
                        .text_color(ink)
                        .child(SharedString::from(text.clone())),
                )
                .into_any_element()
        }
        Some(Source::Icon(ProjectIcon::Lucide { name, color })) => {
            Icon::project_named(name).unwrap_or(Icon::FolderCode).render(size, ink(color)).into_any_element()
        }
        _ => Icon::FolderCode.render(size, muted).into_any_element(),
    }
}

pub(crate) fn color_token(color: &str, p: Palette) -> Hsla {
    hsla(match color {
        "gray" => p.project_icon_gray,
        "red" => p.project_icon_red,
        "orange" => p.project_icon_orange,
        "amber" => p.project_icon_amber,
        "yellow" => p.project_icon_yellow,
        "lime" => p.project_icon_lime,
        "green" => p.project_icon_green,
        "emerald" => p.project_icon_emerald,
        "teal" => p.project_icon_teal,
        "cyan" => p.project_icon_cyan,
        "sky" => p.project_icon_sky,
        "blue" => p.project_icon_blue,
        "indigo" => p.project_icon_indigo,
        "violet" => p.project_icon_violet,
        "purple" => p.project_icon_purple,
        "fuchsia" => p.project_icon_fuchsia,
        "pink" => p.project_icon_pink,
        "rose" => p.project_icon_rose,
        _ => p.icon_muted,
    })
}

pub fn render(state: &AppState, id: &str, size: Pixels, muted: Hsla, cx: &App) -> AnyElement {
    ProjectVisual::for_project(state, id).render(size, muted, cx)
}

pub fn size(web_px: f32, cx: &App) -> Pixels {
    gpui::px(web_px * crate::theme::preferences(cx).interface_font_size / crate::theme::metrics::ROOT_FONT_SIZE)
}

pub(crate) fn preview(icon: &ProjectIcon, size: Pixels, cx: &App) -> AnyElement {
    render_source(
        Some(Source::Icon(icon)),
        size,
        hsla(palette(cx).icon_muted),
        palette(cx),
        false,
        crate::theme::font_family(FontRole::Code, cx),
    )
}
