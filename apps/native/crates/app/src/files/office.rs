//! Office & rich document preview (`OfficeViewer.tsx`, `OpenXmlOfficeViewer.tsx`,
//! `office/Spreadsheet.tsx`, `WordDocument.tsx`, `Presentation.tsx`), including interactive
//! spreadsheet column AND row drag-to-resize clamped to Excel's min/max bounds
//! (`pingdotgg/t3code#35548`).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, DragMoveEvent, EmptyView, FontWeight, IntoElement, ListAlignment,
    ListState, MouseButton, MouseDownEvent, ParentElement, Pixels, ScrollHandle, ScrollWheelEvent,
    SharedString, Styled, Window, canvas, div, list, prelude::*, px,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex};
use t3_core::files::office::{
    DEFAULT_ROW_HEIGHT, DocumentBlock, OFFICE_PREVIEW_MAX_BYTES, OfficeKind, OfficePreview,
    ResizeAxis, column_name, parse_office_preview,
};
use t3_core::files::{
    CREATE_ASSET_URL, absolute_path, decode_asset_url, workspace_asset_request,
};

use super::Root;
use super::preview::Preview;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const HEADER_HEIGHT: f32 = 26.;
const ROW_GUTTER_WIDTH: f32 = 48.;

#[derive(Clone, Copy)]
struct SheetTrackResize {
    axis: ResizeAxis,
    index: usize,
    start_size: f32,
}

pub fn load(
    state: &AppState,
    root: &Root,
    path: &str,
    mutation: Option<&str>,
) -> Option<tokio::task::JoinHandle<Result<Vec<u8>, String>>> {
    let client = state.client_handle()?;
    let request = workspace_asset_request(
        &root.workspace.thread_id,
        &root.workspace.cwd,
        &absolute_path(path, &root.workspace.cwd),
    );
    let mutation = mutation.map(str::to_owned);
    Some(state.runtime().spawn(async move {
        let value = client
            .request(CREATE_ASSET_URL, request)
            .await
            .map_err(|_| "The file could not be loaded. Close it and try again.".to_owned())?;
        let url = decode_asset_url(&value)
            .map_err(|_| "The file could not be loaded. Close it and try again.".to_owned())?;
        client
            .asset_bytes(
                &format!("{url}{}", t3_core::media::workspace_image_suffix(&url, mutation.as_deref())),
                OFFICE_PREVIEW_MAX_BYTES,
            )
            .await
    }))
}

pub fn decode(kind: OfficeKind, bytes: Vec<u8>) -> Result<OfficePreview, String> {
    parse_office_preview(kind, &bytes)
}

pub enum OfficeState {
    Loading,
    Ready(Box<OfficeReady>),
    Failed(String),
}

pub struct OfficeReady {
    pub preview: OfficePreview,
    /// Active sheet index (workbooks), chapter index (documents), or slide index (presentations).
    pub active_index: usize,
    pub h_offset: Pixels,
    pub row_list: ListState,
    pub doc_scroll: ScrollHandle,
    pub resize_anchor: f32,
    pub viewport_width: Rc<Cell<f32>>,
}

impl OfficeReady {
    pub fn new(preview: OfficePreview) -> Self {
        let row_count = match &preview {
            OfficePreview::Workbook(wb) => wb.sheets.first().map_or(0, |s| s.row_count()),
            _ => 0,
        };
        Self {
            preview,
            active_index: 0,
            h_offset: px(0.),
            row_list: ListState::new(row_count, ListAlignment::Top, px(400.)),
            doc_scroll: ScrollHandle::new(),
            resize_anchor: 0.0,
            viewport_width: Rc::new(Cell::new(0.0)),
        }
    }

    pub fn select_index(&mut self, index: usize) {
        let count = match &self.preview {
            OfficePreview::Workbook(wb) => wb.sheets.len(),
            OfficePreview::Document(doc) => doc.chapters.len(),
            OfficePreview::Presentation(pres) => pres.slides.len(),
        };
        if index >= count || index == self.active_index {
            return;
        }
        self.active_index = index;
        self.h_offset = px(0.);
        if let OfficePreview::Workbook(wb) = &self.preview {
            let rows = wb.sheets.get(index).map_or(0, |s| s.row_count());
            self.row_list.reset(rows);
        }
    }
}

impl OfficeState {
    pub fn from_decoded(result: Result<OfficePreview, String>) -> Self {
        match result {
            Ok(preview) => Self::Ready(Box::new(OfficeReady::new(preview))),
            Err(error) => Self::Failed(error),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

impl Preview {
    pub(super) fn render_office(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        match self.office_state() {
            None | Some(OfficeState::Loading) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Loading document…")
                .into_any_element(),
            Some(OfficeState::Failed(error)) => {
                let msg = format!("{error} Save it or open it in another app.");
                v_flex()
                    .id("file-office-error")
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .px(px(24.))
                    .text_center()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("No preview for this file"))
                    .child(div().max_w(px(384.)).text_xs().text_color(hsla(p.muted_foreground)).child(msg))
                    .into_any_element()
            }
            Some(OfficeState::Ready(ready)) => {
                let details = ready.preview.details_label();
                let truncated = ready.preview.truncated();
                let body = match &ready.preview {
                    OfficePreview::Workbook(_) => self.render_office_workbook(cx),
                    OfficePreview::Document(_) => self.render_office_document(cx),
                    OfficePreview::Presentation(_) => self.render_office_presentation(cx),
                };
                v_flex()
                    .id("file-office-viewer")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .bg(hsla(p.background))
                    .child(
                        h_flex()
                            .flex_none()
                            .h(px(32.))
                            .px(px(12.))
                            .items_center()
                            .border_b_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(SharedString::from(details)),
                    )
                    .when(truncated, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .border_b_1()
                                .border_color(hsla(p.warning).opacity(0.2))
                                .bg(hsla(p.warning_surface))
                                .px(px(12.))
                                .py(px(4.))
                                .text_size(px(11.))
                                .text_color(hsla(p.warning_foreground))
                                .child("Preview limited. Save it or open it in another app to read the full document."),
                        )
                    })
                    .child(body)
                    .into_any_element()
            }
        }
    }

    fn render_office_workbook(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(OfficeState::Ready(ready)) = self.office_state() else {
            return div().into_any_element();
        };
        let OfficePreview::Workbook(wb) = &ready.preview else {
            return div().into_any_element();
        };
        let active_idx = ready.active_index.min(wb.sheets.len().saturating_sub(1));
        let Some(sheet) = wb.sheets.get(active_idx) else {
            return div().into_any_element();
        };
        let h_offset = ready.h_offset;
        let show_gridlines = sheet.show_gridlines;
        let row_list = ready.row_list.clone();
        let viewport_width = ready.viewport_width.clone();

        let header_cells = (0..sheet.column_count).map(|col| {
            let width = sheet.col_width(col);
            let label = column_name(col);
            div()
                .id(("office-col-hdr", col))
                .relative()
                .flex_none()
                .w(px(width))
                .h(px(HEADER_HEIGHT))
                .border_r_1()
                .border_color(hsla(p.border).opacity(0.6))
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .child(SharedString::from(label))
                .child(
                    div()
                        .id(("office-col-resize", col))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(
                            SheetTrackResize { axis: ResizeAxis::Column, index: col, start_size: width },
                            |_, _, _, cx| cx.new(|_| EmptyView),
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, _| {
                            if let Some(ready) = this.office_ready_mut() {
                                ready.resize_anchor = f32::from(event.position.x);
                            }
                        })),
                )
        });

        let sheet_tabs = wb.sheets.iter().enumerate().map(|(index, tab)| {
            let selected = index == active_idx;
            div()
                .id(("office-sheet-tab", index))
                .flex_none()
                .h(px(28.))
                .px(px(10.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .text_xs()
                .cursor_pointer()
                .when(selected, |this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground)).font_weight(FontWeight::MEDIUM))
                .when(!selected, |this| this.text_color(hsla(p.muted_foreground)).hover(|s| s.bg(hsla(p.accent))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ready) = this.office_ready_mut() {
                        ready.select_index(index);
                        cx.notify();
                    }
                }))
                .child(SharedString::from(tab.name.clone()))
        });

        v_flex()
            .id("office-spreadsheet")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_drag_move(cx.listener(Self::resize_office_track))
            .on_scroll_wheel(cx.listener(Self::scroll_office_horizontally))
            .child(
                h_flex()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.muted))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .w(px(ROW_GUTTER_WIDTH))
                            .h_full()
                            .border_r_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.background)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(h_flex().ml(-h_offset).children(header_cells)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        list(
                            row_list.clone(),
                            cx.processor(move |this, row_index: usize, _, cx| {
                                this.render_office_sheet_row(row_index, show_gridlines, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .vertical_scrollbar(&row_list)
                    .child(
                        canvas(move |bounds, _, _| viewport_width.set(f32::from(bounds.size.width)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    ),
            )
            .child(
                h_flex()
                    .id("office-sheet-tabs")
                    .flex_none()
                    .h(px(36.))
                    .px(px(8.))
                    .gap(px(4.))
                    .items_center()
                    .overflow_x_scroll()
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.background))
                    .children(sheet_tabs),
            )
            .into_any_element()
    }

    fn render_office_sheet_row(&self, row_index: usize, show_gridlines: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(OfficeState::Ready(ready)) = self.office_state() else {
            return div().into_any_element();
        };
        let OfficePreview::Workbook(wb) = &ready.preview else {
            return div().into_any_element();
        };
        let Some(sheet) = wb.sheets.get(ready.active_index) else {
            return div().into_any_element();
        };
        let Some(cells) = sheet.rows.get(row_index) else {
            return div().into_any_element();
        };
        let height = sheet.row_height(row_index);
        let h_offset = ready.h_offset;
        let border_opacity = if show_gridlines { 0.45 } else { 0.0 };

        h_flex()
            .h(px(height))
            .w_full()
            .border_b_1()
            .border_color(hsla(p.border).opacity(border_opacity))
            .text_xs()
            .overflow_hidden()
            .child(
                div()
                    .id(("office-row-hdr", row_index))
                    .relative()
                    .flex_none()
                    .w(px(ROW_GUTTER_WIDTH))
                    .h_full()
                    .border_r_1()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.muted).opacity(0.6))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .text_color(hsla(p.muted_foreground))
                    .child(SharedString::from((row_index + 1).to_string()))
                    .child(
                        div()
                            .id(("office-row-resize", row_index))
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-3.))
                            .h(px(6.))
                            .cursor_row_resize()
                            .on_drag(
                                SheetTrackResize { axis: ResizeAxis::Row, index: row_index, start_size: height },
                                |_, _, _, cx| cx.new(|_| EmptyView),
                            )
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, _| {
                                if let Some(ready) = this.office_ready_mut() {
                                    ready.resize_anchor = f32::from(event.position.y);
                                }
                            })),
                    ),
            )
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    h_flex().ml(-h_offset).children((0..sheet.column_count).map(|col| {
                        let width = sheet.col_width(col);
                        let value = cells.get(col).map(|s| s.replace('\n', " ↵ ")).unwrap_or_default();
                        div()
                            .flex_none()
                            .w(px(width))
                            .h(px(height))
                            .px(px(8.))
                            .border_r_1()
                            .border_color(hsla(p.border).opacity(border_opacity))
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .child(div().min_w_0().truncate().text_color(hsla(p.foreground)).child(SharedString::from(value)))
                    })),
                ),
            )
            .into_any_element()
    }

    fn resize_office_track(&mut self, event: &DragMoveEvent<SheetTrackResize>, _: &mut Window, cx: &mut Context<Self>) {
        let drag = *event.drag(cx);
        let Some(ready) = self.office_ready_mut() else { return };
        let current_pos = match drag.axis {
            ResizeAxis::Column => f32::from(event.event.position.x),
            ResizeAxis::Row => f32::from(event.event.position.y),
        };
        let delta = current_pos - ready.resize_anchor;
        let target_size = drag.start_size + delta;
        let OfficePreview::Workbook(wb) = &mut ready.preview else { return };
        let Some(sheet) = wb.sheets.get_mut(ready.active_index) else { return };
        let updated = match drag.axis {
            ResizeAxis::Column => sheet.set_col_width(drag.index, target_size).is_ok(),
            ResizeAxis::Row => {
                let ok = sheet.set_row_height(drag.index, target_size).is_ok();
                if ok {
                    ready.row_list.remeasure();
                }
                ok
            }
        };
        if updated {
            cx.notify();
        }
    }

    fn scroll_office_horizontally(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(DEFAULT_ROW_HEIGHT));
        let dx = if event.modifiers.shift && delta.x == px(0.) { delta.y } else { delta.x };
        if !event.modifiers.shift && dx.abs() <= delta.y.abs() {
            return;
        }
        let Some(ready) = self.office_ready_mut() else { return };
        let OfficePreview::Workbook(wb) = &ready.preview else { return };
        let Some(sheet) = wb.sheets.get(ready.active_index) else { return };
        let total = sheet.total_width(ROW_GUTTER_WIDTH);
        let visible = ready.viewport_width.get().max(120.);
        let max = px((total - visible).max(0.));
        let next = (ready.h_offset - dx).clamp(px(0.), max);
        if next != ready.h_offset {
            ready.h_offset = next;
            cx.notify();
        }
    }

    fn render_office_document(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(OfficeState::Ready(ready)) = self.office_state() else {
            return div().into_any_element();
        };
        let OfficePreview::Document(doc) = &ready.preview else {
            return div().into_any_element();
        };
        let chapter_count = doc.chapters.len();
        let active_idx = ready.active_index.min(chapter_count.saturating_sub(1));
        let Some(chapter) = doc.chapters.get(active_idx) else {
            return div().into_any_element();
        };
        let scroll = ready.doc_scroll.clone();
        let blocks: Vec<AnyElement> = chapter
            .blocks
            .iter()
            .map(|block| match block {
                DocumentBlock::Heading { level, text } => {
                    let size = match level {
                        1 => px(20.),
                        2 => px(17.),
                        3 => px(15.),
                        _ => px(14.),
                    };
                    div()
                        .pt(px(8.))
                        .text_size(size)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hsla(p.foreground))
                        .child(SharedString::from(text.clone()))
                        .into_any_element()
                }
                DocumentBlock::Paragraph(text) => div()
                    .text_size(px(13.))
                    .line_height(px(21.))
                    .text_color(hsla(p.foreground).opacity(0.92))
                    .child(SharedString::from(text.clone()))
                    .into_any_element(),
                DocumentBlock::Table(rows) => v_flex()
                    .my(px(6.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .overflow_hidden()
                    .children(rows.iter().enumerate().map(|(r_idx, row)| {
                        h_flex()
                            .when(r_idx > 0, |this| this.border_t_1().border_color(hsla(p.border).opacity(0.6)))
                            .when(r_idx == 0, |this| this.bg(hsla(p.muted).opacity(0.5)).font_weight(FontWeight::MEDIUM))
                            .children(row.iter().enumerate().map(|(c_idx, cell)| {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .px(px(8.))
                                    .py(px(5.))
                                    .when(c_idx > 0, |this| this.border_l_1().border_color(hsla(p.border).opacity(0.4)))
                                    .text_xs()
                                    .text_color(hsla(p.foreground))
                                    .child(SharedString::from(cell.clone()))
                            }))
                    }))
                    .into_any_element(),
            })
            .collect();

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .when(chapter_count > 1, |this| {
                let title = chapter
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Chapter {}", active_idx + 1));
                this.child(
                    h_flex()
                        .flex_none()
                        .h(px(36.))
                        .px(px(12.))
                        .gap(px(8.))
                        .items_center()
                        .border_b_1()
                        .border_color(hsla(p.border).opacity(0.6))
                        .text_xs()
                        .child(
                            Button::new("office-prev-chapter")
                                .variant(Variant::Ghost)
                                .size(Size::IconXs)
                                .icon(Icon::ChevronLeft)
                                .tooltip("Previous chapter")
                                .disabled(active_idx == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(ready) = this.office_ready_mut() {
                                        ready.select_index(active_idx.saturating_sub(1));
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(SharedString::from(title)))
                        .child(div().text_color(hsla(p.muted_foreground)).child(format!("Chapter {} of {chapter_count}", active_idx + 1)))
                        .child(
                            Button::new("office-next-chapter")
                                .variant(Variant::Ghost)
                                .size(Size::IconXs)
                                .icon(Icon::ChevronRight)
                                .tooltip("Next chapter")
                                .disabled(active_idx + 1 >= chapter_count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(ready) = this.office_ready_mut() {
                                        ready.select_index((active_idx + 1).min(chapter_count.saturating_sub(1)));
                                        cx.notify();
                                    }
                                })),
                        ),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .bg(hsla(p.muted).opacity(0.35))
                    .child(
                        div()
                            .id("office-doc-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .px(px(24.))
                            .py(px(20.))
                            .child(
                                v_flex()
                                    .mx_auto()
                                    .w_full()
                                    .max_w(px(760.))
                                    .gap(px(10.))
                                    .p(px(28.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(hsla(p.border).opacity(0.7))
                                    .bg(hsla(p.background))
                                    .children(blocks),
                            ),
                    )
                    .vertical_scrollbar(&scroll),
            )
            .into_any_element()
    }

    fn render_office_presentation(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(OfficeState::Ready(ready)) = self.office_state() else {
            return div().into_any_element();
        };
        let OfficePreview::Presentation(pres) = &ready.preview else {
            return div().into_any_element();
        };
        let slide_count = pres.slides.len();
        let active_idx = ready.active_index.min(slide_count.saturating_sub(1));
        let Some(slide) = pres.slides.get(active_idx) else {
            return div().into_any_element();
        };
        let scroll = ready.doc_scroll.clone();

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .flex_none()
                    .h(px(36.))
                    .px(px(12.))
                    .gap(px(8.))
                    .items_center()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .text_xs()
                    .child(
                        Button::new("office-prev-slide")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::ChevronLeft)
                            .tooltip("Previous slide")
                            .disabled(active_idx == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ready) = this.office_ready_mut() {
                                    ready.select_index(active_idx.saturating_sub(1));
                                    cx.notify();
                                }
                            })),
                    )
                    .child(div().text_color(hsla(p.foreground)).child(format!("Slide {} of {slide_count}", active_idx + 1)))
                    .child(
                        Button::new("office-next-slide")
                            .variant(Variant::Ghost)
                            .size(Size::IconXs)
                            .icon(Icon::ChevronRight)
                            .tooltip("Next slide")
                            .disabled(active_idx + 1 >= slide_count)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ready) = this.office_ready_mut() {
                                    ready.select_index((active_idx + 1).min(slide_count.saturating_sub(1)));
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .bg(hsla(p.muted).opacity(0.35))
                    .child(
                        div()
                            .id("office-slide-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .p(px(24.))
                            .child(
                                v_flex()
                                    .mx_auto()
                                    .w_full()
                                    .max_w(px(820.))
                                    .min_h(px(420.))
                                    .gap(px(14.))
                                    .p(px(36.))
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(hsla(p.border).opacity(0.75))
                                    .bg(hsla(p.background))
                                    .shadow_sm()
                                    .when_some(slide.title.clone(), |this, title| {
                                        this.child(
                                            div()
                                                .text_size(px(22.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(hsla(p.foreground))
                                                .child(SharedString::from(title)),
                                        )
                                    })
                                    .children(slide.paragraphs.iter().map(|line| {
                                        h_flex()
                                            .items_start()
                                            .gap(px(8.))
                                            .child(div().mt(px(7.)).size(px(5.)).flex_none().rounded_full().bg(hsla(p.primary)))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .line_height(px(22.))
                                                    .text_color(hsla(p.foreground).opacity(0.9))
                                                    .child(SharedString::from(line.clone())),
                                            )
                                    })),
                            ),
                    )
                    .vertical_scrollbar(&scroll),
            )
            .into_any_element()
    }
}
