//! Shared pieces of transcript rows: the work-log line (`WorkLog.tsx`), hover timestamps
//! (`TimelineRowParts.tsx`), the system divider (`TimelineSystemDivider.tsx`) and the small
//! ghost buttons rows use. Each owns its look; callers only place them.

use gpui::{AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px};
use gpui_component::h_flex;

use crate::icons::Icon;
use crate::theme::{hsla, palette};

pub const ROW_GROUP: &str = "timeline-row";

/// Wall clock for timestamps, read once per render pass.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now_ms: i64,
    pub offset_minutes: i64,
}

impl Clock {
    pub fn now() -> Self {
        let now = chrono::Local::now();
        Clock { now_ms: now.timestamp_millis(), offset_minutes: i64::from(now.offset().local_minus_utc() / 60) }
    }

    pub fn format(&self, iso: &str) -> String {
        t3_core::timeline::format_day_aware(iso, self.now_ms, self.offset_minutes).unwrap_or_default()
    }
}

/// `TimelineRowTimestamp`: 12px tabular `muted_foreground`, shown on row hover unless `always`.
pub fn timestamp(clock: &Clock, iso: &str, always: bool, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_xs()
        .text_color(hsla(palette(cx).muted_foreground))
        .when(!always, |this| this.invisible().group_hover(ROW_GROUP, |style| style.visible()))
        .child(clock.format(iso))
        .into_any_element()
}

/// A keyboard-reachable control: a tab stop that Enter and Space activate (GPUI delivers them as
/// a click), with the 2px `ring` drawn inside when focused from the keyboard (§4.2).
pub fn focusable(element: gpui::Stateful<gpui::Div>, cx: &App) -> gpui::Stateful<gpui::Div> {
    let ring = hsla(palette(cx).ring);
    element.tab_index(0).focus_visible(move |style| style.border_2().border_color(ring))
}

/// The 24px icon slot of a work row with a 16px icon (§7.7).
pub fn icon_slot(icon: Icon, color: Hsla) -> AnyElement {
    div().size(px(24.)).flex_none().flex().items_center().justify_center().child(icon.render(px(16.), color)).into_any_element()
}

/// `WorkLogLine`: min 24, gap 6, 14px on a 1.625 line, label in `secondary_label`.
pub fn work_line(icon: Option<AnyElement>, label: AnyElement, trailing: Vec<AnyElement>, wrap: bool, cx: &App) -> gpui::Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(px(24.))
        .gap(px(6.))
        .text_size(px(14.))
        .line_height(px(22.75))
        .when(wrap, |this| this.items_start())
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .when(wrap, |this| this.items_start())
                .children(icon)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(hsla(palette(cx).secondary_label))
                        .when(!wrap, |this| this.truncate())
                        .child(label),
                ),
        )
        .children(trailing)
}

/// `WorkLogRow` chrome: radius 6, pad-x 2, 2px vertical when standalone.
pub fn work_row_frame(id: impl Into<ElementId>, grouped: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group(ROW_GROUP)
        .relative()
        .w_full()
        .min_w_0()
        .rounded(px(6.))
        .px(px(2.))
        .when(!grouped, |this| this.py(px(2.)))
}

/// The 12px disclosure chevron at the end of a work row, rotated when open.
pub fn chevron(open: bool, visible: bool, cx: &App) -> AnyElement {
    let icon = if open { Icon::ChevronDown } else { Icon::ChevronRight };
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(!visible, |this| this.invisible())
        .child(icon.render(px(12.), hsla(palette(cx).icon_muted).opacity(0.7)))
        .into_any_element()
}

pub enum DividerTone {
    Neutral,
    Danger,
}

/// `TimelineSystemDivider`: a centered 11px label between two hairlines.
pub fn system_divider(label: SharedString, detail: Option<SharedString>, icon: Option<Icon>, tone: DividerTone, cx: &App) -> AnyElement {
    let p = palette(cx);
    let color = match tone {
        DividerTone::Neutral => hsla(p.muted_foreground),
        DividerTone::Danger => hsla(p.destructive),
    };
    let line = || div().h(px(1.)).flex_1().bg(hsla(p.border).opacity(0.7));
    h_flex()
        .w_full()
        .min_w_0()
        .gap(px(8.))
        .py(px(8.))
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(color)
        .child(line())
        .child(
            h_flex()
                .min_w_0()
                .gap(px(6.))
                .px(px(8.))
                .py(px(4.))
                .children(icon.map(|icon| icon.render(px(12.), color)))
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .children(detail.map(|detail| div().min_w_0().max_w(px(320.)).truncate().opacity(0.7).child(format!("·\u{a0}{detail}")))),
        )
        .child(line())
        .into_any_element()
}

/// An `xs` ghost-muted text button (24 tall, pad-x 7, 12px text).
pub fn ghost_text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon: Option<Icon>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(24.))
        .px(px(7.))
        .rounded(px(6.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
        .children(icon.map(|icon| icon.render(px(12.), hsla(p.muted_foreground))))
        .child(label.into())
        .on_click(on_click);
    focusable(button, cx).into_any_element()
}

/// `InlineButton` tones (`button.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineTone {
    Default,
    Muted,
}

/// `InlineButton`: text-sized, 500 weight, underlined on hover; dimmed to 64% when disabled.
pub fn inline_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: InlineTone,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let (text, hover) = match tone {
        InlineTone::Default => (hsla(p.foreground), hsla(p.foreground)),
        InlineTone::Muted => (hsla(p.muted_foreground), hsla(p.foreground)),
    };
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .whitespace_nowrap()
        .font_weight(FontWeight::MEDIUM)
        .text_color(text)
        .child(label.into());
    if disabled {
        return button.opacity(0.64).into_any_element();
    }
    focusable(button.cursor_pointer().hover(move |style| style.underline().text_color(hover)).on_click(on_click), cx).into_any_element()
}

/// An `xs` ghost text button (24 tall, pad-x 7, 12px text in `foreground`, hover `accent`).
pub fn ghost_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .px(px(7.))
        .rounded(px(6.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.foreground))
        .child(label.into());
    if disabled {
        return button.opacity(0.64).into_any_element();
    }
    focusable(button.cursor_pointer().hover(|style| style.bg(hsla(p.accent))).on_click(on_click), cx).into_any_element()
}

/// An `xs` outline text button (24 tall, pad-x 7, 12px text, `input` border, control shadow).
pub fn outline_button(id: impl Into<ElementId>, label: impl Into<SharedString>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let p = palette(cx);
    let dark = gpui_component::ActiveTheme::theme(cx).is_dark();
    let (fill, hover) = if dark { (hsla(p.input).opacity(0.32), hsla(p.input).opacity(0.64)) } else { (hsla(p.popover), hsla(p.accent).opacity(0.5)) };
    let shadow = gpui::BoxShadow { color: gpui::black().opacity(0.05), offset: gpui::point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false };
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .px(px(7.))
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.input))
        .bg(fill)
        .shadow(vec![shadow])
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.foreground))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label.into())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        });
    focusable(button, cx).into_any_element()
}

/// An `icon-xs` ghost button (24 square) with a 12px icon.
pub fn ghost_icon_button(id: impl Into<ElementId>, icon: Icon, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let p = palette(cx);
    let button = div()
        .id(id)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(p.accent)))
        .child(icon.render(px(12.), hsla(p.muted_foreground)))
        .on_click(on_click);
    focusable(button, cx).into_any_element()
}

/// An `icon-xs` ghost button with a tooltip, dimmed and inert while `disabled`.
pub fn tooltip_icon_button(
    id: impl Into<ElementId>,
    icon: Icon,
    tooltip: &'static str,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let button = div()
        .id(id)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .child(icon.render(px(12.), hsla(p.muted_foreground)))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip).build(window, cx));
    if disabled {
        return button.opacity(0.64).into_any_element();
    }
    focusable(button.cursor_pointer().hover(|style| style.bg(hsla(p.accent))).on_click(on_click), cx).into_any_element()
}

/// Mono text block for expanded tool details: code size on a 1.625 line, `secondary_label`.
pub fn mono_block(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .font_family(gpui_component::ActiveTheme::theme(cx).mono_font_family.clone())
        .text_size(px(crate::theme::metrics::DEFAULT_CODE_FONT_SIZE))
        .line_height(px(crate::theme::metrics::DEFAULT_CODE_FONT_SIZE * 1.625))
        .text_color(hsla(palette(cx).secondary_label))
        .child(text.into())
}
