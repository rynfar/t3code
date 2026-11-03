//! The virtualized code view: one list row per visual line, Pierre's stacked and split layouts
//! with the T3 surface overrides (`lib/diffRendering.ts` `DIFF_SURFACE_THEME_UNSAFE_CSS`,
//! `components/diffs/StyledDiffCodeView.tsx`): 13px mono on 20px lines, 32px file headers that
//! stick to the top, 24px hunk separators, and a 4px change bar in the gutter.

use gpui::{
    AnyElement, App, Context, Div, FontWeight, Hsla, IntoElement, ParentElement, Pixels, ScrollWheelEvent, SharedString,
    Styled, Window, div, list, prelude::*, px,
};
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex};
use t3_core::diff::{ChangeKind, DiffLine, DiffRow, FileDiff, LineKind, RecordedContent, compact_count, separator_label};

use super::DiffPanel;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{Palette, hsla, palette};

pub const LINE_HEIGHT: f32 = 20.;
const CODE_FONT_SIZE: f32 = 13.;
const HEADER_HEIGHT: f32 = 32.;
const SEPARATOR_HEIGHT: f32 = 24.;
const CODE_PADDING: f32 = 8.;

struct LineColors {
    row: Hsla,
    gutter: Hsla,
    number: Hsla,
    bar: Option<Hsla>,
}

fn line_colors(kind: LineKind, p: &Palette) -> LineColors {
    match kind {
        LineKind::Addition => LineColors {
            row: hsla(p.diff_row_addition),
            gutter: hsla(p.diff_row_addition_gutter),
            number: hsla(p.diff_addition_foreground),
            bar: Some(hsla(p.diff_addition)),
        },
        LineKind::Deletion => LineColors {
            row: hsla(p.diff_row_deletion),
            gutter: hsla(p.diff_row_deletion_gutter),
            number: hsla(p.diff_deletion_foreground),
            bar: Some(hsla(p.diff_deletion)),
        },
        LineKind::Context => LineColors {
            row: hsla(p.background),
            gutter: hsla(p.background),
            number: hsla(p.code_foreground).opacity(0.65),
            bar: None,
        },
    }
}

/// Pierre's number column: at least 3ch of digits, 2ch before and 1ch after.
fn gutter_width(digits: usize, char_width: Pixels) -> Pixels {
    char_width * (digits.max(3) + 3) as f32
}

impl DiffPanel {
    pub(super) fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        if self.char_width.is_none() {
            let family = cx.theme().mono_font_family.clone();
            let font_id = window.text_system().resolve_font(&gpui::font(family));
            let advance = window.text_system().advance(font_id, px(CODE_FONT_SIZE), 'm').map(|size| size.width);
            self.char_width = Some(advance.unwrap_or(px(7.8)));
        }
        let top = self.list.logical_scroll_top().item_ix;
        self.top_file = self.view.as_ref().and_then(|view| view.row(top)).map(|row| row.file());
        let sticky = self.top_file;
        div()
            .id("diff-list")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(hsla(p.background))
            .on_scroll_wheel(cx.listener(Self::scroll_horizontally))
            .on_mouse_move(cx.listener(Self::drag_comment_selection))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, event: &gpui::MouseUpEvent, window, cx| {
                this.track_comment_pointer(event.position, cx);
                this.finish_selection(window, cx);
            }))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| this.finish_selection(window, cx)))
            .child(list(self.list.clone(), cx.processor(|this, ix: usize, _, cx| this.render_row(ix, cx))).size_full())
            .children(sticky.map(|file| div().absolute().top_0().left_0().right_0().child(self.render_file_header(file, cx))))
            .vertical_scrollbar(&self.list)
            .into_any_element()
    }

    /// Without wrap, long lines scroll sideways together; the gutter stays put.
    fn scroll_horizontally(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.wrap {
            return;
        }
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let dx = if event.modifiers.shift && delta.x == px(0.) { delta.y } else { delta.x };
        if !event.modifiers.shift && dx.abs() <= delta.y.abs() {
            return;
        }
        let (Some(view), Some(char_width)) = (self.view.as_ref(), self.char_width) else { return };
        let columns = if view.layout == t3_core::diff::Layout::Split { 2. } else { 1. };
        let digits = view.digits.iter().copied().max().unwrap_or(3);
        let visible = self.list.viewport_bounds().size.width / columns - gutter_width(digits, char_width) - px(CODE_PADDING * 2.);
        let content = char_width * view.parsed.max_line_len as f32;
        let max = (content - visible).max(px(0.));
        let next = (self.h_offset - dx).clamp(px(0.), max);
        if next != self.h_offset {
            self.h_offset = next;
            cx.notify();
        }
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = self.view.as_ref().and_then(|view| view.row(ix));
        let code = self.render_code_row(ix, cx);
        self.with_comment_editor(row, code, cx)
    }

    fn render_code_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.view.as_ref().and_then(|view| view.row(ix)) else {
            return div().into_any_element();
        };
        match row {
            DiffRow::FileHeader { file } => self.render_file_header(file, cx),
            DiffRow::Separator { file, hidden, .. } => self.render_separator(file, hidden, cx),
            DiffRow::Line { file, hunk, line } => {
                let Some((line, digits)) = self.line(file, hunk, line).map(|(line, digits)| (line.clone(), digits)) else { return div().into_any_element() };
                let p = palette(cx);
                self.code_row(line_colors(line.kind, p).row, cx).child(self.half(file, Some(&line), digits, true, cx)).into_any_element()
            }
            DiffRow::Split { file, hunk, left, right } => {
                let left = left.and_then(|line| self.line(file, hunk, line)).map(|(line, digits)| (line.clone(), digits));
                let right = right.and_then(|line| self.line(file, hunk, line)).map(|(line, digits)| (line.clone(), digits));
                let digits = left.as_ref().or(right.as_ref()).map_or(3, |(_, digits)| *digits);
                let p = palette(cx);
                self.code_row(hsla(p.background), cx)
                    .child(self.half(file, left.as_ref().map(|(line, _)| line), digits, false, cx))
                    .child(self.half(file, right.as_ref().map(|(line, _)| line), digits, true, cx))
                    .into_any_element()
            }
            DiffRow::NoNewline { file, hunk, line } => {
                let Some((line, digits)) = self.line(file, hunk, line) else { return div().into_any_element() };
                let p = palette(cx);
                self.code_row(line_colors(line.kind, p).row, cx).child(self.no_newline(Some(line), digits, cx)).into_any_element()
            }
            DiffRow::SplitNoNewline { file, hunk, left, right } => {
                let left = left.and_then(|line| self.line(file, hunk, line));
                let right = right.and_then(|line| self.line(file, hunk, line));
                let digits = left.or(right).map_or(3, |(_, digits)| digits);
                let p = palette(cx);
                self.code_row(hsla(p.background), cx)
                    .child(self.no_newline(left.map(|(line, _)| line), digits, cx))
                    .child(self.no_newline(right.map(|(line, _)| line), digits, cx))
                    .into_any_element()
            }
            DiffRow::Notice { .. } => {
                let p = palette(cx);
                div()
                    .h(px(HEADER_HEIGHT))
                    .flex()
                    .items_center()
                    .px(px(12.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child("Binary file not shown")
                    .into_any_element()
            }
        }
    }

    fn line(&self, file: usize, hunk: usize, line: usize) -> Option<(&DiffLine, usize)> {
        let view = self.view.as_ref()?;
        let line = view.parsed.files().get(file)?.hunks.get(hunk)?.lines.get(line)?;
        Some((line, view.digits.get(file).copied().unwrap_or(3)))
    }

    fn code_row(&self, fill: Hsla, cx: &App) -> Div {
        let family = cx.theme().mono_font_family.clone();
        div()
            .flex()
            .w_full()
            .when(!self.wrap, |this| this.h(px(LINE_HEIGHT)))
            .when(self.wrap, |this| this.min_h(px(LINE_HEIGHT)))
            .bg(fill)
            .font_family(family)
            .text_size(px(CODE_FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
    }

    /// One side of a row: a gutter with the line number and change bar, then the code. In the
    /// stacked layout deletions show their old number and everything else its new one.
    fn half(&self, file: usize, line: Option<&DiffLine>, digits: usize, new_side: bool, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let char_width = self.char_width.unwrap_or(px(7.8));
        let colors = line.map_or_else(|| line_colors(LineKind::Context, p), |line| line_colors(line.kind, p));
        let number = line.and_then(|line| match (line.kind, new_side) {
            (LineKind::Deletion, _) | (LineKind::Context, false) => line.old_line,
            _ => line.new_line,
        });
        let text: Option<SharedString> = line.map(display_text);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .bg(colors.row)
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(gutter_width(digits, char_width))
                    .pl(char_width * 2.)
                    .pr(char_width)
                    .bg(colors.gutter)
                    .text_color(colors.number)
                    .text_right()
                    .children(colors.bar.filter(|_| line.is_some()).map(|bar| div().absolute().top_0().bottom_0().left_0().w(px(4.)).bg(bar)))
                    .children(number.map(|number| SharedString::from(number.to_string())))
                    .map(|gutter| self.comment_gutter(gutter, file, line, new_side, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(CODE_PADDING))
                    .text_color(hsla(p.code_foreground))
                    .when(!self.wrap, |this| {
                        this.overflow_hidden().whitespace_nowrap().child(div().ml(-self.h_offset).children(text.clone()))
                    })
                    .when(self.wrap, |this| this.children(text)),
            )
    }

    /// Pierre's `createNoNewlineElement`: the line's fill with an empty gutter and the note at
    /// 60%, under the line that lacks a newline. An empty side paints nothing.
    fn no_newline(&self, line: Option<&DiffLine>, digits: usize, cx: &App) -> Div {
        let p = palette(cx);
        let char_width = self.char_width.unwrap_or(px(7.8));
        let colors = line.map_or_else(|| line_colors(LineKind::Context, p), |line| line_colors(line.kind, p));
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .bg(colors.row)
            .child(div().flex_none().w(gutter_width(digits, char_width)).bg(colors.gutter))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(CODE_PADDING))
                    .truncate()
                    .text_color(hsla(p.code_foreground).opacity(0.6))
                    .children(line.map(|_| "No newline at end of file")),
            )
    }

    fn render_separator(&self, file: usize, hidden: u32, cx: &App) -> AnyElement {
        let p = palette(cx);
        let digits = self.view.as_ref().and_then(|view| view.digits.get(file)).copied().unwrap_or(3);
        let char_width = self.char_width.unwrap_or(px(7.8));
        h_flex()
            .h(px(SEPARATOR_HEIGHT))
            .w_full()
            .bg(hsla(p.diff_hunk_separator))
            .pl(gutter_width(digits, char_width) + px(CODE_PADDING))
            .text_size(px(11.))
            .text_color(hsla(p.code_foreground).opacity(0.5))
            .child(separator_label(hidden))
            .into_any_element()
    }

    pub(super) fn render_file_header(&self, file_ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = self.view.as_ref().and_then(|view| view.parsed.files().get(file_ix)) else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let collapsed = self.is_collapsed(file_ix);
        let (icon, icon_color) = change_icon(file, p);
        let title = match file.change {
            ChangeKind::RenamePure | ChangeKind::RenameChanged => format!("{} → {}", file.previous_path(), file.path()),
            _ => file.path().to_owned(),
        };
        let path_label = file.path().to_owned();
        let toggle = cx.listener(move |this, _, _, cx| this.toggle_file(file_ix, cx));
        let toggle_button = cx.listener(move |this, _, _, cx| this.toggle_file(file_ix, cx));
        let copy = cx.listener(move |this, _, _, cx| this.copy_path(file_ix, cx));
        h_flex()
            .id(("diff-file-header", file_ix))
            .group("diff-file-header")
            .relative()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .w_full()
            .pl(px(8.))
            .pr(px(12.))
            .gap(px(6.))
            .bg(hsla(p.background))
            .text_size(px(12.))
            .text_color(hsla(p.code_foreground))
            .cursor_pointer()
            .on_click(toggle)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(3.))
                    .invisible()
                    .bg(hsla(p.code_foreground).opacity(0.24))
                    .group_hover("diff-file-header", |style| style.visible()),
            )
            .child(
                Button::new(("diff-file-fold", file_ix))
                    .variant(Variant::Ghost)
                    .size(Size::IconMicro)
                    .icon_size(16.)
                    .icon(if collapsed { Icon::ChevronRight } else { Icon::ChevronDown })
                    .tooltip(if collapsed { "Expand diff" } else { "Collapse diff" })
                    .on_click(move |event, window, cx| toggle_button(event, window, cx)),
            )
            .child(icon.svg().size(px(14.)).text_color(icon_color))
            .child(div().min_w_0().text_ellipsis_start().overflow_hidden().whitespace_nowrap().font_weight(FontWeight::MEDIUM).child(title))
            .child(
                Button::new(("diff-file-copy", file_ix))
                    .variant(Variant::Ghost)
                    .size(Size::IconMicro)
                    .icon(Icon::Copy)
                    .tooltip(format!("Copy {path_label}"))
                    .on_click(move |event, window, cx| copy(event, window, cx)),
            )
            .child(div().flex_1())
            .child(stat_label(u64::from(file.additions), u64::from(file.deletions), cx))
            .into_any_element()
    }
}

/// A line as shown: a trailing `\r` (CRLF content) is invisible on the web too, so it is kept
/// in the model but not painted.
fn display_text(line: &DiffLine) -> SharedString {
    match line.text.strip_suffix('\r') {
        Some(text) => SharedString::from(text.to_owned()),
        None => SharedString::from(line.text.clone()),
    }
}

/// Pierre's change icons with the app's tokens (`[data-change-icon]` in `diffRendering.ts`).
fn change_icon(file: &FileDiff, p: &Palette) -> (Icon, Hsla) {
    match file.change {
        ChangeKind::New => (Icon::FilePlus, hsla(p.diff_addition_foreground)),
        ChangeKind::Deleted => (Icon::FileMinus, hsla(p.diff_deletion_foreground)),
        ChangeKind::RenamePure | ChangeKind::RenameChanged => (Icon::FileSymlink, hsla(p.diff_modified_icon)),
        ChangeKind::Modified => (Icon::FileDiff, hsla(p.diff_modified_icon)),
    }
}

/// `DiffStatLabel` with `layout="inline"`.
pub fn stat_label(additions: u64, deletions: u64, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let family = cx.theme().mono_font_family.clone();
    h_flex()
        .flex_none()
        .gap(px(4.))
        .font_family(family)
        .child(div().text_color(hsla(p.diff_addition_foreground)).child(format!("+{}", compact_count(additions))))
        .child(div().text_color(hsla(p.diff_deletion_foreground)).child(format!("-{}", compact_count(deletions))))
}

/// `DiffPanelLoadingState`: a still skeleton of a file header, a separator and a few lines.
pub fn loading_state(cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let fill = hsla(p.muted_foreground).opacity(0.15);
    let bar = move |width: f32, height: f32| div().flex_none().w(px(width)).h(px(height)).rounded_full().bg(fill);
    let header = move |title: f32| {
        h_flex()
            .h(px(HEADER_HEIGHT))
            .px(px(8.))
            .gap(px(8.))
            .child(div().size(px(20.)).flex().items_center().justify_center().child(bar(10., 10.)))
            .child(div().size(px(20.)).rounded(px(4.)).bg(fill))
            .child(bar(title, 12.))
            .child(div().flex_1())
            .child(bar(20., 12.))
            .child(bar(20., 12.))
    };
    v_flex()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .child(header(220.))
        .child(
            h_flex()
                .h(px(SEPARATOR_HEIGHT))
                .px(px(8.))
                .gap(px(8.))
                .child(div().flex_1().h(px(1.)).bg(hsla(p.border).opacity(0.4)))
                .child(bar(96., 10.))
                .child(div().flex_1().h(px(1.)).bg(hsla(p.border).opacity(0.4))),
        )
        .child(
            v_flex()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .children([240., 300., 180.].map(|width| h_flex().gap(px(12.)).child(bar(20., 10.)).child(bar(width, 10.)))),
        )
        .child(header(160.))
        .child(header(260.))
}

/// A patch that did not parse: shown as text with the reason.
pub fn raw_patch(text: &str, reason: &str, wrap: bool, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let family = cx.theme().mono_font_family.clone();
    div().id("diff-raw").flex_1().min_h_0().overflow_y_scroll().p(px(8.)).child(
        v_flex()
            .gap(px(8.))
            .child(div().text_size(px(11.)).text_color(hsla(p.muted_foreground).opacity(0.75)).child(reason.to_owned()))
            .child(
                div()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .bg(hsla(p.background).opacity(0.7))
                    .p(px(12.))
                    .font_family(family)
                    .text_size(px(11.))
                    .line_height(px(18.))
                    .text_color(hsla(p.muted_foreground).opacity(0.9))
                    .when(!wrap, |this| this.whitespace_nowrap().overflow_hidden())
                    .child(text.to_owned()),
            ),
    )
}

/// What a provider recorded instead of a diff (`toolCallPreview.content` in `DiffPanel.tsx`).
pub fn recorded_content(content: &[RecordedContent], wrap: bool, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let family = cx.theme().mono_font_family.clone();
    v_flex()
        .id("diff-recorded-content")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(12.))
        .gap(px(12.))
        .text_xs()
        .child(div().text_color(hsla(p.muted_foreground)).child("The provider recorded file content without a diff."))
        .children(content.iter().enumerate().map(|(index, entry)| {
            let text = if entry.text.is_empty() { "Empty content.".to_owned() } else { entry.text.clone() };
            let pre = div()
                .font_family(family.clone())
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(hsla(p.foreground))
                .map(|this| if wrap { this.flex_1().min_w_0() } else { this.flex_none().whitespace_nowrap() })
                .child(text);
            // Unwrapped lines scroll sideways, like the web's `overflow-auto` <pre>; a vertical
            // wheel still scrolls the list.
            let body = h_flex().id(("diff-recorded-text", index)).items_start().w_full().when(!wrap, |this| {
                this.overflow_x_scroll().map(|mut this| {
                    this.style().restrict_scroll_to_axis = Some(true);
                    this
                })
            });
            v_flex().gap(px(4.)).child(div().text_color(hsla(p.muted_foreground)).child(entry.label)).child(body.child(pre))
        }))
}
