//! "Working for 12s": the one transcript label that changes on a timer (`WorkingTimer` in
//! `TimelineWorkGroupRows.tsx`, `tickTextContent.ts`). It is its own view so the tick redraws
//! only it, and it stops notifying when it was not painted since the last tick (scrolled out
//! of the list's render window, or the window is hidden), resuming once drawn again.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Task, Window, div};
use t3_core::timeline::{format_duration, parse_iso_ms};

pub struct WorkingTimer {
    started_at: String,
    compacting: bool,
    painted: Rc<Cell<bool>>,
    _tick: Task<()>,
}

impl WorkingTimer {
    pub fn new(started_at: String, compacting: bool, cx: &mut Context<Self>) -> Self {
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
        WorkingTimer { started_at, compacting, painted, _tick: tick }
    }

    pub fn matches(&self, started_at: &str, compacting: bool) -> bool {
        self.started_at == started_at && self.compacting == compacting
    }
}

/// `formatWorkingTimerNow`: whole seconds, at least one.
fn elapsed_label(started_at: &str) -> Option<String> {
    let start = parse_iso_ms(started_at)?;
    let seconds = (chrono::Utc::now().timestamp_millis() - start).max(0) / 1_000;
    Some(format_duration((seconds.max(1) * 1_000) as f64))
}

impl Render for WorkingTimer {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.painted.set(true);
        let label = match (self.compacting, elapsed_label(&self.started_at)) {
            (true, Some(elapsed)) => format!("Compacting for {elapsed}"),
            (true, None) => "Compacting…".to_owned(),
            (false, Some(elapsed)) => format!("Working for {elapsed}"),
            (false, None) => "Working...".to_owned(),
        };
        div().whitespace_nowrap().child(label)
    }
}
