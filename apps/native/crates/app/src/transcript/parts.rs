//! Shared pieces of transcript rows: the work-log line (`WorkLog.tsx`), hover timestamps
//! (`TimelineRowParts.tsx`), the system divider (`TimelineSystemDivider.tsx`) and the small
//! ghost buttons rows use. Each owns its look; callers only place them.

use std::rc::Rc;
use std::time::Duration;

use gpui::{AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px};
use gpui_component::h_flex;

use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};

pub const ROW_GROUP: &str = "timeline-row";

/// Wall clock for timestamps, read once per render pass.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now_ms: i64,
    pub zone: t3_core::sidebar::time::NamedZone,
}

impl Clock {
    pub fn now() -> Self {
        let now = chrono::Local::now();
        Clock { now_ms: now.timestamp_millis(), zone: crate::timestamp::viewer_zone() }
    }

    pub fn format(&self, iso: &str, cx: &App) -> String {
        t3_core::timeline::format_day_aware(iso, self.now_ms, &self.zone, crate::settings::store::client(cx).timestamp_format(), &crate::timestamp::SystemTimestampLocale)
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
        .child(clock.format(iso, cx))
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
    div().size(rem(24.)).flex_none().flex().items_center().justify_center().child(icon.render(px(16.), color).size(rem(16.))).into_any_element()
}

/// `WorkLogLine`: min 24, gap 6, 14px on a 1.625 line, label in `secondary_label`.
pub fn work_line(icon: Option<AnyElement>, label: AnyElement, trailing: Vec<AnyElement>, wrap: bool, cx: &App) -> gpui::Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(rem(24.))
        .gap(rem(6.))
        .text_size(rem(14.))
        .line_height(rem(22.75))
        .when(wrap, |this| this.items_start())
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(rem(6.))
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
        .px(rem(2.))
        .when(!grouped, |this| this.py(rem(2.)))
}

/// The 12px disclosure chevron at the end of a work row, rotated when open.
pub fn chevron(open: bool, visible: bool, cx: &App) -> AnyElement {
    let icon = if open { Icon::ChevronDown } else { Icon::ChevronRight };
    div()
        .size(rem(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(!visible, |this| this.invisible())
        .child(icon.render(px(12.), hsla(palette(cx).icon_muted).opacity(0.7)).size(rem(12.)))
        .into_any_element()
}

pub enum DividerTone {
    Neutral,
}

/// `TimelineSystemDivider`: a centered 11px label between two hairlines.
pub fn system_divider(label: SharedString, detail: Option<SharedString>, icon: Option<Icon>, tone: DividerTone, cx: &App) -> AnyElement {
    let p = palette(cx);
    let color = match tone {
        DividerTone::Neutral => hsla(p.muted_foreground),
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

/// Interactive system divider where only the centered pill is focusable, clickable, and shows
/// the tooltip (`TimelineSystemDivider` interactive variant).
pub fn system_divider_button(
    id: impl Into<ElementId>,
    label: SharedString,
    icon: Option<Icon>,
    tooltip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let line = || div().h(px(1.)).flex_1().bg(hsla(p.border).opacity(0.7));
    let pill = h_flex()
        .id(id)
        .min_w_0()
        .gap(px(6.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .text_color(hsla(p.muted_foreground))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(p.accent).opacity(0.6)).text_color(hsla(p.foreground)))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip).build(window, cx))
        .children(icon.map(|icon| icon.render(px(12.), hsla(p.muted_foreground))))
        .child(div().font_weight(FontWeight::MEDIUM).child(label))
        .on_click(on_click);
    h_flex()
        .w_full()
        .min_w_0()
        .gap(px(8.))
        .py(px(8.))
        .text_size(px(11.))
        .line_height(px(16.))
        .child(line())
        .child(focusable(pill, cx))
        .child(line())
        .into_any_element()
}

#[derive(Default)]
struct CopyButtonState {
    seq: u64,
    _reset: Option<Task<()>>,
}

type CopyHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// An `icon-xs` copy button with 2-second check confirmation (`MessageCopyButton`).
#[derive(IntoElement)]
pub struct CopyButton {
    id: SharedString,
    tooltip: &'static str,
    on_copy: CopyHandler,
}

impl CopyButton {
    pub fn new(id: impl Into<SharedString>, tooltip: &'static str, on_copy: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { id: id.into(), tooltip, on_copy: Rc::new(on_copy) }
    }
}

impl RenderOnce for CopyButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| CopyButtonState::default());
        let copied = state.read(cx).seq > 0;
        let icon = if copied { Icon::Check } else { Icon::Copy };
        let color = if copied { hsla(p.primary) } else { hsla(p.muted_foreground) };
        let tip = if copied { "Copied" } else { self.tooltip };
        let on_copy = self.on_copy;
        let button = div()
            .id(self.id)
            .size(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent)))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip).build(window, cx))
            .child(icon.render(px(12.), color))
            .on_click(move |_, window, cx| {
                on_copy(window, cx);
                state.update(cx, |s, cx| {
                    s.seq = s.seq.wrapping_add(1).max(1);
                    let current = s.seq;
                    s._reset = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(2)).await;
                        let _ = this.update(cx, |s, cx| {
                            if s.seq == current {
                                s.seq = 0;
                                cx.notify();
                            }
                        });
                    }));
                    cx.notify();
                });
            });
        focusable(button, cx)
    }
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
    mono_frame(cx).child(text.into())
}

/// `mono_block` without its text, for a styled child.
pub fn mono_frame(cx: &App) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .font_family(gpui_component::ActiveTheme::theme(cx).mono_font_family.clone())
        .text_size(gpui_component::ActiveTheme::theme(cx).mono_font_size)
        .line_height(gpui_component::ActiveTheme::theme(cx).mono_font_size * 1.625)
        .text_color(hsla(palette(cx).secondary_label))
}
