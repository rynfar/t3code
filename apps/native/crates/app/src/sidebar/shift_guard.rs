//! Reject presses on rows that moved under the pointer (web/src/components/Sidebar.shiftGuard.ts).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Styled, canvas, px,
};

use super::PaneRowKey;
use super::tree::TreeRow;

const GRACE: Duration = Duration::from_millis(500);

struct Position {
    row: TreeRow,
    bounds: Bounds<Pixels>,
    moved_at: Option<Instant>,
}

#[derive(Default)]
struct Frame {
    rows: Vec<Position>,
    layout_changed: bool,
    last_input: Option<Instant>,
    blocked_buttons: Vec<MouseButton>,
}

#[derive(Clone, Default)]
pub(super) struct ShiftGuard(Rc<RefCell<Frame>>);

impl ShiftGuard {
    pub(super) fn layout_changed(&self) {
        self.0.borrow_mut().layout_changed = true;
    }

    pub(super) fn note_input(&self) {
        self.0.borrow_mut().last_input = Some(Instant::now());
    }

    pub(super) fn press(&self, event: &MouseDownEvent) -> bool {
        let mut frame = self.0.borrow_mut();
        frame
            .blocked_buttons
            .retain(|button| *button != event.button);
        let now = Instant::now();
        if frame.rows.iter().any(|entry| {
            entry.bounds.contains(&event.position)
                && entry
                    .moved_at
                    .is_some_and(|at| now.duration_since(at) < GRACE)
        }) {
            frame.blocked_buttons.push(event.button);
            return true;
        }
        frame.last_input = Some(now);
        false
    }

    pub(super) fn release(&self, event: &MouseUpEvent) -> bool {
        let mut frame = self.0.borrow_mut();
        if let Some(index) = frame
            .blocked_buttons
            .iter()
            .position(|button| *button == event.button)
        {
            frame.blocked_buttons.swap_remove(index);
            return true;
        }
        frame.last_input = Some(Instant::now());
        false
    }

    pub(super) fn frame(&self) -> LayoutFrame {
        LayoutFrame {
            state: self.clone(),
            rows: Rc::default(),
        }
    }
}

type RowPositions = Vec<(TreeRow, Bounds<Pixels>)>;

#[derive(Clone)]
pub(super) struct LayoutFrame {
    state: ShiftGuard,
    rows: Rc<RefCell<RowPositions>>,
}

impl LayoutFrame {
    pub(super) fn row(&self, key: &PaneRowKey) -> Option<impl IntoElement> {
        let row = TreeRow::from_key(key)?;
        let rows = self.rows.clone();
        Some(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let bounds = bounds.intersect(&window.content_mask().bounds);
                    if bounds.size.height > px(0.) && bounds.size.width > px(0.) {
                        rows.borrow_mut().push((row, bounds));
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
    }

    pub(super) fn finish(self) -> impl IntoElement {
        canvas(
            |_, _, _| {},
            move |_, _, _, _| {
                let mut frame = self.state.0.borrow_mut();
                let now = Instant::now();
                let layout_changed = frame.layout_changed && !frame.rows.is_empty();
                let external_move = frame
                    .last_input
                    .is_none_or(|at| now.duration_since(at) >= GRACE);
                let next = self
                    .rows
                    .borrow_mut()
                    .drain(..)
                    .map(|(row, bounds)| {
                        let previous = frame.rows.iter().find(|entry| entry.row == row);
                        let moved = layout_changed
                            && previous.is_none_or(|entry| {
                                (bounds.top() - entry.bounds.top()).abs() * 2. >= bounds.size.height
                            });
                        let moved_at = if moved {
                            external_move.then_some(now)
                        } else {
                            previous.and_then(|entry| entry.moved_at)
                        };
                        Position {
                            row,
                            bounds,
                            moved_at,
                        }
                    })
                    .collect();
                frame.rows = next;
                frame.layout_changed = false;
            },
        )
        .absolute()
        .size_0()
    }
}
