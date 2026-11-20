//! "Working for 12s", the Stop line's time and a running worktree setup stage's time: the
//! transcript labels that change on a timer (`WorkingTimer` in `TimelineWorkGroupRows.tsx`,
//! `tickTextContent.ts`, `LiveElapsed` in `WorktreeSetupCard.tsx`). Each is its own view so the
//! tick redraws only it, and it stops notifying when it was not painted since the last tick
//! (scrolled out of the list's render window, or the window is hidden), resuming once drawn again.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Task, Window, div};
use t3_core::timeline::{elapsed_since, format_duration, parse_iso_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    Working,
    Compacting,
    /// Just the elapsed time, as the Stop line shows it (`runStopNowItem`).
    Elapsed,
    /// A running worktree setup stage's time (`LiveElapsed` in `WorktreeSetupCard.tsx`).
    SetupStage,
}

pub struct WorkingTimer {
    started_at: String,
    env_id: Option<String>,
    kind: TimerKind,
    painted: Rc<Cell<bool>>,
    _tick: Task<()>,
}

impl WorkingTimer {
    pub fn new(started_at: String, env_id: Option<String>, kind: TimerKind, cx: &mut Context<Self>) -> Self {
        let painted = Rc::new(Cell::new(false));
        let seen = painted.clone();
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let visible = seen.replace(false);
                if this.update(cx, |_, cx| if visible { cx.notify() }).is_err() {
                    return;
                }
            }
        });
        WorkingTimer { started_at, env_id, kind, painted, _tick: tick }
    }

    pub fn matches(&self, started_at: &str, env_id: Option<&str>, kind: TimerKind) -> bool {
        self.started_at == started_at && self.env_id.as_deref() == env_id && self.kind == kind
    }

    fn now_ms(&self) -> i64 {
        let client_ms = chrono::Utc::now().timestamp_millis();
        self.env_id.as_deref().map_or(client_ms, |id| t3_core::clock::to_server_clock(id, client_ms))
    }
}

/// `formatWorkingTimerNow`: whole seconds, at least one.
fn elapsed_label(started_at: &str, now_ms: i64) -> Option<String> {
    let start = parse_iso_ms(started_at)?;
    let seconds = (now_ms - start).max(0) / 1_000;
    Some(format_duration((seconds.max(1) * 1_000) as f64))
}

impl Render for WorkingTimer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.painted.set(true);
        let now_ms = self.now_ms();
        let label = match (self.kind, elapsed_label(&self.started_at, now_ms)) {
            (TimerKind::Compacting, Some(elapsed)) => crate::i18n::t_fmt(cx, "Compacting for {duration}", &[("duration", elapsed.into())]),
            (TimerKind::Compacting, None) => crate::i18n::t(cx, "Compacting…").to_owned(),
            (TimerKind::Working, Some(elapsed)) => crate::i18n::t_fmt(cx, "Working for {duration}", &[("duration", elapsed.into())]),
            (TimerKind::Working, None) => crate::i18n::t(cx, "Working...").to_owned(),
            (TimerKind::Elapsed, _) => elapsed_since(&self.started_at, now_ms).unwrap_or_default(),
            (TimerKind::SetupStage, _) => t3_core::worktree_setup::live_label(&self.started_at, now_ms).unwrap_or_default(),
        };
        div().whitespace_nowrap().child(label)
    }
}
