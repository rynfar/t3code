//! Standalone Mermaid file preview (`.mmd` / `.mermaid`) with directive security guard,
//! theme-aware SVG rendering, and click-to-expand into the media lightbox.
//! Ports `apps/web/src/components/files/MermaidFilePreview.tsx` and `MermaidViewer.tsx`.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, IntoElement, ObjectFit, ParentElement, RenderImage, ScrollHandle,
    Styled, StyledImage, div, img, prelude::*, px,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::files::mermaid::{MermaidFileOutcome, render_mermaid_file, svg_data_url};

use super::preview::{Key, Preview};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

enum CachedMermaid {
    Rendered {
        svg: String,
        image: Arc<RenderImage>,
        width: f32,
        height: f32,
    },
    Unsafe(&'static str),
    Error(String),
}

pub struct MermaidView {
    key: Option<Key>,
    revision: String,
    dark: bool,
    cached: Option<CachedMermaid>,
    scroll: ScrollHandle,
}

impl MermaidView {
    pub fn new() -> Self {
        Self {
            key: None,
            revision: String::new(),
            dark: false,
            cached: None,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn release(&mut self, cx: &mut App) {
        if let Some(CachedMermaid::Rendered { image, .. }) = self.cached.take() {
            cx.drop_image(image, None);
        }
        self.key = None;
        self.revision.clear();
    }

    fn ensure(&mut self, key: &Key, contents: &str, is_dark: bool, cx: &mut App) {
        let revision = t3_core::files::save::file_content_revision(contents);
        if self.key.as_ref() == Some(key) && self.revision == revision && self.dark == is_dark && self.cached.is_some() {
            return;
        }
        self.release(cx);
        self.key = Some(key.clone());
        self.revision = revision;
        self.dark = is_dark;
        let svg_renderer = cx.svg_renderer();
        self.cached = Some(match render_mermaid_file(contents, is_dark) {
            MermaidFileOutcome::UnsafeDirective(notice) => CachedMermaid::Unsafe(notice),
            MermaidFileOutcome::RenderError(err) => CachedMermaid::Error(err),
            MermaidFileOutcome::Rendered(rendered) => {
                match svg_renderer.render_single_frame(rendered.svg.as_bytes(), 2.0) {
                    Ok(image) => CachedMermaid::Rendered {
                        svg: rendered.svg,
                        image,
                        width: rendered.width,
                        height: rendered.height,
                    },
                    Err(err) => CachedMermaid::Error(format!("SVG render failed: {err}")),
                }
            }
        });
    }
}

impl Preview {
    pub(super) fn render_mermaid(&mut self, key: &Key, contents: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let is_dark = cx.theme().mode.is_dark();
        self.mermaid.ensure(key, contents, is_dark, cx);

        match &self.mermaid.cached {
            Some(CachedMermaid::Unsafe(notice)) => v_flex()
                .id("file-mermaid-unsafe")
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(24.))
                .text_center()
                .child(Icon::ShieldOff.render(px(22.), hsla(p.warning_foreground)))
                .child(
                    div()
                        .max_w(px(460.))
                        .text_xs()
                        .line_height(px(19.))
                        .text_color(hsla(p.muted_foreground))
                        .child(*notice),
                )
                .into_any_element(),
            Some(CachedMermaid::Error(err)) => v_flex()
                .id("file-mermaid-error")
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(24.))
                .text_center()
                .child(Icon::TriangleAlert.render(px(20.), hsla(p.muted_foreground)))
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("Preview unavailable: {err}. Show Mermaid source to inspect the file.")),
                )
                .into_any_element(),
            Some(CachedMermaid::Rendered { svg, image, width, height }) => {
                let image = image.clone();
                let svg = svg.clone();
                let (w, h) = (*width, *height);
                let viewer = self.viewer.downgrade();
                v_flex()
                    .id("file-mermaid-preview")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .bg(hsla(p.background))
                    .child(
                        h_flex()
                            .flex_none()
                            .h(px(36.))
                            .px(px(12.))
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!("Mermaid diagram · {} × {}", w.round() as u32, h.round() as u32)),
                            )
                            .child(
                                Button::new("file-mermaid-expand")
                                    .variant(Variant::Ghost)
                                    .size(Size::IconXs)
                                    .icon(Icon::Maximize2)
                                    .tooltip("Expand diagram")
                                    .on_click(move |_, window, cx| {
                                        let data_url = svg_data_url(&svg);
                                        let _ = viewer.update(cx, |viewer, cx| {
                                            viewer.open_with_scope(
                                                vec![crate::media::ViewerItem::image(
                                                    t3_core::media::MediaSource::Direct(data_url),
                                                    "Mermaid diagram",
                                                )],
                                                0,
                                                None,
                                                window,
                                                cx,
                                            );
                                        });
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("file-mermaid-scroll")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.mermaid.scroll)
                                    .p(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        img(image)
                                            .object_fit(ObjectFit::Contain)
                                            .max_w(px(w.max(320.)))
                                            .max_h(px(h.max(240.)))
                                            .w_full(),
                                    ),
                            )
                            .vertical_scrollbar(&self.mermaid.scroll),
                    )
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}
