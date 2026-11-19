//! Jupyter Notebook (`.ipynb`) preview: inert markdown cells, syntax-highlighted code cells,
//! saved outputs (text, HTML summary, base64 raster images), and incremental 20-cell pagination.
//! Ports `apps/web/src/components/files/NotebookPreview.tsx`.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, HighlightStyle, IntoElement, ObjectFit,
    ParentElement, RenderImage, ScrollHandle, SharedString, Styled, StyledImage, StyledText, div, img,
    prelude::*, px,
};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextViewState;
use gpui_component::{ActiveTheme, Rope, v_flex};
use t3_core::files::group_thousands;
use t3_core::files::notebook::{
    NOTEBOOK_CELL_PAGE_STEP, NOTEBOOK_INITIAL_VISIBLE_CELLS, NotebookCellKind, NotebookDocument,
    NotebookOutput, parse_notebook, sanitize_notebook_markdown,
};

use super::image::{self, Decoded};
use super::preview::{Key, Preview};
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

pub struct DecodedNotebook {
    pub doc: NotebookDocument,
    /// Pre-decoded raster output images keyed by `(cell_index, output_index)`.
    pub images: HashMap<(usize, usize), Arc<RenderImage>>,
}

pub struct NotebookView {
    key: Option<Key>,
    revision: String,
    decoded: Option<Result<DecodedNotebook, String>>,
    visible: usize,
    scroll: ScrollHandle,
    markdown_cells: HashMap<usize, Entity<TextViewState>>,
}

impl NotebookView {
    pub fn new() -> Self {
        Self {
            key: None,
            revision: String::new(),
            decoded: None,
            visible: NOTEBOOK_INITIAL_VISIBLE_CELLS,
            scroll: ScrollHandle::new(),
            markdown_cells: HashMap::new(),
        }
    }

    pub fn release_images(&mut self, cx: &mut App) {
        if let Some(Ok(decoded)) = self.decoded.take() {
            for (_, image) in decoded.images {
                cx.drop_image(image, None);
            }
        }
        self.markdown_cells.clear();
        self.key = None;
        self.revision.clear();
    }

    pub fn sync(&mut self, key: &Key, contents: &str, cx: &mut App) {
        let revision = t3_core::files::save::file_content_revision(contents);
        if self.key.as_ref() == Some(key) && self.revision == revision && self.decoded.is_some() {
            return;
        }
        self.release_images(cx);
        self.key = Some(key.clone());
        self.revision = revision;
        self.visible = NOTEBOOK_INITIAL_VISIBLE_CELLS;
        let parsed = parse_notebook(contents.as_bytes()).map(|doc| {
            let mut images = HashMap::new();
            for (ci, cell) in doc.cells.iter().enumerate() {
                for (oi, output) in cell.outputs.iter().enumerate() {
                    if let NotebookOutput::Image { bytes, .. } = output
                        && let Ok(Decoded::Raster { image, .. }) = image::decode(bytes.clone(), false)
                    {
                        images.insert((ci, oi), image);
                    }
                }
            }
            DecodedNotebook { doc, images }
        });
        self.decoded = Some(parsed);
    }
}

impl Preview {
    pub(super) fn render_notebook(&mut self, key: &Key, contents: &str, cx: &mut Context<Self>) -> AnyElement {
        self.notebook.sync(key, contents, cx);
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let mono_size = cx.theme().mono_font_size;
        let highlight_theme = cx.theme().highlight_theme.clone();

        let (language, syntax_lang, total_cells, byte_length, visible) = match &self.notebook.decoded {
            Some(Ok(decoded)) => (
                decoded.doc.language.clone(),
                decoded.doc.syntax_language(),
                decoded.doc.cells.len(),
                decoded.doc.byte_length,
                self.notebook.visible,
            ),
            Some(Err(err)) => {
                return v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .px(px(24.))
                    .text_center()
                    .text_xs()
                    .text_color(hsla(p.destructive))
                    .child(err.clone())
                    .into_any_element();
            }
            None => return div().into_any_element(),
        };

        let shown_count = visible.min(total_cells);
        // Ensure inert markdown states exist for visible markdown cells.
        for idx in 0..shown_count {
            let needs = !self.notebook.markdown_cells.contains_key(&idx);
            if needs
                && let Some(Ok(decoded)) = &self.notebook.decoded
                && let Some(cell) = decoded.doc.cells.get(idx)
                && cell.kind == NotebookCellKind::Markdown
            {
                let safe = sanitize_notebook_markdown(&cell.source);
                let state = cx.new(|cx| TextViewState::markdown(&safe, cx).selectable(true));
                self.notebook.markdown_cells.insert(idx, state);
            }
        }

        let Some(Ok(decoded)) = &self.notebook.decoded else {
            return div().into_any_element();
        };

        let mut cell_elements: Vec<AnyElement> = Vec::with_capacity(shown_count);
        for (index, cell) in decoded.doc.cells.iter().take(shown_count).enumerate() {
            let header_label = match (cell.kind, cell.execution_count) {
                (NotebookCellKind::Code, Some(count)) => format!("{language} [{count}] · {}", index + 1),
                (NotebookCellKind::Code, None) => format!("{language} · {}", index + 1),
                (kind, _) => format!("{} · {}", kind.label(), index + 1),
            };

            let source_el: Option<AnyElement> = match cell.kind {
                NotebookCellKind::Markdown => self.notebook.markdown_cells.get(&index).map(|state| {
                    div()
                        .px(px(16.))
                        .py(px(12.))
                        .text_size(px(13.))
                        .line_height(px(21.))
                        .text_color(hsla(p.foreground).opacity(0.92))
                        .child(crate::transcript::markdown::markdown_view(state, cx))
                        .into_any_element()
                }),
                NotebookCellKind::Code | NotebookCellKind::Raw => {
                    if cell.source.trim().is_empty() {
                        None
                    } else {
                        let runs: Vec<(std::ops::Range<usize>, HighlightStyle)> =
                            if cell.kind == NotebookCellKind::Code
                                && let Some(lang) = syntax_lang
                                && cell.source.len() <= 32_768
                            {
                                let mut highlighter = SyntaxHighlighter::new(lang);
                                highlighter.update(None, &Rope::from_str(&cell.source), None);
                                highlighter
                                    .styles(&(0..cell.source.len()), &*highlight_theme)
                                    .into_iter()
                                    .filter(|(_, style)| *style != HighlightStyle::default())
                                    .collect()
                            } else {
                                Vec::new()
                            };
                        let styled = StyledText::new(SharedString::from(cell.source.clone())).with_highlights(runs);
                        Some(
                            div()
                                .bg(hsla(p.code_background))
                                .px(px(14.))
                                .py(px(10.))
                                .font_family(mono.clone())
                                .text_size(mono_size)
                                .line_height(px(19.))
                                .text_color(hsla(p.code_foreground))
                                .child(styled)
                                .into_any_element(),
                        )
                    }
                }
            };

            let mut output_els: Vec<AnyElement> = Vec::with_capacity(cell.outputs.len());
            for (oi, output) in cell.outputs.iter().enumerate() {
                let body = match output {
                    NotebookOutput::Image { .. } => {
                        if let Some(rendered) = decoded.images.get(&(index, oi)) {
                            div()
                                .p(px(12.))
                                .child(
                                    img(rendered.clone())
                                        .object_fit(ObjectFit::Contain)
                                        .max_h(px(384.))
                                        .max_w_full(),
                                )
                                .into_any_element()
                        } else {
                            div()
                                .p(px(12.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("[Embedded image could not be decoded]")
                                .into_any_element()
                        }
                    }
                    NotebookOutput::Html(summary) => v_flex()
                        .p(px(12.))
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.muted_foreground))
                                .child("HTML output (sanitized text)"),
                        )
                        .child(
                            div()
                                .font_family(mono.clone())
                                .text_xs()
                                .line_height(px(18.))
                                .text_color(hsla(p.foreground).opacity(0.85))
                                .child(SharedString::from(summary.clone())),
                        )
                        .into_any_element(),
                    NotebookOutput::Text(text) => div()
                        .p(px(12.))
                        .font_family(mono.clone())
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(hsla(p.foreground).opacity(0.9))
                        .child(SharedString::from(text.clone()))
                        .into_any_element(),
                };
                output_els.push(
                    div()
                        .border_t_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .child(body)
                        .into_any_element(),
                );
            }

            cell_elements.push(
                v_flex()
                    .id(("nb-cell", index))
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.65))
                    .bg(hsla(p.card))
                    .child(
                        div()
                            .border_b_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .bg(hsla(p.muted).opacity(0.45))
                            .px(px(12.))
                            .py(px(6.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(SharedString::from(header_label)),
                    )
                    .children(source_el)
                    .children(output_els)
                    .into_any_element(),
            );
        }

        let remaining = total_cells.saturating_sub(shown_count);
        let entity = cx.entity().downgrade();

        v_flex()
            .id("file-notebook-preview")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(hsla(p.background))
            .child(
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .px(px(12.))
                    .py(px(6.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(format!(
                        "{} cells · {language} · {} bytes",
                        group_thousands(total_cells as u64),
                        group_thousands(byte_length)
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.muted).opacity(0.35))
                    .px(px(12.))
                    .py(px(5.))
                    .text_size(px(11.))
                    .text_color(hsla(p.muted_foreground))
                    .child("Saved outputs only. Code is never executed."),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("file-notebook-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.notebook.scroll)
                            .child(
                                v_flex()
                                    .mx_auto()
                                    .w_full()
                                    .max_w(px(896.))
                                    .px(px(20.))
                                    .py(px(20.))
                                    .gap(px(16.))
                                    .when(total_cells == 0, |this| {
                                        this.child(
                                            div()
                                                .text_sm()
                                                .text_color(hsla(p.muted_foreground))
                                                .child("This notebook is empty."),
                                        )
                                    })
                                    .children(cell_elements)
                                    .when(remaining > 0, |this| {
                                        this.child(
                                            Button::new("notebook-show-more")
                                                .variant(Variant::Secondary)
                                                .size(Size::Xs)
                                                .label(format!(
                                                    "Show more cells ({remaining} remaining)"
                                                ))
                                                .on_click(move |_, _, cx| {
                                                    if let Some(entity) = entity.upgrade() {
                                                        entity.update(cx, |this, cx| {
                                                            this.notebook.visible += NOTEBOOK_CELL_PAGE_STEP;
                                                            cx.notify();
                                                        });
                                                    }
                                                }),
                                        )
                                    }),
                            ),
                    )
                    .vertical_scrollbar(&self.notebook.scroll),
            )
            .into_any_element()
    }
}
