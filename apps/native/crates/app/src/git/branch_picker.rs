//! The branch picker (`BranchPicker.tsx`, `BranchToolbarBranchSelector.tsx` in panel mode): a
//! searchable list of refs below the branch row. Picking a ref switches the checkout (or moves
//! the thread to the worktree that has it); a query no ref matches offers to create it.

use std::time::{Duration, Instant};

use gpui::{
    App, Context, ElementId, Entity, IntoElement, MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::v_flex;
use t3_core::vcs::{VcsRef, ref_badge, ref_matches_query, sanitize_new_ref_name};

use super::{GitEvent, GitStore};
use crate::composer::controls::{Float, floating, menu_surface};
use crate::icons::Icon;
use crate::sidebar::is_composing;
use crate::theme::{hsla, palette};
use crate::thread_details::controls::{MenuRow, ROW_WIDTH};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Ref(VcsRef),
    Create(String),
}

pub struct BranchPicker {
    git: Entity<GitStore>,
    input: Entity<InputState>,
    open: bool,
    highlighted: usize,
    closed_at: Option<Instant>,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

impl BranchPicker {
    pub fn new(git: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.highlighted = 0;
                    let query = input.read(cx).value().to_string();
                    this.debounce = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                        let _ = this.update(cx, |this, cx| this.git.update(cx, |git, cx| git.load_refs(&query, false, cx)));
                    }));
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.activate(this.highlighted, window, cx),
                _ => {}
            }),
            cx.subscribe(&git, |this, _, _: &GitEvent, cx| {
                if this.open {
                    cx.notify();
                }
            }),
        ];
        BranchPicker { git, input, open: false, highlighted: 0, closed_at: None, debounce: None, _subscriptions: subscriptions }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opening asks for a local status refresh and the first page of refs (`handleOpenChange`).
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            return self.close(cx);
        }
        if self.closed_at.take().is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
            return;
        }
        self.open = true;
        self.highlighted = 0;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.git.update(cx, |git, cx| {
            git.refresh_status(true, cx);
            git.load_refs("", false, cx);
        });
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.closed_at = Some(Instant::now());
            self.debounce = None;
            cx.notify();
        }
    }

    fn items(&self, cx: &App) -> Vec<Item> {
        let query = self.input.read(cx).value().trim().to_owned();
        let normalized = query.to_lowercase();
        let refs = &self.git.read(cx).refs;
        let mut items: Vec<Item> = refs.refs.iter().filter(|item| ref_matches_query(&item.name, &normalized)).cloned().map(Item::Ref).collect();
        let name = sanitize_new_ref_name(&query);
        if !query.is_empty() && !refs.refs.iter().any(|item| item.name == name) {
            items.push(Item::Create(name));
        }
        items
    }

    fn activate(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items(cx).into_iter().nth(index) else { return };
        self.close(cx);
        self.git.update(cx, |git, cx| match &item {
            Item::Ref(target) => git.select_ref(target, cx),
            Item::Create(name) => git.create_ref(name, cx),
        });
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.items(cx).len();
        if count > 0 {
            self.highlighted = (self.highlighted as isize + delta).rem_euclid(count as isize) as usize;
            cx.notify();
        }
    }
}

impl Render for BranchPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let items = self.items(cx);
        let store = self.git.read(cx);
        let refs = &store.refs;
        let project_cwd = store.target.as_ref().map(|target| target.project_cwd.clone());
        let status_text = if refs.loading && !refs.loaded {
            Some("Loading refs...".to_owned())
        } else if refs.loading {
            Some("Loading more refs...".to_owned())
        } else if refs.next_cursor.is_some() {
            Some(format!("Showing {} of {} refs", refs.refs.len(), refs.total))
        } else {
            None
        };
        let has_next = refs.next_cursor.is_some() && !refs.loading;
        let error = refs.error.clone();
        let empty = items.is_empty() && refs.loaded;
        let rows = items.into_iter().enumerate().map(|(index, item)| {
            let highlighted = index == self.highlighted;
            let id = ElementId::Name(format!("branch-picker-{index}").into());
            let row = match &item {
                Item::Ref(target) => {
                    let badge = ref_badge(target, project_cwd.as_deref());
                    MenuRow::new(id, target.name.clone()).trailing(div().children(badge.map(|badge| div().text_size(px(10.)).text_color(hsla(p.muted_foreground).opacity(0.45)).child(badge))))
                }
                Item::Create(name) => MenuRow::new(id, SharedString::from(format!("Create new ref \"{name}\""))),
            };
            div()
                .rounded(px(4.))
                .when(highlighted, |this| this.bg(hsla(p.accent)))
                .child(row.on_click(cx.listener(move |this, _, window, cx| this.activate(index, window, cx))))
        });
        let popup = menu_surface(cx)
            .id("branch-picker")
            .w(px(ROW_WIDTH))
            .max_h(px(360.))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if !is_composing(&this.input, window, cx) {
                    this.move_highlight(1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                if !is_composing(&this.input, window, cx) {
                    this.move_highlight(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if !is_composing(&this.input, window, cx) {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .pt(px(6.))
                    .pb(px(4.))
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .child(Icon::Search.sized(px(16.)).text_color(hsla(p.muted_foreground).opacity(0.55)))
                    .child(div().flex_1().h(px(26.)).flex().items_center().child(Input::new(&self.input).appearance(false).bordered(false))),
            )
            .child(
                v_flex()
                    .id("branch-picker-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .when(empty, |this| this.child(div().py(px(16.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child("No refs found.")))
                    .children(rows),
            )
            .children(error.map(|error| div().px(px(8.)).py(px(6.)).text_xs().text_color(hsla(p.destructive_foreground)).child(error)))
            .children(status_text.map(|text| {
                div()
                    .id("branch-picker-status")
                    .px(px(8.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .when(has_next, |this| {
                        this.cursor_pointer().hover(|style| style.text_color(hsla(p.foreground))).on_click(cx.listener(|this, _, _, cx| {
                            let query = this.input.read(cx).value().to_string();
                            this.git.update(cx, |git, cx| git.load_refs(&query, true, cx));
                        }))
                    })
                    .child(text)
            }));
        floating(Float::BelowStart, div().pt(px(4.)).child(popup))
    }
}
