//! The worktree setup card (`WorktreeSetupCard.tsx`) and the working row's "Background setup"
//! chip with its popover (`BackgroundWorktreeSetupChip` in `TimelineWorkGroupRows.tsx`). Rules and
//! copy come from `t3_core::worktree_setup`. Only a running stage's time changes on a timer, each
//! through its own `WorkingTimer`, which exists only while setup runs. Text and spacing are rem
//! metrics, including offsets that line up with the shared work row's icon slot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui::{AnyElement, App, Context, Entity, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::worktree_setup::{self as rules, Elapsed, Snapshot, Stage, StageId, StageStatus, Tone};

use super::RowEnv;
use super::parts::{focusable, ghost_text_button, icon_slot, work_line, work_row_frame};
use super::timer::WorkingTimer;
use crate::composer::controls::{Float, floating, floating_shadow};
use crate::icons::Icon;
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;
use crate::worktree_setup::{OpenSetupTerminal, SetupView};

/// What the setup card and chip draw from, beside the snapshot.
#[derive(Default)]
pub struct SetupEnv {
    pub view: SetupView,
    pub details_open: bool,
    pub popover_open: bool,
    /// The popover closed on this click-away; the trigger's own click must not reopen it.
    pub popover_dismissed: Option<Instant>,
    /// One ticking label per running stage, by its `startedAt`.
    pub timers: HashMap<String, Entity<WorkingTimer>>,
}

impl SetupEnv {
    /// The running stages' start times, which tick while setup runs and only then.
    pub fn live_starts(&self) -> Vec<String> {
        let mut starts = Vec::new();
        for snapshot in [self.view.visible.as_ref(), self.view.background.as_ref()].into_iter().flatten() {
            for stage in &snapshot.stages {
                if let Some(Elapsed::Live(started_at)) = rules::stage_elapsed(stage, snapshot)
                    && !starts.contains(&started_at)
                {
                    starts.push(started_at);
                }
            }
        }
        starts
    }
}

fn stage_icon(status: StageStatus) -> Icon {
    match status {
        StageStatus::Done => Icon::WorkCheck,
        StageStatus::Running => Icon::LoaderCircle,
        StageStatus::Failed => Icon::WorkX,
        StageStatus::Warning => Icon::WorkCircleAlert,
        StageStatus::Skipped => Icon::WorkMinus,
        StageStatus::Pending | StageStatus::Unknown => Icon::WorkCircle,
    }
}

/// `stageRowClassName`.
fn stage_color(status: StageStatus, cx: &App) -> Hsla {
    let p = palette(cx);
    match status {
        StageStatus::Failed => hsla(p.destructive_foreground),
        StageStatus::Warning => hsla(p.warning_foreground),
        StageStatus::Pending | StageStatus::Unknown => hsla(p.secondary_label).opacity(0.4),
        StageStatus::Running | StageStatus::Skipped | StageStatus::Done => hsla(p.secondary_label),
    }
}

fn status_icon(status: StageStatus, cx: &App) -> AnyElement {
    let color = hsla(palette(cx).icon_muted);
    let pending = matches!(status, StageStatus::Pending | StageStatus::Unknown);
    icon_slot(stage_icon(status), if pending { color.opacity(0.4) } else { color })
}

fn muted(cx: &App) -> gpui::Div {
    div().text_xs().text_color(hsla(palette(cx).muted_foreground))
}

fn muted_text(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    muted(cx).child(text.into())
}

/// A stage's duration: the settled span, or its ticking timer while setup runs.
fn elapsed_label(elapsed: Option<Elapsed>, env: &RowEnv, cx: &App) -> Option<AnyElement> {
    let label = match elapsed? {
        Elapsed::Fixed(text) => muted_text(text, cx),
        Elapsed::Live(started_at) => match env.setup.timers.get(&started_at) {
            Some(timer) => muted(cx).child(timer.clone()),
            None => muted_text(rules::live_label(&started_at, chrono::Utc::now().timestamp_millis()).unwrap_or_default(), cx),
        },
    };
    Some(label.flex_none().into_any_element())
}

fn stage_row(scope: &str, stage: &Stage, snapshot: &Snapshot, env: &RowEnv, cx: &App) -> AnyElement {
    let label = rules::stage_display_label(stage, snapshot.setup_script.as_ref().map(|script| script.name.as_str()));
    let mut trailing: Vec<AnyElement> = Vec::new();
    if let Some(text) = rules::stage_trailing(stage) {
        trailing.push(muted_text(text, cx).min_w_0().truncate().into_any_element());
    }
    trailing.extend(elapsed_label(rules::stage_elapsed(stage, snapshot), env, cx));
    work_row_frame(SharedString::from(format!("{scope}:stage:{:?}", stage.id)), false)
        .child(work_line(
            Some(status_icon(stage.status, cx)),
            div().truncate().text_color(stage_color(stage.status, cx)).child(label.to_owned()).into_any_element(),
            trailing,
            false,
            cx,
        ))
        .into_any_element()
}

/// `OutputTail`: the setup script's last four lines, tinted when it failed.
fn output_tail(lines: &[String], failed: bool, cx: &App) -> AnyElement {
    let p = palette(cx);
    let (border, fill, text) = if failed {
        (hsla(p.destructive).opacity(0.2), hsla(p.error_surface), hsla(p.destructive_foreground))
    } else {
        (hsla(p.border), hsla(p.code_background), hsla(p.muted_foreground))
    };
    let rows = rules::tail_rows(lines).map(|line| div().w_full().min_w_0().truncate().child(if line.is_empty() { "\u{a0}".to_owned() } else { line.to_owned() }));
    v_flex()
        .mb(rem(4.))
        .ml(rem(32.))
        .min_w_0()
        .overflow_hidden()
        .rounded(px(6.))
        .border_1()
        .border_color(border)
        .bg(fill)
        .px(rem(10.))
        .py(rem(6.))
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(rem(11.))
        .line_height(rem(11. * 1.625))
        .text_color(text)
        .children(rows)
        .into_any_element()
}

/// `SetupHeaderRow`, shown once setup failed or was cancelled.
fn header_row(snapshot: &Snapshot, cx: &App) -> AnyElement {
    let p = palette(cx);
    let tone = match rules::header_tone(snapshot) {
        Tone::Destructive => hsla(p.destructive_foreground),
        Tone::Warning => hsla(p.warning_foreground),
        Tone::Muted => hsla(p.muted_foreground),
    };
    let elapsed = match rules::elapsed(Some(&snapshot.started_at), snapshot.ended_at.as_deref(), false) {
        Some(Elapsed::Fixed(text)) => Some(muted_text(text, cx).flex_none().ml_auto()),
        _ => None,
    };
    div()
        .w_full()
        .pt(rem(4.))
        .pb(rem(8.))
        .border_b_1()
        .border_color(hsla(p.border).opacity(0.6))
        .child(
            h_flex()
                .h(rem(24.))
                .min_w_0()
                .gap(rem(8.))
                .px(rem(4.))
                .text_size(rem(14.))
                .text_color(tone)
                .child(div().min_w_0().truncate().child(rules::header_label(snapshot)))
                .children(elapsed),
        )
        .into_any_element()
}

/// `CollapsedSummaryRow`: a settled setup the agent took over.
fn summary_row(scope: &str, snapshot: &Snapshot, cx: &App) -> AnyElement {
    let status = rules::summary_status(snapshot);
    let elapsed = match rules::elapsed(Some(&snapshot.started_at), snapshot.ended_at.as_deref(), false) {
        Some(Elapsed::Fixed(text)) => vec![muted_text(text, cx).flex_none().into_any_element()],
        _ => Vec::new(),
    };
    work_row_frame(SharedString::from(format!("{scope}:summary")), false)
        .child(work_line(
            Some(status_icon(status, cx)),
            div().truncate().text_color(stage_color(status, cx)).child(rules::header_label(snapshot)).into_any_element(),
            elapsed,
            false,
            cx,
        ))
        .into_any_element()
}

/// `SetupDetails`: branch, base, path and setup command, labels in one column.
fn details(snapshot: &Snapshot, cx: &App) -> AnyElement {
    let p = palette(cx);
    let pairs = rules::details(snapshot);
    let mono = cx.theme().mono_font_family.clone();
    let labels = pairs.iter().map(|(label, _)| div().whitespace_nowrap().text_color(hsla(p.foreground).opacity(0.8)).child(*label));
    let values = pairs.iter().map(|(_, value)| div().min_w_0().truncate().font_family(mono.clone()).child(value.to_string()));
    h_flex()
        .items_start()
        .mt(rem(4.))
        .mb(rem(6.))
        .ml(rem(32.))
        .min_w_0()
        .gap(rem(12.))
        .text_xs()
        .text_color(hsla(p.muted_foreground))
        .child(v_flex().flex_none().gap(rem(2.)).children(labels))
        .child(v_flex().flex_1().min_w_0().gap(rem(2.)).children(values))
        .into_any_element()
}

/// `WorktreeSetupCard`. `embedded` is the settled card once the agent took over, or the
/// background chip's popover; neither offers Cancel or Work locally.
pub fn card(scope: &str, snapshot: &Arc<Snapshot>, embedded: bool, env: &RowEnv, cx: &mut Context<ThreadView>) -> AnyElement {
    let layout = rules::layout(snapshot, embedded);
    let running = snapshot.running();
    let body: AnyElement = if layout.collapsed {
        summary_row(scope, snapshot, cx)
    } else {
        let mut rows: Vec<AnyElement> = Vec::new();
        for stage in &snapshot.stages {
            rows.push(stage_row(scope, stage, snapshot, env, cx));
            if stage.id == StageId::SetupScript && layout.show_tail {
                rows.push(output_tail(&stage.tail, stage.status == StageStatus::Failed, cx));
            }
        }
        v_flex().w_full().min_w_0().when(layout.show_header, |this| this.pt(rem(6.))).children(rows).into_any_element()
    };
    let error = snapshot.error.clone().filter(|_| layout.show_error).map(|error| muted_text(error, cx).mt(rem(4.)).ml(rem(32.)));

    let open = env.setup.details_open;
    let mut actions = vec![ghost_text_button(
        SharedString::from(format!("{scope}:details")),
        rules::DETAILS,
        Some(if open { Icon::ChevronDown } else { Icon::ChevronRight }),
        cx.listener(|view, _, _, cx| view.toggle_setup_details(cx)),
        cx,
    )];
    let terminal = snapshot.setup_script.as_ref().map(|script| script.terminal_id.clone()).filter(|_| layout.script_started);
    if let Some(terminal_id) = terminal {
        actions.push(ghost_text_button(
            SharedString::from(format!("{scope}:terminal")),
            rules::OPEN_TERMINAL,
            Some(Icon::Terminal),
            move |_, window: &mut Window, cx: &mut App| window.dispatch_action(Box::new(OpenSetupTerminal { terminal_id: terminal_id.clone() }), cx),
            cx,
        ));
    }
    if !embedded && running && env.setup.view.work_locally {
        actions.push(ghost_text_button(
            SharedString::from(format!("{scope}:local")),
            rules::WORK_LOCALLY,
            Some(Icon::Laptop),
            cx.listener(|view, _, _, cx| view.work_locally(cx)),
            cx,
        ));
    }
    if !embedded && running {
        actions.push(ghost_text_button(
            SharedString::from(format!("{scope}:cancel")),
            rules::CANCEL,
            Some(Icon::X),
            cx.listener(|view, _, _, cx| view.cancel_setup(cx)),
            cx,
        ));
    }

    v_flex()
        .w_full()
        .min_w_0()
        .when(layout.show_header, |this| this.child(header_row(snapshot, cx)))
        .child(body)
        .children(error)
        // Aligns the buttons' icons with the stage icons: 24 + 2 - (8 - 1).
        .child(h_flex().flex_wrap().mt(rem(2.)).ml(px(19.)).gap(rem(2.)).children(actions))
        .when(open, |this| this.child(details(snapshot, cx)))
        .into_any_element()
}

/// The working row's chip while the agent works and the setup script still runs; it opens the
/// embedded card in a popover.
pub fn background_chip(env: &RowEnv, cx: &mut Context<ThreadView>) -> Option<AnyElement> {
    let snapshot = env.setup.view.background.clone()?;
    let p = palette(cx);
    let open = env.setup.popover_open;
    let chip = h_flex()
        .id("background-worktree-setup")
        .flex_none()
        .min_w_0()
        .h(rem(20.))
        .px(rem(5.))
        .gap(rem(4.))
        .rounded(px(4.))
        .text_size(rem(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(hsla(p.muted_foreground))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
        .when(open, |this| this.bg(hsla(p.accent)))
        .child(Icon::LoaderCircle.render(px(12.), hsla(p.muted_foreground)))
        .child(div().truncate().child(rules::BACKGROUND_SETUP))
        .on_click(cx.listener(|view, _, _, cx| view.toggle_setup_popover(cx)));
    let popover = open.then(|| {
        let content = v_flex()
            .id("background-worktree-setup-popover")
            .occlude()
            .mt(rem(4.))
            .w(rem(384.))
            .px(rem(12.))
            .py(rem(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(hsla(p.dropdown_border))
            .bg(hsla(p.popover))
            .text_color(hsla(p.popover_foreground))
            .shadow(floating_shadow(cx.theme().is_dark()))
            .on_mouse_down_out(cx.listener(|view, _, _, cx| view.dismiss_setup_popover(cx)))
            .child(card("background-setup", &snapshot, true, env, cx));
        floating(Float::BelowEnd, content)
    });
    Some(div().relative().ml_auto().flex_none().child(focusable(chip, cx)).children(popover).into_any_element())
}

/// The blank row that keeps the "Thinking" slot's height while the worktree is prepared.
pub fn empty_work_row(id: &str) -> AnyElement {
    work_row_frame(SharedString::from(format!("preparing:{id}")), false).child(div().min_h(rem(24.))).into_any_element()
}
