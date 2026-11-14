//! Live and settled elapsed labels for subagents, workflow phases, and workflow members (`AgentElapsed.tsx`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{AnyElement, App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div};
use t3_core::lineage::AgentTiming;

/// Live ticking elapsed view for an active subagent, phase, or workflow member.
pub struct LiveAgentElapsed {
    timing: AgentTiming,
    compact: bool,
    painted: Rc<Cell<bool>>,
    _tick: Task<()>,
}

impl LiveAgentElapsed {
    pub fn new(timing: AgentTiming, compact: bool, cx: &mut Context<Self>) -> Self {
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
        Self { timing, compact, painted, _tick: tick }
    }

    pub fn sync(&mut self, timing: AgentTiming, compact: bool, cx: &mut Context<Self>) {
        if self.timing != timing || self.compact != compact {
            self.timing = timing;
            self.compact = compact;
            cx.notify();
        }
    }
}

impl Render for LiveAgentElapsed {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.painted.set(true);
        div().whitespace_nowrap().child(self.timing.format(chrono::Utc::now().timestamp_millis(), self.compact))
    }
}

/// Renders either a paint-gated live timer (when `timing.active`) or the settled static text.
pub fn agent_elapsed_element(key: SharedString, timing: AgentTiming, compact: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    if !timing.active {
        return div().whitespace_nowrap().child(timing.format(0, compact)).into_any_element();
    }
    let entity: Entity<LiveAgentElapsed> = window.use_keyed_state(key, cx, |_, cx| LiveAgentElapsed::new(timing, compact, cx));
    entity.update(cx, |view, cx| view.sync(timing, compact, cx));
    entity.into_any_element()
}
