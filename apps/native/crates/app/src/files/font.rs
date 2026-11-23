//! Font file preview (`.ttf`, `.otf`, `.woff`, `.woff2`): parses SFNT/WOFF metadata off-thread,
//! registers native `.ttf`/`.otf` bytes into GPUI's `TextSystem::add_fonts`, and renders the
//! `FontViewer.tsx` specimen at 12 / 16 / 24 / 40 / 64 px.

use std::borrow::Cow;

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, ScrollHandle, SharedString, Styled,
    div, prelude::*, px,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::files::media_preview::{
    FONT_SPECIMEN_LOWER, FONT_SPECIMEN_PANGRAM, FONT_SPECIMEN_SIZES, FONT_SPECIMEN_SYMBOLS,
    FONT_SPECIMEN_UPPER, FontInfo, MAX_FONT_BYTES, parse_font_info,
};
use t3_core::files::{absolute_path, group_thousands, workspace_asset_request};

use super::Root;
use super::preview::{AssetFetchOutcome, KnownAsset, load_workspace_asset};
use crate::app_state::AppState;
use crate::theme::{hsla, palette};

pub struct DecodedFont {
    pub info: FontInfo,
    pub bytes: Vec<u8>,
}

pub enum FontState {
    Loading,
    Ready {
        info: FontInfo,
        loaded_family: Option<SharedString>,
        scroll: ScrollHandle,
    },
    Failed(String),
}

pub(super) fn load(
    state: &AppState,
    root: &Root,
    path: &str,
    mutation: Option<&str>,
    known: KnownAsset,
) -> Option<tokio::task::JoinHandle<Result<AssetFetchOutcome, String>>> {
    let request = workspace_asset_request(
        &root.workspace.thread_id,
        &root.workspace.cwd,
        &absolute_path(path, &root.workspace.cwd),
    );
    load_workspace_asset(state, request, MAX_FONT_BYTES, known, mutation, "The font could not be loaded.")
}

pub fn decode(bytes: Vec<u8>) -> Result<DecodedFont, String> {
    let info = parse_font_info(&bytes)?;
    Ok(DecodedFont { info, bytes })
}

impl FontState {
    pub fn from_decoded(result: Result<DecodedFont, String>, cx: &mut App) -> Self {
        match result {
            Ok(decoded) => {
                let loaded_family = if decoded.info.format.native_loadable() {
                    if cx.text_system().add_fonts(vec![Cow::Owned(decoded.bytes)]).is_ok() {
                        decoded.info.family_name.clone().map(SharedString::from)
                    } else {
                        None
                    }
                } else {
                    None
                };
                FontState::Ready {
                    info: decoded.info,
                    loaded_family,
                    scroll: ScrollHandle::new(),
                }
            }
            Err(error) => FontState::Failed(error),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, FontState::Failed(_))
    }

    pub fn render(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        match self {
            FontState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Loading font…")
                .into_any_element(),
            FontState::Failed(error) => v_flex()
                .id("file-font-error")
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .px(px(24.))
                .text_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child("No preview for this file"),
                )
                .child(
                    div()
                        .max_w(px(380.))
                        .text_xs()
                        .line_height(px(19.))
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("{error} Save it to open in another app.")),
                )
                .into_any_element(),
            FontState::Ready { info, loaded_family, scroll } => {
                let mut meta = vec![info.format.label().to_owned()];
                if let Some(sub) = &info.subfamily_name {
                    meta.push(sub.clone());
                }
                if let Some(glyphs) = info.glyph_count {
                    meta.push(format!("{} glyphs", group_thousands(u64::from(glyphs))));
                }
                if let Some(upm) = info.units_per_em {
                    meta.push(format!("{upm} UPM"));
                }
                meta.push(format!("{} bytes", group_thousands(info.byte_length)));

                let title = info
                    .full_name
                    .clone()
                    .or_else(|| info.family_name.clone())
                    .unwrap_or_else(|| "Font preview".to_owned());

                let specimen_body = v_flex()
                    .w_full()
                    .max_w(px(896.))
                    .mx_auto()
                    .p(px(28.))
                    .gap(px(20.))
                    .when_some(loaded_family.clone(), |this, family| this.font_family(family))
                    .child(
                        div()
                            .text_size(px(36.))
                            .line_height(px(44.))
                            .text_color(hsla(p.foreground))
                            .child(SharedString::from(title)),
                    )
                    .child(
                        v_flex()
                            .gap(px(4.))
                            .text_size(px(22.))
                            .line_height(px(30.))
                            .text_color(hsla(p.foreground))
                            .child(FONT_SPECIMEN_UPPER)
                            .child(FONT_SPECIMEN_LOWER)
                            .child(FONT_SPECIMEN_SYMBOLS),
                    )
                    .children(FONT_SPECIMEN_SIZES.into_iter().map(|size| {
                        v_flex()
                            .gap(px(4.))
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(11.))
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("{} px", size as u32)),
                            )
                            .child(
                                div()
                                    .text_size(px(size))
                                    .line_height(px(size * 1.35))
                                    .text_color(hsla(p.foreground))
                                    .child(FONT_SPECIMEN_PANGRAM),
                            )
                    }));

                v_flex()
                    .id("file-font-preview")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .bg(hsla(p.background))
                    .child(
                        h_flex()
                            .flex_none()
                            .px(px(12.))
                            .py(px(6.))
                            .gap(px(8.))
                            .items_center()
                            .border_b_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(SharedString::from(meta.join(" · "))),
                    )
                    .when(loaded_family.is_none(), |this| {
                        this.child(
                            div()
                                .flex_none()
                                .border_b_1()
                                .border_color(hsla(p.border).opacity(0.6))
                                .bg(hsla(p.muted).opacity(0.4))
                                .px(px(12.))
                                .py(px(5.))
                                .text_size(px(11.))
                                .text_color(hsla(p.muted_foreground))
                                .child("Web font container metadata shown; live glyph rendering requires uncompressed TTF/OTF."),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("file-font-scroll")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .track_scroll(scroll)
                                    .child(specimen_body),
                            )
                            .vertical_scrollbar(scroll),
                    )
                    .into_any_element()
            }
        }
    }
}
