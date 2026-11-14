//! Keeps the reader's place when transcript rows change, as the web list's
//! `maintainVisibleContentPosition` does (`MessagesTimeline.tsx`): the first row in view stays
//! where it was on screen when rows are inserted or replaced around it, such as the earlier turns
//! "Load earlier turns" brings in above it. Positions are the list's logical scroll top, an item
//! and how far into it the viewport starts; items before the rows (the history control) are the
//! header, which the web renders outside its data as `ListHeaderComponent`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

/// Distance from the end within which the timeline counts as at-end (`SCROLL_BOTTOM_THRESHOLD_PX`).
pub const AT_END_THRESHOLD_PX: f32 = 40.;
/// Reveal delay before showing the Scroll-to-end pill (`150ms`).
pub const SCROLL_TO_END_REVEAL_DELAY: Duration = Duration::from_millis(150);
/// Top offset when anchoring a new first-turn user message (`CHAT_TIMELINE_ANCHOR_OFFSET`).
pub const ANCHOR_TOP_OFFSET_PX: f32 = 24.;
/// Finite smooth scroll duration for minimap jumps, wheel glides, and Scroll-to-end (`150ms`).
pub const SMOOTH_SCROLL_DURATION: Duration = Duration::from_millis(150);
/// Maximum number of per-thread scroll positions remembered in memory (`useTimelinePositionRestore`).
pub const REMEMBERED_POSITIONS_LIMIT: usize = 100;

/// The list's scroll position: the viewport's top is `offset` px into `item` (a negative offset
/// leaves `item` that far below it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollTop {
    pub item: usize,
    pub offset: f32,
}

/// The rows in view before a change: id, row index, and the row's top relative to the
/// viewport's top (negative while it is cut off above).
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleRows {
    scroll_top: ScrollTop,
    header: usize,
    rows: Vec<(String, usize, f32)>,
    disclosure: bool,
}

/// Where to put the scroll top after a change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    At(ScrollTop),
    /// `item`'s top sits `gap` px below the viewport's top, with the rows above it in the gap.
    Below { item: usize, gap: f32 },
}

/// The rows in view, measured with `item_top` (an item's top in the list's content). `None` while
/// the list follows its tail or has not laid out, unless a disclosure anchors its own row.
pub fn capture<T>(rows: &[T], id: impl Fn(&T) -> &str, header: usize, scroll_top: ScrollTop, viewport: f32, item_top: impl Fn(usize) -> f32, disclosure: Option<&str>) -> Option<VisibleRows> {
    let mode = disclosure.map_or(TailCapture::Follow, TailCapture::Disclosure);
    capture_inner(rows, id, header, scroll_top, viewport, item_top, mode)
}

/// Like [`capture`], but still captures visible rows at the tail when live follow is paused
/// (e.g. while a full-screen modal overlay is open).
pub fn capture_pinned<T>(rows: &[T], id: impl Fn(&T) -> &str, header: usize, scroll_top: ScrollTop, viewport: f32, item_top: impl Fn(usize) -> f32, disclosure: Option<&str>) -> Option<VisibleRows> {
    capture_inner(rows, id, header, scroll_top, viewport, item_top, TailCapture::Pinned(disclosure))
}

#[derive(Clone, Copy)]
enum TailCapture<'a> {
    Follow,
    Disclosure(&'a str),
    Pinned(Option<&'a str>),
}

fn capture_inner<T>(rows: &[T], id: impl Fn(&T) -> &str, header: usize, scroll_top: ScrollTop, viewport: f32, item_top: impl Fn(usize) -> f32, mode: TailCapture<'_>) -> Option<VisibleRows> {
    let (disclosure, pin_at_tail) = match mode {
        TailCapture::Follow => (None, false),
        TailCapture::Disclosure(key) => (Some(key), false),
        TailCapture::Pinned(key) => (key, true),
    };
    let at_tail = scroll_top.item >= header + rows.len();
    if viewport <= 0. || (at_tail && !pin_at_tail && disclosure.is_none()) {
        return None;
    }
    // At the tail GPUI reports the end of all items (including the footer), not the viewport top.
    let scrolled = item_top(scroll_top.item) + scroll_top.offset - if at_tail { viewport } else { 0. };
    if let Some(key) = disclosure {
        let index = rows.iter().position(|row| id(row) == key)?;
        let top = item_top(header + index) - scrolled;
        return (top < viewport).then(|| VisibleRows { scroll_top, header, rows: vec![(key.to_owned(), index, top)], disclosure: true });
    }
    let mut first = if at_tail && pin_at_tail { rows.len() } else { scroll_top.item.max(header) - header };
    if at_tail && pin_at_tail {
        while first > 0 && item_top(header + first) > scrolled {
            first -= 1;
        }
    }
    let rows: Vec<(String, usize, f32)> = (first..rows.len())
        .map(|index| (index, item_top(header + index) - scrolled))
        .take_while(|(_, top)| *top < viewport)
        .map(|(index, top)| (id(&rows[index]).to_owned(), index, top))
        .collect();
    (!rows.is_empty()).then_some(VisibleRows { scroll_top, header, rows, disclosure: false })
}

/// Puts the first captured row that is still there back where it was. `None` when the list
/// already shows it there (`current` is the list's scroll top after the change) or none is left.
pub fn restore<T>(visible: &VisibleRows, rows: &[T], id: impl Fn(&T) -> &str, header: usize, current: ScrollTop) -> Option<Anchor> {
    let (candidate, index, old_index, top) = visible.rows.iter().enumerate().find_map(|(candidate, (row_id, old_index, top))| {
        let same_place = rows.get(*old_index).is_some_and(|row| id(row) == row_id);
        let index = if same_place { Some(*old_index) } else { rows.iter().position(|row| id(row) == row_id) };
        index.map(|index| (candidate, index, *old_index, *top))
    })?;
    let item = header + index;
    // Unmoved only when the first row in view kept its place: a row in view above the anchor that
    // went may have taken a different height with it.
    if !visible.disclosure && candidate == 0 && item == visible.header + old_index && current == visible.scroll_top {
        return None;
    }
    let anchor = if visible.disclosure || top <= 0. { Anchor::At(ScrollTop { item, offset: -top }) } else { Anchor::Below { item, gap: top } };
    (anchor != Anchor::At(current)).then_some(anchor)
}

/// Includes rows above a negatively offset anchor in the paint range, preserving any remaining
/// bottom-alignment space above the first item instead of clamping it away.
pub fn include_preceding_rows(mut top: ScrollTop, item_height: impl Fn(usize) -> f32) -> ScrollTop {
    while top.offset < 0. && top.item > 0 {
        top.item -= 1;
        top.offset += item_height(top.item);
    }
    top
}

/// Disclosure settling and the web's live-follow eligibility (`MessagesTimeline.tsx`).
#[derive(Default)]
pub struct DisclosureFollow {
    anchor: Option<String>,
    generation: u64,
    following: bool,
}

impl DisclosureFollow {
    pub fn toggle(&mut self, key: &str, at_end: bool) {
        self.anchor = Some(key.to_owned());
        self.following |= at_end;
        self.changed();
    }

    pub fn anchor(&self) -> Option<&str> { self.anchor.as_deref() }

    pub fn generation(&self) -> Option<u64> { self.anchor.as_ref().map(|_| self.generation) }

    pub fn changed(&mut self) {
        if self.anchor.is_some() { self.generation += 1; }
    }

    pub fn cancel(&mut self) { self.anchor = None; self.following = false; }

    pub fn settle(&mut self, generation: u64, distance_from_end: f32, viewport: f32) -> bool {
        if self.generation() != Some(generation) || viewport <= 0. { return false; }
        self.anchor = None;
        self.following |= at_end(distance_from_end, viewport);
        if distance_from_end == 0. {
            self.following = false;
            true
        } else {
            false
        }
    }

    pub fn needs_follow_check(&self) -> bool { self.anchor.is_none() && self.following }

    pub fn follow_change(&mut self, distance_from_end: f32, viewport: f32) -> bool {
        let follow = self.needs_follow_check() && viewport > 0. && distance_from_end <= viewport;
        if follow { self.following = false; }
        follow
    }
}

/// `resolveTimelineIsAtEnd` uses 40px; maintenance on the next change allows one viewport.
pub fn at_end(distance_from_end: f32, viewport: f32) -> bool {
    viewport > 0. && distance_from_end <= AT_END_THRESHOLD_PX
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowPhase {
    Following,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Scrollbar,
    Keyboard,
    Minimap,
    Find,
    Citation,
    EndKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTurnAnchor {
    pub run_id: String,
    pub user_message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAnchorSnapshot<'a> {
    pub run_id: &'a str,
    pub status: &'a str,
    pub user_message_id: &'a str,
    pub has_tool_or_reasoning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorEffect {
    None,
    Anchor { user_message_id: String },
    Keep { user_message_id: String },
    ReleaseAndScrollToEnd,
    ReleaseOnly,
}

pub fn is_active_anchor_status(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running" | "waiting")
}

pub fn is_live_non_queued_run_status(status: &str) -> bool {
    matches!(status, "preparing" | "starting" | "running" | "waiting")
}

/// Formats the optional status badge appended to the Scroll-to-end pill (`MessagesTimeline.tsx`).
pub fn scroll_to_end_summary(queued_count: usize, pending_approvals: usize, has_waiting_children: bool) -> Option<String> {
    if queued_count > 0 {
        return Some(format!("{queued_count} queued"));
    }
    if pending_approvals > 0 {
        return Some("Approval needed".to_owned());
    }
    if has_waiting_children {
        return Some("Child waiting".to_owned());
    }
    None
}

/// Stateful follow, Scroll-to-end reveal debounce, and new-turn scroll anchoring for one thread
/// (`useTimelineFollow.ts` + `timelineScrollAnchoring.ts`).
#[derive(Debug, Clone)]
pub struct TimelineFollow {
    phase: FollowPhase,
    at_end: bool,
    anchor: Option<NewTurnAnchor>,
    observed_run_ids: HashSet<String>,
    hydrated: bool,
    pill_pending_since: Option<Duration>,
    pill_visible: bool,
}

impl Default for TimelineFollow {
    fn default() -> Self {
        Self {
            phase: FollowPhase::Following,
            at_end: true,
            anchor: None,
            observed_run_ids: HashSet::new(),
            hydrated: false,
            pill_pending_since: None,
            pill_visible: false,
        }
    }
}

impl TimelineFollow {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_following(&self) -> bool {
        self.phase == FollowPhase::Following
    }

    pub fn is_at_end(&self) -> bool {
        self.at_end
    }

    pub fn anchored_user_message_id(&self) -> Option<&str> {
        self.anchor.as_ref().map(|anchor| anchor.user_message_id.as_str())
    }

    fn clear_pill(&mut self) {
        self.pill_pending_since = None;
        self.pill_visible = false;
    }

    pub fn scroll_to_end(&mut self) {
        self.phase = FollowPhase::Following;
        self.at_end = true;
        self.anchor = None;
        self.clear_pill();
    }

    pub fn restore_released(&mut self, at_end: bool, now: Duration) {
        self.phase = FollowPhase::Released;
        self.at_end = at_end;
        self.anchor = None;
        if at_end {
            self.clear_pill();
        } else {
            self.pill_pending_since = Some(now);
            self.pill_visible = false;
        }
    }

    /// Passive layout/content growth while `Following` must never release follow.
    /// While `Released`, shrinking back into `<= 40px` updates `at_end` and hides the pill.
    pub fn on_passive_distance(&mut self, at_end: bool, now: Duration) {
        match self.phase {
            FollowPhase::Following => {
                self.at_end = true;
                self.clear_pill();
            }
            FollowPhase::Released => {
                self.at_end = at_end;
                if at_end {
                    self.clear_pill();
                } else if !self.pill_visible && self.pill_pending_since.is_none() {
                    self.pill_pending_since = Some(now);
                }
            }
        }
    }

    /// Explicit user scroll or navigation: cancels any active new-turn anchor, keeps `Released`
    /// hysteresis when navigating while already within 40px, and re-engages `Following` only when
    /// returning to `<= 40px` from away (`!was_at_end && at_end`) or via `EndKey`.
    pub fn on_user_navigation(&mut self, source: ScrollSource, at_end: bool, now: Duration) {
        self.anchor = None;
        if source == ScrollSource::EndKey {
            self.scroll_to_end();
            return;
        }
        if self.phase == FollowPhase::Released && !self.at_end && at_end {
            self.phase = FollowPhase::Following;
            self.at_end = true;
            self.clear_pill();
            return;
        }
        self.phase = FollowPhase::Released;
        self.at_end = at_end;
        if at_end {
            self.clear_pill();
        } else if !self.pill_visible && self.pill_pending_since.is_none() {
            self.pill_pending_since = Some(now);
        }
    }

    /// Returns `(visible_now, next_reveal_deadline)` for the 150ms Scroll-to-end pill debounce.
    pub fn pill_state(&mut self, find_open: bool, has_rows: bool, now: Duration) -> (bool, Option<Duration>) {
        let eligible = self.phase == FollowPhase::Released && !self.at_end && !find_open && self.anchor.is_none() && has_rows;
        if !eligible {
            self.clear_pill();
            return (false, None);
        }
        if self.pill_visible {
            return (true, None);
        }
        if let Some(since) = self.pill_pending_since {
            let deadline = since + SCROLL_TO_END_REVEAL_DELAY;
            if now >= deadline {
                self.pill_visible = true;
                self.pill_pending_since = None;
                return (true, None);
            }
            return (false, Some(deadline));
        }
        self.pill_pending_since = Some(now);
        (false, Some(now + SCROLL_TO_END_REVEAL_DELAY))
    }

    /// Tracks newly started non-queued runs after initial thread hydration and drives first-message
    /// top anchoring (`24px` inset) and release on tool/reasoning activity or run completion.
    pub fn observe_runs<F>(&mut self, runs: &[RunAnchorSnapshot<'_>], total_user_messages: usize, is_first_user_message: F) -> AnchorEffect
    where
        F: Fn(&str) -> bool,
    {
        let initial = !self.hydrated;
        self.hydrated = true;

        let mut candidate: Option<NewTurnAnchor> = None;
        for run in runs {
            if self.observed_run_ids.insert(run.run_id.to_owned()) && is_live_non_queued_run_status(run.status) && !run.has_tool_or_reasoning {
                candidate = Some(NewTurnAnchor {
                    run_id: run.run_id.to_owned(),
                    user_message_id: run.user_message_id.to_owned(),
                });
            }
        }

        if let Some(next) = candidate
            && self.phase == FollowPhase::Following
            && total_user_messages == 1
            && (!initial || runs.len() == 1)
            && is_first_user_message(&next.user_message_id)
        {
            let user_message_id = next.user_message_id.clone();
            self.anchor = Some(next);
            self.clear_pill();
            return AnchorEffect::Anchor { user_message_id };
        }

        let Some(active) = &self.anchor else {
            return AnchorEffect::None;
        };
        let still_valid = runs.iter().any(|run| run.run_id == active.run_id && is_active_anchor_status(run.status) && !run.has_tool_or_reasoning);
        if still_valid {
            return AnchorEffect::Keep { user_message_id: active.user_message_id.clone() };
        }
        self.anchor = None;
        if self.phase == FollowPhase::Following { AnchorEffect::ReleaseAndScrollToEnd } else { AnchorEffect::ReleaseOnly }
    }
}

/// Saved per-thread scroll position and disclosures (`useTimelinePositionRestore.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct SavedThreadPosition {
    pub row_id: Option<String>,
    pub offset_within_row: f32,
    pub scroll_top: f32,
    pub at_end: bool,
    pub expanded_runs: HashSet<String>,
    pub expanded_attempts: HashSet<String>,
    pub expanded_groups: HashSet<String>,
}

/// Bounded LRU store (capacity 100) remembering each visited thread's scroll position & disclosures.
#[derive(Debug, Clone, Default)]
pub struct RememberedPositions {
    entries: HashMap<String, SavedThreadPosition>,
    order: VecDeque<String>,
}

impl RememberedPositions {
    pub fn save(&mut self, thread_id: impl Into<String>, position: SavedThreadPosition) {
        let thread_id = thread_id.into();
        if let Some(pos) = self.order.iter().position(|id| id == &thread_id) {
            self.order.remove(pos);
        }
        self.order.push_back(thread_id.clone());
        self.entries.insert(thread_id, position);
        while self.order.len() > REMEMBERED_POSITIONS_LIMIT {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }

    pub fn get(&mut self, thread_id: &str) -> Option<&SavedThreadPosition> {
        if self.entries.contains_key(thread_id)
            && let Some(pos) = self.order.iter().position(|id| id == thread_id)
        {
            let id = self.order.remove(pos)?;
            self.order.push_back(id);
        }
        self.entries.get(thread_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Cubic ease-out used by smooth timeline jumps (`1 - (1 - t)^3`).
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

/// Fraction of the remaining distance to travel between normalized times `prev_t` and `next_t`.
/// Advancing incrementally by remaining distance preserves smooth ease-out even when unmeasured
/// rows dynamically resize mid-flight.
pub fn smooth_step_fraction(prev_t: f32, next_t: f32) -> f32 {
    let next_t = next_t.clamp(0., 1.);
    if next_t >= 1. {
        return 1.;
    }
    let prev_e = ease_out_cubic(prev_t);
    let next_e = ease_out_cubic(next_t);
    let denom = (1. - prev_e).max(1e-4);
    ((next_e - prev_e) / denom).clamp(0., 1.)
}

#[cfg(test)]
mod tests;
