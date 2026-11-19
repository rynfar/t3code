//! Virtualized CSV/TSV delimited table preview with sortable and resizable columns.
//! Ports `apps/web/src/components/files/DelimitedTablePreview.tsx` and `VirtualTablePreview.tsx`.

use gpui::{
    AnyElement, Context, DragMoveEvent, EmptyView, FontWeight, IntoElement, ParentElement, Pixels,
    ScrollWheelEvent, SharedString, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex};
use t3_core::files::delimited::{DelimitedLimits, DelimitedTable, SortDirection};
use t3_core::files::group_thousands;

use super::preview::{Key, Preview};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 28.;
const HEADER_HEIGHT: f32 = 30.;
const ROW_GUTTER_WIDTH: f32 = 56.;

#[derive(Clone, Copy)]
struct ColumnResize {
    column: usize,
    start_x: f32,
    start_width: f32,
}

pub struct TableView {
    key: Option<Key>,
    revision: String,
    pub(super) table: Option<DelimitedTable>,
    scroll: UniformListScrollHandle,
    h_offset: Pixels,
    viewport_width: f32,
}

impl TableView {
    pub fn new() -> Self {
        Self {
            key: None,
            revision: String::new(),
            table: None,
            scroll: UniformListScrollHandle::new(),
            h_offset: px(0.),
            viewport_width: 0.,
        }
    }

    pub fn sync(&mut self, key: &Key, contents: &str, delimiter: char) {
        let revision = t3_core::files::save::file_content_revision(contents);
        if self.key.as_ref() == Some(key) && self.revision == revision && self.table.is_some() {
            return;
        }
        self.key = Some(key.clone());
        self.revision = revision;
        self.table = Some(DelimitedTable::parse(contents, delimiter, DelimitedLimits::default()));
        self.h_offset = px(0.);
    }

    pub fn clear(&mut self) {
        self.key = None;
        self.revision.clear();
        self.table = None;
        self.h_offset = px(0.);
    }
}

impl Preview {
    pub(super) fn render_table(&mut self, key: &Key, contents: &str, delimiter: char, cx: &mut Context<Self>) -> AnyElement {
        self.table.sync(key, contents, delimiter);
        let p = palette(cx);
        let Some(table) = self.table.table.as_ref() else {
            return div().into_any_element();
        };

        let row_count = table.row_count();
        let col_count = table.column_count();
        let truncated = table.truncated;
        let sort = table.sort;
        let header = table.header.clone();
        let widths = table.column_widths.clone();
        let h_offset = self.table.h_offset;
        let delimiter_name = if delimiter == '\t' { "TSV" } else { "CSV" };
        let status = format!(
            "{} rows · {} columns · {}",
            group_thousands(row_count as u64),
            group_thousands(col_count as u64),
            delimiter_name
        );

        let header_cells = header.into_iter().enumerate().map(|(col, label)| {
            let width = widths.get(col).copied().unwrap_or(140.);
            let sort_icon = match sort {
                Some(active) if active.column == col && active.direction == SortDirection::Ascending => Some(Icon::ArrowUp),
                Some(active) if active.column == col && active.direction == SortDirection::Descending => Some(Icon::ArrowDown),
                _ => None,
            };
            div()
                .id(("table-col-hdr", col))
                .relative()
                .flex_none()
                .w(px(width))
                .h(px(HEADER_HEIGHT))
                .border_r_1()
                .border_color(hsla(p.border).opacity(0.5))
                .child(
                    h_flex()
                        .id(("table-col-sort", col))
                        .size_full()
                        .px(px(10.))
                        .gap(px(4.))
                        .items_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(p.accent)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(table) = this.table.table.as_mut() {
                                table.toggle_sort(col);
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child(SharedString::from(label)),
                        )
                        .when_some(sort_icon, |this, icon| {
                            this.child(icon.svg().flex_none().size(px(12.)).text_color(hsla(p.primary)))
                        }),
                )
                .child(
                    div()
                        .id(("table-col-resize", col))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(
                            ColumnResize { column: col, start_x: 0., start_width: width },
                            |drag, position, _, cx| {
                                let _ = (drag, position);
                                cx.new(|_| EmptyView)
                            },
                        )
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, _| {
                            this.table.viewport_width = f32::from(event.position.x);
                        })),
                )
        });

        v_flex()
            .id("file-delimited-table")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(hsla(p.background))
            .on_drag_move(cx.listener(Self::resize_table_column))
            .on_scroll_wheel(cx.listener(Self::scroll_table_horizontally))
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
                    .child(SharedString::from(status))
                    .when_some(sort, |this, active| {
                        let dir = match active.direction {
                            SortDirection::Ascending => "asc",
                            SortDirection::Descending => "desc",
                        };
                        let name = self
                            .table
                            .table
                            .as_ref()
                            .and_then(|t| t.header.get(active.column))
                            .cloned()
                            .unwrap_or_default();
                        this.child(
                            div()
                                .text_color(hsla(p.foreground))
                                .child(format!("· Sorted by {name} ({dir})")),
                        )
                    }),
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
                        .child("Large table preview truncated to safe row/column limits."),
                )
            })
            .child(
                h_flex()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .border_b_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.muted))
                    .text_xs()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .w(px(ROW_GUTTER_WIDTH))
                            .h_full()
                            .border_r_1()
                            .border_color(hsla(p.border))
                            .bg(hsla(p.background))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .justify_end()
                            .text_color(hsla(p.muted_foreground))
                            .child("#"),
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
                        uniform_list(
                            "delimited-table-rows",
                            row_count,
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range.map(|index| this.render_table_row(index, cx)).collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(&self.table.scroll),
                    )
                    .vertical_scrollbar(&self.table.scroll),
            )
            .into_any_element()
    }

    fn render_table_row(&self, display_index: usize, cx: &Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(table) = self.table.table.as_ref() else {
            return div().into_any_element();
        };
        let Some((row_num, cells)) = table.display_row(display_index) else {
            return div().into_any_element();
        };
        let h_offset = self.table.h_offset;
        let striped = display_index % 2 == 1;
        h_flex()
            .h(px(ROW_HEIGHT))
            .w_full()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.45))
            .when(striped, |this| this.bg(hsla(p.muted).opacity(0.5)))
            .text_xs()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .w(px(ROW_GUTTER_WIDTH))
                    .h_full()
                    .border_r_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .bg(hsla(p.background))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .text_color(hsla(p.muted_foreground))
                    .child(SharedString::from(row_num.to_string())),
            )
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    h_flex().ml(-h_offset).children(
                        table.column_widths.iter().enumerate().map(|(col, &width)| {
                            let value = cells
                                .get(col)
                                .map(|s| s.replace('\n', " ↵ "))
                                .unwrap_or_default();
                            div()
                                .flex_none()
                                .w(px(width))
                                .h(px(ROW_HEIGHT))
                                .px(px(10.))
                                .border_r_1()
                                .border_color(hsla(p.border).opacity(0.35))
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(hsla(p.foreground))
                                        .child(SharedString::from(value)),
                                )
                        }),
                    ),
                ),
            )
            .into_any_element()
    }

    fn resize_table_column(&mut self, event: &DragMoveEvent<ColumnResize>, _: &mut Window, cx: &mut Context<Self>) {
        let drag = *event.drag(cx);
        let anchor_x = if drag.start_x != 0. { drag.start_x } else { self.table.viewport_width };
        let delta = f32::from(event.event.position.x) - anchor_x;
        if let Some(table) = self.table.table.as_mut() {
            table.resize_column(drag.column, drag.start_width + delta);
            cx.notify();
        }
    }

    fn scroll_table_horizontally(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(ROW_HEIGHT));
        let dx = if event.modifiers.shift && delta.x == px(0.) { delta.y } else { delta.x };
        if !event.modifiers.shift && dx.abs() <= delta.y.abs() {
            return;
        }
        let Some(table) = self.table.table.as_ref() else { return };
        let total = table.total_width(ROW_GUTTER_WIDTH);
        let max = px((total - 320.).max(0.));
        let next = (self.table.h_offset - dx).clamp(px(0.), max);
        if next != self.table.h_offset {
            self.table.h_offset = next;
            cx.notify();
        }
    }
}
