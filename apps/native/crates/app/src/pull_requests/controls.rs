//! The pull request surfaces' controls: `button.tsx` in the variants and sizes they use (default,
//! outline, destructive-outline, warning-outline, ghost, ghost-muted), `badge.tsx` (outline,
//! secondary, label, info), and the state, check and review glyphs of `pullRequestIcons.tsx`
//! and `pullRequestPresentation.tsx`. Callers pick a variant and size; they never restyle.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce, SharedString,
    Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px,
};
use gpui_component::menu::DropdownMenu;
use gpui_component::{ActiveTheme, Selectable};
use t3_core::pull_requests::detail::{ReviewOutcome, StateKind};
use t3_core::pull_requests::wire::{CheckStatus, ChecksState, ReviewDecision};

use crate::icons::Icon;
use crate::right_panel::{focusable, tooltip_view};
use crate::theme::{Palette, hsla, palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Default,
    Outline,
    DestructiveOutline,
    WarningOutline,
    Ghost,
    GhostMuted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 24 tall, 12px text, 14px icon.
    Xs,
    /// 28 tall, 14px text.
    Sm,
    /// 32 tall.
    Default,
    /// 32 square.
    Icon,
    /// 28 square.
    IconSm,
}

impl Size {
    fn metrics(self) -> (f32, f32, f32, f32, f32) {
        match self {
            Size::Xs => (24., 7., 12., 14., 4.),
            Size::Sm => (28., 9., 14., 16., 6.),
            Size::Default => (32., 11., 14., 16., 8.),
            Size::Icon => (32., 0., 14., 16., 8.),
            Size::IconSm => (28., 0., 14., 16., 6.),
        }
    }
}

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    base: Stateful<Div>,
    variant: Variant,
    size: Size,
    icon: Option<Icon>,
    label: Option<SharedString>,
    trailing: Option<SharedString>,
    disabled: bool,
    busy: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, variant: Variant, size: Size) -> Self {
        let id = id.into();
        Button { base: div().id(id.clone()), id, variant, size, icon: None, label: None, trailing: None, disabled: false, busy: false, tooltip: None, on_click: None }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// A count pill after the label, as the Filters trigger shows.
    pub fn count(mut self, count: usize) -> Self {
        self.trailing = (count > 0).then(|| count.to_string().into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Shows a static loader in place of the icon while an action runs.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Button {}

impl Selectable for Button {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl DropdownMenu for Button {}

struct Look {
    text: Hsla,
    icon: Hsla,
    fill: Option<Hsla>,
    border: Option<Hsla>,
    hover_fill: Hsla,
    hover_text: Hsla,
}

fn look(variant: Variant, p: &Palette, dark: bool) -> Look {
    let outline_fill = if dark { hsla(p.input).opacity(0.32) } else { hsla(p.popover) };
    let outline_hover = if dark { hsla(p.input).opacity(0.64) } else { hsla(p.accent).opacity(0.5) };
    match variant {
        Variant::Default => Look {
            text: hsla(p.primary_foreground),
            icon: hsla(p.primary_foreground),
            fill: Some(hsla(p.primary)),
            border: Some(hsla(p.primary)),
            hover_fill: hsla(p.primary).opacity(0.9),
            hover_text: hsla(p.primary_foreground),
        },
        Variant::Outline => Look {
            text: hsla(p.foreground),
            icon: hsla(p.muted_foreground),
            fill: Some(outline_fill),
            border: Some(hsla(p.input)),
            hover_fill: outline_hover,
            hover_text: hsla(p.foreground),
        },
        Variant::DestructiveOutline => Look {
            text: hsla(p.destructive_foreground),
            icon: hsla(p.destructive_foreground),
            fill: Some(outline_fill),
            border: Some(hsla(p.input)),
            hover_fill: hsla(p.destructive).opacity(0.04),
            hover_text: hsla(p.destructive_foreground),
        },
        Variant::WarningOutline => Look {
            text: hsla(p.warning_foreground),
            icon: hsla(p.warning_foreground),
            fill: Some(hsla(p.warning_surface)),
            border: Some(hsla(p.warning).opacity(0.32)),
            hover_fill: hsla(p.warning).opacity(if dark { 0.24 } else { 0.16 }),
            hover_text: hsla(p.warning_foreground),
        },
        Variant::Ghost => Look {
            text: hsla(p.foreground),
            icon: hsla(p.muted_foreground),
            fill: None,
            border: None,
            hover_fill: hsla(p.accent),
            hover_text: hsla(p.foreground),
        },
        Variant::GhostMuted => Look {
            text: hsla(p.muted_foreground),
            icon: hsla(p.muted_foreground),
            fill: None,
            border: None,
            hover_fill: hsla(p.accent),
            hover_text: hsla(p.foreground),
        },
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = palette(cx);
        let (height, pad, text, icon_size, gap) = self.size.metrics();
        let look = look(self.variant, p, cx.theme().is_dark());
        let square = matches!(self.size, Size::Icon | Size::IconSm);
        let enabled = !self.disabled;
        let hover_fill = look.hover_fill;
        let hover_text = look.hover_text;
        let icon = if self.busy { Some(Icon::LoaderCircle) } else { self.icon };
        let pill = hsla(p.muted);
        let pill_text = hsla(p.muted_foreground);
        focusable(self.base, self.id, enabled, window, cx)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(height))
            .when(square, |this| this.w(px(height)))
            .when(!square, |this| this.px(px(pad)))
            .gap(px(gap))
            .rounded(px(8.))
            .border_1()
            .border_color(look.border.unwrap_or(gpui::transparent_black()))
            .when_some(look.fill, |this, fill| this.bg(fill))
            .text_size(px(text))
            .font_weight(FontWeight::MEDIUM)
            .text_color(look.text)
            .whitespace_nowrap()
            .when(!enabled, |this| this.opacity(0.64))
            .when(enabled, |this| this.cursor_pointer().hover(move |style| style.bg(hover_fill).text_color(hover_text)))
            .when_some(icon, |this, icon| this.child(icon.sized(px(icon_size)).text_color(look.icon)))
            .when_some(self.label, |this, label| this.child(label))
            .when_some(self.trailing, |this, count| this.child(div().px(px(6.)).rounded_full().bg(pill).text_xs().text_color(pill_text).child(count)))
            .when_some(self.tooltip, |this, text| this.tooltip(move |window, cx| tooltip_view(text.clone(), window, cx)))
            .when_some(self.on_click.filter(|_| enabled), |this, handler| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}

/// `badge.tsx` variants the pull request surfaces use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BadgeKind {
    Outline,
    Secondary,
    /// A host label's own color (`variant="label"`): 8% fill (12% dark), text mixed toward it.
    Label(u32),
    Info,
}

/// A `sm` badge (16 tall, 10px) or default (18 tall, 12px).
pub fn badge(text: impl Into<SharedString>, kind: BadgeKind, large: bool, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let (fill, color, border) = match kind {
        BadgeKind::Outline => (if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) }, hsla(p.foreground), Some(hsla(p.input))),
        BadgeKind::Secondary => (hsla(p.secondary), hsla(p.secondary_foreground), None),
        BadgeKind::Label(rgb) => {
            let base: Hsla = gpui::rgb(rgb).into();
            (base.opacity(if dark { 0.12 } else { 0.08 }), mix(base, hsla(p.foreground), if dark { 0.45 } else { 0.30 }), None)
        }
        BadgeKind::Info => (hsla(p.info).opacity(if dark { 0.16 } else { 0.08 }), hsla(p.info_foreground), None),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .min_w_0()
        .max_w(px(160.))
        .h(px(if large { 18. } else { 16. }))
        .px(px(3.))
        .rounded(px(4.))
        .border_1()
        .border_color(border.unwrap_or(gpui::transparent_black()))
        .bg(fill)
        .text_size(px(if large { 12. } else { 10. }))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(div().min_w_0().truncate().child(text.into()))
}

/// The label badge's text color: the label hue mixed `amount` into the foreground.
fn mix(color: Hsla, foreground: Hsla, amount: f32) -> Hsla {
    let a = color.to_rgb();
    let b = foreground.to_rgb();
    let channel = |x: f32, y: f32| x * amount + y * (1. - amount);
    gpui::Rgba { r: channel(a.r, b.r), g: channel(a.g, b.g), b: channel(a.b, b.b), a: 1. }.into()
}

/// `PULL_REQUEST_STATE_PRESENTATION`: open emerald, draft zinc, closed red, merged violet. The
/// hues are the same Tailwind classes the sidebar status pills use (design-system §2.2).
pub fn state_glyph(kind: StateKind, cx: &App) -> (Icon, Hsla) {
    let p = palette(cx);
    match kind {
        StateKind::Open => (Icon::GitPullRequestArrow, hsla(p.pill_completed)),
        StateKind::Draft => (Icon::GitPullRequestDraft, hsla(p.muted_foreground)),
        StateKind::Closed => (Icon::GitPullRequestClosed, hsla(p.pill_failed)),
        StateKind::Merged => (Icon::GitMerge, hsla(p.pill_plan)),
    }
}

/// `CHECK_STATUS_PRESENTATION`.
pub fn check_glyph(status: CheckStatus, cx: &App) -> (Icon, Hsla) {
    let p = palette(cx);
    match status {
        CheckStatus::Pending => (Icon::LoaderCircle, hsla(p.pill_approval_dot)),
        CheckStatus::ActionRequired => (Icon::CircleDot, hsla(p.pill_approval)),
        CheckStatus::Success => (Icon::CircleCheck, hsla(p.pill_completed)),
        CheckStatus::Failure | CheckStatus::Cancelled => (Icon::CircleX, hsla(p.destructive)),
        CheckStatus::Skipped => (Icon::CircleSlash, hsla(p.muted_foreground).opacity(0.7)),
        CheckStatus::Neutral | CheckStatus::Unknown => (Icon::CircleDashed, hsla(p.muted_foreground).opacity(0.7)),
    }
}

/// `CHECKS_STATE_PRESENTATION`.
pub fn checks_glyph(state: ChecksState, cx: &App) -> (Icon, Hsla) {
    let p = palette(cx);
    match state {
        ChecksState::Passing => (Icon::CircleCheck, hsla(p.pill_completed)),
        ChecksState::Failing => (Icon::CircleX, hsla(p.destructive)),
        ChecksState::Pending | ChecksState::Unknown => (Icon::CircleDot, hsla(p.pill_approval)),
    }
}

/// `reviewDecisionPresentation`.
pub fn review_glyph(decision: ReviewDecision, cx: &App) -> Option<(Icon, Hsla, &'static str)> {
    let p = palette(cx);
    match decision {
        ReviewDecision::Approved => Some((Icon::UserCheck, hsla(p.pill_completed), "Approved")),
        ReviewDecision::ChangesRequested => Some((Icon::UserRoundX, hsla(p.pill_approval).opacity(0.9), "Changes requested")),
        ReviewDecision::ReviewRequired => Some((Icon::UserRound, hsla(p.muted_foreground).opacity(0.6), "Awaiting review")),
        ReviewDecision::Unknown => None,
    }
}

pub fn outcome_glyph(outcome: ReviewOutcome, cx: &App) -> (Icon, Hsla) {
    let p = palette(cx);
    match outcome {
        ReviewOutcome::Approved => (Icon::CircleCheck, hsla(p.pill_completed)),
        ReviewOutcome::ChangesRequested => (Icon::CircleX, hsla(p.destructive)),
        ReviewOutcome::Dismissed => (Icon::CircleDashed, hsla(p.muted_foreground).opacity(0.7)),
    }
}

/// An icon with a tooltip, as the row's signal glyphs show.
pub fn glyph(id: impl Into<ElementId>, icon: Icon, color: Hsla, size: f32, label: &'static str) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .child(icon.sized(px(size)).text_color(color))
        .tooltip(move |window, cx| tooltip_view(label.into(), window, cx))
        .into_any_element()
}

/// `PullRequestActorAvatar` without the image: the login's initial on `muted`.
pub fn avatar(login: &str, size: f32, cx: &App) -> gpui::Div {
    let p = palette(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(hsla(p.muted))
        .text_size(px(if size > 16. { 10. } else { 8. }))
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .child(login.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default())
}

/// `+12 −3` in the diff colors, tabular.
pub fn diff_stat(additions: u64, deletions: u64, cx: &App) -> gpui::Div {
    let p = palette(cx);
    div()
        .flex()
        .flex_none()
        .gap(px(4.))
        .font_family(cx.theme().mono_font_family.clone())
        .text_xs()
        .child(div().text_color(hsla(p.diff_addition_foreground)).child(format!("+{additions}")))
        .child(div().text_color(hsla(p.diff_deletion_foreground)).child(format!("−{deletions}")))
}
